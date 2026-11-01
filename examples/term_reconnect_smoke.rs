//! 终端重建冒烟（阶段 25）：验证"会话不小心结束后能重新建立"。
//!
//! 用户场景（真实发生过）：在终端里敲了 `exit`，或者网络抖了一下，
//! 终端就成了一块"死画面"——光标还在闪，但敲什么都没反应。原先只有
//! 右键菜单里的「重新连接」能救，用户得**自己想到右键**；而且标签头
//! 右键里那一项是「重新连接 SFTP」，救的是另一条链路，点它终端纹丝不动。
//!
//! 覆盖：
//! - 连上本地 sshd → 敲 `exit` 让会话结束；
//! - 终端区出现「会话已结束 · 点此重新连接」条（不用右键就能看见）；
//! - 点它（这里用同一个内部动作 `reconnect_term`）→ 会话重新建立，
//!   条子收起，终端又能执行命令了。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example term_reconnect_smoke
//!
//! 需要一个本地 sshd（公钥认证）。脚本在 /tmp/pp_term_sshd 起一个：
//!   见本文件末尾的说明。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
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

/// 终端屏幕上的文字（用来确认重连后还能跑命令）。
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
/// 提示符各家不一样：bash 是 `$`/`#`，zsh 是 `➜`——都认，别写死一种，
/// 否则换个 shell 的机器上这条冒烟必挂。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

/// 把终端挂进窗口并显示出来。
///
/// **必须挂窗口**：VTE 只有在 widget 被 realize 之后才会处理 PTY 的输出
/// ——没挂窗口时终端永远是一片空白（看着像"连上了但没反应"）。
fn show_in_window(pane: &TermPane) -> gtk::Window {
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    win
}

fn main() {
    gtk::init().expect("gtk init");

    // 本地测试 sshd：127.0.0.1:2233（公钥认证）
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

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: host.clone(),
        port,
        user: user.clone(),
        keyfile: Some(key),
        ..Default::default()
    };

    let pane = TermPane::new(&opts);
    // 挂进窗口：VTE 没被 realize 就不处理 PTY 输出（屏幕永远空白）
    let _win = show_in_window(&pane);

    // 1) 连上：以**远端提示符**出现为准。
    //    别用"子进程活着"当连上的标志——连不上时子进程会秒退，那个
    //    活着的窗口只有几十毫秒，轮询很容易错过（表现就是"没起来"）。
    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(up, "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}", screen_text(&pane));
    println!("[1] 已连接 {host}:{port}");

    // 2) 让会话结束（用户在终端里敲 exit）
    pane.terminal().feed_child(b"exit\n");
    let gone = wait_until(15_000, || !pane.session_alive());
    assert!(gone, "敲 exit 之后子进程还活着？");
    println!("[2] 会话已结束（子进程退出）");

    // 3) 终端区应该摆出「重新连接」条
    assert!(
        pane.reconnect_banner_visible(),
        "会话结束后没有出现「会话已结束 · 点此重新连接」条"
    );
    println!("[3] 「会话已结束 · 点此重新连接」条已出现");

    // 4) 点它 → 重建会话
    pane.reconnect_term();
    // 重建后终端是清过屏的，等新的提示符出来
    let again = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(again, "点了重新连接，远端没再出提示符\n屏幕内容：\n{}", screen_text(&pane));
    assert!(
        !pane.reconnect_banner_visible(),
        "重连成功后「会话已结束」条没收起来"
    );
    println!("[4] 已重建会话，「会话已结束」条收起");

    // 5) 重建后的终端真的还能用（跑一条命令看输出）
    pane.terminal().feed_child(b"echo PP_RECONNECT_OK_9527\n");
    let got = wait_until(15_000, || screen_text(&pane).contains("PP_RECONNECT_OK_9527"));
    assert!(got, "重连后终端不能执行命令");
    println!("[5] 重连后终端可正常执行命令");

    // 第二段：终端子页签被关掉后要能重建
    subtab_rebuild_case(&opts);

    println!("term reconnect smoke ok");
}

/// 第二段：**终端子页签被关掉**之后，「重新连接终端」要把它**重建**出来。
///
/// 这是用户报的场景：把「终端」那个子标签的 × 点掉，标签还在（左边文件
/// 面板还在），但终端没了。这时候点「重新连接终端」如果只是重连那个已经
/// 看不见的对象，用户看着就是"点了没反应"——必须重新建一个页签出来。
fn subtab_rebuild_case(opts: &ConnOptions) {
    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(opts);
    // 同样要挂进窗口，终端才会真正跑起来
    let win = gtk::Window::new();
    win.set_default_size(1100, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(300);
    let st = tabs.current_panel_state().expect("应有当前面板");
    let sub = st.sub_tabs.borrow().clone().expect("应有子标签容器");
    let term_id = sub.id_by_title("终端").expect("应有「终端」子页");
    let page = tabs.current_page_widget().expect("应有当前页");

    // 用户把「终端」子页签关掉了
    assert!(sub.close_page(term_id), "终端子页签没关掉");
    assert!(
        sub.id_by_title("终端").is_none(),
        "关掉后还能按标题找到终端页签"
    );
    println!("[6] 「终端」子页签已被关掉");

    // 点「重新连接终端」
    tabs.reconnect_term_page(&page);
    let new_id = sub.id_by_title("终端").expect("终端子页签没有被重建出来");
    assert_ne!(new_id, term_id, "重建出来的还是原来那个页签");
    assert_eq!(
        sub.current_id(),
        Some(new_id),
        "重建后没有切到终端页签"
    );
    println!("[7] 「重新连接终端」重建出了终端页签（id {term_id} → {new_id}）");

    // 重建出来的终端是活的：自己会 spawn 一条 SSH 会话，出提示符才算数
    let again = wait_until(25_000, || {
        tabs.current_terminal()
            .map(|t| {
                t.text_range_format(vte4::Format::Text, 0, 0, 199, 399)
                    .0
                    .map(|s| has_prompt(&s.to_string()))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(again, "重建后的终端没有建立会话");
    println!("[8] 重建后的终端已建立会话");
}
