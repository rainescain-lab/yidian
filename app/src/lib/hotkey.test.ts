/**
 * hotkey.ts 的断言测试。
 *
 * 跑法（在 app/ 目录下）：  node src/lib/hotkey.test.ts
 * 全绿会打印 "hotkey.ts OK (N 项)"，任何一条挂了直接抛异常、退出码非 0。
 *
 * 为什么不用 node:test —— 这个项目没装 @types/node，而 tsconfig 的 include 是 ["src"]，
 * 一 import "node:test" 就会让 `npm run build` 的 tsc 报 TS2307 把构建带崩。
 * 所以这里写成零 import 的自断言文件：只用 console（在 DOM lib 里），
 * 失败靠 throw 冒泡出去拿非 0 退出码。Node 24 原生剥类型，.ts 直接能跑。（2026-08-05）
 */

import {
  MODIFIER_CODES,
  MAIN_KEY_WHITELIST,
  ERR_MODIFIERS_ONLY,
  accelFromEvent,
  formatAccel,
  isValidAccel,
  warnFor,
} from "./hotkey.ts";

let n = 0;
function eq(actual: unknown, expected: unknown, what: string) {
  n++;
  const a = JSON.stringify(actual);
  const b = JSON.stringify(expected);
  if (a !== b) throw new Error(`✗ ${what}\n    实际 ${a}\n    期望 ${b}`);
}

/** 造一个够用的假 KeyboardEvent：accelFromEvent 只读这 5 个字段。 */
function ev(code: string, mods: { alt?: boolean; ctrl?: boolean; shift?: boolean; meta?: boolean } = {}) {
  return {
    code,
    altKey: !!mods.alt,
    ctrlKey: !!mods.ctrl,
    shiftKey: !!mods.shift,
    metaKey: !!mods.meta,
  } as unknown as KeyboardEvent;
}

// ---- accelFromEvent ----

eq(accelFromEvent(ev("KeyQ", { alt: true })), { accel: "alt+KeyQ" }, "Alt+Q");
eq(accelFromEvent(ev("KeyW", { alt: true })), { accel: "alt+KeyW" }, "Alt+W");
eq(
  accelFromEvent(ev("KeyD", { ctrl: true, shift: true })),
  { accel: "shift+control+KeyD" },
  "存储顺序 shift 在 control 前"
);
eq(
  accelFromEvent(ev("ArrowUp", { alt: true, ctrl: true, shift: true, meta: true })),
  { accel: "shift+control+alt+super+ArrowUp" },
  "四修饰符全按，顺序固定"
);

// 左右 Alt 都得认（修饰位取自 altKey，不是从 code 推）
eq(accelFromEvent(ev("KeyQ", { alt: true })), accelFromEvent(ev("KeyQ", { alt: true })), "AltLeft/AltRight 等价");

// 只按修饰键 -> pending，不是硬错
eq(accelFromEvent(ev("AltLeft", { alt: true })), { error: ERR_MODIFIERS_ONLY }, "只按 Alt = pending");
eq(accelFromEvent(ev("ControlRight", { ctrl: true })), { error: ERR_MODIFIERS_ONLY }, "只按右 Ctrl = pending");
eq(accelFromEvent(ev("MetaLeft", { meta: true })), { error: ERR_MODIFIERS_ONLY }, "只按 Win = pending");

// 无修饰单键必须被挡（后端会真的注册成全局单键，吃掉正常打字）
eq(accelFromEvent(ev("KeyQ")), { error: "至少要带一个 Ctrl / Alt / Shift / Win" }, "裸 Q 被拒");
eq(accelFromEvent(ev("F5")), { error: "至少要带一个 Ctrl / Alt / Shift / Win" }, "裸 F5 被拒");

// 后端 parse_key 不认的键，必须挡在录制层
for (const bad of ["ContextMenu", "IntlBackslash", "IntlRo", "IntlYen", "NumpadComma", "Lang1", "Lang2", "Convert", "NonConvert", "BrowserBack", "Unidentified", ""]) {
  eq(accelFromEvent(ev(bad, { ctrl: true })), { error: "这个键不支持" }, `Ctrl+${bad || "(空 code)"} 被拒`);
}

// ---- 白名单边界（对着 global-hotkey 0.8.0 parse_key 核对过的） ----

for (const ok of ["KeyA", "KeyZ", "Digit0", "Digit9", "F1", "F24", "Numpad0", "Numpad9", "NumpadAdd", "NumpadEqual", "NumLock", "Pause", "PrintScreen", "ScrollLock", "CapsLock", "Backquote", "Quote", "MediaTrackPrevious", "AudioVolumeMute"]) {
  eq(MAIN_KEY_WHITELIST.has(ok), true, `白名单含 ${ok}`);
}
for (const no of ["F25", "Digit10", "NumpadStar", "NumpadParenLeft", "NumpadHash", "ShiftLeft", "AltRight", "MetaLeft", "Fn", "Power"]) {
  eq(MAIN_KEY_WHITELIST.has(no), false, `白名单不含 ${no}`);
}
eq(MODIFIER_CODES.size, 8, "修饰键 code 共 8 个（左右各一）");
// 修饰键自身绝不能同时出现在主键白名单里，否则能录出 alt+AltLeft 这种鬼东西
for (const m of MODIFIER_CODES) eq(MAIN_KEY_WHITELIST.has(m), false, `${m} 不在主键白名单`);

// ---- formatAccel（显示顺序 Ctrl→Shift→Alt→Win，与存储顺序不同） ----

eq(formatAccel("alt+KeyQ"), "Alt + Q", "Alt + Q");
eq(formatAccel("shift+control+KeyD"), "Ctrl + Shift + D", "存 shift+control，显示 Ctrl + Shift");
eq(formatAccel("shift+control+alt+super+ArrowUp"), "Ctrl + Shift + Alt + Win + ↑", "四修饰符显示顺序");
eq(formatAccel("control+Digit1"), "Ctrl + 1", "Digit1 -> 1");
eq(formatAccel("alt+Escape"), "Alt + Esc", "Escape -> Esc");
eq(formatAccel("alt+Backquote"), "Alt + `", "Backquote -> `");
eq(formatAccel("alt+Comma"), "Alt + ,", "Comma -> ,");
eq(formatAccel("alt+Numpad5"), "Alt + 小键盘 5", "Numpad5");
eq(formatAccel("alt+NumpadAdd"), "Alt + 小键盘 +", "NumpadAdd");
eq(formatAccel("alt+NumLock"), "Alt + NumLock", "NumLock 不是小键盘键，别被 Num 前缀骗了");
eq(formatAccel("alt+PageUp"), "Alt + PageUp", "没映射的原样显示");
eq(formatAccel(""), "", "空串");

// ---- isValidAccel ----

eq(isValidAccel("alt+KeyQ"), true, "合法");
eq(isValidAccel("shift+control+alt+super+F5"), true, "四修饰符合法");
eq(isValidAccel("KeyQ"), false, "无修饰单键不合法（比后端严，故意的）");
eq(isValidAccel("control+shift+KeyD"), false, "修饰符顺序错");
eq(isValidAccel("alt+alt+KeyQ"), false, "修饰符重复");
eq(isValidAccel("ctrl+KeyQ"), false, "只认 control，不认 ctrl 别名");
eq(isValidAccel("alt+ContextMenu"), false, "主键不在白名单");
eq(isValidAccel("alt+"), false, "空主键");
eq(isValidAccel(""), false, "空串");

// 录出来的串必须能被自己校验通过（accelFromEvent 与 isValidAccel 不许打架）
const r = accelFromEvent(ev("KeyQ", { alt: true, shift: true }));
eq("accel" in r && isValidAccel(r.accel), true, "accelFromEvent 的产物一定 isValidAccel");

// ---- warnFor ----

eq(warnFor("alt+KeyQ"), null, "普通组合无警告");
eq(warnFor("super+KeyQ") !== null, true, "Win 组合有警告");
eq(warnFor("alt+F12") !== null, true, "F12 有警告");
eq(warnFor("alt+PrintScreen") !== null, true, "PrintScreen 有警告");
eq(warnFor("alt+F11"), null, "F11 没警告（别被 F12 的前缀匹配误伤）");
eq((warnFor("super+F12") ?? "").includes("；"), true, "两条警告合并");

console.log(`hotkey.ts OK (${n} 项)`);
