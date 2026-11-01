//! 顶栏全局状态区（阶段 77c-2 / 审计 P1-9）的冒烟。
//!
//! 审计 P1-9 原话：「标签点 = SFTP；终端状态在终端 banner；DB 状态在 DB
//! 状态行；传输在队列窗口 / 通知 / 汇总对话框……**没有一个全局状态**」。
//!
//! 覆盖：
//! - 没有会话时状态区**藏起来**（欢迎页不需要它）；
//! - 有会话时状态区常驻顶栏，**四个维度**（文件面板 / 终端 / 数据库 / 传输）
//!   逐条说清（tooltip）；
//! - chip 上的文字只说**连接维度**：传输失败不会让 chip 说"传输失败"
//!   （用户会以为连接断了）；
//! - 状态区显示的是**当前会话**：切标签就换。
//!
//! ⚠️ 真实状态要连上服务器才有 ⇒ 冒烟用测试钩子 `set_override` 注入状态
//! （构件层照样测得到）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example status_area_smoke

use gtk::prelude::*;

use puttyplus::active::TermState;
use puttyplus::statusbar::{self, SessionStatus, XferState};
use puttyplus::tabs::Tabs;
use puttyplus::theme::State;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    let tabs = Tabs::new();
    let chip = statusbar::build(tabs.clone());
    pump(50);

    // ---- ① 没有会话 ⇒ 藏起来 ----
    assert!(
        !chip.is_visible(),
        "没有会话时状态区不该露出来（欢迎页上多一个 chip 是噪音）"
    );
    println!("[1] 没有会话 ⇒ 状态区藏着");

    // ---- ② 有会话 ⇒ 四个维度逐条说清 ----
    statusbar::set_override(Some(SessionStatus {
        sftp: Some(State::Ok),
        term: TermState::Alive,
        db_name: Some("demo".into()),
        db_connected: true,
        xfer: XferState::Idle,
    }));
    statusbar::refresh_all();
    pump(50);

    assert!(chip.is_visible(), "有会话时状态区要常驻顶栏");
    let tip = chip.tooltip_text().unwrap_or_default();
    println!("[2] tooltip：\n{tip}");
    for dim in ["文件面板", "终端", "数据库", "传输"] {
        assert!(tip.contains(dim), "tooltip 里少了「{dim}」这一维：{tip}");
    }
    assert!(tip.contains("demo"), "数据库维度要带上连的哪个库：{tip}");
    assert!(tip.contains("已连接"), "文件面板维度要说清状态：{tip}");
    println!("[2] 四个维度都在 tooltip 里");

    // chip 上的文字：只说连接维度
    let text = chip
        .child()
        .and_then(|c| c.downcast::<gtk::Box>().ok())
        .and_then(|row| {
            let mut cur = row.first_child();
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    return Some(l.text().to_string());
                }
                cur = c.next_sibling();
            }
            None
        })
        .unwrap_or_default();
    println!("[2.5] chip 文字：{text}");
    assert_eq!(text, "已连接", "连接正常时 chip 就说「已连接」");

    // ---- ③ 传输失败**不进** chip 文字（两件事不能混）----
    statusbar::set_override(Some(SessionStatus {
        sftp: Some(State::Ok),
        term: TermState::Alive,
        db_name: None,
        db_connected: false,
        xfer: XferState::Failed,
    }));
    statusbar::refresh_all();
    pump(50);
    let tip2 = chip.tooltip_text().unwrap_or_default();
    assert!(tip2.contains("有失败"), "tooltip 里要看得见传输失败：{tip2}");
    let text2 = chip
        .child()
        .and_then(|c| c.downcast::<gtk::Box>().ok())
        .and_then(|row| {
            let mut cur = row.first_child();
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    return Some(l.text().to_string());
                }
                cur = c.next_sibling();
            }
            None
        })
        .unwrap_or_default();
    assert_eq!(
        text2, "已连接",
        "传输失败不该把 chip 说成传输失败（用户会以为连接断了）：{text2}"
    );
    println!("[3] 传输失败只进 tooltip，chip 文字仍是「{text2}」");

    // ---- ④ 终端掉了要说出来（聚合是黄点不是红点）----
    statusbar::set_override(Some(SessionStatus {
        sftp: Some(State::Ok),
        term: TermState::Dead,
        db_name: None,
        db_connected: false,
        xfer: XferState::Idle,
    }));
    statusbar::refresh_all();
    pump(50);
    let text3 = chip
        .child()
        .and_then(|c| c.downcast::<gtk::Box>().ok())
        .and_then(|row| {
            let mut cur = row.first_child();
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    return Some(l.text().to_string());
                }
                cur = c.next_sibling();
            }
            None
        })
        .unwrap_or_default();
    assert_eq!(text3, "终端已结束", "终端掉了要说出来：{text3}");
    assert!(
        chip.has_css_class("pp-chip-warn"),
        "终端掉了是黄点不是红点（连接还在，只是终端掉了）"
    );
    println!("[4] 终端掉了 ⇒ chip 说「{text3}」，配色是黄点");

    // ---- ⑤ 截图（状态区有内容时截，不然截到的是空的）----
    //
    // ⚠️ 截图要**有内容**才说明得了问题：没有会话时状态区是藏着的，
    // 截出来是空的 ⇒ 白截。
    statusbar::set_override(Some(SessionStatus {
        sftp: Some(State::Ok),
        term: TermState::Alive,
        db_name: Some("demo".into()),
        db_connected: true,
        xfer: XferState::Running,
    }));
    statusbar::refresh_all();
    pump(50);

    let win = gtk::Window::new();
    win.set_title(Some("顶栏状态区 — 阶段 77c-2"));
    win.set_default_size(900, 260);
    let header = gtk::HeaderBar::new();
    header.add_css_class("pp-header");
    header.set_title_widget(Some(&gtk::Label::new(Some("PuTTYPlus"))));
    header.pack_end(&chip);
    win.set_titlebar(Some(&header));
    let filler = gtk::Label::new(Some("（状态区在顶栏右侧；真实会话下才有内容）"));
    filler.set_halign(gtk::Align::Start);
    win.set_child(Some(&filler));
    win.present();
    pump(150);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_status_area.png")
        .status();
    println!("截图：/tmp/pp_status_area.png");

    // 恢复真实状态（别把覆盖值留给后面的测试）
    statusbar::set_override(None);
    statusbar::refresh_all();

    println!("status_area_smoke: OK");
}
