import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { Engine, LangOption, ManualDir, TranslateResult, DictResult } from "../types";
import { translate, dictLookup, historyToggleFavorite } from "../api";
import { langLabel, looksLikeWord } from "../lib/format";
import WordCard from "../components/WordCard";

interface Props {
  defaultEngine: Engine;
  prefill: { text: string; nonce: number } | null;
  /** 可选语言（后端语言表）。拉回来之前是空数组，下拉框此时只有"自动"。 */
  langs: LangOption[];
  /** 当前手选方向（null = 该侧自动）。 */
  dir: ManualDir;
  /** 方向已落到后端的信号：变化即重译。 */
  dirNonce: number;
  onChangeDir: (next: ManualDir) => void;
  onTranslated?: () => void; // 通知历史页刷新
}

export default function TranslateView({
  defaultEngine,
  prefill,
  langs,
  dir,
  dirNonce,
  onChangeDir,
  onTranslated,
}: Props) {
  const [input, setInput] = useState("");
  const [result, setResult] = useState<TranslateResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [engine, setEngine] = useState<Engine>(defaultEngine);
  const [fav, setFav] = useState(false);
  const [copied, setCopied] = useState(false);
  const [cards, setCards] = useState<DictResult[]>([]);

  const reqId = useRef(0);
  const engineRef = useRef(engine);
  engineRef.current = engine;
  const inputRef = useRef(input);
  inputRef.current = input;
  /** 最近一次真正发出去翻译的原文。用来消掉"同一段文字被翻两遍"。 */
  const lastRun = useRef("");
  /** 待发的防抖定时器。任何显式翻译都要把它撤掉。 */
  const debounce = useRef<number | null>(null);

  function clearDebounce() {
    if (debounce.current !== null) {
      window.clearTimeout(debounce.current);
      debounce.current = null;
    }
  }
  useEffect(() => clearDebounce, []);

  async function runTranslate(text: string, eng: Engine) {
    // 显式翻译（回车 / 切引擎 / 换方向 / 历史回填）先撤掉待发的防抖，
    // 否则它会在 800ms 后对同一段文字再翻一遍 —— 在线多打一次接口，本地要多等一代生成。
    clearDebounce();
    lastRun.current = text;
    const id = ++reqId.current; // 取消在途：只认最后一次
    setCopied(false);
    if (!text.trim()) {
      setResult(null);
      setError("");
      setCards([]);
      setLoading(false);
      return;
    }
    setLoading(true);
    setError("");

    // 词典卡（单词时并行查，不阻塞翻译）
    if (looksLikeWord(text)) {
      dictLookup(text.trim())
        .then((rs) => {
          if (id === reqId.current) setCards(rs);
        })
        .catch(() => {
          if (id === reqId.current) setCards([]);
        });
    } else {
      setCards([]);
    }

    try {
      const res = await translate(text, eng);
      if (id !== reqId.current) return;
      setResult(res);
      setFav(res.favorite);
      onTranslated?.();
    } catch (e) {
      if (id === reqId.current) setError(String(e));
    } finally {
      if (id === reqId.current) setLoading(false);
    }
  }

  // 停顿自动翻：本地大模型防抖更长（1.2s），在线短（0.8s）
  useEffect(() => {
    // 这段文字刚翻过（程序回填 / 回车已经翻过一次）就别再翻一遍。
    // 不加这道判断的话，从截图结果窗点「编辑」或从「我的翻译」挑一条回填，必然翻两遍：
    // 回填那一次立刻翻，setInput 又让本 effect 重跑、800ms 后再翻同样的文字。
    if (input === lastRun.current) return;
    clearDebounce();
    const delay = engineRef.current === "local" ? 1200 : 800;
    debounce.current = window.setTimeout(
      () => runTranslate(input, engineRef.current),
      delay,
    );
    return clearDebounce;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [input]);

  // 历史回填：nonce 变化即载入并立即翻
  useEffect(() => {
    if (prefill) {
      setInput(prefill.text);
      runTranslate(prefill.text, engineRef.current);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prefill?.nonce]);

  // 方向改完（且已落到后端）重译。dirNonce 初值 0 不触发，避免开屏空翻一次。
  //
  // ⚠ 必须防抖：`<select>` 一旦有焦点，方向键**按一下就 fire 一次 change**（实测），
  // 长按走键盘重复约 25 次/秒，从头翻到尾就是十几发真实翻译请求 —— 在线连打微软/谷歌容易
  // 触风控，本地 Ollama 默认串行、排队的请求各自撞 30s 超时。更隐蔽的是在**设置页**改母语时
  // 本组件挂在 display:none 里，这些请求连转圈都看不见。清理函数会撤掉未发出的那一发，
  // 所以连按只在停手 400ms 后翻一次。
  useEffect(() => {
    if (dirNonce === 0) return;
    clearDebounce();
    debounce.current = window.setTimeout(
      () => runTranslate(inputRef.current, engineRef.current),
      400,
    );
    return clearDebounce;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirNonce]);

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      runTranslate(input, engine); // 回车立刻翻
    }
  }

  function switchEngine(e: Engine) {
    if (e === engine) return;
    setEngine(e);
    runTranslate(input, e); // 切引擎即时重译
  }

  async function copy() {
    if (!result?.text) return;
    await navigator.clipboard.writeText(result.text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  async function toggleFav() {
    if (!result?.history_id) return;
    try {
      const now = await historyToggleFavorite(result.history_id);
      setFav(now);
      onTranslated?.();
    } catch {
      /* 忽略 */
    }
  }

  // 交换用的是**当前实际生效的**语言：某一侧是"自动"时，拿最近一次翻译判出来的顶上。
  // 一次都还没翻过、又两侧都自动时无从交换，按钮禁用（比换出一个瞎猜的方向强）。
  const effSrc = dir.src ?? result?.src_lang ?? null;
  const effTgt = dir.tgt ?? result?.tgt_lang ?? null;
  const canSwap = !!(effSrc && effTgt && effSrc !== effTgt);

  /**
   * 选语言。新选的值撞上另一侧时，把**另一侧让开**（挪到原来的这一侧），与谷歌翻译一致。
   *
   * 为什么必须在这里挡：两侧都手选且相同时，后端的同语言保护会把目标改成别的语言
   * （否则等于拿 zh→zh 去打引擎、原样返回），但方向条渲染的是用户请求值、不是实际生效值
   * ⇒ 界面写着「中文→中文」、译文却是英文、历史里还记成「中文→英语」，三处自相矛盾。
   * 让开这一下在下拉框上肉眼可见，分歧就消失了。
   */
  function pickLang(side: "src" | "tgt", raw: string) {
    const v = raw || null;
    const other = side === "src" ? dir.tgt : dir.src;
    if (v && v === other) {
      // 撞车 → 互换：另一侧接手这一侧原来的值（原来是"自动"就回到自动）
      onChangeDir(side === "src" ? { src: v, tgt: dir.src } : { src: dir.tgt, tgt: v });
      return;
    }
    onChangeDir(side === "src" ? { ...dir, src: v } : { ...dir, tgt: v });
  }

  return (
    <>
      <div className="bar">
        <div className="lang">
          <select
            className="langsel"
            value={dir.src ?? ""}
            onChange={(e) => pickLang("src", e.target.value)}
            title="源语言。选「自动识别」时交给翻译引擎自己判，通常最准"
          >
            <option value="">
              {result && !dir.src ? `自动（${langLabel(result.src_lang)}）` : "自动识别"}
            </option>
            {langs.map((l) => (
              <option key={l.name} value={l.name}>
                {l.label}
              </option>
            ))}
          </select>

          <button
            className="swap"
            disabled={!canSwap}
            title={canSwap ? "交换方向" : "先翻一次，或两侧各选一个语言"}
            onClick={() => canSwap && onChangeDir({ src: effTgt, tgt: effSrc })}
          >
            ⇄
          </button>

          <select
            className="langsel"
            value={dir.tgt ?? ""}
            onChange={(e) => pickLang("tgt", e.target.value)}
            title="目标语言。选「自动」时按设置里的母语规则：母语→外语，外语→母语"
          >
            <option value="">
              {result && !dir.tgt ? `自动（${langLabel(result.tgt_lang)}）` : "自动"}
            </option>
            {langs.map((l) => (
              <option key={l.name} value={l.name}>
                {l.label}
              </option>
            ))}
          </select>
        </div>
        <div className="seg">
          <button className={engine === "local" ? "on" : ""} onClick={() => switchEngine("local")}>
            本地·Qwen
          </button>
          <button className={engine === "online" ? "on" : ""} onClick={() => switchEngine("online")}>
            在线
          </button>
        </div>
      </div>

      <textarea
        className="input"
        placeholder="输入或粘贴要翻译的文本…（回车翻译，Shift+回车换行）"
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={onKeyDown}
        autoFocus
      />

      <div className="out">
        <div className="out-label">
          译文
          {loading && <span className="spin" />}
          {result && !loading && <span style={{ color: "var(--faint)" }}>· {result.engine}</span>}
        </div>
        {error ? (
          <div className="out-error">{error}</div>
        ) : result?.text ? (
          <div className="out-text">{result.text}</div>
        ) : (
          <div className="out-text placeholder">{loading ? "" : "译文会出现在这里"}</div>
        )}
        <div className="out-tools">
          <button className="tool" disabled={!result?.text} onClick={copy}>
            {copied ? <span className="flash">✓ 已复制</span> : "复制"}
          </button>
          <button
            className={"tool" + (fav ? " on" : "")}
            disabled={!result?.history_id}
            onClick={toggleFav}
          >
            {fav ? "★ 已收藏" : "☆ 收藏"}
          </button>
        </div>
      </div>

      {cards.length > 0 && (
        <div className="wordcard-wrap">
          <WordCard r={cards[0]} />
        </div>
      )}
    </>
  );
}
