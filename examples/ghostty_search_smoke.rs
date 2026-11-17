//! ghostty 后端的**终端查找** ＋ **输入后自动滚到底**（用户 2026-11-17 报的两条）。
//!
//! **背景（用户 2026-11-17）**：
//! > 「libvte 中，用户有输入时自动滚动到最底下。ghostty 中似乎还没有实现」
//! > 「增加个功能，就是在终端中查找字符串」
//!
//! **为什么 ghostty 里「查找」像是不存在**：
//! - 查找条、搜索算法**都在**（内核的 `search::Search`），但**搜完不重画**
//!   ⇒ ghostty 是**我们自绘**的 ⇒ 匹配的高亮不会出现在屏幕上
//!   ⇒ 用户看到的是「搜了没反应」；
//! - **视口不会自己回到底部**：内核的视口有「活动 / 历史」两态，
//!   用户往上翻了历史之后，新来的输出**故意**不把画面拽回去
//!   ⇒ 「敲键后回底」必须**显式**做。
//!
//! **断言**（都落在外部量上）：
//! 1. 搜索能命中（匹配数对）；
//! 2. 「找下一个」⇒ 真的选中了那一段匹配（选区文本 == 匹配词）；
//! 3. **搜完之后画面重画了**（DrawingArea 的 draw 信号被触发
//!    ⇒ 用户看得见高亮；不重画的话这一条就红）；
//! 4. **视口滚到匹配处**（匹配在回滚缓冲里 ⇒ 搜完视口不在底部了）；
//! 5. **敲一个键 ⇒ 视口回到底部**（VTE 同款）；
//! 6. 清掉查找词 ⇒ 选区没了（高亮消失）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example ghostty_search_smoke

use gtk::prelude::*;
use puttyplus::termview::ghostty_view::GhosttyView;
use std::cell::RefCell;
use std::rc::Rc;

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

/// 找到窗口 id（xdotool 发键要用）。
fn win_id(title: &str) -> Option<String> {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// 发一个**真的**按键（XTEST ⇒ GTK 那边照常收到 key-press）。
fn send_key(wid: &str, key: &str) {
    let _ = std::process::Command::new("xdotool")
        .args(["windowfocus", "--sync", wid])
        .status();
    let _ = std::process::Command::new("xdotool")
        .args(["key", "--clearmodifiers", key])
        .status();
}

const NEEDLE: &str = "NeedleToken";

fn main() {
    gtk::init().expect("gtk init");

    let view = GhosttyView::new();
    view.set_scrollback_lines(1000);

    // 收集「发出去的字节」（按键 ⇒ 远端）。
    let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let o = out.clone();
        view.set_output_handler(move |b| o.borrow_mut().extend_from_slice(b));
    }

    // 灌 100 行；**要找的词放在第 3 行**（在回滚缓冲里 ⇒ 视口看不到它）。
    let mut buf = String::new();
    for i in 0..100 {
        if i == 3 {
            buf.push_str(&format!("line {i} {NEEDLE} tail\r\n"));
        } else {
            buf.push_str(&format!("line {i} filler\r\n"));
        }
    }
    view.feed(buf.as_bytes());

    let win = gtk::Window::new();
    win.set_title(Some("ghostty find smoke"));
    win.set_default_size(800, 440);
    win.set_child(Some(view.area()));
    win.present();
    pump(500);

    pump(200);

    // 起点：视口贴着底部（活动区）。
    assert!(view.viewport_active(), "起点视口应当贴着底部");
    println!("[1] 起点：视口贴着底部 ✔");

    println!("[诊断] 搜索前 render_count = {}", view.render_count());
    // ---- 2) 搜索能命中 ----
    view.search_set_pattern(Some(NEEDLE));
    pump(200);
    let n = view.search_match_count();
    assert_eq!(n, 1, "匹配数应当是 1（实际 {n}）");
    println!("[2] 匹配数 = {n} ✔");

    // ---- 3) 找下一个 ⇒ 选区就是那一段匹配 ----
    let d0 = view.render_count();
    assert!(view.search_find_next(), "「找下一个」没找到");
    pump(300);
    let sel = view.selection_text().unwrap_or_default();
    assert_eq!(
        sel, NEEDLE,
        "选中的应当就是那一段匹配（实际 {sel:?}）"
    );
    println!("[3] 选中的就是 {sel:?} ✔");

    // ---- 4) 搜完**画面重画了**（不然用户看不见高亮）----
    let d1 = view.render_count();
    println!("[诊断] render_count: {d0} → {d1}（初始 {}）", view.render_count());
    assert!(
        d1 > d0,
        "搜完之后没有重画（draw {d0} → {d1}）⇒ 高亮不会出现在屏幕上"
    );
    println!("[4] 搜完重画了（draw {d0} → {d1}）✔");

    // ---- 5) 视口滚到匹配处（匹配在回滚缓冲里 ⇒ 视口离开底部）----
    assert!(
        !view.viewport_active(),
        "视口还在底部 ⇒ 没滚到匹配那一行"
    );
    println!("[5] 视口滚到匹配处了 ✔");

    // ---- 6) 敲一个键 ⇒ 视口回到底部（VTE 同款）----
    let has_xdo = std::process::Command::new("xdotool")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_xdo {
        println!("[6] ⚠️ 没有 xdotool ⇒ 真按键这一段没验");
    } else {
        let Some(wid) = win_id("ghostty find smoke") else {
            panic!("找不到窗口（标题「ghostty find smoke」）");
        };
        view.area().grab_focus();
        pump(200);
        send_key(&wid, "a");
        pump(400);
        assert!(
            view.viewport_active(),
            "敲了键以后视口没有回到底部（用户 2026-11-17 报的）"
        );
        let sent = out.borrow().clone();
        assert!(
            sent.contains(&b'a'),
            "按键没发出去（发出的是 {sent:?}）"
        );
        println!("[6] 敲键后视口回到底部 ✔（键也发出去了）");
    }

    // ---- 7) 清掉查找词 ⇒ 选区没了 ----
    view.search_set_pattern(None);
    pump(200);
    assert!(
        view.selection_text().unwrap_or_default().is_empty(),
        "清掉查找词以后选区还在"
    );
    println!("[7] 清掉查找词 ⇒ 高亮消失 ✔");

    println!("ghostty find smoke ok");
}
