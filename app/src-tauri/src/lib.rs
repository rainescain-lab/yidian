mod capture;
mod db;
mod dict;
mod engine;
mod ocr;
mod selection;

use std::collections::HashMap;
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rusqlite::Connection;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl,
    WebviewWindowBuilder,
};

pub struct AppState {
    db: Mutex<Connection>,
    dicts: Mutex<dict::DictCache>,
    screenshot: Mutex<Option<capture::Screenshot>>,
    popup_payload: Mutex<Option<PopupPayload>>,
    shot_payload: Mutex<Option<ShotPayload>>,
    paddle: Mutex<Option<ocr::paddle::Paddle>>,
}

#[derive(Clone, serde::Serialize)]
pub struct PopupPayload {
    src: String,
    dst: String,
    engine: String,
}

/// 图像内嵌翻译结果：整块截图 + 每行原文/译文及其在图中的像素框。
#[derive(Clone, serde::Serialize)]
pub struct ShotLine {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    src: String,
    dst: String,
}

#[derive(Clone, serde::Serialize)]
pub struct ShotPayload {
    image: String, // data:image/png;base64,...
    width: u32,    // 裁剪像素宽（坐标系）
    height: u32,
    disp_w: f64, // 图的 CSS 显示宽(=裁剪物理宽/scale)，前端按此摆图不拉伸
    lines: Vec<ShotLine>,
}

// ---------------------------------------------------------------------------
// 翻译
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct TranslateOut {
    text: String,
    src_lang: String,
    tgt_lang: String,
    engine: String,
    history_id: i64,
    favorite: bool,
}

/// 读在线兜底次序（settings.online_order，逗号分隔）；缺省 bing→google。
fn read_online_order(state: &AppState) -> Vec<String> {
    let default = || vec!["bing".to_string(), "google".to_string()];
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(_) => return default(),
    };
    match db::get_setting(&conn, "online_order") {
        Ok(Some(s)) => {
            let v: Vec<String> = s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            if v.is_empty() {
                default()
            } else {
                v
            }
        }
        _ => default(),
    }
}

/// 写历史并取回 (id, favorite)。失败静默（不影响翻译主流程）。
fn record_history(
    state: &AppState,
    source: &str,
    translated: &str,
    src: &str,
    tgt: &str,
    engine: &str,
) -> (i64, bool) {
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(_) => return (0, false),
    };
    if db::add_history(&conn, source, translated, src, tgt, engine).is_err() {
        return (0, false);
    }
    conn.query_row(
        "SELECT id, favorite FROM history WHERE source_text = ?1",
        rusqlite::params![source],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)),
    )
    .unwrap_or((0, false))
}

/// 统一翻译：按 engine(local|online) 路由，返回 (译文, 源语言, 目标语言, 引擎名)。
async fn run_translate(
    state: &AppState,
    text: &str,
    engine: &str,
) -> Result<(String, &'static str, &'static str, String), String> {
    let (src, tgt) = engine::lang::default_direction(text);
    let (translated, label) = if engine == "online" {
        let order = read_online_order(state);
        match engine::online::translate_online(text, tgt, &order).await {
            Ok(v) => v,
            // 在线失败(网络卡/超时/被墙) → 本地 Qwen 兜底
            Err(e) => match engine::ollama::translate_local(text).await {
                Ok(t) => (t, "本地(兜底)".to_string()),
                Err(e2) => return Err(format!("在线失败({e})、本地兜底也失败({e2})")),
            },
        }
    } else {
        (engine::ollama::translate_local(text).await?, "本地".to_string())
    };
    Ok((translated, src, tgt, label))
}

#[tauri::command]
async fn translate(
    state: State<'_, AppState>,
    text: String,
    engine: String,
) -> Result<TranslateOut, String> {
    if text.trim().is_empty() {
        return Ok(TranslateOut {
            text: String::new(),
            src_lang: String::new(),
            tgt_lang: String::new(),
            engine: String::new(),
            history_id: 0,
            favorite: false,
        });
    }

    let (translated, src, tgt, engine_label) =
        run_translate(state.inner(), &text, &engine).await?;

    let (history_id, favorite) =
        record_history(state.inner(), &text, &translated, src, tgt, &engine_label);

    Ok(TranslateOut {
        text: translated,
        src_lang: src.to_string(),
        tgt_lang: tgt.to_string(),
        engine: engine_label,
        history_id,
        favorite,
    })
}

// ---------------------------------------------------------------------------
// 我的翻译（历史）
// ---------------------------------------------------------------------------

#[tauri::command]
fn history_list(
    state: State<AppState>,
    query: String,
    favorites_only: bool,
    limit: i64,
) -> Result<Vec<db::HistoryRow>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::list_history(&conn, Some(&query), favorites_only, limit).map_err(|e| e.to_string())
}

#[tauri::command]
fn history_delete(state: State<AppState>, id: i64) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::delete_history(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
fn history_toggle_favorite(state: State<AppState>, id: i64) -> Result<bool, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::toggle_favorite(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
fn history_clear(state: State<AppState>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::clear_history(&conn).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

#[tauri::command]
fn settings_get_all(state: State<AppState>) -> Result<HashMap<String, String>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::get_all_settings(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn settings_set(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::set_setting(&conn, &key, &value).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 词典
// ---------------------------------------------------------------------------

#[tauri::command]
fn dict_lookup(state: State<AppState>, word: String) -> Result<Vec<dict::DictResult>, String> {
    let enabled = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        db::list_enabled_dicts(&conn).map_err(|e| e.to_string())?
    };
    let mut cache = state.dicts.lock().map_err(|e| e.to_string())?;
    Ok(dict::lookup(&mut cache, &enabled, word.trim()))
}

#[tauri::command]
fn dict_list(state: State<AppState>) -> Result<Vec<db::DictRow>, String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::list_dicts(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn dict_set_enabled(state: State<AppState>, id: i64, enabled: bool) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::set_dict_enabled(&conn, id, enabled).map_err(|e| e.to_string())
}

#[tauri::command]
fn dict_add_mdx(state: State<AppState>, path: String) -> Result<db::DictRow, String> {
    // 先验证能解析
    dict::mdx::MdxDict::open(&path).map_err(|e| format!("无法解析该 mdx：{e}"))?;
    let name = std::path::Path::new(&path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "词典".into());
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let id = db::add_mdx_dict(&conn, &name, &path).map_err(|e| e.to_string())?;
    db::get_dict(&conn, id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "导入后未找到词典记录".to_string())
}

#[tauri::command]
fn dict_remove(state: State<AppState>, id: i64) -> Result<(), String> {
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        db::remove_dict(&conn, id).map_err(|e| e.to_string())?;
    }
    // 从缓存驱逐
    if let Ok(mut cache) = state.dicts.lock() {
        cache.evict_mdx(id);
    }
    Ok(())
}

#[tauri::command]
fn dict_reorder(state: State<AppState>, ids: Vec<i64>) -> Result<(), String> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    db::reorder_dicts(&conn, &ids).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 截图翻译 / 划词翻译
// ---------------------------------------------------------------------------

fn read_setting_val(app: &AppHandle, key: &str) -> Option<String> {
    let st = app.state::<AppState>();
    let conn = st.db.lock().ok()?;
    db::get_setting(&conn, key).ok().flatten()
}

fn prefer_accurate_ocr(app: &AppHandle) -> bool {
    read_setting_val(app, "ocr_engine").as_deref() == Some("accurate")
}

/// 诊断日志：追加到 app_data_dir/diag.log（截图/划词/热键排查用）。
fn diag_log(app: &AppHandle, msg: &str) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("diag.log"))
        {
            use std::io::Write;
            let _ = writeln!(f, "{msg}");
        }
    }
}

/// 在锚点附近放窗，超出所在屏就往反方向翻转 + 夹进屏内。win_w/h 为窗口逻辑尺寸。
fn place_near(
    app: &AppHandle,
    anchor: PhysicalPosition<i32>,
    win_w_logical: f64,
    win_h_logical: f64,
) -> PhysicalPosition<i32> {
    let info = match capture::monitor_at(anchor.x, anchor.y) {
        Ok(i) => i,
        Err(_) => return PhysicalPosition::new(anchor.x + 12, anchor.y + 16),
    };
    let ww = (win_w_logical * info.scale) as i32;
    let wh = (win_h_logical * info.scale) as i32;
    let (mx, my) = (info.x, info.y);
    let (mr, mb) = (info.x + info.w as i32, info.y + info.h as i32);
    let mut x = anchor.x + 12;
    let mut y = anchor.y + 16;
    if y + wh > mb {
        y = anchor.y - wh - 12; // 下方放不下 → 翻到上方
    }
    if x + ww > mr {
        x = mr - ww - 8; // 右边放不下 → 左移
    }
    PhysicalPosition::new(x.max(mx), y.max(my))
}

/// 弹结果小卡：payload 存 State，popup 前端 mount 后主动拉；失焦自动关。
fn show_popup(app: &AppHandle, src: &str, dst: &str, engine: &str, pos: PhysicalPosition<i32>) {
    if let Ok(mut g) = app.state::<AppState>().popup_payload.lock() {
        *g = Some(PopupPayload {
            src: src.into(),
            dst: dst.into(),
            engine: engine.into(),
        });
    }
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.close();
    }
    let popup = match WebviewWindowBuilder::new(app, "popup", WebviewUrl::App("popup.html".into()))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(false)
        .inner_size(440.0, 280.0)
        .visible(false)
        .focused(false)
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("建结果窗失败: {e}");
            return;
        }
    };
    let _ = popup.set_position(pos);
    let _ = popup.show();
    // 关键：不抢焦点、不 set_focus。全局热键触发时窗口拿不稳前台焦点，若 focused(true)+前端 blur 自关，
    // 会一获焦立刻失焦→blur→弹窗创建瞬间消失（用户感知“划词无法唤醒”）；且抢走源程序焦点会害下一次
    // 划词的 Ctrl+C 复制到空（len=0）。故弹窗只做常驻置顶卡片，关闭由前端 × / 超时负责。
}

/// 图像内嵌翻译结果窗：铺在被截区域原位，显示截图 + 逐行译文覆盖在原文上。
fn show_shot(
    app: &AppHandle,
    payload: ShotPayload,
    pos: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) {
    if let Ok(mut g) = app.state::<AppState>().shot_payload.lock() {
        *g = Some(payload);
    }
    // 复用已存在的结果窗：close 是异步的，立刻重建会撞 "already exists"
    if let Some(win) = app.get_webview_window("shot") {
        let _ = win.set_position(pos);
        let _ = win.set_size(size);
        let _ = win.show();
        let _ = win.eval("location.reload()"); // 重新拉新 payload
        let _ = win.set_focus();
        return;
    }
    let win = match WebviewWindowBuilder::new(app, "shot", WebviewUrl::App("shot.html".into()))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(false)
        .visible(false)
        .focused(true)
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            diag_log(app, &format!("show_shot build FAILED: {e}"));
            return;
        }
    };
    let _ = win.set_position(pos);
    let _ = win.set_size(size);
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn close_popup(app: AppHandle) {
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.close();
    }
}

#[tauri::command]
fn take_shot_payload(state: State<AppState>) -> Option<ShotPayload> {
    state.shot_payload.lock().ok().and_then(|g| g.clone())
}

#[tauri::command]
fn close_shot(app: AppHandle) {
    if let Some(w) = app.get_webview_window("shot") {
        let _ = w.close();
    }
}

/// 从截图结果窗「编辑」：把原文送进主界面（复用主界面翻译/编辑/复制），显主窗、关结果窗。
#[tauri::command]
fn edit_in_main(app: AppHandle, text: String) {
    let _ = app.emit_to("main", "yidian://fill", text);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    if let Some(w) = app.get_webview_window("shot") {
        let _ = w.close();
    }
}

/// Alt+Q：在光标所在屏建全屏透明 overlay 供拖框。
async fn start_screenshot(app: AppHandle) {
    // 连按 Alt+Q / 上次遮罩没完成：只关掉旧遮罩并返回，绝不在同一次调用里 close 后又以同 label rebuild
    //（同标签窗口边关边建会与主线程事件循环竞态 → build 阻塞在等 label 释放 → 主线程死锁 = 假死）。
    // 关掉即返回；再按一次 Alt+Q 会开一个全新遮罩。
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.close();
        diag_log(&app, "start_screenshot: 已有遮罩→关闭并忽略本次(再按一次重开)");
        return;
    }
    let cursor = match app.cursor_position() {
        Ok(c) => c,
        Err(e) => {
            diag_log(&app, &format!("start_screenshot: cursor_position 失败 {e}"));
            return;
        }
    };
    let info = match capture::monitor_at(cursor.x as i32, cursor.y as i32) {
        Ok(i) => i,
        Err(e) => {
            diag_log(&app, &format!("start_screenshot: monitor_at 失败 {e}"));
            return;
        }
    };
    // 抓整屏原图（在显示 overlay 之前，图里绝无遮罩）。xcap 是阻塞调用 → spawn_blocking，
    // 避免连按 Alt+Q 时阻塞 async 运行时导致后续不出遮罩。
    let full = match tauri::async_runtime::spawn_blocking(move || capture::capture_full(info.x, info.y))
        .await
    {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => {
            diag_log(&app, &format!("start_screenshot: capture_full 失败 {e}"));
            return;
        }
        Err(e) => {
            diag_log(&app, &format!("start_screenshot: 截屏线程失败 {e}"));
            return;
        }
    };
    if let Ok(mut g) = app.state::<AppState>().screenshot.lock() {
        *g = Some(capture::Screenshot { info, full });
    }
    let win = match WebviewWindowBuilder::new(&app, "overlay", WebviewUrl::App("overlay.html".into()))
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .visible(false)
        .focused(true)
        .build()
    {
        Ok(w) => w,
        Err(e) => {
            diag_log(&app, &format!("start_screenshot: overlay build FAILED: {e}"));
            return;
        }
    };
    let _ = win.set_position(PhysicalPosition::new(info.x, info.y));
    let _ = win.set_size(PhysicalSize::new(info.w, info.h));
    let _ = win.show();
    let _ = win.set_focus();
    // 关键：全局热键触发时本 app 非前台，Windows 前台锁会让新遮罩窗抢不到前台→webview 不激活/不绘制，
    // 表现为"要点一下才出现"。用 AttachThreadInput 绕过前台锁强制拉到前台，做到每次直接唤醒。
    #[cfg(windows)]
    force_foreground(&win);
    diag_log(
        &app,
        &format!("start_screenshot: overlay shown at {},{} {}x{}", info.x, info.y, info.w, info.h),
    );
    // 安全网：遮罩是全屏置顶窗，一旦它的 webview 卡住/失焦收不到 Esc，就会挡死全屏所有点击
    //（连任务栏/托盘都点不动）。这里后端起一个独立计时器，25s 内若遮罩还在（既没截图也没取消），
    // 强制关掉它——不依赖那个可能卡住的 webview，保证屏幕绝不会被永久锁死。
    {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if let Some(w) = app2.get_webview_window("overlay") {
                let _ = w.close();
                diag_log(&app2, "start_screenshot: 遮罩 15s 超时→后端强制关闭(安全网)");
            }
        });
    }
}

/// Alt+W：取选区 → 翻译 → 光标旁弹卡。
async fn start_selection(app: AppHandle) {
    let sel = tauri::async_runtime::spawn_blocking(selection::grab_selection)
        .await
        .ok()
        .flatten();
    diag_log(
        &app,
        &format!("start_selection: got selection len={}", sel.as_deref().map(str::len).unwrap_or(0)),
    );
    let text = match sel {
        Some(t) if !t.trim().is_empty() => t,
        _ => return,
    };
    // 划词也默认在线(快)，网络卡时 run_translate 内部回退本地兜底
    let engine = "online";
    let translated;
    let s;
    let t;
    let label;
    let t0 = std::time::Instant::now();
    {
        let st = app.state::<AppState>();
        match run_translate(st.inner(), &text, engine).await {
            Ok(v) => {
                translated = v.0;
                s = v.1;
                t = v.2;
                label = v.3;
            }
            Err(e) => {
                diag_log(&app, &format!("start_selection: 翻译失败(不弹窗) {e}"));
                return;
            }
        }
    }
    diag_log(
        &app,
        &format!(
            "start_selection: 翻译完成 引擎={} 耗时={}ms 译文len={}",
            label,
            t0.elapsed().as_millis(),
            translated.len()
        ),
    );
    {
        let st = app.state::<AppState>();
        record_history(st.inner(), &text, &translated, s, t, &label);
    }
    let pos = app
        .cursor_position()
        .map(|c| place_near(&app, PhysicalPosition::new(c.x as i32, c.y as i32), 440.0, 280.0))
        .unwrap_or(PhysicalPosition::new(200, 200));
    show_popup(&app, &text, &translated, &label, pos);
}

/// 显示主界面（从托盘或隐藏态唤回并聚焦）。
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 绕过 Windows 前台锁把窗口强制拉到前台并激活（全局热键触发时 app 非前台，SetForegroundWindow
/// 会被静默拒绝→窗口不激活/webview 不绘制）。用 AttachThreadInput 附到当前前台线程输入队列再抢前台。
#[cfg(windows)]
fn force_foreground(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        ShowWindow, SW_SHOW,
    };
    let hwnd = match win.hwnd() {
        Ok(h) => HWND(h.0 as *mut core::ffi::c_void),
        Err(_) => return,
    };
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let cur_thread = GetCurrentThreadId();
        let attached = AttachThreadInput(fg_thread, cur_thread, true.into()).as_bool();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(fg_thread, cur_thread, false.into());
        }
    }
}

/// 定位 PaddleOCR-json.exe：① 安装包内资源目录(生产) ② 环境变量 YIDIAN_PADDLE_EXE(开发/自定义安装位置)。
fn resolve_paddle_exe(app: &AppHandle) -> Option<std::path::PathBuf> {
    if let Ok(p) = app
        .path()
        .resolve("paddleocr/PaddleOCR-json.exe", tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(custom) = std::env::var("YIDIAN_PADDLE_EXE") {
        let p = std::path::PathBuf::from(custom);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// PaddleOCR 识别（懒启动持久子进程；阻塞 I/O 放 spawn_blocking）。
async fn paddle_ocr(app: &AppHandle, b64: String) -> Result<Vec<ocr::LineBox>, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<ocr::LineBox>, String> {
        let st = app2.state::<AppState>();
        let mut guard = st.paddle.lock().map_err(|e| e.to_string())?;
        if guard.is_none() {
            let exe = resolve_paddle_exe(&app2).ok_or("未找到 PaddleOCR-json.exe")?;
            *guard = Some(ocr::paddle::Paddle::start(&exe)?);
        }
        guard.as_mut().unwrap().ocr_base64(&b64)
    })
    .await
    .map_err(|e| format!("PaddleOCR 线程错误: {e}"))?
}

/// 启动时预热 PaddleOCR：跑一次空图，既验证子进程 I/O 通、又预载模型（消首次冷载延迟）。
async fn warmup_paddle(app: AppHandle) {
    let png = capture::blank_png(40, 40);
    let b64 = B64.encode(&png);
    match paddle_ocr(&app, b64).await {
        Ok(lines) => diag_log(
            &app,
            &format!("paddle warmup OK: {} lines (subprocess ready)", lines.len()),
        ),
        Err(e) => diag_log(&app, &format!("paddle warmup FAILED: {e}")),
    }
}

#[tauri::command]
async fn overlay_capture(app: AppHandle, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.close();
    }
    if w < 4.0 || h < 4.0 {
        return Ok(()); // 误点的小框忽略
    }
    let shot = {
        let st = app.state::<AppState>();
        let mut g = st.screenshot.lock().map_err(|e| e.to_string())?;
        g.take().ok_or("无截图数据")?
    };
    let info = shot.info;
    let (png, crop_w, crop_h) = tauri::async_runtime::spawn_blocking(move || {
        capture::crop_to_png(&shot.full, x, y, w, h, info.scale)
    })
    .await
    .map_err(|e| format!("裁剪线程错误: {e}"))??;

    // 结果窗铺在被截区域原位
    let shot_pos = PhysicalPosition::new(
        info.x + capture::dpi_scale(x, info.scale),
        info.y + capture::dpi_scale(y, info.scale),
    );
    let popup_pos = PhysicalPosition::new(
        info.x + capture::dpi_scale(x, info.scale),
        info.y + capture::dpi_scale(y + h, info.scale) + 8,
    );

    // 小图放大后再 OCR（提升模糊/小字/少字识别率），坐标按倍数缩回原图
    let (ocr_png, up) = capture::upscale_for_ocr(&png);
    let b64_ocr = B64.encode(&ocr_png);
    let mut lines = match paddle_ocr(&app, b64_ocr).await {
        Ok(l) => l,
        Err(e) => {
            show_popup(&app, "", &format!("识别失败：{e}"), "截图", popup_pos);
            return Ok(());
        }
    };
    if up > 1 {
        let f = up as f64;
        for l in &mut lines {
            l.x /= f;
            l.y /= f;
            l.w /= f;
            l.h /= f;
        }
    }
    diag_log(&app, &format!("overlay_capture: paddle {} lines (up {}x)", lines.len(), up));
    if lines.is_empty() {
        show_popup(&app, "", "（没认出文字，可框大点/选清楚点再试）", "截图", popup_pos);
        return Ok(());
    }

    // 翻译：截图默认走在线(快)，网络卡时 run_translate 内部自动回退本地 Qwen 兜底。
    // 先合并翻一次按行拆回；凡是对不上/空的行再逐行单独重翻补齐（修「漏翻译」）。
    let joined = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    let engine = "online";
    let translated_all = {
        let st = app.state::<AppState>();
        run_translate(st.inner(), &joined, engine)
            .await
            .map(|v| v.0)
            .unwrap_or_default()
    };
    let mut split: Vec<String> = translated_all.split('\n').map(|s| s.trim().to_string()).collect();
    while split.last().map(|s| s.is_empty()).unwrap_or(false) {
        split.pop();
    }
    // 合并结果按行对齐（行数相符才用），否则留空待逐行补
    let mut dsts: Vec<String> = (0..lines.len())
        .map(|i| if split.len() == lines.len() { split[i].clone() } else { String::new() })
        .collect();
    // 补空：任何空译文单独重翻一次，保证每行都有译文、不漏翻
    {
        let st = app.state::<AppState>();
        for (i, l) in lines.iter().enumerate() {
            if dsts[i].trim().is_empty() && !l.text.trim().is_empty() {
                if let Ok(v) = run_translate(st.inner(), &l.text, engine).await {
                    dsts[i] = v.0;
                }
            }
        }
    }
    diag_log(&app, &format!(
        "overlay_capture: filled {}/{} lines",
        dsts.iter().filter(|s| !s.is_empty()).count(),
        lines.len()
    ));
    {
        let st = app.state::<AppState>();
        record_history(st.inner(), &joined, &translated_all, "-", "-", "截图");
    }
    let shot_lines: Vec<ShotLine> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| ShotLine {
            x: l.x,
            y: l.y,
            w: l.w,
            h: l.h,
            src: l.text.clone(),
            dst: dsts.get(i).cloned().unwrap_or_default(),
        })
        .collect();
    let payload = ShotPayload {
        image: format!("data:image/png;base64,{}", B64.encode(&png)),
        width: crop_w,
        height: crop_h,
        disp_w: crop_w as f64 / info.scale,
        lines: shot_lines,
    };
    // 窗口：底部加一条工具条带；宽度不小于最小值(保证工具条放得下)
    let bar_phys = (34.0 * info.scale).round() as u32;
    let min_w_phys = (300.0 * info.scale).round() as u32;
    show_shot(
        &app,
        payload,
        shot_pos,
        PhysicalSize::new(crop_w.max(min_w_phys), crop_h + bar_phys),
    );
    Ok(())
}

#[tauri::command]
fn cancel_overlay(app: AppHandle) {
    diag_log(&app, "cancel_overlay: 收到取消(Esc/单击)→关遮罩");
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.close();
    }
}

#[tauri::command]
fn take_popup_payload(state: State<AppState>) -> Option<PopupPayload> {
    state.popup_payload.lock().ok().and_then(|g| g.clone())
}

#[tauri::command]
fn ocr_languages() -> Vec<String> {
    ocr::available_languages()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        // single-instance 必须最先注册：保证只有一个 app.exe，避免第二实例抢注全局热键静默失败
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }));
        // 开机自启插件（登录时启动；下方 setup 里首次运行开启）
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None::<Vec<&str>>,
        ));
    }

    builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(desktop)]
    {
        use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};
        builder = builder.plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    // 一次按键会派发 Pressed+Released 两个事件，只认 Pressed
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let alt_q = Shortcut::new(Some(Modifiers::ALT), Code::KeyQ);
                    let alt_w = Shortcut::new(Some(Modifiers::ALT), Code::KeyW);
                    if *shortcut == alt_q {
                        diag_log(app, "hotkey: Alt+Q pressed");
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move { start_screenshot(app).await });
                    } else if *shortcut == alt_w {
                        diag_log(app, "hotkey: Alt+W pressed");
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move { start_selection(app).await });
                    }
                })
                .build(),
        );
    }

    builder
        // 主窗口点 X：隐藏到托盘而非退出（常驻后台待命；真正退出走托盘「退出」）。
        // 只拦 main；overlay/popup/shot 结果窗照常关闭。
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let conn = Connection::open(dir.join("yidian.db"))?;
            db::init_schema(&conn)?;
            db::seed_default_settings(&conn)?;
            app.manage(AppState {
                db: Mutex::new(conn),
                dicts: Mutex::new(dict::DictCache::new()),
                screenshot: Mutex::new(None),
                popup_payload: Mutex::new(None),
                shot_payload: Mutex::new(None),
                paddle: Mutex::new(None),
            });
            // 托盘常驻图标 + 右键菜单
            #[cfg(desktop)]
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
                let show_i = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
                let shot_i = MenuItem::with_id(app, "shot", "截图翻译", true, None::<&str>)?;
                let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_i, &shot_i, &quit_i])?;
                let _tray = TrayIconBuilder::with_id("main-tray")
                    .tooltip("译点")
                    .icon(app.default_window_icon().unwrap().clone())
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => show_main(app),
                        "shot" => {
                            let app = app.clone();
                            tauri::async_runtime::spawn(async move { start_screenshot(app).await });
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_main(tray.app_handle());
                        }
                    })
                    .build(app)?;
                // 首次运行开启开机自启（已启用则不动，尊重用户后续在系统里的关闭）。
                // 仅正式打包(release)版生效——dev 版不要把 target\debug\app.exe 塞进登录项。
                #[cfg(not(debug_assertions))]
                {
                    use tauri_plugin_autostart::ManagerExt;
                    if !app.autolaunch().is_enabled().unwrap_or(false) {
                        let _ = app.autolaunch().enable();
                    }
                }
            }
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
                let gs = app.global_shortcut();
                let aq = Shortcut::new(Some(Modifiers::ALT), Code::KeyQ);
                let aw = Shortcut::new(Some(Modifiers::ALT), Code::KeyW);
                let rq = gs.register(aq);
                let rw = gs.register(aw);
                diag_log(
                    app.handle(),
                    &format!(
                        "startup register: Alt+Q ok={:?} is_registered={:?}; Alt+W ok={:?} is_registered={:?}",
                        rq.is_ok(),
                        gs.is_registered(aq),
                        rw.is_ok(),
                        gs.is_registered(aw),
                    ),
                );
            }
            diag_log(
                app.handle(),
                &format!("ocr languages available: {:?}", ocr::available_languages()),
            );
            // 后台预热 PaddleOCR（验证子进程 + 预载模型，不阻塞启动）
            {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move { warmup_paddle(h).await });
            }
            // 后台预热在线翻译（预抓 Bing token + 暖连接，首次截图翻译即热，消除首次慢）
            {
                let order = read_online_order(app.state::<AppState>().inner());
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    engine::online::warmup(&order).await;
                    diag_log(&h, "online warmup done (token+连接已热)");
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            translate,
            history_list,
            history_delete,
            history_toggle_favorite,
            history_clear,
            settings_get_all,
            settings_set,
            dict_lookup,
            dict_list,
            dict_set_enabled,
            dict_add_mdx,
            dict_remove,
            dict_reorder,
            overlay_capture,
            cancel_overlay,
            take_popup_payload,
            close_popup,
            take_shot_payload,
            close_shot,
            edit_in_main,
            ocr_languages,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
