//! ghostty 后端的**真会话**冒烟（跨平台版 阶段 C4）。
//!
//! 前面几个冒烟（`ghostty_vt_smoke` / `ghostty_view_smoke`）只验了内核与
//! 控件；这个验**整条链路**：
//!
//! 1. 用 ghostty 后端连上本地 sshd（真实子进程、真实 PTY）；
//! 2. 远端**执行命令**并把输出显示出来（输出是我们自己的读线程从
//!    master 读出来、再喂给内核的——VTE 那条路完全不参与）；
//! 3. **PTY 的窗口大小跟终端对齐**（远端 `stty size` 报出来的行×列
//!    要和终端一致）——不同步的话远端会一直按 80×24 排版，画面看着
//!    正常其实全错位。
//!
//! ⚠️ 走的是**应用自己的会话流程**（`TermPane`），不是自己拼一套 ⇒
//!  验的就是「Windows 版将来要走的那条路」。
//!
//! 运行（无桌面时用 xvfb-run）：
//! ```text
//! xvfb-run -a cargo run --features ghostty --example ghostty_session_smoke
//! ```
//!
//! ⚠️ 需要 **Zig 0.17**（编译 libghostty-vt 用）与一个本地 sshd
//! （公钥认证，默认 127.0.0.1:2233；用 `PP_SSH_PORT` 等环境变量改）。

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
fn screen_text(pane: &TermPane) -> String {
    pane.terminal().screen_text(200, 400)
}

/// 屏幕上有没有 shell 提示符。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

fn main() {
    gtk::init().expect("gtk init");

    // ⚠️ **必须让应用用 ghostty 后端**（Linux 上默认是 VTE）。
    // 这个环境变量是 `TermPane::new` 读的（不是进程启动时读）⇒ 这里
    // 设置还来得及。
    //
    // SAFETY：单线程程序，此刻没有别的线程在读环境。
    unsafe {
        std::env::set_var("PP_TERM_BACKEND", "ghostty");
    }

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
    // ⚠️ **必须挂进窗口**：自绘控件要被 realize 才知道自己多大 ⇒
    // 才知道该给 PTY 设多大的窗口（远端靠它排版）。
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(800);

    /* ---------------- 1. 连上 ---------------- */
    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[1] ghostty 后端已连接 {host}:{port}");

    /* ---------------- 2. 跑命令，看到输出 ---------------- */
    pane.terminal().feed_child(b"echo PP_GHOSTTY_MARKER\n");
    let echoed = wait_until(15_000, || screen_text(&pane).contains("PP_GHOSTTY_MARKER"));
    assert!(
        echoed,
        "命令的输出没出现在屏幕上\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[2] 命令输出已上屏（PP_GHOSTTY_MARKER）");

    /* ---------------- 3. PTY 窗口大小跟终端对齐 ---------------- */
    //
    // ⚠️ 这是**远端**看到的屏幕大小：pty 的窗口大小没同步的话，
    // 远端会一直按 80×24 排版 ⇒ 报出来的行列和终端对不上。
    let cols = pane.terminal().column_count();
    let rows = pane.terminal().row_count();
    println!("    终端大小：{rows} 行 × {cols} 列");

    pane.terminal().feed_child(b"stty size\n");
    let want = format!("{rows} {cols}");
    let sized = wait_until(15_000, || {
        screen_text(&pane).contains(&want)
    });
    assert!(
        sized,
        "远端 `stty size` 没报出 {want}（pty 的窗口大小没同步？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[3] PTY 窗口大小已同步（远端 stty size = {want}）");

    /* ---------------- 4. 中文输出也正常 ---------------- */
    //
    // ⚠️ 远端的中文要**原样**过来（中间过了 ghostty 的 VT 解析）。
    pane.terminal().feed_child("echo 中文标记\n".as_bytes());
    let cn = wait_until(15_000, || screen_text(&pane).contains("中文标记"));
    assert!(
        cn,
        "中文输出没原样上屏\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[4] 中文输出原样上屏");

    /* ---------------- 5. 结束会话 ---------------- */
    pane.terminal().feed_child(b"exit\n");
    let gone = wait_until(15_000, || !pane.session_alive());
    assert!(gone, "敲 exit 之后子进程还活着？");
    println!("[5] 会话已结束");

    println!("\nghostty_session_smoke: 全部通过");
}
