import { useEffect, useRef, useState } from "react";
import type { DictItem, DictResult } from "../types";
import {
  dictLookup,
  dictList,
  dictSetEnabled,
  dictAddMdx,
  dictRemove,
  dictReorder,
} from "../api";
import WordCard from "../components/WordCard";

export default function DictView() {
  const [word, setWord] = useState("");
  const [results, setResults] = useState<DictResult[]>([]);
  const [searched, setSearched] = useState(false);
  const [dicts, setDicts] = useState<DictItem[]>([]);
  const [manage, setManage] = useState(false);
  const [toast, setToast] = useState("");
  const reqId = useRef(0);

  function flash(m: string) {
    setToast(m);
    setTimeout(() => setToast(""), 2200);
  }

  async function loadDicts() {
    try {
      setDicts(await dictList());
    } catch {
      setDicts([]);
    }
  }
  useEffect(() => {
    loadDicts();
  }, []);

  async function lookup(w: string) {
    const id = ++reqId.current;
    const q = w.trim();
    if (!q) {
      setResults([]);
      setSearched(false);
      return;
    }
    try {
      const rs = await dictLookup(q);
      if (id === reqId.current) {
        setResults(rs);
        setSearched(true);
      }
    } catch (e) {
      if (id === reqId.current) {
        setResults([]);
        setSearched(true);
        flash(String(e));
      }
    }
  }

  // 查词防抖
  useEffect(() => {
    const t = setTimeout(() => lookup(word), 300);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [word]);

  async function importMdx() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        multiple: false,
        filters: [{ name: "MDX 词典", extensions: ["mdx"] }],
      });
      if (typeof picked === "string") {
        await dictAddMdx(picked);
        await loadDicts();
        flash("已导入词典");
      }
    } catch (e) {
      flash("导入失败：" + String(e));
    }
  }

  async function toggle(d: DictItem) {
    await dictSetEnabled(d.id, !d.enabled);
    loadDicts();
  }
  async function remove(d: DictItem) {
    if (!confirm(`移除词典「${d.name}」？（仅从列表移除，不删源文件）`)) return;
    await dictRemove(d.id);
    loadDicts();
  }
  async function move(idx: number, dir: -1 | 1) {
    const arr = [...dicts];
    const j = idx + dir;
    if (j < 0 || j >= arr.length) return;
    [arr[idx], arr[j]] = [arr[j], arr[idx]];
    setDicts(arr);
    await dictReorder(arr.map((d) => d.id));
  }

  return (
    <>
      <div className="view-head">
        <div className="view-title">词典</div>
        <button className="chip" onClick={() => setManage(!manage)}>
          {manage ? "← 返回查词" : "管理词典"}
        </button>
      </div>

      {!manage ? (
        <>
          <div className="search">
            <span className="ico">🔍</span>
            <input
              placeholder="输入要查的单词…"
              value={word}
              onChange={(e) => setWord(e.target.value)}
              autoFocus
            />
          </div>
          {results.length > 0 ? (
            <div className="list">
              {results.map((r, i) => (
                <WordCard key={i} r={r} />
              ))}
            </div>
          ) : (
            <div className="empty">
              <div className="big">📖</div>
              <div>{searched ? "词典里没有这个词" : "查词结果会显示音标、释义和例句"}</div>
            </div>
          )}
        </>
      ) : (
        <>
          <div className="view-sub">拖动顺序即查词优先级；内置词典可开关、mdx 可增删。</div>
          <div className="list">
            {dicts.map((d, i) => (
              <div className="dictrow" key={d.id}>
                <span className="grip" title="上下移动调整优先级">
                  <button className="iconbtn" onClick={() => move(i, -1)} disabled={i === 0}>
                    ↑
                  </button>
                  <button
                    className="iconbtn"
                    onClick={() => move(i, 1)}
                    disabled={i === dicts.length - 1}
                  >
                    ↓
                  </button>
                </span>
                <div>
                  <div className="dname">{d.name}</div>
                  <div className="dmeta">
                    MDX {d.path && `· ${d.path}`}
                  </div>
                </div>
                <div className="dactions">
                  <label className="switch">
                    <input type="checkbox" checked={d.enabled} onChange={() => toggle(d)} />
                    <span className="slider" />
                  </label>
                  {d.kind === "mdx" && (
                    <button className="iconbtn" onClick={() => remove(d)} title="移除">
                      ✕
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
          <div>
            <button className="btn primary" onClick={importMdx}>
              + 添加 .mdx 词典
            </button>
          </div>
        </>
      )}
      {toast && <div className="toast">{toast}</div>}
    </>
  );
}
