//! 顶栏菜单栏「悬停切换」冒烟（阶段 38）。
//!
//! **用户场景**（2026-10-28 报）：「我打开文件菜单然后鼠标移动到别的菜单上，
//! 别的菜单会自动打开。而我们现在的软件必须先关闭现在的菜单，才能打开
//! 别的菜单，就是说需要点两下，而常用的软件都是根本不用点，只要移动鼠标
//! 就行了。」
//!
//! 也就是 GNOME / Firefox / Windows 的经典菜单栏行为：**鼠标移到另一个
//! 顶级菜单上，那个菜单自动打开**。
//!
//! **为什么单测钉不住**：悬停切换靠的是 `EventControllerMotion` 的
//! `enter` 事件——**必须有真指针移动**才会派发。`gtk::EventControllerMotion`
//! 没有"手动派发事件"的公开 API，所以只能靠 xdotool 挪真指针。
//!
//! 本冒烟查三件事：
//! 1. 点开「文件」菜单 → 它真的打开；
//! 2. **鼠标移到「工具」标题上（不点击）** → 「工具」菜单自动打开；
//! 3. 此时「文件」菜单**已经关上**（不是两个叠着）。
//!
//! 用法（需要 X 显示，无桌面时用 `xvfb-run -a`）：
//!
//! ```text
//! cargo run --example menubar_hover_smoke
//! ```
//!
//! 退出码 0 = 全过；非 0 = 有检查没过（stdout 里逐条打印）。

use gtk::prelude::*;
use puttyplus::menu::{self, MenuBar};
use std::process::Command;

const TITLE: &str = "菜单栏悬停检查";

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

/// 调 xdotool（失败就返回 false，不 panic —— 没装 xdotool 时跳过这条检查）。
fn xdotool(args: &[&str]) -> bool {
    match Command::new("xdotool").args(args).output() {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// 找窗口 id（按标题）。
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

/// 窗口在屏幕上的原点（X11 窗口坐标）。
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

/// 控件在**窗口内容区**里的左上角坐标。
fn offset_in_window(w: &gtk::Widget, win: &gtk::Window) -> Option<(i32, i32)> {
    // gtk4-rs 0.11 的 translate_coordinates(dest, src_x, src_y)
    let (x, y) = w.translate_coordinates(win, 0.0, 0.0)?;
    Some((x.round() as i32, y.round() as i32))
}

/// 控件中心在屏幕上的坐标。
fn screen_center(w: &gtk::Widget, win: &gtk::Window, win_xy: (i32, i32)) -> Option<(i32, i32)> {
    let (x, y) = offset_in_window(w, win)?;
    let alloc = w.allocation();
    Some((
        win_xy.0 + x + alloc.width() / 2,
        win_xy.1 + y + alloc.height() / 2,
    ))
}

fn main() {
    gtk::init().expect("gtk init");

    let bar = MenuBar::new();
    let (btn_file, pop_file) = bar.item("文件");
    let (btn_tool, pop_tool) = bar.item("工具");
    pop_file.set_child(Some(&menu::body()));
    pop_tool.set_child(Some(&menu::body()));

    let win = gtk::Window::new();
    win.set_title(Some(TITLE));
    win.set_default_size(360, 160);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&btn_file);
    row.append(&btn_tool);
    win.set_child(Some(&row));
    win.present();
    pump(600);

    // 找窗口与它的屏幕原点
    let Some(wid) = find_window(TITLE) else {
        eprintln!("找不到窗口（xdotool 不可用？）——跳过");
        std::process::exit(2);
    };
    let Some(wxy) = window_origin(wid) else {
        eprintln!("拿不到窗口位置——跳过");
        std::process::exit(2);
    };
    let Some(p_file) = screen_center(btn_file.upcast_ref::<gtk::Widget>(), &win, wxy) else {
        eprintln!("拿不到按钮坐标——跳过");
        std::process::exit(2);
    };
    let Some(p_tool) = screen_center(btn_tool.upcast_ref::<gtk::Widget>(), &win, wxy) else {
        eprintln!("拿不到按钮坐标——跳过");
        std::process::exit(2);
    };
    println!("[窗口 {wid} @ {wxy:?}] 文件按钮 {p_file:?} · 工具按钮 {p_tool:?}");

    // ① 点开「文件」
    xdotool(&["mousemove", &p_file.0.to_string(), &p_file.1.to_string()]);
    pump(200);
    xdotool(&["click", "1"]);
    pump(500);
    assert!(pop_file.is_visible(), "点「文件」后菜单应当打开");
    println!("[1] 「文件」菜单已打开");

    // ② **只移动指针**到「工具」（不点击）→ 它应当自动打开
    xdotool(&["mousemove", &p_tool.0.to_string(), &p_tool.1.to_string()]);
    pump(800);
    assert!(
        pop_tool.is_visible(),
        "鼠标移到「工具」上时菜单应当**自动打开**（用户 2026-10-28 报的就是这个）"
    );
    println!("[2] 悬停到「工具」→ 菜单自动打开（不用点第二下）");

    // ③ 旧的应当已经关上（不能两个叠着）
    assert!(!pop_file.is_visible(), "「文件」菜单应当已关闭（不能两个叠着）");
    println!("[3] 「文件」菜单已自动收起");

    println!("menubar_hover_smoke: OK");
}
