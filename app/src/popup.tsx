import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { bootTheme } from "./theme";
import "@fontsource/noto-sans-sc/400.css";
import "@fontsource/noto-sans-sc/500.css";
import "./styles.css";
import "./popup.css";

interface Payload {
  src: string;
  dst: string;
  engine: string;
}

function Popup() {
  const [p, setP] = useState<Payload | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    bootTheme(); // 跟随主窗深浅色
    invoke<Payload | null>("take_popup_payload")
      .then((v) => setP(v))
      .catch(() => {});
    const close = () => invoke("close_popup");
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    // 点别处消失：blur 仅在窗口曾获焦后才触发，不会导致"从未获焦→瞬间消失"
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", close);
    // 兜底：18s 后自动收起
    const timer = window.setTimeout(close, 18000);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", close);
      window.clearTimeout(timer);
    };
  }, []);

  function copy() {
    if (!p?.dst) return;
    navigator.clipboard.writeText(p.dst);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  }

  return (
    <div className="pop">
      <div className="pop-card">
        <div className="pop-head">
          <span className="pop-tag">{p?.engine || "翻译中…"}</span>
          <button className="pop-copy" onClick={copy} disabled={!p?.dst}>
            {copied ? "✓ 已复制" : "复制译文"}
          </button>
          <button className="pop-copy" onClick={() => invoke("close_popup")} title="关闭">
            ✕
          </button>
        </div>
        {p?.src && <div className="pop-src">{p.src}</div>}
        <div className="pop-dst">{p ? p.dst : "…"}</div>
      </div>
    </div>
  );
}

createRoot(document.getElementById("popup-root")!).render(<Popup />);
