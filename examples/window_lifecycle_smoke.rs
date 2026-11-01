//! 容器与窗口生命周期的冒烟（审计 P2-17、P2-16，阶段 67）。
//!
//! **问题 1（同一工具开出多个副本）**：连接管理器 / 私钥 / 凭据库都能从
//! 多个入口打开，而且**每次都新建一个窗口**——用户看见两个连接管理器，
//! 第二个还盖着第一个，像程序卡住了。审计 P2-17 还点了"关闭行为不统一
//! （队列是 hide、别的是 destroy）、有的记尺寸有的不记"。
//!
//! **问题 2（对话框没有父窗口）**：新建连接、手动连接这些对话框**没有**
//! 父窗口 ⇒ 多窗口时可能浮在别的窗口上面，甚至被主窗口挡住（审计 P2-16）。
//!
//! **断言**：
//! 1. 同一工具**连开两次** ⇒ 窗口数不变，而且拿到的是**同一个窗口**；
//! 2. 关掉工具窗 ⇒ 从登记表里摘掉（下次开是新的）；
//! 3. **每个对话框都有父窗口**（`transient_for` 非空，且登记表里没有
//!    "无父窗口"的记录）；
//! 4. 工具窗**不是模态**（模态的会挡住主窗口）。
//!
//! ```text
//! xvfb-run -a cargo run --example window_lifecycle_smoke
//! ```

use gtk::prelude::*;
use puttyplus::windows::{self, ToolKind};

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

    // 主窗口（对话框的默认父窗口）
    let main_win = gtk::Window::new();
    main_win.set_title(Some("window lifecycle smoke"));
    main_win.set_default_size(1000, 700);
    main_win.present();
    windows::set_parent(main_win.clone());
    pump(200);

    // ---- 1) 同一工具连开两次 ⇒ 窗口数不变 ----
    //
    // ⚠️ **走真实入口**（`keyui::open`），不是直接调注册表——
    // 直接调注册表测的是"我写的那段逻辑对不对"，测不到"用户点的那个
    // 入口到底开了几个窗口"（AGENTS.md 的老教训）。
    // ⚠️ 只数**看得见**的顶层窗口：`gtk_window_list_toplevels()` 里混着
    // GTK 内部的隐藏窗口（弹层宿主之类），数它们会把"开没开新窗口"算错。
    //
    // ⚠️ 样本用**私钥管理器**：连接管理器在阶段 77b 删掉了（所有连接操作
    // 收敛到侧栏），工具窗单例的语义得另找样本验。
    let toplevels = || {
        gtk::Window::list_toplevels()
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Window>())
            .filter(|w| w.is_visible())
            .count()
    };
    let n0 = toplevels();
    puttyplus::keyui::open(Some(&main_win));
    pump(300);
    let n1 = toplevels();
    assert_eq!(n1, n0 + 1, "私钥管理器第一次没开起来");
    assert!(windows::is_open(ToolKind::Key), "私钥管理器没登记进工具窗表");

    // **再开一次** ⇒ 窗口数不许涨
    puttyplus::keyui::open(Some(&main_win));
    pump(300);
    let n2 = toplevels();
    assert_eq!(
        n2, n1,
        "私钥管理器连开两次 ⇒ 顶层窗口从 {n1} 涨到 {n2}（审计 P2-17：用户看见两个窗口）"
    );
    assert_eq!(
        windows::open_count(),
        1,
        "登记表里有 {} 个工具窗（应当只有私钥管理器一个）",
        windows::open_count()
    );
    println!("[1] 同一工具连开两次 ⇒ 还是同一个窗口（顶层窗口 {n1} → {n2}）✓");

    // ---- 2) 工具窗**不是模态**（模态的会挡住主窗口）----
    let mgr = windows::tool_window(ToolKind::Key).expect("私钥管理器应当开着");
    assert!(
        !mgr.is_modal(),
        "工具窗不该是模态的（模态 ⇒ 主窗口整个不能用）"
    );
    println!("[2] 工具窗不是模态的 ✓");

    // ---- 3) 关掉工具窗 ⇒ 从登记表里摘掉 ----
    mgr.close();
    pump(200);
    assert!(
        !windows::is_open(ToolKind::Key),
        "关掉了还在登记表里（下次打开会拿到已销毁的窗口）"
    );
    println!("[3] 关掉 ⇒ 从登记表摘掉 ✓");

    // ---- 4) 每个对话框都有父窗口 ----
    windows::reset_dialog_audit();
    let dlg_new = windows::dialog(
        "新建连接",
        None,
        &[("取消", gtk::ResponseType::Cancel), ("确定", gtk::ResponseType::Ok)],
    );
    let dlg_manual = windows::dialog(
        "手动连接 — PuTTYPlus",
        None,
        &[("取消", gtk::ResponseType::Cancel)],
    );
    pump(150);
    for (name, d) in [("新建连接", &dlg_new), ("手动连接", &dlg_manual)] {
        assert!(
            d.transient_for().is_some(),
            "{name} 没有父窗口（审计 P2-16：多窗口时它会浮在别的窗口上面）"
        );
    }
    let no_parent = windows::dialogs_without_parent();
    assert!(
        no_parent.is_empty(),
        "这些对话框建的时候就没有父窗口：{no_parent:?}"
    );
    println!("[4] 对话框都挂着父窗口 ✓");

    // 关掉它们，别留着挡镜头
    dlg_new.destroy();
    dlg_manual.destroy();
    pump(100);

    println!("window_lifecycle_smoke: OK");
}
