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
}
