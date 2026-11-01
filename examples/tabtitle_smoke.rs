//! 标签头标题的宽度（阶段 75，审计 P1-14）。
//!
//! **问题**：标签标题原来用 `width_chars(10)`，而 GTK4 里 `width-chars`
//! 把标签的**自然宽**也定成 10 个字符 ⇒
//! - 单标签、满屏空白时也显示成 `ops@127.0...`（明明有地方）；
//! - `max_width_chars(18)` 根本没机会生效（永远先被 10 字卡住）。
//!
//! **断言**：
//! 1. 宽度**随内容变化**（短标题比长标题窄）；
//! 2. **不被压成固定 10 字**（长标题比老的固定 10 字宽 ⇒ 能显示更多）；
//! 3. 短标题不比老的固定 10 字更宽（没给标题页塞更多地方）；
//! 4. 长标题有上限（≤18 字，超出省略号）；
//! 5. 有**显式最小宽**（不会被页签挤成「… ×」）。
//!
//! ```text
//! xvfb-run -a cargo run --example tabtitle_smoke
//! ```

use gtk::prelude::*;
use puttyplus::tabs;

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // 量一个 Label 的（最小宽, 自然宽）
    let measure = |l: &gtk::Label| -> (i32, i32) {
        let (min, nat, _, _) = l.measure(gtk::Orientation::Horizontal, -1);
        (min, nat)
    };

    let short = tabs::tab_title_label("ssh");
    // ⚠️ 标题要**够长**（>18 字）才能验证上限：短的本来就不该被截断
    let long_text = "root@very-long-hostname.example.com:2222";
    let long = tabs::tab_title_label(long_text);

    // 老做法：固定 10 字
    let old = gtk::Label::new(Some("ssh"));
    old.set_ellipsize(gtk::pango::EllipsizeMode::End);
    old.set_width_chars(10);
    old.set_max_width_chars(18);

    // 完全不限长的长标题（看上限有没有生效）
    let plain_long = gtk::Label::new(Some(long_text));

    // 放进窗口里，让 GTK 真正量一遍
    let win = gtk::Window::new();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.append(&short);
    box_.append(&long);
    box_.append(&old);
    box_.append(&plain_long);
    win.set_child(Some(&box_));
    win.present();
    for _ in 0..40 {
        gtk::glib::MainContext::default().iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let (min_short, nat_short) = measure(&short);
    let (_, nat_long) = measure(&long);
    let (_, nat_old) = measure(&old);
    let (_, nat_plain) = measure(&plain_long);
    println!(
        "[实测] 短标题 {min_short}/{nat_short}，长标题 {nat_long}，老做法 {nat_old}，不限长 {nat_plain}"
    );

    check!(
        "宽度随内容变化（短标题比长标题窄）",
        nat_short < nat_long,
        format!("短 {nat_short} vs 长 {nat_long}")
    );
    check!(
        "长标题不再被压成固定 10 字",
        nat_long > nat_old,
        format!("长 {nat_long} vs 老做法 {nat_old}")
    );
    check!(
        "短标题不比老的固定 10 字更宽",
        nat_short <= nat_old,
        format!("短 {nat_short} vs 老做法 {nat_old}")
    );
    check!(
        "长标题有上限（超出用省略号）",
        nat_long < nat_plain,
        format!("上限内 {nat_long} vs 不限长 {nat_plain}")
    );
    check!(
        "有显式最小宽（不会被挤成「…」）",
        min_short >= 60,
        format!("最小宽 {min_short}px")
    );
    // ⚠️ **最关键的一条**：`width_chars` 根本没设（-1 = 没设）。
    // 设了它，GTK4 会把标签的宽度**定死**成那个字数 ⇒ 标题再短也占那么多、
    // 再长也只显示那么多（`max_width_chars` 没机会生效）。
    check!(
        "不用 width_chars（那会把宽度定死）",
        short.width_chars() < 0,
        format!("width_chars = {}", short.width_chars())
    );
    check!(
        "上限是 18 个字",
        short.max_width_chars() == 18,
        format!("max_width_chars = {}", short.max_width_chars())
    );
    check!(
        "超长时用省略号",
        short.ellipsize() == gtk::pango::EllipsizeMode::End,
        format!("{:?}", short.ellipsize())
    );

    if fails.is_empty() {
        println!("tabtitle smoke ok");
    } else {
        println!("tabtitle smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
