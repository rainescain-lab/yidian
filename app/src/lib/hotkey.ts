/**
 * 快捷键的解析 / 格式化 / 校验 —— 全项目唯一真相源。
 * 录制控件（HotkeyInput）和设置页里的徽章都从这里取，别在别处再抄一份键表。
 *
 * 规范串（accel）格式：`shift+control+alt+super+<Code>`
 * 修饰符小写、顺序固定，主键用 W3C KeyboardEvent.code 的原样大小写（KeyQ / Digit1 / F5 / ArrowUp）。
 * 这个格式不是我们定的，是后端 Shortcut::into_string() 的产物，必须逐字节对齐：
 * 存进 DB 的串要能被后端 parse 回去，事件回调里带回来的串还要能跟设置项按字符串比中。
 *
 * 纯函数、零依赖，能被 Node 直接 import（同目录 hotkey.test.ts 就是这么跑的），别在这里引 React。
 */

const range = (n: number, f: (i: number) => string) => Array.from({ length: n }, (_, i) => f(i));

/** 修饰键自身的 e.code。按住它们不算"按下了主键"，录制时要当成"还没录完"。 */
export const MODIFIER_CODES = new Set<string>([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
]);

/**
 * 允许作主键的 e.code 全集。
 *
 * 这张表不是照着 W3C 的 code 清单抄的，是 2026-08-05 逐行读
 * `global-hotkey-0.8.0/src/hotkey.rs` 里的 `parse_key()` 对出来的
 * （tauri-plugin-global-shortcut 2.3.2 的 `Shortcut` 就是 `global_hotkey::hotkey::HotKey`
 * 的别名，解析走的是同一个函数，没有第二套表）。
 *
 * 白名单必须 ⊆ parse_key 的覆盖面：漏了只是少几个键能用；多了就是用户录得进、
 * 后端 parse 直接抛 UnsupportedKey，UI 上表现为莫名其妙一直显示"未生效"，极难排查。
 *
 * 核对时撞到两处与直觉不符的，别想当然：
 *  1. Numpad **不是**整族收录。只有 Numpad0-9 和 Add/Decimal/Divide/Enter/Equal/Multiply/Subtract；
 *     NumpadComma / NumpadParenLeft / NumpadStar / NumpadHash 这些 parse_key 里压根没有分支。
 *  2. 键盘上真实存在、浏览器也会老老实实给 code 的一票键，parse_key 不认：
 *     ContextMenu（右键菜单键）、IntlBackslash / IntlRo / IntlYen（欧/日键盘多出来的那颗）、
 *     Lang1 / Lang2 / Convert / NonConvert（日韩输入法键）、以及 BrowserBack / LaunchMail /
 *     Power / Sleep 这类附加功能键。必须挡在录制这一层，不能指望后端报错。
 *
 * 反过来 MediaPlay / MediaPause 是 parse_key 认、但浏览器永远不会作为 e.code 发出来的
 * （W3C 只有 MediaPlayPause）。留着是为了让这张表严格等于"后端能 parse 的集合"，
 * 这样 isValidAccel 校验手改过的 DB 串时不会误判；录制流程本来也走不到它们。
 */
export const MAIN_KEY_WHITELIST = new Set<string>([
  ...range(26, (i) => `Key${String.fromCharCode(65 + i)}`), // KeyA-KeyZ
  ...range(10, (i) => `Digit${i}`), // Digit0-Digit9
  ...range(24, (i) => `F${i + 1}`), // F1-F24
  ...range(10, (i) => `Numpad${i}`), // Numpad0-Numpad9
  // 符号
  "Backquote",
  "Backslash",
  "BracketLeft",
  "BracketRight",
  "Comma",
  "Equal",
  "Minus",
  "Period",
  "Quote",
  "Semicolon",
  "Slash",
  // 编辑 / 导航
  "Backspace",
  "CapsLock",
  "Enter",
  "Space",
  "Tab",
  "Delete",
  "End",
  "Home",
  "Insert",
  "PageDown",
  "PageUp",
  "PrintScreen",
  "ScrollLock",
  "Pause",
  "Escape",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  // 小键盘（只有这 7 个运算键，见上面第 1 条）
  "NumLock",
  "NumpadAdd",
  "NumpadDecimal",
  "NumpadDivide",
  "NumpadEnter",
  "NumpadEqual",
  "NumpadMultiply",
  "NumpadSubtract",
  // 媒体键
  "AudioVolumeDown",
  "AudioVolumeUp",
  "AudioVolumeMute",
  "MediaPlay",
  "MediaPause",
  "MediaPlayPause",
  "MediaStop",
  "MediaTrackNext",
  "MediaTrackPrevious",
]);

/**
 * accelFromEvent 在"只按住了修饰键、还没落主键"时返回的固定错误串。
 * 调用方可以按引用比较它来区分"录制进行中"（继续显示预览、别报红）和真错误。
 * 契约里 accelFromEvent 只返回 { accel } | { error }，塞不进第三种状态，只能靠这个常量。
 */
export const ERR_MODIFIERS_ONLY = "继续按下主键…";

/** 存储顺序，与后端 into_string() 的拼接顺序一致。isValidAccel 靠它判顺序。 */
const MOD_ORDER = ["shift", "control", "alt", "super"] as const;

/**
 * 从 keydown 事件算出规范串。
 *
 * 修饰位一律从 e.altKey/ctrlKey/shiftKey/metaKey 取，**不从 e.code 推**：
 * AltLeft 和 AltRight 是两个不同的 code，用 code 表达不了"任意一个 Alt"。
 *
 * 注意本函数是纯函数，不会 preventDefault —— 吞掉浏览器默认行为（Ctrl+W 之类）是调用方的事。
 *
 * ⚠ 欧洲布局的 AltGr 在浏览器里会同时置起 ctrlKey 和 altKey，所以 AltGr+E 录出来是
 * `control+alt+KeyE`。这是浏览器的既成事实，没法区分，本机是 US 布局不受影响。
 */
export function accelFromEvent(e: KeyboardEvent): { accel: string } | { error: string } {
  const code = e.code;

  // 先判"只有修饰键"，否则按住 Alt 时会走到下面报"这个键不支持"，那太吓人了
  if (MODIFIER_CODES.has(code)) return { error: ERR_MODIFIERS_ONLY };

  const mods: string[] = [];
  if (e.shiftKey) mods.push("shift");
  if (e.ctrlKey) mods.push("control");
  if (e.altKey) mods.push("alt");
  if (e.metaKey) mods.push("super");

  // 硬禁无修饰单键：后端 parse_key 认单键（"KeyQ" 能 parse 成功）并且真的会注册成全局热键，
  // 那样用户之后每敲一次这个字母都会触发翻译、正常打字全废。必须挡在这里，后端拦不住。
  if (mods.length === 0) return { error: "至少要带一个 Ctrl / Alt / Shift / Win" };

  if (!MAIN_KEY_WHITELIST.has(code)) return { error: "这个键不支持" };

  return { accel: [...mods, code].join("+") };
}

/** 主键的显示写法。没列到的（PageUp、AudioVolumeUp 之类）原样显示，够读了。 */
const MAIN_KEY_LABEL: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Escape: "Esc",
  Backquote: "`",
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
  Comma: ",",
  Equal: "=",
  Minus: "-",
  Period: ".",
  Quote: "'",
  Semicolon: ";",
  Slash: "/",
};

/** 小键盘运算键的显示写法，拼在"小键盘 "后面。 */
const NUMPAD_LABEL: Record<string, string> = {
  Add: "+",
  Subtract: "-",
  Multiply: "*",
  Divide: "/",
  Decimal: ".",
  Equal: "=",
  Enter: "Enter",
};

function formatMainKey(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3); // KeyQ -> Q
  if (/^Digit[0-9]$/.test(code)) return code.slice(5); // Digit1 -> 1
  // NumLock 也是 Num 开头但不是小键盘键，所以这里必须匹配整个 "Numpad" 前缀
  if (code.startsWith("Numpad")) {
    const tail = code.slice(6);
    return `小键盘 ${NUMPAD_LABEL[tail] ?? tail}`;
  }
  return MAIN_KEY_LABEL[code] ?? code;
}

/**
 * 规范串 -> 人读的写法，如 "alt+KeyQ" -> "Alt + Q"。
 *
 * ⚠ 显示顺序是 Ctrl → Shift → Alt → Win，**故意跟存储顺序（shift+control+alt+super）不一样**。
 * 存储那套顺序是后端 into_string() 定死的，改不得；显示这套是 Windows 的习惯写法
 * （系统自己、Office、VSCode 都写 Ctrl+Shift+X，没人写 Shift+Ctrl+X）。
 * 看着像写反了，不是 bug，别"顺手改回来"。
 *
 * 拆分直接按 "+" split 就够：主键里不存在字面的 "+"（小键盘加号叫 NumpadAdd 而不是 "+"），
 * 所以最后一段一定是主键，不会有歧义。
 */
export function formatAccel(accel: string): string {
  if (!accel) return "";
  const parts = accel.split("+");
  const main = parts[parts.length - 1];
  const mods = new Set(parts.slice(0, -1));

  const out: string[] = [];
  if (mods.has("control")) out.push("Ctrl");
  if (mods.has("shift")) out.push("Shift");
  if (mods.has("alt")) out.push("Alt");
  if (mods.has("super")) out.push("Win");
  out.push(formatMainKey(main));
  return out.join(" + ");
}

/**
 * 校验一个规范串是否合法：修饰符必须是 MOD_ORDER 的子序列（顺序固定、不重复），
 * 主键必须在白名单里，且至少要有一个修饰符。
 *
 * 最后这条比后端严：后端 parse 是接受 "KeyQ" 的。这里跟 accelFromEvent 保持同一条红线，
 * 免得 DB 里被手改出一个无修饰单键、界面上还显示得好好的（见 accelFromEvent 里的说明）。
 */
export function isValidAccel(accel: string): boolean {
  const parts = accel.split("+");
  if (parts.length < 2) return false;
  if (!MAIN_KEY_WHITELIST.has(parts[parts.length - 1])) return false;

  // 在 MOD_ORDER 里只许向后推进：顺序错（alt+control）和重复（alt+alt）都会撞到 -1
  let from = 0;
  for (const m of parts.slice(0, -1)) {
    const at = MOD_ORDER.indexOf(m as (typeof MOD_ORDER)[number], from);
    if (at < 0) return false;
    from = at + 1;
  }
  return true;
}

/**
 * 软警告：这些组合允许录，但大概率被系统抢走，提前告诉用户比让他对着"未生效"发呆强。
 * 返回 null 表示没话说。
 *
 * 只管"注册得上但会被系统截胡"这一类。真正注册失败（Win+L、Ctrl+Esc 这些外壳保留组合）
 * 由后端返回的 ok=false 兜底，不在这里穷举。
 */
export function warnFor(accel: string): string | null {
  const parts = accel.split("+");
  const main = parts[parts.length - 1];
  const mods = parts.slice(0, -1);

  const w: string[] = [];
  if (mods.includes("super")) w.push("Win 键组合被系统保留，可能抢不到");
  if (main === "F12") w.push("F12 始终保留给调试器，多半注册不上");
  if (main === "PrintScreen") w.push("Win11 默认用 PrintScreen 拉起截图工具，会冲突");
  return w.length ? w.join("；") : null;
}
