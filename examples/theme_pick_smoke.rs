//! 首选项主题/配色选择的回归冒烟（阶段 89）。
//!
//! 钉的是用户报的 bug：「外观里选浅色 → 点终端页 → 选深色 → 再点外观页，
//! **整个软件都变成高对比了**」。
//!
//! 根因：GTK4 的 `GtkListBox` 在**页签重新显示**时会把选中行自己往后挪一行，
//! 并为此发一次 `row-selected`；我们的处理器把"收到 row-selected"当成
//! "用户改了设置"，于是**切个页签就把主题/配色悄悄换掉了**。
//! （实测：浅色 2 → 高对比 3、深色 0 → 浅色 1，选中行自己往后跑。）
//!
//! 现在：选中状态不用 GTK 的高亮（`SelectionMode::None`），值的变化只在
//! **用户真点了那一行**（`row-activated`）时才生效。
//!
//! 覆盖：
//! - 点一行 ⇒ 值真的变了；
//! - **切页签 ⇒ 值不变**（这条钉的就是 bug）；
//! - 切回来 ⇒ 值还是不变；
//! - 当前值用**勾选标记**标出来（不靠底色，审计 P2-6）；
//! - 旁边的密度/字号分组也不会被页签切换带偏。
//!
//! ⚠ 设置项路径由**外层环境变量**给（`PUTTYPLUS_SETTINGS_INI`）：
//! `glib::user_config_dir()` 进程内会缓存 ⇒ 必须在任何 glib 调用**之前**
//! 由跑它的脚本导出 HOME 与该变量，别让这个例子写用户真实配置。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example theme_pick_smoke
use gtk::glib;
use gtk::prelude::*;
use puttyplus::settingsui::{self, Page};
use puttyplus::theme::{self, Palette, Theme};
use std::time::Instant;

fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let deadline = Instant::now() + std::time::Duration::from_millis(ms);
    while Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 递归找出某个容器里的全部 ListBox。
fn lists(w: &gtk::Widget, out: &mut Vec<gtk::ListBox>) {
    if let Some(l) = w.downcast_ref::<gtk::ListBox>() {
        out.push(l.clone());
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        lists(&x, out);
        c = x.next_sibling();
    }
}

/// 递归收集一个控件树里的所有 Label 文字。
fn texts_of(w: &gtk::Widget, out: &mut String) {
    if let Some(lb) = w.downcast_ref::<gtk::Label>() {
        out.push_str(&lb.text());
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        texts_of(&x, out);
        c = x.next_sibling();
    }
}

/// 一个 ListBox 里所有行的可见文字。
fn row_texts(l: &gtk::ListBox) -> Vec<String> {
    let mut out = Vec::new();
    let mut r = l.first_child();
    while let Some(row) = r {
        if let Some(row) = row.downcast_ref::<gtk::ListBoxRow>() {
            let mut txt = String::new();
            texts_of(row.upcast_ref::<gtk::Widget>(), &mut txt);
            out.push(txt);
        }
        r = row.next_sibling();
    }
    out
}

/// 按行首文字找一行。
fn pick(l: &gtk::ListBox, name: &str) -> Option<gtk::ListBoxRow> {
    let mut r = l.first_child();
    while let Some(row) = r {
        if let Some(row) = row.downcast_ref::<gtk::ListBoxRow>() {
            let mut txt = String::new();
            texts_of(row.upcast_ref::<gtk::Widget>(), &mut txt);
            if txt.starts_with(name) {
                return Some(row.clone());
            }
        }
        r = row.next_sibling();
    }
    None
}

/// 这一行上有没有**可见的勾选标记**（当前值就是它标出来的）。
fn has_visible_check(row: &gtk::ListBoxRow) -> bool {
    fn scan(w: &gtk::Widget) -> bool {
        if let Some(img) = w.downcast_ref::<gtk::Image>() {
            if img.icon_name().as_deref() == Some("object-select-symbolic") && img.is_visible() {
                return true;
            }
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            if scan(&x) {
                return true;
            }
            c = x.next_sibling();
        }
        false
    }
    scan(row.upcast_ref::<gtk::Widget>())
}

/// 这一组里**当前是选中**的 ToggleButton 的文字（密度/字号用）。
fn active_toggle_text(w: &gtk::Widget) -> Option<String> {
    fn scan(w: &gtk::Widget, found: &mut Option<String>) {
        if let Some(tb) = w.downcast_ref::<gtk::ToggleButton>() {
            if tb.is_active() {
                let mut txt = String::new();
                texts_of(tb.upcast_ref::<gtk::Widget>(), &mut txt);
                *found = Some(txt);
            }
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            scan(&x, found);
            c = x.next_sibling();
        }
    }
    let mut found = None;
    scan(w, &mut found);
    found
}

fn main() {
    gtk::init().unwrap();

    settingsui::open(Page::Appearance);
    pump(500);
    let nb = settingsui::pages_notebook().expect("首选项窗口没开起来");

    let app = nb.nth_page(Some(0)).expect("外观页");
    let term = nb.nth_page(Some(1)).expect("终端页");
    let mut app_lists = Vec::new();
    lists(app.upcast_ref::<gtk::Widget>(), &mut app_lists);
    let mut term_lists = Vec::new();
    lists(term.upcast_ref::<gtk::Widget>(), &mut term_lists);
    assert!(!app_lists.is_empty(), "外观页里没找到主题列表");
    assert!(!term_lists.is_empty(), "终端页里没找到配色列表");
    let theme_list = &app_lists[0];
    let pal_list = &term_lists[0];

    println!("主题列表：{:?}", row_texts(theme_list));
    println!("配色列表：{:?}", row_texts(pal_list));

    let density_before = active_toggle_text(app.upcast_ref::<gtk::Widget>());

    /* ---- 1) 外观页点「浅色」 ⇒ 主题真的变了 ---- */
    let light = pick(theme_list, "浅色").expect("没找到浅色行");
    light.activate();
    pump(400);
    assert_eq!(
        theme::theme(),
        Theme::Light,
        "点了浅色，主题却没变（row-activated 没生效？）"
    );
    assert!(
        has_visible_check(&light),
        "当前主题那一行要用**勾选标记**标出来（不靠底色）"
    );
    println!("[1] 点了浅色 ⇒ 主题 = {:?}", theme::theme());

    /* ---- 2) 点「终端」页签 ⇒ 主题/配色都不能变（这条钉的就是 bug）---- */
    nb.set_current_page(Some(1));
    pump(500);
    assert_eq!(
        theme::theme(),
        Theme::Light,
        "只是**切了个页签**，主题就被悄悄换掉了 ⇒ GTK 的选中行在页签重显时自己挪了"
    );
    assert_eq!(
        theme::palette(),
        Palette::Dark,
        "只是**切了个页签**，终端配色就被悄悄换掉了"
    );
    println!(
        "[2] 切到终端页 ⇒ 主题 {:?}、配色 {:?}",
        theme::theme(),
        theme::palette()
    );

    /* ---- 3) 终端页点「深色」⇒ 配色真的变了 ---- */
    let dark = pick(pal_list, "深色").expect("没找到深色行");
    dark.activate();
    pump(400);
    assert_eq!(theme::palette(), Palette::Dark, "点了深色，配色却没变");
    assert!(
        has_visible_check(&dark),
        "当前配色那一行要用**勾选标记**标出来"
    );
    println!("[3] 点了深色 ⇒ 配色 = {:?}", theme::palette());

    /* ---- 4) 点回「外观」页签 ⇒ 还是不能变（bug 的现场）---- */
    nb.set_current_page(Some(0));
    pump(600);
    assert_eq!(
        theme::theme(),
        Theme::Light,
        "回到外观页，主题又变了一次 ⇒ 同一个坑（用户现场：变成了高对比）"
    );
    assert_eq!(
        theme::palette(),
        Palette::Dark,
        "回到外观页，终端配色又变了一次"
    );
    println!(
        "[4] 切回外观页 ⇒ 主题 {:?}、配色 {:?}",
        theme::theme(),
        theme::palette()
    );

    /* ---- 5) 勾选标记得跟着当前值走 ---- */
    assert!(has_visible_check(&light), "浅色那行应当仍带着勾选标记");

    /* ---- 6) 旁边的密度/字号分组也别被页签切换带偏 ---- */
    let density_after = active_toggle_text(app.upcast_ref::<gtk::Widget>());
    assert_eq!(
        density_before, density_after,
        "界面密度/字号的选中项被页签切换带偏了：{density_before:?} → {density_after:?}"
    );

    println!("\n✔ 切页签不再偷换主题/配色；只有真点了那一行才会变");
}
