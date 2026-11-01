//! 主界面按钮图标化冒烟（阶段 39）。
//!
//! **用户要求**（2026-10-28）：「主界面上所有的按钮都改成图标按钮，尽量
//! 少用文字」。范围（用户确认）：**只改按钮条**——顶栏、文件面板、工作台
//! SQL 行；**弹窗对话框保持文字**（那些是表单动作，「保存修改」和「放弃
//! 修改」光看图标分不出来）。
//!
//! **这个冒烟钉住两条不变量**：
//! 1. 按钮条上的按钮是**纯图标**——child 是 `Image`，不是「图标 + 文字」
//!    的 Box；
//! 2. **每个都有 tooltip**——文字挪进 tooltip 了，不能既没图标旁白也没
//!    悬停提示（那样用户就完全不知道这个按钮是干什么的）。
//!
//! 少一条都会让界面悄悄退化回"一排字"，而界面不会报错、不会 panic。
//!
//! 用法（需要 X 显示，无桌面时用 `xvfb-run -a`）：
//!
//! ```text
//! cargo run --example icon_btn_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{dbui, SftpPanel};
use std::rc::Rc;

/// 按钮是不是**纯图标**：child 是 Image（而不是带文字的 Box）。
fn is_icon_only(b: &gtk::Button) -> bool {
    b.child().and_downcast::<gtk::Image>().is_some()
}

/// 断言「纯图标 + 有 tooltip」。
fn assert_icon_button(b: &gtk::Button, what: &str) {
    assert!(
        is_icon_only(b),
        "{what} 应当是纯图标按钮（child 是 Image），现在 child 是 {:?}",
        b.child().map(|c| c.type_().name().to_string())
    );
    let tip = b.tooltip_text().unwrap_or_default();
    assert!(
        !tip.trim().is_empty(),
        "{what} 必须有 tooltip（文字挪进 tooltip 了，不能什么都没有）"
    );
}

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- ① 文件面板的上传/下载按钮 ----
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 2233;
    opts.user = "fwt".into();
    let panel = SftpPanel::new(&opts);
    let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);

    let remote_pane = puttyplus::panel::pane::build_pane(&st, true);
    assert_icon_button(&remote_pane.transfer_btn, "文件面板的下载按钮");
    println!("[1] 文件面板的下载按钮是纯图标 + 有 tooltip");

    // ---- ② 工作台 SQL 行的四个按钮 ----
    // 把工作台开出来（没有子标签容器时它会退回独立窗口，冒烟里没关系）
    dbui::ensure(&st);
    pump(400);
    let btns = dbui::sql_row_buttons_now(&st);
    assert_eq!(
        btns.len(),
        4,
        "SQL 行应当有四个按钮（执行 / 执行选中 / 取消 / 历史），实际 {}",
        btns.len()
    );
    for (i, b) in btns.iter().enumerate() {
        let what = ["执行", "执行选中", "取消", "历史"][i];
        assert_icon_button(b, &format!("SQL 行的「{what}」按钮"));
    }
    println!("[2] SQL 行四个按钮都是纯图标 + 有 tooltip");

    println!("icon_btn_smoke: OK");
}
