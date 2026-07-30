const LANG_LABEL: Record<string, string> = {
  English: "英语",
  Chinese: "中文",
  Japanese: "日语",
  Korean: "韩语",
  French: "法语",
  German: "德语",
  Spanish: "西班牙语",
  Russian: "俄语",
  Portuguese: "葡萄牙语",
  auto: "自动",
};

export function langLabel(name: string): string {
  return LANG_LABEL[name] || name;
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
