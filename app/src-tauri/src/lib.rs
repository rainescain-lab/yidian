mod capture;
mod db;
mod dict;
mod engine;
mod hotkey;
mod hotkey_vk;
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
    /// 交互式翻译的代次 + 最新一次的原文。
    ///
    /// 历史表按 `source_text` UPSERT，谁最后落库谁赢 —— 而前端的 `reqId` 只能取消**显示**，
    /// 取消不了已经发出去的后端请求。于是"同一段文字换个方向重译"时，若旧请求走得慢
    /// （撞上 Bing token 过期→重取 token→在线失败→本地 Qwen 兜底，10s 量级）、新请求走得快
    /// （热 token 300ms），界面显示新方向的译文，历史行却被随后落地的旧请求盖回旧方向，
    /// 而且不会触发任何刷新 —— 两个页面自相矛盾且无从察觉。
    /// 所以落库前先确认自己仍是**这段原文**最新的那一次。
    req_seq: std::sync::atomic::AtomicU64,
    latest_req: Mutex<(String, u64)>,
    /// 主界面上手选的翻译方向。**会话内 sticky、不落盘**。
    ///
    /// 手选是"我这会儿要把这段译成日文"这种**任务级意图**，不是偏好：下次开软件理应回到
    /// 自动。落盘的话，用户某天为了一句话把方向钉成日文，几周后打开软件发现所有翻译都成了
    /// 日文却想不起为什么——这类残留状态的排查成本远高于它省下的那一次点击。
    manual_dir: Mutex<ManualDir>,
}

/// 手选方向。`None` = 那一侧交给自动规则。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManualDir {
    pub src: Option<String>,
    pub tgt: Option<String>,
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

/// 这一次翻译到底按什么方向走。
#[derive(Debug, Clone, PartialEq)]
struct Direction {
    /// 源语言名（喂 prompt、记历史）。
    src: String,
    /// 目标语言名。
    tgt: String,
    /// 源语言是**用户手选**的吗？
    ///
    /// 只有 true 时才把源语言传给在线引擎。false 时一律让引擎自己 `auto` 识别——
    /// 我们的脚本规则在"只含汉字的日语"这类情况上原理性地分不出来（`lang.rs` 的已知盲区），
    /// 拿它去顶掉引擎的自动识别只会更差。
    src_manual: bool,
}

/// 读母语设置，值坏掉/没登记时回落到出厂默认。
///
/// 这里的回落是**配置健全性兜底**，不是"翻译结果降级"：拿一个不在语言表里的名字去调引擎，
/// Google 会返回 200 且原样不翻译（连报错都没有），用户只会觉得软件坏了。写入侧
/// （`settings_set`）已经拦了一道，这里兜住历史脏数据。
fn read_native_pair(state: &AppState) -> (String, String) {
    let get = |k: &str, d: &str, ok: fn(&str) -> bool| -> String {
        let v = state
            .db
            .lock()
            .ok()
            .and_then(|c| db::get_setting(&c, k).ok().flatten());
        match v {
            Some(s) if ok(&s) => s,
            _ => d.to_string(),
        }
    };
    (
        // 母语的判据比目标语言严，理由见 is_native_selectable
        get("native_lang", "Chinese", engine::online::is_native_selectable),
        get("native_to", "English", engine::online::is_supported),
    )
}

/// 算出本次翻译的方向。
///
/// `follow_manual = false` 时完全忽略主界面的手选（划词/截图默认走这条，见
/// `selection_follow_manual` 设置项：划词离 UI 最远，最容易被残留的手选状态咬）。
fn resolve_direction(state: &AppState, text: &str, follow_manual: bool) -> Direction {
    let (native, native_to) = read_native_pair(state);
    let manual = if follow_manual {
        state.manual_dir.lock().map(|g| g.clone()).unwrap_or_default()
    } else {
        ManualDir::default()
    };

    let (auto_src, auto_tgt) = engine::lang::direction_with_native(text, &native, &native_to);
    let src_manual = manual.src.is_some();
    let src = manual.src.unwrap_or(auto_src);
    // 只钉了源语言时，目标仍按"母语→native_to，其他→母语"这条规则现算 ——
    // 不能直接用 auto_tgt，它是按**自动判出来的源语言**算的，跟用户钉的这个可能不是一回事。
    let by_rule = |s: &str| {
        if s == native {
            native_to.clone()
        } else {
            native.clone()
        }
    };
    let tgt_manual = manual.tgt.is_some();
    let mut tgt = match manual.tgt {
        Some(t) => t,
        None if src_manual => by_rule(&src),
        None => auto_tgt,
    };

    // 同语言保护：src==tgt 时引擎会原样返回，界面上看着就是"翻了跟没翻一样"。
    //
    // ⚠ 但**不能拿一个猜出来的源去否决用户明选的目标**（2026-08-07 对抗复核揪出）：
    // src_manual=false 时 src 只是我们的脚本判定，而且**根本不会发给在线引擎**（走 sl=auto）。
    // 典型踩法：用户把目标选成中文、翻 `東京都新宿区`（只含汉字的日语，脚本层必判成中文，
    // 见 lang.rs 已知盲区）→ 保护触发 → 目标被悄悄改成英文 → 引擎自己识别出日语、译成英文。
    // 用户明明选了"译成中文"，拿到的是英文，界面上还显示着中文，毫无提示。
    // 所以：目标是用户明选的、而源只是猜的时候，让步的必须是猜测，不是用户的选择。
    let respect_tgt = tgt_manual && !src_manual;
    if tgt == src && !respect_tgt {
        tgt = by_rule(&src);
        if tgt == src {
            // 病态配置（native == native_to）才会走到这儿，给一个必定不同的落点。
            tgt = if src == "English" { "Chinese" } else { "English" }.to_string();
        }
    }

    Direction {
        src,
        tgt,
        src_manual,
    }
}

/// 认领一次交互式翻译，返回本次代次。见 `AppState.req_seq`。
fn claim_request(state: &AppState, text: &str) -> u64 {
    let gen = state
        .req_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    if let Ok(mut g) = state.latest_req.lock() {
        *g = (text.to_string(), gen);
    }
    gen
}

/// 这一次的结果还该不该写进历史。
///
/// ⚠ 按**原文**比对而不是全局代次：只压掉"同一行的过期写入"，不能误伤打字防抖期间
/// 那些原文各不相同的条目（它们各写各的行，谁也不盖谁）。
fn is_superseded(state: &AppState, text: &str, gen: u64) -> bool {
    state
        .latest_req
        .lock()
        .map(|g| g.0 == text && g.1 != gen)
        .unwrap_or(false)
}

/// 统一翻译：按 engine(local|online) 路由，返回 (译文, 引擎名)。方向由调用方给定。
async fn run_translate(
    state: &AppState,
    text: &str,
    engine: &str,
    dir: &Direction,
) -> Result<(String, String), String> {
    // 源语言只有在用户手选时才传给在线引擎，理由见 Direction::src_manual。
    let online_src = dir.src_manual.then(|| dir.src.as_str());
    if engine == "online" {
        let order = read_online_order(state);
        match engine::online::translate_online(text, online_src, &dir.tgt, &order).await {
            Ok(v) => Ok(v),
            // 在线失败(网络卡/超时/被墙) → 本地 Qwen 兜底。
            // ⚠ 必须把 src/tgt 一并传下去：不传的话，网络一卡方向就**静默打回默认**，
            // 用户手选的方向在兜底路径上悄悄失效（2026-08-07）。
            Err(e) => match engine::ollama::translate_local(text, &dir.src, &dir.tgt).await {
                Ok(t) => Ok((t, "本地(兜底)".to_string())),
                Err(e2) => Err(format!("在线失败({e})、本地兜底也失败({e2})")),
            },
        }
    } else {
        engine::ollama::translate_local(text, &dir.src, &dir.tgt)
            .await
            .map(|t| (t, "本地".to_string()))
    }
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

    let gen = claim_request(state.inner(), &text);

    // 主界面一定听手选（那个下拉框就在用户眼前，不听它才是 bug）。
    let dir = resolve_direction(state.inner(), &text, true);
    let (translated, engine_label) = run_translate(state.inner(), &text, &engine, &dir).await?;

    // 这段原文已经被更新的一次翻过了 ⇒ 不许回写历史（详见 AppState.req_seq）。
    // 返回 history_id=0：此时前端那边这次响应本来也会被 reqId 丢掉，★收藏按钮不受影响。
    let (history_id, favorite) = if is_superseded(state.inner(), &text, gen) {
        (0, false)
    } else {
        record_history(
            state.inner(),
            &text,
            &translated,
            &dir.src,
            &dir.tgt,
            &engine_label,
        )
    };

    Ok(TranslateOut {
        text: translated,
        src_lang: dir.src,
        tgt_lang: dir.tgt,
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
    // 语言类设置在**写入侧**就挡住不认识的值。放进去再说的话，故障要等到下一次翻译才现形，
    // 而且现形方式是 Google 返回 200 且原样不翻译（连报错都没有）—— 极难往设置上想。
    if key == "native_to" && !engine::online::is_supported(&value) {
        return Err(format!("不支持的语言「{value}」"));
    }
    // 母语比目标语言严：它要参与「这段是不是母语」的比较，必须落在脚本判定的值域里。
    // 详见 engine::online::is_native_selectable。
    if key == "native_lang" && !engine::online::is_native_selectable(&value) {
        return Err(format!(
            "「{value}」不能当母语——译点分不出这门语言的原文（拉丁字母各语言在字符层面一样），\
             把它选成「母语译成」的目标是可以的"
        ));
    }
    // 热键不走这条路：它要真的去注册、要处理失败与冲突，见 hotkey_set。
    if key.starts_with("hotkey_") {
        return Err("快捷键请用「修改」按钮设置（需要真正注册才算数）".into());
    }
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
            let _ = writeln!(f, "[{}] {msg}", log_stamp());
        }
    }
}

/// 日志时间戳（本地时间）。没有时间戳时无法把日志行和用户的实际操作对上。
#[cfg(windows)]
fn log_stamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(not(windows))]
fn log_stamp() -> String {
    String::new()
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

/// 遮罩代次：每成功开出一个遮罩 +1。安全网计时器凭它认领"自己那一代"，
/// 免得 15s 后误杀后来新开的遮罩（详见 `start_screenshot` 末尾）。
static OVERLAY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 截图热键（默认 Alt+Q）：在光标所在屏建全屏透明 overlay 供拖框。
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
    // 本次遮罩的代次。安全网计时器只认自己这一代，理由见下面那段。
    let my_gen = OVERLAY_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
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
        &format!(
            "start_screenshot: overlay#{my_gen} shown at {},{} {}x{}",
            info.x, info.y, info.w, info.h
        ),
    );
    // 安全网：遮罩是全屏置顶窗，一旦它的 webview 卡住/失焦收不到 Esc，就会挡死全屏所有点击
    //（连任务栏/托盘都点不动）。这里后端起一个独立计时器，15s 内若遮罩还在（既没截图也没取消），
    // 强制关掉它——不依赖那个可能卡住的 webview，保证屏幕绝不会被永久锁死。
    //
    // ⚠ 必须按**代次**认领，不能只按窗口 label 关（2026-08-07 对抗复核揪出）：计时器只捕获
    // AppHandle，15s 后 `get_webview_window("overlay")` 拿到的是**当时**那个遮罩，可能已经是
    // 后来新开的第二个。而截图翻译的自然节奏（按键→拖框→OCR+翻译→看结果→再按）通常远短于
    // 15s，于是稳态下新遮罩的存活上限 = 15s 减去上一轮已经走掉的时间 —— 表现为"遮罩自己没了/
    // 拖到一半框消失"，日志还谎报"15s 超时"，把排查引向 webview 卡死这个错方向。
    {
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if OVERLAY_GEN.load(std::sync::atomic::Ordering::SeqCst) != my_gen {
                return; // 这一代早已谢幕，屏幕上是后来那个，不许动
            }
            if let Some(w) = app2.get_webview_window("overlay") {
                let _ = w.close();
                diag_log(
                    &app2,
                    &format!("start_screenshot: 遮罩#{my_gen} 15s 超时→后端强制关闭(安全网)"),
                );
            }
        });
    }
}

/// 划词/截图是否继承主界面上手选的方向。默认否，理由见 `db.rs` 的 `selection_follow_manual`。
fn selection_follows_manual(app: &AppHandle) -> bool {
    read_setting_val(app, "selection_follow_manual").as_deref() == Some("1")
}

/// 划词热键（默认 Alt+W）：取选区 → 翻译 → 光标旁弹卡。
async fn start_selection(app: AppHandle) {
    // ⚠ 等键信息必须从**实际注册成功的那个热键**取，不能用"用户想要的那个"。
    let hk = hotkey::global();
    let keys = selection::Keys {
        main_vk: hk.selection_main_vk(),
        accel: hk.accel(hotkey::Action::Selection),
    };
    let accel = hk.accel(hotkey::Action::Selection);
    let got = tauri::async_runtime::spawn_blocking(move || selection::grab_selection(keys))
        .await
        .unwrap_or_else(|e| selection::GrabResult {
            text: None,
            keys_held: None,
            diag: vec![format!("    spawn_blocking 崩了: {e}")],
        });
    diag_log(
        &app,
        &format!(
            "start_selection: got selection len={}",
            got.text.as_deref().map(str::len).unwrap_or(0)
        ),
    );
    for line in &got.diag {
        diag_log(&app, line);
    }
    let text = match got.text {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            // 「等不到松手就放弃」必须说出来 —— 它是常态路径，而且用户自己就能修。
            // 其它 sel=None（最常见的是"压根没选中文字"）保持静默：那种情况下弹卡
            // 只会让每一次误按热键都糊一张卡片在屏幕上。
            if let Some(still) = got.keys_held {
                show_popup(
                    &app,
                    "",
                    &format!(
                        "按住 {} 不放，取不到选中的文字。\n按完就松手再试一次（放开时才会去取词）。\n\n（放弃时仍按着：{still}）",
                        format_accel_zh(&accel)
                    ),
                    "划词",
                    selection_popup_pos(&app),
                );
            }
            return;
        }
    };
    // 划词也默认在线(快)，网络卡时 run_translate 内部回退本地兜底
    let engine = "online";
    let translated;
    let label;
    let dir;
    let t0 = std::time::Instant::now();
    {
        let st = app.state::<AppState>();
        dir = resolve_direction(st.inner(), &text, selection_follows_manual(&app));
        match run_translate(st.inner(), &text, engine, &dir).await {
            Ok(v) => {
                translated = v.0;
                label = v.1;
            }
            Err(e) => {
                // 以前这里是静默 return：翻译失败在界面上和「划词压根没生效」长得一模一样，
                // 用户只看到"划了词什么都没发生"，也无从判断该看哪儿。失败必须可见。
                diag_log(&app, &format!("start_selection: 翻译失败 {e}"));
                show_popup(&app, &text, &format!("翻译失败：{e}"), "错误", selection_popup_pos(&app));
                return;
            }
        }
    }
    diag_log(
        &app,
        &format!(
            "start_selection: 翻译完成 方向={}→{}{} 引擎={} 耗时={}ms 译文len={}",
            dir.src,
            dir.tgt,
            if dir.src_manual { "(手选源)" } else { "" },
            label,
            t0.elapsed().as_millis(),
            translated.len()
        ),
    );
    {
        let st = app.state::<AppState>();
        record_history(st.inner(), &text, &translated, &dir.src, &dir.tgt, &label);
    }
    show_popup(&app, &text, &translated, &label, selection_popup_pos(&app));
}

/// 划词卡片的落点：光标旁，超出所在屏就翻转并夹回屏内。
fn selection_popup_pos(app: &AppHandle) -> PhysicalPosition<i32> {
    app.cursor_position()
        .map(|c| place_near(app, PhysicalPosition::new(c.x as i32, c.y as i32), 440.0, 280.0))
        .unwrap_or(PhysicalPosition::new(200, 200))
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
            let t0 = std::time::Instant::now();
            let p = ocr::paddle::Paddle::start(&exe)?;
            let pid = p.pid();
            // 作业对象没挂上属于"保护降级"，必须留痕，别让它悄悄发生
            let note = p.job_note().map(|s| s.to_string());
            *guard = Some(p);
            diag_log(
                &app2,
                &format!(
                    "paddle 冷启动 pid={pid} 用时 {}ms（启动不再预热，故首次会多等模型冷载）{}",
                    t0.elapsed().as_millis(),
                    note.map(|s| format!(" {s}")).unwrap_or_default()
                ),
            );
        }
        guard.as_mut().unwrap().ocr_base64(&b64)
    })
    .await
    .map_err(|e| format!("PaddleOCR 线程错误: {e}"))?
}

/// 空闲多久就把 PaddleOCR 子进程整个退掉。
///
/// 不是抠门：实测子进程**每做一次真实识别就涨一大截且不释放**（一张 1920×1080、25 行 →
/// 从 635 MB 涨到 2314 MB 提交内存）。这是 PaddleOCR-json 自身的毛病、我们改不了它的源码，
/// 调用方唯一能做的就是别让它常驻。退掉后下次用会重新懒启动，代价是 2~3s 模型冷载。
const PADDLE_IDLE_MAX_SECS: u64 = 180;
const PADDLE_IDLE_CHECK_SECS: u64 = 30;

/// 空闲看门狗：定期看 PaddleOCR 子进程多久没用了，超时就退掉释放内存。
async fn paddle_idle_watchdog(app: AppHandle) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(PADDLE_IDLE_CHECK_SECS)).await;
        // ⚠ 取出来之后要在**锁外** drop：drop 会 kill + wait 子进程，可能阻塞，
        //    不能占着 AppState 的锁做，否则并发的识别请求全被堵住。
        let taken = {
            let st = app.state::<AppState>();
            // ⚠ 用 try_lock 不用 lock：锁被占着＝正有识别在跑＝本来就不算空闲，
            //   而且这是 async 任务，用阻塞的 lock() 会把 tokio 工作线程堵到识别结束。
            let mut g = match st.paddle.try_lock() {
                Ok(g) => g,
                Err(_) => continue, // 正忙 或 锁中毒：下一轮再看，别在看门狗里 panic
            };
            match g.as_ref().map(|p| p.idle_secs()) {
                Some(idle) if idle >= PADDLE_IDLE_MAX_SECS => g.take(),
                _ => None,
            }
        };
        if let Some(p) = taken {
            let (pid, idle) = (p.pid(), p.idle_secs());
            drop(p); // 这一下才真正 kill + wait + 关作业对象
            diag_log(
                &app,
                &format!("paddle 空闲 {idle}s，已退出子进程 pid={pid} 释放内存（下次用会重新懒启动）"),
            );
        }
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
    // 方向按**整块合并文本**算一次，后面补空的逐行重翻沿用同一个方向：
    // 逐行各判各的会让同一张截图里的行译到不同语言去（英文行→中文、中文行→英文）。
    let follow = selection_follows_manual(&app);
    let dir = {
        let st = app.state::<AppState>();
        resolve_direction(st.inner(), &joined, follow)
    };
    let translated_all = {
        let st = app.state::<AppState>();
        run_translate(st.inner(), &joined, engine, &dir)
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
                if let Ok(v) = run_translate(st.inner(), &l.text, engine, &dir).await {
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
        record_history(st.inner(), &joined, &translated_all, &dir.src, &dir.tgt, "截图");
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

// ---------------------------------------------------------------------------
// 语言方向（会话内手选）
// ---------------------------------------------------------------------------

#[tauri::command]
fn supported_languages() -> Vec<engine::online::LangOption> {
    engine::online::supported_languages()
}

/// 设置本会话的手选方向。任一侧传 `null` = 那一侧交回自动。
///
/// 前端改完方向要**先 await 这个命令再触发重译**：两个 invoke 之间没有顺序保证，
/// 抢跑的话第一次重译还是按旧方向走，用户会看到"选了没用、再点一下才对"。
#[tauri::command]
fn set_manual_direction(
    state: State<AppState>,
    src: Option<String>,
    tgt: Option<String>,
) -> Result<ManualDir, String> {
    for (which, v) in [("源", &src), ("目标", &tgt)] {
        if let Some(name) = v {
            if !engine::online::is_supported(name) {
                return Err(format!("不支持的{which}语言「{name}」"));
            }
        }
    }
    let next = ManualDir { src, tgt };
    *state.manual_dir.lock().map_err(|e| e.to_string())? = next.clone();
    Ok(next)
}

#[tauri::command]
fn get_manual_direction(state: State<AppState>) -> ManualDir {
    state
        .manual_dir
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 全局热键
// ---------------------------------------------------------------------------

#[tauri::command]
fn hotkey_list() -> Vec<hotkey::HotkeyInfo> {
    hotkey::global().snapshot()
}

/// 改一个全局热键。
///
/// # 次序：**先注册新的，成功了再注销旧的**
/// 反过来（先注销后注册）在新组合被别的程序占着时会把用户搞成"两个键都没了"。
/// 现在的次序保证失败时旧键**原样还在生效**，用户最多是换不成，不会丢功能。
///
/// # ⚠ 全程不能持有 `HotkeyState` 的锁
/// 插件的 `register/unregister` 内部是"把活儿丢给主线程并阻塞等结果"，而主线程可能正在
/// 跑热键 handler。若我们持着自己的锁去等主线程、handler 又要拿这把锁 ⇒ ABBA 死锁。
/// 所以下面每个 `hotkey::global()` 调用都是"进去拿了就出来"，中间不夹插件调用。
/// 详见 `hotkey.rs` 模块头。
#[tauri::command]
fn hotkey_set(app: AppHandle, action: String, accel: String) -> hotkey::HotkeySetResult {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let fail = |msg: String| hotkey::HotkeySetResult {
        ok: false,
        message: msg,
        hotkeys: hotkey::global().snapshot(),
    };

    let Some(act) = hotkey::Action::from_key(&action) else {
        return fail(format!("未知的动作「{action}」"));
    };
    let (canon, sc) = match hotkey::parse_accel(&accel) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };

    let st = hotkey::global();
    // 撞上另一个动作正占着的组合：先拦下来。让它走到 register 会撞**自己**的
    // AlreadyRegistered，错误信息是"已被注册"，用户完全看不出是被译点自己占了。
    if let Some(other) = st.taken_by_other(act, &sc) {
        return fail(format!(
            "这个组合已经用在「{}」上了，先把那个换掉",
            other.label()
        ));
    }

    let old = st.active(act);
    if old == Some(sc) {
        // 已经就是它：不做任何 OS 操作（重复 register 会撞自己的 AlreadyRegistered）。
        // 但仍把规范串落盘一次，好把历史上写歪的大小写顺手纠正过来。
        persist_hotkey(&app, act, &canon);
        st.record(act, canon, Some(sc), String::new());
        return hotkey::HotkeySetResult {
            ok: true,
            message: String::new(),
            hotkeys: st.snapshot(),
        };
    }

    let gs = app.global_shortcut();
    if let Err(e) = gs.register(sc) {
        diag_log(&app, &format!("hotkey_set: 注册 {canon} 失败: {e}"));
        return fail(format!(
            "「{}」注册不上（多半被别的程序占着）：{e}",
            crate::format_accel_zh(&canon)
        ));
    }
    if let Some(o) = old {
        // 失败只记录不回滚：新键已经生效，回滚反而会把用户刚设好的键也撤掉。
        // 残留的旧键顶多是多一个能触发的组合，下次启动就没了。
        if let Err(e) = gs.unregister(o) {
            diag_log(
                &app,
                &format!("hotkey_set: 旧键 {} 注销失败(已忽略): {e}", o.into_string()),
            );
        }
    }
    persist_hotkey(&app, act, &canon);
    st.record(act, canon.clone(), Some(sc), String::new());
    diag_log(&app, &format!("hotkey_set: {} → {canon} 已生效", act.key()));
    hotkey::HotkeySetResult {
        ok: true,
        message: String::new(),
        hotkeys: st.snapshot(),
    }
}

fn persist_hotkey(app: &AppHandle, act: hotkey::Action, accel: &str) {
    let st = app.state::<AppState>();
    // 不写成 `if let Ok(g) = ...` 收尾：那样它是函数的尾表达式，MutexGuard 这个临时量会
    // 排在局部变量 `st` **之后**析构，借用检查直接不过（E0597）。
    let conn = match st.db.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = db::set_setting(&conn, act.setting_key(), accel);
}

/// 「测一下」：开一个探测窗口，接下来这几秒内按这个热键**只回报、不执行**。
///
/// 这是唯一能查出"低级键盘钩子把键吞了"的办法 —— 那类程序（微信/QQ 截图、Snipaste、
/// 输入法、AHK）不占 RegisterHotKey 的槽位，我们这边 register 照样返回成功、
/// `is_registered` 也照样是 true，只有真按一次才知道到底到没到。
#[tauri::command]
fn hotkey_probe(app: AppHandle, action: String) -> Result<u64, String> {
    let act = hotkey::Action::from_key(&action).ok_or_else(|| format!("未知的动作「{action}」"))?;
    hotkey::global().arm_probe(act);
    diag_log(&app, &format!("hotkey_probe: 开始探测 {}", act.key()));
    Ok(hotkey::PROBE_WINDOW_MS)
}

/// 撤掉探测窗口（用户测到一半跑去改键 / 关掉设置页）。
///
/// 不撤的话，那个窗口内**第一次**按该热键会被吞成"探测命中"——只回报、不执行动作，
/// 而前端此时已经不在等待态、回报被丢弃 ⇒ 界面完全没反应，用户会得出"这个键也被占了"
/// 的相反结论。改键那条路已经在 `HotkeyState::record` 里顺手清了，这个命令补的是
/// 「按 Esc 取消录制」「切走设置页」这两条不经过 record 的路。
#[tauri::command]
fn hotkey_probe_cancel(action: String) {
    if let Some(act) = hotkey::Action::from_key(&action) {
        hotkey::global().disarm_probe(act);
    }
}

/// 侧栏「截图翻译」按钮：走和热键完全相同的通路。
///
/// 点按钮时译点自己在前台，不先让开的话截到的就是译点的窗口。**最小化**而不是隐藏：
/// 任务栏里还留着入口，用户不会找不回来。
#[tauri::command]
async fn trigger_shot(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.minimize();
            // 等窗口真的从屏幕上消失再抓图，否则抓到的还是带着译点的那一帧。
            tokio::time::sleep(std::time::Duration::from_millis(260)).await;
        }
    }
    diag_log(&app, "trigger_shot: 来自侧栏按钮");
    start_screenshot(app).await;
}

#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 规范串 → 人读写法（`alt+KeyQ` → `Alt + Q`）。只用于给用户看的错误文案。
///
/// 前端 `hotkey.ts` 的 `formatAccel` 才是显示层的正主，这里只做**够读**的粗略还原：
/// 后端错误信息里出现一串 `shift+control+KeyQ` 太劝退。两边不需要逐字一致。
fn format_accel_zh(accel: &str) -> String {
    let mut parts: Vec<&str> = accel.split('+').collect();
    let main = parts.pop().unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    for (tok, label) in [
        ("control", "Ctrl"),
        ("shift", "Shift"),
        ("alt", "Alt"),
        ("super", "Win"),
    ] {
        if parts.contains(&tok) {
            out.push(label.to_string());
        }
    }
    out.push(
        main.strip_prefix("Key")
            .or_else(|| main.strip_prefix("Digit"))
            .unwrap_or(main)
            .to_string(),
    );
    out.join(" + ")
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
        use tauri_plugin_global_shortcut::ShortcutState;
        builder = builder.plugin(
            tauri_plugin_global_shortcut::Builder::new()
                // ⚠⚠ 这个闭包是插件**持着它自己那把 Mutex、在主线程 WndProc 里**调进来的。
                //    因此这里只许做两件事：判断、spawn。具体禁忌（会硬死锁）：
                //      · 绝不能调任何 GlobalShortcut 方法（register/unregister/is_registered）；
                //      · 绝不能做任何阻塞或耗时的事（主线程卡住＝整个界面假死）。
                //    判断用的是 hotkey::global() 里的原子量，一把锁都不拿，详见 hotkey.rs 模块头。
                .with_handler(|app, shortcut, event| {
                    // 一次按键会派发 Pressed+Released 两个事件，只认 Pressed
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let Some(action) = hotkey::global().action_of(shortcut.id()) else {
                        return; // 不是我们的键（理论上到不了这儿），静默忽略
                    };
                    // 「测一下」：只回报按到了，不执行真实动作（否则一测就截图/划词）。
                    if hotkey::global().take_probe(action) {
                        let _ = app.emit("yidian://hotkey-probe", action.key());
                        diag_log(
                            app,
                            &format!("hotkey: {} 探测命中（未执行动作）", action.key()),
                        );
                        return;
                    }
                    diag_log(
                        app,
                        &format!("hotkey: {} pressed ({})", action.key(), shortcut.into_string()),
                    );
                    let app = app.clone();
                    match action {
                        hotkey::Action::Shot => {
                            tauri::async_runtime::spawn(async move { start_screenshot(app).await });
                        }
                        hotkey::Action::Selection => {
                            tauri::async_runtime::spawn(async move { start_selection(app).await });
                        }
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
                manual_dir: Mutex::new(ManualDir::default()),
            req_seq: std::sync::atomic::AtomicU64::new(0),
            latest_req: Mutex::new((String::new(), 0)),
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
            // 版本/构建类型/exe 路径：跨机排查时第一件要确认的就是"那台机器跑的到底是哪个包"。
            // 此前日志里完全没有版本信息，为此白白绕过弯路（2026-08-06 补）。
            //
            // ⚠ 这里只记**自动取得、不可能说谎**的东西。原先还手写了一个
            //   `构建标记=v0.4.0-2026-08-07(...)`，发 0.4.1 时忘了同步，日志就成了
            //   「版本=0.4.1 构建标记=v0.4.0-…」自相矛盾 —— 偏偏这行就是给跨机排查用的。
            //   凡是要人手同步的版本字符串迟早会陈旧，索性删掉：
            //   "这一版有什么"该写在 Release 说明里，不该写在会过期的字面量里。
            diag_log(
                app.handle(),
                &format!(
                    "=== 启动 版本={} 构建={} exe={:?}",
                    app.package_info().version,
                    if cfg!(debug_assertions) { "debug" } else { "release" },
                    std::env::current_exe().ok()
                ),
            );
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                let gs = app.global_shortcut();
                let st = hotkey::global();
                // 已经被本程序占下的组合。⚠ 必须有这一步：`hotkey_set` 那边有 taken_by_other
                // 拦自冲突，开机这条路却是直接照 DB 注册 —— 而 DB 里**可以**存下两个相同的组合
                // （给 A 设某组合时，若 B 那串当时正好没注册成功，taken_by_other 就不会拦）。
                // 撞上时第二个动作会收到 ERROR_HOTKEY_ALREADY_REGISTERED，文案却把锅甩给
                // "别的程序占着"，用户照着这个提示怎么换都换不好（2026-08-07 对抗复核揪出）。
                let mut claimed: Vec<(hotkey::Action, tauri_plugin_global_shortcut::Shortcut)> =
                    Vec::new();
                for act in hotkey::Action::ALL {
                    let stored = read_setting_val(app.handle(), act.setting_key());
                    let (accel, sc, warn) = hotkey::parse_or_default(act, stored.as_deref());
                    if let Some(w) = &warn {
                        diag_log(app.handle(), &format!("startup hotkey {}: {w}", act.key()));
                    }
                    // ⚠ 必须把**失败原因**也记下来，不能只记 ok=false：热键被别的程序占住时
                    // （RegisterHotKey 独占），原因就藏在这个 Err 里，否则跨机排查只能靠猜。
                    let (active, err) = match claimed.iter().find(|(_, s)| *s == sc) {
                        Some((other, _)) => (
                            None,
                            format!("和「{}」设成了同一个组合，去设置里把其中一个换掉", other.label()),
                        ),
                        None => match gs.register(sc) {
                            Ok(()) => {
                                claimed.push((act, sc));
                                (Some(sc), String::new())
                            }
                            Err(e) => (None, format!("{e}")),
                        },
                    };
                    st.record(act, accel.clone(), active, err.clone());
                    diag_log(
                        app.handle(),
                        &format!(
                            "startup register: {} = {accel} → {}",
                            act.key(),
                            if err.is_empty() {
                                "ok".to_string()
                            } else {
                                format!("失败: {err}")
                            }
                        ),
                    );
                }
                // 划词等键的主键 VK 是"发 Ctrl+C 之前等谁松开"的依据，单独记一行便于判读日志。
                diag_log(
                    app.handle(),
                    &format!(
                        "startup register: 划词等键主键 vk={:#04x}",
                        st.selection_main_vk()
                    ),
                );
            }
            diag_log(
                app.handle(),
                &format!("ocr languages available: {:?}", ocr::available_languages()),
            );
            // PaddleOCR 子进程：**不再启动预热**。
            //
            // 预热原本是为了消掉首次截图翻译的 2~3s 模型冷载，但代价是这个子进程从开机起就
            // 常驻——实测它光启动就占 600 MB 提交内存，做过一次真实全屏识别后涨到 2.3 GB 且不还。
            // 划词翻译根本用不到它。所以改成**用到才起**（见 paddle_ocr 的懒启动），
            // 代价是首次截图多等 2~3 秒，日志里会记。
            //
            // 这里只做两件跟"别再泄漏"有关的事：清理旧版漏下的孤儿 + 挂空闲看门狗。
            {
                let h = app.handle().clone();
                if let Some(exe) = resolve_paddle_exe(&h) {
                    let killed = ocr::paddle::kill_orphans(&exe);
                    if !killed.is_empty() {
                        diag_log(
                            &h,
                            &format!(
                                "已清理上次残留的 PaddleOCR 孤儿进程 {:?}（旧版从不 kill 子进程，每启动一次漏一个）",
                                killed
                            ),
                        );
                    }
                }
            }
            {
                let h = app.handle().clone();
                tauri::async_runtime::spawn(async move { paddle_idle_watchdog(h).await });
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
            supported_languages,
            set_manual_direction,
            get_manual_direction,
            hotkey_list,
            hotkey_set,
            hotkey_probe,
            hotkey_probe_cancel,
            trigger_shot,
            app_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------------------------------------------------------------------------
// 方向解析的单测
//
// `resolve_direction` 是"自动规则 / 用户手选 / 母语配置"三者汇合的唯一漏斗，
// 它错一点，主界面、划词、截图三条路会一起错。这里用内存库把它整条钉住。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod direction_tests {
    use super::*;

    fn state(native: &str, native_to: &str) -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        db::init_schema(&conn).unwrap();
        db::seed_default_settings(&conn).unwrap();
        // 直接写库、绕过 settings_set 的校验：既是为了方便，也是为了能造出"脏数据"场景。
        db::set_setting(&conn, "native_lang", native).unwrap();
        db::set_setting(&conn, "native_to", native_to).unwrap();
        AppState {
            db: Mutex::new(conn),
            dicts: Mutex::new(dict::DictCache::new()),
            screenshot: Mutex::new(None),
            popup_payload: Mutex::new(None),
            shot_payload: Mutex::new(None),
            paddle: Mutex::new(None),
            manual_dir: Mutex::new(ManualDir::default()),
            req_seq: std::sync::atomic::AtomicU64::new(0),
            latest_req: Mutex::new((String::new(), 0)),
        }
    }

    fn set_manual(st: &AppState, src: Option<&str>, tgt: Option<&str>) {
        *st.manual_dir.lock().unwrap() = ManualDir {
            src: src.map(String::from),
            tgt: tgt.map(String::from),
        };
    }

    fn d(st: &AppState, text: &str) -> (String, String, bool) {
        let r = resolve_direction(st, text, true);
        (r.src, r.tgt, r.src_manual)
    }

    #[test]
    fn auto_follows_the_native_rule() {
        let st = state("Chinese", "English");
        assert_eq!(d(&st, "hello"), ("English".into(), "Chinese".into(), false));
        assert_eq!(d(&st, "你好"), ("Chinese".into(), "English".into(), false));
        assert_eq!(
            d(&st, "こんにちは"),
            ("Japanese".into(), "Chinese".into(), false)
        );
    }

    #[test]
    fn native_pair_is_configurable() {
        // 学日语的人：中文 → 日文，其他外语仍然回中文
        let st = state("Chinese", "Japanese");
        assert_eq!(d(&st, "你好"), ("Chinese".into(), "Japanese".into(), false));
        assert_eq!(d(&st, "hello"), ("English".into(), "Chinese".into(), false));

        // 母语本身是日语的人
        let st = state("Japanese", "English");
        assert_eq!(
            d(&st, "こんにちは"),
            ("Japanese".into(), "English".into(), false)
        );
        assert_eq!(d(&st, "你好"), ("Chinese".into(), "Japanese".into(), false));
    }

    #[test]
    fn manual_target_wins_over_the_rule() {
        let st = state("Chinese", "English");
        set_manual(&st, None, Some("Japanese"));
        // 源仍交给引擎自动识别（src_manual=false），只有目标被钉住
        assert_eq!(d(&st, "hello"), ("English".into(), "Japanese".into(), false));
        assert_eq!(d(&st, "你好"), ("Chinese".into(), "Japanese".into(), false));
    }

    /// 用户手选源语言的**头号用途**：`東京` 这类只含汉字的日语，脚本层原理上判不出来
    /// （见 lang.rs 已知盲区），只能靠手选。此时 src_manual 必须是 true —— 它决定了
    /// 在线引擎收到的是 `sl=ja` 还是 `sl=auto`，而 auto 会把它当中文原样返回。
    #[test]
    fn manual_source_overrides_the_blind_spot() {
        let st = state("Chinese", "English");
        assert_eq!(
            d(&st, "東京"),
            ("Chinese".into(), "English".into(), false),
            "前提：自动判定确实会把它当中文"
        );
        set_manual(&st, Some("Japanese"), None);
        assert_eq!(d(&st, "東京"), ("Japanese".into(), "Chinese".into(), true));
    }

    /// 只钉源语言时，目标要按**钉住的那个源**现算，不能沿用按自动判定算出来的目标。
    /// 反例：文本是英文（自动 → English→Chinese），用户把源钉成 Chinese，
    /// 目标若沿用 auto_tgt 就还是 Chinese ⇒ 中译中。
    #[test]
    fn manual_source_recomputes_the_target() {
        let st = state("Chinese", "English");
        set_manual(&st, Some("Chinese"), None);
        assert_eq!(d(&st, "hello"), ("Chinese".into(), "English".into(), true));
    }

    /// **不能拿一个猜出来的源去顶掉用户明选的目标**（2026-08-07 对抗复核揪出）。
    ///
    /// 现场：目标选中文 + 翻 `東京都新宿区`（只含汉字的日语，脚本层必判成中文）。
    /// 旧逻辑：src==tgt 触发同语言保护 → 目标被悄悄改成英文 → 引擎自己识别出日语、译成英文。
    /// 用户明明选了"译成中文"，拿到英文，界面上还显示着中文。
    /// 关键在于：src_manual=false 时 src 只是我们的猜测，而且**根本不会发给引擎**（走 sl=auto），
    /// 它没有资格否决用户的显式选择。
    #[test]
    fn a_guessed_source_never_overrides_the_explicitly_chosen_target() {
        let st = state("Chinese", "English");
        set_manual(&st, None, Some("Chinese"));
        let r = d(&st, "東京都新宿区");
        assert_eq!(r.1, "Chinese", "用户明选的目标不能被改写");
        assert!(!r.2, "源仍是自动 ⇒ 交给引擎识别（它能认出这是日语）");

        // 混排也一样：`hello 世界` 判中文，用户选目标=中文时不许被顶成英文
        set_manual(&st, None, Some("Chinese"));
        assert_eq!(d(&st, "hello 世界").1, "Chinese");
    }

    /// 反过来：源是**用户手选**的（可信、且真会发给引擎）时，同语言保护仍要生效，
    /// 否则会拿 zh→zh 去打引擎，原样返回。
    #[test]
    fn same_language_protection_still_applies_when_source_is_trusted() {
        let st = state("Chinese", "English");
        set_manual(&st, Some("Chinese"), Some("Chinese"));
        let r = d(&st, "你好");
        assert_eq!(r.0, "Chinese");
        assert_eq!(r.1, "English", "两边都是用户选的且相同 ⇒ 按规则改目标");
    }

    #[test]
    fn same_language_is_never_produced() {
        let st = state("Chinese", "English");
        set_manual(&st, Some("Chinese"), Some("Chinese"));
        let r = d(&st, "你好");
        assert_eq!(r.0, "Chinese");
        assert_ne!(r.1, r.0, "src==tgt 会让引擎原样返回，等于没翻");
        assert_eq!(r.1, "English");

        // 病态配置：母语和"母语译成"设成同一个，也不能产出同语言
        let st = state("Chinese", "Chinese");
        set_manual(&st, Some("Chinese"), None);
        let r = d(&st, "你好");
        assert_ne!(r.1, r.0);
    }

    #[test]
    fn selection_can_ignore_manual_direction() {
        let st = state("Chinese", "English");
        set_manual(&st, Some("Japanese"), Some("Korean"));
        // follow_manual=false ⇒ 划词/截图完全按自动规则走（默认行为）
        let r = resolve_direction(&st, "hello", false);
        assert_eq!((r.src.as_str(), r.tgt.as_str(), r.src_manual), ("English", "Chinese", false));
        // follow_manual=true ⇒ 继承主窗手选
        let r = resolve_direction(&st, "hello", true);
        assert_eq!((r.src.as_str(), r.tgt.as_str(), r.src_manual), ("Japanese", "Korean", true));
    }

    /// 库里被塞进不认识的语言名（旧版本残留/手改）时必须回落，而不是把这个名字一路带到
    /// 引擎——Google 收到无效 tl 会返回 200 且**原样不翻译**，用户只会觉得软件坏了。
    #[test]
    fn broken_native_settings_fall_back_instead_of_reaching_the_engine() {
        let st = state("Klingon", "Elvish");
        assert_eq!(d(&st, "hello"), ("English".into(), "Chinese".into(), false));
        assert_eq!(d(&st, "你好"), ("Chinese".into(), "English".into(), false));
    }

    /// 「同一段文字换方向重译」时，走得慢的那次**不许**把历史行盖回旧方向。
    ///
    /// 前端的 reqId 只能取消显示、取消不了已发出的后端请求；而历史按 source_text UPSERT，
    /// 谁最后落库谁赢。旧请求撞上 token 过期+本地兜底要十秒量级，新请求走热 token 三百毫秒，
    /// 顺序反过来完全正常。（2026-08-07 对抗复核揪出）
    #[test]
    fn a_late_landing_stale_request_must_not_overwrite_history() {
        let st = state("Chinese", "English");
        // 同一段原文连发两次（模拟改方向后重译）
        let old = claim_request(&st, "hello");
        let new = claim_request(&st, "hello");
        assert!(is_superseded(&st, "hello", old), "旧的那次必须被判过期");
        assert!(!is_superseded(&st, "hello", new), "最新那次照常落库");

        // ⚠ 判据必须按**原文**比对：打字防抖期间各次原文不同，它们写的是不同的历史行，
        // 谁也不该压谁。用全局代次的话，前一个字的那次会被后一个字的那次误判成过期。
        let a = claim_request(&st, "hel");
        let b = claim_request(&st, "hell");
        assert!(!is_superseded(&st, "hel", a), "不同原文不该互相压");
        assert!(!is_superseded(&st, "hell", b));
    }

    /// 方向解析产出的两侧语言，必须都能在引擎语言表里查到码。
    /// 这条把"方向解析"和"引擎调用"两个模块的契约钉在一起。
    #[test]
    fn resolved_directions_are_always_translatable() {
        let st = state("Chinese", "English");
        for t in [
            "hello", "你好", "こんにちは", "안녕하세요", "Привет", "สวัสดี", "مرحبا", "123",
            "Γειά", "שלום", "नमस्ते",
        ] {
            let r = resolve_direction(&st, t, true);
            assert!(
                engine::online::is_supported(&r.src),
                "「{t}」判出的源语言 {} 不在语言表里",
                r.src
            );
            assert!(
                engine::online::is_supported(&r.tgt),
                "「{t}」判出的目标语言 {} 不在语言表里",
                r.tgt
            );
        }
    }
}
