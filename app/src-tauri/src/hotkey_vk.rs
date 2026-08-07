//! W3C `KeyboardEvent.code` → Win32 虚拟键码（VK）。**这是 global-hotkey 私有表的手抄副本。**
//!
//! # 为什么非抄不可
//! 划词取词在发 Ctrl+C 之前，必须等用户设的那个热键的**主键物理松开**（根因见 `selection.rs`
//! 文件头：主键还按着时，我们发的 Ctrl 会和它凑成别的组合）。热键写死 Alt+W 的年代只要盯着
//! 0x57 一个常量就行；改成可自定义之后，用户设成 `Ctrl+Alt+V` 时不等 V 松开，发出去的就是
//! **Ctrl+V —— 直接把剪贴板内容粘进用户正在编辑的文档**，比取词失败恶劣得多。
//! 要等就得知道 Code 对应哪个 VK，才能拿去问 `GetAsyncKeyState`。
//!
//! global-hotkey 内部有现成的 `key_to_vk`，但它是 **私有 fn**（`fn`，非 `pub fn`，crate 外拿不到），
//! 所以只能照抄一份。抄源：
//! `~/.cargo/registry/src/index.crates.io-*/global-hotkey-0.8.0/src/platform_impl/windows/mod.rs`
//! 的 `key_to_vk`（204-325 行）；VK 数值取自 `windows-sys` 的 `Win32::UI::Input::KeyboardAndMouse`。
//!
//! # ⚠ 这张表必须与上游保持同步
//! **当前核对版本：global-hotkey 0.8.0（经 tauri-plugin-global-shortcut 2.3.2 引入），2026-08-07 逐条核对。**
//! 升级这两个 crate 中任意一个之后，都要重新打开上游 `key_to_vk` 复核，并更新这里的版本号与行号。
//! 一旦两边不一致，后果不是"报错"而是**静默走错**：上游按 A 键注册热键、我们却去等 B 键松开，
//! B 早就是松的 ⇒ 立刻发 Ctrl+C ⇒ 又回到"Ctrl+主键"误触发的老坑上，且日志里一切正常。
//!
//! # 抄写原则：**照抄，不修正**
//! 上游有几处明显不合 W3C 语义的映射（`NumpadEqual => VK_E` 等，详见下方各条注释），
//! 这里**一律原样保留**。理由：本表的用途不是"求语义正确的 VK"，而是"复现 global-hotkey
//! 实际拿去 `RegisterHotKey` 的那个 VK"。热键就是按那个 VK 注册的、用户按下去能触发的也正是
//! 那个物理键，`GetAsyncKeyState` 查的自然也得是同一个。"修正"反而会让两边错开（见上一段）。
//! 同理，这张表对键盘布局也是"跟着上游一起naive"：AZERTY 上 `KeyW` 依旧映射 VK_W——
//! 注册和等待用的是同一个值，就不会出错。
//!
//! # 不收录修饰键
//! `ShiftLeft`/`ControlLeft`/`AltLeft`/`MetaLeft` 等一律返回 `None`：上游表里就没有它们
//! （修饰键走 `Modifiers`，不可能当主键），而 `selection.rs` 等修饰键松开另有 `VK_MENU`/
//! `VK_SHIFT`/`VK_LWIN`/`VK_RWIN` 那条路，不经过本表。

/// W3C `KeyboardEvent.code` → Win32 虚拟键码。未收录返回 `None`。
///
/// 返回 `i32` 而非 `u16`，是为了能直接喂 `GetAsyncKeyState(vk: i32)`，省掉调用处的 `as i32`。
///
/// **`None` 的含义不是"这个键没有虚拟键码"，而是"global-hotkey 也不认它"**：上游表返回 None
/// 时它压根不会去调 `RegisterHotKey`，热键注册直接失败。所以调用方拿到 None 时，对应的热键
/// 本来就没生效，也就不存在"要等的主键"——按"无需等待"处理即可，不必视作错误。
pub fn code_to_vk(code: &str) -> Option<i32> {
    // 先走"同族连号"规则（KeyA-Z / Digit0-9 / F1-F24 / Numpad0-9 共 70 条）。
    // 这 70 条在上游是 70 行一一列举（mod.rs:206-241、270-293、295-304），这里压成 4 条规则：
    // 它们的连号性是 Win32 的既定事实（VK_A..VK_Z 就等于 ASCII 'A'..'Z'），写成规则比手抄
    // 70 行更不容易抄漏抄错。**但单测仍然逐条断言**，不靠"应该是连号的"这种想当然。
    if let Some(vk) = contiguous_family(code) {
        return Some(vk);
    }

    Some(match code {
        // —— OEM 标点（上游 mod.rs:242-252）——
        // 全表最容易抄错的一段：上游按 Equal/Comma/Minus/Period/Semicolon/Slash/… 的顺序写，
        // 名字和数值毫无对应关系（Semicolon 是 OEM_1、Slash 是 OEM_2、Backquote 是 OEM_3）。
        // 这里**改按 VK 数值升序重排**，好让 0xBA…0xC0 连号、0xDB…0xDE 连号一眼可验；
        // 每行都标了它在上游源码的行号，便于逐条回查。
        "Semicolon" => 0xBA,    // VK_OEM_1      ← mod.rs:246
        "Equal" => 0xBB,        // VK_OEM_PLUS   ← mod.rs:242
        "Comma" => 0xBC,        // VK_OEM_COMMA  ← mod.rs:243
        "Minus" => 0xBD,        // VK_OEM_MINUS  ← mod.rs:244
        "Period" => 0xBE,       // VK_OEM_PERIOD ← mod.rs:245
        "Slash" => 0xBF,        // VK_OEM_2      ← mod.rs:247
        "Backquote" => 0xC0,    // VK_OEM_3      ← mod.rs:248
        "BracketLeft" => 0xDB,  // VK_OEM_4      ← mod.rs:249
        "Backslash" => 0xDC,    // VK_OEM_5      ← mod.rs:250
        "BracketRight" => 0xDD, // VK_OEM_6      ← mod.rs:251
        "Quote" => 0xDE,        // VK_OEM_7      ← mod.rs:252

        // —— 编辑与导航（上游 mod.rs:253-269，此处保持上游顺序）——
        "Backspace" => 0x08,   // VK_BACK     ← mod.rs:253
        "Tab" => 0x09,         // VK_TAB      ← mod.rs:254
        "Space" => 0x20,       // VK_SPACE    ← mod.rs:255
        "Enter" => 0x0D,       // VK_RETURN   ← mod.rs:256
        "CapsLock" => 0x14,    // VK_CAPITAL  ← mod.rs:257
        "Escape" => 0x1B,      // VK_ESCAPE   ← mod.rs:258
        "PageUp" => 0x21,      // VK_PRIOR    ← mod.rs:259（PageUp 的 VK 叫 PRIOR，别被名字带偏）
        "PageDown" => 0x22,    // VK_NEXT     ← mod.rs:260（同上，叫 NEXT）
        "End" => 0x23,         // VK_END      ← mod.rs:261
        "Home" => 0x24,        // VK_HOME     ← mod.rs:262
        "ArrowLeft" => 0x25,   // VK_LEFT     ← mod.rs:263
        "ArrowUp" => 0x26,     // VK_UP       ← mod.rs:264
        "ArrowRight" => 0x27,  // VK_RIGHT    ← mod.rs:265
        "ArrowDown" => 0x28,   // VK_DOWN     ← mod.rs:266
        "PrintScreen" => 0x2C, // VK_SNAPSHOT ← mod.rs:267（VK 名是 SNAPSHOT，不是 PRINTSCREEN）
        "Insert" => 0x2D,      // VK_INSERT   ← mod.rs:268
        "Delete" => 0x2E,      // VK_DELETE   ← mod.rs:269

        // —— 小键盘的非数字键（上游 mod.rs:305-311）——
        "NumpadMultiply" => 0x6A, // VK_MULTIPLY ← mod.rs:310
        "NumpadAdd" => 0x6B,      // VK_ADD      ← mod.rs:305
        "NumpadSubtract" => 0x6D, // VK_SUBTRACT ← mod.rs:311
        "NumpadDecimal" => 0x6E,  // VK_DECIMAL  ← mod.rs:306
        "NumpadDivide" => 0x6F,   // VK_DIVIDE   ← mod.rs:307
        // ↓ 上游怪癖之一：小键盘回车与主键盘回车共用 VK_RETURN（mod.rs:308）。
        //   这是 Win32 本来的设计（两者只靠扫描码的 extended 位区分），照抄即正确：
        //   GetAsyncKeyState(VK_RETURN) 对两个回车键都成立，等谁松开都不会漏。
        "NumpadEnter" => 0x0D, // VK_RETURN ← mod.rs:308
        // ↓ 上游怪癖之二（**看着像 bug，但必须照抄**）：`Code::NumpadEqual => VK_E`（mod.rs:309）。
        //   VK_E = 0x45 是**字母 E 键**，而小键盘等号本该是 VK_OEM_NEC_EQUAL(0x92)。
        //   后果是：用户在设置里选了 NumpadEqual，上游其实注册的是字母 E —— 于是真正能触发
        //   热键的物理键就是 E，用户按下去的也是 E。此时等 VK_E 松开才是对的；若我们"修正"成
        //   0x92，等的将是一个从未被按下的键（永远显示已松开）⇒ 立刻发 Ctrl+C ⇒ 用户还按着 E ⇒
        //   变成 Ctrl+E（浏览器里 = 跳到地址栏搜索）。修正反而制造 bug。
        "NumpadEqual" => 0x45, // VK_E ← mod.rs:309（上游原样，勿改，理由见上）

        // —— 锁定键与媒体键（上游 mod.rs:294、312-322）——
        "NumLock" => 0x90,            // VK_NUMLOCK ← mod.rs:294
        "ScrollLock" => 0x91,         // VK_SCROLL  ← mod.rs:312
        "AudioVolumeMute" => 0xAD,    // VK_VOLUME_MUTE      ← mod.rs:315
        "AudioVolumeDown" => 0xAE,    // VK_VOLUME_DOWN      ← mod.rs:313
        "AudioVolumeUp" => 0xAF,      // VK_VOLUME_UP        ← mod.rs:314
        "MediaTrackNext" => 0xB0,     // VK_MEDIA_NEXT_TRACK ← mod.rs:320
        "MediaTrackPrevious" => 0xB1, // VK_MEDIA_PREV_TRACK ← mod.rs:321
        "MediaStop" => 0xB2,          // VK_MEDIA_STOP       ← mod.rs:319
        "MediaPlayPause" => 0xB3,     // VK_MEDIA_PLAY_PAUSE ← mod.rs:318
        "MediaPlay" => 0xFA,          // VK_PLAY             ← mod.rs:316
        // ↓ 上游怪癖之三：`MediaPause` 和 `Pause` **都映射到 VK_PAUSE(0x13)**（mod.rs:317 与 322）。
        //   0x13 是键盘右上角的 Pause/Break 键，不是媒体暂停键。两个 Code 撞同一个 VK 意味着
        //   这两项热键在 Windows 上等价（后注册的那个会因 already-registered 而失败）。
        //   同样照抄：用户选 MediaPause 时真正会触发的物理键就是 Pause/Break。
        "MediaPause" => 0x13, // VK_PAUSE ← mod.rs:317（上游原样）
        "Pause" => 0x13,      // VK_PAUSE ← mod.rs:322

        // 其余一律 None：与上游 `_ => return None`（mod.rs:323）对齐。
        _ => return None,
    })
}

/// 处理四组"同族连号"的 Code：`KeyA-Z` / `Digit0-9` / `F1-F24` / `Numpad0-9`。
///
/// 必须先于显式表匹配，但两边不会撞车：本函数只接受"前缀 + 恰好一位数字/字母"，
/// 所以 `NumpadAdd`、`NumLock` 这类不会被它误吞（`Add` 不是一位数字，`NumLock` 前缀就不对）。
fn contiguous_family(code: &str) -> Option<i32> {
    // VK_A..VK_Z 与 ASCII 'A'..'Z' 同值（0x41..0x5A），VK_0..VK_9 与 ASCII '0'..'9' 同值
    // （0x30..0x39）——这是 Win32 文档明确写死的，不是巧合。
    if let Some(s) = code.strip_prefix("Key") {
        let b = single_byte(s)?;
        return b.is_ascii_uppercase().then_some(b as i32);
    }
    if let Some(s) = code.strip_prefix("Digit") {
        let b = single_byte(s)?;
        return b.is_ascii_digit().then_some(b as i32);
    }
    if let Some(s) = code.strip_prefix("Numpad") {
        // 只吃 Numpad0-9；NumpadAdd/Decimal/... 交给显式表。
        let b = single_byte(s)?;
        return b.is_ascii_digit().then_some(0x60 + (b - b'0') as i32); // VK_NUMPAD0 = 0x60
    }
    if let Some(s) = code.strip_prefix('F') {
        let n: u32 = s.parse().ok()?;
        // 必须回写比对：`"F01"`、`"F+1"` 都能被 parse 成 1，直接用会错配到 F1。
        // 而 Code 的规范写法只有 "F1"，非规范串一律当不认识（宁可 None 也别猜）。
        if n.to_string() != s || !(1..=24).contains(&n) {
            return None;
        }
        return Some(0x6F + n as i32); // VK_F1 = 0x70，故 Fn = 0x6F + n
    }
    None
}

/// 取出恰好一个 ASCII 字节；长度不为 1（含空串、多字符、非 ASCII）时返回 None。
fn single_byte(s: &str) -> Option<u8> {
    let b = s.as_bytes();
    (b.len() == 1).then(|| b[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐条对照单的一行：(Code, 期望 VK, "上游 VK_* 名 @上游 mod.rs 行号")。
    /// 第三列不是装饰：核对上游时按它回查，断言失败时也直接告诉你该看哪一行。
    type Row = (&'static str, i32, &'static str);

    fn check(table: &[Row]) {
        for &(code, vk, origin) in table {
            assert_eq!(
                code_to_vk(code),
                Some(vk),
                "{code} 应映射到 {vk:#04x}（{origin}）"
            );
        }
    }

    // —— 对照单：OEM 标点。抄错风险最高的一组，故单列一张表 ——
    const OEM: &[Row] = &[
        ("Semicolon", 0xBA, "VK_OEM_1 @246"),
        ("Equal", 0xBB, "VK_OEM_PLUS @242"),
        ("Comma", 0xBC, "VK_OEM_COMMA @243"),
        ("Minus", 0xBD, "VK_OEM_MINUS @244"),
        ("Period", 0xBE, "VK_OEM_PERIOD @245"),
        ("Slash", 0xBF, "VK_OEM_2 @247"),
        ("Backquote", 0xC0, "VK_OEM_3 @248"),
        ("BracketLeft", 0xDB, "VK_OEM_4 @249"),
        ("Backslash", 0xDC, "VK_OEM_5 @250"),
        ("BracketRight", 0xDD, "VK_OEM_6 @251"),
        ("Quote", 0xDE, "VK_OEM_7 @252"),
    ];

    const EDIT_NAV: &[Row] = &[
        ("Backspace", 0x08, "VK_BACK @253"),
        ("Tab", 0x09, "VK_TAB @254"),
        ("Enter", 0x0D, "VK_RETURN @256"),
        ("CapsLock", 0x14, "VK_CAPITAL @257"),
        ("Escape", 0x1B, "VK_ESCAPE @258"),
        ("Space", 0x20, "VK_SPACE @255"),
        ("PageUp", 0x21, "VK_PRIOR @259"),
        ("PageDown", 0x22, "VK_NEXT @260"),
        ("End", 0x23, "VK_END @261"),
        ("Home", 0x24, "VK_HOME @262"),
        ("ArrowLeft", 0x25, "VK_LEFT @263"),
        ("ArrowUp", 0x26, "VK_UP @264"),
        ("ArrowRight", 0x27, "VK_RIGHT @265"),
        ("ArrowDown", 0x28, "VK_DOWN @266"),
        ("PrintScreen", 0x2C, "VK_SNAPSHOT @267"),
        ("Insert", 0x2D, "VK_INSERT @268"),
        ("Delete", 0x2E, "VK_DELETE @269"),
    ];

    const NUMPAD_MISC: &[Row] = &[
        ("NumpadEnter", 0x0D, "VK_RETURN @308"),
        ("NumpadEqual", 0x45, "VK_E @309 上游怪癖"),
        ("NumpadMultiply", 0x6A, "VK_MULTIPLY @310"),
        ("NumpadAdd", 0x6B, "VK_ADD @305"),
        ("NumpadSubtract", 0x6D, "VK_SUBTRACT @311"),
        ("NumpadDecimal", 0x6E, "VK_DECIMAL @306"),
        ("NumpadDivide", 0x6F, "VK_DIVIDE @307"),
    ];

    const LOCK_MEDIA: &[Row] = &[
        ("Pause", 0x13, "VK_PAUSE @322"),
        ("MediaPause", 0x13, "VK_PAUSE @317 上游怪癖"),
        ("NumLock", 0x90, "VK_NUMLOCK @294"),
        ("ScrollLock", 0x91, "VK_SCROLL @312"),
        ("AudioVolumeMute", 0xAD, "VK_VOLUME_MUTE @315"),
        ("AudioVolumeDown", 0xAE, "VK_VOLUME_DOWN @313"),
        ("AudioVolumeUp", 0xAF, "VK_VOLUME_UP @314"),
        ("MediaTrackNext", 0xB0, "VK_MEDIA_NEXT_TRACK @320"),
        ("MediaTrackPrevious", 0xB1, "VK_MEDIA_PREV_TRACK @321"),
        ("MediaStop", 0xB2, "VK_MEDIA_STOP @319"),
        ("MediaPlayPause", 0xB3, "VK_MEDIA_PLAY_PAUSE @318"),
        ("MediaPlay", 0xFA, "VK_PLAY @316"),
    ];

    #[test]
    fn oem_punctuation() {
        check(OEM);
    }

    #[test]
    fn edit_and_navigation_keys() {
        check(EDIT_NAV);
    }

    #[test]
    fn numpad_non_digit_keys() {
        check(NUMPAD_MISC);
    }

    #[test]
    fn lock_and_media_keys() {
        check(LOCK_MEDIA);
    }

    /// 字母/数字：上游 mod.rs:206-241 一行一条，这里逐条断言值就是对应 ASCII 码。
    #[test]
    fn letters_and_digits() {
        for (i, c) in ('A'..='Z').enumerate() {
            let code = format!("Key{c}");
            assert_eq!(code_to_vk(&code), Some(0x41 + i as i32), "{code}"); // VK_A = 0x41
        }
        for (i, c) in ('0'..='9').enumerate() {
            let code = format!("Digit{c}");
            assert_eq!(code_to_vk(&code), Some(0x30 + i as i32), "{code}"); // VK_0 = 0x30
        }
        // 端点与热键默认值单独钉一遍，防止"连号规则整体偏移一位"这种全表一起错的情况。
        assert_eq!(code_to_vk("KeyA"), Some(0x41)); // VK_A
        assert_eq!(code_to_vk("KeyQ"), Some(0x51)); // VK_Q，默认截图热键 alt+KeyQ
        assert_eq!(code_to_vk("KeyW"), Some(0x57)); // VK_W，默认划词热键 alt+KeyW
        assert_eq!(code_to_vk("KeyZ"), Some(0x5A)); // VK_Z
        assert_eq!(code_to_vk("Digit0"), Some(0x30)); // VK_0
        assert_eq!(code_to_vk("Digit9"), Some(0x39)); // VK_9
    }

    /// 功能键：上游 mod.rs:270-293。F1-F24 在 Win32 里连号，但 F13 之后常被误以为不连号，故全测。
    #[test]
    fn function_keys() {
        for n in 1..=24u32 {
            let code = format!("F{n}");
            assert_eq!(code_to_vk(&code), Some(0x6F + n as i32), "{code}");
        }
        assert_eq!(code_to_vk("F1"), Some(0x70)); // VK_F1
        assert_eq!(code_to_vk("F12"), Some(0x7B)); // VK_F12
        assert_eq!(code_to_vk("F13"), Some(0x7C)); // VK_F13，连号不断
        assert_eq!(code_to_vk("F24"), Some(0x87)); // VK_F24
    }

    /// 小键盘数字：上游 mod.rs:295-304。
    #[test]
    fn numpad_digits() {
        for n in 0..=9i32 {
            let code = format!("Numpad{n}");
            assert_eq!(code_to_vk(&code), Some(0x60 + n), "{code}"); // VK_NUMPAD0 = 0x60
        }
    }

    /// 上游那三处"看着像 bug"的映射，是**故意保留**的，任何人想"顺手修一下"都会先撞到这个测试。
    /// 详细理由见 `code_to_vk` 里对应行的注释；一句话：本表要复现的是上游拿去注册的那个 VK。
    #[test]
    fn upstream_quirks_are_reproduced_on_purpose() {
        // ① 小键盘等号被上游映射成字母 E（而非 VK_OEM_NEC_EQUAL 0x92）
        assert_eq!(code_to_vk("NumpadEqual"), code_to_vk("KeyE"));
        assert_ne!(code_to_vk("NumpadEqual"), Some(0x92));
        // ② 小键盘回车与主回车共用 VK_RETURN（Win32 本来如此）
        assert_eq!(code_to_vk("NumpadEnter"), code_to_vk("Enter"));
        // ③ MediaPause 撞上 Pause/Break 的 VK_PAUSE
        assert_eq!(code_to_vk("MediaPause"), code_to_vk("Pause"));
    }

    /// 未收录一律 None：与上游 `_ => return None` 对齐。
    #[test]
    fn unknown_codes_return_none() {
        // 修饰键不在表内（它们走 Modifiers，且等待逻辑另有 VK_MENU/VK_SHIFT/VK_LWIN 一路）
        for c in [
            "ShiftLeft",
            "ShiftRight",
            "ControlLeft",
            "ControlRight",
            "AltLeft",
            "AltRight",
            "MetaLeft",
            "MetaRight",
        ] {
            assert_eq!(code_to_vk(c), None, "修饰键 {c} 不该出现在主键表里");
        }
        // 上游确实没有的键
        for c in [
            "Fn",
            "ContextMenu",
            "IntlBackslash",
            "Lang1",
            "Unidentified",
        ] {
            assert_eq!(code_to_vk(c), None, "{c} 上游未收录");
        }
        // 畸形/非规范串：宁可 None 也不猜（尤其 "F01" 会被 parse 成 1 而错配到 F1）
        for c in [
            "", "Key", "KeyAA", "Keya", "key", "Digit", "Digit10", "Digitx", "F", "F0", "F01",
            "F25", "F1 ", "+F1", "Numpad", "Numpad10", "numpad0", "ESCAPE", "escape",
        ] {
            assert_eq!(code_to_vk(c), None, "畸形串 {c:?} 必须返回 None");
        }
    }

    /// 覆盖条目总数必须与上游一致 —— 抄漏一条是最难自查的错误（漏掉的键单测里根本不会出现）。
    /// 117 = 上游 mod.rs:206-322 的分支条数：字母 26 + 数字 10 + OEM 11 + 编辑导航 17
    ///     + F1-F24 24 + 小键盘数字 10 + 小键盘其它 7 + 锁定与媒体 12。
    /// crate 升级后若上游增删了条目，先在这里把 117 改掉，再逐条补表。
    #[test]
    fn coverage_count_matches_upstream() {
        let families = 26 + 10 + 24 + 10; // KeyA-Z / Digit0-9 / F1-F24 / Numpad0-9
        let listed = OEM.len() + EDIT_NAV.len() + NUMPAD_MISC.len() + LOCK_MEDIA.len();
        assert_eq!(families + listed, 117, "条目数与上游 key_to_vk 对不上");
    }
}
