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

/** 一个可选语言。`name` 是提交回后端的值，`label` 是给人看的中文名。 */
export interface LangOption {
  name: string;
  label: string;
  /**
   * 能不能出现在「我的母语」下拉里。
   *
   * ⚠ 母语要参与「这段文字是不是母语」的判定，而译点在字符层面分不出拉丁语系各语言
   * （法语/德语/西语…一律判成 English）。把法语设成母语会导致法语原文被当成外语、
   * "译回法语"＝原地不动，而且引擎照样返回 200，界面上看不出任何错。
   * 所以母语只能从"真能判出来"的那 10 个里选；当**目标**语言则不受限。
   */
  native_ok: boolean;
}

/**
 * 本会话手选的翻译方向。`null` = 那一侧交给自动规则。
 *
 * ⚠ 这个状态的**真相源在后端**（AppState.manual_dir）：划词/截图走的是全局热键、
 * 完全不经过前端，它们要按同一个方向走就只能读后端那份。前端这份只是镜像，
 * 改完必须 await `setManualDirection` 再触发重译。
 */
export interface ManualDir {
  src: string | null;
  tgt: string | null;
}

/** 一个全局热键的状态。`accel` 是用户想要的，`ok` 才是真的生效了。 */
export interface HotkeyInfo {
  action: "shot" | "selection";
  accel: string;
  ok: boolean;
  error: string;
}

export interface HotkeySetResult {
  ok: boolean;
  message: string;
  /** 操作后两个热键的最新状态，直接拿去覆盖本地缓存，省一次往返。 */
  hotkeys: HotkeyInfo[];
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
