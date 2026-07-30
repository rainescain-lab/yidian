//! PaddleOCR-json 持久子进程封装：本地 OCR，返回每行文字 + 像素坐标框（图像内嵌翻译用）。
//! 协议：stdin 发 `{"image_base64":"..."}\n`；stdout 回 `{"code":100,"data":[{box:[[x,y]*4],text,score}]}`。
//! code 100=有结果、101=无文字、其它=错误。子进程只启一次（模型冷载 ~2-3s），之后每张 ~百毫秒。

use super::LineBox;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Paddle {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Paddle {
    pub fn start(exe: &Path) -> Result<Self, String> {
        let dir = exe.parent().ok_or("PaddleOCR 路径异常")?;
        let mut cmd = Command::new(exe);
        cmd.current_dir(dir) // 模型是相对路径，需以 exe 目录为 cwd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()); // 初始化信息走 stderr，丢弃
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW，不弹黑窗
        }
        let mut child = cmd.spawn().map_err(|e| format!("启动 PaddleOCR 失败: {e}"))?;
        let stdin = child.stdin.take().ok_or("PaddleOCR stdin 缺失")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("PaddleOCR stdout 缺失")?);
        Ok(Paddle {
            _child: child,
            stdin,
            stdout,
        })
    }

    /// OCR 一张图（base64 PNG）→ 每行文字 + 像素框。
    pub fn ocr_base64(&mut self, b64: &str) -> Result<Vec<LineBox>, String> {
        let cmd = format!("{{\"image_base64\":\"{b64}\"}}\n");
        self.stdin
            .write_all(cmd.as_bytes())
            .map_err(|e| format!("PaddleOCR 写入失败: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("PaddleOCR flush 失败: {e}"))?;

        // 读到第一行含 "code" 的 JSON 结果（跳过任何前导行）
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| format!("PaddleOCR 读取失败: {e}"))?;
            if n == 0 {
                return Err("PaddleOCR 无响应（子进程退出）".into());
            }
            let t = line.trim();
            if t.starts_with('{') && t.contains("\"code\"") {
                return parse(t);
            }
        }
    }
}

fn parse(json: &str) -> Result<Vec<LineBox>, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("PaddleOCR 结果解析失败: {e}"))?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code == 101 {
        return Ok(Vec::new()); // 无文字
    }
    if code != 100 {
        let msg = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
        return Err(format!("PaddleOCR 错误 code={code} {msg}"));
    }
    let data = v.get("data").and_then(|d| d.as_array()).ok_or("PaddleOCR 结果无 data")?;
    let mut out = Vec::new();
    for item in data {
        let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if text.trim().is_empty() {
            continue;
        }
        let pts = match item.get("box").and_then(|b| b.as_array()) {
            Some(p) => p,
            None => continue,
        };
        let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for pt in pts {
            if let Some(xy) = pt.as_array() {
                let x = xy.first().and_then(|n| n.as_f64()).unwrap_or(0.0);
                let y = xy.get(1).and_then(|n| n.as_f64()).unwrap_or(0.0);
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
        if maxx > minx && maxy > miny {
            out.push(LineBox {
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
