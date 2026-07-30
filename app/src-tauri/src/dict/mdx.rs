//! .mdx 词典支持（基于 rs-mdict crate，import 名为 rust_mdict）。
//!
//! 现状：覆盖 rs-mdict 支持的常见 mdx（v1/v2、zlib、UTF-8/UTF-16、未加密/index 加密）。
//! 加密/异常文件由 crate 报错，上层优雅降级。@@@LINK= 跳转在此处理（应用层，最多一跳）。

use rust_mdict::Mdx;

pub struct MdxDict {
    inner: Mdx,
}

impl MdxDict {
    pub fn open(path: &str) -> Result<Self, String> {
        Mdx::new(path)
            .map(|inner| Self { inner })
            .map_err(|e| format!("打开 mdx 失败: {e}"))
    }

    /// 查词返回词条 HTML；`@@@LINK=目标词` 自动跳一跳。
    pub fn lookup(&mut self, word: &str) -> Option<String> {
        let def = self.inner.lookup(word)?.definition;
        let trimmed = def.trim();
        if let Some(rest) = trimmed.strip_prefix("@@@LINK=") {
            let target = rest.trim_matches(|c| c == '\r' || c == '\n' || c == ' ');
            if !target.is_empty() && target != word {
                if let Some(hit) = self.inner.lookup(target) {
                    return Some(hit.definition);
                }
            }
        }
        Some(def)
    }
}
