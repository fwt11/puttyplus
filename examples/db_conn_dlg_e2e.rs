//! 「新建数据库连接」对话框：**连不上就留在原地**（不打开工作台）。
//!
//! 背景（用户 2026-11-02 报）：以前点「连接」就直接开工作台，连不上时
//! 工作台里再弹一个「请输入密码」的框 —— 那个框**很容易被错过**，错过之后
//! 用户就不知道该怎么办了（工作台开着、里面是空的、状态栏一句"连接失败"）。
//!
//! 现在：点「连接」**先测通**，通了才开工作台；不通就**留在对话框里**、
//! 在**密码框**那儿提示。
//!
//! 原则（用户原话）：**操作失败要回到发起它的界面并提示**。
//!
//! 跑在 **aly 上的 PG 容器**（见 AGENTS.md「真库测试一律用 aly 上的 postgres」）：
//! ```bash
//! xvfb-run -a cargo run --example db_conn_dlg_e2e -- \
//!     182.92.244.132 22 root 127.0.0.1 5432 postgres postgres mysecretpassword pg
//! ```
//!
//! 形参：<ssh_host> <ssh_port> <ssh_user> <db_host> <db_port> <db_user> <dbname> <db_pass> [pg|mysql]
//!
//! ⚠️ **db_host/db_port 是跳板机视角**（数据库通道走 SSH 的 direct-tcpip
//! 通道 ⇒ `127.0.0.1` 指的是**跳板机**，不是开发机）。

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbstore;
use puttyplus::panel::{PanelState, SftpPanel, dbconn};

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

/// 收集控件树里的 Entry（**按出现顺序**）。
fn entries(w: &gtk::Widget) -> Vec<gtk::Entry> {
    let mut out = Vec::new();
    fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Entry>) {
        if let Ok(e) = w.clone().downcast::<gtk::Entry>() {
            out.push(e);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    walk(w, &mut out);
    out
}

/// 收集控件树里的 Label 文字（**按出现顺序**）。
///
/// ⚠️ **不进按钮子树**：对话框底部的「取消 / 连接」也是 Label（在按钮里），
///    它们排在内容区**后面** ⇒ 不跳过的话取到的"最后一个"是按钮文字。
fn labels(w: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(w: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
        // 按钮里的文字不算（那是动作，不是内容）
        if w.downcast_ref::<gtk::Button>().is_some() {
            return;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    walk(w, &mut out);
    out
}

/// 对话框里**最后一个** Label —— 那就是提示行（表单的标签都在它前面）。
fn hint_text(dlg: &gtk::Dialog) -> String {
    labels(dlg.upcast_ref::<gtk::Widget>())
        .last()
        .cloned()
        .unwrap_or_default()
}

fn wait_dialog(title: &str) -> Option<gtk::Dialog> {
    for _ in 0..50 {
        for w in gtk::Window::list_toplevels() {
            let Some(win) = w.downcast_ref::<gtk::Window>() else {
                continue;
            };
            if win.title().as_deref() != Some(title) {
                continue;
            }
            if let Ok(d) = win.clone().downcast::<gtk::Dialog>() {
                return Some(d);
            }
        }
        pump(100);
    }
    None
}

struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |i: usize, d: &str| -> String { args.get(i).cloned().unwrap_or_else(|| d.to_string()) };
    let ssh_host = get(0, "127.0.0.1");
    let ssh_port: i32 = get(1, "22").parse().unwrap_or(22);
    let ssh_user = get(2, "root");
    let db_host = get(3, "127.0.0.1");
    let db_port = get(4, "5432");
    let db_user = get(5, "postgres");
    let dbname = get(6, "postgres");
    let db_pass = get(7, "");
    let proto = get(8, "pg");

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ---- 隔离所有 ini：绝不碰用户的真实配置 ----
    let tag = format!("pp-dbdlg-e2e-{}", std::process::id());
    let db_ini = std::env::temp_dir().join(format!("{tag}-db.ini"));
    let set_ini = std::env::temp_dir().join(format!("{tag}-settings.ini"));
    let conn_ini = std::env::temp_dir().join(format!("{tag}-conn.ini"));
    let hist_ini = std::env::temp_dir().join(format!("{tag}-hist.ini"));
    for p in [&db_ini, &set_ini, &conn_ini, &hist_ini] {
        let _ = std::fs::remove_file(p);
    }
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &set_ini);
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_HIST_INI", &hist_ini);
    }
    let _g1 = IniGuard(db_ini.clone());
    let _g2 = IniGuard(set_ini.clone());
    let _g3 = IniGuard(conn_ini.clone());
    let _g4 = IniGuard(hist_ini.clone());

    // ---- 真 SSH 连上 aly ----
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = ssh_host.clone();
    opts.port = ssh_port;
    opts.user = ssh_user.clone();
    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);

    let win = gtk::Window::new();
    win.set_title(Some("db conn dlg e2e"));
    win.set_default_size(900, 700);
    win.present();
    pump(200);

    let mut waited = 0;
    while !st.conn_ok.get() && waited < 60_000 {
        pump(200);
        waited += 200;
    }
    assert!(st.conn_ok.get(), "SSH 没连上 {ssh_user}@{ssh_host}:{ssh_port}");
    println!("[1] SSH 已连接");

    // ---- 打开「新建数据库连接」对话框 ----
    let done = Rc::new(Cell::new(false));
    let done2 = Rc::clone(&done);
    // ⚠️ 会话键要用**面板里那份** opts（ssh 别名可能已被展开）
    let key = dbstore::session_key(&st.opts);
    dbconn::ui_new(Some(&win), &key, Some(Rc::clone(&st)), move |_k, _n| {
        done2.set(true);
    });
    pump(300);

    let dlg = wait_dialog("新建数据库连接（当前会话）").expect("对话框没开");
    let es = entries(dlg.upcast_ref::<gtk::Widget>());
    // 字段顺序：连接名 / 地址 / 端口 / 用户 / 数据库 / 密码
    assert!(es.len() >= 6, "对话框字段不对：{} 个输入框", es.len());
    es[1].set_text(&db_host);
    es[2].set_text(&db_port);
    es[3].set_text(&db_user);
    es[4].set_text(&dbname);
    es[5].set_text("definitely-not-the-password");

    // ---- 密码错的 ⇒ 应当留在原地 ----
    dlg.response(gtk::ResponseType::Ok);
    let mut waited = 0;
    let mut hint = String::new();
    while waited < 40_000 {
        pump(200);
        waited += 200;
        hint = hint_text(&dlg);
        if hint.starts_with('⚠') {
            break;
        }
    }
    println!("[2] 提示：{hint}");
    assert!(hint.starts_with('⚠'), "连不上却没有提示：{hint:?}");
    assert!(
        hint.contains("密码不对"),
        "没认出是密码错（用户要知道改哪儿）：{hint}"
    );
    assert!(!done.get(), "连不上就不该打开工作台（回调不该被叫）");
    assert!(dlg.is_visible(), "连不上就不该关掉对话框");
    assert!(
        es[5].css_classes().iter().any(|c| c == "error"),
        "密码框没标红"
    );
    println!("[3] 连不上 ⇒ 留在原地、提示写在密码框那儿、没开工作台");

    // ---- 改对密码再试 ⇒ 这次该通过 ----
    es[5].set_text(&db_pass);
    dlg.response(gtk::ResponseType::Ok);
    let mut waited = 0;
    while waited < 40_000 {
        pump(200);
        waited += 200;
        if done.get() {
            break;
        }
    }
    assert!(done.get(), "密码改对后应当连上并打开工作台");
    println!("[4] 密码改对 ⇒ 连上了，工作台打开了");

    // 截图：对话框（连不上时的样子）
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_db_conn_dlg.png")
        .status();

    println!("db_conn_dlg_e2e: OK (proto={proto})");
}
