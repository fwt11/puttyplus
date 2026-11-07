//! 阶段 105：**双击远程文件后的载入反馈**冒烟。
//!
//! 用户 2026-11-07 报：「远程文件有时候比较大，或者网速比较慢，这时候双击
//! 打开编辑需要等待一段时间，但是现在缺少屏幕反馈，用户可能会以为双击打开
//! 没有成功。」
//!
//! 覆盖两件事：
//! - 双击后**立刻**有一个占位页（转圈 + 「正在读取 xxx…」）；
//! - 内容到了之后占位层**摘掉**，正文变成文件内容。
//!
//! 走的是**真实入口**（`fileedit::open_remote`）—— 文件列表双击调的就是它。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fileedit_load_smoke
//!
//! 需要一个本地 sshd（公钥认证），默认 127.0.0.1:2233。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::fileedit::{self, LoadState};

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

/// 本地测试 sshd 的连接参数。
fn test_opts() -> ConnOptions {
    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    });
    let key = std::env::var("PP_SSH_KEY")
        .unwrap_or_else(|_| format!("{}/.ssh/id_rsa", std::env::var("HOME").unwrap_or_default()));
    ConnOptions {
        proto: "ssh".into(),
        host,
        port,
        user,
        keyfile: Some(key),
        ..Default::default()
    }
}

fn main() {
    gtk::init().expect("gtk init");
    let opts = test_opts();

    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_default_size(1100, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(300);

    let st = tabs.current_panel_state().expect("应有当前面板");
    assert!(
        wait_until(30_000, || st.conn_ok.get() && !st.remote_cwd.borrow().is_empty()),
        "SFTP 没连上（本地 sshd 在 {}:{} 吗？）",
        opts.host,
        opts.port
    );

    // ---- 1) 双击远程文件 ⇒ 立刻有占位页 ----
    //
    // ⚠️ 远端就是本机（本地 sshd）⇒ 同一个路径两边都看得见，
    // 文件放在 /tmp 下，不碰用户家目录。
    let name = format!("pp_edit_load_{}.txt", std::process::id());
    let path = std::path::PathBuf::from(std::env::temp_dir()).join(&name);
    // 2MB 文本：够大（读回来要一会儿），又没到 8MB 的上限
    let mut body = String::with_capacity(2 * 1024 * 1024);
    for i in 0..40_000 {
        body.push_str(&format!("第 {i} 行：puttyplus 载入反馈冒烟\n"));
    }
    std::fs::write(&path, &body).expect("写临时文件");
    let rpath = path.to_string_lossy().into_owned();

    assert_eq!(
        fileedit::load_state(&st, &rpath),
        LoadState::Absent,
        "还没打开过，不该有编辑器"
    );

    // 双击远程文件走的就是这个入口（`panel::on_row_activated` → 这里）
    fileedit::open_remote(&st, &rpath, false);

    // ⚠️ **必须立刻断言**：这一拍里内容还没回来（回包要过 worker 线程
    // + 主循环），占位页一定在。
    match fileedit::load_state(&st, &rpath) {
        LoadState::Loading(msg) => {
            assert!(
                msg.contains(&name),
                "占位提示里该有文件名：{msg:?}"
            );
            println!("[1] 双击后立刻有占位页：「{msg}」");
        }
        other => panic!("双击后应立刻是占位状态，却是 {other:?}"),
    }

    // ---- 2) 内容到了 ⇒ 占位层摘掉，正文是文件内容 ----
    let got = wait_until(60_000, || {
        matches!(fileedit::load_state(&st, &rpath), LoadState::Loaded)
    });
    assert!(got, "内容一直没到位");
    assert_eq!(
        fileedit::load_state(&st, &rpath),
        LoadState::Loaded,
        "内容到位后不该再有占位层"
    );

    let e = fileedit::active_editor(&st).expect("应有编辑器");
    let text = fileedit::buffer_text(&e);
    assert!(
        text.contains("第 39999 行"),
        "正文内容不对（开头：{:?}）",
        &text[..text.len().min(80)]
    );
    assert!(
        !text.contains("正在读取"),
        "正文里还留着占位提示文字"
    );
    println!("[2] 内容到位：占位层摘掉，正文 {} 字节", text.len());

    // ---- 3) 关掉编辑器，清理临时文件 ----
    let _ = std::fs::remove_file(&path);
    println!("fileedit load smoke ok");
}
