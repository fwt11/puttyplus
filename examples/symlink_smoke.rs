//! **远端符号链接**冒烟（用户 2026-11-10 报的「打开 /bin 报错还留个编辑器」）。
//!
//! 用户原话：
//!
//! > 「bin 是个符号链接，当成文件打开之后会报错，这时候可以弹出提示窗口，
//! > 但是不应该出现编辑器。最好的方式是跟踪符号链接，打开对应的目录，
//! > 而不是打开编辑器。」
//!
//! 覆盖三条路径（远端就是本机 ⇒ 软链是本地真造的）：
//!
//! - **目录软链** ⇒ 双击**跟进去**（列它指向的目录），**不开编辑器**；
//! - **文件软链** ⇒ 双击照常**开编辑器**（读到的是软链指的那个文件的内容）；
//! - **断链**（指向不存在的路径）⇒ **只弹提示**，**不留编辑器页**。
//!
//! 走的是**真实入口**（`panel::on_row_activated`）—— 文件列表双击调的就是它。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example symlink_smoke
//!
//! 需要一个本地 sshd（公钥认证），默认 127.0.0.1:2233。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, fileedit, pane};

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

/// 远端列表里有没有这一行；有的话回它的（是不是目录、是不是软链）。
fn row_flags(st: &puttyplus::panel::PanelState, name: &str) -> Option<(bool, bool)> {
    let p = st.pane(true);
    let mut it = p.model.iter_first();
    while let Some(mut i) = it {
        let n: String = p.model.get(&i, pane::COL_NAME);
        if n == name {
            let isdir: bool = p.model.get(&i, pane::COL_ISDIR);
            // ⚠️ **不新增一列**（用户：本来就挤）⇒ 软链看**权限列**的首字符
            let perm: String = p.model.get(&i, pane::COL_PERM);
            return Some((isdir, pane::is_link_perm(&perm)));
        }
        it = if p.model.iter_next(&mut i) { Some(i) } else { None };
    }
    None
}

/// 这一行现在摆的是哪个图标。
fn row_icon(st: &puttyplus::panel::PanelState, name: &str) -> Option<String> {
    let p = st.pane(true);
    let mut it = p.model.iter_first();
    while let Some(mut i) = it {
        let n: String = p.model.get(&i, pane::COL_NAME);
        if n == name {
            let icon: String = p.model.get(&i, pane::COL_ICON);
            return Some(icon);
        }
        it = if p.model.iter_next(&mut i) { Some(i) } else { None };
    }
    None
}

/// 远端列表里有没有这一行（只看名字）。
fn has_row(st: &puttyplus::panel::PanelState, name: &str) -> bool {
    let p = st.pane(true);
    let mut it = p.model.iter_first();
    while let Some(mut i) = it {
        let n: String = p.model.get(&i, pane::COL_NAME);
        if n == name {
            return true;
        }
        it = if p.model.iter_next(&mut i) { Some(i) } else { None };
    }
    false
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 造场景：目录软链 / 文件软链 / 断链 ----
    let base = std::path::PathBuf::from(std::env::temp_dir())
        .join(format!("pp_symlink_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("target")).expect("建 target 目录");
    std::fs::write(base.join("target/hello.txt"), "软链指向的内容\n").expect("写 hello.txt");
    // 一个**普通文件**（用来比图标：文件图标 vs 链接图标）
    std::fs::write(base.join("plain.txt"), "普通文件\n").expect("写 plain.txt");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.join("target"), base.join("dirlink")).expect("造目录软链");
        std::os::unix::fs::symlink(base.join("target/hello.txt"), base.join("filelink"))
            .expect("造文件软链");
        std::os::unix::fs::symlink(base.join("nonexistent"), base.join("broken")).expect("造断链");
    }
    let dir = base.to_string_lossy().into_owned();
    let dirlink = format!("{dir}/dirlink");
    let filelink = format!("{dir}/filelink");
    let broken = format!("{dir}/broken");

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

    // ---- 0) 走到场景目录 ----
    panel::on_entry_activated(&st, true, &dir);
    assert!(
        wait_until(30_000, || st.pane_cwd(true) == dir && has_row(&st, "dirlink")),
        "没列到场景目录里的软链"
    );

    // ⚠️ **列表里软链是「软链自己」**：不是目录，但**是软链**
    //    （以前就是这里把它当成文件 ⇒ 双击去开编辑器 ⇒ 报错）。
    let (isdir, islink) = row_flags(&st, "dirlink").expect("列表里该有 dirlink");
    assert!(!isdir, "软链在列表里不该被当成目录");
    assert!(islink, "dirlink 该被认成软链（这是这次改动的关键）");
    // ⚠️ **图标也要区分**（用户：「链接用链接的图标，文件用文件的图标」）
    assert_eq!(
        row_icon(&st, "dirlink").as_deref(),
        Some(pane::ICON_LINK),
        "软链该摆链接图标"
    );
    assert_eq!(
        row_icon(&st, "plain.txt").as_deref(),
        Some(pane::ICON_FILE),
        "普通文件该摆文件图标"
    );
    assert_eq!(
        row_icon(&st, "target").as_deref(),
        Some(pane::ICON_DIR),
        "目录该摆目录图标"
    );
    println!("[0] 列表里 dirlink：isdir={isdir} islink={islink}");

    // ---- 1) 双击目录软链 ⇒ 跟进去，**不开编辑器** ----
    let before_windows = gtk::Window::list_toplevels().len();
    panel::on_row_activated(&st, true, "dirlink", isdir, islink);
    assert!(
        wait_until(30_000, || st.pane_cwd(true) == dirlink),
        "双击目录软链后**没有跟进去**（cwd 仍是 {}）",
        st.pane_cwd(true)
    );
    assert!(
        wait_until(30_000, || has_row(&st, "hello.txt")),
        "跟进去后没看到软链指向的目录里的内容"
    );
    assert_eq!(
        fileedit::load_state(&st, &dirlink),
        fileedit::LoadState::Absent,
        "目录软链不该开出编辑器页"
    );
    assert_eq!(
        gtk::Window::list_toplevels().len(),
        before_windows,
        "目录软链不该弹任何对话框"
    );
    println!("[1] 双击目录软链 ⇒ 跟进去（cwd={}），没开编辑器", st.pane_cwd(true));

    // ---- 2) 回到场景目录，双击文件软链 ⇒ 照常开编辑器 ----
    panel::on_entry_activated(&st, true, &dir);
    assert!(
        wait_until(30_000, || st.pane_cwd(true) == dir && has_row(&st, "filelink")),
        "没回到场景目录"
    );
    let (isdir, islink) = row_flags(&st, "filelink").expect("列表里该有 filelink");
    assert!(!isdir && islink, "filelink 该是软链");
    panel::on_row_activated(&st, true, "filelink", isdir, islink);
    assert!(
        wait_until(30_000, || matches!(
            fileedit::load_state(&st, &filelink),
            fileedit::LoadState::Loaded
        )),
        "文件软链该照常开编辑器并读到内容"
    );
    let e = fileedit::active_editor(&st).expect("应有编辑器");
    let text = fileedit::buffer_text(&e);
    assert!(
        text.contains("软链指向的内容"),
        "文件软链读到的内容不对：{text:?}"
    );
    println!("[2] 双击文件软链 ⇒ 打开编辑器，内容正确");

    // ---- 3) 双击断链 ⇒ 只弹提示，**不留编辑器页** ----
    panel::on_entry_activated(&st, true, &dir);
    assert!(
        wait_until(30_000, || st.pane_cwd(true) == dir && has_row(&st, "broken")),
        "没回到场景目录"
    );
    let (isdir, islink) = row_flags(&st, "broken").expect("列表里该有 broken");
    assert!(islink, "broken 该被标成软链");
    let before = gtk::Window::list_toplevels().len();
    panel::on_row_activated(&st, true, "broken", isdir, islink);
    assert!(
        wait_until(30_000, || gtk::Window::list_toplevels().len() > before),
        "断链该弹一句提示"
    );
    assert_eq!(
        fileedit::load_state(&st, &broken),
        fileedit::LoadState::Absent,
        "**断链不该留下编辑器页**（用户：可以弹提示，但不应该出现编辑器）"
    );
    println!("[3] 双击断链 ⇒ 弹了提示，没留编辑器页");

    // ---- 4) 读不出来 ⇒ 也不留编辑器页 ----
    //
    // ⚠️ 断链走的是「问路径类型」那条路（压根没进编辑器）⇒ 这里单独验
    // 「**进了编辑器但读不出来**」的情况：直接用编辑器的入口打开一个目录
    // （服务端 stat 说「这是目录」⇒ 读失败）。
    //
    // 用户原话：「这时候可以弹出提示窗口，但是不应该出现编辑器。」
    let dirpath = format!("{dir}/target");
    fileedit::open_remote(&st, &dirpath, false);
    assert!(
        wait_until(30_000, || matches!(
            fileedit::load_state(&st, &dirpath),
            fileedit::LoadState::Absent
        )),
        "读失败后不该一直停在转圈（占位页要关掉）"
    );
    assert_eq!(
        fileedit::load_state(&st, &dirpath),
        fileedit::LoadState::Absent,
        "**读失败不该留下编辑器页**（占位页要关掉，只留一个提示框）"
    );
    println!("[4] 读失败 ⇒ 弹了提示，没留编辑器页");

    // 清理：关掉可能还开着的对话框 + 删场景
    for w in gtk::Window::list_toplevels() {
        if let Some(w) = w.downcast_ref::<gtk::Window>() {
            if matches!(w.title().as_deref(), Some("无法打开") | Some("读取远程文件失败")) {
                w.destroy();
            }
        }
    }
    println!("symlink smoke ok");

    // ⚠️ 想截图时：PP_HOLD=1 ⇒ 窗口别关、场景目录也别删（外层用 scrot 抓屏）
    if std::env::var("PP_HOLD").is_ok() {
        panel::on_entry_activated(&st, true, &dir);
        wait_until(10_000, || st.pane_cwd(true) == dir && has_row(&st, "dirlink"));
        pump(60_000);
    }
    let _ = std::fs::remove_dir_all(&base);
}
