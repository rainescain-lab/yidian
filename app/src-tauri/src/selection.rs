//! 划词取选区：等热键按键真正松开 → 模拟 Ctrl+C → 读剪贴板，含备份/轮询/恢复。
//! 必须在源程序仍为前台时调用（先取选区、再建 popup）。同步阻塞，放 spawn_blocking。
//!
//! # 为什么要等按键松开（2026-08-02 受控实验实证的根因）
//! 旧实现盲等 140ms 赌用户已松手，再补发一个 Alt↑。实测这在两个方向上都不成立：
//!   A. Alt 松开时发 Ctrl+C  → 剪贴板序列号 6840→6845，复制成功
//!   B. Alt 按住时发 Ctrl+C  → 序列号纹丝不动，完全没复制
//!   C. 按住 Alt→补发 Alt↑→30ms→重复又按下→发 Ctrl+C → 同样没复制（＝旧实现的失败现场）
//! 因为 Windows 按键重复（本机延迟 500ms、频率最快档约每 33ms 一次）会把补发的 Alt↑ 立刻盖掉，
//! 于是发出去的其实是 Alt+Ctrl+C，没有程序把它当复制 → 取词返回 len=0，划词静默失效。
//! 表现就是"按得快就成、按得慢就废"的间歇故障，且会自己"好"再复发。
//!
//! 所以改为**轮询等物理按键真正松开**再发 Ctrl+C。**热键的主键也必须等**：我们发的 Ctrl 会和
//! 用户还按着的主键凑成别的组合（默认键 Alt+W 时是 `Ctrl+W`＝关浏览器标签页）。旧的 140ms
//! 恰好挡住了这个，改动中不能漏掉。
//!
//! # 主键不再写死（2026-08-07，热键可自定义之后的硬性配套）
//! 从前这里写死 `("W", 0x57)`。热键改成用户可自定义之后，这个常量一旦不跟着走，后果是
//! **静默走错**：用户设 `Ctrl+Alt+V`，我们只等 Alt 松开、不等 V ⇒ 发出去的 Ctrl 和还按着的 V
//! 凑成 **Ctrl+V，把剪贴板内容直接粘进用户正在编辑的文档**。比取词失败恶劣得多。
//! 现在主键由调用方按"实际注册成功的那个热键"算出 VK 传进来（见 `hotkey.rs`）。
//!
//! # 等不到就放弃，不再硬发（同上，2026-08-07）
//! 旧实现等满上限后仍会硬发 Ctrl+C，理由是"顶多取不到词"。可自定义键之后这个理由不成立了：
//! 主键是 V 时硬发就是**往用户文档里粘贴**，是 C 时是复制、是 S 时是保存……
//! 破坏性随用户设的键而变，不能赌。等不到就干脆放弃，并在日志里说清为什么。
//!
//! # 取证
//! 取词失败历史上只留一行 `len=0`，照不出断在哪一步。现每步都记诊断，判读要点——
//! **剪贴板序列号是决定性证据**：目标只要真复制了，GetClipboardSequenceNumber 必然递增。
//!   ① 序列号没变 + enigo 报错 → 按键没送出去（多半 UIPI：目标进程完整性比本程序高）
//!   ② 序列号没变 + enigo 正常 → 按键送到了但目标没复制（无选区／修饰键污染／目标不认 Ctrl+C）
//!   ③ 序列号变了但读不到文本 → 目标复制的不是纯文本格式，或剪贴板被别人占着

use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

/// 等物理按键松开的上限。正常松手在几十毫秒内，超过这个数说明用户真按着不放，硬发也没用。
const RELEASE_WAIT_MAX_MS: u64 = 1200;

/// 等目标程序把选区写进剪贴板的总预算（2026-08-06 由 300ms 放宽到此值）。
///
/// 旧版是写死的 15 轮 × 20ms = **300ms**，这是本文件里最依赖机器快慢的一个数：原生小程序
/// 几十毫秒就回来了，但 Electron 类（VS Code / 微信 / Discord）、重页面浏览器、笔记本降频、
/// 机器正忙时，一次 Ctrl+C 往返轻松超过 300ms —— 于是同一份代码在这台机器上好好的、
/// 换台机器就"有时候行有时候不行"。前 300ms 仍按 20ms 快轮询（常态路径耗时不变），
/// 之后降速到 50ms 一轮，避免为了兜底而空转 CPU。
const POLL_BUDGET_MS: u64 = 1500;
/// 前若干轮的快轮询间隔与轮数（＝旧版全部行为，保持快路径零回归）。
const POLL_FAST_ROUNDS: u32 = 15;
const POLL_FAST_MS: u64 = 20;
/// 快轮询用完之后的间隔。
const POLL_SLOW_MS: u64 = 50;
/// 放弃之后再补等多久做一次"迟到检查"（把"根本没复制"和"复制得比我们等得久"分开）。
const LATE_CHECK_MS: u64 = 800;

/// 同一时刻只允许一个取词流程。连按 Alt+W 会让两个流程并发抢剪贴板：A 刚置空哨兵，
/// B 就把这个空串当作"原内容"备份走，随后互相覆盖——于是一次失败引来连按、连按又制造新的失败。
static GRABBING: AtomicBool = AtomicBool::new(false);

struct GrabGuard;
impl Drop for GrabGuard {
    fn drop(&mut self) {
        GRABBING.store(false, Ordering::Release);
    }
}

/// 划词取词需要知道的热键信息。
///
/// ⚠ 一律由**实际注册成功的那个热键**算出来，不能用"用户想要的那个"：两者在注册失败时
/// 是不一样的，用错了就会去等一个根本没人按的键（永远显示已松开）⇒ 立刻发 Ctrl+C ⇒
/// 又回到"Ctrl+主键"误触发的老坑，且日志里一切正常。
#[derive(Debug, Clone, Default)]
pub struct Keys {
    /// 热键主键的 Win32 虚拟键码。`0` = 没有可等的主键（热键没生效，或该键不在 VK 表里）。
    pub main_vk: i32,
    /// 热键规范串，如 `alt+KeyW`。只用于日志判读。
    pub accel: String,
}

/// 一次取词的结果。
#[derive(Debug, Default)]
pub struct GrabResult {
    /// 取到的选中文本。
    pub text: Option<String>,
    /// **只有**"等键超时、主动放弃"这一条失败路径会带值，内容是当时仍按着的键。
    ///
    /// 为什么单独拎出来：其它 `text=None`（最常见的是"压根没选中文字"）必须保持静默，
    /// 否则每次误按热键都会弹一张卡；而"你一直按着不放"是用户自己就能修的，
    /// 不说出来他只会觉得划词坏了 —— 那句唯一的自救指引以前只写进了日志文件。
    pub keys_held: Option<String>,
    /// 取证日志行。
    pub diag: Vec<String>,
}

impl GrabResult {
    fn only(diag: Vec<String>) -> Self {
        GrabResult {
            text: None,
            keys_held: None,
            diag,
        }
    }
}

/// 取当前选中的文本 + 取证日志行；取不到时文本为 None。会尽力恢复原剪贴板文本。
pub fn grab_selection(keys: Keys) -> GrabResult {
    let t0 = Instant::now();
    let mut d: Vec<String> = Vec::new();
    macro_rules! note {
        ($($a:tt)*) => {
            d.push(format!("    [+{:>4}ms] {}", t0.elapsed().as_millis(), format_args!($($a)*)))
        };
    }

    if GRABBING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        note!("上一次取词尚未结束，本次忽略（并发取词会互抢剪贴板）");
        return GrabResult::only(d);
    }
    let _guard = GrabGuard;

    note!(
        "热键={} 要等的主键 vk={:#04x}{}",
        if keys.accel.is_empty() { "?" } else { &keys.accel },
        keys.main_vk,
        if keys.main_vk == 0 {
            "（无主键可等——热键未生效或该键不在 VK 表内）"
        } else {
            ""
        }
    );

    #[cfg(windows)]
    {
        note!("{}", win::foreground_info());
        note!("{}", win::input_route_info());
    }

    let mut cb = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            note!("✗ Clipboard::new 失败: {e}");
            return GrabResult::only(d);
        }
    };

    #[cfg(windows)]
    let seq_start = win::clip_seq();
    #[cfg(not(windows))]
    let seq_start = 0u32;

    let backup = cb.get_text().ok();
    note!(
        "剪贴板序列号={seq_start} 原内容备份={}",
        backup
            .as_deref()
            .map(|t| format!("{}字符", t.chars().count()))
            .unwrap_or("无/非文本".into())
    );

    // ---- ① 先等物理松开。此刻还没动过剪贴板，所以"等不到就放弃"是零副作用的 ----
    // 次序是有意的：旧实现先置空哨兵再等键，一旦在等键阶段退出就会把用户的剪贴板永久留在
    // "空"上。放弃是常态路径（用户按住不放就会走到），绝不能有这种副作用。
    #[cfg(windows)]
    {
        let (ok, ms, still) = win::wait_keys_released(keys.main_vk, RELEASE_WAIT_MAX_MS);
        if !ok {
            note!(
                "✗ 等满 {ms}ms 仍按着 [{still}] → **本次放弃取词**（不硬发 Ctrl+C：主键还按着时\
                 发出去会凑成 Ctrl+主键，按用户设的键不同可能是关标签页/粘贴/保存）。请按完热键就松手。"
            );
            return GrabResult {
                text: None,
                keys_held: Some(still),
                diag: d,
            };
        }
        note!("等 {ms}ms → 热键涉及的键已全部物理松开");
    }
    #[cfg(not(windows))]
    thread::sleep(Duration::from_millis(140));

    // ---- ② 置空哨兵 → 发 Ctrl+C → 轮询读回 ----
    // 用带标签的块把这一段收口：无论从哪条路径退出，都保证走到下面那句剪贴板恢复。
    // （旧实现里 Enigo 创建失败 / 发送失败两条路径是直接 return 的，会把用户剪贴板留在空值上。）
    //
    // ⚠ backup 为 None 时**不许置哨兵**（2026-08-07 对抗复核揪出的数据丢失）：
    // `get_text()` 对图片/文件这类非文本剪贴板返回 Err ⇒ backup=None；而哨兵 `set_text("")`
    // 走的是 EmptyClipboard + SetClipboardData，会把**所有格式**一并清掉；末尾的恢复又是
    // `if let Some(b) = backup`，None 时什么都不做 ⇒ 用户刚截的图/刚复制的文件被永久抹掉。
    // 好在这种状态下哨兵本来也是多余的：既然读不到文本，"之后读到了文本"本身就是新复制的信号。
    let sentinel = backup.is_some();
    let sel: Option<String> = 'grab: {
        if sentinel {
            match cb.set_text(String::new()) {
                // 空哨兵：之后剪贴板一旦非空，就说明目标程序确实复制进来了
                Ok(()) => note!("已置空哨兵"),
                Err(e) => note!("⚠ 置空哨兵失败: {e}（后续读到的可能是旧内容）"),
            }
        } else {
            note!("原内容非文本/为空 → 跳过置空哨兵（清空会毁掉图片/文件且恢复不回来；此时「读到文本」本身即哨兵）");
        }

        // ⚠ 判据基线必须在**我们自己写完哨兵之后**采样（2026-08-07 对抗复核揪出）：
        // 哨兵那一步必然把剪贴板序列号推高，拿写之前的 seq_start 当基线的话，
        // 下面失败取证里的 `seq_end == 基线` 分支**永远不成立** ⇒ 每次失败都打印
        // 「【已变】⇒ 目标确实写了剪贴板」，给出的是与事实相反的结论，把排查带偏。
        #[cfg(windows)]
        let seq_base = win::clip_seq();

        {
            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => {
                    note!("✗ Enigo::new 失败: {e}");
                    break 'grab None;
                }
            };

            // 物理已松，这里再显式松一次是补刀：清掉注入层可能残留的按下状态。
            let _ = enigo.key(Key::Alt, Release);
            let _ = enigo.key(Key::Meta, Release);
            let _ = enigo.key(Key::Shift, Release);
            let _ = enigo.key(Key::Control, Release);
            thread::sleep(Duration::from_millis(30));

            // 发 Ctrl+C 之前照一眼：前台是不是已经被 Alt 抬起顶进菜单模态（详见 menu_mode_info）。
            #[cfg(windows)]
            note!("{}", win::menu_mode_info());

            let r1 = enigo.key(Key::Control, Press);
            let r2 = enigo.key(Key::Unicode('c'), Click);
            let r3 = enigo.key(Key::Control, Release);
            let bad = [("Ctrl↓", &r1), ("C", &r2), ("Ctrl↑", &r3)]
                .iter()
                .filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}:{e}")))
                .collect::<Vec<_>>();
            if bad.is_empty() {
                note!("Ctrl+C 已发送(SendInput 全部成功)");
            } else {
                note!(
                    "✗ Ctrl+C 发送失败 [{}] ← 按键没进目标窗口，典型原因是 UIPI（目标进程完整性比本程序高）",
                    bad.join(", ")
                );
                break 'grab None;
            }
        }

        // Ctrl+C 异步：目标程序在自己消息循环里填剪贴板，轮询到非空更稳。预算见 POLL_BUDGET_MS。
        let poll_t0 = Instant::now();
        let mut sel = None;
        let mut last_err = String::new();
        let mut rounds = 0u32;
        let mut waited_ms: u64; // 每条退出路径都会先赋值（见下），故不给初值以免留下死赋值
        loop {
            if poll_t0.elapsed().as_millis() as u64 >= POLL_BUDGET_MS {
                waited_ms = poll_t0.elapsed().as_millis() as u64;
                break;
            }
            thread::sleep(Duration::from_millis(if rounds < POLL_FAST_ROUNDS {
                POLL_FAST_MS
            } else {
                POLL_SLOW_MS
            }));
            rounds += 1;
            waited_ms = poll_t0.elapsed().as_millis() as u64;
            match cb.get_text() {
                Ok(t) => {
                    if !t.is_empty() && looks_fresh(sentinel, seq_start) {
                        note!("第{rounds}轮({waited_ms}ms)读到文本 {}字符", t.chars().count());
                        // 决定性判据：这次如果超过旧版 300ms 窗口，说明**旧版在这台机器/这个程序上
                        // 必然失败**——"换台电脑就时灵时不灵"的直接物证。
                        if waited_ms > 300 {
                            note!(
                                "⚠ 本次取词耗时 {waited_ms}ms > 旧版 300ms 窗口 ⇒ 旧版在此处会判失败（根因①：窗口太短）"
                            );
                        }
                        sel = Some(t);
                        break;
                    }
                    last_err = "Ok(空串)".into();
                }
                Err(e) => last_err = format!("{e}"),
            }
        }

        if sel.is_none() {
            #[cfg(windows)]
            {
                let seq_end = win::clip_seq();
                note!(
                    "✗ {rounds}轮({waited_ms}ms)全空。剪贴板序列号 {seq_base}→{seq_end}（基线取自写完哨兵之后）{}；最后一次读取={last_err}",
                    if seq_end == seq_base {
                        "【放弃时未变】"
                    } else {
                        "【已变】⇒ 目标确实写了剪贴板，但读不到纯文本(格式不符/被占用)"
                    }
                );
                note!("放弃时剪贴板格式: {}", win::clip_formats());
                note!("放弃时剪贴板占用者: {}", win::clip_owner());
                note!("放弃时物理按键: {}", win::mods_state(keys.main_vk));

                // —— 迟到检查（2026-08-06 补的取证漏洞）——
                // 旧版只在放弃那一刻查一次序列号：目标"根本没复制"和"复制得比我们等得久"两种
                // 情况在那一刻**长得一模一样**（都显示未变），日志却断言"全程没往剪贴板写过任何
                // 东西"，会把排查引向完全相反的方向。这里再等一会儿补查一次，把两者彻底分开。
                thread::sleep(Duration::from_millis(LATE_CHECK_MS));
                let late_seq = win::clip_seq();
                let late_txt = cb.get_text().ok().filter(|t| !t.is_empty());
                match (late_seq != seq_end, &late_txt) {
                    (_, Some(t)) => note!(
                        "★迟到检查(+{LATE_CHECK_MS}ms)：序列号 {seq_end}→{late_seq}，**读到了 {}字符** ⇒ 目标复制成功、只是比 {POLL_BUDGET_MS}ms 预算还慢（根因①：窗口仍不够）",
                        t.chars().count()
                    ),
                    (true, None) => note!(
                        "★迟到检查(+{LATE_CHECK_MS}ms)：序列号 {seq_end}→{late_seq} 变了但仍读不到纯文本 ⇒ 目标写的不是文本格式，或剪贴板被别人占着（根因③）"
                    ),
                    (false, None) => note!(
                        "★迟到检查(+{LATE_CHECK_MS}ms)：序列号 {seq_end}→{late_seq} 依旧未变 ⇒ 目标程序确实一次都没往剪贴板写过（可排除「慢」，看根因②：按键根本没被当成 Ctrl+C）"
                    ),
                }
            }
            #[cfg(not(windows))]
            note!("✗ {rounds}轮({waited_ms}ms)全空，最后一次读取={last_err}");
        }
        sel
    };

    // 恢复原剪贴板。**这是全流程唯一会直接损害用户数据的一步**：失败＝用户的剪贴板被我们
    // 永久留在空哨兵上，下次 Ctrl+V 粘出空白，而且他完全不会想到是译点干的。
    // arboard 内部只重试约 30ms，而这一刻恰恰是抢占高峰（目标刚复制完，所有剪贴板监听者
    // 都在响应 WM_CLIPBOARDUPDATE），所以这里自己多试几轮；实在失败必须留证。
    if let Some(b) = backup {
        let mut tries = 1;
        let mut r = cb.set_text(b.clone());
        while r.is_err() && tries < 5 {
            thread::sleep(Duration::from_millis(120));
            r = cb.set_text(b.clone());
            tries += 1;
        }
        match r {
            Ok(()) if tries > 1 => note!("剪贴板已恢复（第{tries}次才成功，说明恢复那一刻被人占着）"),
            Ok(()) => {}
            Err(e) => {
                note!("✗✗ 恢复原剪贴板失败（试了{tries}次）: {e} ⇒ **用户的剪贴板被留在空哨兵上**，下次 Ctrl+V 会粘出空白");
                #[cfg(windows)]
                note!("恢复失败时剪贴板占用者: {}", win::clip_owner());
            }
        }
    }
    GrabResult {
        text: sel,
        keys_held: None,
        diag: d,
    }
}

/// 读到的这段文本，能不能当作"目标程序刚刚复制进来的"。
///
/// 置了哨兵就有强判据（非空即新）。没置哨兵时（原内容是图片/文件，见上）只能退而求其次：
/// 目标真复制了，剪贴板序列号必然递增；序列号没动却读到了文本，那是备份读失败时留下的
/// **旧内容**，拿它去翻译等于凭空翻了段用户没选的东西。
#[cfg(windows)]
fn looks_fresh(sentinel: bool, seq_start: u32) -> bool {
    sentinel || win::clip_seq() != seq_start
}

#[cfg(not(windows))]
fn looks_fresh(_sentinel: bool, _seq_start: u32) -> bool {
    true
}

#[cfg(windows)]
mod win {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, CountClipboardFormats, EnumClipboardFormats, GetClipboardFormatNameW,
        GetClipboardSequenceNumber, GetOpenClipboardWindow, OpenClipboard,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetKeyboardLayout, MapVirtualKeyExW, VkKeyScanExW, MAPVK_VK_TO_VSC_EX,
        VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowTextW, GetWindowThreadProcessId,
        GUITHREADINFO, GUI_INMENUMODE,
    };

    /// 必须等到物理松开的**修饰键**。Alt 没松 → 发出去变 Alt+Ctrl+C（实测收不到）；
    /// Shift/Win 会把 Ctrl+C 变成别的组合。Ctrl 不必等——它按着正是我们要的。
    ///
    /// 这四个与用户设了什么热键**无关**：它们本来就不该在划词那一刻按着，多等一下零成本，
    /// 而少等一个就可能让 Ctrl+C 变味。热键的主键另外由参数传入（见 `still_down`）。
    const WATCH_MODS: [(&str, i32); 4] = [
        ("Alt", VK_MENU.0 as i32),
        ("Shift", VK_SHIFT.0 as i32),
        ("LWin", VK_LWIN.0 as i32),
        ("RWin", VK_RWIN.0 as i32),
    ];

    fn is_down(vk: i32) -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
    }

    /// 此刻仍按着、且必须等它松开的键。`main_vk = 0` 表示没有可等的主键。
    fn still_down(main_vk: i32) -> Vec<String> {
        let mut v: Vec<String> = WATCH_MODS
            .iter()
            .filter(|(_, vk)| is_down(*vk))
            .map(|(n, _)| (*n).to_string())
            .collect();
        // ⚠ 主键必须与"实际注册成功的热键"一致，否则等的是一个没人按的键（永远显示已松开）。
        if main_vk != 0 && is_down(main_vk) {
            v.push(format!("主键(vk={main_vk:#04x})"));
        }
        v
    }

    /// 轮询等这些键全部物理松开。返回 (是否等到, 实际等待毫秒, 超时时仍按着的键)。
    pub fn wait_keys_released(main_vk: i32, max_ms: u64) -> (bool, u64, String) {
        let t = Instant::now();
        loop {
            let still = still_down(main_vk);
            let waited = t.elapsed().as_millis() as u64;
            if still.is_empty() {
                return (true, waited, String::new());
            }
            if waited >= max_ms {
                return (false, waited, still.join("+"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn clip_seq() -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    /// 取证：我们发出去的 Ctrl+C，实际会被翻译成哪个按键、用哪种方式注入。
    ///
    /// enigo 0.6.1 对 `Key::Unicode('c')` 的路径（win_impl.rs:428-473 + keycodes.rs:1049-1072）：
    ///   ① 用**前台窗口线程的键盘布局**调 `VkKeyScanExW('c', hkl)` 求虚拟键；
    ///   ② 再 `MapVirtualKeyExW` 转扫描码，并强制加 `KEYEVENTF_SCANCODE`，按**扫描码**注入。
    /// 而 `Key::Control` 走的是普通虚拟键路径（不带 SCANCODE 标志）。
    /// ⇒ 一对组合键用了**两种不同的注入方式**，且换算结果随前台程序/输入法状态而变——
    /// 这正是"这个软件里行、那个软件里不行"的一条通路，必须把实际换算值记下来才能判读。
    ///
    /// ⚠ enigo 自身隐患：`keycodes.rs:1072` 把 `VkKeyScanExW` 的返回值整个 `as u16`，
    ///   **没有剥掉高字节的 shift 状态**。小写 c 在常见布局上 shift 位为 0 才侥幸正确；
    ///   若某布局下 shift 位非 0，enigo 会把它连同虚拟键一起发出去 ⇒ 发的根本不是 C 键。
    pub fn input_route_info() -> String {
        unsafe {
            let tid = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let hkl = GetKeyboardLayout(tid);
            let hkl_num = hkl.0 as usize;
            let raw = VkKeyScanExW(b'c' as u16, hkl);
            if raw < 0 {
                return format!(
                    "按键换算: HKL={hkl_num:#x} 上 VkKeyScanExW('c')=-1 ⇒ ⚠该布局压根打不出 c，Ctrl+C 必然发错键"
                );
            }
            let vk = (raw as u16) & 0x00FF;
            let shift = ((raw as u16) >> 8) & 0x00FF;
            let scan = MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC_EX, Some(hkl));
            format!(
                "按键换算: HKL={hkl_num:#x} → VkKeyScanExW('c')={:#06x} (vk={vk:#04x}{}, shift位={shift:#04x}{}) → 扫描码={scan:#06x}；注入方式: Ctrl=虚拟键 / C=扫描码",
                raw as u16,
                if vk == 0x43 { "=VK_C ✓" } else { " ⚠不是 VK_C" },
                if shift == 0 { " ✓" } else { " ⚠非0：enigo 会连 shift 位一起当虚拟键发→发错键" },
            )
        }
    }

    /// 取证：前台线程此刻是不是已经进了**菜单模态态**。
    ///
    /// 机制（2026-08-06 受控实验实证）：`DefWindowProc` 在 **Alt 抬起**时发
    /// `WM_SYSCOMMAND / SC_KEYMENU` 打开菜单栏 / Office KeyTips，**当且仅当 Alt↓ 到 Alt↑
    /// 之间没有任何别的按键事件落进该输入队列**。而 `RegisterHotKey` 吞掉字母的 keydown、
    /// **不吞 keyup** ⇒
    ///   · 先松 W 再松 Alt：W↑ 落在中间 → 不开菜单（安全）
    ///   · 先松 Alt 再松 W：中间空空 → 菜单打开 → **我们随后发的 Ctrl+C 被菜单循环整个吃掉**
    /// 松手顺序取决于用户当次手感 ⇒ 天然间歇，且我们是"等全部松开才发"，正好落在菜单开了之后。
    /// 这条通路只在带菜单栏的传统 Win32 程序上实证过，Chromium/Electron/WPF/Qt 未验，故要靠日志判。
    pub fn menu_mode_info() -> String {
        unsafe {
            let tid = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let mut gti = GUITHREADINFO::default();
            gti.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(tid, &mut gti).is_err() {
                return "菜单态: 查询失败(GetGUIThreadInfo)".into();
            }
            let f = gti.flags.0;
            if f & GUI_INMENUMODE.0 != 0 {
                format!("菜单态: flags={f:#06x} ⚠**GUI_INMENUMODE** ⇒ 前台已进菜单模态，Ctrl+C 会被菜单循环吃掉（根因④：Alt 抬起触发 SC_KEYMENU）")
            } else {
                format!("菜单态: flags={f:#06x} 正常")
            }
        }
    }

    /// 剪贴板此刻有没有被别的进程占着。占用者会同时害两头：目标程序 SetClipboardData 失败、
    /// 我们也读不到。远控软件的剪贴板同步（ToDesk/向日葵）、剪贴板管理器、微信都会周期性抢占
    /// ——抢中就失败、抢不中就成功，表现为完全随机的间歇故障。
    pub fn clip_owner() -> String {
        unsafe {
            // windows-rs 把「返回 NULL」包装成 Err ⇒ Err 与空句柄都表示此刻没人占着剪贴板。
            let h = match GetOpenClipboardWindow() {
                Ok(h) if !h.0.is_null() => h,
                _ => return "无人占用".into(),
            };
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            format!("⚠ 正被占用 pid={pid} exe={}", proc_path(pid))
        }
    }

    /// 物理按键当前状态（GetAsyncKeyState 高位＝此刻真的按着）。
    pub fn mods_state(main_vk: i32) -> String {
        let s = still_down(main_vk);
        if s.is_empty() {
            "全部松开".into()
        } else {
            format!("仍按着 {}", s.join("+"))
        }
    }

    pub fn foreground_info() -> String {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return "前台窗口=无(GetForegroundWindow 返回 0)".into();
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
            let title: String = String::from_utf16_lossy(&buf[..n]);
            format!("前台窗口 pid={pid} exe={} 标题=\"{title}\"", proc_path(pid))
        }
    }

    fn proc_path(pid: u32) -> String {
        unsafe {
            match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => {
                    let mut buf = [0u16; 512];
                    let mut len = buf.len() as u32;
                    let ok = QueryFullProcessImageNameW(
                        h,
                        PROCESS_NAME_WIN32,
                        PWSTR(buf.as_mut_ptr()),
                        &mut len,
                    )
                    .is_ok();
                    let _ = CloseHandle(h);
                    if ok {
                        String::from_utf16_lossy(&buf[..len as usize])
                    } else {
                        "?(取路径失败)".into()
                    }
                }
                Err(e) => format!(
                    "?(OpenProcess 被拒 {e:?} ⚠很可能是提权进程→UIPI 会静默丢弃我们发的按键)"
                ),
            }
        }
    }

    /// 列出剪贴板当前有哪些格式：目标复制了但我们读不到纯文本时，这里能看出它到底写了什么。
    pub fn clip_formats() -> String {
        unsafe {
            if OpenClipboard(None).is_err() {
                return "打不开剪贴板(被其它进程占用)".into();
            }
            let n = CountClipboardFormats();
            let mut names = Vec::new();
            let mut f = EnumClipboardFormats(0);
            while f != 0 {
                let mut buf = [0u16; 80];
                let len = GetClipboardFormatNameW(f, &mut buf);
                names.push(if len > 0 {
                    String::from_utf16_lossy(&buf[..len as usize])
                } else {
                    match f {
                        1 => "CF_TEXT".into(),
                        13 => "CF_UNICODETEXT".into(),
                        7 => "CF_OEMTEXT".into(),
                        2 => "CF_BITMAP".into(),
                        8 => "CF_DIB".into(),
                        15 => "CF_HDROP".into(),
                        other => format!("#{other}"),
                    }
                });
                f = EnumClipboardFormats(f);
            }
            let _ = CloseClipboard();
            if n == 0 {
                "空(0 种格式)".into()
            } else {
                names.join(", ")
            }
        }
    }
}
