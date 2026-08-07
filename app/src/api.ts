import { invoke } from "@tauri-apps/api/core";
import type {
  Engine,
  TranslateResult,
  HistoryItem,
  DictItem,
  DictResult,
  LangOption,
  ManualDir,
  HotkeyInfo,
  HotkeySetResult,
} from "./types";

// ---------------- 翻译 ----------------
export function translate(text: string, engine: Engine): Promise<TranslateResult> {
  return invoke<TranslateResult>("translate", { text, engine });
}

// ---------------- 我的翻译（历史） ----------------
export function historyList(
  query: string,
  favoritesOnly: boolean,
  limit = 200,
): Promise<HistoryItem[]> {
  return invoke<HistoryItem[]>("history_list", { query, favoritesOnly, limit });
}
export function historyDelete(id: number): Promise<void> {
  return invoke("history_delete", { id });
}
export function historyToggleFavorite(id: number): Promise<boolean> {
  return invoke<boolean>("history_toggle_favorite", { id });
}
export function historyClear(): Promise<void> {
  return invoke("history_clear", {});
}

// ---------------- 设置 ----------------
export function settingsGetAll(): Promise<Record<string, string>> {
  return invoke<Record<string, string>>("settings_get_all", {});
}
export function settingsSet(key: string, value: string): Promise<void> {
  return invoke("settings_set", { key, value });
}

// ---------------- 语言方向 ----------------
/** 受支持的语言列表。**唯一真相源在后端**（engine/online.rs 的 LANGS），前端别再抄一份。 */
export function supportedLanguages(): Promise<LangOption[]> {
  return invoke<LangOption[]>("supported_languages", {});
}
/**
 * 设置本会话的手选方向，任一侧传 null = 交回自动。
 *
 * ⚠ 调用方必须 **await 完再触发重译**：两个 invoke 之间没有顺序保证，抢跑的话第一次
 * 重译还是按旧方向走，用户会看到"选了没用、再点一下才对"。
 */
export function setManualDirection(
  src: string | null,
  tgt: string | null,
): Promise<ManualDir> {
  return invoke<ManualDir>("set_manual_direction", { src, tgt });
}
export function getManualDirection(): Promise<ManualDir> {
  return invoke<ManualDir>("get_manual_direction", {});
}

// ---------------- 全局快捷键 ----------------
export function hotkeyList(): Promise<HotkeyInfo[]> {
  return invoke<HotkeyInfo[]>("hotkey_list", {});
}
/** 改一个热键。失败时旧键仍然生效，返回值里带着两个热键的最新状态。 */
export function hotkeySet(action: string, accel: string): Promise<HotkeySetResult> {
  return invoke<HotkeySetResult>("hotkey_set", { action, accel });
}
/** 「测一下」：开一个探测窗口，返回窗口毫秒数。期间按下该热键只回报、不执行动作。 */
export function hotkeyProbe(action: string): Promise<number> {
  return invoke<number>("hotkey_probe", { action });
}
/**
 * 撤掉探测窗口。**测到一半跑去干别的时必须调**：窗口留着的话，窗口内第一次按该热键
 * 会被后端吞成"探测命中"（只回报不执行），而前端已不在等待态、回报被丢弃 ⇒
 * 界面完全没反应，用户会误判成"这个键被别的程序占了"。
 */
export function hotkeyProbeCancel(action: string): Promise<void> {
  return invoke("hotkey_probe_cancel", { action });
}
/** 侧栏「截图翻译」按钮：走与热键完全相同的通路（后端会先把主窗最小化让开）。 */
export function triggerShot(): Promise<void> {
  return invoke("trigger_shot", {});
}
export function appVersion(): Promise<string> {
  return invoke<string>("app_version", {});
}

// ---------------- 词典 ----------------
/** 查一个词，返回各启用词典的命中（按优先级）。 */
export function dictLookup(word: string): Promise<DictResult[]> {
  return invoke<DictResult[]>("dict_lookup", { word });
}
export function dictList(): Promise<DictItem[]> {
  return invoke<DictItem[]>("dict_list", {});
}
export function dictSetEnabled(id: number, enabled: boolean): Promise<void> {
  return invoke("dict_set_enabled", { id, enabled });
}
export function dictAddMdx(path: string): Promise<DictItem> {
  return invoke<DictItem>("dict_add_mdx", { path });
}
export function dictRemove(id: number): Promise<void> {
  return invoke("dict_remove", { id });
}
export function dictReorder(ids: number[]): Promise<void> {
  return invoke("dict_reorder", { ids });
}
