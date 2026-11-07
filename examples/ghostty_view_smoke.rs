//! ghostty 自绘终端控件冒烟（跨平台版 阶段 C3）。
//!
//! 验三件事（都是**真路径**，不是另写一份模拟）：
//!
//! 1. **屏幕内容对**：喂一段 ANSI，网格里的字符就该是那样；
//! 2. **按键发出的字节对**：回车 / Ctrl+C / 方向键；
//! 3. **中文 commit 发的是 UTF-8 字节**（不是问号、不是半个字）。
//!
//! 另外截一张图（`/tmp/pp_ghostty_view.png`）供人工看画面——
//! **画面本身没法用断言钉住**（字形由字体决定），只能人眼比。
//!
//! 运行：
//! ```text
//! cargo run --example ghostty_view_smoke --features ghostty
//! ```
//! ⚠️ 需要 **Zig 0.17**（编译 libghostty-vt 用）。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::termview::ghostty_view::GhosttyView;

/// 喂给终端的测试数据（带颜色、带定位）。
const SAMPLE: &[u8] = b"\x1b[31mred\x1b[0m \x1b[42mgreenbg\x1b[0m\r\nsecond line\x1b[3;5Hmarker";

/// 泵一会儿主循环。
fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn main() {
    gtk::init().expect("gtk init");

    let view = GhosttyView::new();
    // 收集「发出去的字节」（将来这就是写给子进程的东西）
    let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let o = out.clone();
        view.set_output_handler(move |b| o.borrow_mut().extend_from_slice(b));
    }

    let win = gtk::Window::new();
    win.set_title(Some("ghostty 自绘终端 — 阶段 C3"));
    win.set_default_size(900, 560);
    win.set_child(Some(view.widget()));
    win.present();
    pump(600);

    /* ---------------- 1. 屏幕内容 ---------------- */
    view.feed(SAMPLE);
    pump(400);
    let rows = view.screen_rows();
    println!("[ok] 网格行数：{}", rows.len());
    for (i, r) in rows.iter().enumerate().take(4) {
        println!("      第 {i} 行：{r:?}");
    }
    assert_eq!(
        rows.first().map(String::as_str),
        Some("red greenbg"),
        "第 0 行不对"
    );
    assert_eq!(
        rows.get(1).map(String::as_str),
        Some("second line"),
        "第 1 行不对"
    );
    assert_eq!(
        rows.get(2).map(String::as_str),
        Some("    marker"),
        "第 2 行不对（ESC[3;5H 定位 + 4 个空格）"
    );
    println!("[ok] 屏幕内容：red greenbg / second line / '    marker'");

    /* ---------------- 1b. 中文（宽字符） ---------------- */
    view.feed("\r\n中文测试 wide".as_bytes());
    pump(400);
    let rows = view.screen_rows();
    let cn_row = rows
        .iter()
        .find(|r| r.contains("中文"))
        .expect("屏幕上应该有中文");
    println!("[ok] 中文行：{cn_row:?}");
    assert!(
        cn_row.starts_with("中文测试 wide"),
        "中文行不对：{cn_row:?}"
    );
    // ⚠️ 宽字符的「尾巴格」不能变成空格（那会让中文后面多一格）
    assert!(
        !cn_row.contains("文 测"),
        "中文之间多了空格（宽字符尾巴格没处理）：{cn_row:?}"
    );
    println!("[ok] 中文：宽字符占 2 列、尾巴格不占位");

    /* ---------------- 2. 按键 ⇒ 字节 ---------------- */
    let mut check = |name: &str, key: gdk::Key, mods: gdk::ModifierType, want: &[u8]| {
        out.borrow_mut().clear();
        view.press_key(key, mods);
        let got = out.borrow().clone();
        assert_eq!(
            got, want,
            "{name} 发出的字节不对：期望 {want:?}，实际 {got:?}"
        );
        println!("[ok] {name} ⇒ {want:?}");
    };
    check("回车", gdk::Key::Return, gdk::ModifierType::empty(), b"\r");
    check(
        "Ctrl+C",
        gdk::Key::c,
        gdk::ModifierType::CONTROL_MASK,
        b"\x03",
    );
    check("上箭头", gdk::Key::Up, gdk::ModifierType::empty(), b"\x1b[A");
    check("退格", gdk::Key::BackSpace, gdk::ModifierType::empty(), b"\x7f");
    check("空格", gdk::Key::space, gdk::ModifierType::empty(), b" ");

    /* ---------------- 3. 中文 commit ⇒ UTF-8 ---------------- */
    out.borrow_mut().clear();
    view.commit_text("中文");
    let got = out.borrow().clone();
    assert_eq!(
        got,
        "中文".as_bytes(),
        "中文 commit 发出的不是 UTF-8 字节：{got:?}"
    );
    println!("[ok] 中文 commit ⇒ UTF-8 字节（{} 字节）", got.len());

    /* ---------------- 截图（人工看画面） ---------------- */
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_ghostty_view.png")
        .status();
    println!("截图：/tmp/pp_ghostty_view.png");

    println!("\nghostty_view_smoke: 全部通过");
}
