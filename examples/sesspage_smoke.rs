//! 连接编辑器「会话」页的冒烟（阶段 69 之二，审计 P1-4）。
//!
//! **问题**：会话相关的设置（保活 / TERM / 认证优先级 / 终端字号 / 响铃）
//! 以前只有一处能改——标签上的「会话选项…」对话框，而那是**运行时临时
//! 覆盖**；用户想给一条连接**固定**一套会话设置（堡垒机要固定认证顺序、
//! 跳板机要固定保活）根本没有地方填。
//!
//! **断言**（走真实对话框，不走内部函数）：
//! 1. 连接编辑器有**两个页签**：「常规」「会话」；
//! 2. 「会话」页里那五个控件都在；
//! 3. 在「会话」页填的值 → 保存 → 真的写进 connections.ini；
//! 4. 重新打开编辑器 → 「会话」页**预填的正是存下去的值**；
//! 5. 改一个值再存 → ini 里跟着变（局部回写，不碰别的键）。
//!
//! ```text
//! xvfb-run -a cargo run --example sesspage_smoke
//! ```

use gtk::prelude::*;
use puttyplus::connmgr::{self, ConnEntry};

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

/// 递归走一遍控件树。
fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

/// 树里第一个某类型的控件。
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

/// 树里所有某类型的控件。
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

/// 占位文字匹配的输入框（用来区分 TERM 与字号两个 Entry）。
fn entry_with_placeholder(root: &impl IsA<gtk::Widget>, ph: &str) -> Option<gtk::Entry> {
    all_of::<gtk::Entry>(root)
        .into_iter()
        .find(|e| e.placeholder_text().as_deref() == Some(ph))
}

/// 表单里"标签文字 → 同一行的输入框"。
///
/// 表单版式是「标签在上、控件在下」的一行 Box ⇒ 标签的父容器就是那一行，
/// 里面的 Entry 就是这一行的控件。
fn entry_by_label(root: &impl IsA<gtk::Widget>, label: &str) -> Option<gtk::Entry> {
    for l in all_of::<gtk::Label>(root) {
        if l.text() != label {
            continue;
        }
        let Some(row) = l.parent() else { continue };
        if let Some(e) = first_of::<gtk::Entry>(&row) {
            return Some(e);
        }
    }
    None
}

/// 找到标题对上的那个对话框窗口。
fn find_dialog(title: &str) -> Option<gtk::Dialog> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some(title))
        .find_map(|w| w.downcast::<gtk::Dialog>().ok())
}

fn tab_titles(nb: &gtk::Notebook) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..nb.n_pages() {
        let Some(page) = nb.nth_page(Some(i)) else {
            continue;
        };
        if let Some(l) = nb.tab_label(&page).and_then(|w| w.downcast::<gtk::Label>().ok()) {
            out.push(l.text().to_string());
        }
    }
    out
}

/// 「会话」页（页签序号 3：常规 / 认证 / 转发 / 会话）。
fn session_page(nb: &gtk::Notebook) -> gtk::Widget {
    nb.nth_page(Some(3)).expect("应当有第四个页签（会话）")
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ⚠️ 测试不许写用户真实配置：ini 指到临时文件
    let ini = std::env::temp_dir().join(format!("pp_sesspage_smoke_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑，不存在并发改环境变量
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &ini) };

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

    // ---------- 1. 打开连接编辑器，看页签 ----------
    connmgr::ui_new_connection(None, |_| {});
    pump(500);
    let dlg = find_dialog("新建连接").expect("新建连接对话框要开着");
    let nb = first_of::<gtk::Notebook>(&dlg).expect("对话框里要有页签容器");

    check!(
        "连接编辑器有四个页签",
        nb.n_pages() == 4,
        format!("{} 个", nb.n_pages())
    );
    check!(
        "页签是「常规」「认证」「转发」「会话」",
        tab_titles(&nb)
            == vec![
                "常规".to_string(),
                "认证".to_string(),
                "转发".to_string(),
                "会话".to_string()
            ],
        format!("{:?}", tab_titles(&nb))
    );

    // ---------- 2. 「会话」页里那五个控件都在 ----------
    let sp = session_page(&nb);
    let keep = first_of::<gtk::SpinButton>(&sp);
    let term = entry_with_placeholder(&sp, "留空 = xterm-256color");
    let font = entry_with_placeholder(&sp, "留空 = 跟随首选项");
    let auth = first_of::<gtk::ComboBoxText>(&sp);
    let bell = first_of::<gtk::CheckButton>(&sp);
    check!(
        "会话页有保活/TERM/认证/字号/响铃",
        keep.is_some()
            && term.is_some()
            && font.is_some()
            && auth.is_some()
            && bell.is_some(),
        format!(
            "keep={} term={} font={} auth={} bell={}",
            keep.is_some(),
            term.is_some(),
            font.is_some(),
            auth.is_some(),
            bell.is_some()
        )
    );

    // ---------- 3. 填值 + 填常规页 → 保存 ----------
    keep.as_ref().unwrap().set_value(0.0);
    term.as_ref().unwrap().set_text("screen-256color");
    auth.as_ref().unwrap().set_active_id(Some("publickey-first"));
    font.as_ref().unwrap().set_text("12.0");
    bell.as_ref().unwrap().set_active(false);

    let general = nb.nth_page(Some(0)).expect("常规页");
    let name = entry_by_label(&general, "名称").expect("名称输入框");
    let host = entry_by_label(&general, "主机").expect("主机输入框");
    name.set_text("冒烟连接");
    host.set_text("10.0.0.1");

    // 阶段 73b：新建对话框的动作收敛成「取消」+「连接」，保存与否看
    // 「保存到连接列表」勾选（默认勾上 ⇒ 等同老对话框的「保存并连接」）。
    dlg.response(gtk::ResponseType::Ok); // 连接
    pump(500);

    // ---------- 4. 真的写进 connections.ini 了 ----------
    let text = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "ini 里有会话设置那几把键",
        text.contains("keepalive=0")
            && text.contains("term=screen-256color")
            && text.contains("authorder=publickey-first")
            && text.contains("font=12.0")
            && text.contains("bell=0"),
        text
    );

    let entries = connmgr::load_all();
    let e = entries
        .iter()
        .find(|e| e.name == "冒烟连接")
        .expect("刚存的连接要能读回来");
    check!(
        "读回来的会话设置与填的一致",
        e.sess.keepalive_sec == Some(0)
            && e.sess.term_type.as_deref() == Some("screen-256color")
            && e.sess.auth_order == Some(puttyplus::args::AuthOrder::PublicKeyFirst)
            && e.sess.font_size == Some(12.0)
            && e.sess.bell == Some(false),
        format!("{:?}", e.sess)
    );
    // 连接级默认值要真的进会话参数
    let o = e.to_opts();
    check!(
        "连接默认值进到会话参数里",
        o.keepalive_sec == 0
            && o.term_type.as_deref() == Some("screen-256color")
            && o.font_size == Some(12.0)
            && o.bell == Some(false),
        format!("keep={} term={:?} font={:?} bell={:?}", o.keepalive_sec, o.term_type, o.font_size, o.bell)
    );

    // ---------- 5. 重新打开编辑器：会话页预填的就是存下去的值 ----------
    let entry = ConnEntry {
        name: e.name.clone(),
        host: e.host.clone(),
        user: e.user.clone(),
        password: None,
        port: e.port,
        custom: true,
        proto: e.proto.clone(),
        sercfg: e.sercfg.clone(),
        forwards: Vec::new(),
        inproc_forwards: Vec::new(),
        sess: e.sess.clone(),
        keyfile: e.keyfile.clone(),
        auth_method: e.auth_method,
    };
    connmgr::ui_edit_connection(None, entry, || {});
    pump(500);
    let dlg2 = find_dialog("编辑连接").expect("编辑连接对话框要开着");
    let nb2 = first_of::<gtk::Notebook>(&dlg2).expect("对话框里要有页签容器");
    let sp2 = session_page(&nb2);
    let keep2 = first_of::<gtk::SpinButton>(&sp2).expect("保活");
    let term2 = entry_with_placeholder(&sp2, "留空 = xterm-256color").expect("TERM");
    let font2 = entry_with_placeholder(&sp2, "留空 = 跟随首选项").expect("字号");
    let auth2 = first_of::<gtk::ComboBoxText>(&sp2).expect("认证优先级");
    let bell2 = first_of::<gtk::CheckButton>(&sp2).expect("响铃");
    check!(
        "重开后会话页预填的是存下去的值",
        keep2.value() == 0.0
            && term2.text() == "screen-256color"
            && font2.text() == "12.0"
            && auth2.active_id().as_deref() == Some("publickey-first")
            && !bell2.is_active(),
        format!(
            "keep={} term={} font={} auth={:?} bell={}",
            keep2.value(),
            term2.text(),
            font2.text(),
            auth2.active_id(),
            bell2.is_active()
        )
    );

    // ---------- 6. 改一个值再存 → ini 跟着变（局部回写） ----------
    keep2.set_value(45.0);
    dlg2.response(gtk::ResponseType::Ok); // 保存
    pump(500);
    let text2 = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "改一个值只动那把键",
        text2.contains("keepalive=45")
            && text2.contains("term=screen-256color")
            && text2.contains("font=12.0"),
        text2
    );

    let _ = std::fs::remove_file(&ini);

    if fails.is_empty() {
        println!("sesspage smoke ok");
    } else {
        println!("sesspage smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
