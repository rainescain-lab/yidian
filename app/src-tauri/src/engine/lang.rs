//! 语言判定与翻译方向。
//!
//! # 病史（2026-08-07 重写，别再按老直觉改）
//!
//! 旧实现是 `if has_cjk(text) { 中→英 } else { 英→中 }`，把**整个汉字圈**当成中文，
//! 于是日语、韩语（若谚文被并进来）统统被判成"中文侧"→ 翻成英文。用户实测：日语被译成英文。
//!
//! ⚠ **两条踩过的弯路，别重走**：
//! 1. **「把假名段 0x3040-0x30FF 从 has_cjk 里删掉」不管用。** 日语正文（`東京`、
//!    `日本語を勉強します`）命中的是**汉字段**，删了假名段照样判中文侧。病根是那个
//!    **二元分支**，不是某个区段。
//! 2. **「补齐 has_cjk 漏掉的区段」会让 bug 更严重。** 半角片假名 `ｱｲｳ`、`㈱` 这些
//!    当前恰好走对（不在 has_cjk 里 → 判非中文 → 翻成中文），一旦"补全"进 CJK 立刻
//!    全部改判中文侧、被送去翻英文。
//! 3. **韩语现在是"碰巧对"**：`has_cjk` 恰好不含谚文。谁按"补全 CJK"的直觉把 Hangul
//!    加进去，韩语立刻变成翻英文。所以这里改成**显式判谚文 → 韩语**，把碰巧对变成真的对。
//!
//! # 判定方式：Unicode 脚本 + 排他优先级（不是计数，也不是 any() 短路）
//!
//! 为什么不引 whatlang / whichlang / lingua：本模块只需要一个二元位「是不是中文侧」，
//! 而这三个库对 CJK **根本不做统计判定**（whatlang 里 `Script::Mandarin => &[Lang::Cmn]`
//! 是纯脚本短路），`日本語`/`東京` 三库同样全判中文 —— 与自写规则**同一盲区**，加库买不到。
//! 且划词文本极短（几个字），正落在统计法最不可靠的一档。
//!
//! 为什么必须**排他优先级**而不是按字符计数：`hello 世界` 若按票数，5 个拉丁票会压过
//! 2 个汉字票（统计库正是这么判错的）。这里按"命中即定级"，混排稳定归中文侧。

/// 按 Unicode 脚本能判出来的语言。**不做拉丁语系细分**（脚本层做不到）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptLang {
    Japanese,
    Korean,
    Chinese,
    Cyrillic,
    Arabic,
    Greek,
    Thai,
    Hebrew,
    Devanagari,
    /// 拉丁字母。脚本层无法区分英/法/德/西…，方向判定上一律按"非中文侧"处理。
    Latin,
    Unknown,
}

impl ScriptLang {
    /// 喂给翻译 prompt / 历史记录的语言名。
    ///
    /// ⚠ `Latin` 一律报 `English`：脚本规则区分不了英/法/德/西，而英文占绝大多数，
    /// 报 English 是**尽力而为的默认**（这一步不会影响结果——在线引擎的源语言走
    /// `sl=auto` 自己识别，这个名字只进 prompt 与历史标签）。用户手选方向的能力落地后，
    /// 由用户显式覆盖。
    pub fn name(self) -> &'static str {
        match self {
            ScriptLang::Japanese => "Japanese",
            ScriptLang::Korean => "Korean",
            ScriptLang::Chinese => "Chinese",
            ScriptLang::Cyrillic => "Russian",
            ScriptLang::Arabic => "Arabic",
            ScriptLang::Greek => "Greek",
            ScriptLang::Thai => "Thai",
            ScriptLang::Hebrew => "Hebrew",
            ScriptLang::Devanagari => "Hindi",
            ScriptLang::Latin | ScriptLang::Unknown => "English",
        }
    }
}

fn is_kana(u: u32) -> bool {
    (0x3040..=0x309F).contains(&u)      // 平假名
        || (0x30A0..=0x30FF).contains(&u) // 片假名
        || (0x31F0..=0x31FF).contains(&u) // 片假名语音扩展
        || (0xFF66..=0xFF9D).contains(&u) // 半角片假名
}

fn is_hangul(u: u32) -> bool {
    (0xAC00..=0xD7AF).contains(&u)      // 谚文音节
        || (0x1100..=0x11FF).contains(&u) // 谚文字母
        || (0x3130..=0x318F).contains(&u) // 谚文兼容字母
}

fn is_han(u: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&u)        // CJK 统一表意
        || (0x3400..=0x4DBF).contains(&u)   // 扩展 A
        || (0xF900..=0xFAFF).contains(&u)   // 兼容表意
        || (0x20000..=0x2A6DF).contains(&u) // 扩展 B
        || (0x2F00..=0x2FDF).contains(&u)   // 康熙部首
        || (0x3105..=0x312F).contains(&u)   // 注音符号
}

/// 判定文本的脚本语言。排他优先级：假名 > 谚文 > 汉字 > 其他非拉丁脚本(取最多) > 拉丁。
pub fn detect_script_lang(s: &str) -> ScriptLang {
    let (mut kana, mut hangul, mut han, mut latin) = (false, false, false, false);
    let (mut cyr, mut arab, mut grk, mut thai, mut hebr, mut deva) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);

    for c in s.chars() {
        let u = c as u32;
        if is_kana(u) {
            kana = true;
            break; // 假名优先级最高，见到就能定案
        }
        if is_hangul(u) {
            hangul = true;
        } else if is_han(u) {
            han = true;
        } else if (0x0400..=0x04FF).contains(&u) {
            cyr += 1;
        } else if (0x0600..=0x06FF).contains(&u) || (0x0750..=0x077F).contains(&u) {
            arab += 1;
        } else if (0x0370..=0x03FF).contains(&u) || (0x1F00..=0x1FFF).contains(&u) {
            grk += 1;
        } else if (0x0E00..=0x0E7F).contains(&u) {
            thai += 1;
        } else if (0x0590..=0x05FF).contains(&u) {
            hebr += 1;
        } else if (0x0900..=0x097F).contains(&u) {
            deva += 1;
        } else if c.is_ascii_alphabetic() || (0x00C0..=0x024F).contains(&u) {
            latin = true;
        }
    }

    if kana {
        return ScriptLang::Japanese;
    }
    if hangul {
        return ScriptLang::Korean;
    }
    if han {
        return ScriptLang::Chinese;
    }
    let best = [
        (cyr, ScriptLang::Cyrillic),
        (arab, ScriptLang::Arabic),
        (grk, ScriptLang::Greek),
        (thai, ScriptLang::Thai),
        (hebr, ScriptLang::Hebrew),
        (deva, ScriptLang::Devanagari),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .max_by_key(|(n, _)| *n);
    if let Some((_, l)) = best {
        return l;
    }
    if latin {
        return ScriptLang::Latin;
    }
    ScriptLang::Unknown
}

/// **长文本**（截图 OCR 的整块结果）用的脚本判定：几个杂字不许翻转整体判定。
///
/// # 为什么要有两套
///
/// [`detect_script_lang`] 是「命中即定级」—— 见到一个汉字就归中文侧。**那条规则对划词是
/// 对的**：选中的就几个字，按票数会让拉丁票压过汉字票（`hello 世界` 会被判成英文）。
///
/// 但截图完全是另一回事。2026-08-16 用户实测：截了一张 Excel 的数字格式菜单，14 行**全是
/// 英文**，可 OCR 把日历图标认成了 `茴`/`芭`、货币图标认成 `￥` —— **3 个杂字**就让整批
/// 被判成"中文侧"，方向变成中→英，于是英文译英文、**一个字都没翻**，而界面上还显示翻译成功。
///
/// 所以这里改成：CJK 只占极小比例、且文本够长时，把它们当噪声剔掉再判。
/// 两条门槛缺一不可 —— 只看比例会让「三个字的中文标题」被当噪声。
pub fn detect_script_lang_bulk(s: &str) -> ScriptLang {
    /// 低于这个占比才可能是噪声。
    const NOISE_RATIO: f64 = 0.10;
    /// 文本至少要这么多个字母才谈得上"占比"。短文本一律走严格规则。
    const MIN_LETTERS: usize = 40;

    let mut cjk = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        let u = c as u32;
        if is_han(u) || is_kana(u) || is_hangul(u) {
            cjk += 1;
        } else if c.is_alphabetic() {
            other += 1;
        }
    }
    let total = cjk + other;
    if cjk == 0 || total < MIN_LETTERS || (cjk as f64) >= NOISE_RATIO * total as f64 {
        return detect_script_lang(s);
    }
    // 剔掉零星 CJK 再判。剩下的交给同一套规则，不另立门户。
    let cleaned: String = s
        .chars()
        .filter(|c| {
            let u = *c as u32;
            !(is_han(u) || is_kana(u) || is_hangul(u))
        })
        .collect();
    detect_script_lang(&cleaned)
}

/// 含汉字/假名（旧口径）。
///
/// ⚠ **只留给测试断言用**（判"译文里有没有中文"）。**禁止拿它做方向判定** —— 它把
/// 日语也算成"中文侧"，正是本模块病史里那个 bug 的来源。方向请用 [`direction_with_native`]。
#[allow(dead_code)] // 生产路径不用它，只服务真跑冒烟测试的断言
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        is_han(u) || is_kana(u)
    })
}

/// 返回 (源语言, 目标语言) 名称，喂给 prompt / 历史。
///
/// 规则（用户 2026-08-07 定）：**中文 → 英文；其他任何语言 → 中文**。
///
/// ⚠ 生产路径已改走 [`direction_with_native`]（母语可配）。本函数留下来当**规则基准**：
/// 单测拿它和可配版逐条对拍，防止"母语可配"这层把默认行为悄悄改掉。所以它没有调用方是对的。
#[allow(dead_code)]
pub fn default_direction(text: &str) -> (&'static str, &'static str) {
    let src = detect_script_lang(text);
    if src == ScriptLang::Chinese {
        ("Chinese", "English")
    } else {
        (src.name(), "Chinese")
    }
}

/// 同一条规则，但**母语可配**：母语 → `native_to`；其他任何语言 → 母语。
///
/// 用两个参数表达用户那条「中文→英文，其他→中文」，顺带让学日语的人把 `native` 留成中文、
/// `native_to` 改成 Japanese，就变成「中文→日文，其他→中文」。
///
/// 返回 `String` 而非 `&'static str`：母语来自设置，不是编译期常量。
/// 与 [`default_direction`] 的一致性由单测钉死（`native="Chinese", native_to="English"` 时两者必须同解）。
pub fn direction_with_native(text: &str, native: &str, native_to: &str) -> (String, String) {
    direction_from_script(detect_script_lang(text), native, native_to)
}

/// 同一条规则，但脚本判定走**抗噪**口径 —— 截图整块文本专用，见 [`detect_script_lang_bulk`]。
pub fn direction_with_native_bulk(text: &str, native: &str, native_to: &str) -> (String, String) {
    direction_from_script(detect_script_lang_bulk(text), native, native_to)
}

fn direction_from_script(script: ScriptLang, native: &str, native_to: &str) -> (String, String) {
    let src = script.name();
    if src == native {
        (native.to_string(), native_to.to_string())
    } else {
        (src.to_string(), native.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_goes_to_chinese() {
        assert_eq!(default_direction("hello world"), ("English", "Chinese"));
    }

    #[test]
    fn chinese_goes_to_english() {
        assert_eq!(default_direction("你好，世界"), ("Chinese", "English"));
    }

    #[test]
    fn mixed_with_cjk_is_chinese_side() {
        // 中外混排按"命中即定级"归中文侧（不按字符票数，否则拉丁票会压过汉字票）。
        assert_eq!(default_direction("hello 世界"), ("Chinese", "English"));
    }

    // ---- 用户报障的那一类：日语必须翻成中文，不能翻成英文 ----

    #[test]
    fn japanese_hiragana_goes_to_chinese() {
        assert_eq!(default_direction("こんにちは"), ("Japanese", "Chinese"));
    }

    #[test]
    fn japanese_katakana_goes_to_chinese() {
        assert_eq!(default_direction("コーヒー"), ("Japanese", "Chinese"));
    }

    #[test]
    fn japanese_halfwidth_katakana_goes_to_chinese() {
        assert_eq!(default_direction("ｺﾝﾆﾁﾜ"), ("Japanese", "Chinese"));
    }

    /// 汉字+假名混排 —— 真实日语正文的常态。旧实现在这里必然判错（命中汉字段）。
    #[test]
    fn japanese_kanji_plus_kana_goes_to_chinese() {
        assert_eq!(
            default_direction("日本語を勉強します"),
            ("Japanese", "Chinese")
        );
    }

    #[test]
    fn korean_goes_to_chinese() {
        // 旧实现是"碰巧对"（has_cjk 恰好不含谚文）；这里改成显式判定，防止后人"补全 CJK"时踩雷。
        assert_eq!(default_direction("안녕하세요"), ("Korean", "Chinese"));
    }

    #[test]
    fn russian_goes_to_chinese() {
        assert_eq!(default_direction("Привет, мир"), ("Russian", "Chinese"));
    }

    #[test]
    fn thai_and_arabic_go_to_chinese() {
        assert_eq!(default_direction("สวัสดี").1, "Chinese");
        assert_eq!(default_direction("مرحبا").1, "Chinese");
    }

    /// ⚠ **已知盲区，不是 bug**：只含汉字、没有假名的日语（`東京`、`株式会社`、`無料`）
    /// 在 Unicode 脚本层与中文**原理上不可区分** —— 它们就是同一批码位，whatlang /
    /// whichlang / lingua 三个库实测同样全判中文。只能靠用户手选方向覆盖。
    /// 本测试把这个行为**显式钉死**，防止后人误以为"修好了"或误以为"这是回归"。
    #[test]
    fn known_blind_spot_kanji_only_japanese_is_treated_as_chinese() {
        assert_eq!(default_direction("東京"), ("Chinese", "English"));
        assert_eq!(default_direction("株式会社"), ("Chinese", "English"));
    }

    #[test]
    fn detect_covers_scripts() {
        assert_eq!(detect_script_lang("ひらがな"), ScriptLang::Japanese);
        assert_eq!(detect_script_lang("한글"), ScriptLang::Korean);
        assert_eq!(detect_script_lang("汉字"), ScriptLang::Chinese);
        assert_eq!(detect_script_lang("abc"), ScriptLang::Latin);
        assert_eq!(detect_script_lang("123 !@#"), ScriptLang::Unknown);
    }

    // ---- 母语可配之后：规则不变，只是"中文/英文"变成参数 ----

    /// 默认参数下，可配版必须与写死版**逐字同解**。两者一旦漂移，主界面与划词就会
    /// 在同一段文字上给出不同方向，而且只在某些语言上现形，极难发现。
    #[test]
    fn configurable_direction_agrees_with_default_on_default_settings() {
        for t in [
            "hello world",
            "你好，世界",
            "hello 世界",
            "こんにちは",
            "日本語を勉強します",
            "안녕하세요",
            "Привет",
            "東京",
            "123 !@#",
        ] {
            let (a, b) = default_direction(t);
            assert_eq!(
                direction_with_native(t, "Chinese", "English"),
                (a.to_string(), b.to_string()),
                "「{t}」两套实现不一致"
            );
        }
    }

    /// 学日语的人：母语仍是中文，但「中文→」改成日文。其他语言照旧回中文。
    #[test]
    fn native_to_can_be_switched_to_japanese() {
        assert_eq!(
            direction_with_native("你好", "Chinese", "Japanese"),
            ("Chinese".into(), "Japanese".into())
        );
        assert_eq!(
            direction_with_native("hello", "Chinese", "Japanese"),
            ("English".into(), "Chinese".into()),
            "非母语仍应译回母语，不受 native_to 影响"
        );
    }

    /// 母语本身也能换（母语＝日语的用户）。此时日语原文该被译成 native_to，中文该被译成日语。
    #[test]
    fn native_language_itself_can_be_switched() {
        assert_eq!(
            direction_with_native("こんにちは", "Japanese", "English"),
            ("Japanese".into(), "English".into())
        );
        assert_eq!(
            direction_with_native("你好", "Japanese", "English"),
            ("Chinese".into(), "Japanese".into()),
            "中文对日语母语者来说是外语，应译回日语"
        );
    }

    // ---- 抗噪口径（截图整块文本专用）----

    /// **回归钉（2026-08-16 用户实测）**：截了一张 Excel 数字格式菜单，14 行全是英文，
    /// 但 OCR 把日历/货币图标误认成了 `茴`/`芭`/`￥` 三个汉字。严格口径会把整批判成中文侧
    /// ⇒ 英译英 ⇒ **一个字都没翻**，界面上还显示翻译成功。
    #[test]
    fn bulk_ignores_a_few_ocr_garbage_cjk_chars() {
        let ocrd = "General 123Number Currency Accounting 芭 Short Date 茴LongDate Time \
                    % Percentage Fraction 10nScientific AText CommaStyle ￥ Special Custom";
        assert_eq!(
            detect_script_lang(ocrd),
            ScriptLang::Chinese,
            "严格口径确实会被三个杂字带翻——这正是那个 bug"
        );
        assert_eq!(
            detect_script_lang_bulk(ocrd),
            ScriptLang::Latin,
            "抗噪口径必须把它们当噪声剔掉"
        );
        assert_eq!(
            direction_with_native_bulk(ocrd, "Chinese", "English"),
            ("English".into(), "Chinese".into())
        );
    }

    /// 真中文长文本不许被当成噪声剔掉。
    #[test]
    fn bulk_keeps_real_chinese() {
        let s = "这是一段足够长的中文文本，用来验证抗噪口径不会把真正的中文内容当成噪声剔掉，\
                 里面还夹杂了一些 English words 和数字 12345 混排。";
        assert_eq!(detect_script_lang_bulk(s), ScriptLang::Chinese);
    }

    /// **短文本一律走严格口径**：三个字的中文标题不能因为"占比低"被当噪声。
    #[test]
    fn bulk_falls_back_to_strict_on_short_text() {
        assert_eq!(detect_script_lang_bulk("hello 世界"), ScriptLang::Chinese);
        assert_eq!(detect_script_lang_bulk("设置"), ScriptLang::Chinese);
    }

    /// 日语长文本照旧判日语（抗噪只剔"零星"CJK，不改语言优先级）。
    #[test]
    fn bulk_still_detects_japanese() {
        let s = "これは日本語の長い文章です。スクリーンショット翻訳のテストのために書きました。\
                 かなり長くしてあります。";
        assert_eq!(detect_script_lang_bulk(s), ScriptLang::Japanese);
    }

    #[test]
    fn has_cjk_still_detects_chinese_output() {
        // 这个函数只服务于测试断言（判译文里有没有中文），不参与方向判定。
        assert!(has_cjk("你好"));
        assert!(!has_cjk("hello"));
    }
}
