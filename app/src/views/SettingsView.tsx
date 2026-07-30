import type { ThemeMode } from "../types";

interface Props {
  settings: Record<string, string>;
  onChange: (key: string, value: string) => void;
  onNavigate: (view: string) => void;
}

function Seg<T extends string>({
  value,
  options,
  onPick,
}: {
  value: string;
  options: { v: T; label: string }[];
  onPick: (v: T) => void;
}) {
  return (
    <div className="segbig">
      {options.map((o) => (
        <button key={o.v} className={value === o.v ? "on" : ""} onClick={() => onPick(o.v)}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

export default function SettingsView({ settings, onChange, onNavigate }: Props) {
  const theme = (settings.theme as ThemeMode) || "system";
  const engine = settings.default_engine || "local";
  const order = settings.online_order || "bing,google";
  const ocr = settings.ocr_engine || "fast";

  return (
    <>
      <div className="view-head">
        <div className="view-title">设置</div>
      </div>

      <div className="settings">
        <div className="group">
          <div className="group-title">外观</div>
          <div className="setrow">
            <div>
              <div className="label">主题</div>
              <div className="desc">深色 / 浅色，或跟随系统</div>
            </div>
            <Seg
              value={theme}
              onPick={(v) => onChange("theme", v)}
              options={[
                { v: "light", label: "浅色" },
                { v: "dark", label: "深色" },
                { v: "system", label: "跟随系统" },
              ]}
            />
          </div>
        </div>

        <div className="group">
          <div className="group-title">翻译引擎</div>
          <div className="setrow">
            <div>
              <div className="label">默认引擎</div>
              <div className="desc">本地 Qwen 离线私密；在线更准，联网走微软→谷歌</div>
            </div>
            <Seg
              value={engine}
              onPick={(v) => onChange("default_engine", v)}
              options={[
                { v: "local", label: "本地·Qwen" },
                { v: "online", label: "在线" },
              ]}
            />
          </div>
          <div className="setrow">
            <div>
              <div className="label">在线兜底次序</div>
              <div className="desc">主引擎失败时自动切换到下一个</div>
            </div>
            <Seg
              value={order}
              onPick={(v) => onChange("online_order", v)}
              options={[
                { v: "bing,google", label: "微软→谷歌" },
                { v: "google,bing", label: "谷歌→微软" },
              ]}
            />
          </div>
        </div>

        <div className="group">
          <div className="group-title">截图 · 划词翻译</div>
          <div className="setrow">
            <div>
              <div className="label">认字方式</div>
              <div className="desc">快 = Windows 自带（秒出）；准 = 本地 AI 模型（慢几秒，小字/花字更强）</div>
            </div>
            <Seg
              value={ocr}
              onPick={(v) => onChange("ocr_engine", v)}
              options={[
                { v: "fast", label: "快" },
                { v: "accurate", label: "准" },
              ]}
            />
          </div>
          <div className="setrow">
            <div>
              <div className="label">快捷键</div>
              <div className="desc">Alt + Q 截图翻译 · Alt + W 划词翻译（全局，任意程序里可用）</div>
            </div>
            <span style={{ color: "var(--faint)", fontSize: 12 }}>全局热键</span>
          </div>
        </div>

        <div className="group">
          <div className="group-title">词典</div>
          <div className="setrow" onClick={() => onNavigate("dict")} style={{ cursor: "pointer" }}>
            <div>
              <div className="label">管理词典</div>
              <div className="desc">导入 .mdx 词典 · 调整查词优先级</div>
            </div>
            <span style={{ color: "var(--faint)" }}>›</span>
          </div>
        </div>

        <div className="group">
          <div className="group-title">关于</div>
          <div className="setrow">
            <div>
              <div className="label">译点 YiDian</div>
              <div className="desc">桌面翻译器 · 本地优先 · 数据只存本机</div>
            </div>
            <span style={{ color: "var(--faint)", fontSize: 12 }}>v0.2</span>
          </div>
        </div>
      </div>
    </>
  );
}
