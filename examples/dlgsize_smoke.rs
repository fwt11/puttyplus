//! 对话框尺寸冒烟（阶段 73b，审计 P1-11）。
//!
//! **问题**：新建连接对话框自然高度 810px（1366×768 笔记本上底部按钮区
//! 会出屏），数据库连接对话框自然宽度 940px（说明 label 没限宽），
//! 而且数据库连接表单里「记住密码」标签出现了两次。
//!
//! **断言**（走真实对话框）：
//! 1. 新建连接对话框：高度 ≤ 520（长表单套了滚动容器 + 限高）；
//! 2. 新建连接对话框：宽度 ≤ 560（不撑爆小屏）；
//! 3. 新建连接对话框：动作按钮收敛为「取消」「连接」，另有「保存到连接列表」勾选；
//! 4. 数据库连接对话框：宽度落在 520–640；
//! 5. 数据库连接对话框：说明文字有宽度上限（不撑爆对话框）；
//! 6. 数据库连接对话框：「记住密码」只出现一次。
//!
//! ```text
//! xvfb-run -a cargo run --example dlgsize_smoke
//! ```

use gtk::prelude::*;
use puttyplus::connmgr;
use puttyplus::panel::dbconn;

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

fn all_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if let Some(t) = w.downcast_ref::<T>() {
            out.push(t.clone());
        }
    });
    out
}

fn first_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Option<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut found: Option<T> = None;
    walk(&root, &mut |w| {
        if found.is_none() {
            if let Some(t) = w.downcast_ref::<T>() {
                found = Some(t.clone());
            }
        }
    });
    found
}

/// 按文字找勾选框。
fn find_check(root: &impl IsA<gtk::Widget>, label: &str) -> Option<gtk::CheckButton> {
    all_of::<gtk::CheckButton>(root)
        .into_iter()
        .find(|c| c.label().unwrap_or_default() == label)
}

/// 填「常规」页里的名称与主机（按标签找它那一行的输入框）。
fn fill_general(dlg: &gtk::Dialog, name: &str, host: &str) {
    let nb = first_of::<gtk::Notebook>(dlg).expect("页签容器");
    let general = nb.nth_page(Some(0)).expect("常规页");
    for l in all_of::<gtk::Label>(&general) {
        let Some(row) = l.parent() else { continue };
        let e = first_of::<gtk::Entry>(&row);
        match l.text().as_str() {
            "名称" => {
                if let Some(e) = e {
                    e.set_text(name);
                }
            }
            "主机" => {
                if let Some(e) = e {
                    e.set_text(host);
                }
            }
            _ => {}
        }
    }
}

fn find_dialog(title: &str) -> Option<gtk::Dialog> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some(title))
        .find_map(|w| w.downcast::<gtk::Dialog>().ok())
}

/// 对话框底部的动作区（放「取消 / 连接」那排按钮的容器）。
///
/// GTK4 没绑 `gtk_dialog_get_action_area`，但那个容器带
/// `dialog-action-area` 样式类——`dialogs::style_dialog` 也是这么找的。
fn find_action_area(root: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut found: Option<gtk::Widget> = None;
    walk(&root, &mut |w| {
        if found.is_none() && w.has_css_class("dialog-action-area") {
            found = Some(w.clone());
        }
    });
    found
}

/// 对话框里所有可见文字（标签 / 按钮 / 勾选框）。
fn all_text(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in all_of::<gtk::Label>(root) {
        out.push(l.text().to_string());
    }
    for b in all_of::<gtk::Button>(root) {
        if let Some(t) = b.label() {
            out.push(t.to_string());
        }
    }
    out
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let ini = std::env::temp_dir().join(format!("pp_dlgsize_smoke_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_CONN_INI", &ini);
        std::env::set_var("PUTTYPLUS_DB_INI", &ini);
    }

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ---------- 1. 新建连接对话框 ----------
    connmgr::ui_new_connection(None, |_| {});
    pump(600);
    let dlg = find_dialog("新建连接").expect("新建连接对话框要开着");
    let (w, h) = (dlg.width(), dlg.height());
    println!("[实测] 新建连接对话框 {w}×{h}");
    // ⚠️ 阶段 91：限高从「固定 520」改成**按屏幕高度自适应**
    // （用户 2026-11-01 报「这个对话框可以长一点，把内容显示全不要用滚动条」——
    //  「认证」页最后那段说明被切掉半截，还多出一截滚动条）。
    // ⇒ 不能再钉死 520；现在钉两件事：
    //   ① 对话框**不出屏**（1366×768 的笔记本上底部按钮区仍看得见）；
    //   ② **内容一次显示得下**（不用滚动条）。
    let screen_h = gtk::gdk::Display::default()
        .and_then(|d| {
            let mons = d.monitors();
            let first = mons.item(0)?;
            first.downcast::<gtk::gdk::Monitor>().ok()
        })
        .map(|m| m.geometry().height())
        .unwrap_or(1080);
    check!(
        "新建连接对话框不出屏",
        h <= screen_h - 60,
        format!("实测 {h}px / 屏幕 {screen_h}px")
    );
    check!("新建连接对话框宽度 ≤ 560", w <= 560, format!("实测 {w}px"));

    // 内容放得下 ⇒ 不用滚动条（判据是**调整量**，不是滚动条控件的 visible：
    // GTK4 里那根条子一直"存在"，只是没被画出来）
    {
        let sw = all_of::<gtk::ScrolledWindow>(&dlg)
            .into_iter()
            .next()
            .expect("对话框里要有滚动容器");
        let adj = sw.vadjustment();
        let (upper, page) = (adj.upper(), adj.page_size());
        check!(
            "内容一次显示得下（不用滚动条）",
            upper <= page + 0.5,
            format!("内容 {upper}px > 视口 {page}px")
        );
    }

    // 动作按钮：只剩「取消」「连接」
    // ⚠️ 只数**动作区**的按钮：对话框正文里还有「浏览…」「添加…」这类
    // 工具按钮（转发页的表格工具条），它们不是动作。
    let area = find_action_area(&dlg).expect("对话框要有动作区");
    let btns: Vec<String> = all_of::<gtk::Button>(&area)
        .into_iter()
        .filter_map(|b| b.label().map(|s| s.to_string()))
        .collect();
    println!("[实测] 动作区按钮：{btns:?}");
    check!(
        "动作按钮只有「取消」「连接」",
        btns == vec!["取消".to_string(), "连接".to_string()],
        format!("{btns:?}")
    );

    // 「保存到连接列表」勾选
    let checks: Vec<String> = all_of::<gtk::CheckButton>(&dlg)
        .into_iter()
        .map(|c| c.label().unwrap_or_default().to_string())
        .collect();
    println!("[实测] 勾选框：{checks:?}");
    check!(
        "有「保存到连接列表」勾选",
        checks.iter().any(|c| c.contains("保存到连接列表")),
        format!("{checks:?}")
    );

    // 滚动容器：长表单能滚
    check!(
        "长表单套了滚动容器",
        !all_of::<gtk::ScrolledWindow>(&dlg).is_empty(),
        "没找到 ScrolledWindow"
    );

    // ---------- 1b. 勾选决定要不要落盘（动作收敛后的行为）----------
    //
    // 勾上 ⇒ 保存并连接（老对话框的默认行为）；不勾 ⇒ 仅连接、不留痕。
    let connected = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let connected = connected.clone();
        connmgr::ui_new_connection(None, move |_| connected.set(true));
    }
    pump(500);
    let d2 = find_dialog("新建连接").expect("第二个新建连接对话框要开着");
    fill_general(&d2, "冒烟落盘", "10.0.0.9");
    let cb = find_check(&d2, "保存到连接列表").expect("「保存到连接列表」勾选");
    check!("勾选默认是勾上的", cb.is_active(), "默认没勾");
    d2.response(gtk::ResponseType::Ok);
    pump(600);
    let text = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "勾上 + 连接 ⇒ 写进 connections.ini",
        text.contains("冒烟落盘"),
        text
    );
    check!("勾上 + 连接 ⇒ 连上了", connected.get(), "回调没被调用");

    // 不勾 ⇒ 不落盘，但**照样连**
    connected.set(false);
    {
        let connected = connected.clone();
        connmgr::ui_new_connection(None, move |_| connected.set(true));
    }
    pump(500);
    let d3 = find_dialog("新建连接").expect("第三个新建连接对话框要开着");
    fill_general(&d3, "冒烟不落盘", "10.0.0.10");
    let cb3 = find_check(&d3, "保存到连接列表").expect("「保存到连接列表」勾选");
    cb3.set_active(false);
    d3.response(gtk::ResponseType::Ok);
    pump(600);
    let text2 = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "不勾 + 连接 ⇒ 不写进 connections.ini",
        !text2.contains("冒烟不落盘"),
        text2
    );
    check!("不勾 + 连接 ⇒ 照样连", connected.get(), "回调没被调用");

    // ---------- 2. 数据库连接对话框 ----------
    dbconn::ui_new(None, "冒烟会话", |_, _| {});
    pump(600);
    let ddlg = find_dialog("新建数据库连接（当前会话）").expect("数据库连接对话框要开着");
    let (dw, dh) = (ddlg.width(), ddlg.height());
    println!("[实测] 数据库连接对话框 {dw}×{dh}");
    check!(
        "数据库连接对话框宽度落在 520–640",
        (520..=640).contains(&dw),
        format!("实测 {dw}px")
    );

    // 说明 label 有限宽（不然自然宽能到 940px）
    let tip = all_of::<gtk::Label>(&ddlg)
        .into_iter()
        .find(|l| l.text().contains("这条连接挂在当前会话上"));
    let tip_w = tip.as_ref().map(|l| l.max_width_chars());
    println!("[实测] 说明 label 的 max_width_chars = {tip_w:?}");
    check!(
        "说明文字设了宽度上限",
        tip_w.map(|v| v > 0).unwrap_or(false),
        format!("{tip_w:?}")
    );

    // 「记住密码」只出现一次
    let texts = all_text(&ddlg);
    let n_remember = texts.iter().filter(|t| t.contains("记住密码")).count();
    println!(
        "[实测] 含「记住密码」的文本：{:?}",
        texts
            .iter()
            .filter(|t| t.contains("记住密码"))
            .collect::<Vec<_>>()
    );
    check!(
        "「记住密码」只出现一次",
        n_remember == 1,
        format!("出现 {n_remember} 次")
    );

    let _ = std::fs::remove_file(&ini);

    if fails.is_empty() {
        println!("dlgsize smoke ok");
    } else {
        println!("dlgsize smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
