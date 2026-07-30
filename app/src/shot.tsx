import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "@fontsource/noto-sans-sc/400.css";
import "@fontsource/noto-sans-sc/500.css";
import "./shot.css";

interface Line {
  x: number;
  y: number;
  w: number;
  h: number;
  src: string;
  dst: string;
}
interface Payload {
  image: string;
  width: number;
  height: number;
  disp_w: number;
  lines: Line[];
}

function Shot() {
  const [p, setP] = useState<Payload | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    invoke<Payload | null>("take_shot_payload")
      .then((v) => setP(v))
      .catch(() => {});
    const close = () => invoke("close_shot");
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    // 关闭只由：点空白(onClick) / Esc / 30s 超时。不用 blur——全局触发时窗口未必获焦。
    window.addEventListener("keydown", onKey);
    const t = window.setTimeout(close, 30000);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.clearTimeout(t);
    };
  }, []);

  if (!p) return <div className="shot" />;

  const W = p.width || 1;
  const H = p.height || 1;
  const dispScale = (p.disp_w || W) / W; // 显示像素 / 原图像素
  const dispH = p.disp_w * (H / W); // 图在窗口里的显示高
  const shownH = p.lines
    .filter((l) => l.dst)
    .map((l) => l.h)
    .sort((a, b) => a - b);
  const medianH = shownH.length ? shownH[Math.floor(shownH.length / 2)] : 20;
  // 全行统一字号（取框高中位数）→ 不再每行大小不一；按显示比例缩放并夹在可读区间
  const fontPx = Math.max(13, Math.min(30, medianH * dispScale * 0.8));
  const joinedSrc = p.lines.map((l) => l.src).join("\n");
  const joinedDst = p.lines
    .map((l) => l.dst)
    .filter(Boolean)
    .join("\n");

  function copy(e: React.MouseEvent) {
    e.stopPropagation();
    navigator.clipboard.writeText(joinedDst);
    setCopied(true);
    setTimeout(() => setCopied(false), 1000);
  }

  return (
    <div className="shot" onClick={() => invoke("close_shot")}>
      <div className="imgwrap" style={{ width: `${p.disp_w}px` }}>
        <img src={p.image} alt="" />
        {p.lines.map((l, i) =>
          l.dst ? (
            <div
              className="ln"
              key={i}
              title={l.src}
              style={{
                left: `${(l.x / W) * 100}%`,
                top: `${(l.y / H) * 100}%`,
                minWidth: `${(l.w / W) * p.disp_w}px`,
                maxWidth: `${Math.max(
                  (l.w / W) * p.disp_w,
                  (p.disp_w * (W - l.x)) / W - 4,
                )}px`,
                minHeight: `${(l.h / H) * dispH}px`,
                fontSize: `${fontPx}px`,
              }}
            >
              {l.dst}
            </div>
          ) : null,
        )}
      </div>
      <div className="bar" onClick={(e) => e.stopPropagation()}>
        <button
          className="b primary"
          onClick={() => invoke("edit_in_main", { text: joinedSrc })}
          title="把原文和译文放到主界面编辑"
        >
          编辑
        </button>
        <button className="b" onClick={copy}>
          {copied ? "已复制" : "复制"}
        </button>
        <span className="tip">点空白 / Esc 关</span>
      </div>
    </div>
  );
}

createRoot(document.getElementById("shot-root")!).render(<Shot />);
