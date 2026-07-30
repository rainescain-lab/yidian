import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { Engine, TranslateResult, DictResult } from "../types";
import { translate, dictLookup, historyToggleFavorite } from "../api";
import { langLabel, looksLikeWord } from "../lib/format";
import WordCard from "../components/WordCard";

interface Props {
  defaultEngine: Engine;
  prefill: { text: string; nonce: number } | null;
  onTranslated?: () => void; // 通知历史页刷新
}

export default function TranslateView({ defaultEngine, prefill, onTranslated }: Props) {
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

  async function runTranslate(text: string, eng: Engine) {
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
    const delay = engineRef.current === "local" ? 1200 : 800;
    const t = setTimeout(() => runTranslate(input, engineRef.current), delay);
    return () => clearTimeout(t);
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

  const src = result ? langLabel(result.src_lang) : "自动";
  const tgt = result ? langLabel(result.tgt_lang) : "";

  return (
    <>
      <div className="bar">
        <div className="lang">
          <span>{src}</span>
          <span className="swap">⇄</span>
          <span>{tgt || "中文/英语"}</span>
          {!result && <span className="auto">自动识别</span>}
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
