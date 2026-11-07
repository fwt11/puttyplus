//! 文件操作三项新功能的冒烟（阶段 32）。
//!
//! 覆盖：
//! - **「在此处打开终端」的 `cd` 机制**：新终端起来后真的进到指定目录
//!   （`pwd` 得是那个目录）——这是最容易"看着对其实没生效"的一环；
//! - **走真实入口**（`Tabs::open_term_at`）也成立：右键菜单那条路
//!   最终就是调到它；
//! - **终端上挂着 FileList 的 DropTarget**：拖文件到终端 = 上传到当前
//!   远程目录（VTE 自己的文本拖放接 String，两者不冲突）；
//! - **拖拽上传真的把文件传上去了**：拖到远程文件列表 / 拖到终端，
//!   最后都落到远端目录里（本地 sshd 就是本机，能直接验文件落地）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fileops_smoke
//!
//! 需要一个本地 sshd（公钥认证）。见本文件末尾的说明。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel;
use puttyplus::term::TermPane;
use vte4::TerminalExt;

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

/// 终端屏幕上的文字。
///
/// **范围要给得宽松**：终端没被 realize 时 VTE 的行列数是 0，按实际行列
/// 取就只有 1×1 的内容；这里直接要 200 行 × 400 列，让 VTE 自己裁剪。
fn screen_text(pane: &TermPane) -> String {
    let term = pane.terminal();
    match term.text_range_format(vte4::Format::Text, 0, 0, 199, 399).0 {
        Some(t) => t.to_string(),
        None => String::new(),
    }
}

/// 屏幕上有没有 shell 提示符。
///
/// **提示符各家不一样**：bash 是 `$`/`#`，zsh 是 `➜`——都认，别写死一种，
/// 否则换个 shell 的机器上这条冒烟必挂（本机的 zsh 就没有 `$`）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

/// 屏幕上有没有一行**内容正好是** `want`（`pwd` 的输出就是这种独占一行）。
fn has_line_equals(text: &str, want: &str) -> bool {
    text.lines().any(|l| l.trim() == want)
}

/// 反复问 `pwd`，直到看到 `want`。
///
/// ⚠️ **不能只问一次**：`cd` 是「就绪钩子」**异步**发的，可能比这一轮
/// `pwd` 晚到 ⇒ 只问一次会看到旧目录，误判成「cd 没生效」。
fn wait_for_pwd(pane: &TermPane, want: &str, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        pane.terminal().feed_child(b"pwd\n");
        let until = std::time::Instant::now() + std::time::Duration::from_millis(700);
        while std::time::Instant::now() < until {
            if has_line_equals(&screen_text(pane), want) {
                return true;
            }
            pump(50);
        }
    }
    false
}

/// 本地 sshd 的连接参数。
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

    // ---- 1) 「在此处打开终端」的 cd 机制（机制级）----
    //
    // 走的是 `TermPane::set_ready_hook`：**远端说完了话**才往 pty 写
    // `cd <目录>`（前面还带一个空行喂给堡垒机的登录脚本）。
    // ⚠️ 写早不行：pty 有行规程，早写会被本地回显 + 缓冲，转发给远端
    //    时远端可能还停在登录流程里 ⇒ 那行被吃掉，`cd` 不执行。
    let pane = TermPane::new(&opts);
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();

    // ⚠️ 用**生产代码**那一套（`tabs::cwd_cmd` + 就绪钩子）：
    //    `Cmd` = 发 `cd <目录>`；`Probe` = 只发一个空行
    //    （喂给堡垒机登录脚本的「按回车」）。
    let target = "/tmp";
    let cmd = puttyplus::tabs::cwd_cmd(target);
    pane.set_ready_hook(move |term, what| {
        let bytes: &[u8] = match what {
            puttyplus::term::ReadyCmd::Cmd => cmd.as_bytes(),
            puttyplus::term::ReadyCmd::Probe => b"\n",
        };
        term.feed_child(bytes);
    });

    // 等提示符出来，再问 pwd
    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {}:{} 吗？）\n屏幕内容：\n{}",
        opts.host,
        opts.port,
        screen_text(&pane)
    );

    // 先敲一条带标记的命令，证明终端**真的能执行**（不只是有提示符）。
    pane.terminal().feed_child(b"echo PP_ALIVE_9527\n");
    let alive = wait_until(15_000, || screen_text(&pane).contains("PP_ALIVE_9527"));
    assert!(alive, "终端连上了但不能执行命令");

    let got = wait_for_pwd(&pane, target, 30_000);
    assert!(
        got,
        "新终端没进到 {target}（pwd 的输出里没有它）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[1] 「在此处打开终端」的 cd 生效（pwd = {target}）");

    // ---- 2) 走真实入口：Tabs::open_term_at ----
    //
    // 右键菜单那条路最终调的就是它；这里直接验它开出来的新终端也
    // 进了指定目录（而不只是"函数被调到了"）。
    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win2 = gtk::Window::new();
    win2.set_default_size(1100, 700);
    win2.set_child(Some(&tabs.notebook));
    win2.present();
    pump(300);

    let st = tabs.current_panel_state().expect("应有当前面板");
    tabs.open_term_at(&st, target);

    // 新终端在 `term_pages` 里（子标签栏「+」新建的那些）
    let newpane = {
        let t = tabs.tabs.borrow();
        let tab = t.first().expect("应有标签");
        let pages = tab.term_pages.borrow();
        assert_eq!(pages.len(), 1, "「在此处打开终端」没开出新终端页签");
        pages.last().unwrap().1.clone()
    };

    let up = wait_until(30_000, || has_prompt(&screen_text(&newpane)));
    assert!(up, "新终端没出提示符\n屏幕内容：\n{}", screen_text(&newpane));
    let entered = wait_for_pwd(&newpane, target, 30_000);
    assert!(
        entered,
        "open_term_at 开出来的新终端没进到 {target}\n屏幕内容：\n{}",
        screen_text(&newpane)
    );
    println!("[2] Tabs::open_term_at 开出的新终端也进了 {target}");

    // ---- 3) 终端上挂着接本地文件的拖放接收器 ----
    //
    // 拖文件到终端 = 上传到当前远程目录。VTE 自己的拖放接 String（拖选
    // 文本），我们接 `text/uri-list`（本地文件），两者不抢。
    let term = st.term.borrow().clone().expect("应有终端");
    assert!(
        has_async_drop_target(&term),
        "终端上没挂接本地文件的拖放接收器——拖文件到终端不会触发上传"
    );
    println!("[3] 终端上挂着接本地文件的拖放接收器（拖文件进来 = 上传）");

    // ---- 4) 拖拽上传真的把文件传到远端 ----
    //
    // GTK 没有"模拟拖放"的 API，所以走的是**同一个上传入口**
    // （`drop_upload_files`）——DropTarget 回调和这里调的是同一段代码。
    let connected = wait_until(30_000, || st.conn_ok.get() && !st.remote_cwd.borrow().is_empty());
    assert!(connected, "SFTP 没连上（没法验上传）");

    let name = format!("pp_fileops_{}.txt", std::process::id());
    let local = std::path::PathBuf::from(std::env::temp_dir()).join(&name);
    std::fs::write(&local, b"puttyplus drag upload ok\n").expect("写临时文件");

    let fl = gtk::gdk::FileList::from_array(&[gtk::gio::File::for_path(&local)]);
    assert!(
        panel::drop_upload_files(&st, &fl),
        "拖拽上传入口拒绝了这次拖拽（没取到路径？）"
    );

    // 远端就是本机（本地 sshd），所以远端目录里的文件能直接 stat 到
    let remote = std::path::PathBuf::from(st.remote_cwd.borrow().clone()).join(&name);
    let landed = wait_until(30_000, || remote.exists());
    assert!(
        landed,
        "拖拽上传没把文件传到远端（期望落在 {}）",
        remote.display()
    );
    let content = std::fs::read_to_string(&remote).unwrap_or_default();
    assert!(
        content.contains("puttyplus drag upload ok"),
        "传过去的文件内容不对：{content:?}"
    );
    println!("[4] 拖拽上传落到远端并内容正确（{}）", remote.display());

    // 清理：本地和远端（同一个文件）都删掉，别留垃圾
    let _ = std::fs::remove_file(&local);
    let _ = std::fs::remove_file(&remote);

    // ---- 5) 右键菜单里有「在此处打开终端」，并弹出来截图 ----
    //
    // 菜单项是 Popover 里的按钮，Popover 是 TreeView 的子控件，所以
    // 顺着控件树能摸到它——**这比"函数被调到了"更接近用户看到的东西**。
    let view = st.pane(true).view.clone();
    let root = view.upcast_ref::<gtk::Widget>();
    let lbl = find_label(root, "在此处打开终端").expect("远程列表右键菜单里应有「在此处打开终端」");
    let btn = ancestor_button(lbl.upcast_ref::<gtk::Widget>()).expect("菜单文字外面得有按钮");
    assert!(
        own_visible(btn.upcast_ref::<gtk::Widget>()),
        "远程列表的「在此处打开终端」应该是可见的"
    );
    // ⚠️ **本地面板不该显示这一项**（本程序的终端全是远程 SSH 会话，
    // 没有本地 shell）。它是"建了但隐藏"，所以查的是它自己的 visible
    // 标志，不是"在不在控件树里"。
    let lview = st.pane(false).view.clone();
    if let Some(l2) = find_label(lview.upcast_ref::<gtk::Widget>(), "在此处打开终端") {
        let b2 = ancestor_button(l2.upcast_ref::<gtk::Widget>()).expect("菜单文字外面得有按钮");
        assert!(
            !own_visible(b2.upcast_ref::<gtk::Widget>()),
            "本地面板的「在此处打开终端」应该隐藏（没有本地 shell）"
        );
    }

    let pop = ancestor_popover(btn.upcast_ref::<gtk::Widget>()).expect("菜单项应该在 Popover 里");
    pop.popup();
    pump(800);
    shot("/tmp/pp_ctx_menu.png");
    println!("[5] 右键菜单里有「在此处打开终端」（本地面板没有）");

    // ---- 6) 「粘贴上传」这条兜底路能用 ----
    //
    // X11 的拖拽传文件要源程序提供选区数据，而 Deepin 文件管理器（Qt）
    // **声明了类型却从不提供数据** ⇒ 拖不进去是源侧的限制。它的**剪贴板
    // 是好的**（复制文件后剪贴板里有 `text/uri-list`），所以「粘贴上传」
    // 才是那条一定走得通的路。这里验它的解析部分。
    let clip_name = format!("pp_fileops_clip_{}.txt", std::process::id());
    let clip_local = std::path::PathBuf::from(std::env::temp_dir()).join(&clip_name);
    std::fs::write(&clip_local, b"puttyplus paste upload ok\n").expect("写临时文件");
    let clip_text = format!(
        "file://{}\n",
        clip_local.to_string_lossy().replace(' ', "%20")
    );
    let clip_paths = panel::clipfiles::local_paths_from_text(&clip_text);
    assert_eq!(
        clip_paths,
        vec![clip_local.to_string_lossy().into_owned()],
        "剪贴板里的 file:// URI 没解析成本地路径"
    );
    // 剪贴板里是普通文字时不该误判成文件
    assert!(
        panel::clipfiles::local_paths_from_text("随便一段文字\n/etc/passwdx\n").is_empty(),
        "剪贴板里的普通文字被误判成了文件"
    );
    let _ = std::fs::remove_file(&clip_local);
    println!("[6] 「粘贴上传」的剪贴板解析正常");

    println!("fileops smoke ok");
}

/// 在控件树里找文字等于 `text` 的 Label。
fn find_label(root: &gtk::Widget, text: &str) -> Option<gtk::Label> {
    fn walk(root: &gtk::Widget, text: &str, out: &mut Option<gtk::Label>) {
        if out.is_some() {
            return;
        }
        if let Some(l) = root.clone().downcast_ref::<gtk::Label>() {
            if l.text() == text {
                *out = Some(l.clone());
                return;
            }
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            walk(&c, text, out);
            if out.is_some() {
                return;
            }
            child = c.next_sibling();
        }
    }
    let mut out = None;
    walk(root, text, &mut out);
    out
}

/// 往上找第一个祖先按钮。
fn ancestor_button(w: &gtk::Widget) -> Option<gtk::Button> {
    let mut cur = w.parent();
    while let Some(c) = cur {
        if let Some(b) = c.clone().downcast_ref::<gtk::Button>() {
            return Some(b.clone());
        }
        cur = c.parent();
    }
    None
}

/// 往上找第一个祖先 Popover。
fn ancestor_popover(w: &gtk::Widget) -> Option<gtk::Popover> {
    let mut cur = w.parent();
    while let Some(c) = cur {
        if let Some(p) = c.clone().downcast_ref::<gtk::Popover>() {
            return Some(p.clone());
        }
        cur = c.parent();
    }
    None
}

/// 控件**自己**的 visible 标志（不看祖先）。
///
/// `is_visible()` 是"自己+祖先都可见"，弹层里的按钮在弹出前一律
/// false，测不出"这一项被隐藏了"。
fn own_visible(w: &gtk::Widget) -> bool {
    w.property::<bool>("visible")
}

/// 截图（Xvfb 下也要用 scrot，直接抓整个屏幕）。
fn shot(path: &str) {
    pump(600);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}

/// 终端上是否挂了**接本地文件**的拖放接收器。
///
/// ⚠️ 现在用的是 `gtk::DropTargetAsync`（不是同步版 `gtk::DropTarget`）：
/// 同步版在「读数据没回调」时会把自己**永久卡死**（`self->drop` 不清），
/// 之后这个控件上的所有拖拽全部失灵——详见 `panel::dnd` 的说明。
///
/// 只看"有没有拖放接收器"也不够——VTE 自带的是同步版（接 String，拖选
/// 文本），接的类型不对，拖文件进来照样没反应。
fn has_async_drop_target(term: &vte4::Terminal) -> bool {
    let model = term.observe_controllers();
    for i in 0..model.n_items() {
        let Some(c) = model.item(i) else { continue };
        if c.downcast_ref::<gtk::DropTargetAsync>().is_some() {
            return true;
        }
    }
    false
}

// 本地测试 sshd（公钥认证）。脚本在 `~/.cache/puttyplus-test-sshd` 起一个
// （主机密钥**固定放这里**，别放 /tmp —— /tmp 会被清理，host key 一变
// ssh 会因 `StrictHostKeyChecking=accept-new` 拒绝已变更的密钥而秒退）。
//
//   D=$HOME/.cache/puttyplus-test-sshd
//   /usr/sbin/sshd -f "$D/sshd_config" -E "$D/sshd.log"
