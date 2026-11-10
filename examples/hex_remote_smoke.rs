//! **远端**非文本文件的 hex 打开冒烟。
//! 运行：cargo run --example hex_remote_smoke
//!
//! 用户 2026-11-10：「文本编辑器只能打开文本，对于非文本文件提供以 hex
//! 模式打开的选项……如果用户不同意，则无动作。」
//!
//! ⚠️ 远端路径**特有**的一件事：远端文件是**异步**读的 ⇒ 打开时会先开一个
//! 「正在读取…」的**占位页**。⇒ 用户点「取消」之后，那个占位页**必须撤掉**
//! （用户 2026-11-10：「可以弹出提示窗口，但是不应该出现编辑器」）⇒ 不然
//! 白占一个页签、底下还是空的。
//!
//! 需要本机测试 sshd（127.0.0.1:2233）。

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{dialogs, fileedit, PanelState, SftpPanel};

fn pump(n: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..n {
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
    // ⚠️ 私钥路径用**真 HOME** 算（别改 HOME，否则找不到密钥）
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

/// 找到「问要不要 hex 打开」的那个对话框。
fn find_hex_dialog() -> Option<gtk::Dialog> {
    for w in gtk::Window::list_toplevels() {
        let Some(d) = w.downcast_ref::<gtk::Dialog>() else {
            continue;
        };
        let labels = dialogs::button_labels(d);
        if labels.iter().any(|l| l.contains("hex")) {
            return Some(d.clone());
        }
    }
    None
}

/// 远端的一个**二进制**文件（Linux 上 ELF 一定是二进制）。
const REMOTE_BIN: &str = "/bin/ls";

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    let opts = test_opts();
    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);
    pump(20);

    // ---- 等 SFTP 连上 ----
    let ok = wait_until(30_000, || st.conn_ok.get());
    assert!(
        ok,
        "SFTP 没连上（本地 sshd 在 {}:{} 吗？）",
        opts.host, opts.port
    );
    println!("[1] SFTP 已连接 {}@{}:{}", opts.user, opts.host, opts.port);

    // ---- 2. 打开远端二进制 ⇒ 弹窗 ----
    fileedit::open_remote(&st, REMOTE_BIN, false);
    let got = wait_until(30_000, || find_hex_dialog().is_some());
    assert!(got, "打开远端非文本文件**没有**弹窗问「要不要以 hex 模式打开」");
    let dlg = find_hex_dialog().unwrap();
    println!("[2] 打开远端二进制 ⇒ 弹窗问「这不是文本文件」");

    // ⚠️ 此刻底下应该是一个**占位页**（内容还在路上）
    assert!(
        matches!(
            fileedit::load_state(&st, REMOTE_BIN),
            fileedit::LoadState::Loading(_)
        ),
        "远端文件还没读到，底下应当是「正在读取…」的占位页"
    );
    println!("[3] 底下是「正在读取…」的占位页（还没建正文）");

    // ---- 3. 点「取消」⇒ 占位页也要撤掉 ----
    dlg.response(gtk::ResponseType::Cancel);
    pump(40);
    assert_eq!(
        fileedit::is_hex(&st, REMOTE_BIN),
        None,
        "用户点了「取消」却还是开了 hex 编辑器（应当无动作）"
    );
    assert!(
        matches!(
            fileedit::load_state(&st, REMOTE_BIN),
            fileedit::LoadState::Absent
        ),
        "用户点了「取消」之后**占位页还留着**（白占一个页签，底下还是空的）"
    );
    println!("[4] 不同意 ⇒ 无动作：编辑器没了，占位页也撤了");

    // ---- 4. 再打开一次 ⇒ 点「以 hex 模式打开」⇒ 进 hex 模式 ----
    fileedit::open_remote(&st, REMOTE_BIN, false);
    let got = wait_until(30_000, || find_hex_dialog().is_some());
    assert!(got, "第二次打开远端二进制没弹窗");
    let dlg = find_hex_dialog().unwrap();
    dlg.response(gtk::ResponseType::Ok);
    pump(60);
    assert_eq!(
        fileedit::is_hex(&st, REMOTE_BIN),
        Some(true),
        "点了「以 hex 模式打开」之后**没有**进 hex 模式"
    );
    let body = fileedit::text_of(&st, REMOTE_BIN).expect("hex 模式应当有编辑器");
    assert!(
        body.starts_with("00000000  "),
        "正文应当是 hex 转储，实际开头 {:?}",
        &body[..body.len().min(30)]
    );
    // ELF 文件头：7f 45 4c 46
    assert!(
        body.contains("7f 45 4c 46"),
        "转储里应当能看到 ELF 魔数，实际 {:?}",
        &body[..body.len().min(80)]
    );
    println!("[5] 同意 ⇒ 远端文件以 hex 模式打开（能看到 ELF 魔数 7f 45 4c 46）");

    println!("\n✔ 全部通过：远端非文本文件先问 → 取消不留占位页 → 同意才进 hex 模式");
}
