//! **进程内 SSH**（阶段 C7）的真会话冒烟。
//!
//! 干什么用：Windows 上默认没有 `ssh.exe`、`plink` 又不想依赖 ⇒ 终端改成
//! **进程内实现 SSH 客户端**（russh 直接驱动终端内核，**不 spawn 任何外部
//! 程序**）。这个冒烟在 **Linux 上**就能把那条通路整条验掉：
//!
//! 1. 走**应用自己的会话流程**（`TermPane`）+ `PP_TERM_SSH=inproc`
//!    ⇒ 连上本地 sshd（真实 SSH 协议、真实 pty 请求）；
//! 2. 远程**执行命令**并把输出显示出来（输出是我们自己的通道喂给内核的，
//!    **没有子进程、没有本地 pty、没有 VTE 参与**）；
//! 3. **窗口大小跟终端对齐**（远程 `stty size` 报出来的行×列要和终端
//!    一致）——这一步验的是 SSH 的 **window-change**：进程内那条路没有
//!    本地 pty，尺寸只能靠 `window_change` 告诉远程 ⇒ 不同步的话远程
//!    会一直按最初的 80×24 排版。
//!
//! ⚠️ 需要 **Zig 0.17**（编译 libghostty-vt 用）与一个本地 sshd
//! （公钥认证，默认 127.0.0.1:2233；用 `PP_SSH_PORT` 等环境变量改）。
//!
//! 运行（无桌面时用 xvfb-run）：
//! ```text
//! ZIG=~/.cache/puttyplus-zig/bin/zig xvfb-run -a \
//!   cargo run --features ghostty --example ssh_shell_smoke
//! ```

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

    // ⚠️ **两个环境变量都要设**：
    // - `PP_TERM_BACKEND=ghostty`：Linux 上默认是 VTE，而**进程内 SSH
    //   只有 ghostty 后端能用**（VTE 的输入必须过一个本地 pty）；
    // - `PP_TERM_SSH=inproc`：强制走进程内 SSH（Windows 上是默认）。
    //
    // SAFETY：单线程程序，此刻没有别的线程在读环境。
    unsafe {
        std::env::set_var("PP_TERM_BACKEND", "ghostty");
        std::env::set_var("PP_TERM_SSH", "inproc");
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
    // 才知道该给远程发多大的窗口（`window_change`）。
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(800);

    /* ---------------- 1. 连上 ---------------- */
    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "进程内 SSH 没连上（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[1] 进程内 SSH 已连接 {host}:{port}（没有 spawn 任何外部程序）");

    /* ---------------- 2. 跑命令，看到输出 ---------------- */
    pane.terminal().feed_child(b"echo PP_INPROC_MARKER\n");
    let echoed = wait_until(15_000, || screen_text(&pane).contains("PP_INPROC_MARKER"));
    assert!(
        echoed,
        "命令的输出没出现在屏幕上\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[2] 命令输出已上屏（PP_INPROC_MARKER）");

    /* ---------------- 3. 窗口大小跟终端对齐 ---------------- */
    //
    // ⚠️ 这是**远程**看到的屏幕大小：进程内那条路没有本地 pty ⇒ 尺寸
    // 只能靠 SSH 的 window-change 告诉远程 ⇒ 不发的话远程会一直按
    // 最初的 80×24 排版。
    let cols = pane.terminal().column_count();
    let rows = pane.terminal().row_count();
    println!("    终端大小：{rows} 行 × {cols} 列");

    pane.terminal().feed_child(b"stty size\n");
    let want = format!("{rows} {cols}");
    let sized = wait_until(15_000, || screen_text(&pane).contains(&want));
    assert!(
        sized,
        "远程 `stty size` 没报出 {want}（window-change 没发？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[3] 窗口大小已同步（远程 stty size = {want}）");

    /* ---------------- 4. 中文能过去 ---------------- */
    //
    // ⚠️ 这一条验的是**字节通路**：中文是 UTF-8 多字节 ⇒ 通道要是按
    // 「字符」而不是「字节」处理，就会截断或乱码。
    pane.terminal().feed_child("echo PP_中文_OK\n".as_bytes());
    let cn = wait_until(15_000, || screen_text(&pane).contains("PP_中文_OK"));
    assert!(
        cn,
        "中文没过去（UTF-8 字节被截断了？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[4] 中文字节已过去（PP_中文_OK）");

    println!("\n✅ 进程内 SSH 冒烟全绿");
}
