import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { Engine } from "./types";
import { settingsGetAll, settingsSet } from "./api";
import { bootTheme, watchTheme } from "./theme";
import type { ThemeMode } from "./types";
import Sidebar from "./components/Sidebar";
import TranslateView from "./views/TranslateView";
import HistoryView from "./views/HistoryView";
import DictView from "./views/DictView";
import SettingsView from "./views/SettingsView";

const DEFAULTS: Record<string, string> = {
  theme: "system",
  default_engine: "local",
  online_order: "bing,google",
};

export default function App() {
  const [view, setView] = useState("translate");
  const [settings, setSettings] = useState<Record<string, string> | null>(null);
  const [prefill, setPrefill] = useState<{ text: string; nonce: number } | null>(null);
  const [historyReload, setHistoryReload] = useState(0);

  // 启动：先按上次主题即时渲染（防闪），再从后端拉设置对齐
  useEffect(() => {
    bootTheme();
    settingsGetAll()
      .then((s) => {
        const merged = { ...DEFAULTS, ...s };
        setSettings(merged);
        watchTheme(merged.theme as ThemeMode);
      })
      .catch(() => {
        setSettings({ ...DEFAULTS });
      });
  }, []);

  // 截图结果窗「编辑」→ 原文送进主界面（复用翻译/编辑/复制）
  useEffect(() => {
    const un = listen<string>("yidian://fill", (e) => {
      setPrefill({ text: e.payload, nonce: performance.now() });
      setView("translate");
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  function changeSetting(key: string, value: string) {
    setSettings((prev) => ({ ...(prev || DEFAULTS), [key]: value }));
    settingsSet(key, value).catch(() => {});
    if (key === "theme") watchTheme(value as ThemeMode);
  }

  function pickFromHistory(text: string) {
    setPrefill({ text, nonce: performance.now() });
    setView("translate");
  }

  if (!settings) {
    return <div className="app" />;
  }

  const defaultEngine: Engine = settings.default_engine === "online" ? "online" : "local";

  return (
    <div className="app">
      <Sidebar active={view} onNavigate={setView} />
      <main className="main">
        {/* 翻译页常驻挂载（保留输入/结果），其余按需 */}
        <div style={{ display: view === "translate" ? "contents" : "none" }}>
          <TranslateView
            defaultEngine={defaultEngine}
            prefill={prefill}
            onTranslated={() => setHistoryReload((n) => n + 1)}
          />
        </div>
        {view === "history" && <HistoryView onPick={pickFromHistory} reloadKey={historyReload} />}
        {view === "dict" && <DictView />}
        {view === "settings" && (
          <SettingsView settings={settings} onChange={changeSetting} onNavigate={setView} />
        )}
      </main>
    </div>
  );
}
