//! 冒烟：**终端与文件面板共用同一条 SSH 连接**（连接中枢的端到端验证）。
//!
//! 用户 2026-11-11 的原话：
//! 「我们可以保证对每个服务器的访问只需要一个连接。」
//!
//! 这个冒烟开**真窗口 + 真面板 + 真终端**，两者指向同一台服务器，然后断言：
//! 1. 终端（进程内 SSH）连上 ⇒ 全局只有 **1 条**连接；
//! 2. 文件面板（同一台服务器）连上 ⇒ **还是 1 条**连接（不是 2 条！）；
//! 3. 这条连接的**使用者数 ≥ 2**（终端 + 面板都攥着它）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   PP_TERM_SSH=inproc PP_TERM_BACKEND=ghostty \
//!     xvfb-run -a cargo run --example hub_shared_gui_smoke
//!
//! 需要一个本地 sshd（公钥认证）：默认 127.0.0.1:2233，
//! 可用 PP_SSH_HOST / PP_SSH_PORT / PP_SSH_USER / PP_SSH_KEY 覆盖。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::hub;
use puttyplus::panel::SftpPanel;
use puttyplus::term::TermPane;

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
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

/// 等某个条件成立（最多 timeout 毫秒）。返回是否成立。
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

/// 屏幕上有 shell 提示符（各家提示符不一样，别写死一种）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

fn main() {
    gtk::init().expect("gtk init");

    // ⚠️ **别碰用户真实配置**：测试不许写用户真实配置（AGENTS.md 第 6 条）
    let home = std::env::temp_dir().join(format!("pp-hub-gui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", home.join("settings.ini"));
        std::env::set_var("PUTTYPLUS_DB_INI", home.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", home.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_HIST_INI", home.join("history.ini"));
        std::env::set_var("PUTTYPLUS_COLS_INI", home.join("columns.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", home.join("recent.ini"));
        std::env::set_var("PUTTYPLUS_VAULT", home.join("vault.bin"));
    }

    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    });
    // ⚠️ **HOME 已经被改成临时目录了** ⇒ 私钥要从**真实的 HOME** 找
    let real_home = std::env::var("REAL_HOME").unwrap_or_else(|_| "/home/fwt".to_string());
    let key = std::env::var("PP_SSH_KEY")
        .unwrap_or_else(|_| format!("{real_home}/.ssh/id_rsa"));

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: host.clone(),
        port,
        user: user.clone(),
        keyfile: Some(key),
        ..Default::default()
    };

    // ---- 1. 终端先连（进程内 SSH ⇒ 走共享连接） ------------------------
    let pane = TermPane::new(&opts);
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    let up = wait_until(25_000, || {
        has_prompt(&pane.terminal().screen_text(200, 400))
    });
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        pane.terminal().screen_text(200, 400)
    );
    let live = hub::live_connections();
    assert_eq!(live.len(), 1, "终端连上后应当只有 1 条连接：{live:?}");
    println!("[1] 终端已连上，全局连接数 = {}", live.len());

    // ---- 2. 文件面板连同一台服务器 ⇒ **不该多出一条连接** --------------
    let panel = SftpPanel::new(&opts);
    let panel_win = gtk::Window::new();
    panel_win.set_default_size(900, 600);
    panel_win.set_child(Some(panel.widget()));
    panel_win.present();

    // ⚠️ **等「面板真的连上了」**：不能用 `live_connections()` 的使用者数
    //    当信号（面板可能还在建连途中，那时的数字没有意义）。
    let got = wait_until(25_000, || panel.conn_ok());
    assert!(got, "文件面板没连上（本地 sshd 在 {host}:{port} 吗？）");
    // 给 worker 一点时间把连接交完（面板连上 ⇒ 它已经拿到了共享连接）
    pump(300);

    // ---- 3. **核心断言**：指向同一台服务器 ⇒ 仍然只有 1 条连接 --------
    let live = hub::live_connections();
    assert_eq!(
        live.len(),
        1,
        "终端 + 文件面板指向同一台服务器 ⇒ 应当仍然只有 1 条连接：{live:?}"
    );
    assert!(
        live[0].1 >= 2,
        "这条连接应当有 ≥2 个使用者（终端 + 面板）：{live:?}"
    );
    assert_eq!(
        hub::conn_count(),
        1,
        "全局实际活着的连接数应当是 1：{:?}",
        hub::live_connections()
    );
    println!(
        "[2] 文件面板也连上了 ⇒ 连接数仍是 1，使用者数 = {}",
        live[0].1
    );

    println!("hub_shared_gui_smoke: 全部通过");
}
