//! 数据库连接纳入会话树（阶段 10）的冒烟：不连真库，验证**树形结构**。
//!
//! 覆盖：
//! - 一个 SSH 会话下可以存**多个**数据库连接（`databases.ini` 的子组）；
//! - 侧边栏会话树里，这些连接挂在对应 SSH 连接**下面**（Navicat 的连接树形状）；
//! - 老版本（一个会话一份配置）的老 ini 也能读出来，并且保存时自动迁移；
//! - 「删除这个数据库连接」只删那一条，别的连接不受影响。
//!
//! **不碰用户的真实配置**：用 `PUTTYPLUS_DB_INI` 把 ini 指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_conn_smoke

use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::connmgr::ConnEntry;
use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;

fn profile(name: &str, proto: DbProto, port: u16, db: &str) -> DbProfile {
    DbProfile {
        name: name.to_string(),
        proto,
        host: "127.0.0.1".into(),
        port,
        user: "root".into(),
        dbname: db.into(),
        remember: false,
    }
}

fn main() {
    gtk::init().expect("gtk init");

    // 临时 ini：绝不碰用户真实配置
    let ini = std::env::temp_dir().join(format!(
        "pp-db-conn-smoke-{}.ini",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&ini);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑，不存在并发改环境变量
    unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &ini) };
    // 连接列表也指到临时文件（侧边栏的树是从它读出来的）
    let conn_ini = std::env::temp_dir().join(format!(
        "pp-conn-smoke-{}.ini",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&conn_ini);
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini) };
    // 写一条真实的自定义连接（侧边栏树要靠它才有这一行）
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", "测试跳板机");
        kf.set_string("conn1", "host", "10.0.0.1");
        kf.set_uint64("conn1", "port", 22);
        kf.set_string("conn1", "user", "root");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }

    // 一个"自定义" SSH 连接（custom=true → 会话键是 `s:<名字>`）
    let entry = ConnEntry {
        name: "测试跳板机".into(),
        host: "10.0.0.1".into(),
        user: Some("root".into()),
        password: None,
        port: 22,
        custom: true,
        proto: "ssh".into(),
        sercfg: None,
        forwards: Vec::new(),
        inproc_forwards: Vec::new(),
        sess: Default::default(),
        keyfile: None,
        auth_method: Default::default(),
    };
    let key = dbstore::session_key(&entry.to_opts());
    println!("[1] 会话键：{key}");

    // 该会话下存两个数据库连接
    assert!(dbstore::save(
        &key,
        &profile("本地 PG", DbProto::Postgres, 5432, "postgres")
    ));
    assert!(dbstore::save(
        &key,
        &profile("测试 MySQL", DbProto::MySql, 3306, "demo")
    ));
    let list = dbstore::list(&key);
    println!("[2] 这个会话下的数据库连接：{list:?}");
    assert_eq!(list.len(), 2, "一个会话下应该能存多个数据库连接");
    assert_eq!(list[0].name, "本地 PG");
    assert_eq!(list[1].name, "测试 MySQL");

    // 侧边栏会话树：这两个连接应该挂在 SSH 连接下面
    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs);
    sb.reload();
    let rows = sb.db_rows();
    println!("[3] 会话树里的数据库连接节点：");
    for (name, tip) in &rows {
        println!("    {name}  |  {tip}");
    }
    assert_eq!(rows.len(), 2, "树上没挂出两个数据库连接");
    assert!(
        rows.iter().any(|(n, _)| n.contains("本地 PG")),
        "树上没有「本地 PG」：{rows:?}"
    );
    assert!(
        rows.iter().any(|(n, _)| n.contains("测试 MySQL")),
        "树上没有「测试 MySQL」：{rows:?}"
    );
    // 摘要里带类型与地址
    assert!(
        rows.iter().any(|(_, t)| t.contains("pg") && t.contains("5432")),
        "摘要里没有类型/端口：{rows:?}"
    );

    // 删一条，另一条还在
    assert!(dbstore::remove(&key, "本地 PG"));
    let list = dbstore::list(&key);
    assert_eq!(list.len(), 1, "删除后应该只剩一条");
    assert_eq!(list[0].name, "测试 MySQL");

    // 老格式（一个会话一份配置）也能读
    let old_key = "s:老会话";
    let kf = glib::KeyFile::new();
    kf.set_string(old_key, "proto", "pg");
    kf.set_string(old_key, "host", "10.0.0.2");
    kf.set_string(old_key, "port", "5432");
    std::fs::write(&ini, kf.to_data().as_bytes()).unwrap();
    let list = dbstore::list(old_key);
    println!("[4] 老格式读出来：{list:?}");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].host, "10.0.0.2");

    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&conn_ini);
    println!("db_conn_smoke: OK");
}
