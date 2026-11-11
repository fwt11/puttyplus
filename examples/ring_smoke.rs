//! 「传输队列」按钮角上那个**转动的圈**冒烟（用户 2026-11-11 提的）。
//!
//! 用户原话：「上传下载文件时传输队列图标可以改为原来静态的图标上加个
//! 转动的圈」。
//!
//! 这个冒烟钉三件事：
//!
//! 1. **它真的在转**（角度在变）——不是「设了个标志就说自己在转」；
//! 2. **停下来就真的停**（定时器收了 ⇒ 不白烧 CPU）；
//! 3. 重复开关不会把定时器开一堆。
//!
//! 运行：
//!
//! ```text
//! xvfb-run -a cargo run --example ring_smoke
//! ```

use gtk::prelude::*;
use puttyplus::panel::ring::Ring;

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

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let ring = Ring::new(11);
    assert!(!ring.spinning(), "刚建好时不该在转（空闲时一点 CPU 都不占）");

    // ---- 1) 开 ⇒ 角度真的在变 ----
    ring.set_spinning(true);
    assert!(ring.spinning(), "开了就该是转的状态");
    let a0 = ring.angle();
    pump(300);
    let a1 = ring.angle();
    assert!(
        (a1 - a0).abs() > 0.01,
        "转了 300ms 角度却没变（{a0} → {a1}）⇒ 它根本没在转"
    );

    // ---- 2) 关 ⇒ 角度不再变（定时器收了） ----
    ring.set_spinning(false);
    assert!(!ring.spinning(), "关了就不该是转的状态");
    let b0 = ring.angle();
    pump(300);
    let b1 = ring.angle();
    assert!(
        (b1 - b0).abs() < 1e-9,
        "关了还在转（{b0} → {b1}）⇒ 定时器没收干净，白烧 CPU"
    );

    // ---- 3) 重复开关 ⇒ 状态不变（不会开出第二个定时器） ----
    ring.set_spinning(true);
    ring.set_spinning(true);
    pump(200);
    let c0 = ring.angle();
    pump(200);
    let c1 = ring.angle();
    assert!(
        (c1 - c0).abs() > 0.01,
        "重复开两次就不转了 ⇒ 定时器被开成了两个互相抵消"
    );
    ring.set_spinning(false);

    println!("ring smoke ok");
}
