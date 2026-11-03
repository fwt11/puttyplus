//! 数据库工作台**截图**工具：真 SSH + 真数据库，拍一张「连上了、对象树展开了、
//! 表里有数据」的工作台画面（README 用）。
//!
//! 用法：
//! ```text
//! cargo run --example db_shot -- <ssh_host> <ssh_port> <ssh_user> \
//!     <db_host> <db_port> <db_user> <dbname> <db_pass> <schema> <table> [out.png]
//! ```
//!
//! ⚠️ 不碰用户的真实配置：ini 都指到临时文件（`PUTTYPLUS_DB_INI` 等）。
//! ⚠️ SSH 用的私钥来自 `~/.ssh`（`HOME` 由调用方给）——本例**不改** `HOME`，
//!    因为真库那台机器的密钥就在真实家目录里。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{SftpPanel, dbui};
use puttyplus::theme::{self, Theme};

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

/// 找窗口里的**竖向**分栏（结果区 / SQL 编辑器之间那道）。
///
/// 找不到就返回 None（截图照旧，只是网格矮一点）。
fn find_vpaned(w: &gtk::Widget) -> Option<gtk::Paned> {
    if let Some(p) = w.downcast_ref::<gtk::Paned>() {
        if p.orientation() == gtk::Orientation::Vertical {
            return Some(p.clone());
        }
    }
    let mut c = w.first_child();
    while let Some(ch) = c {
        let next = ch.next_sibling();
        if let Some(f) = find_vpaned(&ch) {
            return Some(f);
        }
        c = next;
    }
    None
}

/// 临时 ini 用完就删（别留垃圾）。
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
    let schema = get(8, "public");
    let table = get(9, "");
    let out = get(10, "/tmp/pp_db_shot.png");

    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);

    // 隔离 ini：连上库会 persist（写 databases.ini），不隔离就会往用户
    // 配置里塞一条连接。
    let tag = std::process::id();
    let tmp = std::env::temp_dir();
    let mut guards: Vec<IniGuard> = Vec::new();
    for (var, file) in [
        ("PUTTYPLUS_DB_INI", format!("pp-db-shot-db-{tag}.ini")),
        ("PUTTYPLUS_SETTINGS_INI", format!("pp-db-shot-set-{tag}.ini")),
        ("PUTTYPLUS_HIST_INI", format!("pp-db-shot-hist-{tag}.ini")),
        ("PUTTYPLUS_COLS_INI", format!("pp-db-shot-cols-{tag}.ini")),
    ] {
        let p = tmp.join(&file);
        let _ = std::fs::remove_file(&p);
        // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑
        unsafe { std::env::set_var(var, &p) };
        guards.push(IniGuard(p));
    }

    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = ssh_host.clone();
    opts.port = ssh_port;
    opts.user = ssh_user.clone();
    let panel = SftpPanel::new(&opts);
    let st = std::rc::Rc::clone(&panel.st);

    // 等 SSH 连上（远端 + 网络，60 秒保险）
    let mut waited = 0;
    while !st.conn_ok.get() && waited < 60_000 {
        pump(200);
        waited += 200;
    }
    assert!(st.conn_ok.get(), "SSH 没连上 {ssh_user}@{ssh_host}:{ssh_port}");
    println!("[1] SSH 已连接");

    // 建工作台。
    //
    // ⚠️ **不走 `panel::on_open_db`**：它会先连「上次用的连接」，这时候
    // 一条都还没有 ⇒ 会弹一个没用的错误框（还挡着截图）。
    dbui::ensure(&st);
    pump(400);

    dbui::set_conn_now(&st, "pg", &db_host, &db_port, &db_user, &dbname, &db_pass);
    pump(200);
    dbui::connect_now(&st);

    let mut waited = 0;
    while waited < 60_000 {
        pump(300);
        waited += 300;
        if dbui::is_connected_now(&st) {
            break;
        }
    }
    assert!(
        dbui::is_connected_now(&st),
        "数据库没连上：{}",
        dbui::status_text(&st)
    );
    println!("[2] 数据库已连接：{}", dbui::status_text(&st));

    // 等对象树（默认模式自动展开 ⇒ 表直接列出来）
    pump(3000);
    println!("[3] 对象树：{}", dbui::tree_state(&st).join(" / "));

    // 打开一张表 ⇒ 数据页签里有行
    if !table.is_empty() {
        let idx = dbui::open_table_now(&st, &schema, &table);
        println!("[4] 打开 {schema}.{table}：{idx:?}");
        let mut waited = 0;
        while waited < 30_000 {
            pump(300);
            waited += 300;
            if !dbui::grid_rows(&st).is_empty() {
                break;
            }
        }
        let rows = dbui::grid_rows(&st);
        println!(
            "[5] 网格：{} 行",
            rows.last().map(|(_, r)| r.len()).unwrap_or(0)
        );
    }
    pump(1500);

    // 打磨画面：SQL 编辑器里放一条示例查询（不执行——主视图留给表格数据）
    {
        let slot = st.db_ui.borrow();
        if let Some(ui) = slot.as_ref() {
            ui.sql.buffer().set_text(
                "SELECT sku, name, category, price, stock\n  FROM products\n WHERE category = '外设'\n ORDER BY price DESC;\n",
            );
        }
    }
    pump(600);

    // 窗口摆到 README 要的尺寸
    {
        let slot = st.db_ui.borrow();
        if let Some(ui) = slot.as_ref() {
            ui.win.set_default_size(1440, 900);
            // ⚠️ 没有窗口管理器 ⇒ 窗口不会被人摆正，`set_default_size` 在
            // 已经 present 之后也不一定生效 ⇒ 用 size_request 顶住最小尺寸。
            ui.win.set_size_request(1440, 900);
            ui.win.present();
        }
    }
    pump(1500);

    // 挪分隔条：**上半区（结果网格）矮了一截，最后一行只露半行**。
    // 往下拉 SQL 编辑器，把多出来的空间让给数据。
    {
        let slot = st.db_ui.borrow();
        if let Some(ui) = slot.as_ref() {
            if let Some(p) = find_vpaned(ui.win.upcast_ref::<gtk::Widget>()) {
                p.set_position(505);
            }
        }
    }
    pump(1200);

    let _ = std::fs::remove_file(&out);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(&out)
        .status();
    println!("截图：{out}");
}
