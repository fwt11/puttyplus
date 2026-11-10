//! **载入占位层的位置与大小**冒烟（用户 2026-11-10：
//! 「loading 的动画和提示文字放在屏幕中部吧，不要放在顶部」）。
//!
//! 验两件事：
//! - **转圈与提示文字在页面中间**（不是贴在顶上）；
//! - **尺寸合适**：转圈 24px（32px 那会儿又黑又重）。
//!
//! 走的是**真实构建路径**（`fileedit::open_placeholder_for_smoke` → `build`
//! + `attach_tab`），只是**不发读请求** ⇒ 占位层一直在 ⇒ 几何量得准。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fileedit_placeholder_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::fileedit;

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

fn test_opts() -> ConnOptions {
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
    ConnOptions {
        proto: "ssh".into(),
        host,
        port,
        user,
        keyfile: Some(key),
        ..Default::default()
    }
}

fn main() {
    gtk::init().expect("gtk init");
    let opts = test_opts();

    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_default_size(1100, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(500);

    let st = tabs.current_panel_state().expect("应有当前面板");

    // 建一个**永远停在载入中**的页（不发读请求）
    let path = "/tmp/pp_placeholder_smoke.log";
    fileedit::open_placeholder_for_smoke(&st, path, "正在读取 t.log…");
    pump(600); // 等布局算完

    let l = fileedit::placeholder_layout(&st, path).expect("该有占位层");
    println!(
        "[0] 占位区 {}×{}；内容盒 ({},{}) {}×{}；转圈 ({},{}) {}×{}；文字 ({},{}) {}×{}",
        l.area_w, l.area_h,
        l.content_x, l.content_y, l.content_w, l.content_h,
        l.spinner_x, l.spinner_y, l.spinner_w, l.spinner_h,
        l.label_x, l.label_y, l.label_w, l.label_h
    );

    // ---- 1) 占位区得够大（正文那一块，不是零高）----
    assert!(
        l.area_h > 200,
        "占位区只有 {} 高：正文没铺开，量不到位置",
        l.area_h
    );

    // ---- 2) 内容（转圈 + 文字这一组）在**垂直中间** ----
    //
    // ⚠️ 旧行为：占位层是「竖直 Box 里放一个不扩张的子盒」⇒ 子盒只分到
    // 自然高度、还排在**顶上** ⇒ 转圈贴在页面顶部（用户看到的正是这个）。
    //
    // ⚠️ **量整组，不量转圈**：转圈自己的 allocation 跟它画出来的位置有
    // 出入（量它会误判）⇒ 量内容盒（Overlay 的 overlay 子控件）才准。
    let content_center = l.content_y + l.content_h / 2;
    let area_center = l.area_h / 2;
    let off = (content_center - area_center).abs();
    assert!(
        off <= 6,
        "内容没在垂直中间：中心在 {content_center}，区域中心在 {area_center}（差 {off}px）"
    );

    // ---- 3) 内容在**水平中间** ----
    let content_center_x = l.content_x + l.content_w / 2;
    let off_x = (content_center_x - l.area_w / 2).abs();
    assert!(
        off_x <= 6,
        "内容没在水平中间：中心在 {content_center_x}，区域中心在 {}（差 {off_x}px）",
        l.area_w / 2
    );

    // ---- 4) 尺寸：24px（32px 那会儿又黑又重）----
    assert_eq!(
        (l.spinner_w, l.spinner_h),
        (24, 24),
        "转圈的大小该是 24×24，实际 {}×{}",
        l.spinner_w,
        l.spinner_h
    );

    // ---- 5) 转圈与文字之间要留点距离（别挤在一起）----
    //
    // ⚠️ 用**内容盒**的坐标算（转圈自己的 allocation 不可靠）。
    let gap = l.label_y - l.content_y - l.spinner_h;
    assert!(gap >= 6, "转圈与文字挤在一起了（间距 {gap}px）");

    println!("fileedit placeholder smoke ok");

    // ⚠️ 想截图时：PP_HOLD=1 ⇒ 窗口别关（外层用 scrot 抓屏）
    if std::env::var("PP_HOLD").is_ok() {
        println!("（PP_HOLD ⇒ 保持窗口 60 秒，方便截图）");
        pump(60_000);
    }
}
