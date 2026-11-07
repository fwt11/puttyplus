//! ghostty 终端内核冒烟（跨平台版 阶段 C2）。
//!
//! 跑一遍 libghostty-vt 内核，确认三件事：
//!
//! 1. **网格内容对**：喂进去的 ANSI 文本，屏幕上就该是那样；
//! 2. **光标位置对**：CSI 行列定位 (`ESC[3;5H`) 之后光标落在哪；
//! 3. **Formatter 出的 ANSI 行对**：带样式的行里要有 SGR 序列。
//!
//! 顺带**和 VTE 对拍**（Linux 上才做，VTE 需要显示服务）：
//! 同一段字节喂给两个引擎，比屏幕文字——这是「Windows 版画面与
//! Linux 版一致」的第一道证据（阶段 C3 再比画面）。
//!
//! 运行：
//! ```text
//! cargo run --example ghostty_vt_smoke --features ghostty
//! ```
//! ⚠️ 需要 **Zig 0.17**（编译 libghostty-vt 用，详见 docs/跨平台-构建说明.md）。

use puttyplus::termview::ghostty::GhosttyTerm;

/// 喂给终端的测试数据（带颜色、带定位）。
///
/// - `\x1b[31m` 红色、`\x1b[42m` 绿底
/// - `\r\n` 换行
/// - `\x1b[3;5H` 光标移到第 3 行第 5 列（1 起算）
const SAMPLE: &[u8] = b"\x1b[31mred\x1b[0m \x1b[42mgreenbg\x1b[0m\r\nsecond line\x1b[3;5Hmarker";

/// 归一化屏幕文字：逐行去行尾空白，去掉末尾的空行。
///
/// ⚠️ 两个引擎对「行尾有多少空格」的取舍不一样 ⇒ 比之前先归一化。
fn norm_lines(s: &str) -> Vec<String> {
    let mut lines: Vec<String> = s
        .split('\n')
        .map(|l| l.trim_end().to_string())
        .collect();
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines
}

fn main() {
    let mut t = GhosttyTerm::new(80, 24).expect("建 ghostty 终端");
    t.feed(SAMPLE);

    // 1. 网格内容
    let text = t.screen_text();
    for want in ["red", "greenbg", "second line", "marker"] {
        assert!(
            text.contains(want),
            "屏幕上应该有 {want:?}，实际屏幕：\n{text}"
        );
    }
    println!("[ok] 网格内容：red / greenbg / second line / marker 都在");
    for (i, l) in norm_lines(&text).iter().enumerate().take(4) {
        println!("      第 {i} 行：{l:?}");
    }

    // 2. 光标位置
    //
    // ⚠️ `ESC[3;5H` 把光标送到第 3 行第 5 列（**1 起算**），
    // 接着又写了 6 个字符 `marker` ⇒ 光标最终落在**第 3 行第 11 列**
    // （0 起算就是行 2、列 10）。
    assert_eq!(t.cursor_row(), 2, "光标行号不对");
    assert_eq!(t.cursor_col(), 10, "光标列号不对");
    let lines = norm_lines(&text);
    assert_eq!(
        lines.get(2).map(String::as_str),
        Some("    marker"),
        "第 3 行应该是 4 个空格 + marker"
    );
    assert_eq!(lines.get(0).map(String::as_str), Some("red greenbg"));
    assert_eq!(lines.get(1).map(String::as_str), Some("second line"));
    println!("[ok] 光标位置：行 2 列 10（定位 + 写入后）");

    // 3. Formatter 出的 ANSI 行
    let ansi = t.screen_ansi();
    assert!(ansi.contains('\u{1b}'), "ANSI 输出里没有转义序列：{ansi:?}");
    let rows = t.ansi_rows();
    assert_eq!(rows.len(), 24, "ANSI 行数应该补齐到视口高度 24");
    assert!(
        rows[0].contains('\u{1b}'),
        "第 0 行应该有 SGR 序列：{:?}",
        rows[0]
    );
    assert!(
        rows[2].contains("marker"),
        "第 2 行应该是 marker（行号要与屏幕对齐）：{:?}",
        rows[2]
    );
    println!("[ok] Formatter：24 行 ANSI（行号与屏幕对齐），第 0 行含 SGR");

    // 4. 查找（ghostty 原生普通文本，不用转义）
    t.search_set_pattern(Some("greenbg"));
    assert!(
        t.search_find_next(),
        "应该能查到 greenbg（总匹配 {}）",
        t.search_match_count()
    );
    assert!(
        t.search_match_count() >= 1,
        "匹配数应该是 1 以上，实际 {}",
        t.search_match_count()
    );
    println!("[ok] 查找：greenbg 命中 {} 处", t.search_match_count());

    // 5. 和 VTE 对拍（Linux + 有显示服务时才做）
    match gtk::init() {
        Ok(()) => vte_cross_check(&text),
        Err(_) => println!("[跳过] 没有显示服务 ⇒ 不做 VTE 对拍（ghostty 部分已全过）"),
    }

    println!("\nghostty_vt_smoke: 全部通过");
}

/// 同一段字节喂给 VTE，比屏幕文字是否一致。
fn vte_cross_check(ghostty_text: &str) {
    use gtk::glib;
    use gtk::prelude::*;
    use vte4::prelude::*;

    /// 泵一会儿主循环（GTK 的布局与 realize 都要主循环转起来）。
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

    // ⚠️ **VTE 必须先 realize**（放进窗口并 show）才有网格——
    // 没 realize 的终端控件，`text_range_format` 一律返回空串
    // （踩过：对照时 VTE 侧是空的，还以为是两个引擎不一致）。
    let win = gtk::Window::new();
    win.set_default_size(800, 500);
    let term = vte4::Terminal::new();
    win.set_child(Some(&term));
    win.present();
    pump(300);
    term.set_size(80, 24);
    pump(100);

    term.feed(SAMPLE);
    pump(100);

    let vte_text = term
        .text_range_format(vte4::Format::Text, 0, 0, 23, 79)
        .0
        .map(|s| s.to_string())
        .unwrap_or_default();

    let a = norm_lines(ghostty_text);
    let b = norm_lines(&vte_text);
    assert_eq!(
        a, b,
        "两个引擎的屏幕文字不一致\nVTE:\n{:?}\nGHOSTTY:\n{:?}",
        b, a
    );
    println!("[ok] VTE 对拍：两个引擎的屏幕文字完全一致");
}
