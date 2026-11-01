//! 顶栏菜单弹层**左对齐**冒烟（阶段 54）。
//!
//! **用户场景**（2026-10-30，用户拿别的软件截图比对）：
//! 「它的查看菜单打开以后，菜单和『查看』这两个字是左对齐的，
//! 但我们的菜单似乎是中间对齐？」
//!
//! GTK4 的 `GtkPopover` 默认**居中**对着锚点按钮，没有左对齐开关
//! （`pointing_to` 只换锚点、不改对齐方式），所以是我们自己算偏移挪过去的。
//! 这条冒烟钉住那个偏移算得对不对。
//!
//! 本冒烟查两件事：
//! 1. 弹层的**左边缘**与标题**文字**的左边缘对齐（差 ≤ 1px）；
//! 2. 弹层表面 = 菜单卡片（没有阴影留白）——不然对齐的是黑框而不是卡片。
//!
//! **为什么用 xdotool 真点击**：GTK4 的弹层只有收到**真实指针事件**
//! 才会 map（实测：`emit_clicked()` / 直接 `popup()` 都弹不出来）。
//! 所以这里挪真指针点标题，跟用户点的是同一条路径（menubar_hover_smoke
//! 同理）。没装 xdotool 就跳过。
//!
//! 用法（需要 X 显示，无桌面时用 `xvfb-run -a`）：
//!
//! ```text
//! cargo run --example menu_align_smoke
//! ```
//!
//! 退出码 0 = 全过；非 0 = 有检查没过（stdout 里逐条打印）。

use gtk::prelude::*;
use puttyplus::menu::{self, MenuBar};
use puttyplus::theme::{self, Theme};
use std::process::Command;

const TITLE: &str = "菜单左对齐检查";

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
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

/// 调 xdotool；失败返回 false（没装 xdotool 时跳过检查，不 panic）。
fn xdotool(args: &[String]) -> bool {
    match Command::new("xdotool").args(args).output() {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// 窗口在屏幕上的原点（xdotool 查；查不到就当在 0,0）。
fn window_origin(title: &str) -> (i32, i32) {
    let Ok(out) = Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
    else {
        return (0, 0);
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let Some(id) = s.split_whitespace().next() else {
        return (0, 0);
    };
    let Ok(geo) = Command::new("xdotool")
        .args(["getwindowgeometry", "--shell", id])
        .output()
    else {
        return (0, 0);
    };
    let g = String::from_utf8_lossy(&geo.stdout);
    let get = |k: &str| -> i32 {
        g.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split('=').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    (get("X"), get("Y"))
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpMenuAlign")
        .build();
    app.connect_activate(|app| {
        // 必须装主题：`.pp-menubar-item` 的 padding（标题文字盒的起点）
        // 和 `.pp-menu` 的卡片样式都来自主题 CSS。
        theme::apply(Theme::Light);

        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title(TITLE)
            .default_width(600)
            .default_height(300)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("pp-menubar");
        bar.set_margin_start(180);
        bar.set_margin_top(12);
        win.set_child(Some(&bar));

        let menubar = MenuBar::new();
        let (btn, pop) = menubar.item("视图");
        let body = menu::body();
        body.append(&menu::item("view-list-symbolic", "显示/隐藏侧边栏", None));
        body.append(&menu::item("preferences-system-symbolic", "外观…", None));
        pop.set_child(Some(&body));
        bar.append(&btn);

        win.present();
        pump(500);

        // 用户点标题那一下（真指针；合成事件弹层不会 map）
        let (ox, oy) = window_origin(TITLE);
        let bx = btn.translate_coordinates(&win, 0.0, 0.0).map(|v| v.0).unwrap_or(0.0);
        let by = btn.translate_coordinates(&win, 0.0, 0.0).map(|v| v.1).unwrap_or(0.0);
        let sx = ox + bx as i32 + 8;
        let sy = oy + by as i32 + 8;
        let moved = xdotool(&[
            "mousemove".into(),
            sx.to_string(),
            sy.to_string(),
            "click".into(),
            "1".into(),
        ]);
        if !moved {
            println!("跳过：没装 xdotool，发不出真实指针事件");
            app.quit();
            std::process::exit(0);
        }
        pump(700);

        let mut ok = true;

        // ① 弹层左边缘 vs 标题文字左边缘
        //    标题文字的排版盒从「按钮内容区起点」开始 = 按钮左 + padding-left。
        let expect = btn.style_context().padding().left() as f64;
        match pop.translate_coordinates(&btn, 0.0, 0.0) {
            Some((x, _)) => {
                let d = (x - expect).abs();
                println!("弹层左边缘相对按钮 = {x:.1}（标题文字左边缘 = {expect:.1}），差 {d:.1}px");
                if d > 1.0 {
                    println!("✗ 没对齐：菜单跟标题文字差了 {d:.1}px");
                    ok = false;
                } else {
                    println!("✓ 菜单左边缘与标题文字左边缘对齐");
                }
            }
            None => {
                println!("✗ 量不到弹层位置（弹层没显示？）");
                ok = false;
            }
        }

        // ② 弹层表面 = 卡片（没有阴影留白）
        //    卡片宽 = 菜单体自然宽 + padding 6×2 + border 1×2 = +14。
        let (_, nat, _, _) = body.measure(gtk::Orientation::Horizontal, -1);
        let pop_w = pop.width();
        let want = nat + 14;
        println!("弹层表面宽 = {pop_w}，卡片应有宽 = {want}（菜单体自然宽 {nat} + 14）");
        if pop_w != want {
            println!(
                "✗ 弹层表面宽 {pop_w} ≠ 卡片宽 {want}：多出来的是阴影留白（会有一圈黑框）"
            );
            ok = false;
        } else {
            println!("✓ 弹层表面就是卡片本身（无阴影留白）");
        }

        println!("{}", if ok { "全部通过" } else { "有检查没过" });
        app.quit();
        std::process::exit(if ok { 0 } else { 1 });
    });
    app.run();
}
