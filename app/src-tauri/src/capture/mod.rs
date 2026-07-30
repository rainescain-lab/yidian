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

/// 小图放大后再 OCR（提升模糊/小字/少字的识别率）。返回 (放大后 PNG, 倍数)。
/// 仅当原图较小时放大(大图已够清 + 防爆)；坐标需按倍数缩回原图。
pub fn upscale_for_ocr(png: &[u8]) -> (Vec<u8>, u32) {
    let img = match xcap::image::load_from_memory(png) {
        Ok(i) => i,
        Err(_) => return (png.to_vec(), 1),
    };
    let (w, h) = (img.width(), img.height());
    let factor: u32 = if w.max(h) < 700 {
        3
    } else if w.max(h) < 1400 {
        2
    } else {
        1
    };
    if factor == 1 {
        return (png.to_vec(), 1);
    }
    let up = img.resize(
        w * factor,
        h * factor,
        xcap::image::imageops::FilterType::Lanczos3,
    );
    let mut buf = Cursor::new(Vec::<u8>::new());
    if up.write_to(&mut buf, ImageFormat::Png).is_err() {
        return (png.to_vec(), 1);
    }
    (buf.into_inner(), factor)
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
}
