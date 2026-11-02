//! 可访问名称冒烟（阶段 82 / 审计 P2-7）。
//!
//! **钉死的不变量**：界面里每一个**纯图标按钮**都必须
//!   1. 有非空 tooltip（用户看得懂它是干什么的），并且
//!   2. 有**可访问名称**（读屏软件念的是这个，不是 tooltip）。
//!
//! 少一条都会让界面悄悄退化：读屏用户只会听到"按钮"，
//! 界面上却看不出少了什么——**不报错、不 panic、截图也正常**。
//!
//! 用法：cargo run --example a11y_smoke
use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::a11y;
use puttyplus::panel::{dbui, SftpPanel};
use std::rc::Rc;

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

/// 遍历控件树。
fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    let mut c = w.first_child();
    while let Some(ch) = c {
        let next = ch.next_sibling();
        walk(&ch, out);
        c = next;
    }
}

/// 是不是**纯图标**按钮（child 是 Image，不是「图标 + 文字」的 Box）。
fn icon_only_of(w: &gtk::Widget) -> bool {
    let child = if let Some(b) = w.downcast_ref::<gtk::Button>() {
        b.child()
    } else if let Some(b) = w.downcast_ref::<gtk::ToggleButton>() {
        b.child()
    } else if let Some(b) = w.downcast_ref::<gtk::MenuButton>() {
        b.child()
    } else {
        None
    };
    child.and_downcast::<gtk::Image>().is_some()
}

/// 检查一棵树里的所有纯图标按钮。
fn check_tree(root: &gtk::Widget, where_: &str) -> (usize, Vec<String>) {
    let mut all = Vec::new();
    walk(root, &mut all);
    let mut n = 0usize;
    let mut bad = Vec::new();
    for w in &all {
        if !icon_only_of(w) {
            continue;
        }
        n += 1;
        let tip = w.tooltip_text().unwrap_or_default();
        let has_label = a11y::has_label(w);
        if tip.trim().is_empty() || !has_label {
            bad.push(format!(
                "{where_}: tooltip={:?} 可访问名称={}",
                tip.trim(),
                if has_label { "有" } else { "没有" }
            ));
        }
    }
    (n, bad)
}

fn main() {
    gtk::init().expect("gtk init");

    let mut roots: Vec<(String, gtk::Widget)> = Vec::new();

    /* ---- ① 侧栏 ---- */
    let tabs = puttyplus::tabs::Tabs::new();
    let sidebar = puttyplus::sidebar::Sidebar::new(Rc::clone(&tabs));
    sidebar.setup();
    roots.push(("侧栏".into(), sidebar.widget.clone().upcast()));

    /* ---- ② 文件面板 ---- */
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 2233;
    opts.user = "fwt".into();
    let panel = SftpPanel::new(&opts);
    let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);
    let remote_pane = puttyplus::panel::pane::build_pane(&st, true);
    roots.push(("文件面板工具条".into(), remote_pane.toolbar.clone().upcast()));

    /* ---- ③ 工作台（DB） ---- */
    dbui::ensure(&st);
    dbui::mount_for_test(&st);
    pump(600);
    for b in dbui::sql_row_buttons_now(&st) {
        roots.push(("工作台 SQL 行".into(), b.upcast()));
    }
    // 结果页签的工具条（编辑 / 插入 / 删除 / 导出……）
    for tip in dbui::result_toolbar_tips(&st, 0) {
        let _ = tip;
    }
    if let Some((a, b, c)) = dbui::edit_toolbar_buttons(&st, 0) {
        roots.push(("结果工具条".into(), a.upcast()));
        roots.push(("结果工具条".into(), b.upcast()));
        roots.push(("结果工具条".into(), c.upcast()));
    }

    // tooltip → 可访问名称的同步走的是 notify 信号，要泵一下主循环
    pump(300);

    let mut total = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for (name, root) in &roots {
        let (n, b) = check_tree(root, name);
        total += n;
        bad.extend(b);
        println!("[{name}] 纯图标按钮 {n} 个");
    }
    println!("共检查 {total} 个纯图标按钮");

    /* ---- ④ 对话框：默认框看得见 + 词表统一（审计 P2-5） ---- */
    use puttyplus::panel::dialogs;
    let dlg = puttyplus::windows::dialog(
        "冒烟",
        None,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("确定", gtk::ResponseType::Ok),
        ],
    );
    dlg.set_default_response(gtk::ResponseType::Cancel);
    dialogs::mark_default(&dlg, "取消");
    let labels = dialogs::button_labels(&dlg);
    println!("[对话框] 按钮 {labels:?}，默认 {:?}", dialogs::default_button_label(&dlg));
    assert!(
        labels.contains(&"确定".to_string()) && labels.contains(&"取消".to_string()),
        "对话框按钮该是「确定 / 取消」，实际 {labels:?}"
    );
    assert_eq!(
        dialogs::default_button_label(&dlg).as_deref(),
        Some("取消"),
        "默认按钮（回车按的那个）要看得见 ⇒ 得挂 pp-default"
    );
    // 这个冒烟自己造了个没父窗口的对话框，别污染审计计数
    puttyplus::windows::reset_dialog_audit();

    if !bad.is_empty() {
        println!("\n❌ 以下按钮不合格：");
        for b in &bad {
            println!("  - {b}");
        }
        std::process::exit(1);
    }
    println!("\n✔ 每个纯图标按钮都有 tooltip + 可访问名称");
}
