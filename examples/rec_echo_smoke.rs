//! 录制冒烟：敲的命令照样在回放里 + 密码不会进文件（阶段 87 续）。
//!
//! 两条性质钉死：
//!   1. **不录输入事件，回放里照样看得到敲的命令**——远端 tty 的回显
//!      本身就是输出字节（敲 `ls` → 录到的输出行是 `["$ ls", ...]`）。
//!      没有这一条，"不录输入"就等于"输出凭空冒出来"。
//!   2. **文件里不会有明文密码**：密码输入时远端把 echo 关掉，那串字节
//!      不会回传（客户端却抓得到）⇒ 录进文件就是事故。
//!
//! 运行：cargo run --example rec_echo_smoke
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

/// 抓一次"新渲染出来的行"（与 `term::rec_poll` 同一套逻辑）。
fn capture(t: &vte4::Terminal, prev: &mut i64) -> Vec<String> {
    let cur = puttyplus::termview::vte::cursor_row(t);
    if cur <= *prev {
        return Vec::new();
    }
    let rows = puttyplus::termview::vte::ansi_rows(t, *prev, cur - 1);
    *prev = cur;
    rows
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }
    let win = gtk::Window::new();
    win.set_default_size(900, 500);
    let t = vte4::Terminal::new();
    t.set_size(80, 24);
    t.set_scrollback_lines(10_000);
    win.set_child(Some(&t));
    win.present();

    let dir = std::env::temp_dir().join("pp_rec_echo_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("echo.cast");
    let _ = std::fs::remove_file(&path);
    let cols = t.column_count().max(1);
    let rows = t.row_count().max(1);
    let mut rec = replay::Recorder::start(&path, cols, rows, "echo").expect("应能建 .cast");

    let mut prev = puttyplus::termview::vte::cursor_row(&t);

    /* ---- 1. 普通命令：远端回显 "ls" ---- */
    t.feed(b"$ ");
    pump(150);
    let got = capture(&t, &mut prev);
    rec.output(&got);

    // 用户敲了 "ls" 并回车 → 远端 tty 回显 + ls 的输出
    t.feed(b"ls\r\nfile1\r\nfile2\r\n");
    pump(200);
    let got = capture(&t, &mut prev);
    rec.output(&got);
    let joined = got.concat();
    println!("普通命令录到 {} 行：{got:?}", got.len());
    assert!(
        joined.contains("ls"),
        "敲的命令必须出现在录制的输出里（回显也是输出字节）：{got:?}"
    );
    assert!(joined.contains("file1"), "输出也要在：{got:?}");

    /* ---- 2. 密码输入：远端关掉 echo，屏幕上只有提示 ---- */
    // 用户敲的是 "s3cret"（我们**不再录输入**），远端只回一句提示
    t.feed(b"\r\nPassword: ");
    pump(150);
    let got = capture(&t, &mut prev);
    rec.output(&got);

    rec.stop();
    let raw = std::fs::read_to_string(&path).expect("应能读回");
    println!("---- .cast 内容 ----\n{raw}\n--------------------");
    assert!(!raw.contains("s3cret"), "密码原文绝不能出现在录制文件里");

    let cast = replay::load(&path).expect("应能解析");
    let inputs = cast
        .events
        .iter()
        .filter(|e| e.kind == replay::EventKind::Input)
        .count();
    println!("事件 {} 条，其中输入事件 {inputs} 条", cast.events.len());
    assert_eq!(
        inputs, 0,
        "我们不再录输入事件（asciicast v2 规范：不应默认录）"
    );
    assert_eq!(cast.skipped, 0);

    let _ = std::fs::remove_file(&path);
    println!("\n✔ 回放里看得到敲的命令；文件里没有密码，也没有输入事件");
}
