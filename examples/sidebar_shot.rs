//! 侧栏版式截图（阶段 93）。
//!
//! 用途：侧栏的版式改动**光看代码看不出来**——名字和地址挤不挤、组头
//! 和条目分不分得清、行距够不够，只有渲染出来才知道。这个例子用**假
//! 数据**（不碰用户真实配置）摆一棵有代表性的树，截图到 `/tmp`。
//!
//! 运行：
//!   xvfb-run -a cargo run --example sidebar_shot -- [light|dark]
//!
//! 截图：`/tmp/pp_sidebar_<主题>.png`

use gtk::prelude::*;

use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::connmgr::ConnEntry;
use puttyplus::recent::RecentConn;
use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;
use puttyplus::theme;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

fn profile(name: &str, dbname: &str) -> DbProfile {
    DbProfile {
        name: name.to_string(),
        proto: DbProto::Postgres,
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        dbname: dbname.to_string(),
        remember: false,
    }
}

fn entry(name: &str, host: &str, user: &str, port: i32, custom: bool) -> ConnEntry {
    ConnEntry {
        name: name.into(),
        host: host.into(),
        user: Some(user.into()),
        password: None,
        port,
        custom,
        proto: "ssh".into(),
        sercfg: None,
        forwards: vec![puttyplus::backend::forward::FwdRule::new('L', "127.0.0.1:9000:10.0.0.1:80", false)],
        inproc_forwards: vec![puttyplus::backend::forward::FwdRule::new(
            'L',
            "127.0.0.1:5432:10.0.0.5:5432",
            false,
        )],
        sess: Default::default(),
        keyfile: None,
        auth_method: Default::default(),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "light".into());
    let dark = which == "dark";

    gtk::init().expect("gtk init");

    // ---- 临时 ini：绝不碰用户真实配置 ----
    let db_ini = std::env::temp_dir().join(format!("pp-sbshot-db-{}.ini", std::process::id()));
    let conn_ini = std::env::temp_dir().join(format!("pp-sbshot-conn-{}.ini", std::process::id()));
    let recent = std::env::temp_dir().join(format!("pp-sbshot-recent-{}.ini", std::process::id()));
    let vault = std::env::temp_dir().join(format!("pp-sbshot-vault-{}", std::process::id()));
    // ⚠️ **settings 也要隔离**：`theme::set_theme` 会写用户真实
    // settings.ini（改用户的界面主题！）⇒ 必须指到临时文件。
    let settings = std::env::temp_dir().join(format!("pp-sbshot-settings-{}.ini", std::process::id()));
    for f in [&db_ini, &conn_ini, &recent, &vault, &settings] {
        let _ = std::fs::remove_file(f);
    }
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings);
    }

    if dark {
        theme::set_theme(theme::Theme::Dark);
    } else {
        theme::set_theme(theme::Theme::Light);
    }

    // ---- 最近连接（假数据）----
    for (label, host, port, user) in [
        ("srv-01", "10.0.0.9", 22, "root"),
        ("ops@host.example.com", "ops@host.example.com", 2222, "ops"),
        ("测试跳板机", "127.0.0.1", 22, "root"),
        ("txy", "43.159.54.57", 22, "root"),
        ("www.baidu.com", "abc@www.baidu.com", 22, "abc"),
    ] {
        puttyplus::recent::push(RecentConn {
            label: label.into(),
            host: host.into(),
            port,
            user: user.into(),
            ref_name: None,
        });
    }

    // ---- 自定义连接（假数据）----
    let e = entry("srv-01", "10.0.0.9", "root", 22, true);
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", &e.name);
        kf.set_string("conn1", "host", &e.host);
        kf.set_uint64("conn1", "port", e.port as u64);
        kf.set_string("conn1", "user", "root");
        kf.set_string("conn1", "custom", "true");
        kf.set_string("conn1", "inprocess_forwardings", "L 127.0.0.1:5432:10.0.0.5:5432");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }
    let key = dbstore::session_key(&e.to_opts());
    assert!(dbstore::save(&key, &profile("postgres", "postgres")));
    assert!(dbstore::save(&key, &profile("ttt", "demo")));

    // ---- 侧栏 ----
    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs);
    sb.reload();
    pump(80);

    let win = gtk::Window::new();
    win.set_title(Some("侧栏版式 — 阶段 93"));
    win.set_default_size(360, 640);
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.append(&sb.widget);
    let filler = gtk::Label::new(Some("（右侧是空的：侧栏只有这一列）"));
    filler.set_hexpand(true);
    filler.set_halign(gtk::Align::Start);
    outer.append(&filler);
    win.set_child(Some(&outer));
    win.present();
    pump(150);

    let out = format!("/tmp/pp_sidebar_{which}.png");
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(&out)
        .status();
    println!("截图：{out}");

    for f in [&db_ini, &conn_ini, &recent, &vault, &settings] {
        let _ = std::fs::remove_file(f);
    }
    println!("sidebar_shot: OK");
}
