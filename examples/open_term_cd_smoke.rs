//! 「在此处打开终端」的 `cd` 时机回归（堡垒机场景）。
//!
//! **用户实测的问题**：右键文件列表 →「在此处打开终端」，`cd` 命令发得太早，
//! 被远端的登录脚本吃掉，终端没进到那个目录。**不止堡垒机**——凡是会发
//! 一大堆欢迎消息、并在登录脚本里读一行的服务器，都会踩这个坑。
//!
//! **为什么会被吃掉**：堡垒机的登录脚本是这样跑的——
//!
//! ```text
//! Access granted. Press Return to begin session.
//! ↑ 这里 read 掉一整行才往下走
//! ```
//!
//! 我们若在 ssh 子进程刚起来时就写 `cd`，那一行正好被 `read` 当回车吃掉。
//!
//! **本例怎么测**：不连真堡垒机，而是放一个**假的 `ssh`**（还有 `plink`）
//! 到 `PATH` 最前面，让它复刻那段登录流程（banner → `read` 吃一行 → MOTD
//! → `exec bash`），然后走**生产代码路径**，断言新终端的 `pwd` 落在目标目录。
//!
//! ⚠ **修复前这个例子必挂**：`cd` 在子进程刚起来时就发出去，被登录脚本的
//! `read` 吃掉 ⇒ `pwd` 是家目录而不是目标目录；而且命令会被**本地回显**
//! 在 banner 上面（用户看到的正是这个现象）。
//!
//! **修法**（`src/term.rs::feed_when_remote_ready`）：
//!
//! - 等**看见 shell 提示符**再发 `cd` —— 落进的是真 shell，不会被任何
//!   登录脚本吃掉；
//! - 提示符等不到（它卡在登录脚本后面）⇒ 先发一个**空行探路**，喂登录
//!   脚本那回「按回车」，它走完了提示符就出来了；
//! - 本地 pty 切 raw 之前**不写**（否则会被本地回显，命令落在 banner 上面）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example open_term_cd_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::term::TermPane;
use vte4::TerminalExt;

/// 假的 `ssh`：复刻堡垒机的登录流程。
///
/// 关键在中间那个 `read`——真堡垒机会**读掉一整行**才往下走。
const FAKE_SSH: &str = r##"#!/bin/sh
# 假的 ssh：复刻堡垒机的登录流程（用户实测的那台）。
#
#   Using username "fwt".
#   Access granted. Press Return to begin session.
#   O & M session login successful
#   Last login: ...
#
# 关键：提示之后 **read 掉一整行** 才往下走 ⇒ 早发的 `cd` 行正好被吃掉。

# 真 ssh 要**先认证**、认证通过后才把本地 tty 切到 raw（ICANON/ECHO 关掉）。
# 这里用 sleep 模拟那段认证耗时（顺便让「早写」必然落在 ECHO 还开着的
# 窗口里 ⇒ 屏幕上会多出一行本地回显，就像用户实测看到的那样），
# 然后再切 raw —— 否则测试根本进不到「远端就绪」那一步。
sleep 0.5
stty -echo -icanon 2>/dev/null || true

echo "Using username \"fwt\"."
echo "Access granted. Press Return to begin session."

# 一大堆欢迎消息（用户实测：凡是发一堆欢迎消息的服务器都会踩坑）
i=0
while [ $i -lt 30 ]; do
  echo "Welcome to the server. This is welcome message line number $i."
  i=$((i+1))
done

# ← 登录脚本吃一行（堡垒机行为）
IFS= read -r _pp_eaten

echo ""
echo "O & M session login successful"
echo ""
echo "Authorized users only. All activities may be monitored and reported."
echo ""
echo "Last login: Tue Nov  3 13:43:56 2026 from 10.108.249.250"

exec /bin/bash --noprofile --norc -i
"##;

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

/// 终端屏幕上的文字（范围给得宽松，VTE 自己裁剪）。
fn screen_text(pane: &TermPane) -> String {
    let term = pane.terminal();
    match term.text_range_format(vte4::Format::Text, 0, 0, 199, 399).0 {
        Some(t) => t.to_string(),
        None => String::new(),
    }
}

/// 屏幕上有没有 shell 提示符（bash 默认 PS1 是 `bash-5.2$ `）。
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

/// 断言屏幕上**没有**本地回显出来的 `cd` 命令。
///
/// 假 ssh 把本地 tty 的 ECHO 关掉了 ⇒ 屏幕上只要出现 `cd '`，就说明
/// 那条命令是在 tty 切 raw **之前**写的（= 用户看到的「发得太早」）。
fn assert_no_local_echo(text: &str, where_: &str) {
    assert!(
        !text.contains("cd '"),
        "[{where_}] cd 被本地回显了 —— 命令发得太早（还没切 raw 就写了）\n屏幕：\n{text}"
    );
}

fn main() {
    // ---- 0) 隔离环境：假 ssh + 临时配置（别碰用户真实配置）----
    let tmp = std::env::temp_dir().join(format!("pp_open_term_cd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("建临时目录");
    let fake_bin = tmp.join("bin");
    std::fs::create_dir_all(&fake_bin).expect("建假 bin 目录");

    // 假 ssh / 假 plink（应用优先用 plink，所以两个都得放）
    for name in ["ssh", "plink"] {
        let p = fake_bin.join(name);
        std::fs::write(&p, FAKE_SSH).expect("写假 ssh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
                .expect("给假 ssh 加执行权限");
        }
    }

    let home = tmp.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin.display(), old_path);
    // SAFETY: 本例是单线程的，且这些环境变量都在任何 GTK/glib 调用**之前**
    // 设好（glib 初始化后再改环境变量会有数据竞争）。
    unsafe {
        std::env::set_var("PATH", &new_path);
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", tmp.join("settings.ini"));
        std::env::set_var("PUTTYPLUS_DB_INI", tmp.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", tmp.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_HIST_INI", tmp.join("history.ini"));
        std::env::set_var("PUTTYPLUS_COLS_INI", tmp.join("columns.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", tmp.join("recent.ini"));
        std::env::set_var("PUTTYPLUS_VAULT", tmp.join("vault.ini"));
        std::env::set_var("PUTTYPLUS_CAPTURE_DIR", tmp.join("capture"));
    }

    gtk::init().expect("gtk init");

    let target = tmp.join("target-dir");
    std::fs::create_dir_all(&target).expect("建目标目录");
    let target_s = target.to_string_lossy().into_owned();

    // 假的远端：host/port 根本没意义（假 ssh 不连网），但得给全，
    // 免得走别的分支（比如 askpass 自动应答）。
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 1,
        user: "fwt".into(),
        ..Default::default()
    };

    // ---- 1) 机制级：TermPane + 生产用的那一串字节 ----
    //
    // 用 `tabs::cwd_feed_cmd`（生产代码里喂给 pty 的就是它），保证测的
    // 是**真正发出去的那串字节**，不是测试自己拼的。
    let pane = TermPane::new(&opts);
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();

    let cmd = puttyplus::tabs::cwd_cmd(&target_s);
    assert!(
        cmd.contains(&format!("cd '{}'", target_s)),
        "命令里必须有 cd + 带引号的目录，实际是：{cmd:?}"
    );
    pane.set_ready_hook(move |term, what| {
        let bytes: &[u8] = match what {
            puttyplus::term::ReadyCmd::Cmd => cmd.as_bytes(),
            // 空行探路：喂给登录脚本的「按回车」
            puttyplus::term::ReadyCmd::Probe => b"\n",
        };
        term.feed_child(bytes);
    });

    let up = wait_until(40_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "假堡垒机没出提示符（脚本没跑起来？）\n屏幕：\n{}",
        screen_text(&pane)
    );
    assert_no_local_echo(&screen_text(&pane), "机制级");

    let got = wait_for_pwd(&pane, &target_s, 30_000);
    assert!(
        got,
        "新终端没进到 {target_s}（cd 被堡垒机登录脚本吃掉了）\n屏幕：\n{}",
        screen_text(&pane)
    );
    println!("[1] 机制级：堡垒机登录流程下 cd 生效（pwd = {target_s}）");

    // ---- 2) 真实入口：Tabs::open_term_at ----
    //
    // 右键菜单那条路最终调的就是它；这里验它开出来的新终端也进了目录。
    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win2 = gtk::Window::new();
    win2.set_default_size(1100, 700);
    win2.set_child(Some(&tabs.notebook));
    win2.present();
    pump(500);

    let st = tabs.current_panel_state().expect("应有当前面板");
    tabs.open_term_at(&st, &target_s);

    let newpane = {
        let t = tabs.tabs.borrow();
        let tab = t.first().expect("应有标签");
        let pages = tab.term_pages.borrow();
        assert_eq!(pages.len(), 1, "「在此处打开终端」没开出新终端页签");
        pages.last().unwrap().1.clone()
    };

    let up2 = wait_until(40_000, || has_prompt(&screen_text(&newpane)));
    assert!(
        up2,
        "open_term_at 开出的新终端没出提示符\n屏幕：\n{}",
        screen_text(&newpane)
    );
    assert_no_local_echo(&screen_text(&newpane), "真实入口");

    let got2 = wait_for_pwd(&newpane, &target_s, 30_000);
    assert!(
        got2,
        "open_term_at 开出的新终端没进到 {target_s}（cd 被登录脚本吃掉了）\n屏幕：\n{}",
        screen_text(&newpane)
    );
    println!("[2] 真实入口：Tabs::open_term_at 开出的新终端也进了 {target_s}");

    // 清理临时目录
    let _ = std::fs::remove_dir_all(&tmp);
    println!("open_term_cd smoke ok");
}
