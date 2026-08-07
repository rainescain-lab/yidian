import type { ReactElement } from "react";
import type { HotkeyInfo } from "../types";
import { triggerShot } from "../api";
import { formatAccel, isValidAccel } from "../lib/hotkey";

const ICONS: Record<string, ReactElement> = {
  translate: (
    <path d="M4 5h7M7.5 5v1.5c0 3-2 5.5-4.5 6.5M5 9.5c0 2 2.2 3.8 5.5 5M13 19l3.5-8 3.5 8M14.3 16.3h4.4" />
  ),
  history: (
    <>
      <circle cx="12" cy="12" r="8" />
      <path d="M12 8v4l3 2" />
    </>
  ),
  dict: (
    <path d="M12 6.5C10.5 5.4 8 5 6 5v12c2 0 4.5.4 6 1.5M12 6.5C13.5 5.4 16 5 18 5v12c-2 0-4.5.4-6 1.5" />
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 3.5v2.2M12 18.3v2.2M4.7 7.5l1.9 1.1M17.4 15.4l1.9 1.1M4.7 16.5l1.9-1.1M17.4 8.6l1.9-1.1" />
    </>
  ),
  shot: (
    <>
      <path d="M4 8.5V6.5A1.5 1.5 0 0 1 5.5 5h2M16.5 5h2A1.5 1.5 0 0 1 20 6.5v2M20 15.5v2a1.5 1.5 0 0 1-1.5 1.5h-2M7.5 19h-2A1.5 1.5 0 0 1 4 17.5v-2" />
      <path d="M8.5 12h7" />
    </>
  ),
  selection: (
    <>
      <path d="M5 5h9l5 5v9H5z" />
      <path d="M8 13h6M8 16h4" />
    </>
  ),
};

function Ico({ name }: { name: string }) {
  return (
    <svg
      className="ico"
      viewBox="0 0 24 24"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {ICONS[name]}
    </svg>
  );
}

const NAV = [
  { key: "translate", label: "翻译" },
  { key: "history", label: "我的翻译" },
  { key: "dict", label: "词典" },
  { key: "settings", label: "设置" },
];

interface Props {
  active: string;
  onNavigate: (view: string) => void;
  hotkeys: HotkeyInfo[];
  /** 点「划词翻译」时弹说明卡（它按不出效果，理由见下方注释）。 */
  onSelectionHint: () => void;
}

/** 侧栏底部那两行的快捷键徽章：生效就显示组合，没生效就明说。 */
function KeyTag({ hk }: { hk?: HotkeyInfo }) {
  if (!hk) return null;
  if (!hk.ok) {
    return (
      <span className="tag warn" title={hk.error}>
        未生效
      </span>
    );
  }
  return <span className="tag">{isValidAccel(hk.accel) ? formatAccel(hk.accel) : "未设置"}</span>;
}

export default function Sidebar({ active, onNavigate, hotkeys, onSelectionHint }: Props) {
  const shot = hotkeys.find((h) => h.action === "shot");
  const selection = hotkeys.find((h) => h.action === "selection");

  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="logo">译</span>译点
      </div>
      {NAV.map((n) => (
        <button
          key={n.key}
          className={"nav" + (active === n.key ? " active" : "")}
          onClick={() => onNavigate(n.key)}
        >
          <Ico name={n.key} />
          {n.label}
        </button>
      ))}
      <div className="spacer" />

      {/* 截图翻译：真执行。后端会先把主窗最小化让开，否则截到的就是译点自己。 */}
      <button className="nav" onClick={() => void triggerShot()} title="框选屏幕上任意区域直接翻译">
        <Ico name="shot" />
        截图翻译
        <KeyTag hk={shot} />
      </button>

      {/* 划词翻译：**故意不真执行**。
          划词的原理是"在源程序还是前台时模拟 Ctrl+C 取走选中文字"（见 selection.rs 文件头）。
          点这个按钮的那一刻前台是译点自己，Ctrl+C 发给自己必然取不到词 —— 做成"点了就跑"
          只会让用户以为功能坏了。所以这里弹一张卡说明怎么用、并提供改键入口。 */}
      <button className="nav" onClick={onSelectionHint} title="划词翻译怎么用">
        <Ico name="selection" />
        划词翻译
        <KeyTag hk={selection} />
      </button>
    </aside>
  );
}
