//! 侧栏右键菜单的**弹层位置**冒烟（用户 2026-11-18 报的「点了没反应」）。
//!
//! ## 症状
//!
//! 用户报：在侧栏里**点数据库连接那一行** ⇒ 右键菜单的任何项都不起作用。
//!
//! ## 根因
//!
//! 右键菜单的弹层是 `set_parent(&view)` 挂在**树**上的 ⇒ `set_pointing_to`
//! 的锚点矩形按**树的坐标**解释。以前代码却把锚点算成了**侧栏盒子**
//! （`sb.widget`）的坐标 ⇒ 平白差了「树顶 ↔ 侧栏顶」那一百来像素。
//!
//! 数据库连接都排在树的最底下 ⇒ 锚点被顶到**屏幕外** ⇒ GTK 把弹层
//! SHOW 之后**立刻关掉** ⇒ 用户看着就是「菜单根本不弹 / 点了没反应」。
//!
//! ## 这里验什么
//!
//! - 把**窗口撑到屏幕那么高**、树也撑长 ⇒ 数据库连接行落在**屏幕最底边**
//!   （真场景：它就是树的最后一行）；
//! - 在那一行上弹右键菜单；
//! - 断言弹层**可见**，而且**整个落在屏幕里**（用户看得见、点得到）。
//!
//! ⚠️ 断言的是**与实现无关的外部量**（弹层的可见性与落点），不是
//!    「我们自己调了几次 set_pointing_to」。
//!
//! **不碰用户的真实配置**：ini/凭据库都指到临时文件；`HOME` 也指到临时
//! 目录（不然会读用户真实的 `~/.ssh/config`，树里会混进用户的机器）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sidebar_rowmenu_pos_smoke

use gtk::prelude::*;

use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::connmgr::ConnEntry;
use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

fn profile(name: &str) -> DbProfile {
    DbProfile {
        name: name.to_string(),
        proto: DbProto::Postgres,
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        dbname: "demo".into(),
        remember: false,
    }
}

fn main() {
    // ---- 临时 home：别读用户真实的 ~/.ssh/config ----
    //
    // ⚠️ 必须在任何 glib 调用**之前**改：`glib::home_dir()` 第一次调用就缓存。
    let home = std::env::temp_dir().join(format!("pp-sbrowmenu-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).expect("建临时 home");
    // ⚠️ set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑，不存在并发改环境变量
    unsafe {
        std::env::set_var("HOME", &home);
    }

    gtk::init().expect("gtk init");

    // ---- 临时 ini：绝不碰用户真实配置 ----
    let tmp = |tag: &str| {
        std::env::temp_dir().join(format!("pp-sbrowmenu-{tag}-{}", std::process::id()))
    };
    let ini = tmp("db");
    let conn_ini = tmp("conn");
    let vault = tmp("vault");
    let recent = tmp("recent");
    let settings = tmp("settings");
    for p in [&ini, &conn_ini, &vault, &recent, &settings] {
        let _ = std::fs::remove_file(p);
    }
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &ini);
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings);
    }

    // ---- 自建连接：塞一堆，把树撑长 ----
    //
    // ⚠️ 树必须**比视口高** ⇒ 数据库连接行（最后一行）才真的落在**屏幕
    //    最底边**（真场景：用户的树里有一堆机器，数据库连接排在最后）。
    //    「最近」有上限（设置项，默认 10 条）⇒ 这里用**自建连接**撑长度。
    {
        let kf = glib::KeyFile::new();
        for i in 0..40 {
            let name = if i == 39 { "底边那台" } else { "机器" };
            let host = if i == 39 {
                "10.0.0.1".to_string()
            } else {
                format!("10.2.{}.{}", i / 256, i % 256)
            };
            let sec = format!("conn{i}");
            kf.set_string(&sec, "name", name);
            kf.set_string(&sec, "host", &host);
            kf.set_uint64(&sec, "port", 22);
            kf.set_string(&sec, "user", "root");
        }
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }
    let entry = ConnEntry {
        name: "底边那台".into(),
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
    assert!(dbstore::save(&key, &profile("ttt")), "存不进数据库连接");

    // ---- 「最近」塞一堆，把树撑长 ----
    //
    // ⚠️ 「最近」按 用户/主机/端口 **只留一条** ⇒ 主机必须各不相同。
    for i in 0..40 {
        puttyplus::recent::push(puttyplus::recent::RecentConn {
            label: format!("设备{i}"),
            host: format!("10.1.{}.{}", i / 256, i % 256),
            port: 22,
            user: "ops".into(),
            ref_name: None,
        });
    }

    // ---- 屏幕尺寸（窗口要跟屏幕一样高 ⇒ 树的最下面那行贴着屏幕底边）----
    let display = gtk::gdk::Display::default().expect("gdk display");
    // ⚠️ GTK 4.10 起没有 `primary_monitor()` 了 ⇒ 取 `monitors()` 的第一个。
    let monitors = display.monitors();
    let monitor = monitors
        .item(0)
        .and_downcast::<gtk::gdk::Monitor>()
        .expect("没有显示器");
    let screen = monitor.geometry();
    println!(
        "[0] 屏幕：{}x{}",
        screen.width(),
        screen.height()
    );

    // ---- 侧栏 + 窗口 ----
    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs);
    // ⚠️ `setup()` 才装**行右键菜单**（`install_row_menu`）⇒ 不调就没有。
    sb.setup();
    sb.reload();
    pump(50);

    let win = gtk::Window::new();
    win.set_title(Some("侧栏右键菜单位置"));
    // ⚠️ 窗口**跟屏幕一样高**：树的最下面那行（数据库连接）就贴在屏幕底边
    //    ⇒ 「菜单被顶到屏幕外」那个坑才复现（真场景就是这样）。
    win.set_default_size(420, screen.height());
    win.set_child(Some(&sb.widget));
    win.present();
    pump(120);

    // ---- ① 树上确实有那条数据库连接 ----
    let paths = sb.db_row_paths();
    println!("[1] 树上的数据库连接行：{}", paths.len());
    assert_eq!(paths.len(), 1, "树上没挂出数据库连接行");
    let db_path = paths[0].clone();

    // ---- ② 把树滚到底 ⇒ 数据库连接行（最后一行）落在屏幕最底边 ----
    {
        let adj = sb.tree_sw().vadjustment();
        adj.set_value(adj.upper() - adj.page_size());
        pump(60);
    }
    let area = sb.row_area(&db_path);
    println!(
        "[2] 数据库连接行：树坐标 y={} h={}（窗口高 {}）",
        area.y(),
        area.height(),
        win.height()
    );
    assert!(area.height() > 0, "这一行没有高度（没显示出来？）");

    // ---- ③ 在那一行上弹右键菜单 ----
    let cx = area.x() + 8;
    let cy = area.y() + area.height() / 2;
    sb.open_row_menu_at(cx, cy);
    pump(120);

    let pop = sb.row_menu_popover().expect("侧栏没装行右键菜单");
    println!(
        "[3] 弹层：visible={} size={}x{}",
        pop.is_visible(),
        pop.width(),
        pop.height()
    );

    // ---- ④ 断言：弹层**看得见** ----
    //
    // ⛔ 以前锚点算错坐标系 ⇒ 锚点落到屏幕外 ⇒ GTK 把弹层 SHOW 之后
    //    **立刻关掉** ⇒ `is_visible()` 为假（用户看着就是"菜单不弹"）。
    assert!(
        pop.is_visible(),
        "右键菜单弹层没显示出来（锚点算错坐标系 ⇒ 弹到屏幕外被 GTK 关掉了）"
    );

    // ---- ⑤ 断言：弹层**整个落在窗口里**（用户看得见、点得到）----
    //
    // ⚠️ 窗口**跟屏幕一样高** ⇒ 「落在窗口里」＝「落在屏幕里」。
    let origin = pop
        .compute_point(&win, &gtk::graphene::Point::new(0.0, 0.0))
        .expect("算不出弹层的位置");
    let (w, h) = (pop.width(), pop.height());
    let win_h = win.height() as f32;
    let win_w = win.width() as f32;
    println!(
        "[4] 弹层（相对窗口）：x={} y={} w={} h={}（窗口 {}x{}）",
        origin.x(),
        origin.y(),
        w,
        h,
        win_w,
        win_h
    );
    assert!(
        origin.y() >= 0.0 && origin.y() + h as f32 <= win_h,
        "右键菜单掉到窗口下边外了（y={} h={} 窗口高 {}）⇒ 用户点不到",
        origin.y(),
        h,
        win_h
    );
    assert!(
        origin.x() >= -4.0 && origin.x() + w as f32 <= win_w + 4.0,
        "右键菜单掉到窗口左边外了（x={} w={} 窗口宽 {}）",
        origin.x(),
        w,
        win_w
    );

    // ---- ⑥ 菜单项确实摆出来了（不是空弹层）----
    let labels: Vec<String> = pop
        .child()
        .and_downcast::<gtk::Box>()
        .map(|b| {
            let mut out = Vec::new();
            let mut c = b.first_child();
            while let Some(ch) = c {
                if let Some(btn) = ch.downcast_ref::<gtk::Button>() {
                    if let Some(l) = btn.child() {
                        let mut cur = l.first_child();
                        while let Some(cc) = cur {
                            if let Some(t) = cc.downcast_ref::<gtk::Label>() {
                                out.push(t.text().to_string());
                                break;
                            }
                            cur = cc.next_sibling();
                        }
                    }
                }
                c = ch.next_sibling();
            }
            out
        })
        .unwrap_or_default();
    println!("[5] 菜单项：{labels:?}");
    assert!(
        labels.iter().any(|l| l.contains("编辑这个数据库连接")),
        "菜单里没有「编辑这个数据库连接…」：{labels:?}"
    );

    for p in [&ini, &conn_ini, &vault, &recent, &settings] {
        let _ = std::fs::remove_file(p);
    }
    let _ = std::fs::remove_dir_all(&home);
    println!("sidebar_rowmenu_pos_smoke: OK");
}
