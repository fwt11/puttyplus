//! 冒烟：**VTE 后端 + 进程内 SSH**（阶段 B2 的端到端验证）。
//!
//! 干什么用：阶段 B2 让 Linux **默认**的 VTE 后端也能用**进程内 SSH**
//! （russh）⇒ 终端与文件面板、端口映射、数据库工作台**共用同一条
//! SSH 连接**（用户 2026-11-11 的硬约束：「一台服务器一条连接」）。
//!
//! ⚠️ **VTE 以前只能用 spawn**（键盘输入必须过一个本地 pty）⇒ 这条
//!    通路是阶段 B2 的产物 ⇒ 这个冒烟就是它的守门人。
//!
//! 覆盖：
//! 1. VTE 后端连上本地 sshd（**进程内** SSH，不 spawn 任何 ssh 子进程）；
//! 2. 远端提示符出现 ⇒ **显示方向**通了（SSH → 终端）；
//! 3. 敲一条命令能看到输出 ⇒ **键盘方向**通了（终端 → 假 pty → SSH）；
//!    ⚠️ 顺便钉住「**没有双回显**」——本地 pty 的 `ECHO` 必须关着，
//!    否则标记会出现 **3** 次（本地回显 + 远程回显 + 命令输出）；
//! 4. 敲 `exit` ⇒ 会话结束 + 「会话已结束」条出现（**退出回调**通了）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example vte_inproc_smoke
//!
//! 需要一个本地 sshd（公钥认证）：默认 127.0.0.1:2233，
//! 可用 PP_SSH_HOST / PP_SSH_PORT / PP_SSH_USER / PP_SSH_KEY 覆盖。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
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

/// 终端屏幕上的文字。
///
/// ⚠️ **范围要给得宽松**：终端没被 realize 时 VTE 的行列数是 0。
fn screen_text(pane: &TermPane) -> String {
    pane.terminal().screen_text(200, 400)
}

/// 屏幕上有没有 shell 提示符（各家提示符不一样，别写死一种）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

fn main() {
    gtk::init().expect("gtk init");

    // ⚠️ **别碰用户真实配置**（AGENTS.md 第 6 条）
    let home = std::env::temp_dir().join(format!("pp-vte-inproc-{}", std::process::id()));
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

        // ⚠️ **强制走「VTE + 进程内 SSH」**：这两个开关就是阶段 B2 的
        //    产物（VTE 以前只能 spawn 外部 ssh ⇒ 用不了进程内 SSH）。
        std::env::set_var("PP_TERM_BACKEND", "vte");
        std::env::set_var("PP_TERM_SSH", "inproc");
    }
    assert!(
        puttyplus::term::use_inproc_ssh(),
        "PP_TERM_SSH=inproc 没生效（终端会走 spawn ⇒ 这个冒烟就没意义了）"
    );

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

    let pane = TermPane::new(&opts);
    // ⚠️ **必须挂进窗口**：VTE 只有在 widget 被 realize 之后才会处理
    //    PTY 的输出——没挂窗口时终端永远是一片空白。
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();

    // ---- 1) 连上：远端提示符出现（显示方向：SSH → 终端）--------------
    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[1] VTE 后端已用进程内 SSH 连上 {host}:{port}");

    // ⚠️ **必须真的走了进程内**（不是 spawn）：进程内那条路会向**连接中枢**
    //    要一条**共享连接** ⇒ 中枢里应当有 1 条活连接；spawn 那条路压根
    //    不碰中枢 ⇒ 0 条。
    let live = puttyplus::backend::hub::live_connections();
    assert_eq!(
        live.len(),
        1,
        "进程内 SSH 应当向连接中枢要了 1 条共享连接：{live:?}"
    );
    println!("[2] 连接中枢里有 1 条共享连接（确实走的是进程内）");

    // ---- 3) 敲命令 ⇒ 看输出（键盘方向：终端 → 假 pty → SSH）----------
    const MARK: &str = "PP_VTE_INPROC_OK_9527";
    pane.terminal()
        .feed_child(format!("echo {MARK}\n").as_bytes());
    // ⚠️ **等标记出现 2 次**（远程回显的命令行 + 命令的输出）——
    //    只等「出现 1 次」会撞上「本地回显先到」的时序 ⇒ 假阴性。
    let got = wait_until(15_000, || screen_text(&pane).matches(MARK).count() >= 2);
    assert!(
        got,
        "敲的命令没有输出（键盘字节没过去？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    // ⚠️ **不许双回显**：本地 pty 的 `ECHO` 关着 ⇒ 标记只出现 **2** 次；
    //    出现 3 次 = 本地也回显了（用户会看到两行一模一样的命令）。
    //    ⚠️ 多泵一会儿 ⇒ 让迟到的回显也有机会出现（不然这条会假阴性）。
    pump(800);
    let n = screen_text(&pane).matches(MARK).count();
    assert_eq!(
        n, 2,
        "标记出现了 {n} 次（应当是 2 次：命令行 + 输出）⇒ 本地 pty 在回显（ECHO 没关干净）"
    );
    println!("[3] 键盘输入已通过假 pty 送到远程（无双回显）");

    // ---- 4) 退出 ⇒ 会话结束 + 「会话已结束」条（退出回调）------------
    pane.terminal().feed_child(b"exit\n");
    let gone = wait_until(15_000, || !pane.session_alive());
    assert!(gone, "敲 exit 之后会话还活着？");
    assert!(
        pane.reconnect_banner_visible(),
        "会话结束后没有出现「会话已结束 · 点此重新连接」条"
    );
    println!("[4] 会话结束已收尾（退出回调生效）");

    println!("vte_inproc_smoke: 全部通过");
}
