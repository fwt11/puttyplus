//! 录制长输出回归冒烟（阶段 87）：一次性 2000 行输出必须**一行不落**。
//!
//! 这里踩过的坑（别再改回去）：
//!   `text_range_format(0, 0, rows-1, cols-1)` 取到的**不是屏幕**，
//!   而是**会话历史最前面的 rows 行**——那几行滚走后再也不变，
//!   于是每次采样都是"没变化"，**录到 0 行**。
//!   正确做法：拿光标的**绝对行号**（`cursor_position().1`），
//!   按 `[prev_row, cur_row-1]` 这个区间去取，滚动缓冲里还留着的行也取得到。
//!
//! 运行：cargo run --example rec_long_smoke
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::replay;
use puttyplus::term;
use std::time::{Duration, Instant};
use vte4::prelude::*;

const TOTAL: i64 = 2000;

fn pump(deadline: Instant) {
    let ctx = glib::MainContext::default();
    while Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    let win = gtk::Window::new();
    win.set_default_size(900, 500);
    let t = vte4::Terminal::new();
    t.set_size(80, 24);
    // 与应用里一致（term.rs 建终端时就是 10000 行）
    t.set_scrollback_lines(10_000);
    win.set_child(Some(&t));
    win.present();

    let pty = vte4::Pty::new_sync(vte4::PtyFlags::empty(), None::<&gio::Cancellable>).unwrap();
    t.set_pty(Some(&pty));
    let argv = ["/bin/sh", "-c", "sleep 1; seq 1 2000; sleep 10"];
    let argv_ref: Vec<&str> = argv.iter().copied().collect();
    let env: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let env_ref: Vec<&str> = env.iter().map(String::as_str).collect();
    pty.spawn_async(
        None::<&str>,
        &argv_ref,
        &env_ref,
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gio::Cancellable>,
        |_| {},
    );

    // 基线：开始录制时的光标行号
    pump(Instant::now() + Duration::from_millis(300));
    let mut prev_row = term::cursor_row(&t);
    let mut captured: Vec<String> = Vec::new();
    let mut polls = 0usize;

    // 采样：跟 `term::rec_poll` 同一套逻辑（公开出来给冒烟复用）
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        pump(Instant::now() + Duration::from_millis(250));
        polls += 1;
        let cur = term::cursor_row(&t);
        if cur <= prev_row {
            continue;
        }
        captured.extend(term::ansi_rows(&t, prev_row, cur - 1));
        prev_row = cur;
    }

    println!(
        "采样 {polls} 次，录到 {} 行（期望 {TOTAL} 行）",
        captured.len()
    );
    assert_eq!(
        captured.len() as i64,
        TOTAL,
        "长输出必须一行不落（实际 {} 行）",
        captured.len()
    );
    assert_eq!(
        captured.first().map(String::as_str),
        Some("1"),
        "第一行应当是 1"
    );
    assert_eq!(
        captured.last().map(String::as_str),
        Some("2000"),
        "最后一行应当是 2000"
    );

    // 对照组：**老做法**（取历史最前面的 rows 行）抓不到尾巴——
    // 这正是修复前"录到 0 行"的根因，留着当护栏。
    let rows = t.row_count().max(1);
    let cols = t.column_count().max(1);
    let (old_html, _) = t.text_range_format(vte4::Format::Html, 0, 0, rows - 1, cols - 1);
    let old = old_html.map(|h| h.to_string()).unwrap_or_default();
    assert!(
        !old.contains("2000"),
        "老做法取的是历史最前面几行，不该拿到末尾（若 VTE 改了语义，这条要重估）"
    );

    // 端到端：写进 .cast 再回放，确认真的能播出来
    let dir = std::env::temp_dir().join("pp_rec_long_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("long.cast");
    let _ = std::fs::remove_file(&path);
    let mut rec = replay::Recorder::start(&path, cols, rows, "long").expect("应能建 .cast");
    rec.output(&captured);
    rec.stop();

    let cast = replay::load(&path).expect("应能读回");
    assert_eq!(cast.skipped, 0, "不该有解析不掉的行");
    // 回放终端要**上窗口**：没上窗口时 VTE 不分配网格，喂多少行都读不出来
    let rwin = gtk::Window::new();
    rwin.set_default_size(900, 500);
    let rt = vte4::Terminal::new();
    rt.set_size(cast.cols, cast.rows);
    rwin.set_child(Some(&rt));
    rwin.present();
    for ev in &cast.events {
        if ev.kind == replay::EventKind::Output {
            rt.feed(ev.data.as_bytes());
        }
    }
    pump(Instant::now() + Duration::from_secs(2));
    let text = rt
        .text_format(vte4::Format::Text)
        .unwrap_or_default()
        .to_string();
    println!(
        "回放终端可见 {} 字符，末行 = {:?}",
        text.len(),
        text.lines().last()
    );
    assert!(text.contains("2000"), "回放里应看得到末尾的 2000");
    assert!(text.contains("1\n"), "回放里应看得到开头的 1");

    let _ = std::fs::remove_file(&path);
    println!("\n✔ 长输出录制：一行不落，且能回放");
}
