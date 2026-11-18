//! 连接编辑对话框的**人眼核对图**（美化·二批 8）。
//!
//! 连接管理器窗口已经不存在了（阶段 77b：侧栏是唯一 CRUD 入口）⇒
//! 用户最常见的"管理器"就是这张**新建/编辑连接对话框**——美化改样式后
//! 跑一遍，三套主题各截一张，肉眼核对原语（卡片/分段/键帽/间距）有没有
//! 漏接到这层。
//!
//! ⚠️ **只摆出来看，不点保存** ⇒ 不写 connections.ini（AGENTS.md 规矩 6）。
//!
//! ```text
//! xvfb-run -a cargo run --example connmgr_shot
//! # 截图：/tmp/pp-connmgr-{light,dark,contrast}.png
//! ```

use gtk::prelude::*;
use puttyplus::connmgr;
use puttyplus::theme::{self, Theme};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("截图：{path}");
}

fn main() {
    gtk::init().expect("gtk init");

    for (t, name) in [
        (Theme::Light, "light"),
        (Theme::Dark, "dark"),
        (Theme::HighContrast, "contrast"),
    ] {
        theme::apply(t).then_some(()).expect("CSS 没装上");
        // ⚠️ 只展示，不保存：on_connect 回调永远不触发（我们不点按钮）
        connmgr::ui_new_connection(None, |_opts| {
            panic!("冒烟不该触发连接回调");
        });
        pump(500);
        shot(&format!("/tmp/pp-connmgr-{name}.png"));
        // 下一轮前把上一张对话框收掉（对话框是模态的，留着会挡后一张）
        pump(100);
        // Dialog 也是 Window 的子类 ⇒ 统一 downcast 成 Window 收掉即可
        for w in gtk::Window::list_toplevels() {
            if let Some(win) = w.downcast::<gtk::Window>().ok() {
                win.close();
            }
        }
        pump(200);
    }

    println!("connmgr_shot: OK");
    std::process::exit(0);
}
