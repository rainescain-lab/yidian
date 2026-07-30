//! 本机 SQLite 存储层（rusqlite）：翻译历史 / 设置 / 词典注册表。
//!
//! 设计要点：
//! - 所有函数只吃 `&Connection`，不碰 Tauri，故可用 `Connection::open_in_memory()` 做纯单测。
//! - 历史按 `source_text` 唯一去重（upsert）：同一段原文再翻只更新译文并置顶，不产生重复条目。
//! - 置顶用单调递增的 `seq`（每次 insert/update 取 MAX(seq)+1），不依赖时钟粒度 —— 保证
//!   「重译移到最前」在同一秒内也可确定性验证。`created_at`（秒）仅用于展示。

use rusqlite::{named_params, params, Connection, OptionalExtension, Result};
use std::collections::HashMap;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS history (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    source_text     TEXT NOT NULL UNIQUE,
    translated_text TEXT NOT NULL,
    src_lang        TEXT NOT NULL DEFAULT '',
    tgt_lang        TEXT NOT NULL DEFAULT '',
    engine          TEXT NOT NULL DEFAULT '',
    favorite        INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    seq             INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS dictionaries (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,              -- 'mdx'
    name       TEXT NOT NULL,
    path       TEXT NOT NULL DEFAULT '',   -- mdx 文件路径
    lang       TEXT NOT NULL DEFAULT '',   -- 预留
    enabled    INTEGER NOT NULL DEFAULT 1,
    sort_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE(kind, path, lang)
);
"#;

// ---------------------------------------------------------------------------
// 结构体
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryRow {
    pub id: i64,
    pub source_text: String,
    pub translated_text: String,
    pub src_lang: String,
    pub tgt_lang: String,
    pub engine: String,
    pub favorite: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DictRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub lang: String,
    pub enabled: bool,
    pub sort_order: i64,
}

// ---------------------------------------------------------------------------
// 初始化 / 种子
// ---------------------------------------------------------------------------

pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// 只在缺失时写入默认设置，不覆盖用户已改的值。
pub fn seed_default_settings(conn: &Connection) -> Result<()> {
    let defaults = [
        ("theme", "system"),          // light | dark | system
        ("default_engine", "online"), // local | online（在线更快，卡时自动回退本地）
        ("online_order", "bing,google"),
        ("ocr_engine", "fast"),       // fast(Windows WinRT) | accurate(qwen3-vl)
    ];
    for (k, v) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO settings(key, value) VALUES(?1, ?2)",
            params![k, v],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 历史
// ---------------------------------------------------------------------------

/// 写入一条历史。原文/译文为空则跳过。按 source_text 去重：已存在则更新译文+置顶，
/// 保留原有 favorite。
pub fn add_history(
    conn: &Connection,
    source_text: &str,
    translated_text: &str,
    src_lang: &str,
    tgt_lang: &str,
    engine: &str,
) -> Result<()> {
    if source_text.trim().is_empty() || translated_text.trim().is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO history
            (source_text, translated_text, src_lang, tgt_lang, engine, created_at, seq)
         VALUES
            (?1, ?2, ?3, ?4, ?5,
             CAST(strftime('%s','now') AS INTEGER),
             (SELECT COALESCE(MAX(seq), 0) + 1 FROM history))
         ON CONFLICT(source_text) DO UPDATE SET
            translated_text = excluded.translated_text,
            src_lang        = excluded.src_lang,
            tgt_lang        = excluded.tgt_lang,
            engine          = excluded.engine,
            created_at      = excluded.created_at,
            seq             = excluded.seq",
        params![source_text, translated_text, src_lang, tgt_lang, engine],
    )?;
    Ok(())
}

const HISTORY_COLS: &str =
    "id, source_text, translated_text, src_lang, tgt_lang, engine, favorite, created_at";

fn map_history(row: &rusqlite::Row) -> Result<HistoryRow> {
    Ok(HistoryRow {
        id: row.get(0)?,
        source_text: row.get(1)?,
        translated_text: row.get(2)?,
        src_lang: row.get(3)?,
        tgt_lang: row.get(4)?,
        engine: row.get(5)?,
        favorite: row.get::<_, i64>(6)? != 0,
        created_at: row.get(7)?,
    })
}

/// 列出历史，最近置顶（seq DESC）。`query` 非空则模糊匹配原文或译文；
/// `favorites_only` 只看收藏。
pub fn list_history(
    conn: &Connection,
    query: Option<&str>,
    favorites_only: bool,
    limit: i64,
) -> Result<Vec<HistoryRow>> {
    let like = query
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q));

    let mut sql = format!("SELECT {HISTORY_COLS} FROM history WHERE 1=1");
    if like.is_some() {
        sql.push_str(" AND (source_text LIKE :like OR translated_text LIKE :like)");
    }
    if favorites_only {
        sql.push_str(" AND favorite = 1");
    }
    sql.push_str(" ORDER BY seq DESC LIMIT :limit");

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(like) = &like {
        stmt.query_map(named_params! { ":like": like, ":limit": limit }, map_history)?
            .collect::<Result<Vec<_>>>()?
    } else {
        stmt.query_map(named_params! { ":limit": limit }, map_history)?
            .collect::<Result<Vec<_>>>()?
    };
    Ok(rows)
}

pub fn delete_history(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM history WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn clear_history(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM history", [])?;
    Ok(())
}

/// 切换收藏，返回切换后的状态。
pub fn toggle_favorite(conn: &Connection, id: i64) -> Result<bool> {
    conn.execute(
        "UPDATE history SET favorite = 1 - favorite WHERE id = ?1",
        params![id],
    )?;
    let fav: Option<i64> = conn
        .query_row("SELECT favorite FROM history WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(fav.unwrap_or(0) != 0)
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

pub fn get_all_settings(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut map = HashMap::new();
    for r in rows {
        let (k, v) = r?;
        map.insert(k, v);
    }
    Ok(map)
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| {
        r.get::<_, String>(0)
    })
    .optional()
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 词典注册表
// ---------------------------------------------------------------------------

fn map_dict(row: &rusqlite::Row) -> Result<DictRow> {
    Ok(DictRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        path: row.get(3)?,
        lang: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        sort_order: row.get(6)?,
    })
}

/// 列出全部词典，按查词优先级（sort_order 升序，同序内内置在前）。
pub fn list_dicts(conn: &Connection) -> Result<Vec<DictRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, path, lang, enabled, sort_order
         FROM dictionaries
         ORDER BY sort_order ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([], map_dict)?
        .collect::<Result<Vec<_>>>()?;
    Ok(rows)
}

/// 只列启用中的词典（查词时用）。
pub fn list_enabled_dicts(conn: &Connection) -> Result<Vec<DictRow>> {
    Ok(list_dicts(conn)?.into_iter().filter(|d| d.enabled).collect())
}

pub fn get_dict(conn: &Connection, id: i64) -> Result<Option<DictRow>> {
    conn.query_row(
        "SELECT id, kind, name, path, lang, enabled, sort_order FROM dictionaries WHERE id = ?1",
        params![id],
        map_dict,
    )
    .optional()
}

pub fn set_dict_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE dictionaries SET enabled = ?2 WHERE id = ?1",
        params![id, enabled as i64],
    )?;
    Ok(())
}

/// 导入一本 mdx 词典，返回新行 id。若同路径已存在则返回既有 id（幂等）。
pub fn add_mdx_dict(conn: &Connection, name: &str, path: &str) -> Result<i64> {
    let next_order: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM dictionaries",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO dictionaries(kind, name, path, lang, enabled, sort_order)
         VALUES('mdx', ?1, ?2, '', 1, ?3)",
        params![name, path, next_order],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM dictionaries WHERE kind='mdx' AND path = ?1",
        params![path],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// 删除一本 mdx 词典。
pub fn remove_dict(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM dictionaries WHERE id = ?1 AND kind = 'mdx'",
        params![id],
    )?;
    Ok(())
}

/// 按给定 id 顺序重排 sort_order（查词优先级）。
pub fn reorder_dicts(conn: &Connection, ordered_ids: &[i64]) -> Result<()> {
    for (i, id) in ordered_ids.iter().enumerate() {
        conn.execute(
            "UPDATE dictionaries SET sort_order = ?2 WHERE id = ?1",
            params![id, i as i64],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试（内存库，纯逻辑）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        c
    }

    #[test]
    fn schema_init_is_idempotent() {
        let c = mem();
        // 再跑一次不应报错
        init_schema(&c).unwrap();
        // 三张表都在
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('history','settings','dictionaries')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn add_and_list_history() {
        let c = mem();
        add_history(&c, "hello", "你好", "English", "Chinese", "local").unwrap();
        let rows = list_history(&c, None, false, 50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_text, "hello");
        assert_eq!(rows[0].translated_text, "你好");
        assert_eq!(rows[0].engine, "local");
        assert!(!rows[0].favorite);
    }

    #[test]
    fn empty_source_or_target_is_skipped() {
        let c = mem();
        add_history(&c, "   ", "你好", "English", "Chinese", "local").unwrap();
        add_history(&c, "hello", "  ", "English", "Chinese", "local").unwrap();
        assert_eq!(list_history(&c, None, false, 50).unwrap().len(), 0);
    }

    #[test]
    fn re_adding_same_source_dedups_and_moves_to_top() {
        let c = mem();
        add_history(&c, "hello", "你好", "English", "Chinese", "local").unwrap();
        add_history(&c, "world", "世界", "English", "Chinese", "local").unwrap();
        // 重译 hello（不同译文/引擎）
        add_history(&c, "hello", "喂", "English", "Chinese", "online").unwrap();

        let rows = list_history(&c, None, false, 50).unwrap();
        assert_eq!(rows.len(), 2, "同原文不产生重复条目");
        assert_eq!(rows[0].source_text, "hello", "重译后置顶");
        assert_eq!(rows[0].translated_text, "喂", "译文被更新");
        assert_eq!(rows[0].engine, "online");
        assert_eq!(rows[1].source_text, "world");
    }

    #[test]
    fn search_matches_source_and_target() {
        let c = mem();
        add_history(&c, "hello", "你好", "English", "Chinese", "local").unwrap();
        add_history(&c, "computer", "计算机", "English", "Chinese", "local").unwrap();

        // 命中原文
        let by_src = list_history(&c, Some("comp"), false, 50).unwrap();
        assert_eq!(by_src.len(), 1);
        assert_eq!(by_src[0].source_text, "computer");

        // 命中译文（中文）
        let by_tgt = list_history(&c, Some("你好"), false, 50).unwrap();
        assert_eq!(by_tgt.len(), 1);
        assert_eq!(by_tgt[0].source_text, "hello");

        // 空白查询等同不过滤
        assert_eq!(list_history(&c, Some("   "), false, 50).unwrap().len(), 2);
    }

    #[test]
    fn favorites_toggle_and_filter_and_preserved_on_readd() {
        let c = mem();
        add_history(&c, "hello", "你好", "English", "Chinese", "local").unwrap();
        let id = list_history(&c, None, false, 50).unwrap()[0].id;

        // 切收藏
        assert!(toggle_favorite(&c, id).unwrap());
        assert_eq!(list_history(&c, None, true, 50).unwrap().len(), 1);

        // 收藏应在重译后保留
        add_history(&c, "hello", "喂", "English", "Chinese", "online").unwrap();
        let rows = list_history(&c, None, true, 50).unwrap();
        assert_eq!(rows.len(), 1, "重译不清收藏");
        assert!(rows[0].favorite);

        // 再切回
        assert!(!toggle_favorite(&c, id).unwrap());
        assert_eq!(list_history(&c, None, true, 50).unwrap().len(), 0);
    }

    #[test]
    fn delete_and_clear() {
        let c = mem();
        add_history(&c, "a", "甲", "English", "Chinese", "local").unwrap();
        add_history(&c, "b", "乙", "English", "Chinese", "local").unwrap();
        let id = list_history(&c, Some("a"), false, 50).unwrap()[0].id;
        delete_history(&c, id).unwrap();
        assert_eq!(list_history(&c, None, false, 50).unwrap().len(), 1);
        clear_history(&c).unwrap();
        assert_eq!(list_history(&c, None, false, 50).unwrap().len(), 0);
    }

    #[test]
    fn limit_caps_results() {
        let c = mem();
        for i in 0..5 {
            add_history(&c, &format!("w{i}"), &format!("词{i}"), "English", "Chinese", "local")
                .unwrap();
        }
        assert_eq!(list_history(&c, None, false, 3).unwrap().len(), 3);
    }

    #[test]
    fn settings_get_set_and_defaults() {
        let c = mem();
        seed_default_settings(&c).unwrap();
        let s = get_all_settings(&c).unwrap();
        assert_eq!(s.get("theme").map(String::as_str), Some("system"));
        assert_eq!(s.get("default_engine").map(String::as_str), Some("online"));

        // 覆盖
        set_setting(&c, "theme", "dark").unwrap();
        // seed 再跑不应覆盖用户值
        seed_default_settings(&c).unwrap();
        assert_eq!(
            get_all_settings(&c).unwrap().get("theme").map(String::as_str),
            Some("dark")
        );
    }

    #[test]
    fn mdx_add_remove_reorder() {
        let c = mem();
        let id1 = add_mdx_dict(&c, "牛津高阶", "D:/dict/oald.mdx").unwrap();
        // 幂等：同路径再加返回同 id
        let id1b = add_mdx_dict(&c, "牛津高阶(改名)", "D:/dict/oald.mdx").unwrap();
        assert_eq!(id1, id1b);
        let id2 = add_mdx_dict(&c, "柯林斯", "D:/dict/collins.mdx").unwrap();
        assert_eq!(list_dicts(&c).unwrap().len(), 2);

        // 重排：把 collins 排到最前
        reorder_dicts(&c, &[id2, id1]).unwrap();
        assert_eq!(list_dicts(&c).unwrap()[0].id, id2);

        // 启用/禁用
        set_dict_enabled(&c, id1, false).unwrap();
        assert_eq!(list_enabled_dicts(&c).unwrap().len(), 1);

        // 删 mdx
        remove_dict(&c, id1).unwrap();
        assert!(list_dicts(&c).unwrap().iter().all(|d| d.id != id1));
        assert_eq!(list_dicts(&c).unwrap().len(), 1);
    }
}
