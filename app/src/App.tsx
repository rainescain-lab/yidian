import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { Engine, HotkeyInfo, LangOption, ManualDir, ThemeMode } from "./types";
import {
  appVersion,
  getManualDirection,
  hotkeyList,
  hotkeySet,
  setManualDirection,
  settingsGetAll,
  settingsSet,
  supportedLanguages,
} from "./api";
import { setLangLabels } from "./lib/format";
import { bootTheme, watchTheme } from "./theme";
import Sidebar from "./components/Sidebar";
import SelectionHint from "./components/SelectionHint";
import TranslateView from "./views/TranslateView";
import HistoryView from "./views/HistoryView";
import DictView from "./views/DictView";
import SettingsView from "./views/SettingsView";

// DB 读失败时的唯一兜底。⚠ 必须与 db.rs 的 seed_default_settings 逐条一致 ——
// 2026-08-07 发现两处早已漂移：这里写 default_engine:"local" 而 db.rs 是 "online"，
// 且完全漏了 ocr_engine。新增设置项时**两边都要加**。
const DEFAULTS: Record<string, string> = {
  theme: "system",
  default_engine: "online",
  online_order: "bing,google",
  ocr_engine: "fast",
  native_lang: "Chinese",
  native_to: "English",
  selection_follow_manual: "0",
  hotkey_shot: "alt+KeyQ",
  hotkey_selection: "alt+KeyW",
};

/** 热键还没从后端拉回来时的占位，让侧栏首帧不至于空着一块。 */
const HOTKEY_PLACEHOLDER: HotkeyInfo[] = [
  { action: "shot", accel: DEFAULTS.hotkey_shot, ok: true, error: "" },
  { action: "selection", accel: DEFAULTS.hotkey_selection, ok: true, error: "" },
];

export default function App() {
  const [view, setView] = useState("translate");
  const [settings, setSettings] = useState<Record<string, string> | null>(null);
  const [prefill, setPrefill] = useState<{ text: string; nonce: number } | null>(null);
  const [historyReload, setHistoryReload] = useState(0);
  const [langs, setLangs] = useState<LangOption[]>([]);
  const [hotkeys, setHotkeys] = useState<HotkeyInfo[]>(HOTKEY_PLACEHOLDER);
  const [version, setVersion] = useState("");
  const [showSelectionHint, setShowSelectionHint] = useState(false);
  /**
   * 手选方向。**会话内 sticky、不落盘**：手选是任务级意图不是偏好（详见 lib.rs 的
   * AppState.manual_dir）。这里只是后端那份的镜像，改动一律先 await 后端再重译。
   */
  const [dir, setDir] = useState<ManualDir>({ src: null, tgt: null });
  /** 探测回报：`{action, nonce}`；nonce 变化即表示"这个动作刚刚真的收到了按键"。 */
  const [probeHit, setProbeHit] = useState<{ action: string; nonce: number } | null>(null);
  /** 方向变化后要触发的重译信号（TranslateView 按 nonce 重跑）。 */
  const [dirNonce, setDirNonce] = useState(0);

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
    supportedLanguages()
      .then((l) => {
        setLangs(l);
        setLangLabels(l); // 历史页等处的 langLabel 也跟着后端表走
      })
      .catch(() => {});
    hotkeyList().then(setHotkeys).catch(() => {});
    appVersion().then(setVersion).catch(() => {});
    // 方向的真相源在后端。webview 单独重载（HMR / location.reload）时后端进程没重启、
    // 手选方向还在，不同步的话界面显示"自动"而实际按手选翻，两边说的不是一回事。
    getManualDirection().then(setDir).catch(() => {});
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

  // 「测一下」的回报：后端在探测窗口内收到热键就发这个事件
  useEffect(() => {
    const un = listen<string>("yidian://hotkey-probe", (e) => {
      setProbeHit({ action: e.payload, nonce: performance.now() });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  function changeSetting(key: string, value: string) {
    const prevValue = settings?.[key] ?? DEFAULTS[key];
    setSettings((prev) => ({ ...(prev || DEFAULTS), [key]: value }));
    settingsSet(key, value)
      .then(() => {
        // 母语规则变了，当前这段文字的方向也就变了，重译一次免得停在旧结果上。
        // ⚠ 必须等写库**返回之后**再触发：方向是后端现读 settings 算的，
        // 抢在写入前重译会按旧母语翻一次，用户看到的是"改了设置没反应、再动一下才对"。
        if (key === "native_lang" || key === "native_to") setDirNonce((n) => n + 1);
      })
      .catch(() => {
        // 后端会拒掉不认识的语言名和 hotkey_*（后者必须走 hotkey_set 才算数）。
        // 拒了就把界面回滚：显示一个库里根本没存下的值，比报错更难查。
        setSettings((prev) => ({ ...(prev || DEFAULTS), [key]: prevValue }));
      });
    if (key === "theme") watchTheme(value as ThemeMode);
  }

  /**
   * 改热键。真相源是后端返回的 hotkeys 快照 —— 不要在这里乐观更新：
   * 注册失败时旧键仍然生效，乐观更新会让界面显示一个**按下去没反应**的组合。
   */
  const commitHotkey = useCallback(async (action: string, accel: string) => {
    const r = await hotkeySet(action, accel);
    if (r.hotkeys?.length) setHotkeys(r.hotkeys);
    return { ok: r.ok, message: r.message };
  }, []);

  /** 方向变更：**先落到后端再重译**（划词/截图读的是后端那份，顺序反了会用旧方向翻一次）。 */
  const commitDir = useCallback(async (next: ManualDir) => {
    try {
      const saved = await setManualDirection(next.src, next.tgt);
      setDir(saved);
    } catch {
      // 后端拒了（不支持的语言）就不动本地状态，避免界面显示一个后端不认的方向
      return;
    }
    setDirNonce((n) => n + 1);
  }, []);

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
      <Sidebar
        active={view}
        onNavigate={setView}
        hotkeys={hotkeys}
        onSelectionHint={() => setShowSelectionHint(true)}
      />
      <main className="main">
        {/* 翻译页常驻挂载（保留输入/结果），其余按需 */}
        <div style={{ display: view === "translate" ? "contents" : "none" }}>
          <TranslateView
            defaultEngine={defaultEngine}
            prefill={prefill}
            langs={langs}
            dir={dir}
            dirNonce={dirNonce}
            onChangeDir={commitDir}
            onTranslated={() => setHistoryReload((n) => n + 1)}
          />
        </div>
        {view === "history" && <HistoryView onPick={pickFromHistory} reloadKey={historyReload} />}
        {view === "dict" && <DictView />}
        {view === "settings" && (
          <SettingsView
            settings={settings}
            onChange={changeSetting}
            onNavigate={setView}
            langs={langs}
            hotkeys={hotkeys}
            onCommitHotkey={commitHotkey}
            probeHit={probeHit}
            version={version}
          />
        )}
      </main>
      {showSelectionHint && (
        <SelectionHint
          hotkeys={hotkeys}
          onClose={() => setShowSelectionHint(false)}
          onGoSettings={() => {
            setShowSelectionHint(false);
            setView("settings");
          }}
        />
      )}
    </div>
  );
}
