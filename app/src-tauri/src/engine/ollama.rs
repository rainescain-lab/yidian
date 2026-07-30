use super::{lang, prompt};
use serde::Deserialize;

#[derive(Deserialize)]
struct ChatResp {
    message: ChatMsg,
}
#[derive(Deserialize)]
struct ChatMsg {
    content: String,
}

/// 从 Ollama /api/chat 的 JSON 文本里取出译文内容。
pub fn parse_chat_content(json: &str) -> Result<String, String> {
    let r: ChatResp = serde_json::from_str(json).map_err(|e| format!("解析响应失败: {e}"))?;
    Ok(r.message.content)
}

/// 调本地 Ollama /api/chat 翻译一段文本，返回清洗后的译文。
/// localhost 必须直连、绝不走系统/Clash 代理（.no_proxy()）。
pub async fn translate_local(text: &str) -> Result<String, String> {
    let (src, tgt) = lang::default_direction(text);
    let body = serde_json::json!({
        "model": "qwen2.5:7b-instruct",
        "stream": false,
        "options": { "temperature": 0.2 },
        "messages": [
            { "role": "system", "content": prompt::system_prompt(src, tgt) },
            { "role": "user",   "content": text }
        ]
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))?;
    let resp = client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("连接本地引擎失败（Ollama 是否在运行？）: {e}"))?;
    let txt = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    let content = parse_chat_content(&txt)?;
    Ok(prompt::clean_translation(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_content() {
        let j = r#"{"model":"qwen2.5:7b-instruct","message":{"role":"assistant","content":"你好"},"done":true}"#;
        assert_eq!(parse_chat_content(j).unwrap(), "你好");
    }

    #[test]
    fn bad_json_is_err() {
        assert!(parse_chat_content("not json").is_err());
    }

    // 真跑冒烟：需本地 Ollama 在运行。用 `cargo test -- --ignored --nocapture` 触发。
    #[test]
    #[ignore]
    fn translate_local_real_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(translate_local("hello")).expect("翻译应成功");
        println!("REAL OUTPUT for 'hello' => {out}");
        assert!(!out.is_empty(), "译文不应为空");
        assert!(
            crate::engine::lang::has_cjk(&out),
            "英→中，译文应含中文: {out}"
        );
    }
}
