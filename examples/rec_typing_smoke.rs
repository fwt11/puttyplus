//! 录制冒烟：回放里看得到**敲命令的过程**（阶段 88）。
//!
//! 三条性质钉死：
//!   1. **渐进**：录下来的输出事件里能看到**中间态**（"$ l" → "$ ls" → …），
//!      而不是命令和输出**同时**冒出来（那是阶段 88 之前的毛病）；
//!   2. **一致**：把录下来的字节喂进一个**真 VTE**（回放端就是这么播的），
//!      回放出来的画面必须和原始终端的画面**逐行相同**
//!      ——命令不重复（不能出现两遍）、也不丢；
//!   3. **不丢**：敲得比采样还快（一个采样周期内敲完）时，
//!      命令**不能凭空消失**（那行从没显示过 ⇒ 必须照旧批量录进去）。
//!
//! 运行：cargo run --example rec_typing_smoke
use gtk::glib;
use gtk::prelude::*;
use puttyplus::replay;
use puttyplus::term;
use std::time::{Duration, Instant};
use vte4::prelude::*;

fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 去掉 ANSI 转义，只留**看得见的文字**（比画面用）。
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            // CSI 序列：`ESC [` 开头，到第一个 ASCII 字母结束
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

/// 取终端前 `n` 行的可见文字。
fn screen(t: &vte4::Terminal, n: i64) -> Vec<String> {
    (0..n)
        .map(|r| {
            term::ansi_rows(t, r, r)
                .first()
                .map(|s| plain(s))
                .unwrap_or_default()
        })
        .collect()
}

/// 把录制文件里的**输出字节**喂进一个**真 VTE**——回放端（`replayui`）就是这么播的。
fn replay_screen(cast: &replay::Cast, rows: i64) -> Vec<String> {
    let win = gtk::Window::new();
    win.set_default_size(900, 500);
    let vt = vte4::Terminal::new();
    vt.set_size(cast.cols.max(1), rows.max(1));
    vt.set_scrollback_lines(10_000);
    win.set_child(Some(&vt));
    win.present();
    pump(120);
    for ev in &cast.events {
        if ev.kind == replay::EventKind::Output {
            vt.feed(ev.data.as_bytes());
        }
    }
    pump(250);
    let s = screen(&vt, rows);
    win.close();
    s
}

/// 一条输出事件里**最后一段**行文本（重写事件是 `\r\x1b[K` + 行文本）。
fn last_line(data: &str) -> String {
    const MARK: &str = "\u{1b}[K";
    match data.rfind(MARK) {
        Some(i) => plain(&data[i + MARK.len()..]),
        None => {
            // 不是重写事件：取最后一个换行之后的文本
            let tail = data.rsplit('\n').next().unwrap_or("");
            plain(tail).trim_end_matches('\r').to_string()
        }
    }
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    let dir = std::env::temp_dir().join("pp_rec_typing_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("typing.cast");
    let _ = std::fs::remove_file(&path);

    /* ================= 第一段：一个字一个字地敲 ================= */
    let win = gtk::Window::new();
    win.set_default_size(900, 500);
    let t = vte4::Terminal::new();
    t.set_size(80, 24);
    t.set_scrollback_lines(10_000);
    win.set_child(Some(&t));
    win.present();
    pump(150);

    let cols = t.column_count().max(1);
    let rows = t.row_count().max(1);
    let mut rec = replay::Recorder::start(&path, cols, rows, "typing").expect("应能建 .cast");
    let mut sampler = term::RecSampler::new(term::cursor_row(&t));

    // 提示符先出现
    t.feed(b"$ ");
    pump(120);
    sampler.sample(&t, &mut rec);

    // 一个字一个字地敲 "ls -la"
    for ch in ["l", "s", " ", "-", "l", "a"] {
        t.feed(ch.as_bytes());
        pump(120);
        sampler.sample(&t, &mut rec);
    }

    // 回车 → 光标下移
    t.feed(b"\r\n");
    pump(120);
    sampler.sample(&t, &mut rec);

    // 远端的输出 + 下一个提示符
    t.feed(b"file1\r\nfile2\r\n$ ");
    pump(120);
    sampler.sample(&t, &mut rec);
    // 再采一次：新的提示符那行才被录下来
    sampler.sample(&t, &mut rec);
    rec.stop();

    let cast = replay::load(&path).expect("应能解析");
    let outs: Vec<&replay::Event> = cast
        .events
        .iter()
        .filter(|e| e.kind == replay::EventKind::Output)
        .collect();
    println!(
        "---- 录了 {} 条事件，其中输出 {} 条 ----",
        cast.events.len(),
        outs.len()
    );

    // ---- 性质 1：能看到中间态（命令是"敲"出来的，不是一次冒出来的）----
    let states: Vec<String> = outs.iter().map(|e| last_line(&e.data)).collect();
    for s in &states {
        println!("  输出事件：{s:?}");
    }
    assert!(
        states.iter().any(|s| s == "$ l"),
        "回放里必须能看到敲到一半的样子（\"$ l\"）：{states:?}"
    );
    assert!(
        states.iter().any(|s| s == "$ ls"),
        "回放里必须能看到 \"$ ls\" 这个中间态：{states:?}"
    );
    assert!(
        states.iter().any(|s| s == "$ ls -la"),
        "最终态 \"$ ls -la\" 也要在：{states:?}"
    );
    // 敲了 6 个字 ⇒ 至少有 6 个"逐步"的输出事件（老实现只有 1~2 条）
    let gradual = states.iter().filter(|s| s.starts_with("$ l")).count();
    assert!(
        gradual >= 6,
        "敲 6 个字就该有 6 个逐步的中间态（老实现只有 1 条 ⇒ 这条钉的是阶段 88）：{states:?}"
    );

    // ---- 性质 2：回放画面 == 原画面 ----
    let orig = screen(&t, 5);
    let replayed = replay_screen(&cast, 5);
    println!("---- 原始终端 ----\n{orig:?}\n---- 回放 ----\n{replayed:?}");
    for i in 0..orig.len() {
        assert_eq!(
            orig[i].trim_end(),
            replayed[i].trim_end(),
            "第 {i} 行：回放的画面必须和原始终端一样（命令不能重复、也不能丢）"
        );
    }
    // 命令行只该出现**一遍**
    let cmd_rows = replayed
        .iter()
        .filter(|s| s.trim_end() == "$ ls -la")
        .count();
    assert_eq!(
        cmd_rows, 1,
        "命令在回放里只能出现一遍（重写机制要是没配套好就会变成两遍）：{replayed:?}"
    );

    /* ================= 第二段：敲得比采样还快 ================= */
    let path2 = dir.join("fast.cast");
    let _ = std::fs::remove_file(&path2);
    let win2 = gtk::Window::new();
    win2.set_default_size(900, 500);
    let t2 = vte4::Terminal::new();
    t2.set_size(80, 24);
    t2.set_scrollback_lines(10_000);
    win2.set_child(Some(&t2));
    win2.present();
    pump(150);

    let mut rec2 = replay::Recorder::start(&path2, cols, rows, "fast").expect("应能建 .cast");
    let mut sampler2 = term::RecSampler::new(term::cursor_row(&t2));
    // 提示符 + 命令 + 回车 + 输出，**一口气**喂进去（相当于一个采样周期内敲完）
    t2.feed(b"$ cmd\r\nout\r\n");
    pump(150);
    sampler2.sample(&t2, &mut rec2);
    rec2.stop();

    let cast2 = replay::load(&path2).expect("应能解析");
    let replayed2 = replay_screen(&cast2, 3);
    println!("---- 敲得飞快：回放 ----\n{replayed2:?}");
    assert!(
        replayed2.iter().any(|s| s.trim_end() == "$ cmd"),
        "敲得比采样快时命令**不能凭空消失**：{replayed2:?}"
    );
    assert!(
        replayed2.iter().any(|s| s.trim_end() == "out"),
        "输出也不能丢：{replayed2:?}"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&path2);
    println!("\n✔ 回放里命令是一个字一个字敲出来的；画面和原始终端一致；敲得快也不丢");
}
