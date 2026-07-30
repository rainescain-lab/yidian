import { useCallback, useEffect, useState } from "react";
import type { HistoryItem } from "../types";
import { historyList, historyDelete, historyToggleFavorite, historyClear } from "../api";
import { timeAgo, langLabel } from "../lib/format";

interface Props {
  onPick: (text: string) => void;
  reloadKey: number; // 外部触发刷新（翻译后）
}

export default function HistoryView({ onPick, reloadKey }: Props) {
  const [query, setQuery] = useState("");
  const [favOnly, setFavOnly] = useState(false);
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async () => {
    try {
      const rows = await historyList(query, favOnly, 300);
      setItems(rows);
    } catch {
      setItems([]);
    } finally {
      setLoaded(true);
    }
  }, [query, favOnly]);

  // 搜索防抖
  useEffect(() => {
    const t = setTimeout(load, 200);
    return () => clearTimeout(t);
  }, [load, reloadKey]);

  async function toggleFav(e: React.MouseEvent, id: number) {
    e.stopPropagation();
    await historyToggleFavorite(id);
    load();
  }
  async function del(e: React.MouseEvent, id: number) {
    e.stopPropagation();
    await historyDelete(id);
    load();
  }
  async function clearAll() {
    if (!confirm("清空全部翻译历史？收藏也会一并删除。")) return;
    await historyClear();
    load();
  }

  return (
    <>
      <div className="view-head">
        <div className="view-title">我的翻译</div>
        {items.length > 0 && (
          <button className="btn danger" onClick={clearAll}>
            清空
          </button>
        )}
      </div>

      <div className="bar">
        <div className="search" style={{ flex: 1 }}>
          <span className="ico">🔍</span>
          <input
            placeholder="搜索原文或译文…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <button className={"chip" + (favOnly ? " on" : "")} onClick={() => setFavOnly(!favOnly)}>
          ★ 仅收藏
        </button>
      </div>

      {items.length === 0 ? (
        <div className="empty">
          <div className="big">🕘</div>
          <div>{!loaded ? "" : favOnly ? "还没有收藏" : query ? "没有匹配的记录" : "翻译过的内容会自动出现在这里"}</div>
        </div>
      ) : (
        <div className="list">
          {items.map((it) => (
            <div className="row" key={it.id} onClick={() => onPick(it.source_text)} title="点击回填到翻译框">
              <div className="src">{it.source_text}</div>
              <div className="tgt">{it.translated_text}</div>
              <div className="meta">
                <span className="badge">
                  {langLabel(it.src_lang)}→{langLabel(it.tgt_lang)}
                </span>
                <span>{it.engine}</span>
                <span>{timeAgo(it.created_at)}</span>
                <span className="row-actions">
                  <button
                    className={"iconbtn star" + (it.favorite ? " on" : "")}
                    onClick={(e) => toggleFav(e, it.id)}
                    title={it.favorite ? "取消收藏" : "收藏"}
                  >
                    {it.favorite ? "★" : "☆"}
                  </button>
                  <button className="iconbtn" onClick={(e) => del(e, it.id)} title="删除">
                    ✕
                  </button>
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
