//! 录制回放链路冒烟（6.1）：真 VTE → 屏幕快照 → .cast → 回放终端。
//!
//! 单测只能验字符串变换，这里把整条链在真 VTE 上跑一遍：
//! 起一个 PTY，让子进程吐带颜色的输出，两次采样各录一段，
//! 再把 .cast 喂进第二个终端，看渲染出来的文字对不对。
//!
//! 运行：cargo run --example rec_smoke
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::replay;
use puttyplus::term;
use vte4::prelude::*;

fn pump(rounds: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..rounds {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 转到屏幕上出现 `want` 为止（最多 `max_ms`）。返回是否等到了。
fn pump_until(t: &vte4::Terminal, want: &str, max_ms: usize) -> bool {
    let ctx = glib::MainContext::default();
    for _ in 0..(max_ms / 10) {
        while ctx.pending() {
            ctx.iteration(false);
        }
        let cur = t.text_format(vte4::Format::Text).unwrap_or_default().to_string();
        if cur.contains(want) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// 与 term::ansi_snapshot 同一条路径（那边是私有的，这里复刻一遍调用）
fn snap(t: &vte4::Terminal) -> (Vec<String>, i64, i64) {
    let cols = t.column_count().max(1);
    let rows = t.row_count().max(1);
    let (html, _) = t.text_range_format(vte4::Format::Html, 0, 0, rows - 1, cols - 1);
    let lines = html.map(|h| replay::ansi_lines(&h)).unwrap_or_default();
    (lines, cols, rows)
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    let dir = std::env::temp_dir().join("pp_rec_smoke");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("smoke.cast");
    let _ = std::fs::remove_file(&path);

    /* ---- 1. 真的 VTE + 真的子进程 ---- */
    let t = vte4::Terminal::new();
    let t2 = t.clone();
    t.set_size(80, 24);
    let pty = vte4::Pty::new_sync(vte4::PtyFlags::empty(), None::<&gio::Cancellable>).unwrap();
    t.set_pty(Some(&pty));
    let argv = ["/bin/sh", "-c", "printf '\\033[31mRED\\033[0m plain\\n'; sleep 2; printf '\\033[1;32mGREEN\\033[0m\\n'; sleep 20"];
    let argv_ref: Vec<&str> = argv.iter().copied().collect();
    // envp 传空数组等于"空环境"（/bin/sh 都起不来），要显式带上继承的环境
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
        move |res| {
            if let Ok(pid) = res {
                t2.watch_child(pid);
            }
        },
    );
    // 先等第一行出来，保证基线之后确实还有新内容
    assert!(pump_until(&t, "RED", 8000), "第一行始终没出现，PTY 没接上");

    /* ---- 2. 第一轮采样：建基线 ---- */
    let (prev, cols, rows) = snap(&t);
    let mut rec = replay::Recorder::start(&path, cols, rows, "smoke").expect("应能建 .cast");
    rec.marker("smoke");
    println!("屏幕 {cols}x{rows}，首轮快照 {} 行", prev.len());
    assert!(
        !prev.is_empty(),
        "子进程输出后屏幕不该是空的（PTY 读没接上？）"
    );

    /* ---- 3. 第二轮：录下新增行 ---- */
    assert!(pump_until(&t, "GREEN", 8000), "第二行始终没出现");
    let (cur, _, _) = snap(&t);
    println!("两轮之间的屏幕：\n{}\n", t.text_format(vte4::Format::Text).unwrap_or_default());
    let added = term::new_lines(&prev, &cur);
    println!("第二轮新增 {} 行：{added:?}", added.len());
    assert!(!added.is_empty(), "第二轮应有新增行");
    // 基线是"RED plain"那一屏，所以增量里应当正好是后来出现的 GREEN 行
    let joined: String = added.concat();
    assert!(joined.contains("GREEN"), "增量应含 GREEN，实际 {joined:?}");
    assert!(!joined.contains("RED"), "RED 在基线里，不该重复录：{joined:?}");
    rec.output(&added);
    rec.input(b"ls\r");

    /* ---- 4. 回放进第二个终端 ---- */
    rec.stop();
    let cast = replay::load(&path).expect("应能读回");
    println!("录到 {} 条事件，尺寸 {}x{}", cast.events.len(), cast.cols, cast.rows);
    assert_eq!(cast.skipped, 0);

    let rt = vte4::Terminal::new();
    rt.set_size(cast.cols, cast.rows);
    for ev in &cast.events {
        if ev.kind == replay::EventKind::Output {
            rt.feed(ev.data.as_bytes());
        }
    }
    pump(30);
    let text = rt.text_format(vte4::Format::Text).unwrap_or_default().to_string();
    println!("--- 回放终端里的内容 ---\n{text}\n------------------------");
    assert!(text.contains("GREEN"), "回放里应看得到 GREEN：\n{text}");

    // 颜色要真的进了文件：RED 是 #C00000 系的真彩
    let has_truecolor = cast
        .events
        .iter()
        .any(|e| e.data.contains("38;2;") || e.data.contains("48;2;"));
    println!("文件里含真彩 SGR = {has_truecolor}");
    assert!(has_truecolor, "录出来应当带颜色（真彩 SGR），否则等于白录");

    let _ = std::fs::remove_file(&path);
    println!("\n✔ 录制→回放整条链路通");
}
