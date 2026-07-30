import { useRef, useState } from "react";
import type { DictResult } from "../types";

/** mdx 词条：沙箱 iframe（不允许脚本）渲染 HTML，按内容自适应高度。 */
function MdxFrame({ html }: { html: string }) {
  const ref = useRef<HTMLIFrameElement>(null);
  const [h, setH] = useState(80);
  // 基础防护：sandbox 不含 allow-scripts，脚本天然不执行；再简单剥一层 <script>。
  const safe = html.replace(/<script[\s\S]*?<\/script>/gi, "");
  const doc = `<!doctype html><meta charset="utf-8"><base target="_blank"><style>
    html,body{margin:0;padding:6px 8px;font:14px/1.65 -apple-system,"PingFang SC","Microsoft YaHei",sans-serif;color:#1d1d1f;background:#fff;}
    img{max-width:100%;height:auto;}
  </style>${safe}`;
  return (
    <iframe
      ref={ref}
      title="dict"
      sandbox="allow-same-origin allow-popups"
      srcDoc={doc}
      style={{ height: h }}
      onLoad={() => {
        try {
          const d = ref.current?.contentDocument;
          if (d?.body) setH(Math.min(560, d.body.scrollHeight + 16));
        } catch {
          /* 跨源读取失败则用默认高度 */
        }
      }}
    />
  );
}

export default function WordCard({ r }: { r: DictResult }) {
  if (r.kind === "mdx") {
    return (
      <div className="dictcard">
        <MdxFrame html={r.html} />
        <div className="src-tag">{r.source}</div>
      </div>
    );
  }

  // 有道：结构化释义
  return (
    <div className="dictcard">
      <div className="word">{r.word}</div>
      {(r.uk || r.us) && (
        <div className="phon">
          {r.uk && <span>英&nbsp;{r.uk}&nbsp;&nbsp;</span>}
          {r.us && <span>美&nbsp;{r.us}</span>}
        </div>
      )}
      {r.senses.length > 0 && (
        <div className="defs">
          {r.senses.map((s, i) => (
            <div className="def" key={i}>
              {s.pos && <span className="pos">{s.pos}</span>}
              {s.text}
            </div>
          ))}
        </div>
      )}
      <div className="src-tag">{r.source}</div>
    </div>
  );
}
