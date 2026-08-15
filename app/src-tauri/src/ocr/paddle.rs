//! PaddleOCR-json 持久子进程封装：本地 OCR，返回每行文字 + 像素坐标框（图像内嵌翻译用）。
//! 协议：stdin 发 `{"image_base64":"..."}\n`；stdout 回 `{"code":100,"data":[{box:[[x,y]*4],text,score}]}`。
//! code 100=有结果、101=无文字、其它=错误。子进程模型冷载 ~2-3s，之后每张 ~百毫秒。
//!
//! ⚠ **生命周期是这个文件的重点，别再退回去**（2026-08-13 由实测事故修）：
//!
//! - **`std::process::Child` 没有实现 `Drop`** —— 标准库文档明写「不主动确保其退出的话，即使
//!   `Child` 句柄离开作用域，子进程仍会继续运行」。原先这里的字段写成 `_child`（下划线＝故意
//!   不用），等于从来没杀过它 ⇒ 译点**每启动一次就漏一个** PaddleOCR-json。实测机器上同时躺着
//!   三个孤儿，合计 **5.5 GB 提交内存**；其中一个因父进程死后 stdin 断开、读循环退化成忙等，
//!   在 159.7 小时里烧掉 **153 小时 CPU**（≈96% 满载一个核，连烧六天）。
//! - 故这里设两层保险：
//!   ① **作业对象 `KILL_ON_JOB_CLOSE`** —— 父进程无论怎么死（正常退出 / 任务管理器强杀 /
//!      崩溃），句柄随进程销毁而关闭，内核**自动连带收掉**作业里的子进程。这是**唯一对强杀
//!      有效**的机制，也是本文件最不能删的一行。
//!   ② **`impl Drop`** —— 显式 `kill()` + `wait()`，正常路径（空闲超时、应用退出）干净收尾。
//! - 另有一条**我们改不了**的：子进程每做一次真实识别就涨一大截且不释放（实测一张 1920×1080、
//!   25 行 → 从 635 MB 涨到 2314 MB，+1.7 GB）。这是 PaddleOCR-json 自身的问题，调用方唯一能做的
//!   是**空闲超时把它整个退掉**。`last_used()` 就是给外面的看门狗判空闲用的。
//! - ⚠ 量它必须看**提交内存（Private Bytes）**，不是任务管理器默认那列工作集 —— 上面那个
//!   2314 MB 的进程，工作集只显示 54 MB。
//! - ⚠ **已证伪**：曾以为膨胀是 MKL/OpenMP 线程池撑的。实测把 `OMP_NUM_THREADS`/`MKL_NUM_THREADS`
//!   设成 1 或 2，启动占用 599.9 MB → 599.6 MB，**几乎没变**。别再走调线程数这条路。

use super::LineBox;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

pub struct Paddle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    last_used: Instant,
    /// 作业对象句柄。必须与子进程同生命周期：它一关，子进程就被内核杀掉。
    ///
    /// ⚠ 是 `Option`：**兜底手段不许变成单点故障**。万一作业对象建不出来或加不进去
    /// （理论上只在资源耗尽、或 Win7 那种不支持嵌套作业的老系统上发生），也只是退化成
    /// "只有 Drop 这一层保护"，绝不能让截图翻译整个用不了。失败原因记在 `job_note` 里。
    #[cfg(windows)]
    job: Option<win::Job>,
    job_note: Option<String>,
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

        // 先入作业对象再做别的：万一后面哪一步出错提前 return，Job 的 Drop 也会把它收掉。
        // 失败只降级、不阻断（理由见 `job` 字段注释）。
        #[cfg(windows)]
        let (job, job_note) = match win::Job::kill_on_close().and_then(|j| j.adopt(&child).map(|_| j))
        {
            Ok(j) => (Some(j), None),
            Err(e) => (
                None,
                Some(format!("⚠ 作业对象未生效({e})：父进程被强杀时子进程不会被自动收掉，只剩正常退出这一层保护")),
            ),
        };
        #[cfg(not(windows))]
        let job_note = None;

        let stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                return Err("PaddleOCR stdin 缺失".into());
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => BufReader::new(s),
            None => {
                let _ = child.kill();
                return Err("PaddleOCR stdout 缺失".into());
            }
        };
        Ok(Paddle {
            child,
            stdin,
            stdout,
            last_used: Instant::now(),
            #[cfg(windows)]
            job,
            job_note,
        })
    }

    /// 距上次识别过了多少秒。外面的空闲看门狗据此决定是否退掉子进程。
    pub fn idle_secs(&self) -> u64 {
        self.last_used.elapsed().as_secs()
    }

    /// 作业对象没挂上时的说明（正常情况是 None）。调用方应把它记进日志——
    /// 保护降级了必须留痕，否则下次又要靠猜。
    pub fn job_note(&self) -> Option<&str> {
        self.job_note.as_deref()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// OCR 一张图（base64 PNG）→ 每行文字 + 像素框。
    pub fn ocr_base64(&mut self, b64: &str) -> Result<Vec<LineBox>, String> {
        self.last_used = Instant::now();
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
                self.last_used = Instant::now(); // 长图识别可能跑很久，按完成时刻算空闲
                return parse(t);
            }
        }
    }
}

impl Drop for Paddle {
    fn drop(&mut self) {
        // 正常路径显式收尾；强杀路径由作业对象兜底（见文件头）。
        let _ = self.child.kill();
        let _ = self.child.wait(); // 收句柄，别留僵尸
        #[cfg(windows)]
        if let Some(j) = self.job.as_mut() {
            j.close();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// 作业对象句柄。
    ///
    /// 存成 `isize` 而不是 `HANDLE`：`HANDLE` 内含裸指针、不是 `Send`/`Sync`，
    /// 而 `Paddle` 要放进 `AppState` 的 `Mutex` 里跨线程用。
    pub struct Job(isize);

    impl Job {
        /// 建一个「句柄一关就杀光成员」的作业对象。
        pub fn kill_on_close() -> Result<Self, String> {
            unsafe {
                let h = CreateJobObjectW(None, PCWSTR::null())
                    .map_err(|e| format!("创建作业对象失败: {e}"))?;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if let Err(e) = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    core::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) {
                    let _ = CloseHandle(h);
                    return Err(format!("设置作业对象限制失败: {e}"));
                }
                Ok(Job(h.0 as isize))
            }
        }

        pub fn adopt(&self, child: &std::process::Child) -> Result<(), String> {
            unsafe {
                AssignProcessToJobObject(
                    HANDLE(self.0 as *mut core::ffi::c_void),
                    HANDLE(child.as_raw_handle()),
                )
                .map_err(|e| format!("子进程加入作业对象失败: {e}"))
            }
        }

        /// 关闭句柄 ⇒ 作业里还活着的成员会被内核杀掉。
        pub fn close(&mut self) {
            if self.0 != 0 {
                unsafe {
                    let _ = CloseHandle(HANDLE(self.0 as *mut core::ffi::c_void));
                }
                self.0 = 0;
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            self.close();
        }
    }
}

/// 收拾上一次运行漏下的 PaddleOCR-json 孤儿，返回被清掉的 pid。
///
/// 为什么可以直接杀：`tauri-plugin-single-instance` 保证同时只有一个译点在跑，所以
/// 「跑着我们这个 exe 路径」的 PaddleOCR-json 一律是遗留（本次的子进程要等到真正用截图翻译
/// 时才懒启动，此刻还不存在）。按**完整路径**比对而不是按进程名，免得误杀别的软件带的同名程序。
#[cfg(windows)]
pub fn kill_orphans(exe: &Path) -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    let target = match exe.canonicalize() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let want_name = match exe.file_name().and_then(|s| s.to_str()) {
        Some(s) => s.to_ascii_lowercase(),
        None => return Vec::new(),
    };
    let me = std::process::id();
    let mut killed = Vec::new();

    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return killed,
        };
        let mut pe = PROCESSENTRY32W {
            dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let pid = pe.th32ProcessID;
                // 先按进程名粗筛，名字对得上才去开句柄查完整路径（开句柄有成本，也少踩权限）
                let name = String::from_utf16_lossy(
                    &pe.szExeFile[..pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)],
                )
                .to_ascii_lowercase();
                if pid != me && name == want_name {
                    if let Ok(h) = OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                        false,
                        pid,
                    ) {
                        let mut buf = [0u16; 32768];
                        let mut len = buf.len() as u32;
                        let ok = QueryFullProcessImageNameW(
                            h,
                            PROCESS_NAME_WIN32,
                            windows::core::PWSTR(buf.as_mut_ptr()),
                            &mut len,
                        )
                        .is_ok();
                        if ok {
                            let path = String::from_utf16_lossy(&buf[..len as usize]);
                            let same = Path::new(&path)
                                .canonicalize()
                                .map(|p| p == target)
                                .unwrap_or(false);
                            if same && TerminateProcess(h, 1).is_ok() {
                                killed.push(pid);
                            }
                        }
                        let _ = CloseHandle(h);
                    }
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    killed
}

#[cfg(not(windows))]
pub fn kill_orphans(_exe: &Path) -> Vec<u32> {
    Vec::new()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// 下面两个真跑测试都会动「系统里所有跑着这个 exe 的进程」，并行跑会互相把对方的
    /// 子进程杀掉 ⇒ 必须串行。用锁而不是靠运行时记得加 `--test-threads=1`。
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 找 PaddleOCR-json.exe：环境变量 → 仓库 resources → 装机目录。
    fn find_exe() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("YIDIAN_PADDLE_EXE") {
            let p = std::path::PathBuf::from(p);
            if p.exists() {
                return Some(p);
            }
        }
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/paddleocr/PaddleOCR-json.exe");
        if repo.exists() {
            return Some(repo);
        }
        let installed = std::env::var("LOCALAPPDATA").ok().map(|d| {
            std::path::PathBuf::from(d).join("yidian/paddleocr/PaddleOCR-json.exe")
        })?;
        installed.exists().then_some(installed)
    }

    fn pid_alive(pid: u32) -> bool {
        unsafe {
            let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(_) => return false,
            };
            let mut code = 0u32;
            let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
            let _ = CloseHandle(h);
            alive
        }
    }

    fn wait_gone(pid: u32) -> bool {
        for _ in 0..50 {
            if !pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// 这就是 2026-08-13 那个泄漏的复现：0.4.1 及以前，`Paddle` 被丢弃后子进程照样活着。
    #[test]
    #[ignore = "要真起 PaddleOCR-json 子进程；跑：cargo test --lib -- --ignored"]
    fn dropping_paddle_kills_the_child() {
        let _g = serial();
        let exe = find_exe().expect("找不到 PaddleOCR-json.exe（可设 YIDIAN_PADDLE_EXE）");
        let p = Paddle::start(&exe).expect("启动 PaddleOCR 失败");
        let pid = p.pid();
        assert!(pid_alive(pid), "刚起的子进程 pid={pid} 应该活着");
        drop(p);
        assert!(
            wait_gone(pid),
            "Paddle 被 drop 后子进程 pid={pid} 仍活着 —— 这正是旧版每启动一次漏一个的原因"
        );
    }

    /// 作业对象那条防线：**不显式 kill**，只关掉作业句柄，子进程也必须死。
    ///
    /// 这是三层防线里唯一能覆盖「父进程被强杀 / 崩溃」的一条 —— 那种情况下轮不到 `Drop` 跑，
    /// 全靠进程销毁时句柄自动关闭、内核连带收掉作业成员。这里手工关句柄来验证同一条内核语义。
    #[test]
    #[ignore = "要真起 PaddleOCR-json 子进程；跑：cargo test --lib -- --ignored"]
    fn closing_the_job_kills_the_child_without_any_explicit_kill() {
        let _g = serial();
        let exe = find_exe().expect("找不到 PaddleOCR-json.exe（可设 YIDIAN_PADDLE_EXE）");
        let mut cmd = Command::new(&exe);
        cmd.current_dir(exe.parent().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let child = cmd.spawn().expect("起子进程失败");
        let pid = child.id();

        let mut job = win::Job::kill_on_close().expect("建作业对象失败");
        job.adopt(&child).expect("加入作业对象失败");
        assert!(pid_alive(pid), "子进程 pid={pid} 应该活着");

        std::mem::forget(child); // 故意丢掉句柄且不 kill：只留作业对象这一条路
        job.close(); // ← 唯一的动作
        assert!(
            wait_gone(pid),
            "只关作业句柄之后子进程 pid={pid} 仍活着 —— KILL_ON_JOB_CLOSE 没生效，强杀场景就没有兜底了"
        );
    }

    /// 清理孤儿：手工绕开 `Paddle`（因而没有作业对象保护）起一个，再让 kill_orphans 收掉它。
    #[test]
    #[ignore = "要真起 PaddleOCR-json 子进程；跑：cargo test --lib -- --ignored"]
    fn kill_orphans_reaps_a_leaked_child() {
        let _g = serial();
        let exe = find_exe().expect("找不到 PaddleOCR-json.exe（可设 YIDIAN_PADDLE_EXE）");
        let mut orphan = Command::new(&exe);
        orphan
            .current_dir(exe.parent().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        {
            use std::os::windows::process::CommandExt;
            orphan.creation_flags(0x0800_0000);
        }
        let child = orphan.spawn().expect("起孤儿失败");
        let pid = child.id();
        std::mem::forget(child); // 故意不留句柄，模拟"父进程已经没了"
        assert!(pid_alive(pid), "孤儿 pid={pid} 应该活着");

        let killed = kill_orphans(&exe);
        assert!(killed.contains(&pid), "kill_orphans 应当收掉 pid={pid}，实得 {killed:?}");
        assert!(wait_gone(pid), "kill_orphans 返回了 pid={pid} 但它还活着");
    }

    /// **真跑端到端复现回归**：一张宽截图条走完「切块 → 送检 → 版面后处理」，整句不许丢字。
    ///
    /// 这一条钉的就是 2026-08-15 那个「截图翻译对照不上」的根因：同一张 1327×49 的条，
    /// 旧做法（整张放大到 2654 后一次送检）会把中间一整段 **静默漏掉**，屏幕上那段原文
    /// 就没有任何译文块。切块之后必须一个词不少。
    ///
    /// 跑：
    /// ```text
    /// $env:YIDIAN_OCR_FIXTURE="<一张宽截图条.png>"
    /// cargo test --lib -- --ignored --nocapture wide_strip
    /// ```
    #[test]
    #[ignore = "要真起 PaddleOCR-json 子进程 + 一张宽截图 fixture(环境变量 YIDIAN_OCR_FIXTURE)"]
    fn wide_strip_keeps_every_word() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let _g = serial();
        let fixture = match std::env::var("YIDIAN_OCR_FIXTURE") {
            Ok(p) => p,
            Err(_) => {
                println!("跳过：未设 YIDIAN_OCR_FIXTURE");
                return;
            }
        };
        let png = std::fs::read(&fixture).expect("读不到 fixture");
        let exe = find_exe().expect("找不到 PaddleOCR-json.exe（可设 YIDIAN_PADDLE_EXE）");
        let mut p = Paddle::start(&exe).expect("启动 PaddleOCR 失败");

        let tiles = crate::capture::ocr_tiles(&png);
        println!("切成 {} 块", tiles.len());
        assert!(tiles.len() >= 2, "这么宽的条必须切块，实得 {} 块", tiles.len());

        let mut all = Vec::new();
        for (tile, ox, oy, f) in tiles {
            let b64 = STANDARD.encode(&tile);
            let mut lines = p.ocr_base64(&b64).expect("识别失败");
            let fd = f.max(1) as f64;
            for l in &mut lines {
                l.x = l.x / fd + ox as f64;
                l.y = l.y / fd + oy as f64;
                l.w /= fd;
                l.h /= fd;
            }
            all.append(&mut lines);
        }
        println!("原始 {} 框", all.len());
        let (kept, dropped) = crate::ocr::layout::drop_junk(all);
        for d in &dropped {
            println!("  丢弃[{}] score={:.2} 「{}」", d.reason, d.line.score, d.line.text);
        }
        let lines = crate::ocr::layout::group_lines(kept);
        for (i, l) in lines.iter().enumerate() {
            println!("  行[{i}] x={:.0} y={:.0} w={:.0} s={:.2} 「{}」", l.x, l.y, l.w, l.score, l.text);
        }
        let text = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
        println!("合并后：{text}");
        // ⚠ 断言**对空格不敏感**。这条钉的是「整段文字被静默漏掉」，不是识别精度：
        //   识别层对很长的一行会把字挤在一起、吞掉空格（`A file` → `Afile`），那是另一码事，
        //   拿它当断言只会让这条回归测试变得脆弱、动不动就红。
        let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        for w in ["3timesinarow", "afilebeingreador", "atooloutputis"] {
            assert!(flat.contains(w), "丢了「{w}」—— 这正是旧做法的病症。实得：{text}");
        }
    }

    /// 路径不存在时不许乱杀，也不许 panic。
    #[test]
    fn kill_orphans_on_bogus_path_is_a_noop() {
        let bogus = std::path::Path::new(r"C:\definitely\not\here\PaddleOCR-json.exe");
        assert!(kill_orphans(bogus).is_empty());
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
        // ⚠ `score` 缺失时按 1.0 算（＝不过滤），不能按 0：宁可放过幻觉，也不能因为
        //   哪天上游改了字段名就把所有文字全滤光、截图翻译整个变成"没认出文字"。
        let score = item.get("score").and_then(|s| s.as_f64()).unwrap_or(1.0);
        if maxx > minx && maxy > miny {
            out.push(LineBox {
                text,
                x: minx,
                y: miny,
                w: maxx - minx,
                h: maxy - miny,
                score,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// 真实响应样本（字段名与结构照抄 PaddleOCR-json v1.4.1 的输出）。
    #[test]
    fn parse_reads_box_and_score() {
        let j = r#"{"code":100,"data":[
            {"box":[[63,18],[102,18],[102,39],[63,39]],"score":0.86,"text":"Aut"},
            {"box":[[608,21],[641,21],[641,30],[608,30]],"score":0.44,"text":"E"}
        ]}"#;
        let v = parse(j).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].text, "Aut");
        assert_eq!((v[0].x, v[0].y, v[0].w, v[0].h), (63.0, 18.0, 39.0, 21.0));
        assert!((v[0].score - 0.86).abs() < 1e-9);
        assert!((v[1].score - 0.44).abs() < 1e-9, "低分也要如实带上来，交给版面层去滤");
    }

    /// 上游哪天不给 score 了也不能把文字全滤光 —— 缺失按 1.0（不过滤）算。
    #[test]
    fn parse_defaults_missing_score_to_one() {
        let j = r#"{"code":100,"data":[{"box":[[0,0],[10,0],[10,5],[0,5]],"text":"hi"}]}"#;
        let v = parse(j).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].score, 1.0);
    }

    #[test]
    fn parse_no_text_is_empty_not_error() {
        let v = parse(r#"{"code":101,"data":"No text found in image."}"#).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn parse_error_code_is_an_error() {
        assert!(parse(r#"{"code":200,"data":"boom"}"#).is_err());
    }
}
