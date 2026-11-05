//! 冒烟：**SFTP 断了会自动重连**（2026-11-05）。
//!
//! 用户原话：「重连终端后，sftp仍然没有重连……或者可以改成、有任何变化，
//! sftp自动重连，比如改变目录的时候、刷新的时候，发现 sftp断了就自动重连。」
//!
//! 覆盖三件事：
//! 1. **断了要看得出来**：worker 报「连接已断开」⇒ 面板标成未连接、
//!    状态栏写「连接已断开」、「重新连接 SFTP」那颗按钮重新可点；
//! 2. **刷新 / 换目录时自动重连**：断了之后点刷新 ⇒ 自动发起重连
//!    （不是弹个错让人自己处理）；
//! 3. **用户输入的路径别丢**：断着的时候在路径框敲了目录 ⇒ 连上之后
//!    自动跳过去（不用再敲一遍）。
//!
//! ⚠️ **不连真服务器**：事件是直接喂给面板的（`handle_event` 是公开的）。
//! 也**不泵主循环**——真连不上时 worker 会回一个「连接失败」，主循环一跑
//! 就会弹对话框（冒烟不需要那个）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::worker::PanelEvent;
use puttyplus::panel::{self, SftpPanel};

fn main() {
    gtk::init().expect("gtk init");

    // 别碰用户真实配置
    let home = std::env::temp_dir().join(format!("pp-sftp-reconnect-{}", std::process::id()));
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
        std::env::set_var("PUTTYPLUS_CAPTURE_DIR", home.join("capture"));
    }

    // 故意指一个没人监听的端口：worker 连不上，也不会连上
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 9;
    opts.user = "nobody".into();
    let panel = SftpPanel::new(&opts);
    let st = panel.st.clone();

    // ---- 1) 先假装连上了 ----
    panel::handle_event(&st, PanelEvent::Connected(Ok("/tmp".into())));
    assert!(st.conn_ok.get(), "喂了 Connected 之后应当是连着的");
    assert!(!st.connecting.get(), "连上了就不该还在「正在连接」");
    println!("[1] 连上了 ✓");

    // ---- 2) 会话断了 ⇒ 面板要看得出来 ----
    panel::handle_event(&st, PanelEvent::Disconnected);
    assert!(!st.conn_ok.get(), "断了之后 conn_ok 应当是 false");
    let (level, text, _) = st.status.borrow().clone().expect("状态栏该有内容");
    assert_eq!(text, "连接已断开", "状态栏没写「连接已断开」：{text}");
    assert!(
        matches!(level, puttyplus::theme::State::Err),
        "断线的状态级别应当是 Err"
    );
    println!("[2] 断开 ⇒ 状态栏写「{text}」✓");

    // ---- 3) 刷新 ⇒ 自动重连（不是弹错让人自己处理）----
    panel::on_refresh(&st, true);
    assert!(
        st.connecting.get(),
        "断了之后点刷新应当自动发起重连（connecting 应当为 true）"
    );
    let (_, text, _) = st.status.borrow().clone().expect("状态栏该有内容");
    assert!(
        text.contains("重新连接"),
        "状态栏没写「正在重新连接」：{text}"
    );
    println!("[3] 刷新 ⇒ 自动重连（状态栏：{text}）✓");

    // ---- 4) 断着的时候敲路径 ⇒ 连上之后要跳过去 ----
    panel::on_entry_activated(&st, true, "/etc");
    assert_eq!(
        st.pending_cd.borrow().clone(),
        Some("/etc".to_string()),
        "用户输入的路径没被记住"
    );
    println!("[4] 断着时输入的路径被记住了 ✓");

    // ---- 5) 重连成功 ⇒ 路径被消费掉，列表重新发出去 ----
    panel::handle_event(&st, PanelEvent::Connected(Ok("/tmp".into())));
    assert!(!st.connecting.get(), "连上了就不该还在「正在连接」");
    assert!(
        st.pending_cd.borrow().is_none(),
        "重连成功之后待跳的路径应当被用掉"
    );
    assert!(
        st.remote_list_pending.get(),
        "重连成功之后应当重新列一次目录"
    );
    println!("[5] 重连成功 ⇒ 回到原来的目录 ✓");

    let _ = std::fs::remove_dir_all(&home);
    println!("sftp_reconnect_smoke: OK");
}
