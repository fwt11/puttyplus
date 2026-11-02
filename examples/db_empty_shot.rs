//! 数据库工作台**空状态**的截图工具（阶段 96；阶段 97 加了"连上了"那张）。
//!
//! 复刻用户报的那个场景：一个会话下有**两条数据库连接**、浅色主题、
//! 工作台没连上库 ⇒ 拍下那张「开始用数据库」的卡片，供目视核对版式。
//!
//! ⚠️ 阶段 97：空态重做成**品牌页**（图标 + 产品名 + 三个看得见效果的动作），
//! 而用户截图里那一版是**已经连上**的样子 ⇒ 这里再拍一张"连上了"的。
//! 那张用 `empty_state_set_connected` **只切界面状态**（不真的连库）。
//!
//! ⚠️ 不碰用户的真实配置：ini 都指到临时文件（`PUTTYPLUS_DB_INI` 等）。
//!
//! ```text
//! xvfb-run -a cargo run --example db_empty_shot
//! xvfb-run -a cargo run --example db_empty_shot -- --dark
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::panel::{self, SftpPanel};
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

fn main() {
    let dark = std::env::args().any(|a| a == "--dark");
    let tag = std::process::id();
    let home = std::env::temp_dir().join(format!("pp-dbempty-home-{tag}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let db_ini = std::env::temp_dir().join(format!("pp-dbempty-db-{tag}.ini"));
    let _ = std::fs::remove_file(&db_ini);
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
    }

    gtk::init().expect("gtk init");
    theme::apply(if dark { Theme::Dark } else { Theme::Light });

    // 一个会话 + 两条数据库连接（跟用户截图里的情形一样）
    let mut o = ConnOptions::default();
    o.host = "10.0.0.9".into();
    o.port = 22;
    o.user = "ops".into();
    // ⚠️ 会话键要用 **dbstore 那个**（`user@host:port`），
    // 不是 `ConnOptions::session_key()`（那个是 `host:port`，少个用户）——
    // 用错了连接就存到别的组里去，列表自然是空的。
    let key = dbstore::session_key(&o);
    for (name, proto, host, port, db) in [
        ("postgres", DbProto::Postgres, "127.0.0.1", 5432u16, "postgres"),
        ("ttt", DbProto::Postgres, "127.0.0.1", 5432, "postgres"),
    ] {
        dbstore::save(
            &key,
            &DbProfile {
                name: name.to_string(),
                proto,
                host: host.to_string(),
                port,
                user: "postgres".into(),
                dbname: db.into(),
                remember: false,
            },
        );
    }
    println!("会话键 {key}，连接数 {}", dbstore::list(&key).len());

    let panel = SftpPanel::new(&o);
    panel::on_open_db(&panel.st);
    puttyplus::panel::dbui::mount_for_test(&panel.st);
    pump(700);

    let out = if dark {
        "/tmp/pp_db_empty_dark.png"
    } else {
        "/tmp/pp_db_empty_light.png"
    };
    let _ = std::fs::remove_file(out);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(out)
        .status();
    println!("截图：{out}");

    // ---- 再拍一张"已经连上"的（阶段 97）----
    //
    // 用户报的那张截图就是"已连接 postgres"的状态 ⇒ 空态的**主要**长相
    // 其实是这一版（第一版只是没连上时的样子）。
    // ⚠️ 只切界面状态，不真的连库（树是空的，版面是真的）。
    let ok = puttyplus::panel::dbui::empty_state_set_connected(&panel.st, "postgres");
    println!("切到已连接状态：{ok}");
    pump(600);
    let out2 = if dark {
        "/tmp/pp_db_empty_dark_conn.png"
    } else {
        "/tmp/pp_db_empty_light_conn.png"
    };
    let _ = std::fs::remove_file(out2);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(out2)
        .status();
    println!("截图：{out2}");

    let _ = std::fs::remove_file(&db_ini);
}
