//! 连接编辑器「转发」页的冒烟（阶段 71，审计 P1-3）。
//!
//! **问题**：端口转发以前是两个原始规则域（终端转发 / 进程内端口映射），
//! 长得一模一样，用户分不清哪条走哪套机制，也得自己拼
//! `L:[addr:]port:host:port` 这种字符串。
//!
//! **断言**（走真实对话框）：
//! 1. 连接编辑器有**四个页签**：常规 / 认证 / 转发 / 会话；
//! 2. 「转发」页里有一张**结构化表**（类型/监听/目标/机制/启用）；
//! 3. 用「添加…」加一条规则 ⇒ 表里多一行，类型与机制都看得见；
//! 4. 保存 ⇒ 真的写进 `connections.ini` 的 `forwardings`（**存储格式不变**）；
//! 5. 原始规则框里粘贴规则 ⇒ 以它为准（粘贴导入的老路径还在）。
//!
//! ```text
//! xvfb-run -a cargo run --example fwdpage_smoke
//! ```

use gtk::prelude::*;
use puttyplus::connmgr;

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

/// 表里第 idx 行的第 col 列文字（0=类型 1=监听 2=目标 3=机制）。
fn cell_text(view: &gtk::TreeView, idx: usize, col: i32) -> Option<String> {
    let model = view.model()?;
    let mut it = model.iter_first()?;
    for _ in 0..idx {
        if !model.iter_next(&mut it) {
            return None;
        }
    }
    Some(
        model
            .get_value(&it, col)
            .get::<String>()
            .unwrap_or_default(),
    )
}

fn row_count(view: &gtk::TreeView) -> usize {
    let Some(model) = view.model() else {
        return 0;
    };
    let mut n = 0;
    if let Some(mut it) = model.iter_first() {
        n += 1;
        while model.iter_next(&mut it) {
            n += 1;
        }
    }
    n
}

/// 按文字找按钮。
fn button_with_label(root: &impl IsA<gtk::Widget>, label: &str) -> Option<gtk::Button> {
    all_of::<gtk::Button>(root)
        .into_iter()
        .find(|b| b.label().as_deref() == Some(label))
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let ini = std::env::temp_dir().join(format!("pp_fwdpage_smoke_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
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

    // ---------- 1. 打开连接编辑器 ----------
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

    // ---------- 2. 「转发」页里有一张结构化表 ----------
    let fp = nb.nth_page(Some(2)).expect("转发页");
    let view = first_of::<gtk::TreeView>(&fp).expect("结构化表");
    let headers: Vec<String> = (0..view.n_columns() as i32)
        .filter_map(|i| view.column(i).map(|c| c.title().to_string()))
        .collect();
    check!(
        "表头是 类型/监听/目标/机制/启用",
        headers == vec!["类型", "监听", "目标", "机制", "启用"],
        format!("{headers:?}")
    );
    check!("表里还没有规则", row_count(&view) == 0, format!("{}", row_count(&view)));

    // ---------- 3. 「添加…」加一条规则 ----------
    let add = button_with_label(&fp, "添加…").expect("添加按钮");
    add.emit_clicked();
    pump(400);
    let rdlg = find_dialog("转发规则").expect("转发规则对话框要开着");
    // 填：类型=本地、机制=终端、监听=9000、目标=host:80
    let listens: Vec<gtk::Entry> = all_of::<gtk::Entry>(&rdlg);
    check!(
        "规则对话框里有监听与目标两个输入框",
        listens.len() >= 2,
        format!("{} 个", listens.len())
    );
    let listen = listens.first().cloned().expect("监听");
    let target = listens.get(1).cloned().expect("目标");
    listen.set_text("9000");
    target.set_text("host:80");
    rdlg.response(gtk::ResponseType::Ok);
    pump(400);

    check!(
        "表里多了一行",
        row_count(&view) == 1,
        format!("{} 行", row_count(&view))
    );
    check!(
        "行内容是 本地/9000/host:80/终端",
        cell_text(&view, 0, 0).as_deref() == Some("本地")
            && cell_text(&view, 0, 1).as_deref() == Some("9000")
            && cell_text(&view, 0, 2).as_deref() == Some("host:80")
            && cell_text(&view, 0, 3).as_deref() == Some("终端"),
        format!(
            "{:?} {:?} {:?} {:?}",
            cell_text(&view, 0, 0),
            cell_text(&view, 0, 1),
            cell_text(&view, 0, 2),
            cell_text(&view, 0, 3)
        )
    );

    // ---------- 4. 保存 ⇒ 写进 connections.ini（存储格式不变）----------
    let general = nb.nth_page(Some(0)).expect("常规页");
    let mut name: Option<gtk::Entry> = None;
    let mut host: Option<gtk::Entry> = None;
    for l in all_of::<gtk::Label>(&general) {
        let row = l.parent();
        let Some(row) = row else { continue };
        let e = first_of::<gtk::Entry>(&row);
        match l.text().as_str() {
            "名称" => name = e,
            "主机" => host = e,
            _ => {}
        }
    }
    name.expect("名称").set_text("冒烟连接");
    host.expect("主机").set_text("10.0.0.1");
    // 阶段 73b：新建对话框的动作收敛成「取消」+「连接」，保存与否看
    // 「保存到连接列表」勾选（默认勾上 ⇒ 等同老对话框的「保存并连接」）。
    dlg.response(gtk::ResponseType::Ok); // 连接
    pump(500);

    let text = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "ini 里的 forwardings 是老格式（存储没变）",
        text.contains("forwardings=L:9000:host:80"),
        text
    );

    // ---------- 5. 原始规则框粘贴 ⇒ 以它为准 ----------
    // 重开编辑器，往原始规则框里粘一条规则
    let entries = connmgr::load_all();
    let e = entries
        .iter()
        .find(|e| e.name == "冒烟连接")
        .expect("刚存的连接要能读回来");
    let entry = puttyplus::connmgr::ConnEntry {
        name: e.name.clone(),
        host: e.host.clone(),
        user: e.user.clone(),
        password: None,
        port: e.port,
        custom: true,
        proto: e.proto.clone(),
        sercfg: e.sercfg.clone(),
        forwards: e.forwards.clone(),
        inproc_forwards: e.inproc_forwards.clone(),
        sess: e.sess.clone(),
        keyfile: e.keyfile.clone(),
        auth_method: e.auth_method,
    };
    connmgr::ui_edit_connection(None, entry, || {});
    pump(500);
    let dlg2 = find_dialog("编辑连接").expect("编辑连接对话框要开着");
    let nb2 = first_of::<gtk::Notebook>(&dlg2).expect("页签容器");
    let fp2 = nb2.nth_page(Some(2)).expect("转发页");
    // ⚠️ 折叠的 Expander 里**摸不到**子控件（GTK4 展开时才把它挂进去）
    // ⇒ 先展开再找
    if let Some(exp) = first_of::<gtk::Expander>(&fp2) {
        exp.set_expanded(true);
        pump(200);
    }
    let raws = all_of::<gtk::TextView>(&fp2);
    check!(
        "转发页里有原始规则框（终端/进程内各一个）",
        raws.len() == 2,
        format!("{} 个", raws.len())
    );
    // 往终端那个原始框里粘一条规则
    let raw = raws.first().cloned().expect("原始规则框");
    raw.buffer().set_text("L:1234:h:9");
    dlg2.response(gtk::ResponseType::Ok); // 保存
    pump(500);

    let text2 = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "原始规则框里的内容优先（粘贴导入的老路径还在）",
        text2.contains("forwardings=L:1234:h:9"),
        text2
    );

    let _ = std::fs::remove_file(&ini);

    if fails.is_empty() {
        println!("fwdpage smoke ok");
    } else {
        println!("fwdpage smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
