import { invoke } from "@tauri-apps/api/core";
import type { Engine, TranslateResult, HistoryItem, DictItem, DictResult } from "./types";

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
