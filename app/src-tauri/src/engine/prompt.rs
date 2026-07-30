pub fn system_prompt(src: &str, tgt: &str) -> String {
    format!(
        "You are a professional translator. Translate the user's text from {src} to {tgt}. \
Output ONLY the translation itself — no quotes, no explanations, no notes, no pinyin. Preserve line breaks."
    )
}

/// 去掉模型偶尔加的成对引号与首尾空白。
pub fn clean_translation(raw: &str) -> String {
    let t = raw.trim();
    let t = match (t.starts_with('"'), t.ends_with('"'), t.len() >= 2) {
        (true, true, true) => &t[1..t.len() - 1],
        _ => t,
    };
    let t = match (t.starts_with('「'), t.ends_with('」')) {
        (true, true) => t.trim_start_matches('「').trim_end_matches('」'),
        _ => t,
    };
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_names_both_langs() {
        let p = system_prompt("English", "Chinese");
        assert!(p.contains("English") && p.contains("Chinese"));
        assert!(p.contains("ONLY"));
    }

    #[test]
    fn clean_strips_quotes_and_space() {
        assert_eq!(clean_translation("  \"你好\"  "), "你好");
        assert_eq!(clean_translation("「你好」"), "你好");
        assert_eq!(clean_translation("你好"), "你好");
        assert_eq!(clean_translation("say \"hi\" now"), "say \"hi\" now");
    }
}
