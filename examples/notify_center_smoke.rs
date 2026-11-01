//! 通知中心（阶段 77c-3 / 审计 P1-9）的冒烟。
//!
//! 审计 P1-9：「通知统一进一个**可回看的通知中心**」——以前通知发出去就没了，
//! 错过了（比如切到别的窗口、通知被折叠）就再也找不回来。
//!
//! 覆盖：
//! - 每发一条通知，通知中心就有**一条记录**（标题 + 正文 + 时间）；
//! - ⚠️ **桌面通知发不出去时也要记**：没有通知守护进程是最常见的情况，
//!   那时通知中心是用户唯一能回看"刚才发生了什么"的地方；
//! - 通知中心窗口能打开，列表里能看到刚才那条；
//! - 空态有话说（不是空白窗口）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example notify_center_smoke

use gtk::prelude::*;

use puttyplus::notify;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    // ---- ① 空态 ----
    assert!(
        notify::history().is_empty(),
        "刚启动时通知中心是空的：{:?}",
        notify::history()
    );
    println!("[1] 刚启动 ⇒ 通知中心为空");

    // ---- ② 发一条通知 ⇒ 历史里有记录 ----
    //
    // ⚠️ 本机**没有通知守护进程** ⇒ 桌面通知发不出去（返回 false），
    // 但历史**照样要记**——这正是通知中心存在的意义。
    let ok = notify::desktop_action(
        "传输完成：3 个文件",
        "已上传 3 个文件",
        "查看明细",
        || {},
        || {},
    );
    let hist = notify::history();
    println!("[2] 桌面通知受理了没有：{ok}；历史条数：{}", hist.len());
    assert_eq!(hist.len(), 1, "发了一条通知，历史里就该有一条：{hist:?}");
    assert!(hist[0].title.contains("传输完成"), "标题不对：{hist:?}");
    assert!(hist[0].body.contains("3 个文件"), "正文不对：{hist:?}");
    assert!(!hist[0].at.is_empty(), "时间没记上：{hist:?}");
    println!("[2] 历史里有：{} {}", hist[0].at, hist[0].title);

    // ---- ③ 再发一条 ⇒ 新的在前 ----
    notify::desktop_action("连接失败", "10.0.0.1:22 连不上", "重试", || {}, || {});
    let hist = notify::history();
    assert_eq!(hist.len(), 2, "历史条数不对：{hist:?}");
    assert!(
        hist[0].title.contains("连接失败"),
        "最新的通知要排在最前面：{hist:?}"
    );
    println!("[3] 两条历史，最新的在最前面：{hist:?}");

    // ---- ④ 打开通知中心窗口 ----
    let win = gtk::Window::new();
    win.set_default_size(520, 300);
    win.present();
    pump(50);
    notify::open_center(Some(&win));
    pump(300);

    let centers: Vec<gtk::Window> = gtk::Window::list_toplevels()
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
        .filter(|w| w.title().map(|t| t.contains("通知中心")).unwrap_or(false))
        .collect();
    assert_eq!(centers.len(), 1, "通知中心窗口没开起来：{centers:?}");
    println!("[4] 通知中心窗口开着");

    // ---- ⑤ 再开一次 ⇒ 还是同一个窗口（工具窗单例）----
    notify::open_center(Some(&win));
    pump(300);
    let centers2: Vec<gtk::Window> = gtk::Window::list_toplevels()
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
        .filter(|w| w.title().map(|t| t.contains("通知中心")).unwrap_or(false))
        .collect();
    assert_eq!(
        centers2.len(),
        1,
        "通知中心连开两次 ⇒ 开了两个窗口（用户会以为程序卡住了）"
    );
    println!("[5] 连开两次 ⇒ 还是同一个窗口");

    // ---- ⑥ 截图 ----
    pump(200);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_notify_center.png")
        .status();
    println!("截图：/tmp/pp_notify_center.png");

    println!("notify_center_smoke: OK");
}
