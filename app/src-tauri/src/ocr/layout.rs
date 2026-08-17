//! OCR 结果的版面后处理：滤掉幻觉框、把同一视觉行上的碎框并回一行。
//!
//! **为什么需要这一层**（2026-08-15 实测定位）：检测端交回来的从来不是「一行一个框」，
//! 而是一堆碎片 —— 一句话可能被切成两三段，还会夹带置信度不高的幻觉字。而上层
//! （`overlay_capture`）是**按框翻译、按框贴回原位**的，于是：
//!
//! - 碎片各翻各的 ⇒ 每块译文只是半句话，贴回去就成了「对照不上」；
//! - 幻觉框也会被翻译、也会被贴上屏 ⇒ 用户看到凭空冒出来的两个字（实测就是「翁帕」）；
//! - 幻觉框的 y 常落在两行之间 ⇒ 深蓝块互相重叠。
//!
//! 所以在送去翻译之前，先把框收拾成「一行一个」。

use super::LineBox;

/// 置信度低于此值一律丢弃。实测幻觉框拿到过 0.42 / 0.56，而真文字普遍 0.82 以上。
pub const MIN_SCORE: f64 = 0.60;

/// **单字框**要留下来必须达到的置信度。
///
/// 单独一条 [`MIN_SCORE`] 挡不住全部幻觉：实测边缘被框选切掉一半的字形，放大后会被
/// 检测成一个小斑块并认成某个汉字，拿到过 0.72 / 0.75 —— 分数不低，但内容纯属虚构。
/// 判别特征是「就一个字」：截图里真正孤零零只有一个字、且模型还认不太准的情形很少，
/// 宁可漏掉这种也不要让幻觉上屏。被丢掉的框都会记进日志，滤错了查得出来。
pub const MIN_SCORE_SINGLE_CHAR: f64 = 0.85;

/// 同一行里两个框的水平间隙小于「行高 × 此系数」就并成一段文字。
/// 取 1.5 是为了区分「词与词之间的空格」和「左右两栏之间的留白」。
const MERGE_GAP_RATIO: f64 = 1.5;

/// 纵向重叠超过较矮者高度的这个比例，就认为两个框在同一视觉行上。
const SAME_ROW_OVERLAP: f64 = 0.5;

/// 被丢掉的框 + 原因，供日志留痕。
#[derive(Debug, Clone)]
pub struct Dropped {
    pub reason: &'static str,
    pub line: LineBox,
}

/// 滤掉明显是幻觉的框。返回 (留下的, 丢掉的)。
pub fn drop_junk(boxes: Vec<LineBox>) -> (Vec<LineBox>, Vec<Dropped>) {
    let mut keep = Vec::with_capacity(boxes.len());
    let mut dropped = Vec::new();
    for b in boxes {
        let chars = b.text.trim().chars().count();
        if chars == 0 {
            dropped.push(Dropped { reason: "空文本", line: b });
        } else if b.score < MIN_SCORE {
            dropped.push(Dropped { reason: "置信度过低", line: b });
        } else if chars == 1 && b.score < MIN_SCORE_SINGLE_CHAR {
            dropped.push(Dropped { reason: "单字且不够确信(疑似幻觉)", line: b });
        } else {
            keep.push(b);
        }
    }
    (keep, dropped)
}

/// 两个框之间的间隙要占到行高的这个比例，才认为原文那里本来就有个空格。
///
/// 送检切块的切口是挑在**墨水最少处**的：落在词与词之间时，两块的框之间会留下一整个
/// 空格的宽度；落在字与字之间时只剩一两像素。所以间隙本身就是「这里原本有没有空格」
/// 的可靠信号 —— 不看它的话，被切口分开的一个单词会被接成 `com pact`。
const SPACE_GAP_RATIO: f64 = 0.2;

/// 两个框在同一行上时怎么接文字：中日韩两侧不加空格；拉丁文字按实际间隙决定。
fn join_text(a: &str, b: &str, gap: f64, h: f64) -> String {
    let (a, b) = (a.trim_end(), b.trim_start());
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() {
        return a.to_string();
    }
    let is_cjk = |c: char| {
        matches!(c as u32,
            0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFF65)
    };
    let cjk_side = a.chars().next_back().map(is_cjk).unwrap_or(false)
        || b.chars().next().map(is_cjk).unwrap_or(false);
    if cjk_side || gap < SPACE_GAP_RATIO * h.max(1.0) {
        format!("{a}{b}")
    } else {
        format!("{a} {b}")
    }
}

fn union_into(dst: &mut LineBox, src: &LineBox) {
    // 间隙要在合并框之前量：合并之后 dst.w 就把这段间隙吞掉了
    let gap = src.x - (dst.x + dst.w);
    let h = dst.h.max(src.h);
    let (x0, y0) = (dst.x.min(src.x), dst.y.min(src.y));
    let x1 = (dst.x + dst.w).max(src.x + src.w);
    let y1 = (dst.y + dst.h).max(src.y + src.h);
    dst.text = join_text(&dst.text, &src.text, gap, h);
    dst.x = x0;
    dst.y = y0;
    dst.w = x1 - x0;
    dst.h = y1 - y0;
    dst.score = dst.score.min(src.score); // 一行的可信度取最弱的那一段
}

/// 把碎框并成「一行一个」，并按阅读顺序（上→下、左→右）返回。
///
/// 只合并**同一视觉行上挨得够近**的框：左右分栏那种大留白不会被并到一起。
pub fn group_lines(boxes: Vec<LineBox>) -> Vec<LineBox> {
    let mut boxes: Vec<LineBox> = boxes
        .into_iter()
        .filter(|b| b.w > 0.0 && b.h > 0.0 && !b.text.trim().is_empty())
        .collect();
    if boxes.len() < 2 {
        return boxes;
    }
    // 按纵向中心排；同一行内按 x 排。NaN 不该出现，真出现就当相等，别 panic。
    boxes.sort_by(|a, b| {
        let key = |l: &LineBox| (l.y + l.h / 2.0, l.x);
        let (ka, kb) = (key(a), key(b));
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });

    // 分行：排序后同一行的框必然相邻，故只跟"当前行"比即可
    let mut rows: Vec<Vec<LineBox>> = Vec::new();
    for b in boxes {
        let joined = match rows.last() {
            Some(row) => {
                let ry0 = row.iter().fold(f64::MAX, |m, l| m.min(l.y));
                let ry1 = row.iter().fold(f64::MIN, |m, l| m.max(l.y + l.h));
                let overlap = (b.y + b.h).min(ry1) - b.y.max(ry0);
                overlap > 0.0 && overlap >= SAME_ROW_OVERLAP * b.h.min(ry1 - ry0)
            }
            None => false,
        };
        if joined {
            rows.last_mut().unwrap().push(b);
        } else {
            rows.push(vec![b]);
        }
    }

    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        let mut cur: Option<LineBox> = None;
        for b in row {
            match cur.as_mut() {
                Some(c) => {
                    let gap = b.x - (c.x + c.w);
                    if gap <= MERGE_GAP_RATIO * c.h.max(b.h) {
                        union_into(c, &b);
                    } else {
                        out.push(c.clone());
                        *c = b;
                    }
                }
                None => cur = Some(b),
            }
        }
        if let Some(c) = cur {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 行 → 段
// ---------------------------------------------------------------------------

/// 一段文字：由若干**视觉行**合并而来。
///
/// # 为什么必须有这一层
///
/// 上层是「按框翻译、按框贴回原位」。如果框＝行，那么一段被换行切开的话就会**各翻各的**，
/// 贴回去就是半句半句对不上 —— 用户报的正是这个。实测同一条亚马逊商品标题：
/// 逐行翻出来是「…合身喇叭形**毕业派对**」+「**连衣裙**、商务休闲工作服…」（名词被劈成两半），
/// 整段翻出来是「…合身喇叭**毕业派对礼服**、商务休闲工作服…」。
///
/// 有道也是这么干的：它图片翻译 API 的返回单位就是 region —— `linesCount: 7` 表示
/// 7 行合并成 1 个区域，整段一个 `context`、整段一个 `tranContent`。
#[derive(Debug, Clone)]
pub struct Paragraph {
    /// 组成它的视觉行（保留原框，渲染时按行擦除比按整段外接框擦更安全）。
    pub lines: Vec<LineBox>,
    /// 合并后的整段原文。
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 段内最弱的一行的可信度。
    pub score: f64,
}

/// 行间空隙超过「中位行高 × 此系数」就不可能是同一段。
const PARA_MAX_GAP: f64 = 0.8;
/// 两行横向重叠率（分母取较窄者）低于此值就不是同一段。
const PARA_MIN_X_OVERLAP: f64 = 0.2;
/// 字号相对差超过此值就不是同一段。
///
/// ⚠ **别按 0.25 来**（那是 manga-image-translator / BallonsTranslator / WindowTranslator
/// 三家收敛到的值，但它们量的是文本**区域**的短边，不是单行框高）。我们拿到的是逐行框高，
/// 而**同一段里两行的框高本来就能差近一半**：一行有大写字母+降部（`Apply`）时框高约 0.9×字号，
/// 另一行只有 x 高度的字母（`some now`）时只有约 0.5×字号。
/// 2026-08-16 真机实测同一段的两行是 14px 和 10px（差 40%），0.25 直接把它们判成两段。
const PARA_MAX_FONT_DIFF: f64 = 0.45;

/// 左对齐/居中对齐的容差，按中位行高的倍数算。
///
/// ⚠ **给足，因为缩进是常态**：首行缩进、悬挂缩进都会让同一段里两行的左边缘差好几个字符。
/// 真机踩过的例子：`• Autocompact is thrashing: …` 的续行对齐在项目符号之后，左边缘差 14px，
/// 而当时容差只有 0.6×14=8.4px ⇒ 同一句话被判成两段。
/// 放宽不会误并左右分栏 —— 那是靠横向重叠率那一关挡的。
const PARA_ALIGN_TOL: f64 = 2.5;

fn is_cjk_char(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFF65)
}

/// 行尾出现这些，说明话还没说完 —— 强续接信号。
fn ends_with_continuation(s: &str) -> bool {
    matches!(
        s.trim_end().chars().next_back(),
        Some(',' | '，' | '、' | ':' | '：' | ';' | '；' | '(' | '（' | '[' | '【' | '“' | '「' | '《' | '/' | '-')
    )
}

/// 行首出现这些，说明它是上一行的续尾 —— 强续接信号。
fn starts_with_continuation(s: &str) -> bool {
    matches!(
        s.trim_start().chars().next(),
        Some(')' | '）' | ']' | '】' | '”' | '」' | '》' | ',' | '，' | '、' | '。' | '.' | '!' | '！' | '?' | '？')
    )
}

/// 行尾是句末标点。注意**这不等于分段** —— 一段里本来就可以有多句话，
/// 所以它的优先级排在「下一行首词本来塞得下吗」之后。
fn ends_with_terminator(s: &str) -> bool {
    matches!(
        s.trim_end().chars().next_back(),
        Some('。' | '！' | '？' | '!' | '?' | '.' | '…')
    )
}

/// 有没有没闭合的括号/引号（有就说明还没说完）。
fn has_unclosed(s: &str) -> bool {
    let mut depth = 0i32;
    let mut dq = 0usize;
    for c in s.chars() {
        match c {
            '(' | '（' | '[' | '【' | '{' | '「' | '《' => depth += 1,
            ')' | '）' | ']' | '】' | '}' | '」' | '》' => depth -= 1,
            '"' => dq += 1,
            _ => {}
        }
    }
    depth > 0 || dq % 2 == 1
}

/// **Tesseract 的 `FirstWordWouldHaveFit`**：下一行的第一个词，塞得进上一行行尾的空白吗？
///
/// 塞得进 ⇒ 上一行是**主动**断的 ⇒ 新段落；塞不进 ⇒ 是被**迫**折行 ⇒ 同一段。
/// 这一条比任何间距阈值都准，而且纯几何、不依赖语言。
fn first_word_would_have_fit(prev: &LineBox, next: &LineBox, column_right: f64) -> bool {
    let avail = column_right - (prev.x + prev.w);
    if avail <= 0.0 {
        return false;
    }
    let chars = next.text.trim().chars().count().max(1) as f64;
    let per_char = next.w / chars;
    let first_word = next
        .text
        .trim_start()
        .split_whitespace()
        .next()
        .map(|w| w.chars().count())
        .unwrap_or(0)
        .max(1) as f64;
    // 留一个词距的余量：正好卡满不算"塞得进"
    first_word * per_char + per_char < avail
}

/// 换行处怎么接文字：连字符断词去掉连字符；中日韩不加空格；其余补一个空格
/// （换行本身就代替了那个空格）。
fn join_wrapped(a: &str, b: &str) -> String {
    let (a, b) = (a.trim_end(), b.trim_start());
    if a.is_empty() {
        return b.to_string();
    }
    if b.is_empty() {
        return a.to_string();
    }
    let last = a.chars().next_back().unwrap();
    let first = b.chars().next().unwrap();
    if last == '-' && first.is_lowercase() {
        return format!("{}{}", &a[..a.len() - last.len_utf8()], b); // word-\nwrap → wordwrap
    }
    if is_cjk_char(last) || is_cjk_char(first) {
        format!("{a}{b}")
    } else {
        format!("{a} {b}")
    }
}

/// 把**视觉行**合并成**段**。输入应当是 [`group_lines`] 的产物（已按阅读顺序排好）。
pub fn group_paragraphs(lines: Vec<LineBox>) -> Vec<Paragraph> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut heights: Vec<f64> = lines.iter().map(|l| l.h).collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_h = heights[heights.len() / 2].max(1.0);

    // ⚠ **「下一行首词本来塞得下吗」要拿整列的右边界去量，不能拿当前段的。**
    //   段里只有一行时，段的右边界就是那一行自己，可用空白恒为 0 ⇒ 这条判据直接失效，
    //   两个本来无关的单行条目会被粘成一段（实测：亚马逊页上的「#1 Best Seller…」和
    //   「2K+ bought in past month」两个角标会被并到一起）。
    //   列的右边界＝所有**左边缘与它对齐**的行里最靠右的那个，也就是文本换行的真实边界。
    let col_tol = PARA_ALIGN_TOL * median_h;
    let column_right: Vec<f64> = lines
        .iter()
        .map(|a| {
            lines
                .iter()
                .filter(|b| (a.x - b.x).abs() <= col_tol)
                .fold(a.x + a.w, |m, b| m.max(b.x + b.w))
        })
        .collect();

    let mut paras: Vec<Vec<LineBox>> = Vec::new();
    for (i, l) in lines.into_iter().enumerate() {
        let joins = match paras.last() {
            None => false,
            Some(cur) => {
                let prev = cur.last().unwrap();
                can_join_paragraph(prev, &l, column_right[i], median_h)
            }
        };
        if joins {
            paras.last_mut().unwrap().push(l);
        } else {
            paras.push(vec![l]);
        }
    }

    paras
        .into_iter()
        .map(|group| {
            let x = group.iter().fold(f64::MAX, |m, l| m.min(l.x));
            let y = group.iter().fold(f64::MAX, |m, l| m.min(l.y));
            let r = group.iter().fold(f64::MIN, |m, l| m.max(l.x + l.w));
            let b = group.iter().fold(f64::MIN, |m, l| m.max(l.y + l.h));
            let score = group.iter().fold(f64::MAX, |m, l| m.min(l.score));
            let mut text = String::new();
            for l in &group {
                text = join_wrapped(&text, &l.text);
            }
            Paragraph {
                lines: group,
                text,
                x,
                y,
                w: r - x,
                h: b - y,
                score,
            }
        })
        .collect()
}

/// 判据的优先级是有讲究的，别随手调换：
/// 1. **几何门槛**不过 → 断（连挨都不挨，谈不上同段）
/// 2. **强标点线索** → 续（行尾逗号/未闭合括号、行首右括号，这些几乎不会错）
/// 3. **下一行首词本来塞得下** → 断（上一行是主动断的）
/// 4. **上一行以句末标点结尾** → 断
/// 5. 其余 → 续（默认认为是被迫折行）
///
/// ⚠ 第 4 条必须排在第 3 条后面：**一段里本来就可以有多句话**，
/// 单看"上一行以句号结尾"就分段，会把正常段落切碎。
fn can_join_paragraph(prev: &LineBox, next: &LineBox, column_right: f64, median_h: f64) -> bool {
    // 1. 几何门槛
    let gap = next.y - (prev.y + prev.h);
    if gap > PARA_MAX_GAP * median_h || gap < -prev.h {
        return false;
    }
    let (l, r) = (prev.x.max(next.x), (prev.x + prev.w).min(next.x + next.w));
    let overlap = (r - l).max(0.0);
    if overlap < PARA_MIN_X_OVERLAP * prev.w.min(next.w).max(1.0) {
        return false;
    }
    let (hi, lo) = (prev.h.max(next.h), prev.h.min(next.h).max(1.0));
    if (hi - lo) / lo > PARA_MAX_FONT_DIFF {
        return false;
    }
    let tol = PARA_ALIGN_TOL * median_h;
    let left_aligned = (prev.x - next.x).abs() <= tol;
    let centered = ((prev.x + prev.w / 2.0) - (next.x + next.w / 2.0)).abs() <= tol;
    if !left_aligned && !centered {
        return false;
    }

    // 2. 强标点线索
    if ends_with_continuation(&prev.text)
        || starts_with_continuation(&next.text)
        || has_unclosed(&prev.text)
    {
        return true;
    }
    // 3. 上一行是主动断的？
    if first_word_would_have_fit(prev, next, column_right) {
        return false;
    }
    // 4. 句末标点
    if ends_with_terminator(&prev.text) {
        return false;
    }
    // 5. 默认：被迫折行
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lb(x: f64, y: f64, w: f64, h: f64, text: &str, score: f64) -> LineBox {
        LineBox {
            text: text.to_string(),
            x,
            y,
            w,
            h,
            score,
        }
    }

    // ---- drop_junk ----

    #[test]
    fn junk_drops_low_confidence_boxes() {
        let (keep, dropped) = drop_junk(vec![
            lb(0.0, 0.0, 100.0, 12.0, "hello world", 0.92),
            lb(0.0, 20.0, 10.0, 10.0, "x", 0.42), // 实测幻觉分数
        ]);
        assert_eq!(keep.len(), 1);
        assert_eq!(keep[0].text, "hello world");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].reason, "置信度过低");
    }

    /// **回归钉**：0.72 分的单字幻觉（就是屏幕上那个「翁帕」的来源）必须被挡住，
    /// 而同样分数的整句必须留下 —— 只靠一条分数线是做不到的。
    #[test]
    fn junk_drops_a_confident_looking_single_char_but_keeps_a_sentence() {
        let (keep, dropped) = drop_junk(vec![
            lb(0.0, 18.0, 22.0, 15.0, "帕", 0.72),
            lb(0.0, 0.0, 400.0, 16.0, "3 times in a row. A file", 0.72),
        ]);
        assert_eq!(keep.len(), 1, "整句该留下");
        assert_eq!(keep[0].text, "3 times in a row. A file");
        assert_eq!(dropped[0].reason, "单字且不够确信(疑似幻觉)");
    }

    #[test]
    fn junk_keeps_a_single_char_when_the_model_is_sure() {
        let (keep, _) = drop_junk(vec![lb(0.0, 0.0, 20.0, 20.0, "文", 0.97)]);
        assert_eq!(keep.len(), 1, "确信的单字是正常内容，不许误杀");
    }

    #[test]
    fn junk_drops_blank_text() {
        let (keep, dropped) = drop_junk(vec![lb(0.0, 0.0, 10.0, 10.0, "   ", 0.99)]);
        assert!(keep.is_empty());
        assert_eq!(dropped[0].reason, "空文本");
    }

    // ---- group_lines ----

    /// **回归钉**：这正是切块之后必然出现的形态 —— 同一行文字被切口分成两段。
    /// 不并回去的话，两段各翻各的，贴回屏幕就是半句半句对不上。
    #[test]
    fn lines_merge_two_fragments_of_the_same_row() {
        let out = group_lines(vec![
            lb(24.0, 7.0, 640.0, 14.0, "3 times in a row. A", 0.9),
            lb(670.0, 8.0, 450.0, 14.0, "file being read", 0.88),
        ]);
        assert_eq!(out.len(), 1, "同一行的两段该并成一行: {out:?}");
        assert_eq!(out[0].text, "3 times in a row. A file being read");
        assert_eq!(out[0].x, 24.0);
        assert_eq!(out[0].w, 670.0 + 450.0 - 24.0, "框应取两者并集");
        assert!((out[0].score - 0.88).abs() < 1e-9, "可信度取最弱那段");
    }

    #[test]
    fn lines_do_not_merge_across_a_wide_gutter() {
        // 左右分栏：间隙 300px 远大于 1.5 倍行高，不许并
        let out = group_lines(vec![
            lb(0.0, 0.0, 100.0, 14.0, "left", 0.9),
            lb(400.0, 0.0, 100.0, 14.0, "right", 0.9),
        ]);
        assert_eq!(out.len(), 2, "分栏不该被并成一行: {out:?}");
    }

    #[test]
    fn lines_keep_rows_separate_and_in_reading_order() {
        let out = group_lines(vec![
            lb(30.0, 28.0, 500.0, 14.0, "second row", 0.9),
            lb(24.0, 7.0, 600.0, 14.0, "first row", 0.9),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "first row");
        assert_eq!(out[1].text, "second row");
    }

    /// **回归钉**：切口落在**单词中间**时（框间只剩一两像素），接回去不许多出空格。
    /// 否则 "compact" 会被接成 "com pact"，翻译当场就废。
    #[test]
    fn lines_rejoin_a_word_split_by_the_cut_without_a_space() {
        let out = group_lines(vec![
            lb(600.0, 8.0, 62.0, 14.0, "com", 0.9),
            lb(663.0, 8.0, 60.0, 14.0, "pact", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "compact", "间隙只有 1px，本来就没有空格");
    }

    /// 反过来：切口落在**词与词之间的空格**上（框间留着一个空格宽），必须接出空格。
    #[test]
    fn lines_keep_a_real_word_space_at_the_cut() {
        let out = group_lines(vec![
            lb(600.0, 8.0, 60.0, 14.0, "a tool", 0.9),
            lb(668.0, 8.0, 60.0, 14.0, "output", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "a tool output");
    }

    #[test]
    fn lines_join_cjk_without_inserting_a_space() {
        let out = group_lines(vec![
            lb(0.0, 0.0, 40.0, 20.0, "上下文", 0.9),
            lb(45.0, 0.0, 40.0, 20.0, "窗口", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "上下文窗口", "中文之间不该塞空格");
    }

    #[test]
    fn lines_handle_empty_and_single_input() {
        assert!(group_lines(Vec::new()).is_empty());
        let one = group_lines(vec![lb(0.0, 0.0, 10.0, 10.0, "a", 0.9)]);
        assert_eq!(one.len(), 1);
    }

    /// 退化框（宽或高为 0）会让「同行判定」除零/永远成立，直接剔掉。
    #[test]
    fn lines_discard_degenerate_boxes() {
        let out = group_lines(vec![
            lb(0.0, 0.0, 0.0, 10.0, "zero width", 0.9),
            lb(0.0, 0.0, 10.0, 10.0, "ok", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "ok");
    }

    // ---- group_paragraphs ----

    /// 用户 2026-08-16 真实截图（亚马逊商品页）的 OCR 行框，坐标一字未改地照抄自 diag.log。
    fn amazon_lines() -> Vec<LineBox> {
        vec![
            lb(54.0, 21.0, 196.0, 14.0, "Visit the PRETTYGARDEN Store", 0.95),
            lb(53.0, 44.0, 654.0, 21.0, "PRETTYGARDEN Summer A Line Dresses for Women Elegant", 0.95),
            lb(53.0, 76.0, 639.0, 22.0, "Classy Sleeveless Tank Top, Fit and Flare Graduation Party", 0.91),
            lb(52.0, 107.0, 636.0, 22.0, "Dress, Business Casual Work Clothes, Spring Short Cocktail", 0.93),
            lb(52.0, 141.0, 135.0, 20.0, "Date Outfits", 0.88),
            lb(52.0, 172.0, 137.0, 14.0, "4.1(2.054)", 0.89),
            lb(53.0, 195.0, 191.0, 22.0, "1 sustainability feature", 0.91),
            lb(54.0, 229.0, 244.0, 17.0, "#1 Best Seller in Women's Cocktail Dresses", 0.89),
            lb(52.0, 261.0, 146.0, 14.0, "2K+ bought in past month", 0.85),
        ]
    }

    /// **回归钉（这条就是用户报的「对不准」）**：换行切开的商品标题必须并回一段。
    ///
    /// 逐行翻的实测后果：「…合身喇叭形**毕业派对**」+「**连衣裙**、商务休闲工作服…」——
    /// `Graduation Party Dress` 被劈成两块，中文读起来就是断的。
    #[test]
    fn paragraphs_merge_a_wrapped_title() {
        let paras = group_paragraphs(group_lines(amazon_lines()));
        let title = paras
            .iter()
            .find(|p| p.text.starts_with("PRETTYGARDEN Summer"))
            .expect("找不到标题段");
        assert_eq!(title.lines.len(), 4, "标题的 4 行该并成一段: {:?}", title.text);
        assert!(
            title.text.contains("Graduation Party Dress"),
            "跨行的名词短语必须接上，实得：{}",
            title.text
        );
        assert!(title.text.ends_with("Date Outfits"));
        // 外接框要盖住这 4 行
        assert!((title.x - 52.0).abs() < 1.0);
        assert!((title.y - 44.0).abs() < 1.0);
        assert!(title.w >= 654.0 && title.h >= 117.0, "{title:?}");
    }

    /// 字号不同的相邻行不许并段（「Visit the … Store」是小字链接，标题是大字）。
    #[test]
    fn paragraphs_do_not_merge_across_a_font_size_change() {
        let paras = group_paragraphs(group_lines(amazon_lines()));
        assert!(
            paras.iter().any(|p| p.text == "Visit the PRETTYGARDEN Store"),
            "小字链接该自成一段: {:?}",
            paras.iter().map(|p| &p.text).collect::<Vec<_>>()
        );
    }

    /// **回归钉**：两个各自独立的单行角标不许被粘成一段。
    ///
    /// 这一条专治「首词塞得下」判据在单行段上的退化 —— 用当前段的右边界去量，
    /// 可用空白恒为 0、判据失效，两条角标就会被并到一起。必须用**整列**的右边界。
    #[test]
    fn paragraphs_keep_two_standalone_badges_apart() {
        let paras = group_paragraphs(group_lines(amazon_lines()));
        assert!(
            paras.iter().any(|p| p.text.starts_with("#1 Best Seller") && p.lines.len() == 1),
            "两个角标被粘住了: {:?}",
            paras.iter().map(|p| &p.text).collect::<Vec<_>>()
        );
    }

    /// **回归钉（2026-08-16 真机 e2e）**：一句话换行成两行，两行的
    /// ①框高差 40%（14px vs 10px，大写字母/降部造成的）
    /// ②左边缘差 14px（悬挂缩进，续行对齐在项目符号之后）
    /// 都必须**不**妨碍它们并成一段。坐标照抄自 diag.log。
    #[test]
    fn paragraphs_survive_a_hanging_indent_and_a_tall_first_line() {
        let out = group_paragraphs(vec![
            lb(14.0, 6.0, 1285.0, 14.0,
               "Autocompact is thrashing: context refilled to the limit within 3 turns of the previous compact, 3 times in a row. A file being read or a tool output is probably too large for the", 0.92),
            lb(28.0, 29.0, 541.0, 10.0,
               "context window. Try reading in smaller chunks, or use /clear to start fresh)", 0.91),
        ]);
        assert_eq!(out.len(), 1, "同一句话被判成了两段: {out:?}");
        assert!(
            out[0].text.contains("too large for the context window"),
            "跨行的句子必须接上，实得：{}",
            out[0].text
        );
    }

    /// 行尾逗号是强续接信号，哪怕首词本来塞得下也要接着。
    #[test]
    fn paragraphs_follow_a_trailing_comma() {
        let out = group_paragraphs(vec![
            lb(0.0, 0.0, 100.0, 14.0, "first part,", 0.9),
            lb(0.0, 18.0, 400.0, 14.0, "second part continues here", 0.9),
        ]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].text, "first part, second part continues here");
    }

    /// 连字符断词接回去要去掉连字符，不能留成 `word- wrap`。
    #[test]
    fn paragraphs_rejoin_a_hyphenated_word() {
        let out = group_paragraphs(vec![
            lb(0.0, 0.0, 200.0, 14.0, "this is a long hyphen-", 0.9),
            lb(0.0, 18.0, 200.0, 14.0, "ated word here", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "this is a long hyphenated word here");
    }

    /// 中文换行接回去不许多出空格。
    #[test]
    fn paragraphs_join_cjk_without_a_space() {
        let out = group_paragraphs(vec![
            lb(0.0, 0.0, 200.0, 16.0, "这是一段很长的中文", 0.9),
            lb(0.0, 20.0, 200.0, 16.0, "它被换行切开了", 0.9),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "这是一段很长的中文它被换行切开了");
    }

    /// 隔得远的两行不许并段。
    #[test]
    fn paragraphs_break_on_a_large_vertical_gap() {
        let out = group_paragraphs(vec![
            lb(0.0, 0.0, 200.0, 14.0, "alpha", 0.9),
            lb(0.0, 90.0, 200.0, 14.0, "beta", 0.9),
        ]);
        assert_eq!(out.len(), 2, "{out:?}");
    }

    #[test]
    fn paragraphs_handle_empty_input() {
        assert!(group_paragraphs(Vec::new()).is_empty());
    }
}
