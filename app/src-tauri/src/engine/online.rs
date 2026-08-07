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

/// **「译点支持哪些语言」的唯一真相源**：(内部名, 中文显示名, 谷歌码, 微软码)。
///
/// 内部名同时是 `lang.rs` 里 `ScriptLang::name()` 的产物、prompt 里的语言名、历史记录的标签、
/// 以及前端下拉框的 value —— 全项目只认这一套命名，**不要再引入 zh/en 这类语言码**。
/// 前端的语言列表由 `supported_languages()` 直接从这张表生成，不许在前端再抄一份。
///
/// ⚠ **2026-08-07 拆掉了原来的 `_ => ("en","en")` 兜底**，那是两个叠加的静默失败：
/// ①任何没登记的语言被**悄悄降级成英文** —— 界面显示"翻译成功"，内容却是英文；
/// ②实测 Google gtx 收到无效 `tl`（包括误把语言名当代码传进去的 `tl=Chinese`）
///   **返回 HTTP 200 且原样不翻译**，连报错都没有。
/// 方向可由用户自选之后，下拉框里一出现没登记的语言就会踩，所以必须显式失败。
///
/// ⚠ 中文两家的码**确实不同，别"统一"**：Google 用 `zh-CN`，而 Azure/Bing 的语言表里
/// 根本没有 `zh-CN` 这一条、只有 `zh-Hans`/`zh-Hant`。
///
/// ⚠ 葡语反过来，两家**恰好都是 `pt`**：微软只认 `pt`(巴西)/`pt-pt`(葡萄牙)，
/// **没有 `pt-br`**。写 `pt-br` 会拿到 `{"statusCode":400}`，还会被 `bing_translate`
/// 误判成 token 失效、白刷一次 token 再失败一遍（2026-08-07 真打微软端点逐码实测：
/// `to=pt-br` → 400；`to=pt` / `to=pt-PT` → 200；其余 15 个码全部 200）。
pub const LANGS: &[(&str, &str, &str, &str)] = &[
    ("Chinese", "中文", "zh-CN", "zh-Hans"),
    ("English", "英语", "en", "en"),
    ("Japanese", "日语", "ja", "ja"),
    ("Korean", "韩语", "ko", "ko"),
    ("French", "法语", "fr", "fr"),
    ("German", "德语", "de", "de"),
    ("Spanish", "西班牙语", "es", "es"),
    ("Russian", "俄语", "ru", "ru"),
    ("Portuguese", "葡萄牙语", "pt", "pt"),
    ("Italian", "意大利语", "it", "it"),
    ("Thai", "泰语", "th", "th"),
    ("Arabic", "阿拉伯语", "ar", "ar"),
    ("Greek", "希腊语", "el", "el"),
    ("Hebrew", "希伯来语", "he", "he"),
    ("Hindi", "印地语", "hi", "hi"),
    ("Vietnamese", "越南语", "vi", "vi"),
];

/// 源语言"交给引擎自己识别"时用的码。**自动识别比我们的脚本规则准得多**
/// （尤其是只含汉字的日语这类脚本层原理上分不开的情况），所以除非用户手选，一律走它。
const AUTO_GOOGLE: &str = "auto";
const AUTO_BING: &str = "auto-detect";

/// 语言名 → (谷歌码, 微软码)。未收录返回 `None`。源语言和目标语言用的是同一套码。
pub fn lang_codes(name: &str) -> Option<(&'static str, &'static str)> {
    LANGS
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(_, _, g, b)| (*g, *b))
}

/// 这个名字是不是受支持的语言（校验设置值/手选值用）。
pub fn is_supported(name: &str) -> bool {
    lang_codes(name).is_some()
}

/// 这个语言能不能当**母语**。
///
/// ⚠ 母语不是普通的语言选项：它要参与「这段文字是不是母语」的比较，比较的另一边是
/// `detect_script_lang(text).name()`。而脚本判定对**全部拉丁字母**一律产出 `English`
/// （`ScriptLang::Latin | Unknown => "English"`，脚本层原理上区分不了英/法/德/西）。
/// 于是把法语设成母语时 `src == native` **永远不成立** ⇒ 法语原文被判成外语、"译回法语"
/// ＝原地不动，而引擎照样返回 200，界面上看不出任何错。
///
/// 所以母语的可选集必须收窄到「脚本层真能判出来的那 10 个」。
/// 「母语译成」(`native_to`) 只当目标用、不参与比较，不受这条限制，仍是全部 16 个。
/// 2026-08-07 对抗复核实证。
pub fn is_native_selectable(name: &str) -> bool {
    use crate::engine::lang::ScriptLang::*;
    [
        Japanese,
        Korean,
        Chinese,
        Cyrillic,
        Arabic,
        Greek,
        Thai,
        Hebrew,
        Devanagari,
        Latin,
    ]
    .iter()
    .any(|l| l.name() == name)
}

/// 给前端的语言下拉项。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LangOption {
    /// 内部名，也是提交回后端时用的值。
    pub name: String,
    /// 中文显示名。
    pub label: String,
    /// 能不能出现在「我的母语」那个下拉里（见 [`is_native_selectable`]）。
    pub native_ok: bool,
}

pub fn supported_languages() -> Vec<LangOption> {
    LANGS
        .iter()
        .map(|(n, l, ..)| LangOption {
            name: (*n).to_string(),
            label: (*l).to_string(),
            native_ok: is_native_selectable(n),
        })
        .collect()
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

async fn bing_translate(text: &str, from_b: &str, to_b: &str) -> Result<String, String> {
    let token = get_token(false).await?;
    match bing_call(&token, text, from_b, to_b).await {
        Ok(s) => Ok(s),
        Err(_) => {
            // token 可能失效，强刷一次再试；再败则上抛触发兜底
            let token = get_token(true).await?;
            bing_call(&token, text, from_b, to_b).await
        }
    }
}

async fn google_translate(text: &str, sl: &str, tl: &str) -> Result<String, String> {
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl={}&tl={}&dt=t&q={}",
        sl,
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
///
/// `src` 为 `None` 时把源语言交给引擎自动识别 —— **这才是常态**。只有用户在界面上
/// 明确选了源语言才传 `Some`：我们自己的脚本规则在"只含汉字的日语"这类情况上原理性地
/// 分不出来（见 `lang.rs` 的已知盲区），拿它去覆盖引擎的自动识别只会更差。
pub async fn translate_online(
    text: &str,
    src: Option<&str>,
    tgt: &str,
    order: &[String],
) -> Result<(String, String), String> {
    let (g_tl, b_to) = lang_codes(tgt)
        .ok_or_else(|| format!("不支持的目标语言「{tgt}」——请在语言列表里换一个"))?;
    let (g_sl, b_from) = match src {
        None => (AUTO_GOOGLE, AUTO_BING),
        Some(s) => lang_codes(s)
            .ok_or_else(|| format!("不支持的源语言「{s}」——请在语言列表里换一个"))?,
    };
    let mut last_err = String::from("无可用在线引擎");
    for eng in order {
        let r = match eng.as_str() {
            "bing" => bing_translate(text, b_from, b_to)
                .await
                .map(|s| (s, "微软".to_string())),
            "google" => google_translate(text, g_sl, g_tl)
                .await
                .map(|s| (s, "谷歌".to_string())),
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
    let _ = translate_online("hi", None, "Chinese", order).await;
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

    // ---- 语言表：它同时是前端下拉、方向解析、引擎调用的真相源，错一条到处错 ----

    /// `lang.rs` 自动判出来的每一个语言名都必须能在表里查到码，否则"自动识别 → 译回母语"
    /// 这条主路径会在某些语言上直接报"不支持"。
    #[test]
    fn every_detectable_language_has_codes() {
        use crate::engine::lang::ScriptLang::*;
        for l in [
            Japanese, Korean, Chinese, Cyrillic, Arabic, Greek, Thai, Hebrew, Devanagari, Latin,
            Unknown,
        ] {
            assert!(
                is_supported(l.name()),
                "脚本判定会产出「{}」，但语言表里没有它",
                l.name()
            );
        }
    }

    #[test]
    fn lang_table_is_self_consistent() {
        // 名字唯一（重复会让 find 静默取第一条，改码时改了后一条却不生效）
        let mut names: Vec<&str> = LANGS.iter().map(|(n, ..)| *n).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "语言表里有重名");

        // 两家的中文码确实不同，别被"统一"掉
        assert_eq!(lang_codes("Chinese"), Some(("zh-CN", "zh-Hans")));
        // ⚠ 葡语两家都是 pt。微软**没有 pt-br**（实测 400），别照着中文的样子想当然分家。
        assert_eq!(lang_codes("Portuguese"), Some(("pt", "pt")));
        assert_eq!(lang_codes("不存在的语言"), None);

        // 下拉项与表逐条对应
        let opts = supported_languages();
        assert_eq!(opts.len(), LANGS.len());
        assert_eq!(opts[0].name, "Chinese");
        assert_eq!(opts[0].label, "中文");
    }

    /// 母语可选集必须恰好等于「脚本层能判出来的语言」——多一个就会出现
    /// "母语文本被判成外语、译回母语＝原地不动"，而且引擎返回 200、界面看不出错。
    #[test]
    fn native_selectable_is_exactly_the_script_detectable_set() {
        use crate::engine::lang::ScriptLang;
        // 每个脚本判定的产出都必须可选为母语（否则那门语言的用户根本没法正确设置）
        for l in [
            ScriptLang::Japanese,
            ScriptLang::Korean,
            ScriptLang::Chinese,
            ScriptLang::Cyrillic,
            ScriptLang::Arabic,
            ScriptLang::Greek,
            ScriptLang::Thai,
            ScriptLang::Hebrew,
            ScriptLang::Devanagari,
            ScriptLang::Latin,
            ScriptLang::Unknown,
        ] {
            assert!(is_native_selectable(l.name()), "{} 应可选为母语", l.name());
        }
        // 拉丁语系里除英语外一个都不许当母语：脚本层对它们一律产出 English，
        // 设成母语后 src==native 永远不成立（2026-08-07 实证）。
        for n in [
            "French",
            "German",
            "Spanish",
            "Portuguese",
            "Italian",
            "Vietnamese",
        ] {
            assert!(!is_native_selectable(n), "{n} 不该能当母语");
            assert!(is_supported(n), "{n} 仍应能当目标语言");
        }
        assert_eq!(
            supported_languages().iter().filter(|o| o.native_ok).count(),
            10
        );
    }

    /// **语言码真跑体检**：把 `LANGS` 里每个码分别喂给微软和谷歌，看有没有被拒的。
    ///
    /// 为什么非真跑不可：语言码写错**不会**在任何纯逻辑测试里现形，而线上表现极其隐蔽 ——
    /// 微软返回 `{"statusCode":400}`（还会被 `bing_translate` 误判成 token 失效、白刷一次
    /// token 再失败一遍），谷歌更狠，收到无效 `tl` 时**返回 200 且原样不翻译**。
    /// 2026-08-07 正是靠真打端点才发现 Portuguese 的微软码 `pt-br` 是无效码（应为 `pt`）。
    ///
    /// 换语言表 / 加语言之后跑一次：
    /// `cargo test --lib all_language_codes -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn all_language_codes_are_accepted_by_both_engines_real() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut bad: Vec<String> = Vec::new();
        for (name, label, ..) in LANGS {
            // 源就是英语，英→英当然原样返回，不能当成"码无效"
            if *name == "English" {
                continue;
            }
            for (eng, order) in [("微软", vec!["bing".to_string()]), ("谷歌", vec!["google".to_string()])] {
                match rt.block_on(translate_online("hello world", Some("English"), name, &order)) {
                    Ok((out, _)) => {
                        // 谷歌对无效 tl 是"200 + 原样返回"，所以必须连内容一起验
                        if out.trim().eq_ignore_ascii_case("hello world") {
                            bad.push(format!("{eng} {name}({label}): 原样返回未翻译 ⇒ 码多半无效"));
                        } else {
                            println!("  OK {eng} {name}({label}) => {out}");
                        }
                    }
                    Err(e) => bad.push(format!("{eng} {name}({label}): {e}")),
                }
            }
        }
        assert!(bad.is_empty(), "有语言码不被引擎接受：\n{}", bad.join("\n"));
    }

    // 真跑冒烟（需联网经 Clash）：`cargo test online -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn online_real_smoke() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (out, eng) = rt
            .block_on(translate_online(
                "hello world",
                None,
                "Chinese",
                &["bing".to_string(), "google".to_string()],
            ))
            .expect("在线翻译应成功");
        println!("在线译文 [{eng}] hello world => {out}");
        assert!(!out.is_empty());
        assert!(crate::engine::lang::has_cjk(&out), "英→中应含中文: {out}");
    }
}

#[cfg(test)]
mod jp_smoke {
    /// 端到端真跑：日语 → 方向判定 → 在线引擎，译文必须是**中文**（用户报障的正是这里）。
    /// `cargo test -- --ignored jp_to_chinese`
    #[test]
    #[ignore]
    fn jp_to_chinese_real() {
        let text = "日本語を勉強しています";
        let (src, tgt) = crate::engine::lang::default_direction(text);
        assert_eq!((src, tgt), ("Japanese", "Chinese"), "方向判定错了");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let order = vec!["google".to_string(), "bing".to_string()];
        let (out, engine) = rt
            .block_on(super::translate_online(text, None, tgt, &order))
            .expect("在线翻译应成功");
        println!("REAL: 日语「{text}」--[{engine}]--> 「{out}」");
        assert!(
            out.chars().any(|c| (0x4E00..=0x9FFF).contains(&(c as u32))),
            "译文应含中文，实际={out}"
        );
        assert!(
            !out.chars().any(|c| (0x3040..=0x30FF).contains(&(c as u32))),
            "译文不应还留着假名（说明没翻），实际={out}"
        );
    }

    /// 手选方向真跑：把「只含汉字的日语」这个**原理上判不出来的盲区**交给用户手选之后，
    /// 必须真的按手选走 —— 源语言强制 Japanese、目标 Chinese，译文得是中文而不是日文原样。
    /// 这条同时验证了"手选源语言真的传给了在线引擎"（否则 sl=auto 会把它当中文，原样返回）。
    /// `cargo test -- --ignored manual_direction`
    #[test]
    #[ignore]
    fn manual_direction_overrides_detection_real() {
        let text = "東京都新宿区";
        // 前提：自动判定在这里必然判成中文（已知盲区，见 lang.rs）
        assert_eq!(
            crate::engine::lang::default_direction(text),
            ("Chinese", "English"),
            "盲区行为变了，请先确认 lang.rs 的改动"
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let order = vec!["google".to_string(), "bing".to_string()];
        let (out, engine) = rt
            .block_on(super::translate_online(text, Some("Japanese"), "Chinese", &order))
            .expect("在线翻译应成功");
        println!("REAL: 手选日→中「{text}」--[{engine}]--> 「{out}」");
        assert!(!out.is_empty());
        assert_ne!(out.trim(), text, "原样返回＝手选的源语言没传到引擎");
    }
}
