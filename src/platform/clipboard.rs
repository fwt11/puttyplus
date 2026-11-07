//! 剪贴板入口（跨平台版 阶段 C0）。
//!
//! ## 现状
//!
//! GTK4 的剪贴板（`gdk::Clipboard`）**本身就是跨平台的** ⇒ 这里只做收口。
//!
//! ## 平台差异在别处
//!
//! - **X11 选区直读**（`src/panel/xdnd11.rs`）：那是 X11 特有的协议，
//!   已整模块 `cfg(unix)`；Windows 上拖拽传文件走**剪贴板**这条路
//!   （`src/panel/clipfiles.rs` 的「粘贴上传」），不依赖 X11。
//! - Wayland 上没有「选区转换」那套 ⇒ 拖拽本来就走剪贴板兜底。

use gtk::prelude::*;

/// 系统剪贴板（拿不到就是没有剪贴板，比如无显示环境）。
pub fn clipboard() -> Option<gtk::gdk::Clipboard> {
    gtk::gdk::Display::default().map(|d| d.clipboard())
}

/// 读剪贴板里的纯文本（**异步**）。
///
/// ⚠️ GTK4 的剪贴板**只有异步读**（`read_text` 在 GTK4 里被删了）——
/// 同步读会卡住主线程（要去问剪贴板持有者）。
/// ⇒ 回调**最多被调用一次**（成功或失败各算一次）。
pub fn read_text<F>(cb: F)
where
    F: Fn(Option<String>) + 'static,
{
    let Some(clip) = clipboard() else {
        cb(None);
        return;
    };
    clip.read_text_async(None::<&gtk::gio::Cancellable>, move |r| {
        cb(r.ok().flatten().map(|s| s.to_string()))
    });
}

/// 写剪贴板里的纯文本。
pub fn write_text(text: &str) {
    if let Some(clip) = clipboard() {
        clip.set_text(text);
    }
}

// ⚠️ **这里不放单元测试**：本模块碰 GTK（`gdk::Display` / `Clipboard`），
// 而 `cargo test --lib` 把每个用例跑在**子线程**上 ⇒ GTK 在那儿起不来，
// 与并行用例互相踩（实测：单独跑绿、全量跑红）。
// ⇒ GTK 的断言一律放 example 冒烟（主线程）。

