//! 屏幕捕获：选屏 + 按逻辑坐标裁物理区域 → PNG。
//!
//! DPI 铁律：xcap 全程物理像素；前端 overlay 坐标是 CSS 逻辑像素；overlay 铺满单屏，
//! 故前端坐标天然相对该屏左上角，`物理 = 逻辑 × 该屏 scale_factor`（避开负原点/虚拟桌面）。

use std::io::Cursor;
use xcap::image::{DynamicImage, ImageFormat, RgbaImage};
use xcap::Monitor;

/// 逻辑像素 → 物理像素（四舍五入，非截断）。
pub fn dpi_scale(v: f64, scale: f64) -> i32 {
    (v * scale).round() as i32
}

#[derive(Clone, Copy, Debug)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub scale: f64,
}

/// Alt+Q 瞬间抓好的整屏原图 + 该屏信息（overlay 显示前抓，故图里绝无遮罩）。
pub struct Screenshot {
    pub info: MonitorInfo,
    pub full: RgbaImage,
}

/// 取包含物理点 (px,py) 的显示器信息（原点/物理尺寸/缩放）。
pub fn monitor_at(px: i32, py: i32) -> Result<MonitorInfo, String> {
    let m = Monitor::from_point(px, py).map_err(|e| format!("选屏失败: {e}"))?;
    Ok(MonitorInfo {
        x: m.x().map_err(|e| e.to_string())?,
        y: m.y().map_err(|e| e.to_string())?,
        w: m.width().map_err(|e| e.to_string())?,
        h: m.height().map_err(|e| e.to_string())?,
        scale: m.scale_factor().map_err(|e| e.to_string())? as f64,
    })
}

// ---------------------------------------------------------------------------
// 送检前的切块（图像内嵌翻译的成败全在这里）
// ---------------------------------------------------------------------------

/// PaddleOCR **检测端的长边预算**：超过大约这个像素数，它就把整张图等比缩回去，
/// 多喂的像素一点用没有 —— 只会把字缩小到检测不出来。
///
/// **这个常量是实测出来的，不是查文档抄的**（2026-08-15，真 `PaddleOCR-json v1.4.1`
/// + PP-OCRv3 det，素材＝一条真实截图里的英文）：
///
/// | 喂进去的图 | 结果 |
/// |---|---|
/// | 434×49 原样 | 一个框读全整句 |
/// | 664×49 原样 | 一个框读全整句 |
/// | **1327×49 原样** | **中间整段「A file being read or」静默漏检** |
/// | 2654×98（＝旧 `upscale_for_ocr` 放大 2 倍的做法） | **和不放大时一样地漏** |
/// | 5308×196（放大 4 倍） | **依旧漏**，还多幻觉出两个字 |
/// | 手工缩到 **960×35** | **和 1327 原样一样地漏** ← 就是这条钉死了内部缩放的存在 |
/// | 切成三块各自送检 | **漏掉的那句当场读回来** |
///
/// 后四行合在一起只能有一个解释：检测端把长边压到了同一个尺度，所以
/// 「1327 原样 / 放大 2 倍 / 放大 4 倍 / 手工缩到 960」殊途同归。
///
/// ⇒ **对宽图，放大是纯粹的无用功（还制造幻觉框），唯一有效的办法是切块。**
/// 这也是当初「截图翻译段落散、大段漏译、冒出莫名其妙的两个字」的真正根因。
pub const OCR_LONG_SIDE_BUDGET: u32 = 960;

/// 放大倍数上限。再高对识别没有额外收益，只是白烧 CPU 和内存。
const OCR_MAX_UPSCALE: u32 = 3;

/// 切割点允许偏离等分位置的最大距离（像素）。给它一点自由度，是为了能**挑到字缝**上切。
const CUT_SEARCH_RADIUS: u32 = 60;

/// 一块要送去 OCR 的瓦片：它在原图里的位置 + 送检时的放大倍数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// 送检前把这块放大几倍（坐标要按这个倍数缩回去）。
    pub factor: u32,
}

/// 把 `[0, len)` 切成若干段，**保证每段 ≤ budget**，并尽量把切口落在墨水最少的位置。
///
/// 为什么要挑墨水最少处：切在字缝里，每块内部都是完整的字 ⇒ **不需要重叠带、也不需要
/// 跨块去重**（重叠带会让同一段文字被两块各认一遍，合并时还要对文本做最长公共子串，
/// 又慢又容易错）。等分位置附近的自由度由 [`CUT_SEARCH_RADIUS`] 给。
///
/// `ink[i]` 是第 i 列（或行）的墨水量，长度不足按 0 处理（＝退化成等分切，仍然正确）。
pub fn plan_cuts(len: u32, budget: u32, ink: &[u32]) -> Vec<(u32, u32)> {
    if len == 0 {
        return Vec::new();
    }
    if budget == 0 || len <= budget {
        return vec![(0, len)];
    }
    // 段长的名义值必须留出 2×radius 的余量，否则挑字缝时可能把某一段撑过 budget。
    let radius = if budget > CUT_SEARCH_RADIUS * 4 {
        CUT_SEARCH_RADIUS
    } else {
        budget / 4
    };
    let usable = budget - radius * 2;
    let n = (len + usable - 1) / usable; // ceil，且 n ≥ 2
    let step = len as f64 / n as f64;

    let at = |p: u32| ink.get(p as usize).copied().unwrap_or(0);
    let mut cuts: Vec<u32> = Vec::with_capacity(n as usize - 1);
    for i in 1..n {
        let target = (i as f64 * step).round().clamp(1.0, (len - 1) as f64) as u32;
        let lo = target.saturating_sub(radius).max(1);
        let hi = (target + radius).min(len - 1);
        // 挑**最宽的那段空隙**、切在它中央。
        //
        // ⚠ 别退回成「取墨水最少的那一列」—— 真机上栽过：等分点附近既有词与词之间的空格
        //   （连续 6 列无墨），又有**字母 `c` 内部的那个缺口**（只有 1 列无墨），偏偏后者离
        //   等分点更近，于是切进了字母里，把那个 `c` 整个切没，`previous compact` 变成了
        //   `previousompact`。一列无墨不代表那里没字，一段无墨才代表。
        let mut best = target;
        if lo <= hi {
            let min_ink = (lo..=hi).map(at).min().unwrap_or(0);
            let mut best_key = (0u32, u32::MAX); // (空隙宽度→越大越好, 距等分点→越近越好)
            let mut p = lo;
            while p <= hi {
                if at(p) > min_ink {
                    p += 1;
                    continue;
                }
                let start = p;
                while p <= hi && at(p) <= min_ink {
                    p += 1;
                }
                let run = p - start; // 空隙段 [start, p)
                let center = start + run / 2;
                let key = (run, target.abs_diff(center));
                if key.0 > best_key.0 || (key.0 == best_key.0 && key.1 < best_key.1) {
                    best_key = key;
                    best = center;
                }
            }
        }
        // 切点必须严格递增，否则会切出空段
        if let Some(&prev) = cuts.last() {
            if best <= prev {
                best = prev + 1;
            }
        }
        cuts.push(best.min(len - 1));
    }

    let mut segs = Vec::with_capacity(cuts.len() + 1);
    let mut prev = 0u32;
    for c in cuts {
        segs.push((prev, c));
        prev = c;
    }
    segs.push((prev, len));
    segs
}

/// 每块四周补的背景边（像素）。
///
/// **不是可有可无的美化**：切口那一侧的字会**紧贴块的边缘**，检测端对贴边的字形容易漏检
/// 或只认半个 —— 真机上就这么丢过 `previous compact` 里的 `c`。补一圈背景色让字不贴边，
/// 实测同一块的识别分从 0.87 升到 0.92，`row.A file` 也恢复成 `row. A file`。
///
/// 代价是每块多占 2×PAD 像素的预算，所以 [`plan_tiles`] 切块时要先把这部分让出来。
pub const TILE_PAD: u32 = 8;

/// 给一张 w×h 的图排出送检瓦片：两个方向各自切到预算内，再给每块配放大倍数。
///
/// 放大倍数的规则也变了：**只在放大后仍不超预算时才放大**。旧规则
/// （`<700→3× / <1400→2× / else 1×`）只看图有多大、不看会不会被压回去，
/// 于是 1327 宽的图被放大到 2654 —— 白算一遍，还平添幻觉框。
///
/// 尺寸口径统一按**补边之后**算：切块预算先扣掉 `2×TILE_PAD`，倍数也按补边后的尺寸定，
/// 这样「补完边、放大完，仍不超预算」是恒成立的。
pub fn plan_tiles(w: u32, h: u32, ink_cols: &[u32], ink_rows: &[u32]) -> Vec<Tile> {
    let budget = OCR_LONG_SIDE_BUDGET.saturating_sub(TILE_PAD * 2).max(1);
    let xs = plan_cuts(w, budget, ink_cols);
    let ys = plan_cuts(h, budget, ink_rows);
    let mut out = Vec::with_capacity(xs.len() * ys.len());
    for &(y0, y1) in &ys {
        for &(x0, x1) in &xs {
            let (tw, th) = (x1 - x0, y1 - y0);
            let padded = (tw + TILE_PAD * 2).max(th + TILE_PAD * 2).max(1);
            let factor = (OCR_LONG_SIDE_BUDGET / padded).clamp(1, OCR_MAX_UPSCALE);
            out.push(Tile {
                x: x0,
                y: y0,
                w: tw,
                h: th,
                factor,
            });
        }
    }
    out
}

/// 估计整图的背景色：取亮度中位数附近那批像素的平均色。
///
/// 用来给瓦片补边 —— 补错颜色等于在字旁边画了一条杠，检测端会当成内容。
pub fn background_color(img: &RgbaImage) -> xcap::image::Rgba<u8> {
    let mut hist = [0u32; 256];
    let lum = |p: &xcap::image::Rgba<u8>| {
        ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000).min(255) as u8
    };
    for p in img.pixels() {
        hist[lum(p) as usize] += 1;
    }
    let half = (img.width() as u32 * img.height() as u32) / 2;
    let mut acc = 0u32;
    let mut bg = 0u8;
    for (v, c) in hist.iter().enumerate() {
        acc += c;
        if acc > half {
            bg = v as u8;
            break;
        }
    }
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for p in img.pixels() {
        if lum(p).abs_diff(bg) <= 2 {
            r += p[0] as u64;
            g += p[1] as u64;
            b += p[2] as u64;
            n += 1;
        }
    }
    if n == 0 {
        return xcap::image::Rgba([bg, bg, bg, 255]);
    }
    xcap::image::Rgba([(r / n) as u8, (g / n) as u8, (b / n) as u8, 255])
}

/// 逐列 / 逐行的「墨水量」剖面，供 [`plan_cuts`] 挑字缝用。
///
/// 墨水＝像素亮度与**背景亮度**之差；背景亮度取全图亮度的中位数（截图里绝大多数像素
/// 是背景，故中位数稳）。这样深色主题和浅色主题都成立，不必假设底色是黑是白。
pub fn ink_profiles(img: &RgbaImage) -> (Vec<u32>, Vec<u32>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut hist = [0u32; 256];
    let mut lum = vec![0u8; w * h];
    for (i, p) in img.pixels().enumerate() {
        let v = ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000).min(255) as u8;
        lum[i] = v;
        hist[v as usize] += 1;
    }
    let half = (w * h) as u32 / 2;
    let mut acc = 0u32;
    let mut bg = 0u8;
    for (v, c) in hist.iter().enumerate() {
        acc += c;
        if acc > half {
            bg = v as u8;
            break;
        }
    }
    // 阈值 24：滤掉抗锯齿/压缩噪声，只把真正的字算成墨水
    const INK_MIN_DELTA: u32 = 24;
    let mut cols = vec![0u32; w];
    let mut rows = vec![0u32; h];
    for y in 0..h {
        for x in 0..w {
            let d = (lum[y * w + x] as i32 - bg as i32).unsigned_abs();
            if d >= INK_MIN_DELTA {
                cols[x] += d;
                rows[y] += d;
            }
        }
    }
    (cols, rows)
}

/// 把整张截图切成送检瓦片：返回 (PNG 字节, 该块**内容原点**在原图中的 x, y, 放大倍数)。
///
/// ⚠ 返回的偏移是 **i32 且通常为负**：每块四周补了 [`TILE_PAD`] 的背景边，块内坐标要减掉
/// 这一圈才对得回原图，即 `原图坐标 = 块内坐标 / factor + offset`，其中 `offset = t.x - PAD`。
///
/// 解码失败时退化成「原图一整块、不放大、不补边」——识别质量会差，但绝不能因此整个用不了。
pub fn ocr_tiles(png: &[u8]) -> Vec<(Vec<u8>, i32, i32, u32)> {
    let img = match xcap::image::load_from_memory(png) {
        Ok(i) => i.to_rgba8(),
        Err(_) => return vec![(png.to_vec(), 0, 0, 1)],
    };
    let (w, h) = (img.width(), img.height());
    let (cols, rows) = ink_profiles(&img);
    let bg = background_color(&img);
    let tiles = plan_tiles(w, h, &cols, &rows);
    let mut out = Vec::with_capacity(tiles.len());
    for t in tiles {
        let sub = xcap::image::imageops::crop_imm(&img, t.x, t.y, t.w, t.h).to_image();
        let mut canvas = RgbaImage::from_pixel(t.w + TILE_PAD * 2, t.h + TILE_PAD * 2, bg);
        for (px, py, p) in sub.enumerate_pixels() {
            canvas.put_pixel(px + TILE_PAD, py + TILE_PAD, *p);
        }
        let (cw, ch) = (canvas.width(), canvas.height());
        let dynimg = DynamicImage::ImageRgba8(canvas);
        let dynimg = if t.factor > 1 {
            dynimg.resize(
                cw * t.factor,
                ch * t.factor,
                xcap::image::imageops::FilterType::Lanczos3,
            )
        } else {
            dynimg
        };
        let mut buf = Cursor::new(Vec::<u8>::new());
        if dynimg.write_to(&mut buf, ImageFormat::Png).is_ok() {
            out.push((
                buf.into_inner(),
                t.x as i32 - TILE_PAD as i32,
                t.y as i32 - TILE_PAD as i32,
                t.factor,
            ));
        }
    }
    if out.is_empty() {
        return vec![(png.to_vec(), 0, 0, 1)];
    }
    out
}

/// 生成一张纯白 PNG（PaddleOCR 预热用）。
pub fn blank_png(w: u32, h: u32) -> Vec<u8> {
    let img = RgbaImage::from_pixel(w, h, xcap::image::Rgba([255, 255, 255, 255]));
    let mut buf = Cursor::new(Vec::<u8>::new());
    let _ = DynamicImage::ImageRgba8(img).write_to(&mut buf, ImageFormat::Png);
    buf.into_inner()
}

/// 抓取指定屏（物理原点定位）的整屏原图。overlay 显示前调用。
pub fn capture_full(origin_x: i32, origin_y: i32) -> Result<RgbaImage, String> {
    let m = Monitor::from_point(origin_x, origin_y).map_err(|e| format!("选屏失败: {e}"))?;
    m.capture_image().map_err(|e| format!("截屏失败: {e}"))
}

/// 从整屏原图裁出逻辑坐标给定的矩形 → (PNG 字节, 裁剪宽 px, 裁剪高 px)。物理 = 逻辑 × scale。
pub fn crop_to_png(
    full: &RgbaImage,
    x_log: f64,
    y_log: f64,
    w_log: f64,
    h_log: f64,
    scale: f64,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (iw, ih) = (full.width(), full.height());
    let px = dpi_scale(x_log, scale).max(0) as u32;
    let py = dpi_scale(y_log, scale).max(0) as u32;
    let mut pw = dpi_scale(w_log, scale).max(1) as u32;
    let mut ph = dpi_scale(h_log, scale).max(1) as u32;
    pw = pw.min(iw.saturating_sub(px));
    ph = ph.min(ih.saturating_sub(py));
    if pw == 0 || ph == 0 {
        return Err("选区为空".into());
    }

    let cropped = xcap::image::imageops::crop_imm(full, px, py, pw, ph).to_image();
    let mut buf = Cursor::new(Vec::<u8>::new());
    DynamicImage::ImageRgba8(cropped)
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok((buf.into_inner(), pw, ph))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_scale_rounds() {
        assert_eq!(dpi_scale(100.0, 1.25), 125);
        assert_eq!(dpi_scale(0.0, 1.0), 0);
        assert_eq!(dpi_scale(10.4, 1.0), 10); // 四舍五入
        assert_eq!(dpi_scale(10.5, 1.0), 11);
        assert_eq!(dpi_scale(80.0, 1.5), 120);
    }

    // ---- 送检切块：这几条直接对应 2026-08-15 那次「大段漏译」的实测根因 ----

    /// 段必须首尾相接、覆盖整个区间、且都不超预算。切块一旦漏掉一段，
    /// 表现就是**那段原文没有任何译文块**——正是用户截图里第一行中间空着的那 400 px。
    fn assert_covers(segs: &[(u32, u32)], len: u32, budget: u32) {
        assert!(!segs.is_empty(), "不该切出空计划");
        assert_eq!(segs[0].0, 0, "必须从 0 开始");
        assert_eq!(segs.last().unwrap().1, len, "必须盖到 {len}");
        for w in segs.windows(2) {
            assert_eq!(w[0].1, w[1].0, "段之间不许有缝也不许重叠: {segs:?}");
        }
        for &(a, b) in segs {
            assert!(b > a, "不许有空段: {segs:?}");
            assert!(b - a <= budget, "段 {a}..{b} 超了预算 {budget}");
        }
    }

    #[test]
    fn cuts_short_input_stays_whole() {
        assert_eq!(plan_cuts(434, 960, &[]), vec![(0, 434)]);
        assert_eq!(plan_cuts(960, 960, &[]), vec![(0, 960)]);
        assert_eq!(plan_cuts(0, 960, &[]), Vec::new());
    }

    /// **回归钉**：1327×49 就是用户那张截图的裁剪尺寸。旧代码把它整张（还放大到 2654）
    /// 一次送检 ⇒ 中间整段静默漏检。现在必须切到预算内。
    #[test]
    fn cuts_the_real_failing_width_into_budget() {
        let segs = plan_cuts(1327, 960, &[]);
        assert_covers(&segs, 1327, 960);
        assert_eq!(segs.len(), 2, "1327 切两段就够: {segs:?}");
    }

    #[test]
    fn cuts_a_full_hd_screen_in_both_axes() {
        assert_covers(&plan_cuts(1920, 960, &[]), 1920, 960);
        assert_covers(&plan_cuts(1080, 960, &[]), 1080, 960);
    }

    /// 切口要挑在字缝里（切在段中央），这样每块内部都是完整的字，
    /// 就不需要重叠带 + 跨块文本去重那一套。
    #[test]
    fn cuts_snap_to_the_ink_gap() {
        let len = 1200u32;
        let mut ink = vec![100u32; len as usize];
        // 等分点应在 600 附近；在 638..642 挖一条 5 列宽的无墨缝
        for p in 638..643 {
            ink[p] = 0;
        }
        let segs = plan_cuts(len, 960, &ink);
        assert_covers(&segs, len, 960);
        assert_eq!(segs[0].1, 640, "该切在字缝**中央**(638+5/2)，实得 {segs:?}");
    }

    /// **回归钉（真机栽过）**：等分点附近同时存在
    /// ① 词与词之间的空格 —— 连续 6 列无墨
    /// ② 字母 `c` 内部的那个缺口 —— 只有 1 列无墨，**而且离等分点更近**
    ///
    /// 只挑「墨水最少的那一列」会切进字母里、把那个字母整个切没
    /// （`previous compact` → `previousompact`）。必须挑**最宽的那段**。
    #[test]
    fn cuts_prefer_the_widest_gap_over_a_nearer_single_column() {
        let len = 1200u32;
        let mut ink = vec![100u32; len as usize];
        for p in 570..576 {
            ink[p] = 0; // 词间空格：6 列
        }
        ink[600] = 0; // 字母内部缺口：1 列，正好落在等分点上
        let segs = plan_cuts(len, 960, &ink);
        assert_covers(&segs, len, 960);
        assert_eq!(
            segs[0].1, 573,
            "该切在 6 列宽那段的中央(570+3)，而不是离等分点更近的单列缺口，实得 {segs:?}"
        );
    }

    /// 字缝离等分点太远（超出搜索半径）就不许追过去，否则会把某段撑过预算。
    #[test]
    fn cuts_ignore_a_gap_outside_the_search_radius() {
        let len = 1200u32;
        let mut ink = vec![100u32; len as usize];
        ink[100] = 0; // 离等分点 ~500px，远超 CUT_SEARCH_RADIUS
        let segs = plan_cuts(len, 960, &ink);
        assert_covers(&segs, len, 960);
        assert!(segs[0].1 > 500, "不该被远处的缝带跑: {segs:?}");
    }

    /// 剖面比区间短（理论上不该发生）也要按 0 处理、不许 panic。
    #[test]
    fn cuts_tolerate_a_short_ink_profile() {
        let segs = plan_cuts(1327, 960, &[7, 7, 7]);
        assert_covers(&segs, 1327, 960);
    }

    /// 补边 + 放大之后仍不许超预算（尺寸口径一律按补边后算）。
    fn assert_within_budget(t: &Tile) {
        let padded = (t.w + TILE_PAD * 2).max(t.h + TILE_PAD * 2);
        assert!(
            padded * t.factor <= OCR_LONG_SIDE_BUDGET,
            "补边放大后超预算({padded}x{}): {t:?}",
            t.factor
        );
    }

    #[test]
    fn tiles_upscale_only_while_staying_under_budget() {
        // 小图：放大到不超预算为止（补边后 450，960/450 = 2）
        let t = plan_tiles(434, 49, &[], &[]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].factor, 2, "434 宽该放大 2 倍(补边后 900 仍在预算内)");

        // 极小图：倍数封顶
        assert_eq!(plan_tiles(100, 30, &[], &[])[0].factor, 3);

        // **旧规则的病灶**：1327 宽会被放大到 2654 —— 白算一遍还制造幻觉。
        // 现在切成两块，每块 ~664，放大 1 倍(补边后 ×2 已超预算)。
        let wide = plan_tiles(1327, 49, &[], &[]);
        assert_eq!(wide.len(), 2, "1327 该切两块: {wide:?}");
        for t in &wide {
            assert_eq!(t.factor, 1);
            assert_within_budget(t);
        }
    }

    /// 瓦片必须严丝合缝地铺满整张图：面积之和 == 原图面积，且无重叠。
    #[test]
    fn tiles_partition_the_image_exactly() {
        for (w, h) in [(1327u32, 49u32), (1920, 1080), (300, 200), (960, 960)] {
            let tiles = plan_tiles(w, h, &[], &[]);
            let area: u64 = tiles.iter().map(|t| t.w as u64 * t.h as u64).sum();
            assert_eq!(area, w as u64 * h as u64, "{w}x{h} 的瓦片没铺满/铺重了");
            for t in &tiles {
                assert!(t.x + t.w <= w && t.y + t.h <= h, "瓦片越界: {t:?}");
                assert_within_budget(t);
            }
        }
    }

    /// 送检的块必须**四周带背景边**，且偏移量把这一圈减了回去 ——
    /// 否则所有译文块都会整体偏移 8px，看着像"框歪了"。
    #[test]
    fn tiles_are_padded_and_the_offset_compensates_for_it() {
        let png = blank_png(100, 40);
        let tiles = ocr_tiles(&png);
        assert_eq!(tiles.len(), 1);
        let (bytes, ox, oy, f) = &tiles[0];
        assert_eq!((*ox, *oy), (-(TILE_PAD as i32), -(TILE_PAD as i32)), "偏移要抵掉补边");
        assert_eq!(*f, 3, "116x56 补边后可放大 3 倍");
        let img = xcap::image::load_from_memory(bytes).expect("送检的块必须是可解码的 PNG");
        assert_eq!(
            (img.width(), img.height()),
            ((100 + TILE_PAD * 2) * f, (40 + TILE_PAD * 2) * f),
            "尺寸应为 (原块 + 两侧补边) × 倍数"
        );
    }

    /// 诊断用：把 `ocr_tiles` 的实际产物写到磁盘，便于拿真 PaddleOCR 逐块比对。
    /// `$env:YIDIAN_DUMP_SRC=<图>; $env:YIDIAN_DUMP_DIR=<目录>; cargo test --lib -- --ignored --nocapture dump_tiles`
    #[test]
    #[ignore = "诊断用，需要 YIDIAN_DUMP_SRC / YIDIAN_DUMP_DIR"]
    fn dump_tiles_for_inspection() {
        let (src, dir) = match (
            std::env::var("YIDIAN_DUMP_SRC"),
            std::env::var("YIDIAN_DUMP_DIR"),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                println!("跳过：未设 YIDIAN_DUMP_SRC / YIDIAN_DUMP_DIR");
                return;
            }
        };
        let png = std::fs::read(&src).expect("读不到源图");
        let img = xcap::image::load_from_memory(&png).expect("解码失败").to_rgba8();
        let bg = background_color(&img);
        println!("源图 {}x{}  背景色 rgba{:?}", img.width(), img.height(), bg.0);
        for (i, (bytes, ox, oy, f)) in ocr_tiles(&png).into_iter().enumerate() {
            let p = format!("{dir}/dump_tile{i}.png");
            std::fs::write(&p, &bytes).expect("写不出瓦片");
            let t = xcap::image::load_from_memory(&bytes).unwrap();
            println!("tile{i}: {}x{} off=({ox},{oy}) f={f} -> {p}", t.width(), t.height());
        }
    }

    #[test]
    fn background_color_of_a_flat_image_is_that_color() {
        let img = RgbaImage::from_pixel(8, 8, xcap::image::Rgba([26, 27, 31, 255]));
        let c = background_color(&img);
        assert_eq!((c[0], c[1], c[2]), (26, 27, 31));
    }

    #[test]
    fn ink_profile_finds_the_bright_bar_on_a_dark_background() {
        // 深色底 + 一条竖亮条：列剖面应只在亮条那几列有墨
        let mut img = RgbaImage::from_pixel(20, 10, xcap::image::Rgba([26, 26, 30, 255]));
        for y in 0..10 {
            for x in 8..12 {
                img.put_pixel(x, y, xcap::image::Rgba([230, 230, 230, 255]));
            }
        }
        let (cols, rows) = ink_profiles(&img);
        assert_eq!(cols.len(), 20);
        assert_eq!(rows.len(), 10);
        assert_eq!(cols[0], 0, "空白列不该有墨");
        assert!(cols[9] > 0, "亮条那列该有墨");
        assert!(rows.iter().all(|&r| r > 0), "每一行都穿过亮条");
    }

    /// 浅色主题也得成立（背景取中位数，不假设底色是黑的）。
    #[test]
    fn ink_profile_works_on_a_light_background_too() {
        let mut img = RgbaImage::from_pixel(20, 10, xcap::image::Rgba([250, 250, 250, 255]));
        for y in 0..10 {
            for x in 8..12 {
                img.put_pixel(x, y, xcap::image::Rgba([20, 20, 20, 255]));
            }
        }
        let (cols, _) = ink_profiles(&img);
        assert_eq!(cols[0], 0);
        assert!(cols[9] > 0);
    }
}
