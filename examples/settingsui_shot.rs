//! 首选项中心六个页的实机截图（审计 P1-4，阶段 69）。
//!
//! 用法：`cargo run --example settingsui_shot -- <页号 0..5>`
//! （页号 = 外观 0 / 终端 1 / 传输 2 / 安全 3 / 快捷键 4 / 高级 5）
//!
//! 走的是**真实入口**（`settingsui::open(page)`）⇒ 截图里就是用户看到的
//! 那六页。

use gtk::prelude::*;
use puttyplus::settingsui::{self, Page};

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

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let idx: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0);
    let page = Page::from_index(idx);

    settingsui::open(page);
    pump(600);
    println!("已打开首选项中心：{}（页号 {idx}）", page.title());
    println!("窗口标题：{}", settingsui::pages_notebook().is_some());

    // 停住等截图
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        pump(200);
    }
}
