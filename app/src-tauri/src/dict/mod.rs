//! 词典层：编排 .mdx 词典，按启用/优先级查词，产出统一的 DictResult。

pub mod mdx;

use crate::db::DictRow;
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WordSense {
    pub pos: String,
    pub text: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DictResult {
    pub word: String,
    pub source: String,
    pub kind: String, // 'mdx'
    pub uk: String,
    pub us: String,
    pub senses: Vec<WordSense>,
    pub html: String,
}

/// 懒加载缓存：mdx 按注册 id。None = 加载失败（避免反复重试）。
pub struct DictCache {
    mdx: HashMap<i64, Option<mdx::MdxDict>>,
}

impl Default for DictCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DictCache {
    pub fn new() -> Self {
        Self {
            mdx: HashMap::new(),
        }
    }

    fn ensure_mdx(&mut self, id: i64, path: &str) {
        if self.mdx.contains_key(&id) {
            return;
        }
        let loaded = mdx::MdxDict::open(path).ok();
        self.mdx.insert(id, loaded);
    }

    fn mdx_lookup(&mut self, id: i64, path: &str, word: &str) -> Option<String> {
        self.ensure_mdx(id, path);
        self.mdx
            .get_mut(&id)
            .and_then(|o| o.as_mut())
            .and_then(|d| d.lookup(word))
    }

    pub fn evict_mdx(&mut self, id: i64) {
        self.mdx.remove(&id);
    }
}

/// 在启用词典（已按优先级排序）中查词，返回全部命中。
pub fn lookup(cache: &mut DictCache, dicts: &[DictRow], word: &str) -> Vec<DictResult> {
    let word = word.trim();
    if word.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in dicts {
        if d.kind == "mdx" {
            if let Some(html) = cache.mdx_lookup(d.id, &d.path, word) {
                if !html.trim().is_empty() {
                    out.push(DictResult {
                        word: word.to_string(),
                        source: d.name.clone(),
                        kind: "mdx".into(),
                        uk: String::new(),
                        us: String::new(),
                        senses: Vec::new(),
                        html,
                    });
                }
            }
        }
    }
    out
}
