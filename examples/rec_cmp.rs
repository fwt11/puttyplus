//! 录制**对拍**工具：把我们录的 .cast 与 asciinema 的放一起比。
//!
//! 用法：
//! ```text
//! cargo run --example rec_cmp -- /tmp/pp-ours.cast
//! asciinema rec -c 'seq 1 20' /tmp/asci.cast
//! ```
//! 两条命令录的是**同一段输出**，可以直接 diff 两个文件看差在哪。
//!
//! 关注点（阶段 87/86 的对照清单）：
//!   · 头：版本、term{cols,rows,type}、command、env
//!   · 事件：时间是**间隔**（v3）还是绝对（v2）
//!   · `o` 事件的内容：**原始 ANSI 字节流**（asciinema）还是**渲染后的屏幕行**（我们）
//!   · 有没有 `x`（退出码）事件
//!
//! 运行：cargo run --example rec_cmp -- <输出路径>
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::replay;
use puttyplus::term;
use std::time::{Duration, Instant};
use vte4::prelude::*;

const CMD: &str = "seq 1 20";

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

fn main() {
    gtk::init().expect("gtk init");
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/pp-ours.cast".to_string());
    let path = std::path::PathBuf::from(&out);
    let _ = std::fs::remove_file(&path);

    let win = gtk::Window::new();
    win.set_default_size(900, 500);
    let t = vte4::Terminal::new();
    t.set_size(80, 24);
    t.set_scrollback_lines(10_000);
    win.set_child(Some(&t));
    win.present();

    let pty = vte4::Pty::new_sync(vte4::PtyFlags::empty(), None::<&gio::Cancellable>).unwrap();
    t.set_pty(Some(&pty));
    // ⚠ 命令**自己带**：别去读什么外部脚本文件（跑的人不一定有，
    // 而且那个文件要是没了，示例就变成了"编译能过、跑起来没输出"）。
    let cmd = format!("sleep 0.6; {CMD}");
    let argv = ["/bin/sh", "-c", cmd.as_str()];
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

    pump(50);
    let cols = t.column_count().max(1);
    let rows = t.row_count().max(1);
    let mut rec = replay::Recorder::start(&path, cols, rows, "ours").expect("应能建 .cast");

    // 采样：跟 `term::rec_poll` 同一套逻辑
    // ⚠ 基线要在**输出之前**取（子进程先睡 0.6s）
    let mut prev = puttyplus::termview::vte::cursor_row(&t);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        pump(100);
        let cur = puttyplus::termview::vte::cursor_row(&t);
        if cur > prev {
            let lines = puttyplus::termview::vte::ansi_rows(&t, prev, cur - 1);
            rec.output(&lines);
            prev = cur;
        }
    }
    rec.exit(0);
    rec.stop();

    println!("已写入 {}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    for (i, l) in text.lines().enumerate() {
        if i >= 6 {
            println!("…");
            break;
        }
        println!("  {l}");
    }
}
