//! 「传输队列」按钮的新图标：**角标数字外面套一个在转的圈**。
//!
//! 用户原话（2026-11-11）：
//! 「这个感觉不是很好，或者你可以在 1 周围有个圆圈在转？或者你想想怎么能
//! 更好点？发挥你的想象力。」
//!
//! 这个冒烟钉四件事：
//!
//! 1. **角标数字读得出来**（`btn_state` 走的是生产代码那条路）；
//! 2. **蓝圆跟着数字走**（有数字才有底色 ⇒ 传完不留空蓝圆）；
//! 3. **圈真的在转**（角度在变）；
//! 4. **传完了干净**（数字空、无底色、圈不转）。
//!
//! 顺带出截图（浅色 / 深色，各一张忙态）供人眼看。
//!
//! ```text
//! xvfb-run -a cargo run --example xfer_icon_shot
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{queue, SftpPanel};
use puttyplus::theme::{self, Theme};

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

fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("截图：{path}");
}

/// 造一个**带角标数字、转着圈**的传输队列按钮（用的就是生产代码那条路）。
fn build(theme: Theme, out: &str) {
    theme::apply(theme);
    let panel = SftpPanel::new(&ConnOptions::default());
    let st = panel.st.clone();

    // 整个面板（远程面板的工具栏就在上半边）——
    // ⚠️ **不能直接把 toolbar 塞进窗口**：它已经有父节点了 ⇒ GTK 会
    //    拒绝（`gtk_window_set_child` 断言失败）⇒ 窗口里会是个空的。
    let root = panel.widget().clone().upcast::<gtk::Widget>();
    let win = gtk::Window::new();
    win.set_title(Some("传输队列图标"));
    win.set_default_size(1100, 620);
    win.set_child(Some(&root));
    win.present();
    pump(400);

    let b = st.queue_btn.borrow().clone().expect("队列按钮");

    // ---- 1) 设数字 + 起圈（跟生产代码同一条路） ----
    queue::set_badge(&st, 1);
    queue::set_btn_spinning(&st, true);
    // 忙态（跟 `refresh_btn` 里那段一样）
    b.remove_css_class("flat");
    b.add_css_class("pp-xfer-busy");
    pump(300);

    // ---- 2) 角标数字读得出来（`btn_state` 走的是同一条路） ----
    let (badge, busy, badge_on) = queue::btn_state(&st).expect("按钮状态");
    assert_eq!(badge, "1", "角标数字读不出来");
    assert!(busy, "忙态没上");
    assert!(badge_on, "蓝圆没画（数字有了底色就该有）");
    println!("[1] 角标 = {badge}，忙态 = {busy}，底色 = {badge_on} ✓");

    // ---- 3) 圈真的在转 ----
    assert!(queue::btn_ring_spinning(&st), "圈没转起来");
    let a0 = queue::btn_ring_angle(&st).expect("圈的角度");
    pump(300);
    let a1 = queue::btn_ring_angle(&st).expect("圈的角度");
    assert!((a1 - a0).abs() > 0.01, "圈的角度没变（{a0} → {a1}）⇒ 它根本没在转");
    println!("[2] 圈在转（{a0:.3} → {a1:.3}）✓");

    // 忙态截图（人眼看的就是这张）
    shot(out);

    // ---- 4) 传完了不许剩个空蓝圆（用户 2026-10-07 报过） ----
    //
    // ⚠️ **走 `refresh_btn`**（生产代码那条路）：它会按「还在传几个」重算
    //    ⇒ 数字清空、忙态摘掉、圈停转。没有真的传输 ⇒ 计数就是 0。
    queue::refresh_btn(&st);
    pump(200);
    let (badge, busy, badge_on) = queue::btn_state(&st).expect("按钮状态");
    assert_eq!(badge, "", "传完了角标数字还在");
    assert!(!busy, "传完了还是忙态");
    assert!(!badge_on, "传完了还剩个蓝圆（用户 2026-10-07 报过）");
    assert!(!queue::btn_ring_spinning(&st), "传完了圈还在转");
    println!("[3] 传完后：数字空、无底色、圈不转 ✓");
}

fn main() {
    gtk::init().expect("gtk init");
    build(Theme::Light, "/tmp/pp_xfer_icon_light.png");
    build(Theme::Dark, "/tmp/pp_xfer_icon_dark.png");
    println!("xfer_icon_shot: OK");
}
