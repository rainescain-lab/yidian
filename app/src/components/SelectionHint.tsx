import { useEffect, useRef } from "react";
import type { HotkeyInfo } from "../types";
import { formatAccel, isValidAccel } from "../lib/hotkey";

/**
 * 点侧栏「划词翻译」时弹的说明卡。
 *
 * 为什么是说明而不是"点了就划词"：划词的原理是**在源程序仍是前台时**模拟 Ctrl+C 取走
 * 选中的文字（见 `src-tauri/src/selection.rs` 文件头）。点这个按钮的那一刻前台是译点自己，
 * Ctrl+C 发给译点必然取不到词 —— 做成"点了就跑"只会让用户看到一次静默失败，
 * 然后认定这个功能坏了。所以这里老实说明怎么用，并把改键入口放在手边。
 */
interface Props {
  hotkeys: HotkeyInfo[];
  onClose: () => void;
  onGoSettings: () => void;
}

export default function SelectionHint({ hotkeys, onClose, onGoSettings }: Props) {
  const hk = hotkeys.find((h) => h.action === "selection");
  const closeRef = useRef<HTMLButtonElement>(null);

  // Esc 关闭 + 打开即聚焦，让键盘用户不至于被困在卡里
  useEffect(() => {
    closeRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const badge = hk && isValidAccel(hk.accel) ? formatAccel(hk.accel) : "未设置";

  return (
    <div className="modal-mask" onClick={onClose}>
      {/* 阻止冒泡：点卡片内部不该关掉它 */}
      <div className="modal" role="dialog" aria-modal="true" onClick={(e) => e.stopPropagation()}>
        <div className="modal-title">划词翻译怎么用</div>
        <div className="modal-body">
          <p>
            在<b>别的程序</b>里（浏览器、文档、聊天窗口……）选中一段文字，然后按
            <span className="kbd">{badge}</span>，译文会直接弹在光标旁边。
          </p>
          <p className="dim">
            这里点不出效果是正常的：划词要在你选中文字的那个程序还处于最前面时才取得到词，
            而点这个按钮时最前面的是译点自己。
          </p>
          {hk && !hk.ok && (
            <p className="danger">
              当前这个组合没能注册上（{hk.error || "多半被别的程序占着"}），换一个再试。
            </p>
          )}
        </div>
        <div className="modal-actions">
          <button className="btn" onClick={onGoSettings}>
            改快捷键
          </button>
          <button ref={closeRef} className="btn primary" onClick={onClose}>
            知道了
          </button>
        </div>
      </div>
    </div>
  );
}
