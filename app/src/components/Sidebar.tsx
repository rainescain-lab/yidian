import type { ReactElement } from "react";

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
}

export default function Sidebar({ active, onNavigate }: Props) {
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
      <div className="nav v2">
        截图翻译<span className="tag">v2</span>
      </div>
      <div className="nav v2">
        划词<span className="tag">v2</span>
      </div>
    </aside>
  );
}
