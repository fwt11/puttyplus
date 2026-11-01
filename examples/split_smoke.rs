//! 终端「左右分屏」/「关闭当前终端」冒烟（阶段 37）。
//!
//! **为什么要有这个冒烟**：阶段 12 把终端改成子标签之后，终端面板的父容器
//! 从 `Paned` 变成了 notebook 的页面容器（`Box`）——而分屏的实现要求
//! 「目标面板的父容器本身就是一个 Paned」，不满足就**静默 return**。
//! 于是用户点「左右分屏」看着毫无反应；「关闭当前终端」更是从来没生效过
//! （只剩一块面板时直接 `return false`）。
//!
//! 这类"点了没反应"的回归光看代码看不出来（它不报错、不 panic），只能
//! **断言控件树和状态**：分屏后目标面板的父容器里应当出现分屏容器，
//! `current_is_split()` 应当为真；关闭后应当退回单面板。
//!
//! 覆盖：
//! - 左右分屏 → 进入分屏状态（两半都在）；
//! - 关掉其中一半 → 退回单面板；
//! - 只剩一块时「关闭当前终端」→ **关掉那个终端子页签**（不是没反应）；
//! - 额外终端子页签（子标签栏「+」新建的）也能分屏（阶段 37 之前它不在
//!   任何布局树里，点了没反应）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example split_smoke
//!
//! 需要一个本地 sshd（公钥认证）：默认 127.0.0.1:2233，
//! 可用 PP_SSH_HOST / PP_SSH_PORT / PP_SSH_USER / PP_SSH_KEY 覆盖。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::tabs::Tabs;

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

/// 等某个条件成立（最多 timeout 毫秒）。返回是否成立。
fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

fn main() {
    gtk::init().expect("gtk init");

    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    });
    let key = std::env::var("PP_SSH_KEY")
        .unwrap_or_else(|_| format!("{}/.ssh/id_rsa", std::env::var("HOME").unwrap_or_default()));

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: host.clone(),
        port,
        user: user.clone(),
        keyfile: Some(key),
        ..Default::default()
    };

    let tabs = Tabs::new();
    // 挂进窗口：控件得 realize 才有真实尺寸（分屏容器要算得出位置）
    let win = gtk::Window::new();
    win.set_default_size(1000, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(300);

    tabs.open(&opts);
    pump(800);

    // ① 刚开的标签不是分屏状态
    assert!(
        !tabs.current_is_split(),
        "刚开的标签不该是分屏状态"
    );
    let (page, pane) = {
        let t = &tabs.tabs.borrow()[0];
        (t.page.clone(), t.term_pane.clone())
    };
    println!("[1] 标签已打开，初始不是分屏状态");

    // ② 左右分屏 → 进入分屏状态
    tabs.split_pane(&page, &pane, false);
    pump(300);
    assert!(
        tabs.current_is_split(),
        "点了「左右分屏」但状态还是单面板——分屏没生效"
    );
    // 控件树也要对：目标面板的父容器应当是**分屏容器**（两半都有）
    let parent = pane
        .widget()
        .parent()
        .and_downcast::<gtk::Paned>()
        .expect("分屏后目标面板的父容器应当是 Paned");
    assert!(
        parent.start_child().is_some() && parent.end_child().is_some(),
        "分屏容器里应当有两半"
    );
    println!("[2] 左右分屏已生效（两半都在）");

    // ③ 关掉其中一半 → 退回单面板
    tabs.close_pane(&page, &pane);
    pump(300);
    assert!(
        !tabs.current_is_split(),
        "关掉一半之后应当退回单面板"
    );
    println!("[3] 关掉一半，已退回单面板");

    // ④ 只剩一块时「关闭当前终端」→ 关掉那个**终端子页签**
    //    （阶段 37 之前这里是 `return false`：用户点着像没反应）
    let (sub, pages_before) = {
        let t = &tabs.tabs.borrow()[0];
        let sub = t.panel_st.sub_tabs.borrow().clone().expect("有子标签栏");
        let pages_before = sub.page_count();
        (sub, pages_before)
    };
    assert!(
        tabs.close_pane_current(),
        "只剩一块时「关闭当前终端」应当有效果（关掉终端子页签）"
    );
    pump(300);
    let pages_after = sub.page_count();
    assert!(
        pages_after < pages_before,
        "「关闭当前终端」后终端子页签数应减少（{pages_before} → {pages_after}）"
    );
    println!("[4] 单面板时「关闭当前终端」已关掉终端子页签（{pages_before} → {pages_after}）");

    // ⑤ 额外终端子页签（子标签栏「+」新建的）也能分屏
    //    （阶段 37 之前它不在任何布局树里 → 点了没反应）
    tabs.new_term_page(&page);
    pump(500);
    let extra = {
        let t = &tabs.tabs.borrow()[0];
        t.term_pages
            .borrow()
            .first()
            .cloned()
            .expect("应当多出一个终端子页签")
    };
    let extra_pane = extra.1.clone();
    tabs.split_pane(&page, &extra_pane, false);
    pump(300);
    assert!(
        tabs.current_is_split(),
        "额外终端子页签上的「左右分屏」应当生效"
    );
    println!("[5] 额外终端子页签也能左右分屏");

    println!("split_smoke: OK");
}
