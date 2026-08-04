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
//! 所以改为**轮询等物理按键真正松开**再发 Ctrl+C。W 也必须等：我们发的 Ctrl 会和用户还按着的
//! W 凑成 Ctrl+W，在浏览器里就是关掉当前标签页（旧的 140ms 恰好挡住了这个，不能在改动中漏掉）。
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

/// 同一时刻只允许一个取词流程。连按 Alt+W 会让两个流程并发抢剪贴板：A 刚置空哨兵，
/// B 就把这个空串当作"原内容"备份走，随后互相覆盖——于是一次失败引来连按、连按又制造新的失败。
static GRABBING: AtomicBool = AtomicBool::new(false);

struct GrabGuard;
impl Drop for GrabGuard {
    fn drop(&mut self) {
        GRABBING.store(false, Ordering::Release);
    }
}

/// 取当前选中的文本 + 取证日志行；取不到时文本为 None。会尽力恢复原剪贴板文本。
pub fn grab_selection() -> (Option<String>, Vec<String>) {
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
        return (None, d);
    }
    let _guard = GrabGuard;

    #[cfg(windows)]
    note!("{}", win::foreground_info());

    let mut cb = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            note!("✗ Clipboard::new 失败: {e}");
            return (None, d);
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

    match cb.set_text(String::new()) {
        // 空哨兵：之后剪贴板一旦非空，就说明目标程序确实复制进来了
        Ok(()) => note!("已置空哨兵"),
        Err(e) => note!("⚠ 置空哨兵失败: {e}（后续读到的可能是旧内容）"),
    }

    {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => {
                note!("✗ Enigo::new 失败: {e}");
                return (None, d);
            }
        };

        // 关键：等 Alt/W/Shift/Win 物理松开再发 Ctrl+C（理由见文件头）。
        #[cfg(windows)]
        {
            let (ok, ms, still) = win::wait_keys_released(RELEASE_WAIT_MAX_MS);
            if ok {
                note!("等 {ms}ms → Alt/W/Shift/Win 已全部物理松开");
            } else {
                note!("⚠ 等满 {ms}ms 仍按着 [{still}]，只能硬发（大概率复制不到，请按完热键就松手）");
            }
        }
        #[cfg(not(windows))]
        thread::sleep(Duration::from_millis(140));

        // 物理已松，这里再显式松一次是补刀：清掉注入层可能残留的按下状态。
        let _ = enigo.key(Key::Alt, Release);
        let _ = enigo.key(Key::Meta, Release);
        let _ = enigo.key(Key::Shift, Release);
        let _ = enigo.key(Key::Control, Release);
        thread::sleep(Duration::from_millis(30));

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
            return (None, d);
        }
    }

    // Ctrl+C 异步：目标程序在自己消息循环里填剪贴板，轮询到非空更稳
    let mut sel = None;
    let mut last_err = String::new();
    let mut rounds = 0;
    for i in 0..15 {
        thread::sleep(Duration::from_millis(20));
        rounds = i + 1;
        match cb.get_text() {
            Ok(t) => {
                if !t.is_empty() {
                    note!("第{}轮({}ms)读到文本 {}字符", rounds, rounds * 20, t.chars().count());
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
                "✗ {}轮({}ms)全空。剪贴板序列号 {}→{} {}；最后一次读取={}",
                rounds,
                rounds * 20,
                seq_start,
                seq_end,
                if seq_end == seq_start {
                    "【未变】⇒ 目标程序全程没往剪贴板写过任何东西"
                } else {
                    "【已变】⇒ 目标确实写了剪贴板，但读不到纯文本(格式不符/被占用)"
                },
                last_err
            );
            note!("此刻剪贴板格式: {}", win::clip_formats());
            note!("此刻物理按键: {}", win::mods_state());
        }
        #[cfg(not(windows))]
        note!("✗ {rounds}轮全空，最后一次读取={last_err}");
    }

    if let Some(b) = backup {
        let _ = cb.set_text(b); // 读完再恢复
    }
    (sel, d)
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
        GetClipboardSequenceNumber, OpenClipboard,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    /// 必须等到物理松开才能发 Ctrl+C 的键。
    /// Alt/W = 热键 Alt+W 本身：Alt 没松 → 发出去变 Alt+Ctrl+C（实测收不到）；
    ///         W 没松 → 我们发的 Ctrl 与它凑成 Ctrl+W，在浏览器里会关掉当前标签页。
    /// Shift/Win 会把 Ctrl+C 变成别的组合。Ctrl 不必等——它按着正是我们要的。
    const WATCH: [(&str, i32); 5] = [
        ("Alt", VK_MENU.0 as i32),
        ("W", 0x57),
        ("Shift", VK_SHIFT.0 as i32),
        ("LWin", VK_LWIN.0 as i32),
        ("RWin", VK_RWIN.0 as i32),
    ];

    fn is_down(vk: i32) -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
    }

    fn still_down() -> Vec<&'static str> {
        WATCH.iter().filter(|(_, vk)| is_down(*vk)).map(|(n, _)| *n).collect()
    }

    /// 轮询等这些键全部物理松开。返回 (是否等到, 实际等待毫秒, 超时时仍按着的键)。
    pub fn wait_keys_released(max_ms: u64) -> (bool, u64, String) {
        let t = Instant::now();
        loop {
            let still = still_down();
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

    /// 物理按键当前状态（GetAsyncKeyState 高位＝此刻真的按着）。
    pub fn mods_state() -> String {
        let s = still_down();
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
