import type { HotkeyInfo, LangOption, ThemeMode } from "../types";
import { hotkeyProbe, hotkeyProbeCancel } from "../api";
import HotkeyInput from "../components/HotkeyInput";

interface Props {
  settings: Record<string, string>;
  onChange: (key: string, value: string) => void;
  onNavigate: (view: string) => void;
  langs: LangOption[];
  hotkeys: HotkeyInfo[];
  /** 真相源是后端返回的快照，所以改键要走这条而不是 onChange。 */
  onCommitHotkey: (action: string, accel: string) => Promise<{ ok: boolean; message?: string }>;
  probeHit: { action: string; nonce: number } | null;
  version: string;
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

function LangSelect({
  value,
  langs,
  onPick,
}: {
  value: string;
  langs: LangOption[];
  onPick: (v: string) => void;
}) {
  return (
    <select className="langsel wide" value={value} onChange={(e) => onPick(e.target.value)}>
      {/* langs 还没拉回来时至少保住当前值，否则 select 会显示成空白 */}
      {langs.length === 0 && <option value={value}>{value}</option>}
      {langs.map((l) => (
        <option key={l.name} value={l.name}>
          {l.label}
        </option>
      ))}
    </select>
  );
}

export default function SettingsView({
  settings,
  onChange,
  onNavigate,
  langs,
  hotkeys,
  onCommitHotkey,
  probeHit,
  version,
}: Props) {
  const theme = (settings.theme as ThemeMode) || "system";
  const engine = settings.default_engine || "online";
  const order = settings.online_order || "bing,google";
  const ocr = settings.ocr_engine || "fast";
  const nativeLang = settings.native_lang || "Chinese";
  const nativeTo = settings.native_to || "English";
  const followManual = settings.selection_follow_manual === "1";

  const shot = hotkeys.find((h) => h.action === "shot");
  const selection = hotkeys.find((h) => h.action === "selection");
  const hitFor = (a: string) => (probeHit && probeHit.action === a ? probeHit.nonce : undefined);

  const nativeLabel = langs.find((l) => l.name === nativeLang)?.label ?? nativeLang;
  const nativeToLabel = langs.find((l) => l.name === nativeTo)?.label ?? nativeTo;
  // 母语只能从"译点真能判出来的语言"里选（后端 native_ok）。法语/德语/西语这些在字符层面
  // 与英语一样，选成母语会让母语原文被判成外语、"译回母语"＝原地不动且看不出错。
  const nativeLangs = langs.filter((l) => l.native_ok);

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
          <div className="group-title">翻译方向</div>
          <div className="setrow">
            <div>
              <div className="label">我的母语</div>
              <div className="desc">
                看不懂的东西一律译成它。当前规则：{nativeLabel} → {nativeToLabel}，其他任何语言 →{" "}
                {nativeLabel}
              </div>
              <div className="desc" style={{ fontSize: 11, color: "var(--faint)" }}>
                只列出译点能认出原文的语言。法语/德语/西语等在字符层面与英语无异，认不出来，
                但可以选作下面的「母语译成」
              </div>
            </div>
            <LangSelect
              value={nativeLang}
              langs={nativeLangs}
              onPick={(v) => onChange("native_lang", v)}
            />
          </div>
          <div className="setrow">
            <div>
              <div className="label">母语译成</div>
              <div className="desc">
                输入的是母语时译成哪种语言。学日语就把它改成日语，中文会直接译成日文
              </div>
            </div>
            <LangSelect value={nativeTo} langs={langs} onPick={(v) => onChange("native_to", v)} />
          </div>
          <div className="setrow">
            <div>
              <div className="label">划词 / 截图跟随主界面手选</div>
              <div className="desc">
                关（推荐）：划词截图永远按上面这条规则走。开：跟着主界面顶上那两个下拉框 ——
                手选是一次性的意图，忘了改回来会让划词一直译到奇怪的语言去
              </div>
            </div>
            <label className="switch">
              <input
                type="checkbox"
                checked={followManual}
                onChange={(e) => onChange("selection_follow_manual", e.target.checked ? "1" : "0")}
              />
              <span className="slider" />
            </label>
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
          {/* ⚠ HotkeyInput 的根节点**就是**一行 .setrow，外面不要再包 div，
              否则 `.setrow + .setrow` 那套「相邻行去边框 + 首尾圆角」的规则会断。 */}
          <HotkeyInput
            label="截图翻译"
            value={shot?.accel ?? settings.hotkey_shot}
            ok={shot?.ok ?? false}
            onCommit={(a) => onCommitHotkey("shot", a)}
            onProbe={() => hotkeyProbe("shot")}
            onProbeCancel={() => void hotkeyProbeCancel("shot")}
            probeHit={hitFor("shot")}
          />
          <HotkeyInput
            label="划词翻译"
            value={selection?.accel ?? settings.hotkey_selection}
            ok={selection?.ok ?? false}
            onCommit={(a) => onCommitHotkey("selection", a)}
            onProbe={() => hotkeyProbe("selection")}
            onProbeCancel={() => void hotkeyProbeCancel("selection")}
            probeHit={hitFor("selection")}
          />
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
            <span style={{ color: "var(--faint)", fontSize: 12 }}>
              {version ? `v${version}` : ""}
            </span>
          </div>
        </div>
      </div>
    </>
  );
}
