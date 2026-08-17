import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "@fontsource/noto-sans-sc/400.css";
import "@fontsource/noto-sans-sc/500.css";
import "./shot.css";

interface Line {
  /** 整段的外接框（可能跨多行）。 */
  x: number;
  y: number;
  w: number;
  h: number;
  /** 段内逐行的框 [x,y,w,h]。用来算「这段原文一行有多高」——外接框的高是整段的。 */
  rects?: number[][];
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
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") invoke("close_shot");
    };
    // 关闭只由：右上角 ✕ / Esc。**不设自动消失**——用户明确要求「不需要关的时候它应该一直存在」。
    // 已废弃「点空白关」：空白区的边界不可见，底部工具条又吃掉点击，用户会点到不关的地方而以为卡死。
    // 不用 blur——全局触发时窗口未必获焦。
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!p) return <div className="shot" />;

  const W = p.width || 1;
  const H = p.height || 1;
  const dispScale = (p.disp_w || W) / W; // 显示像素 / 原图像素
  const dispH = p.disp_w * (H / W); // 图在窗口里的显示高
  // 一段可能跨多行，所以「原文一行有多高」要用段内行框去算，**不能拿段的外接框**
  // —— 拿外接框的话，一段 4 行的标题会被当成 4 倍高的字，字号直接爆掉。
  const lineHeightOf = (l: Line) =>
    l.rects && l.rects.length ? l.h / l.rects.length : l.h;
  // 字号**按每段自己的行高定**，不再全图统一：标题和正文本来就不一样大，
  // 统一成一个中位数必然一半偏大一半偏小（这是「看着歪」的一个来源）。
  const fontOf = (l: Line) =>
    Math.max(13, Math.min(30, lineHeightOf(l) * dispScale * 0.8));
  const joinedSrc = p.lines.map((l) => l.src).join("\n");
  const joinedDst = p.lines
    .map((l) => l.dst)
    .filter(Boolean)
    .join("\n");

  function copy() {
    navigator.clipboard.writeText(joinedDst);
    setCopied(true);
    setTimeout(() => setCopied(false), 1000);
  }

  return (
    <div className="shot">
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
                fontSize: `${fontOf(l)}px`,
              }}
            >
              {l.dst}
            </div>
          ) : null,
        )}
        <button className="closex" onClick={() => invoke("close_shot")} title="关闭（Esc）">
          ✕
        </button>
      </div>
      <div className="bar">
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
        <span className="tip">✕ 或 Esc 关 · 不点就一直在</span>
      </div>
    </div>
  );
}

createRoot(document.getElementById("shot-root")!).render(<Shot />);
