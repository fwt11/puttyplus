//! 连接编辑器「认证」页的冒烟（阶段 70，审计 P1-5）。
//!
//! **问题**：一条连接能用什么凭据，以前**没有地方选**——要么密码、要么
//! 私钥/agent 全都试；用户想"只用密码，别把本地身份出示给堡垒机网关"
//! 做不到。私钥也没有绑定到连接上（只能每次在命令行里 `-i`）。
//!
//! **断言**（走真实对话框）：
//! 1. 连接编辑器有**三个页签**：常规 / 认证 / 会话；
//! 2. 「认证」页里认证方式、私钥、用户名、密码、「记住密码」都在；
//! 3. **凭据库未启用时「记住密码」不可勾**（勾了也只是明文写进
//!    `connections.ini`，用户以为"记住密码"= 加密保存就正好了）；
//! 4. 认证方式与私钥选好后保存 ⇒ 真的写进 `connections.ini`
//!    （新键 `auth=` / `keyfile=`）；
//! 5. 重新打开编辑器 ⇒ 认证页**预填的正是存下去的值**。
//!
//! ```text
//! xvfb-run -a cargo run --example authpage_smoke
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

/// 按 tooltip 认控件（认证方式与私钥都是下拉框，靠提示文字区分）。
fn combo_with_tooltip(root: &impl IsA<gtk::Widget>, tip: &str) -> Option<gtk::ComboBoxText> {
    all_of::<gtk::ComboBoxText>(root)
        .into_iter()
        .find(|c| c.tooltip_text().as_deref().unwrap_or("").contains(tip))
}

/// 表单里"标签文字 → 同一行的输入框"。
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

fn main() {
    // ⚠️ HOME 必须在任何 glib 调用之前改（凭据库/ini 都在 HOME 底下）
    let home = std::env::temp_dir().join(format!("pp_authpage_home_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    // 一把"私钥"：列表只认文件名与内容，冒烟不真的用它认证
    std::fs::write(home.join(".ssh/id_fake"), "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    }
    let ini = home.join("connections.ini");
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &ini) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

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

    // ---------- 2. 「认证」页的控件都在 ----------
    let ap = nb.nth_page(Some(1)).expect("认证页");
    let method = combo_with_tooltip(&ap, "允许用哪些凭据");
    let key = combo_with_tooltip(&ap, "私钥文件");
    let user = entry_by_label(&ap, "用户名");
    let pass = entry_by_label(&ap, "密码（可选）");
    let remember = first_of::<gtk::CheckButton>(&ap);
    check!(
        "认证页有认证方式/私钥/用户名/密码/记住密码",
        method.is_some()
            && key.is_some()
            && user.is_some()
            && pass.is_some()
            && remember.is_some(),
        format!(
            "method={} key={} user={} pass={} remember={}",
            method.is_some(),
            key.is_some(),
            user.is_some(),
            pass.is_some(),
            remember.is_some()
        )
    );
    let method = method.expect("认证方式");
    let key = key.expect("私钥文件");
    let user = user.expect("用户名");
    let _pass = pass.expect("密码");
    let remember = remember.expect("记住密码");

    // ---------- 3. 凭据库未启用 ⇒「记住密码」不可勾 ----------
    check!(
        "凭据库未启用时「记住密码」不可勾",
        !remember.is_sensitive(),
        "勾了也只是明文写进 connections.ini"
    );
    let status_text: String = all_of::<gtk::Label>(&ap)
        .iter()
        .map(|l| l.text().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    check!(
        "状态行说清了凭据库没启用",
        status_text.contains("凭据库") && status_text.contains("没启用"),
        status_text.clone()
    );

    // ---------- 4. 填值 → 保存 ----------
    method.set_active_id(Some("keyfile"));
    let key_path = home.join(".ssh/id_fake").to_string_lossy().into_owned();
    // 私钥列表里要能找到那把"私钥"（列表来自 ~/.ssh）
    let picked = key.set_active_id(Some(key_path.as_str()))
        && key.active_id().as_deref() == Some(key_path.as_str());
    check!(
        "私钥列表里有 ~/.ssh 里的私钥",
        picked,
        format!("{:?}", key.active_id())
    );
    user.set_text("ops");

    let general = nb.nth_page(Some(0)).expect("常规页");
    let name = entry_by_label(&general, "名称").expect("名称");
    let host = entry_by_label(&general, "主机").expect("主机");
    name.set_text("冒烟连接");
    host.set_text("10.0.0.1");

    // 阶段 73b：新建对话框的动作收敛成「取消」+「连接」，保存与否看
    // 「保存到连接列表」勾选（默认勾上 ⇒ 等同老对话框的「保存并连接」）。
    dlg.response(gtk::ResponseType::Ok); // 连接
    pump(500);

    // ---------- 5. 真的写进 connections.ini ----------
    let text = std::fs::read_to_string(&ini).unwrap_or_default();
    check!(
        "ini 里有 auth= 与 keyfile=",
        text.contains("auth=keyfile") && text.contains(&format!("keyfile={key_path}")),
        text
    );
    let entries = connmgr::load_all();
    let e = entries
        .iter()
        .find(|e| e.name == "冒烟连接")
        .expect("刚存的连接要能读回来");
    check!(
        "读回来的认证方式与私钥一致",
        e.auth_method == puttyplus::args::AuthMethod::KeyFile
            && e.keyfile.as_deref() == Some(key_path.as_str()),
        format!("{:?} {:?}", e.auth_method, e.keyfile)
    );
    let o = e.to_opts();
    check!(
        "认证方式与私钥进到会话参数里",
        o.auth_method == puttyplus::args::AuthMethod::KeyFile
            && o.keyfile.as_deref() == Some(key_path.as_str()),
        format!("{:?} {:?}", o.auth_method, o.keyfile)
    );

    // ---------- 6. 重新打开：认证页预填的是存下去的值 ----------
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
    let ap2 = nb2.nth_page(Some(1)).expect("认证页");
    let method2 = combo_with_tooltip(&ap2, "允许用哪些凭据").expect("认证方式");
    let key2 = combo_with_tooltip(&ap2, "私钥文件").expect("私钥文件");
    let user2 = entry_by_label(&ap2, "用户名").expect("用户名");
    check!(
        "重开后认证页预填的是存下去的值",
        method2.active_id().as_deref() == Some("keyfile")
            && key2.active_id().as_deref() == Some(key_path.as_str())
            && user2.text() == "ops",
        format!(
            "method={:?} key={:?} user={}",
            method2.active_id(),
            key2.active_id(),
            user2.text()
        )
    );

    // ---------- 7. 阶段 91：内容一次显示得下（不要滚动条）----------
    //
    // 用户 2026-11-01 报「这个对话框可以长一点，把内容显示全不要用滚动条」——
    // 「认证」页最后那段说明（凭据库状态）被切掉半截，还多出一截滚动条。
    //
    // ⚠️ **别看滚动条控件的 visible**：GTK4 里那根条子一直"存在"，只是没被
    // 画出来 ⇒ 真正的判据是**调整量**：内容高度（upper）不超过视口
    // （page_size）就说明不用滚。
    {
        let sw = first_of::<gtk::ScrolledWindow>(&dlg2).expect("对话框里要有滚动容器");
        let adj = sw.vadjustment();
        let (upper, page) = (adj.upper(), adj.page_size());
        check!(
            "内容一次显示得下（不用滚动条）",
            upper <= page + 0.5,
            format!("内容 {upper}px > 视口 {page}px ⇒ 会出滚动条")
        );
    }

    let _ = std::fs::remove_dir_all(&home);

    if fails.is_empty() {
        println!("authpage smoke ok");
    } else {
        println!("authpage smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
