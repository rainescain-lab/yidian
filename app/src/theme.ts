import type { ThemeMode } from "./types";

export const THEME_KEY = "yidian-theme";

function resolve(mode: ThemeMode): "light" | "dark" {
  if (mode === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  return mode;
}

let mediaCleanup: (() => void) | null = null;

/** 应用主题：写 data-theme；system 模式下监听系统深浅色变化。 */
export function watchTheme(mode: ThemeMode) {
  if (mediaCleanup) {
    mediaCleanup();
    mediaCleanup = null;
  }
  document.documentElement.setAttribute("data-theme", resolve(mode));
  localStorage.setItem(THEME_KEY, mode);

  if (mode === "system") {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => document.documentElement.setAttribute("data-theme", resolve("system"));
    mq.addEventListener("change", handler);
    mediaCleanup = () => mq.removeEventListener("change", handler);
  }
}

/** 启动即用（避免深色下白闪）：读上次主题并立即应用。返回该主题以初始化 state。 */
export function bootTheme(): ThemeMode {
  const saved = (localStorage.getItem(THEME_KEY) as ThemeMode) || "system";
  watchTheme(saved);
  return saved;
}
