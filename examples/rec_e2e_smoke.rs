//! 录制端到端冒烟（阶段 88）：**真 SSH 会话**里敲命令，录出来的 `.cast` 必须
//! 看得到**敲的过程**。
//!
//! 与 `rec_typing_smoke` 的分工：那个是**模拟** VTE（自己喂字节），这个是
//! **真会话**——连本地 sshd，让**真 shell** 回显，验证录制器在真实回显下也能
//! 把命令一个个敲出来。
//!
//! ⚠ **为什么命令里用每轮随机的怪 token**：用户的 zsh 开了**自动建议**
//! （敲到一半时行尾会带上一条历史命令的**灰色建议文本**）⇒ 屏幕上"敲了一半"
//! 的那一行**仍然含有完整命令**，断言就没法用"含不含完整命令"来判断中间态。
//! token 每轮都换 ⇒ 建议在第 6 个字符就与历史分叉、随即消失 ⇒ 断言才成立。
//!
//! 需要一个本地 sshd（公钥认证）。环境变量：
//! `PP_SSH_HOST`（默认 127.0.0.1）/`PP_SSH_PORT`（默认 2222）/
//! `PP_SSH_USER`（默认当前用户）/`PP_SSH_KEY`（默认 `~/.ssh/id_rsa`）。
//!
//! ⚠ 录制目录用 `PUTTYPLUS_CAPTURE_DIR` 指到临时目录，别写用户真实数据目录。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example rec_e2e_smoke
use gtk::glib;
use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::replay;
use puttyplus::term::TermPane;
use std::time::{Duration, Instant};
use vte4::TerminalExt;

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 等某个条件成立（最多 timeout 毫秒）。返回是否成立。
fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

/// 去掉 ANSI 转义，只留看得见的文字。
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for c2 in it.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 终端屏幕上的文字（范围给得宽松：终端没 realize 时行列数是 0）。
fn screen_text(pane: &TermPane) -> String {
    match pane
        .terminal()
        .text_range_format(vte4::Format::Text, 0, 0, 199, 399)
        .0
    {
        Some(t) => t.to_string(),
        None => String::new(),
    }
}

/// 屏幕上有没有 shell 提示符（各家不一样，都认）。
fn has_prompt(text: &str) -> bool {
    text.contains('$') || text.contains('#') || text.contains('➜')
}

/// 把录下来的字节喂进一个**真 VTE**（回放端就是这么播的），取前 `n` 行。
fn replay_rows(cast: &replay::Cast, n: i64) -> Vec<String> {
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    let vt = vte4::Terminal::new();
    vt.set_size(cast.cols.max(1), 24);
    vt.set_scrollback_lines(10_000);
    win.set_child(Some(&vt));
    win.present();
    pump(150);
    for ev in &cast.events {
        if ev.kind == replay::EventKind::Output {
            vt.feed(ev.data.as_bytes());
        }
    }
    pump(300);
    let rows: Vec<String> = (0..n)
        .map(|r| {
            puttyplus::term::ansi_rows(&vt, r, r)
                .first()
                .map(|s| plain(s))
                .unwrap_or_default()
        })
        .collect();
    win.close();
    rows
}

fn main() {
    gtk::init().expect("gtk init");

    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2222);
    let user = std::env::var("PP_SSH_USER")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "root".into());
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
    // 必须挂窗口：VTE 没 realize 就不处理 PTY 输出
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(300);

    let up = wait_until(25_000, || has_prompt(&screen_text(&pane)));
    assert!(
        up,
        "远端没出提示符（本地 sshd 在 {host}:{port} 吗？）\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    println!("[1] 已连接 {host}:{port}（用户 {user}）");

    // ---- 开始录制 ----
    let on = pane.toggle_recording().expect("开始录制失败");
    assert!(on, "录制没开起来");
    let path = pane.rec_path().expect("录制文件路径拿不到");
    println!("[2] 正在录制 → {path}");

    // ---- 一个字一个字地敲 ----
    //
    // ⚠ **不能只靠 sleep**：SSH 通道会把连着写的几个字**批量**送过去，
    // 远端回显也就一次性到达 ⇒ 采样器一步就看到了完整命令，抓不到中间态。
    // ⇒ 每写一个字就**等光标真的往前走一格**（回显到了），再等采样器跑一拍，
    // 这样每一拍的行文本都不同 ⇒ 中间态是确定的。
    // ⚠ **token 必须每轮都换**：上一轮敲过的命令会进 zsh 历史 ⇒ 下次敲 "echo "
    // 时**自动建议**会把那条旧命令整条显示出来 ⇒ 屏幕上"敲了一半"的那一行
    // 仍然含有完整命令 ⇒ 断言没法用"含不含完整命令"判断中间态。
    // ⇒ 用「随机数字 + 随机串」：第 6 个字符就与历史里的任何条目分叉，
    // 建议随即消失，后面的中间态才是干净的。
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let alphabet: Vec<char> = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".chars().collect();
    let mut rnd = seed | 1;
    let mut token = String::new();
    token.push(char::from_digit(seed % 10, 10).unwrap_or('7'));
    for _ in 0..8 {
        rnd = rnd.wrapping_mul(1103515245).wrapping_add(12345);
        token.push(alphabet[(rnd >> 16) as usize % alphabet.len()]);
    }
    let cmd = format!("echo {token}");
    // ⚠ **每字之间要留够时间**：比采样间隔（250ms）长 ⇒ 每一拍采样器看到的是
    // **不同的**行文本 ⇒ 中间态是确定的。
    // ⚠ 不能更快：SSH 通道会把连着写的几个字**批量**送过去，远端回显一次性
    // 到达 ⇒ 采样器一步就看到完整命令 ⇒ 抓不到中间态（实测 110ms 一字时
    // 时抓得到、时抓不到）。也不能用"光标列变了"当判据——用户的 zsh 提示符
    // 插件自己会重画（带时钟），光标会被挪走 ⇒ 判据立刻通过 ⇒ 又变回批量。
    for ch in cmd.chars() {
        pane.terminal().feed_child(ch.to_string().as_bytes());
        pump(350);
    }

    // 回车
    pane.terminal().feed_child(b"\n");
    let got = wait_until(15_000, || screen_text(&pane).contains(&token));
    assert!(
        got,
        "命令没跑出预期输出\n屏幕内容：\n{}",
        screen_text(&pane)
    );
    pump(700);

    // ---- 停录制 ----
    pane.stop_recording();
    assert!(!pane.is_recording(), "停了还在录");
    println!("[3] 已停录制");

    let path = std::path::PathBuf::from(&path);
    let cast = replay::load(&path).expect("应能解析 .cast");
    let outs: Vec<&replay::Event> = cast
        .events
        .iter()
        .filter(|e| e.kind == replay::EventKind::Output)
        .collect();
    println!(
        "[4] 录了 {} 条事件（其中输出 {} 条）",
        cast.events.len(),
        outs.len()
    );

    // ---- 断言 1：能看到**中间态**（命令是敲出来的，不是一次冒出来的）----
    let texts: Vec<String> = outs.iter().map(|e| plain(&e.data)).collect();
    for t in &texts {
        println!("  输出事件：{t:?}");
    }
    // token 的一部分（还没敲完）⇒ 敲的过程被录下来了
    let head = &token[..token.len() - 1];
    let mid = texts
        .iter()
        .any(|t| t.contains(head) && !t.contains(&token));
    let full = texts.iter().any(|t| t.contains(&token));
    assert!(
        mid,
        "没抓到敲到一半的样子（\"{head}\"）⇒ 敲的过程没录下来：{texts:?}"
    );
    assert!(full, "最终命令没录到：{texts:?}");

    // ---- 断言 2：回放画面里有命令、有输出，且**中间态没留下多余的行** ----
    let rows = replay_rows(&cast, 16);
    println!("---- 回放画面 ----\n{rows:?}");
    assert!(
        rows.iter().any(|r| r.trim_end() == token),
        "回放里没有命令的输出行：{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains(&token)),
        "回放里连命令都没有：{rows:?}"
    );
    // ⚠ 中间态（敲到一半）必须被**重写**掉，不能在画面上留一行
    assert!(
        !rows
            .iter()
            .any(|r| r.trim_end().ends_with(&token[..token.len() - 1])),
        "回放画面上留了一行敲到一半的命令（重写机制没配套好）：{rows:?}"
    );

    let _ = std::fs::remove_file(&path);
    println!("\n✔ 真会话里敲的命令，录出来是逐次敲出来的；回放里没有多余的半截行");
}
