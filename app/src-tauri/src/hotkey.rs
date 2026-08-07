//! 全局热键的状态机：谁被注册上了、按下时该派给谁、要等哪个物理键松开。
//!
//! # 为什么需要一个"状态"，而不是直接问插件
//!
//! `GlobalShortcut::is_registered()` **不能用来判断热键是否真的可用**：官方文档明写，
//! 组合被**别的程序**占着时它照样返回 false / true 只看自己那张 HashMap。所以
//! 「这个键到底生没生效」的真相源只有一个：**我们调 `register()` 那一刻的返回值**。
//! 本模块就是把这个返回值记下来，供界面显示"未生效"、供划词逻辑决定等哪个键。
//!
//! # ⚠ 死锁：本模块存在的头号理由（2026-08-07 逐行读插件源码确认）
//!
//! `tauri-plugin-global-shortcut-2.3.2/src/lib.rs` 的事件分发长这样：
//! ```ignore
//! GlobalHotKeyEvent::set_event_handler(Some(move |e| {
//!     if let Some(shortcut) = shortcuts_.lock().unwrap().get(&e.id) {   // ← 持锁
//!         if let Some(handler) = &handler { handler(&app_handle, ...) } // ← 持锁调我们的 handler
//!     }
//! }));
//! ```
//! 也就是说**我们的 handler 是在插件持着它自己那把 Mutex 的情况下、在主线程 WndProc 里被调用的**。
//! 由此推出两条硬约束：
//!
//! 1. **handler 内绝不能调任何 `GlobalShortcut` 方法**（register / unregister / is_registered
//!    都要拿同一把非可重入 Mutex）⇒ 立刻硬死锁。
//! 2. **绝不能"持有本模块的锁"去调插件方法**。`register()`/`unregister()` 内部是
//!    `run_main_thread!`（把活儿丢给主线程并**阻塞等结果**）。若命令线程持着我们的锁去等主线程，
//!    而主线程正好在跑 handler、handler 又要拿我们的锁 ⇒ ABBA 死锁。
//!
//! 第 2 条靠"写代码时小心"是守不住的，所以这里**从结构上消灭它**：handler 走的那条路
//! （[`HotkeyState::action_of`] / [`HotkeyState::take_probe`]）**一把锁都不拿**，只读原子量。
//! 需要读写字符串的那些接口（快照 / 改键）都不在 handler 里调。
//!
//! # 另外两个上游坑（冷区已记，这里落成代码约束）
//! - **别用 `unregister_all()`**：它先 `mem::take` 清空自己的表、再做可能失败的 OS 注销；
//!   中途失败就会留下"OS 里还在、我们表里没了"的幽灵热键，之后重新注册会撞**自己**的
//!   AlreadyRegistered，表现为"莫名其妙一直冲突"且不重启无解。本模块只用单个 `unregister()`。
//! - **双重触发**：per-shortcut handler 与 builder 的全局 handler 是并列的两个 `if let`，
//!   都会跑。本项目只用 builder 全局 handler，注册一律走**不带 handler** 的 `register()`。

use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use tauri_plugin_global_shortcut::Shortcut;

use crate::hotkey_vk::code_to_vk;

/// 可自定义热键的两个动作。数值同时用作数组下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 截图翻译（默认 Alt+Q）
    Shot = 0,
    /// 划词翻译（默认 Alt+W）
    Selection = 1,
}

impl Action {
    pub const ALL: [Action; 2] = [Action::Shot, Action::Selection];

    /// 与前端、DB 键名一致的稳定标识。
    pub fn key(self) -> &'static str {
        match self {
            Action::Shot => "shot",
            Action::Selection => "selection",
        }
    }

    /// settings 表里的键名。
    pub fn setting_key(self) -> &'static str {
        match self {
            Action::Shot => "hotkey_shot",
            Action::Selection => "hotkey_selection",
        }
    }

    /// 出现在提示文案里的中文名。
    pub fn label(self) -> &'static str {
        match self {
            Action::Shot => "截图翻译",
            Action::Selection => "划词翻译",
        }
    }

    pub fn from_key(s: &str) -> Option<Action> {
        match s {
            "shot" => Some(Action::Shot),
            "selection" => Some(Action::Selection),
            _ => None,
        }
    }

    fn idx(self) -> usize {
        self as usize
    }
}

/// 出厂默认值。DB 里没有 / 值坏掉时回落到它。
pub fn default_accel(a: Action) -> &'static str {
    match a {
        Action::Shot => "alt+KeyQ",
        Action::Selection => "alt+KeyW",
    }
}

/// 给前端的一条热键状态。
///
/// `accel` 是"用户想要的"，`ok` 才是"真的生效了"——两者会不一致（组合被别的程序占着时），
/// 界面必须把这种不一致显示出来，否则用户会对着一个按下去没反应的徽章发呆。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HotkeyInfo {
    pub action: String,
    pub accel: String,
    pub ok: bool,
    pub error: String,
}

/// 改键的结果。失败时旧键仍然生效（见 [`HotkeyState`] 的注册次序说明）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HotkeySetResult {
    pub ok: bool,
    pub message: String,
    /// 操作结束后两个动作各自的实际状态，前端直接拿去覆盖，省一次往返。
    pub hotkeys: Vec<HotkeyInfo>,
}

/// 探测窗口：点了「测一下」之后，多久之内按热键算这次探测。
///
/// 太短用户来不及抬手去按，太长会让"上次没测成"的残留状态在下一次误命中。8 秒是折中。
pub const PROBE_WINDOW_MS: u64 = 8_000;

/// 单调时钟基点。`SystemTime` 会被改系统时间/NTP 校时带偏，探测窗口只关心相对时间。
static EPOCH: Lazy<Instant> = Lazy::new(Instant::now);

fn now_ms() -> u64 {
    EPOCH.elapsed().as_millis() as u64
}

/// `ids` 里表示"没有注册成功的热键"。`Shortcut::id()` 是 u32，永远撞不上这个值。
const ID_NONE: u64 = u64::MAX;

#[derive(Debug, Clone)]
struct Slot {
    /// 用户想要的规范串（= DB 值）。注册失败时它与 `active` 不一致，这是有意的。
    accel: String,
    /// 实际注册成功的组合。`None` = 这个动作当前**按下去不会有任何反应**。
    active: Option<Shortcut>,
    /// `active` 为 None 时的原因，直接给用户看。
    error: String,
}

pub struct HotkeyState {
    slots: Mutex<[Slot; 2]>,
    /// 【无锁快路径】实际注册成功的 `Shortcut::id()`，`ID_NONE` = 未注册。
    /// handler 只允许读这里，理由见模块头「死锁」一节。
    ids: [AtomicU64; 2],
    /// 【无锁快路径】划词热键的**主键**虚拟键码，供 `selection.rs` 等它物理松开。
    /// 0 = 没有可等的主键（热键没生效，或该 Code 不在 `hotkey_vk` 表里）。
    selection_main_vk: AtomicI32,
    /// 【无锁快路径】探测截止时刻（`now_ms()` 口径）。0 = 没在探测。
    probe_until: [AtomicU64; 2],
}

/// 进程内唯一的热键状态。
///
/// 为什么是全局而不是塞进 `AppState`：热键 handler 是插件在**主线程 WndProc、且持着它自己的锁**
/// 时调进来的（见模块头），那条路径上做的事越少越好。`app.state::<AppState>()` 要过 Tauri 的
/// 状态表，虽然实测不会死锁，但那是"目前不会"而不是"结构上不可能"。直接读一个静态量，
/// handler 就只剩几条原子读，没有任何"将来某次重构可能引入阻塞"的空间。
///
/// 初值是出厂默认键；`setup` 会立刻用 DB 里的值 + 真实注册结果覆盖掉。
static GLOBAL: Lazy<HotkeyState> = Lazy::new(|| {
    HotkeyState::new(
        default_accel(Action::Shot),
        default_accel(Action::Selection),
    )
});

/// 取进程内唯一的热键状态。
pub fn global() -> &'static HotkeyState {
    &GLOBAL
}

impl HotkeyState {
    /// 用 DB 里读到的值构造。**此刻还没有注册任何东西**，两个动作都是"未生效"，
    /// 由 `setup` 随后逐个调 [`HotkeyState::record`] 把真实注册结果填进来。
    pub fn new(shot_accel: &str, selection_accel: &str) -> Self {
        let mk = |accel: &str| Slot {
            accel: accel.to_string(),
            active: None,
            error: "尚未注册".into(),
        };
        HotkeyState {
            slots: Mutex::new([mk(shot_accel), mk(selection_accel)]),
            ids: [AtomicU64::new(ID_NONE), AtomicU64::new(ID_NONE)],
            selection_main_vk: AtomicI32::new(0),
            probe_until: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    // -----------------------------------------------------------------------
    // 无锁快路径（handler 只能用这一组）
    // -----------------------------------------------------------------------

    /// 热键事件到了，判断是哪个动作。**这是 handler 唯一允许调的查询**。
    pub fn action_of(&self, id: u32) -> Option<Action> {
        let id = id as u64;
        Action::ALL
            .into_iter()
            .find(|a| self.ids[a.idx()].load(Ordering::SeqCst) == id)
    }

    /// 若该动作正在探测窗口内：**消费掉**探测态并返回 true（调用方此时只回报、不执行真实动作）。
    ///
    /// 用 `swap(0)` 而不是"读了再清"：两次按键挤在一起时也只会有一次被算作探测。
    pub fn take_probe(&self, a: Action) -> bool {
        let until = self.probe_until[a.idx()].swap(0, Ordering::SeqCst);
        if until == 0 {
            return false;
        }
        if now_ms() <= until {
            true
        } else {
            // 过期的残留：清掉即可，这次按键当成正常触发。
            false
        }
    }

    /// 开一次探测窗口。
    pub fn arm_probe(&self, a: Action) {
        self.probe_until[a.idx()].store(now_ms() + PROBE_WINDOW_MS, Ordering::SeqCst);
    }

    /// 关掉探测窗口（用户中途去干别的了）。
    ///
    /// 必须有这个：探测窗口一旦开着没人消费，窗口内**第一次**按该热键会被当成探测吞掉
    /// ——只回报、不执行。而那时前端早已不在等待态、回报被丢弃 ⇒ 界面完全没反应，
    /// 用户会得出"这个键也被别的程序占了"的相反结论，正好是本功能要排除的误判。
    pub fn disarm_probe(&self, a: Action) {
        self.probe_until[a.idx()].store(0, Ordering::SeqCst);
    }

    /// 划词热键的主键虚拟键码；0 = 无需等待任何主键。
    ///
    /// 划词取词在发 `Ctrl+C` 之前必须等这个键物理松开，否则我们发的 Ctrl 会和用户还按着的
    /// 主键凑成别的组合（`Ctrl+W`=关标签页、`Ctrl+V`=**把剪贴板粘进用户文档**）。详见 `selection.rs`。
    pub fn selection_main_vk(&self) -> i32 {
        self.selection_main_vk.load(Ordering::SeqCst)
    }

    // -----------------------------------------------------------------------
    // 需要加锁的接口（⚠ 绝不能在持锁期间调用任何 GlobalShortcut 方法）
    // -----------------------------------------------------------------------

    /// 记录一次注册尝试的结果。`active=None` 表示没注册上，`error` 说明原因。
    ///
    /// ⚠ 调用方必须**已经调完** `register()`/`unregister()`，本函数只负责记账；
    /// 反过来（持着本锁去调插件）会 ABBA 死锁，见模块头。
    pub fn record(&self, a: Action, accel: String, active: Option<Shortcut>, error: String) {
        // 改键 = 这个动作的注册结果变了 ⇒ 上一轮「测一下」开的窗口说的是**旧键**，作废。
        // 不清的话，新键的第一次按下会被 take_probe 吃成"探测命中"（只回报不执行），
        // 而前端此时已退出等待态、回报被丢弃 ⇒ 刚设好的键按下去什么都不发生。
        self.probe_until[a.idx()].store(0, Ordering::SeqCst);
        // 原子量先更新还是后更新都可以（handler 拿到旧值最多是一次派发落空），
        // 但先更新原子量能保证"注册成功后立刻按下就能响应"。
        self.ids[a.idx()].store(
            active.map(|s| s.id() as u64).unwrap_or(ID_NONE),
            Ordering::SeqCst,
        );
        if a == Action::Selection {
            self.selection_main_vk
                .store(active.and_then(main_key_vk).unwrap_or(0), Ordering::SeqCst);
        }
        if let Ok(mut g) = self.slots.lock() {
            g[a.idx()] = Slot {
                accel,
                active,
                error,
            };
        }
    }

    /// 当前实际注册成功的组合（用于改键时注销旧的）。
    pub fn active(&self, a: Action) -> Option<Shortcut> {
        self.slots.lock().ok().and_then(|g| g[a.idx()].active)
    }

    /// 用户想要的规范串。
    pub fn accel(&self, a: Action) -> String {
        self.slots
            .lock()
            .map(|g| g[a.idx()].accel.clone())
            .unwrap_or_else(|_| default_accel(a).to_string())
    }

    /// 另一个动作是否已经**实际占用**了这个组合。
    ///
    /// 只看 `active` 不看 `accel`：另一个动作那串没注册上的话，它并没有占住任何东西，
    /// 此时不该拦着用户把这个组合用到本动作上。
    pub fn taken_by_other(&self, a: Action, s: &Shortcut) -> Option<Action> {
        let g = self.slots.lock().ok()?;
        Action::ALL
            .into_iter()
            .find(|&o| o != a && g[o.idx()].active.as_ref() == Some(s))
    }

    /// 给前端的完整快照。
    pub fn snapshot(&self) -> Vec<HotkeyInfo> {
        let g = match self.slots.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        Action::ALL
            .into_iter()
            .map(|a| {
                let s = &g[a.idx()];
                HotkeyInfo {
                    action: a.key().to_string(),
                    accel: s.accel.clone(),
                    ok: s.active.is_some(),
                    error: s.error.clone(),
                }
            })
            .collect()
    }
}

/// 一个组合的**主键**对应的 Win32 虚拟键码。
///
/// `Code` 的 `Display` 逐字给出 W3C code（`KeyW` / `NumpadEnter` / `ArrowUp`，
/// 见 keyboard-types-0.7.0/src/code.rs:465），正是 `hotkey_vk` 那张表的输入格式。
fn main_key_vk(s: Shortcut) -> Option<i32> {
    code_to_vk(&s.key.to_string())
}

/// 校验并规范化用户提交（或 DB 里读到）的组合串。
///
/// 返回 `(规范串, Shortcut)`。规范串一律取 `into_string()` 的产物，**不是**用户传进来那个：
/// 上游 `parse_hotkey` 对 token 是大小写不敏感的（`"ALT+ctrl+w"` 也能 parse），若把原串存进 DB，
/// 下次跟事件回调带回来的 `into_string()` 结果按字符串比就会比不中。
pub fn parse_accel(input: &str) -> Result<(String, Shortcut), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("没收到组合键".into());
    }
    let sc: Shortcut = raw
        .parse()
        .map_err(|e| format!("认不出这个组合（{e}）"))?;

    // 硬禁无修饰单键。上游 parse 是**接受** "KeyQ" 的，而且真的会注册成全局热键 ——
    // 那之后用户每敲一次这个字母都会触发翻译，正常打字直接全废。前端已经拦了一道，
    // 这里再拦一道：DB 可以被手改，旧版本配置也可能漂进来。
    if sc.mods.is_empty() {
        return Err("至少要带一个 Ctrl / Alt / Shift / Win".into());
    }
    Ok((sc.into_string(), sc))
}

/// 从 DB 值取一个可用的组合；值坏掉时静默回落到出厂默认（并告诉调用方回落了）。
///
/// 开机时宁可用默认键也不能"什么都不注册"：用户不会去看日志，只会觉得软件坏了。
pub fn parse_or_default(a: Action, stored: Option<&str>) -> (String, Shortcut, Option<String>) {
    if let Some(v) = stored {
        match parse_accel(v) {
            Ok((accel, sc)) => return (accel, sc, None),
            Err(e) => {
                let (accel, sc) = parse_accel(default_accel(a)).expect("出厂默认键必须可解析");
                return (
                    accel,
                    sc,
                    Some(format!("配置里的「{v}」不可用（{e}），已回落到默认键")),
                );
            }
        }
    }
    let (accel, sc) = parse_accel(default_accel(a)).expect("出厂默认键必须可解析");
    (accel, sc, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    fn sc(m: Modifiers, c: Code) -> Shortcut {
        Shortcut::new(Some(m), c)
    }

    #[test]
    fn parse_round_trips_to_canonical_form() {
        let (accel, s) = parse_accel("alt+KeyW").unwrap();
        assert_eq!(accel, "alt+KeyW");
        assert_eq!(s, sc(Modifiers::ALT, Code::KeyW));
    }

    /// 上游 parse 大小写不敏感 ⇒ 必须以 `into_string()` 为准做规范化，否则存进 DB 的串
    /// 和事件回调带回来的串对不上。
    #[test]
    fn parse_normalizes_case_and_order() {
        assert_eq!(parse_accel("ALT+ctrl+w").unwrap().0, "control+alt+KeyW");
        assert_eq!(parse_accel("Ctrl+Shift+Q").unwrap().0, "shift+control+KeyQ");
        // 别名也归一：CmdOrCtrl 在 Windows 上就是 Control
        assert_eq!(parse_accel("CmdOrCtrl+KeyA").unwrap().0, "control+KeyA");
    }

    #[test]
    fn bare_key_is_rejected() {
        // 上游其实 parse 得过（这正是危险之处）：注册成功后每敲一次 Q 都会触发翻译。
        assert!("KeyQ".parse::<Shortcut>().is_ok(), "前提：上游确实接受单键");
        let e = parse_accel("KeyQ").unwrap_err();
        assert!(e.contains("至少要带"), "实际错误：{e}");
    }

    #[test]
    fn garbage_is_rejected() {
        for bad in ["", "   ", "alt+", "alt+Fn", "alt+ContextMenu", "肯定不是键"] {
            assert!(parse_accel(bad).is_err(), "{bad:?} 不该通过");
        }
    }

    /// 前端 `hotkey.ts` 的 `MAIN_KEY_WHITELIST` 必须 ⊆ 后端 `parse_key` 的覆盖面。
    /// 多了就是"用户录得进、后端 parse 直接失败、界面上只显示一个莫名其妙的『未生效』"。
    ///
    /// 这条不靠人眼核对：直接把前端那份表读进来逐条跑一遍后端解析。
    /// ⚠ 若前端文件被挪走/改了写法，这个测试会**编译或断言失败**——那是对的，
    ///   两边的键表本来就必须一起改。
    #[test]
    fn frontend_whitelist_is_all_parseable_by_backend() {
        const TS: &str = include_str!("../../src/lib/hotkey.ts");
        let body = TS
            .split("MAIN_KEY_WHITELIST = new Set<string>([")
            .nth(1)
            .expect("前端白名单的写法变了，请同步本测试")
            .split("]);")
            .next()
            .unwrap();

        // 显式列出的键：每行一个 "Xxx",；range(...) 生成的四族单独展开。
        let mut codes: Vec<String> = body
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                t.strip_prefix('"')?
                    .split('"')
                    .next()
                    .map(|s| s.to_string())
            })
            .filter(|s| !s.is_empty())
            .collect();
        for c in 'A'..='Z' {
            codes.push(format!("Key{c}"));
        }
        for d in 0..=9 {
            codes.push(format!("Digit{d}"));
            codes.push(format!("Numpad{d}"));
        }
        for n in 1..=24 {
            codes.push(format!("F{n}"));
        }
        // 117 = 字母 26 + 数字 10 + F1-F24 24 + 小键盘数字 10 + 显式列出的 47。
        // 与 `hotkey_vk.rs` 那张 VK 表条目数相同**不是巧合**：两张表覆盖的就是同一批键。
        assert_eq!(
            codes.len(),
            117,
            "白名单条目数变了（现 {}），先确认前端改了什么再改这个数",
            codes.len()
        );

        for code in &codes {
            let input = format!("alt+{code}");
            let (accel, _) = parse_accel(&input)
                .unwrap_or_else(|e| panic!("前端允许录 {code}，后端却解析不了：{e}"));
            assert_eq!(
                accel, input,
                "{code} 的规范串来回不一致 ⇒ 存进 DB 的串会跟事件回调对不上"
            );
        }
    }

    /// 白名单里的每个键都得能查到 VK，否则划词改成那个键之后就没有"要等的主键"，
    /// 会退回到"Ctrl+主键"误触发的老坑。
    #[test]
    fn every_whitelisted_main_key_has_a_vk() {
        for code in ["KeyW", "KeyQ", "F5", "Digit1", "Numpad0", "ArrowUp", "Slash"] {
            let (_, s) = parse_accel(&format!("alt+{code}")).unwrap();
            assert!(main_key_vk(s).is_some(), "{code} 查不到 VK");
        }
        // 默认键必须能查到（划词等键全靠它）
        let (_, w) = parse_accel(default_accel(Action::Selection)).unwrap();
        assert_eq!(main_key_vk(w), Some(0x57), "划词默认键的主键应是 VK_W");
    }

    #[test]
    fn dispatch_matches_only_registered_ids() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        let q = sc(Modifiers::ALT, Code::KeyQ);
        let w = sc(Modifiers::ALT, Code::KeyW);
        // 还没注册 → 谁都不认
        assert_eq!(st.action_of(q.id()), None);

        st.record(Action::Shot, "alt+KeyQ".into(), Some(q), String::new());
        st.record(Action::Selection, "alt+KeyW".into(), Some(w), String::new());
        assert_eq!(st.action_of(q.id()), Some(Action::Shot));
        assert_eq!(st.action_of(w.id()), Some(Action::Selection));
        assert_eq!(st.action_of(sc(Modifiers::ALT, Code::KeyE).id()), None);

        // 注册失败（active=None）后必须立刻停止派发，否则会出现"界面说未生效、按下去却有反应"
        st.record(Action::Shot, "alt+KeyQ".into(), None, "被占用".into());
        assert_eq!(st.action_of(q.id()), None);
        assert_eq!(st.action_of(w.id()), Some(Action::Selection), "别牵连另一个");
    }

    #[test]
    fn selection_main_vk_follows_the_registered_key() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        assert_eq!(st.selection_main_vk(), 0, "没注册时无键可等");

        let w = sc(Modifiers::ALT, Code::KeyW);
        st.record(Action::Selection, "alt+KeyW".into(), Some(w), String::new());
        assert_eq!(st.selection_main_vk(), 0x57);

        // 改成 Ctrl+Alt+V：必须跟着变成 VK_V，否则发 Ctrl+C 时 V 还按着 ⇒ Ctrl+V 粘贴
        let v = sc(Modifiers::CONTROL | Modifiers::ALT, Code::KeyV);
        st.record(Action::Selection, "control+alt+KeyV".into(), Some(v), String::new());
        assert_eq!(st.selection_main_vk(), 0x56);

        // 截图键换了不该影响划词的等键
        st.record(
            Action::Shot,
            "alt+KeyZ".into(),
            Some(sc(Modifiers::ALT, Code::KeyZ)),
            String::new(),
        );
        assert_eq!(st.selection_main_vk(), 0x56);

        // 注册失败 → 没有键可等
        st.record(Action::Selection, "control+alt+KeyV".into(), None, "被占".into());
        assert_eq!(st.selection_main_vk(), 0);
    }

    #[test]
    fn probe_is_one_shot_and_per_action() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        assert!(!st.take_probe(Action::Shot), "没开探测时不能命中");

        st.arm_probe(Action::Shot);
        assert!(!st.take_probe(Action::Selection), "探测不能串到另一个动作");
        assert!(st.take_probe(Action::Shot));
        assert!(!st.take_probe(Action::Shot), "只算一次，第二次按属正常触发");
    }

    /// 点了「测一下」却没按键、转手去改键 —— 那个为**旧键**开的窗口必须作废，
    /// 否则新键的第一次按下会被吞成"探测命中"（只回报不执行），界面上完全没反应，
    /// 用户会误判成"新键也被占了"。2026-08-07 对抗复核揪出。
    #[test]
    fn rekey_clears_a_pending_probe() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        st.record(
            Action::Shot,
            "alt+KeyQ".into(),
            Some(sc(Modifiers::ALT, Code::KeyQ)),
            String::new(),
        );
        st.arm_probe(Action::Shot); // 点了「测一下」，没按键
        let n = sc(Modifiers::CONTROL | Modifiers::ALT, Code::KeyZ);
        st.record(Action::Shot, "control+alt+KeyZ".into(), Some(n), String::new());
        assert!(!st.take_probe(Action::Shot), "新键第一次按下不该被当成探测吞掉");
    }

    /// 兄弟路径：录制中按 Esc 取消 / 切走设置页时根本不会调 record，得能单独撤。
    #[test]
    fn probe_can_be_cancelled() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        st.arm_probe(Action::Selection);
        st.disarm_probe(Action::Selection);
        assert!(!st.take_probe(Action::Selection));
        // 撤销是幂等的，没开也能撤
        st.disarm_probe(Action::Shot);
        assert!(!st.take_probe(Action::Shot));
    }

    #[test]
    fn taken_by_other_only_counts_actually_registered() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        let q = sc(Modifiers::ALT, Code::KeyQ);
        st.record(Action::Shot, "alt+KeyQ".into(), Some(q), String::new());
        assert_eq!(st.taken_by_other(Action::Selection, &q), Some(Action::Shot));
        assert_eq!(st.taken_by_other(Action::Shot, &q), None, "自己不算占自己");

        // 截图键没注册上时，它并没有占住任何东西 ⇒ 不该拦着划词用这个组合
        st.record(Action::Shot, "alt+KeyQ".into(), None, "被占".into());
        assert_eq!(st.taken_by_other(Action::Selection, &q), None);
    }

    #[test]
    fn snapshot_reports_desired_and_effective_separately() {
        let st = HotkeyState::new("alt+KeyQ", "alt+KeyW");
        st.record(Action::Shot, "alt+KeyQ".into(), None, "被别的程序占着".into());
        let snap = st.snapshot();
        assert_eq!(snap.len(), 2);
        let shot = snap.iter().find(|h| h.action == "shot").unwrap();
        assert_eq!(shot.accel, "alt+KeyQ", "仍显示用户想要的那个");
        assert!(!shot.ok);
        assert_eq!(shot.error, "被别的程序占着");
    }

    #[test]
    fn stored_value_falls_back_to_default_when_broken() {
        let (accel, sc_, warn) = parse_or_default(Action::Selection, Some("完全不是键"));
        assert_eq!(accel, "alt+KeyW");
        assert_eq!(main_key_vk(sc_), Some(0x57));
        assert!(warn.unwrap().contains("已回落到默认键"));

        let (accel, _, warn) = parse_or_default(Action::Shot, Some("control+shift+KeyQ"));
        assert_eq!(accel, "shift+control+KeyQ", "合法值要按规范串归一");
        assert!(warn.is_none());

        let (accel, _, warn) = parse_or_default(Action::Shot, None);
        assert_eq!(accel, "alt+KeyQ");
        assert!(warn.is_none());
    }

    #[test]
    fn action_key_names_match_setting_keys() {
        // 前端按 action.key 索引，DB 按 setting_key 存；两者错开过一次就到处对不上。
        assert_eq!(Action::from_key("shot"), Some(Action::Shot));
        assert_eq!(Action::from_key("selection"), Some(Action::Selection));
        assert_eq!(Action::from_key("nope"), None);
        for a in Action::ALL {
            assert_eq!(a.setting_key(), format!("hotkey_{}", a.key()));
        }
    }
}
