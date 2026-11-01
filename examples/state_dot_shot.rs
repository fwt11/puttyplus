//! 标签状态点的**实机截图**（审计 P0-4，阶段 65）。
//!
//! **看什么**：标签上那个状态点，在**终端会话结束**之后不再是绿的，
//! 而是变成**黄点**（文件通道还连着，只是终端掉了）。
//!
//! **怎么造出这个场景**（真连，不造假）：
//! 1. 连本地测试 sshd（127.0.0.1:2233，公钥认证）；
//! 2. 等远端提示符出现（真的连上了）；
//! 3. 在终端里敲 `exit` —— 就是用户"会话结束"的真实操作；
//! 4. 等子进程退出 ⇒ 状态点应当变黄，tooltip 说清「终端：已结束」。
//!
//! ```text
//! xvfb-run -a cargo run --example state_dot_shot -- --hold
//! ```

use gtk::prelude::*;
use puttyplus::active::TermState;
use puttyplus::args::ConnOptions;
use puttyplus::panel;
use puttyplus::theme::{self, State, Theme};
use vte4::TerminalExt;

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

fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

/// 屏幕上有没有 shell 提示符（各家不一样，都认）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);
    let hold = std::env::args().any(|a| a == "--hold");

    // 本地测试 sshd：127.0.0.1:2233（公钥认证）
    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| "fwt".into());
    // ⚠️ 密钥要指**真实 HOME** 里的（截图时 HOME 指向临时目录，
    // 那儿没有 ~/.ssh/id_rsa）
    let key = std::env::var("PP_SSH_KEY").unwrap_or_else(|_| {
        let h = std::env::var("PP_REAL_HOME").unwrap_or_else(|_| "/home/user".into());
        format!("{h}/.ssh/id_rsa")
    });

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: host.clone(),
        port,
        user: user.clone(),
        keyfile: Some(key),
        ..Default::default()
    };

    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_title(Some("阶段 65 状态点"));
    win.set_default_size(1200, 800);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(500);

    let st = tabs.current_panel_state().expect("应有当前面板");

    // ---- 1) 等真的连上（远端提示符出现） ----
    let up = wait_until(25_000, || {
        tabs.current_terminal()
            .map(|t| {
                t.text_range_format(vte4::Format::Text, 0, 0, 199, 399)
                    .0
                    .map(|s| has_prompt(&s.to_string()))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(up, "没连上 {host}:{port}（本地测试 sshd 在跑吗？）");
    pump(300);
    println!("[1] 已连上 {host}:{port}");

    // 连上时：绿点
    {
        let (level, tip) = puttyplus::tabs::dot_state_for(&st);
        assert_eq!(level, Some(State::Ok), "连上了还不是绿点？{tip}");
        println!("[2] 连上时：{tip} ✓");
    }

    // ---- 2) 敲 exit（用户"会话结束"的真实操作） ----
    if let Some(t) = tabs.current_terminal() {
        t.feed_child(b"exit\n");
    }
    let gone = wait_until(20_000, || {
        panel::term_state(&st) == TermState::Dead
    });
    assert!(gone, "敲了 exit 之后状态还是 {:?}（结束事件没传到面板？）", panel::term_state(&st));
    pump(400);
    println!("[3] 会话已结束");

    // ---- 3) 状态点应当变**黄**（文件通道还在，只是终端掉了） ----
    {
        let (level, tip) = puttyplus::tabs::dot_state_for(&st);
        assert_eq!(
            level,
            Some(State::Warn),
            "终端都结束了还是绿点（审计 P0-4 就是这个）：{tip}"
        );
        assert!(tip.contains("终端：已结束"), "tooltip 要说清哪一部分坏了：{tip}");
        println!("[4] 终端结束后：{tip} ✓");
    }

    if hold {
        println!("（--hold：停住等截图，终端里能看到「会话已结束」条）");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            pump(200);
        }
    }
    println!("state_dot_shot: OK");
}
