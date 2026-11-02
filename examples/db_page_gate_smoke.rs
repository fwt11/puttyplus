//! 回归：**没连上数据库 ⇒ 不许出现数据库管理页面**。
//!
//! 用户 2026-11-02：「任何时候，必须连上数据库之后才允许显示数据库管理页面，
//! 不允许在还没连接上数据库时显示数据库管理页面」。
//!
//! 覆盖三条：
//! 1. `ensure`（只建控件）之后，**子标签里没有**「数据库」这一页；
//! 2. `mount` 在**没连上**时是**保险**——调用了也挂不上去；
//! 3. 连上之后（测试钩子）`mount` 才把页面挂出来，并且切过去。
//!
//! 另外验证「没有子标签容器」的独立窗口路径：没连上 ⇒ 窗口不弹出来。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_page_gate_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel, dbui, subtabs};

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
    // ⚠️ **别碰用户的真实配置**（数据库连接配置在 databases.ini）。
    let db_ini = std::env::temp_dir().join(format!("pp-db-gate-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&db_ini);
    unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ---- ① 子标签路径 ----
    {
        let panel = SftpPanel::new(&ConnOptions::default());
        // 阶段 12：连接页的子标签（真实运行时由 tabs.rs 建）
        *panel.st.sub_tabs.borrow_mut() = Some(subtabs::SubTabs::new());
        pump(100);
        let sub = panel.st.sub_tabs().expect("子标签容器");
        let before = sub.page_count();

        // 只建控件 ⇒ **页签还不该在**
        dbui::ensure(&panel.st);
        pump(150);
        check!(
            "只建控件时没有「数据库」页",
            sub.page_count() == before,
            format!("页签数 {} → {}", before, sub.page_count())
        );

        // 没连上 ⇒ mount 是**保险**，挂不上去
        dbui::set_connected_for_test(&panel.st, false);
        dbui::mount(&panel.st);
        pump(150);
        check!(
            "没连上时 mount 挂不上去",
            sub.page_count() == before,
            format!("页签数 {} → {}", before, sub.page_count())
        );

        // 连上 ⇒ 挂上去，并且切过去
        dbui::set_connected_for_test(&panel.st, true);
        dbui::mount(&panel.st);
        pump(250);
        check!(
            "连上之后页面挂出来了",
            sub.page_count() == before + 1,
            format!("页签数 {} → {}", before, sub.page_count())
        );
        check!(
            "连上之后切到数据库页",
            sub.current_kind() == panel::subtabs::PageKind::Db,
            format!("当前页种类 {:?}", sub.current_kind())
        );
    }

    // ---- ② 独立窗口路径（没有子标签容器）----
    {
        let panel = SftpPanel::new(&ConnOptions::default());
        dbui::ensure(&panel.st);
        pump(150);
        let visible = panel
            .st
            .db_ui
            .borrow()
            .as_ref()
            .map(|u| u.win.is_visible())
            .unwrap_or(false);
        check!("没连上时独立窗口不弹出来", !visible, "窗口已经弹出来了");

        dbui::mount_for_test(&panel.st);
        pump(400);
        let visible = panel
            .st
            .db_ui
            .borrow()
            .as_ref()
            .map(|u| u.win.is_visible())
            .unwrap_or(false);
        check!("连上之后独立窗口弹出来了", visible, "窗口没弹出来");
    }

    let _ = std::fs::remove_file(&db_ini);
    if fails.is_empty() {
        println!("db_page_gate_smoke: OK");
    } else {
        println!("db_page_gate_smoke: {} 条断言没过", fails.len());
        std::process::exit(1);
    }
}
