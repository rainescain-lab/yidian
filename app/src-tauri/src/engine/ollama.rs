use super::prompt;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct ChatResp {
    message: ChatMsg,
}
#[derive(Deserialize)]
struct ChatMsg {
    content: String,
}

/// 本地 Ollama 的对话端点。
const OLLAMA_CHAT_URL: &str = "http://127.0.0.1:11434/api/chat";

/// 整体超时。**没有超时是不行的**：reqwest 默认不设任何超时，只要 Ollama 接受了连接却不回数据
/// （进程卡住／模型冷加载 4GB+／正被别的大请求占住——Ollama 默认串行处理），请求就永远不返回；
/// 上层于是既不打"翻译完成"也不打"翻译失败"，界面表现为划了词毫无反应、日志只剩取词那半截。
/// 30s 足够 7B 冷加载后出一句译文，又不至于把交互无限期挂死。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// 本地服务连不上就该立刻失败，没有等待的价值（Ollama 没开时应当秒回错误而不是耗满超时）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// 从 Ollama /api/chat 的 JSON 文本里取出译文内容。
pub fn parse_chat_content(json: &str) -> Result<String, String> {
    let r: ChatResp = serde_json::from_str(json).map_err(|e| format!("解析响应失败: {e}"))?;
    Ok(r.message.content)
}

/// 调本地 Ollama /api/chat 把一段文本从 `src` 译成 `tgt`，返回清洗后的译文。
///
/// ⚠ **方向必须由调用方给，本函数不再自己判**（2026-08-07 修）。原先这里自己又调了一次
/// `lang::default_direction(text)`，于是「用户手选的方向」只对在线路径生效、本地路径我行我素。
/// 这个半截 bug 只在**切到本地引擎**或**在线失败触发本地兜底**时才现形，极难发现。
pub async fn translate_local(text: &str, src: &str, tgt: &str) -> Result<String, String> {
    translate_local_with(text, src, tgt, OLLAMA_CHAT_URL, DEFAULT_TIMEOUT).await
}

/// 端点与超时可注入的版本，便于测试「服务不回数据时会超时返回而不是永久挂起」。
/// localhost 必须直连、绝不走系统/Clash 代理（`.no_proxy()`）。
pub async fn translate_local_with(
    text: &str,
    src: &str,
    tgt: &str,
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
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
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))?;
    let secs = timeout.as_secs();
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("本地引擎 {secs}s 无响应（Ollama 卡住，或模型仍在加载／正被其它任务占用）")
            } else {
                format!("连接本地引擎失败（Ollama 是否在运行？）: {e}")
            }
        })?;
    let txt = resp.text().await.map_err(|e| {
        if e.is_timeout() {
            format!("本地引擎 {secs}s 未读完响应（Ollama 生成中断或过慢）")
        } else {
            format!("读取响应失败: {e}")
        }
    })?;
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

    /// 回归测试：服务端接受连接却永不回数据时，必须在超时后报错返回，而不是一直挂着。
    /// 这正是线上「取到词了却既没有翻译完成、也没有翻译失败」的成因。
    #[test]
    fn hanging_server_times_out_instead_of_blocking_forever() {
        use std::io::Read;
        use std::net::TcpListener;

        // 假 Ollama：收下连接、读掉请求，然后什么都不回
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                std::thread::sleep(Duration::from_secs(20)); // 保持连接、永不响应
            }
        });

        let url = format!("http://{addr}/api/chat");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let t0 = std::time::Instant::now();
        let r = rt.block_on(translate_local_with("hello", "English", "Chinese", &url, Duration::from_millis(800)));
        let elapsed = t0.elapsed();

        let err = r.expect_err("服务不回数据时应当超时报错，而不是返回成功");
        assert!(
            elapsed < Duration::from_secs(5),
            "应在超时后迅速返回，实际耗时 {elapsed:?}"
        );
        assert!(
            err.contains("无响应") || err.contains("未读完响应"),
            "错误信息应说明是超时，实际: {err}"
        );
    }

    /// Ollama 没运行时应当很快失败（连接被拒），不该耗满整体超时。
    #[test]
    fn refused_connection_fails_fast() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // 绑定后立刻 drop，得到一个几乎必然无人监听的端口
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let url = format!("http://127.0.0.1:{port}/api/chat");
        let t0 = std::time::Instant::now();
        let r = rt.block_on(translate_local_with("hello", "English", "Chinese", &url, Duration::from_secs(30)));
        assert!(r.is_err(), "无人监听时应当报错");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "连接被拒应当秒失败，实际 {:?}",
            t0.elapsed()
        );
    }

    // 真跑冒烟：需本地 Ollama 在运行。用 `cargo test -- --ignored --nocapture` 触发。
    #[test]
    #[ignore]
    fn translate_local_real_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(translate_local("hello", "English", "Chinese")).expect("翻译应成功");
        println!("REAL OUTPUT for 'hello' => {out}");
        assert!(!out.is_empty(), "译文不应为空");
        assert!(
            crate::engine::lang::has_cjk(&out),
            "英→中，译文应含中文: {out}"
        );
    }
}
