import { createRoot } from "react-dom/client";
import { useEffect, useState } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./overlay.css";

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

function Overlay() {
  const [start, setStart] = useState<{ x: number; y: number } | null>(null);
  const [box, setBox] = useState<Box | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") invoke("cancel_overlay");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function onDown(e: ReactMouseEvent) {
    setStart({ x: e.clientX, y: e.clientY });
    setBox({ x: e.clientX, y: e.clientY, w: 0, h: 0 });
  }
  function onMove(e: ReactMouseEvent) {
    if (!start) return;
    const x = Math.min(start.x, e.clientX);
    const y = Math.min(start.y, e.clientY);
    const w = Math.abs(e.clientX - start.x);
    const h = Math.abs(e.clientY - start.y);
    setBox({ x, y, w, h });
  }
  function onUp() {
    // 送 CSS 逻辑像素（相对该屏左上角），后端 × scale 转物理
    if (box && box.w > 3 && box.h > 3) {
      invoke("overlay_capture", { x: box.x, y: box.y, w: box.w, h: box.h });
    } else {
      invoke("cancel_overlay");
    }
    setStart(null);
  }

  return (
    <div
      className={box ? "ov" : "ov idle"}
      onMouseDown={onDown}
      onMouseMove={onMove}
      onMouseUp={onUp}
    >
      {box && (box.w > 0 || box.h > 0) && (
        <div className="sel" style={{ left: box.x, top: box.y, width: box.w, height: box.h }} />
      )}
      {!start && <div className="hint">🖱️ 拖动框选要翻译的区域　·　Esc 取消</div>}
    </div>
  );
}

createRoot(document.getElementById("overlay-root")!).render(<Overlay />);
