//! 顶栏菜单的**版式截图**（阶段 74，审计 P1-13）。
//!
//! 把「终端」菜单弹开截一张：看快捷键列（文字右边）排得对不对、
//! 没有终端时那些项是不是灰的。
//!
//! ```text
//! xvfb-run -a cargo run --example topmenu_shot
//! ```

use std::process::Command;

use gtk::prelude::*;
use puttyplus::menu;
use puttyplus::topbar;
use puttyplus::topmenus::TOP_MENUS;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn xdotool(args: &[&str]) -> bool {
    match Command::new("xdotool").args(args).output() {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

fn find_window(title: &str) -> Option<u32> {
    let out = Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .next()
}

fn window_origin(id: u32) -> Option<(i32, i32)> {
    let out = Command::new("xdotool")
        .args(["getwindowgeometry", "--shell", &id.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut x = None;
    let mut y = None;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("X=") {
            x = v.trim().parse().ok();
        }
        if let Some(v) = line.strip_prefix("Y=") {
            y = v.trim().parse().ok();
        }
    }
    Some((x?, y?))
}

fn offset_in_window(w: &gtk::Widget, win: &gtk::Window) -> Option<(i32, i32)> {
    let (x, y) = w.translate_coordinates(win, 0.0, 0.0)?;
    Some((x.round() as i32, y.round() as i32))
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    const TITLE: &str = "顶栏菜单截图";
    let mut acts: std::collections::HashMap<&'static str, gtk::gio::SimpleAction> =
        std::collections::HashMap::new();
    for a in puttyplus::topmenus::all_referenced_actions() {
        acts.insert(a, gtk::gio::SimpleAction::new(a, None));
    }
    let acts = std::rc::Rc::new(acts);

    let win = gtk::Window::new();
    win.set_title(Some(TITLE));
    win.set_default_size(520, 200);

    let bar = menu::MenuBar::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("pp-menubar");
    let mut term_btn: Option<gtk::Button> = None;
    for (title, entries) in TOP_MENUS {
        let (b, pop) = bar.item(title);
        let m = topbar::build_body(entries, &acts, &pop, win.upcast_ref::<gtk::Window>());
        pop.set_child(Some(&m.body));
        if *title == "终端" {
            term_btn = Some(b.clone());
        }
        row.append(&b);
    }
    win.set_child(Some(&row));
    win.present();
    pump(600);

    // ⚠️ GTK4 的弹层只有**真实指针事件**才会 map ⇒ 用 xdotool 点一下
    let shot = "/tmp/pp_topmenu.png";
    if let (Some(btn), Some(id)) = (term_btn, find_window(TITLE)) {
        if let Some((wx, wy)) = window_origin(id) {
            let btnw: gtk::Widget = btn.clone().upcast();
            if let Some((x, y)) = offset_in_window(&btnw, win.upcast_ref::<gtk::Window>()) {
                let a = btn.allocation();
                let cx = wx + x + a.width() / 2;
                let cy = wy + y + a.height() / 2;
                xdotool(&["mousemove", &cx.to_string(), &cy.to_string()]);
                pump(200);
                xdotool(&["click", "1"]);
                pump(600);
            }
        }
    }
    let _ = Command::new("/usr/bin/scrot").arg("-o").arg(shot).status();
    println!("截图：{shot}");
}
