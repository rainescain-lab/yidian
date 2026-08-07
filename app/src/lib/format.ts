/**
 * 语言名 → 中文显示名的**静态兜底表**。
 *
 * ⚠ 真相源是后端 `engine/online.rs` 的 `LANGS`，启动时由 App 调 `supportedLanguages()`
 * 拉下来喂给 `setLangLabels`。这张静态表只服务两种场景：①拉取完成之前的首帧渲染；
 * ②历史记录里存着的、后端语言表里已经没有的老语言名。**新增语言只改后端那张表**，
 * 别来这里加。
 */
const LANG_LABEL: Record<string, string> = {
  Chinese: "中文",
  English: "英语",
  Japanese: "日语",
  Korean: "韩语",
  French: "法语",
  German: "德语",
  Spanish: "西班牙语",
  Russian: "俄语",
  Portuguese: "葡萄牙语",
  Italian: "意大利语",
  Thai: "泰语",
  Arabic: "阿拉伯语",
  Greek: "希腊语",
  Hebrew: "希伯来语",
  Hindi: "印地语",
  Vietnamese: "越南语",
  auto: "自动",
};

/** 后端下发的语言名→显示名，优先于静态表。 */
let RUNTIME_LABEL: Record<string, string> = {};

/** App 启动拿到后端语言表后调一次。 */
export function setLangLabels(list: { name: string; label: string }[]): void {
  RUNTIME_LABEL = Object.fromEntries(list.map((l) => [l.name, l.label]));
}

export function langLabel(name: string): string {
  return RUNTIME_LABEL[name] || LANG_LABEL[name] || name;
}

export function timeAgo(sec: number): string {
  if (!sec) return "";
  const d = Date.now() / 1000 - sec;
  if (d < 60) return "刚刚";
  if (d < 3600) return `${Math.floor(d / 60)} 分钟前`;
  if (d < 86400) return `${Math.floor(d / 3600)} 小时前`;
  const date = new Date(sec * 1000);
  return `${date.getMonth() + 1} 月 ${date.getDate()} 日`;
}

/** 单词判定：无内部空白，且是英文词或短中文词。用于触发词典卡。 */
export function looksLikeWord(text: string): boolean {
  const t = text.trim();
  if (!t || /\s/.test(t)) return false;
  if (/^[A-Za-z][A-Za-z'’-]*$/.test(t)) return true; // 英文词
  if (/^[一-鿿]{1,8}$/.test(t)) return true; // 短中文词
  return false;
}
