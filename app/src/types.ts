// 前后端共享的数据形状。字段命名与 Rust 序列化（serde 默认 snake_case）保持一致。

export type Engine = "local" | "online";
export type ThemeMode = "light" | "dark" | "system";

export interface TranslateResult {
  text: string;
  src_lang: string; // 语言名（English/Chinese/…）
  tgt_lang: string;
  engine: string; // 实际服务的引擎：本地 / 微软 / 谷歌
  history_id: number; // 写入历史后的行 id（用于收藏；0=未记录）
  favorite: boolean; // 该条当前是否已收藏
}

export interface HistoryItem {
  id: number;
  source_text: string;
  translated_text: string;
  src_lang: string;
  tgt_lang: string;
  engine: string;
  favorite: boolean;
  created_at: number; // unix 秒
}

export interface DictItem {
  id: number;
  kind: string; // 'mdx'
  name: string;
  path: string;
  lang: string; // 预留
  enabled: boolean;
  sort_order: number;
}

export interface WordSense {
  pos: string; // 词性，如 n. / vt.（可能为空）
  text: string;
}

/** 一本词典对一个词的查词结果（mdx 走 html；结构化字段预留）。 */
export interface DictResult {
  word: string;
  source: string; // 词典展示名
  kind: string; // 'mdx'
  uk: string; // 音标（预留，可能空）
  us: string; // 音标（预留，可能空）
  senses: WordSense[]; // 结构化释义（预留）
  html: string; // mdx 原始词条 HTML
}
