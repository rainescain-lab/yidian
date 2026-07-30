//! 在线翻译引擎（keyless）：微软 Bing 为主 → 谷歌 gtx 兜底。
//!
//! 规格与真实样本来自实抓（2026-07-27）：
//! - Google gtx：GET translate_a/single?client=gtx，返回嵌套数组，译文=拼接 root[0][i][0]。
//! - Bing ttranslatev3：先从 bing.com/translator 抓 IG/IID/key/token（key 是铸造毫秒时间戳、
//!   有效期读返回值≈1h），POST 表单翻译，译文=root[0].translations[0].text；失败态是对象非数组。
//! 机器走 Clash TUN 透明代理，reqwest 直连即经 TUN，无需设 proxy。

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(UA)
        .cookie_store(true) // Bing token 与 cookie jar 绑定
        .timeout(Duration::from_secs(9))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .expect("构建 reqwest client")
});

static BING_TOKEN: Lazy<Mutex<Option<BingToken>>> = Lazy::new(|| Mutex::new(None));

#[derive(Clone, Debug)]
struct BingToken {
    key: u128, // 铸造毫秒时间戳
    token: String,
    ig: String,
    iid: String,
    expiry_ms: u128,
}

impl BingToken {
    fn valid(&self) -> bool {
        now_ms() < self.key + self.expiry_ms.saturating_sub(60_000) // 60s 安全垫
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 解析（纯函数，用真实样本 TDD）
// ---------------------------------------------------------------------------

/// 谷歌 gtx：拼接 root[0] 里各段的 [0]。每段已自带尾空格，直接连接。
pub fn parse_google(json: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("谷歌响应解析失败: {e}"))?;
    let segs = v
        .get(0)
        .and_then(|x| x.as_array())
        .ok_or("谷歌响应结构异常")?;
    let out: String = segs
        .iter()
        .filter_map(|s| s.get(0).and_then(|t| t.as_str()))
        .collect();
    Ok(out)
}

/// 微软 ttranslatev3：取 [0].translations[0].text。顶层非数组 = 风控/token 过期/400。
pub fn parse_bing(json: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("微软响应解析失败: {e}"))?;
    if !v.is_array() {
        let head: String = json.chars().take(120).collect();
        return Err(format!("微软返回非预期(风控/token 失效): {head}"));
    }
    v.get(0)
        .and_then(|o| o.get("translations"))
        .and_then(|t| t.get(0))
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "微软响应缺少译文".to_string())
}

/// 从 bing.com/translator 页面抓 IG/IID/key/token/有效期。
fn parse_bing_token(html: &str) -> Option<BingToken> {
    static IG: Lazy<Regex> = Lazy::new(|| Regex::new(r#"IG:"([A-F0-9]+)""#).unwrap());
    static IID: Lazy<Regex> = Lazy::new(|| Regex::new(r#"data-iid="(translator\.\d+)""#).unwrap());
    static AP: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"params_AbusePreventionHelper\s*=\s*\[(\d+),"([^"]+)",(\d+)\]"#).unwrap()
    });

    let ig = IG.captures(html)?.get(1)?.as_str().to_string();
    let iid = IID.captures(html)?.get(1)?.as_str().to_string();
    let ap = AP.captures(html)?;
    let key: u128 = ap.get(1)?.as_str().parse().ok()?;
    let token = ap.get(2)?.as_str().to_string();
    let expiry_ms: u128 = ap.get(3)?.as_str().parse().ok()?;
    Some(BingToken {
        key,
        token,
        ig,
        iid,
        expiry_ms,
    })
}

/// 目标语言名 → (谷歌码, 微软码)。M1 覆盖中英，其余留待扩展。
fn target_codes(tgt: &str) -> (&'static str, &'static str) {
    match tgt {
        "Chinese" => ("zh-CN", "zh-Hans"),
        "Japanese" => ("ja", "ja"),
        "Korean" => ("ko", "ko"),
        "French" => ("fr", "fr"),
        "German" => ("de", "de"),
        "Spanish" => ("es", "es"),
        "Russian" => ("ru", "ru"),
        "Portuguese" => ("pt", "pt"),
        _ => ("en", "en"),
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

async fn fetch_token() -> Result<BingToken, String> {
    let html = CLIENT
        .get("https://www.bing.com/translator")
        .send()
        .await
        .map_err(|e| format!("打开微软翻译页失败: {e}"))?
        .text()
        .await
        .map_err(|e| format!("读取微软翻译页失败: {e}"))?;
    parse_bing_token(&html).ok_or_else(|| "未能从微软页面获取 token".to_string())
}

async fn get_token(force: bool) -> Result<BingToken, String> {
    let mut guard = BING_TOKEN.lock().await;
    if !force {
        if let Some(t) = guard.as_ref() {
            if t.valid() {
                return Ok(t.clone());
            }
        }
    }
    let t = fetch_token().await?;
    *guard = Some(t.clone());
    Ok(t)
}

async fn bing_call(token: &BingToken, text: &str, from_b: &str, to_b: &str) -> Result<String, String> {
    let url = format!(
        "https://www.bing.com/ttranslatev3?isVertical=1&IG={}&IID={}",
        token.ig, token.iid
    );
    let key_s = token.key.to_string();
    let resp = CLIENT
        .post(&url)
        .header("Referer", "https://www.bing.com/translator")
        .form(&[
            ("fromLang", from_b),
            ("to", to_b),
            ("text", text),
            ("token", token.token.as_str()),
            ("key", key_s.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("微软请求失败: {e}"))?
        .text()
        .await
        .map_err(|e| format!("读取微软响应失败: {e}"))?;
    parse_bing(&resp)
}

async fn bing_translate(text: &str, to_b: &str) -> Result<String, String> {
    let token = get_token(false).await?;
    match bing_call(&token, text, "auto-detect", to_b).await {
        Ok(s) => Ok(s),
        Err(_) => {
            // token 可能失效，强刷一次再试；再败则上抛触发兜底
            let token = get_token(true).await?;
            bing_call(&token, text, "auto-detect", to_b).await
        }
    }
}

async fn google_translate(text: &str, tl: &str) -> Result<String, String> {
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={}&dt=t&q={}",
        tl,
        urlencoding::encode(text)
    );
    let resp = CLIENT
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("谷歌请求失败: {e}"))?
        .text()
        .await
        .map_err(|e| format!("读取谷歌响应失败: {e}"))?;
    parse_google(&resp)
}

/// 按 order（如 ["bing","google"]）依次尝试，任一成功即返回 (译文, 引擎名)。
pub async fn translate_online(
    text: &str,
    tgt: &str,
    order: &[String],
) -> Result<(String, String), String> {
    let (g_tl, b_to) = target_codes(tgt);
    let mut last_err = String::from("无可用在线引擎");
    for eng in order {
        let r = match eng.as_str() {
            "bing" => bing_translate(text, b_to).await.map(|s| (s, "微软".to_string())),
            "google" => google_translate(text, g_tl).await.map(|s| (s, "谷歌".to_string())),
            _ => continue,
        };
        match r {
            Ok((t, label)) if !t.trim().is_empty() => return Ok((t, label)),
            Ok(_) => last_err = format!("{eng} 返回空译文"),
            Err(e) => last_err = e,
        }
    }
    Err(format!("在线翻译失败：{last_err}"))
}

/// 启动时后台预热：预抓 Bing token + 暖 TLS 连接 + 跑通一次翻译路径，
/// 把"首次取 token / 首连握手"的一次性开销挪到启动，用户第一次截图翻译即热。
pub async fn warmup(order: &[String]) {
    let _ = translate_online("hi", "Chinese", order).await;
}

// ---------------------------------------------------------------------------
// 测试（真实抓取样本作 fixture）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_single() {
        let j = r#"[[["你好世界","hello world",null,null,10]],null,"en",null,null,null,null,[]]"#;
        assert_eq!(parse_google(j).unwrap(), "你好世界");
    }

    #[test]
    fn google_multi_segment_concat() {
        let j = r#"[[["你好。","Hello. ",null,null,10],["你好吗？","How are you? ",null,null,10],["我很好谢谢。","I am fine thanks.",null,null,3]],null,"en"]"#;
        assert_eq!(parse_google(j).unwrap(), "你好。你好吗？我很好谢谢。");
    }

    #[test]
    fn google_empty_input() {
        let j = r#"[[["","",null,null,5]],null,"en",null,null,null,null,[]]"#;
        assert_eq!(parse_google(j).unwrap(), "");
    }

    #[test]
    fn bing_basic() {
        let j = r#"[{"translations":[{"text":"你好，世界","to":"zh-Hans","transliteration":{"text":"Nǐ hǎo, shìjiè","script":"Latn"}}],"usedLLM":true,"detectedLanguage":{"language":"en"}}]"#;
        assert_eq!(parse_bing(j).unwrap(), "你好，世界");
    }

    #[test]
    fn bing_ignores_extra_transliteration_element() {
        // zh→en 时数组多一个 inputTransliteration 元素，只认 [0]
        let j = r#"[{"translations":[{"text":"Hello, World","to":"en"}],"usedLLM":true,"detectedLanguage":{"language":"zh-Hant"}},{"inputTransliteration":"Nǐ hǎo shìjiè","script":"Latn"}]"#;
        assert_eq!(parse_bing(j).unwrap(), "Hello, World");
    }

    #[test]
    fn bing_non_array_is_error() {
        assert!(parse_bing(r#"{"statusCode":400}"#).is_err());
        assert!(parse_bing(r#"{"ShowCaptcha":true}"#).is_err());
    }

    #[test]
    fn bing_token_parse() {
        let html = r#"
            <div data-iid="translator.5023">
            <script>var IG="X"; IG:"A70492BA11ED48F788491917592E8066";
            var params_AbusePreventionHelper = [1785149075369,"osqfrid1MvWDR6aLEaW2pSHZAQ1i1h3E",3600000];</script>
            <div data-iid="translator.5024"></div>"#;
        let t = parse_bing_token(html).expect("应解析出 token");
        assert_eq!(t.key, 1785149075369);
        assert_eq!(t.token, "osqfrid1MvWDR6aLEaW2pSHZAQ1i1h3E");
        assert_eq!(t.ig, "A70492BA11ED48F788491917592E8066");
        assert_eq!(t.iid, "translator.5023"); // 取第一个（输入容器）
        assert_eq!(t.expiry_ms, 3600000);
    }

    #[test]
    fn token_validity_window() {
        let mut t = BingToken {
            key: now_ms(),
            token: "x".into(),
            ig: "x".into(),
            iid: "x".into(),
            expiry_ms: 3_600_000,
        };
        assert!(t.valid(), "刚铸造应有效");
        t.key = now_ms().saturating_sub(3_600_000); // 已过整个有效期
        assert!(!t.valid(), "过期应失效");
    }

    // 真跑冒烟（需联网经 Clash）：`cargo test online -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn online_real_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (out, eng) = rt
            .block_on(translate_online(
                "hello world",
                "Chinese",
                &["bing".to_string(), "google".to_string()],
            ))
            .expect("在线翻译应成功");
        println!("在线译文 [{eng}] hello world => {out}");
        assert!(!out.is_empty());
        assert!(crate::engine::lang::has_cjk(&out), "英→中应含中文: {out}");
    }
}
