//! OCR：两条路。
//! - 快(默认)：Windows 自带 WinRT OCR，进程内、离线、几十 ms。需 MTA + spawn_blocking。
//! - 准：Ollama 视觉模型 qwen3-vl:4b，慢几秒但小字/花字/多语言更强。复用 no_proxy 直连。
//! 编排：prefer_accurate 直接走准；否则先试快，快失败/空则自动回退准。

use base64::{engine::general_purpose::STANDARD, Engine as _};

pub mod paddle;

/// 一行文字 + 它在图中的像素包围盒（用于图像内嵌翻译）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LineBox {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 带坐标的逐行识别（图像内嵌翻译用）。仅 Windows WinRT OCR 提供坐标；
/// 失败/非 Windows 返回 Err，调用方回退到纯文本弹卡。
pub async fn recognize_lines(png: &[u8]) -> Result<Vec<LineBox>, String> {
    #[cfg(windows)]
    {
        // 放大后再 OCR：Windows OCR 对小字/低对比识别率显著提升；识别后坐标除回原尺度
        let (up_png, factor) = upscale_png(png, 2);
        let mut lines = winrt::recognize_lines(&up_png).await?;
        if factor > 1 {
            let f = factor as f64;
            for l in &mut lines {
                l.x /= f;
                l.y /= f;
                l.w /= f;
                l.h /= f;
            }
        }
        Ok(lines)
    }
    #[cfg(not(windows))]
    {
        let _ = png;
        Err("非 Windows 无坐标 OCR".into())
    }
}

/// 放大 PNG（返回放大后的 PNG 字节 + 实际倍数）。过大则不放大（返回原图, 1）。
#[cfg(windows)]
fn upscale_png(png: &[u8], factor: u32) -> (Vec<u8>, u32) {
    use std::io::Cursor;
    use xcap::image::{imageops::FilterType, ImageFormat};
    let img = match xcap::image::load_from_memory(png) {
        Ok(i) => i,
        Err(_) => return (png.to_vec(), 1),
    };
    let (w, h) = (img.width(), img.height());
    // 放大后最大边超 4000 就不放（Windows OCR 有尺寸上限、也防爆内存）
    let f = if factor > 1 && w.max(h) * factor <= 4000 {
        factor
    } else {
        1
    };
    if f == 1 {
        return (png.to_vec(), 1);
    }
    let up = img.resize(w * f, h * f, FilterType::Triangle);
    let mut buf = Cursor::new(Vec::<u8>::new());
    if up.write_to(&mut buf, ImageFormat::Png).is_err() {
        return (png.to_vec(), 1);
    }
    (buf.into_inner(), f)
}

/// 识别一张 PNG（字节）→ 文本。prefer_accurate=true 直接用 qwen3-vl。
pub async fn recognize(png: Vec<u8>, prefer_accurate: bool) -> Result<String, String> {
    if !prefer_accurate {
        #[cfg(windows)]
        {
            if let Ok(t) = winrt::ocr_png_bytes(&png).await {
                if !t.trim().is_empty() {
                    return Ok(t);
                }
            }
            // 快失败或空 → 回退到 qwen3-vl
        }
    }
    let b64 = STANDARD.encode(&png);
    ocr_vlm(&b64).await
}

/// 探测本机已装的 WinRT OCR 语言（如 en-US / zh-Hans-CN）。非 Windows 返回空。
pub fn available_languages() -> Vec<String> {
    #[cfg(windows)]
    {
        winrt::available_languages().unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// qwen3-vl:4b 识别图中文字（只取原文，不翻译；翻译交给现有管线）。
async fn ocr_vlm(png_b64: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": "qwen3-vl:4b",
        "stream": false,
        "options": { "temperature": 0 },
        "messages": [{
            "role": "user",
            "content": "Output ONLY the text visible in this image, preserving line breaks and reading order. No commentary, no translation, no quotes.",
            "images": [png_b64]
        }]
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))?;
    let resp = client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("连接本地视觉模型失败（Ollama/qwen3-vl 是否就绪？）: {e}"))?;
    let txt = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
    let content = crate::engine::ollama::parse_chat_content(&txt)?;
    Ok(content.trim().to_string())
}

// ---------------------------------------------------------------------------
// Windows WinRT OCR
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod winrt {
    use std::sync::Once;
    use windows::core::Result;
    use windows::Graphics::Imaging::{BitmapDecoder, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::Com::CoIncrementMTAUsage;

    /// 进程级隐式 MTA：让在 tokio 线程 .await WinRT 异步操作合法（WebView2 主线程是 STA）。
    fn ensure_mta() {
        static INIT: Once = Once::new();
        INIT.call_once(|| unsafe {
            let _ = CoIncrementMTAUsage();
        });
    }

    async fn recognize_inner(png: &[u8]) -> Result<String> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(png)?;
        writer.StoreAsync()?.await?;
        writer.DetachStream()?;
        stream.Seek(0)?;

        let decoder = BitmapDecoder::CreateAsync(&stream)?.await?;
        let bitmap: SoftwareBitmap = decoder.GetSoftwareBitmapAsync()?.await?;

        let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
        let result = engine.RecognizeAsync(&bitmap)?.await?;
        Ok(result.Text()?.to_string())
    }

    /// PNG 字节 → 文本（隐式 MTA + await WinRT）。
    pub async fn ocr_png_bytes(png: &[u8]) -> std::result::Result<String, String> {
        ensure_mta();
        recognize_inner(png)
            .await
            .map_err(|e| format!("WinRT OCR 失败: {e}"))
    }

    pub fn available_languages() -> std::result::Result<Vec<String>, String> {
        ensure_mta();
        let mut out = Vec::new();
        for lang in OcrEngine::AvailableRecognizerLanguages().map_err(|e| e.to_string())? {
            if let Ok(tag) = lang.LanguageTag() {
                out.push(tag.to_string());
            }
        }
        Ok(out)
    }

    pub async fn recognize_lines(png: &[u8]) -> std::result::Result<Vec<super::LineBox>, String> {
        ensure_mta();
        lines_inner(png)
            .await
            .map_err(|e| format!("WinRT OCR(逐行) 失败: {e}"))
    }

    async fn lines_inner(png: &[u8]) -> Result<Vec<super::LineBox>> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(png)?;
        writer.StoreAsync()?.await?;
        writer.DetachStream()?;
        stream.Seek(0)?;
        let decoder = BitmapDecoder::CreateAsync(&stream)?.await?;
        let bitmap: SoftwareBitmap = decoder.GetSoftwareBitmapAsync()?.await?;
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
        let result = engine.RecognizeAsync(&bitmap)?.await?;

        let mut out = Vec::new();
        for line in result.Lines()? {
            let text = line.Text()?.to_string();
            if text.trim().is_empty() {
                continue;
            }
            let (mut minx, mut miny, mut maxx, mut maxy) =
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for word in line.Words()? {
                let r = word.BoundingRect()?; // Foundation::Rect (f32)
                let (x, y, w, h) = (r.X as f64, r.Y as f64, r.Width as f64, r.Height as f64);
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x + w);
                maxy = maxy.max(y + h);
            }
            if maxx > minx {
                out.push(super::LineBox {
                    text,
                    x: minx,
                    y: miny,
                    w: maxx - minx,
                    h: maxy - miny,
                });
            }
        }
        Ok(out)
    }
}
