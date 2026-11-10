//! 冒烟：**终端重连后回到原来的目录**（用户 2026-11-10）。
//!
//! 用户原话：「终端短线重连后，应该还回到原来的目录」。
//!
//! 场景：终端里 `cd` 到某个目录 → 网络抖了一下 / 用户敲了 `exit` →
//! 会话断了 → 「重新连接终端」→ **新会话应该直接开在原来那个目录**
//! （而不是把用户丢回 home，还得自己再 `cd` 一遍）。
//!
//! 覆盖：
//! 1. 终端 `cd` 到一个目录 ⇒ 面板的**目录跟随轮询**观察到了它（面板用
//!    exec 问远端那个 shell 要它的 cwd —— 终端自己不知道，OSC 7 只有部分
//!    shell 会发）；
//! 2. 会话结束（敲 `exit`）；
//! 3. 「重新连接终端」⇒ 新会话就绪后**自动 `cd` 回原来那个目录**。
//!
//! ⚠️ **怎么证明「真的在那个目录」**：不能只看屏幕上出现了那个路径
//!    （我们自己发出去的 `cd` 命令本身就会把它显示出来）⇒ 用
//!    `echo PP_CWD=$(pwd)` 的**输出**判定（命令回显里是字面量
//!    `$(pwd)`，不会误判）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example term_cwd_restore_smoke
//!
//! 需要一个本地 sshd（公钥认证，端口 2233）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel;
use puttyplus::tabs::Tabs;

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
fn screen_text(tabs: &Tabs) -> String {
    tabs.current_terminal()
        .map(|t| t.screen_text(200, 400))
        .unwrap_or_default()
}

/// 屏幕上有没有 shell 提示符（各家不一样，都认）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

fn main() {
    gtk::init().expect("gtk init");

    // ⚠️ **先记住真 HOME**：下面把 HOME 指到临时目录（别碰用户真实配置），
    //    而**私钥路径要用真的那个 HOME 算**（指到临时目录就找不到密钥了
    //    ⇒ 面板的 SFTP 连不上 ⇒ 目录跟随轮询永远不跑）。
    let real_home = std::env::var("HOME").unwrap_or_default();

    // 别碰用户真实配置
    let home = std::env::temp_dir().join(format!("pp-cwd-restore-{}", std::process::id()));
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
        // 目录跟随轮询快一点（默认 2 秒；冒烟不用等那么久）
        std::env::set_var("PUTTYPLUS_FOLLOW_MS", "300");
    }

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
        .unwrap_or_else(|_| format!("{real_home}/.ssh/id_rsa"));

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: host.clone(),
        port,
        user: user.clone(),
        keyfile: Some(key),
        ..Default::default()
    };

    let tabs = Tabs::new();
    tabs.open(&opts);
    // 挂进窗口：VTE 没被 realize 就不处理 PTY 输出（屏幕永远空白）
    let win = gtk::Window::new();
    win.set_default_size(1000, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(300);

    let page = tabs.current_page_widget().expect("应有当前页");
    let st = tabs.current_panel_state().expect("应有当前面板");

    // 1) 连上：以**远端提示符**出现为准
    let up = wait_until(25_000, || has_prompt(&screen_text(&tabs)));
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        screen_text(&tabs)
    );
    println!("[1] 已连接 {host}:{port}");

    // 2) 终端里 cd 到一个目录（远端就是本机，所以本地建一个）
    let dir = std::env::temp_dir().join(format!("pp_cwd_restore_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_string_lossy().into_owned();
    tabs.current_terminal()
        .expect("应有终端")
        .feed_child(format!("cd {dir_s}\n").as_bytes());

    // 3) 面板的目录跟随轮询要**观察到**这个目录（终端自己不知道 cwd，
    //    是面板用 exec 问远端那个 shell 要来的）
    let seen = wait_until(20_000, || {
        panel::last_term_cwd(&st).as_deref() == Some(dir_s.as_str())
    });
    assert!(
        seen,
        "面板没观察到终端的新目录（轮询没跑起来？）\n观察到的是：{:?}",
        panel::last_term_cwd(&st)
    );
    println!("[2] 面板观察到终端的目录：{dir_s}");

    // 4) 会话结束（用户敲 exit）
    tabs.current_terminal()
        .expect("应有终端")
        .feed_child(b"exit\n");
    let gone = wait_until(15_000, || {
        let t = screen_text(&tabs);
        t.contains("logout") || t.contains("closed")
    });
    assert!(gone, "敲 exit 之后会话没结束\n屏幕内容：\n{}", screen_text(&tabs));
    println!("[3] 会话已结束");

    // 5) 「重新连接终端」
    tabs.reconnect_term_page(&page);
    let again = wait_until(30_000, || has_prompt(&screen_text(&tabs)));
    assert!(
        again,
        "重连后远端没出提示符\n屏幕内容：\n{}",
        screen_text(&tabs)
    );
    println!("[4] 已重建会话");

    // 6) **新会话在原来那个目录吗**？
    //
    //    ⚠️ **时序**：恢复目录的 `cd` 是**看见提示符之后**才补发的（见
    //    `feed_when_remote_ready`）⇒ 提示符一出现就问 `pwd`，可能问在
    //    `cd` 执行之前 ⇒ 问出来还是 home ⇒ 误判。
    //    ⇒ 先等那条 `cd` 落到屏幕上，再问；问出来的值不对就再问一次
    //    （`cd` 与下一次输入之间还有极小的窗口）。
    //
    //    ⚠️ **怎么证明「真的在那个目录」**：不能只看屏幕上出现了那个路径
    //    （我们自己发出去的 `cd` 命令本身就会把它显示出来）⇒ 用
    //    `echo PP_CWD=$(pwd)` 的**输出**判定（命令回显里是字面量
    //    `$(pwd)`，不会误判）。
    let got_cd = wait_until(20_000, || screen_text(&tabs).contains("cd '"));
    assert!(
        got_cd,
        "重连后**没有**自动 cd 回原来的目录\n屏幕内容：\n{}",
        screen_text(&tabs)
    );
    println!("[5] 重连后自动 cd 回原来的目录");

    let marker = format!("PP_CWD={dir_s}");
    let mut ok = false;
    for _ in 0..3 {
        tabs.current_terminal()
            .expect("应有终端")
            .feed_child(b"echo PP_CWD=$(pwd)\n");
        ok = wait_until(8_000, || screen_text(&tabs).contains(&marker));
        if ok {
            break;
        }
        pump(300);
    }
    assert!(
        ok,
        "重连后**没有**回到原来的目录（期望看到 {marker}）\n屏幕内容：\n{}",
        screen_text(&tabs)
    );
    println!("[6] 重连后就在原来的目录：{dir_s}");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
    println!("term cwd restore smoke ok");
}
