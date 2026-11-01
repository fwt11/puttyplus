//! 菜单文字**不能被裁**的冒烟（用户 2026-10-29：「左右分屏」的「左」字
//! 底下一横不见了；顶栏菜单标题下半截也被截断）。
//!
//! **为什么要有这个用例**：这类问题**肉眼看截图很准**（少一横），但
//! **说不清是哪一层压的**，而且**换台机器/换个字号就复现不了**——
//! 开发机上字体的自然高度比写死的 `min-height` 小几像素，看着好好的；
//! 用户办公机上的中文字体（fallback 的 ascent/descent 更大）一换，
//! 分配高度就不够，底部被削。
//!
//! **判据**：「文字拿到的分配高度 ≥ 纯文本高度 + 2px」。
//!
//! ⚠️ **为什么不能直接拿 Label 的 natural height 当"纯文本高度"**：
//! 一旦给 Label 设了 CSS `min-height`，GTK 报的 natural 就被**抬高成
//! min-height 那个值**，于是"分配高度 = natural"恒成立，余量无从量起。
//! 所以这里另造一个**同字号、但不受任何 min-height 规则影响**的
//! 对照 Label（放在普通 Box 里，不在按钮内），量它的 natural —— 那才是
//! 纯文本高度。
//!
//! **跑多个字号**：写死的 px 高度在小字号下富余、在大字号下不够，
//! 只测一个字号等于没测。
//!
//! ```text
//! cargo run --example menu_fit_smoke
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use puttyplus::menu;
use puttyplus::theme::{self, Theme};

/// 对照 Label 的样式：只对齐字号，**不设 min-height**（它要量的是
/// 纯文本高度，设了 min-height 就量不到了）。
const PROBE_CSS: &str = "
.pp-fit-probe-92 { font-size: 0.92em; }
.pp-fit-probe-100 { font-size: 1em; }
";

/// 收集控件树里所有带指定类的 Label。
fn labels_of_class(w: &gtk::Widget, class: &str, out: &mut Vec<gtk::Label>) {
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            if l.css_classes().iter().any(|x| x == class) {
                out.push(l.clone());
            }
        }
        labels_of_class(&c, class, out);
        cur = c.next_sibling();
    }
}

/// 收集控件树里所有 Label（刻意省略号截断的不算——那是设计如此）。
fn labels_of(w: &gtk::Widget, out: &mut Vec<gtk::Label>) {
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            if l.ellipsize() == gtk::pango::EllipsizeMode::None {
                out.push(l.clone());
            }
        }
        labels_of(&c, out);
        cur = c.next_sibling();
    }
}

/// 纯文本高度：同字号对照 Label 的 natural height。
#[allow(deprecated)]
fn text_height(probe: &gtk::Label) -> i32 {
    let (_, nat, _, _) = probe.measure(gtk::Orientation::Vertical, -1);
    nat
}

/// 检查一批 Label：分配高度必须 ≥ 纯文本高度 + 2px，否则文字被削。
#[allow(deprecated)]
fn check(tag: &str, labels: &[gtk::Label], text_h: i32) -> bool {
    let mut ok = true;
    for l in labels {
        let t = l.text().to_string();
        let h = l.allocation().height();
        if h == 0 {
            continue; // 还没布局（不在可见容器里）
        }
        if h < text_h + 2 {
            eprintln!(
                "❌ {tag}「{t}」余量不足：分配高 {h}，纯文本高 {text_h}（差 {}px）",
                text_h + 2 - h
            );
            ok = false;
        } else {
            eprintln!("✅ {tag}「{t}」完整：分配高 {h} ≥ 纯文本高 {text_h} + 2");
        }
    }
    ok
}

/// 对照 Label：同字号、无 min-height 规则，量纯文本高度用。
fn probe_label(text: &str, cls: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(cls);
    l
}

/// 搭出三个真实场景：弹层里的菜单 + 顶栏菜单标题 + 对话框按钮。
fn scenes() -> (gtk::Window, gtk::Button, gtk::Label, gtk::Window, gtk::Button, gtk::Label, gtk::Window, gtk::Button, gtk::Label) {
    // ① 弹层菜单（右键菜单 / 顶栏下拉都是这个结构）
    let menu_win = gtk::Window::new();
    menu_win.set_default_size(320, 300);
    let anchor = gtk::Button::new();
    let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu_box.append(&anchor);
    // 对照：放在普通 Box 里（不在 button 内）→ 不受 `button label` 规则影响
    let m_probe = probe_label("左右分屏", "pp-fit-probe-92");
    menu_box.append(&m_probe);
    menu_win.set_child(Some(&menu_box));

    let pop = menu::popover_detached();
    pop.set_parent(&anchor);
    let body = menu::body();
    let it1 = menu::item("view-split-left-symbolic", "左右分屏", None);
    let it2 = menu::item("view-split-top-symbolic", "上下分屏", Some("Ctrl+Shift+E"));
    body.append(&it1);
    body.append(&it2);
    pop.set_child(Some(&body));
    pop.popup();
    menu_win.present();

    // ② 顶栏菜单标题
    let bar = menu::MenuBar::new();
    let (top, _pop) = bar.item("文件");
    let header = gtk::HeaderBar::new();
    header.pack_start(&top);
    let top_win = gtk::Window::new();
    top_win.set_titlebar(Some(&header));
    top_win.set_default_size(400, 200);
    let top_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let t_probe = probe_label("文件", "pp-fit-probe-100");
    top_box.append(&t_probe);
    top_win.set_child(Some(&top_box));
    top_win.present();

    // ③ 对话框底部的文字按钮（「保存修改」这类）
    let dlg_win = gtk::Window::new();
    let dlg_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let dlg = gtk::Button::new();
    dlg.set_label("保存修改");
    dlg_box.append(&dlg);
    let d_probe = probe_label("保存修改", "pp-fit-probe-100");
    dlg_box.append(&d_probe);
    dlg_win.set_child(Some(&dlg_box));
    dlg_win.set_default_size(200, 120);
    dlg_win.present();

    (
        menu_win,
        it1,
        m_probe,
        top_win,
        top,
        t_probe,
        dlg_win,
        dlg,
        d_probe,
    )
}

/// 在一个字号下跑一遍全部检查，然后进入下一个字号。
fn step(fonts: &'static [&'static str], idx: usize, all_ok: Rc<Cell<bool>>) {
    if idx >= fonts.len() {
        eprintln!(
            "—— 全部字号跑完：{} ——",
            if all_ok.get() {
                "菜单文字没有被裁 ✅"
            } else {
                "有文字被裁 ❌"
            }
        );
        std::process::exit(if all_ok.get() { 0 } else { 1 });
    }
    let font = fonts[idx];
    let settings = gtk::Settings::default().expect("settings");
    settings.set_gtk_font_name(Some(font));

    let (menu_win, it1, m_probe, top_win, top, t_probe, dlg_win, dlg, d_probe) = scenes();
    let it1 = it1.clone();
    let top = top.clone();
    let dlg = dlg.clone();
    let m_probe = m_probe.clone();
    let t_probe = t_probe.clone();
    let d_probe = d_probe.clone();
    let fonts2 = fonts;
    gtk::glib::timeout_add_local_once(Duration::from_millis(500), move || {
        eprintln!("=== 字号 {font} ===");
        let menu_h = text_height(&m_probe);
        let top_h = text_height(&t_probe);
        let dlg_h = text_height(&d_probe);
        eprintln!("    纯文本高度：菜单 {menu_h} / 顶栏 {top_h} / 对话框 {dlg_h}");

        let mut menu_labels = Vec::new();
        labels_of_class(it1.upcast_ref::<gtk::Widget>(), "pp-menu-label", &mut menu_labels);
        let ok1 = check("菜单项", &menu_labels, menu_h);

        let mut top_labels = Vec::new();
        labels_of(top.upcast_ref::<gtk::Widget>(), &mut top_labels);
        let ok2 = check("顶栏标题", &top_labels, top_h);

        let mut dlg_labels = Vec::new();
        labels_of(dlg.upcast_ref::<gtk::Widget>(), &mut dlg_labels);
        let ok3 = check("对话框按钮", &dlg_labels, dlg_h);

        if ok1 && ok2 && ok3 {
            eprintln!("—— 字号 {font} 通过 ——");
        } else {
            eprintln!("—— 字号 {font} 失败 ——");
            all_ok.set(false);
        }
        // 这一轮的窗口拆掉，换下一档字号重来
        menu_win.destroy();
        top_win.destroy();
        dlg_win.destroy();
        step(fonts2, idx + 1, all_ok);
    });
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);
    // 对照 Label 的样式：只对齐字号，不设 min-height
    let provider = gtk::CssProvider::new();
    provider.load_from_data(PROBE_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    step(&["Sans 11", "Sans 16", "Sans 22"], 0, Rc::new(Cell::new(true)));
    gtk::glib::MainLoop::new(None, false).run();
}
