import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { MODIFIER_CODES, accelFromEvent, formatAccel, isValidAccel, warnFor } from "../lib/hotkey";

/**
 * 全局快捷键录制控件（截图 / 划词各一个）。
 *
 * 给 SettingsView 这样接（本文件不动 SettingsView）：
 *   <div className="group">
 *     <div className="group-title">快捷键</div>
 *     <HotkeyInput label="截图翻译" value={hk.shot} ok={hk.shotOk} onCommit={(a) => setHotkey("shot", a)} />
 *     <HotkeyInput label="划词翻译" value={hk.pick} ok={hk.pickOk} onCommit={(a) => setHotkey("pick", a)} />
 *   </div>
 * 组件根节点**就是**一行 .setrow：外面别再包 div，否则 `.setrow + .setrow` 的
 * 「相邻行去掉重复边框 + 首尾圆角」那套规则会断，两行会各自描一圈边看着很脏。
 *
 * ⚠ 录制时按「当前已经生效的那个组合」是录不到的：它已被 global-hotkey 全局占用，
 *   按下去先被系统吞走，keydown 根本到不了这里。用户把 Alt+Q 又设成 Alt+Q 会觉得
 *   「没反应」——这是预期行为，不是 bug。(2026-08-05)
 */

interface Props {
  label: string;
  value: string;
  /** 后端注册结果。false 必须在界面上看得见——不能显示一个按了没反应的组合还装作正常。 */
  ok: boolean;
  onCommit: (accel: string) => Promise<{ ok: boolean; message?: string }>;
  /** 开一次探测窗口，返回窗口毫秒数。期间按下该热键只回报、不执行动作。 */
  onProbe: () => Promise<number>;
  /** 撤掉后端的探测窗口。测到一半跑去改键/切走页面时必须调，理由见 api.ts。 */
  onProbeCancel: () => void;
  /**
   * 本动作"刚刚真的收到按键"的信号（父层从后端事件转发来的 nonce）。
   * 只在探测等待中才被采信；变化即判定为收到。
   */
  probeHit?: number;
}

type Phase = "idle" | "recording" | "applying";
/** 「测一下」的状态。idle 之外都会在按钮下方给一句人话。 */
type Probe = "idle" | "waiting" | "ok" | "timeout";

/**
 * 只按住修饰键时的实时预览，如 "Ctrl + Alt + …"；一个修饰键都没按返回空串。
 *
 * 标签和排序**全都甩给 formatAccel**：拼一个带占位主键的规范串喂进去，再把占位主键切掉。
 * 别自己排 —— hotkey.ts 的存储顺序是 shift+control+alt+super，显示顺序却是
 * Ctrl → Shift → Alt → Win（它那边故意不一致，为的是符合 Windows 习惯）。
 * 照存储顺序自己拼标签，按 Ctrl 会显示成 "Shift"，而且只在预览这一瞬间错、
 * 录完就被徽章盖掉，属于很难被发现的那种错。(2026-08-05 差点写错)
 */
function previewOf(e: { shiftKey: boolean; ctrlKey: boolean; altKey: boolean; metaKey: boolean }): string {
  const mods: string[] = []; // 存储顺序，与 accelFromEvent 一致
  if (e.shiftKey) mods.push("shift");
  if (e.ctrlKey) mods.push("control");
  if (e.altKey) mods.push("alt");
  if (e.metaKey) mods.push("super");
  if (!mods.length) return "";

  const full = formatAccel([...mods, "KeyA"].join("+")); // 如 "Ctrl + Alt + A"
  const cut = full.lastIndexOf(" + ");
  return cut < 0 ? "" : full.slice(0, cut) + " + …";
}

export function HotkeyInput({
  label,
  value,
  ok,
  onCommit,
  onProbe,
  onProbeCancel,
  probeHit,
}: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [preview, setPreview] = useState("");
  const [err, setErr] = useState<string | null>(null);
  /** 正在提交的组合。只为了让软警告跟着「即将生效的那个键」走。 */
  const [pending, setPending] = useState<string | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);

  // ---- 「测一下」----
  // 这是唯一能查出"低级键盘钩子把键吞了"的办法：那类程序（微信/QQ 截图、Snipaste、输入法、
  // AHK）不占 RegisterHotKey 的槽位，后端 register 照样成功，只有真按一次才知道到没到。
  const [probe, setProbe] = useState<Probe>("idle");
  const probeTimer = useRef<number | null>(null);
  const probeRef = useRef<Probe>("idle");
  probeRef.current = probe;

  function clearProbeTimer() {
    if (probeTimer.current !== null) {
      window.clearTimeout(probeTimer.current);
      probeTimer.current = null;
    }
  }
  /** 前端计时器 + 后端窗口一起收掉。只清前端会在后端留下一个吞键的窗口。 */
  function abortProbe() {
    clearProbeTimer();
    if (probeRef.current === "waiting") onProbeCancel();
  }
  // 组件卸载（切走设置页）时收尾：既避免在已卸载组件上 setState，也别把后端窗口留着
  const cancelRef = useRef(onProbeCancel);
  cancelRef.current = onProbeCancel;
  useEffect(
    () => () => {
      if (probeTimer.current !== null) window.clearTimeout(probeTimer.current);
      if (probeRef.current === "waiting") cancelRef.current();
    },
    [],
  );

  // 收到回报。⚠ 只有正在等的时候才采信：探测窗口过期后用户又按了一次热键，
  // 后端不会再发这个事件；但父层的 nonce 可能因为另一次探测而变，别误判成本次成功。
  useEffect(() => {
    if (probeHit === undefined) return;
    if (probeRef.current !== "waiting") return;
    clearProbeTimer();
    setProbe("ok");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [probeHit]);

  async function startProbe() {
    clearProbeTimer();
    setProbe("waiting");
    let windowMs = 8000;
    try {
      windowMs = await onProbe();
    } catch {
      setProbe("idle");
      return;
    }
    // 比后端窗口多留半秒：事件从后端到前端要过一趟 IPC，卡在边界上会误报"没收到"。
    probeTimer.current = window.setTimeout(() => {
      if (probeRef.current === "waiting") setProbe("timeout");
    }, windowMs + 500);
  }

  // 软警告不进 state：它永远由「当前正在说的那个键」现算。
  // 存 state 的话提交失败一回滚，警告说的是新键、徽章显示的是旧键，两边打架。
  const subject = pending ?? value;
  const warn = isValidAccel(subject) ? warnFor(subject) : null;
  // value 可能是旧版本配置或被手改坏的库里值，不校验就直接 formatAccel 会显示成乱码
  const badge = isValidAccel(value) ? formatAccel(value) : "未设置";

  function start() {
    setPhase("recording");
    setPreview("");
    setErr(null);
    setPending(null);
    // 一开始录就把上次的探测清掉：结论说的是**旧键**，留着会让人以为新键已经测过了；
    // 后端那个窗口更要撤，否则新键的第一次按下会被它吞成"探测命中"、界面完全没反应。
    abortProbe();
    setProbe("idle");
    btnRef.current?.focus(); // 录制全靠按钮自身的 keydown，没焦点就等于没在录
  }

  function stop() {
    setPhase("idle");
    setPreview("");
    setPending(null);
    // 退出录制态就把失败红字收掉：idle 时上面那行 desc 写的是「全局生效，任意程序里都能按」，
    // 底下再挂一条红色「没能设成这个组合」＝同一行里两句话自相矛盾，用户无从判断到底能不能用。
    // 不会吞掉刚出现的错误：commit 失败那条路径**故意不走 stop()**（它要停在录制态让用户直接再按）。
    setErr(null);
  }

  async function commit(accel: string) {
    setPhase("applying");
    setPending(accel);
    setErr(null);
    let res: { ok: boolean; message?: string };
    try {
      res = await onCommit(accel);
    } catch (e) {
      // onCommit 一般是 invoke()，后端 panic / 通道断了会 reject 而不是返回 {ok:false}
      res = { ok: false, message: String(e) };
    }
    if (res.ok) {
      stop();
      return;
    }
    // 失败就停在录制态，让用户抬手再按一个，不用重新点「修改」。
    setPending(null);
    setPreview("");
    setPhase("recording");
    setErr(res.message || "没能设成这个组合，换一个试试");
    // 补一次 focus：提交期间按钮没有真 disabled（见渲染处注释），焦点本该还在，
    // 但用户可能中途点到别处去了——不抢回焦点，「停在录制态」就是假的，按键进不来。
    btnRef.current?.focus();
  }

  function onKeyDown(e: ReactKeyboardEvent<HTMLButtonElement>) {
    // ⚠ idle 态必须先 return，**不能吞键**（2026-08-07 复核揪出的焦点陷阱）：
    // 提交成功后会 stop() 回 idle，而按钮**仍然持有焦点**。若在这里无条件 preventDefault，
    // 该按钮从此只剩鼠标可用——Tab 移不走焦点、Enter/Space 也激活不了它（start 只挂在 onClick）。
    // 这条路径在正常成功流程里必经，不是边角情况。
    if (phase === "idle") return;
    // 录制/应用期间必须拦：不拦的话 Space/Enter 被当成「点按钮」、Backspace 触发页面后退、
    // Tab 直接把焦点带走（一带走就 blur 退出录制，含 Tab 的组合永远录不到）。
    e.preventDefault();
    e.stopPropagation();
    if (phase !== "recording") return; // applying 期间按键一律吞掉
    if (e.repeat) return; // 长按会连发 keydown，只认第一下

    const bare = !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey;
    if (e.code === "Escape" && bare) {
      // 光秃秃的 Esc 才是取消；Shift+Esc 之类是合法组合，得放给 accelFromEvent
      setErr(null);
      stop();
      return;
    }

    if (MODIFIER_CODES.has(e.code)) {
      setPreview(previewOf(e));
      return;
    }

    const r = accelFromEvent(e.nativeEvent);
    if ("error" in r) {
      setErr(r.error);
      return;
    }
    void commit(r.accel);
  }

  function onKeyUp(e: ReactKeyboardEvent<HTMLButtonElement>) {
    // 修饰键全松开就清预览，否则屏幕上会一直挂着一串「Ctrl + …」，
    // 用户以为还按着，其实早松手了。
    // 松开**任何一个**修饰键都要重算，不能只在全松时清空：按住 Ctrl+Alt 后只松 Alt，
    // 屏幕上会一直挂着「Ctrl + Alt + …」。previewOf 在全松时本就返回 ""，一行覆盖两种情况。
    if (phase === "recording") setPreview(previewOf(e));
  }

  const desc =
    phase === "recording"
      ? "按下新的组合键，Esc 取消"
      : ok
        ? "全局生效，任意程序里都能按"
        : "这个组合没能注册上——多半被别的程序占着，换一个";

  const btnText =
    phase === "applying" ? "应用中…" : phase === "recording" ? preview || "按下组合键…" : "修改";

  const probeText: Record<Probe, string> = {
    idle: "",
    waiting: `现在按一下 ${badge} 试试（这几秒内按它只会回报，不会真的翻译）`,
    ok: "✓ 按到了，这个组合能用",
    timeout: "没收到——多半被别的软件抢走了（微信/QQ 截图、Snipaste、输入法之类），换一个",
  };
  const probeColor: Record<Probe, string> = {
    idle: "var(--sub)",
    waiting: "var(--accent)",
    ok: "var(--ok)",
    timeout: "var(--danger)",
  };

  return (
    <div className="setrow">
      <div>
        <div className="label">{label}</div>
        <div className="desc">{desc}</div>
        {err && (
          <div className="desc" style={{ color: "var(--danger)" }}>
            {err}
          </div>
        )}
        {warn && (
          <div className="desc" style={{ color: "var(--star)" }}>
            {warn}
          </div>
        )}
        {probe !== "idle" && (
          <div className="desc" style={{ color: probeColor[probe] }}>
            {probeText[probe]}
          </div>
        )}
        {/* 常驻兜底。低级键盘钩子（微信/QQ 截图、Snipaste、输入法、AHK）抢键时，
            后端 register 照样返回成功，我们这边检测不出来——所以绝不能让用户把
            「没报错」理解成「一定能用」。 */}
        <div className="desc" style={{ fontSize: 11, color: "var(--faint)" }}>
          没报错也可能被别的软件抢走——拿不准就点「测一下」
        </div>
      </div>

      <div style={{ display: "flex", alignItems: "center", gap: 8, flexShrink: 0 }}>
        <span
          className={ok ? "chip on" : "chip"}
          style={{
            cursor: "default", // .chip 本来是按钮样式，这儿只是个徽章，别给点得动的暗示
            opacity: phase === "idle" ? 1 : 0.45, // 录制中压暗：它是「正在被替换的那个」
            ...(ok ? {} : { color: "var(--faint)", borderColor: "var(--line)" }),
          }}
        >
          {badge}
        </span>
        {!ok && (
          <span style={{ fontSize: 11, color: "var(--danger)", whiteSpace: "nowrap" }}>未生效</span>
        )}
        {/* 录制/应用中不给测：那两个状态下按键要么被录走、要么被吞，测了也没意义 */}
        <button
          type="button"
          className="btn"
          disabled={phase !== "idle" || probe === "waiting"}
          onClick={() => void startProbe()}
          title="按一下这个键，看它到底有没有传到译点"
        >
          {probe === "waiting" ? "等你按…" : "测一下"}
        </button>
        <button
          ref={btnRef}
          type="button"
          className="btn"
          // 录制挂按钮不挂 window：挂 window 的话设置页一开就在全局偷录，
          // 用户按什么都被吃掉。顺带也避开了输入法——按钮不是输入区，IME 不会插一脚。
          onKeyDown={onKeyDown}
          onKeyUp={onKeyUp}
          onClick={() => {
            if (phase === "idle") start();
          }}
          onBlur={() => {
            // 点走了就别在背后偷偷继续录。applying 不退：提交结果还没回来，
            // 退了就没地方显示失败信息、也接不住「再按一个」。
            if (phase === "recording") stop();
          }}
          // 提交中故意**不用** disabled 属性：disabled 会立刻夺走焦点 → 触发 onBlur →
          // 退出录制态，失败时就没法停在录制态让用户直接再按了。所以只做视觉禁用，
          // 拦截逻辑放在 onClick / onKeyDown 里。
          aria-disabled={phase === "applying"}
          style={{
            minWidth: 118, // 文案在「修改 / Ctrl + Alt + … / 应用中…」之间跳，钉住宽度免得整行抖
            textAlign: "center",
            opacity: phase === "applying" ? 0.55 : 1,
            cursor: phase === "applying" ? "default" : "pointer",
            ...(phase === "recording"
              ? { boxShadow: "inset 0 0 0 1.5px var(--accent)", color: "var(--accent)", fontWeight: 600 }
              : {}),
          }}
        >
          {btnText}
        </button>
      </div>
    </div>
  );
}

// 本项目其它组件都是默认导出，两种写法都留一个，SettingsView 怎么 import 都不会踩空。
export default HotkeyInput;
