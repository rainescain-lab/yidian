/// 含 CJK（中日）字符即视为“中文侧”。M1 只区分中↔英方向。
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        (0x4E00..=0x9FFF).contains(&u)      // CJK 统一表意
            || (0x3400..=0x4DBF).contains(&u) // 扩展 A
            || (0x3040..=0x30FF).contains(&u) // 日文假名
            || (0xF900..=0xFAFF).contains(&u) // 兼容表意
    })
}

/// 返回 (源语言, 目标语言) 名称，喂给 prompt。
pub fn default_direction(text: &str) -> (&'static str, &'static str) {
    if has_cjk(text) {
        ("Chinese", "English")
    } else {
        ("English", "Chinese")
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
        assert_eq!(default_direction("hello 世界"), ("Chinese", "English"));
    }
}
