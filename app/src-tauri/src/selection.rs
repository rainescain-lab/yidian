//! 划词取选区：模拟 Ctrl+C → 读剪贴板，含备份/轮询/恢复。
//! 必须在源程序仍为前台时调用（先取选区、再建 popup）。同步阻塞，放 spawn_blocking。

use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};
use std::{thread, time::Duration};

/// 取当前选中的文本；取不到返回 None。会尽力恢复原剪贴板文本。
pub fn grab_selection() -> Option<String> {
    let mut cb = Clipboard::new().ok()?;
    let backup = cb.get_text().ok(); // 仅文本备份
    let _ = cb.set_text(String::new()); // 空哨兵，便于判断是否复制到

    {
        let mut enigo = Enigo::new(&Settings::default()).ok()?;
        // 热键 Alt+W 的 Alt 此刻多半还按着 → 直接发 Ctrl+C 会变成 Alt+Ctrl+C 复制不到。
        // 先给用户松手的时间，再显式松开各修饰键，清掉残留按下状态。
        thread::sleep(Duration::from_millis(140));
        let _ = enigo.key(Key::Alt, Release);
        let _ = enigo.key(Key::Meta, Release);
        let _ = enigo.key(Key::Shift, Release);
        let _ = enigo.key(Key::Control, Release);
        thread::sleep(Duration::from_millis(30));
        enigo.key(Key::Control, Press).ok()?;
        enigo.key(Key::Unicode('c'), Click).ok()?;
        enigo.key(Key::Control, Release).ok()?;
    }

    // Ctrl+C 异步：目标程序在自己消息循环里填剪贴板，轮询到非空更稳
    let mut sel = None;
    for _ in 0..15 {
        thread::sleep(Duration::from_millis(20));
        if let Ok(t) = cb.get_text() {
            if !t.is_empty() {
                sel = Some(t);
                break;
            }
        }
    }

    if let Some(b) = backup {
        let _ = cb.set_text(b); // 读完再恢复
    }
    sel
}
