//! 新建数据库连接：**当场输入的密码这次连接就要用**（阶段 34）。
//!
//! 用户 2026-10-27 报的现象：在「新建数据库连接」对话框里敲了密码，点
//! 「连接」——**工作台又弹一个框让再输一遍数据库密码**。看着像密码框
//! 根本没生效。
//!
//! 根因：对话框里敲的密码**只**在勾了「记住密码」时才进凭据库；没勾的
//! 时候它被**直接丢掉**了。工作台连库时按「内存口袋 → 凭据库」找密码，
//! 两处都找不到 → 只能问用户。
//!
//! 覆盖：
//! - 没勾「记住密码」，密码**照样**进了这次连接的口袋（`db_pass`）；
//! - 口袋里是**连接名 → 密码**（多条连接各管各的）；
//! - 密码框留空 → 口袋里**不塞**空密码（不然会拿空密码去连）。
//!
//! **不碰用户真实配置**：`PUTTYPLUS_DB_INI` / `PUTTYPLUS_CONN_INI` /
//! `PUTTYPLUS_RECENT_INI` 全指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbconn_pw_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbstore;
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

fn main() {
    let dir = std::env::temp_dir().join(format!("pp_dbconn_pw_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // SAFETY：example 是单线程的，设完立刻用。
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", dir.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", dir.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", dir.join("recent.ini"));
    }

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    // 一个**连不上**的会话：端口 1 会被立刻拒绝。
    // 这里的验证只需要"这个会话的面板状态存在"，不需要真的连上数据库。
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 1,
        user: "u".into(),
        ..Default::default()
    };
    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_default_size(1000, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(400);

    let st = tabs.current_panel_state().expect("应有当前面板");
    let key = dbstore::session_key(&st.opts);
    println!("[0] 会话键 {key}");

    // ---- 1) 填连接名 + 密码，点「连接」 ----
    let name = "pp_当场密码";
    let pw = "s3cret-9527";
    dbconn::ui_new(None, &key, |_k, _n| {});
    pump(600);

    let dlg = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast_ref::<gtk::Dialog>().map(|w| w.clone()))
        .find(|w| w.title().as_deref() == Some("新建数据库连接（当前会话）"))
        .expect("应弹出「新建数据库连接（当前会话）」对话框");
    let root = dlg.upcast_ref::<gtk::Widget>();

    let entries = find_widgets::<gtk::Entry>(root);
    let name_e = entries
        .iter()
        .find(|e| {
            e.placeholder_text()
                .as_deref()
                .unwrap_or("")
                .contains("连接名")
        })
        .expect("应有「连接名」输入框");
    let pw_e = entries
        .iter()
        .find(|e| {
            e.placeholder_text()
                .as_deref()
                .unwrap_or("")
                .contains("密码")
        })
        .expect("应有「密码」输入框");
    name_e.set_text(name);
    pw_e.set_text(pw);

    // ⚠️ **不勾**「记住密码」——正是用户报的那个场景
    dlg.response(gtk::ResponseType::Ok);
    pump(600);

    // ---- 2) 密码进了这次连接的口袋 ----
    let got = st.db_pass.borrow().get(name).cloned();
    let got = got.expect(
        "刚敲的密码没进内存口袋 → 工作台连库时会**再问一遍**（用户 2026-10-27 报的现象）",
    );
    assert_eq!(got.as_str(), pw, "口袋里的密码不对");
    println!("[1] 没勾「记住密码」，密码照样进了这次连接的口袋");

    // ---- 3) 连接定义照样落盘（工作台要靠它列连接） ----
    assert!(
        dbstore::get(&key, name).is_some(),
        "连接定义应该落进 databases.ini"
    );
    println!("[2] 连接定义照样落盘");

    // ---- 4) 密码框留空 → 不塞空密码 ----
    {
        let name2 = "pp_空密码";
        dbconn::ui_new(None, &key, |_k, _n| {});
        pump(600);
        let dlg2 = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast_ref::<gtk::Dialog>().map(|w| w.clone()))
            .find(|w| w.title().as_deref() == Some("新建数据库连接（当前会话）"))
            .expect("应再弹一次对话框");
        let root2 = dlg2.upcast_ref::<gtk::Widget>();
        let es = find_widgets::<gtk::Entry>(root2);
        let n2 = es
            .iter()
            .find(|e| {
                e.placeholder_text()
                    .as_deref()
                    .unwrap_or("")
                    .contains("连接名")
            })
            .expect("应有「连接名」输入框");
        n2.set_text(name2);
        // 密码框**留空**
        dlg2.response(gtk::ResponseType::Ok);
        pump(600);
        assert!(
            !st.db_pass.borrow().contains_key(name2),
            "没填密码就不该往口袋里塞空密码（否则会拿空密码去连）"
        );
        println!("[3] 密码框留空 → 口袋里不塞空密码");
    }

    let _ = std::fs::remove_dir_all(&dir);
    println!("dbconn pw smoke ok");
}
