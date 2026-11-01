//! 「+」新建数据库连接对话框的冒烟（阶段 29）。
//!
//! 钉死三件事：
//! - 按钮是「**连接**」不是「保存」（建完就连上并开工作台，写「保存」是误导）；
//! - **没有**「在哪个会话上连」的下拉框（会话是定死的：用户 2026-10-27
//!   明确"任何情况下都不需要那个下拉框，要换连接就切到别的 tab 再点 +"）；
//! - 堡垒机场景（当前会话不在连接列表里）说明文字讲得清楚，且**不劝用户**
//!   去连接管理器把会话存一条（他没保存是刻意的）。
//!
//! **不碰用户真实配置**：`PUTTYPLUS_DB_INI` / `PUTTYPLUS_CONN_INI` /
//! `PUTTYPLUS_RECENT_INI` 全指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbconn_dialog_smoke

use gtk::prelude::*;
use puttyplus::panel::dbconn;

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

/// 在窗口里递归找某类型的控件。
fn find_widgets<T>(root: &gtk::Widget) -> Vec<T>
where
    T: Clone + IsA<gtk::Widget>,
{
    fn walk<T>(w: &gtk::Widget, out: &mut Vec<T>)
    where
        T: Clone + IsA<gtk::Widget>,
    {
        if let Ok(t) = w.clone().downcast::<T>() {
            out.push(t);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn shot(path: &str) {
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}

fn main() {
    let dir = std::env::temp_dir().join(format!("pp_dbconn_dlg_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // SAFETY：example 是单线程的，设完立刻用。
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", dir.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", dir.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", dir.join("recent.ini"));
    }

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // 堡垒机场景：当前会话**不在**连接列表里
    let sess = "root@10.0.0.9:22";
    dbconn::ui_new(None, sess, |_k, _n| {});
    pump(600);

    // 找到那个对话框
    let dlg = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().map(|w| w.clone()))
        .find(|w| {
            w.title().as_deref() == Some("新建数据库连接（当前会话）")
        })
        .expect("应弹出「新建数据库连接（当前会话）」对话框");
    let root = dlg.upcast_ref::<gtk::Widget>();

    // ---- 1) 按钮是「连接」，不是「保存」 ----
    let buttons: Vec<String> = find_widgets::<gtk::Button>(root)
        .into_iter()
        .filter_map(|b| b.label().map(|l| l.to_string()))
        .collect();
    assert!(
        buttons.iter().any(|b| b == "连接"),
        "按钮里应该有「连接」，实得：{buttons:?}"
    );
    assert!(
        !buttons.iter().any(|b| b == "保存"),
        "这个对话框不该再出现「保存」——建完就连上，写「保存」是误导，实得：{buttons:?}"
    );
    println!("[1] 按钮是「连接」（不是「保存」）");

    // ---- 2) 没有「在哪个会话上连」的下拉框 ----
    let labels: Vec<String> = find_widgets::<gtk::Label>(root)
        .into_iter()
        .map(|l| l.text().to_string())
        .collect();
    assert!(
        !labels.iter().any(|l| l == "在哪个会话上连"),
        "不该再有「在哪个会话上连」的下拉框，实得：{labels:?}"
    );
    // 只应有一个下拉框（数据库类型），不是两个
    assert_eq!(
        find_widgets::<gtk::ComboBoxText>(root).len(),
        1,
        "只该有「数据库类型」一个下拉框"
    );
    println!("[2] 没有「在哪个会话上连」的下拉框（只有一个：数据库类型）");

    // ---- 3) 堡垒机场景的说明 ----
    let tip = labels
        .iter()
        .find(|l| l.contains("挂在当前会话上"))
        .expect("应有一行说明这条连接挂在当前会话上");
    assert!(
        tip.contains(sess),
        "说明里要点名当前会话：{tip}"
    );
    assert!(
        labels.iter().any(|l| l.contains("当前会话没有保存在连接列表里")),
        "堡垒机场景要说清当前会话不在连接列表里，实得：{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("去连接管理器把这个会话存一条")),
        "**不要**劝用户去把会话存下来——他没保存是刻意的"
    );
    println!("[3] 堡垒机场景说明到位，且不劝用户保存会话");

    shot("/tmp/pp_dbconn_dialog.png");
    let _ = std::fs::remove_dir_all(&dir);
    println!("dbconn dialog smoke ok");
}
