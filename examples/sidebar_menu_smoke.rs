//! 侧栏右键菜单清单 + 凭据库状态标志（阶段 77a）的冒烟。
//!
//! 背景：阶段 77 决定**删掉连接管理器**（它跟侧栏是两套完整 CRUD，用户
//! 不知道谁是主入口），所有连接操作收敛到**侧栏**。删之前得先确认侧栏
//! 接得住：
//! - 菜单项清单（纯函数 `row_menu_items`）：自定义连接能编辑/删除/**导出
//!   选中项**；ssh config 条目只读（只能连）；**数据库连接行能编辑/删除**
//!   （阶段 85 的成果 —— 连接管理器删掉后这儿就是唯一的地方）；
//! - **凭据库状态标志**常驻在侧栏标题栏（原来挂在连接管理器顶栏，而连接
//!   管理器本身就是要删的那块）。
//!
//! ⚠️ 菜单是**每次右键重建**的 ⇒ 靠"控件在不在树里"测不到 ⇒ 清单是
//! 纯函数、UI 照它建、这里照它断言（只有一份事实来源）。
//!
//! **不碰用户的真实配置**：ini 都指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sidebar_menu_smoke

use gtk::prelude::*;

use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::connmgr::ConnEntry;
use puttyplus::sidebar::{row_menu_items, RowMenuCtx, RowMenuItem, Sidebar};
use puttyplus::tabs::Tabs;

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

/// 清单里所有动作项的文案（不含分隔线）。
fn labels(items: &[RowMenuItem]) -> Vec<&'static str> {
    items
        .iter()
        .filter_map(|i| match i {
            RowMenuItem::Act { label, .. } => Some(*label),
            RowMenuItem::Sep => None,
        })
        .collect()
}

fn ctx(kind: i32, custom: bool, has_db: bool) -> RowMenuCtx {
    RowMenuCtx {
        kind,
        custom,
        has_db,
        fwd: None,
        group: None,
    }
}

/// 组头的上下文（kind==0）。
fn group_ctx(name: &str) -> RowMenuCtx {
    RowMenuCtx {
        kind: 0,
        custom: false,
        has_db: false,
        fwd: None,
        group: Some(name.to_string()),
    }
}

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

/// 按 CSS 类数控件个数（数"同一件事在屏幕上出现了几处"用）。
fn count_class(root: &impl IsA<gtk::Widget>, cls: &str) -> usize {
    let mut n = 0usize;
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if w.css_classes().iter().any(|c| c == cls) {
            n += 1;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    n
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 临时 ini：绝不碰用户真实配置 ----
    let ini = std::env::temp_dir().join(format!("pp-sbmenu-db-{}.ini", std::process::id()));
    let conn_ini = std::env::temp_dir().join(format!("pp-sbmenu-conn-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&conn_ini);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑，不存在并发改环境变量
    // ⚠️ **凭据库也得隔离**：`glib::user_config_dir()` 在进程内**缓存**，
    // 换 HOME 不生效 ⇒ 不设这个的话，冒烟会读到用户**真实的**凭据库状态
    // （状态还随用户机器变，断言就不稳了）。
    let vault = std::env::temp_dir().join(format!("pp-sbmenu-vault-{}", std::process::id()));
    let recent = std::env::temp_dir().join(format!("pp-sbmenu-recent-{}", std::process::id()));
    let _ = std::fs::remove_file(&vault);
    let _ = std::fs::remove_file(&recent);
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &ini);
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        // ⚠️ 「最近连接」也要隔离：不隔离的话截图里会露出用户**真实的**
        //   连接历史（`glib::user_config_dir()` 进程内缓存，换 HOME 不生效）。
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
    }
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", "测试跳板机");
        kf.set_string("conn1", "host", "10.0.0.1");
        kf.set_uint64("conn1", "port", 22);
        kf.set_string("conn1", "user", "root");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }

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
    assert!(dbstore::save(&key, &profile("ttt")));

    // ---- 侧栏 ----
    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs);
    sb.reload();
    pump(50);

    // ---- ① 凭据库状态标志常驻在侧栏 ----
    let vb = sb.vault_button();
    let tip = vb.tooltip_text().unwrap_or_default();
    assert!(!tip.is_empty(), "凭据库标志没有 tooltip（用户不知道它是什么）");
    assert!(
        tip.contains("凭据库"),
        "凭据库标志的 tooltip 没说清是什么：{tip}"
    );
    assert!(
        !tip.contains("**"),
        "tooltip 里留了 Markdown 的 ** ：{tip}"
    );
    println!("[1] 侧栏标题栏有凭据库状态标志（tooltip：{tip}）");

    // ---- ①b 凭据库状态标志：屏幕上**只许一处**（阶段 92）----
    //
    // 用户 2026-11-01 报「界面上有两处『凭证库 已锁定』是否重复」——顶栏与
    // 侧栏标题栏各建了一个徽标 ⇒ 同一个状态显示两遍。顶栏那个已删，
    // 现在侧栏里只该有一个。
    {
        let n = count_class(&sb.widget, "pp-vault-badge");
        assert_eq!(n, 1, "侧栏里的凭据库标志应当只有一处，实得 {n} 处");
        println!("[1b] 侧栏里凭据库标志只有一处");
    }

    // ---- ② 自定义连接行：编辑 / 删除 / 导出选中项 ----
    let v = labels(&row_menu_items(&ctx(2, true, false)));
    println!("[2] 自定义连接的菜单：{v:?}");
    assert!(v.contains(&"编辑…"), "{v:?}");
    assert!(v.contains(&"删除"), "{v:?}");
    assert!(
        v.contains(&"导出为 PuTTY 会话…"),
        "连接管理器删掉后，导出选中项只剩这里：{v:?}"
    );
    assert!(v.contains(&"连接"), "{v:?}");

    // ---- ③ ssh config 条目：只读 ----
    let v = labels(&row_menu_items(&ctx(2, false, false)));
    println!("[3] ssh config 条目的菜单：{v:?}");
    assert!(!v.contains(&"编辑…"), "ssh config 条目不该能编辑：{v:?}");
    assert!(!v.contains(&"删除"), "ssh config 条目不该能删除：{v:?}");
    assert!(v.contains(&"连接"), "{v:?}");

    // ---- ④ 数据库连接行：阶段 85 的成果（连接管理器删掉后唯一的地方）----
    let v = labels(&row_menu_items(&ctx(3, false, true)));
    println!("[4] 数据库连接行的菜单：{v:?}");
    assert!(v.contains(&"编辑这个数据库连接…"), "{v:?}");
    assert!(v.contains(&"删除这个数据库连接"), "{v:?}");
    assert!(v.contains(&"打开数据库"), "{v:?}");

    // ---- ⑤ 树上确实挂着那条数据库连接（不是只在清单里写着）----
    let rows = sb.db_rows();
    println!("[5] 树上的数据库连接行：{rows:?}");
    assert_eq!(rows.len(), 1, "树上没挂出数据库连接：{rows:?}");
    assert!(rows[0].0.contains("ttt"), "树上没有「ttt」：{rows:?}");

    // ---- ⑥ 「最近」组头：能清空最近连接（堡垒机场景）----
    //
    // ⚠️ 堡垒机每次拉起都是一条一次性连接 ⇒ 「最近」列表会被冲垮
    // ⇒ 用户得能**一次性清掉**（一条条「从最近中移除」太累）。
    {
        puttyplus::recent::push(puttyplus::recent::RecentConn {
            label: "堡垒机拉起的设备".into(),
            host: "10.0.0.99".into(),
            port: 22,
            user: "ops".into(),
            ref_name: None,
        });
        sb.reload();
        pump(50);
        // 组头确实在树上（kind==0，名字是「最近」）
        let rows = sb.tree_rows();
        let has_group = rows
            .iter()
            .any(|(k, name, _, _)| *k == 0 && name == "最近");
        assert!(has_group, "树上没有「最近」组头：{rows:?}");
        // 组头的菜单里能清空
        let v = labels(&row_menu_items(&group_ctx("最近")));
        println!("[6] 「最近」组头的菜单：{v:?}");
        assert!(v.contains(&"清空最近连接"), "最近组头要能清空：{v:?}");
        assert!(!v.contains(&"连接"), "组头不是连接：{v:?}");
        // 别的组头没有清理动作
        let v2 = labels(&row_menu_items(&group_ctx("自定义")));
        assert!(v2.is_empty(), "别的组头不该有清理动作：{v2:?}");
        // 清空之后最近项真的没了
        puttyplus::recent::clear();
        assert!(!puttyplus::recent::has_any(), "清空后不该还有最近连接");
    }

    // ---- 截图：侧栏（含凭据库标志）----
    let win = gtk::Window::new();
    win.set_title(Some("侧栏 — 阶段 77a"));
    win.set_default_size(420, 520);
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.append(&sb.widget);
    let filler = gtk::Label::new(Some("（右侧是空的：侧栏只有这一列）"));
    filler.set_hexpand(true);
    filler.set_halign(gtk::Align::Start);
    outer.append(&filler);
    win.set_child(Some(&outer));
    win.present();
    pump(120);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_sidebar_menu.png")
        .status();
    println!("截图：/tmp/pp_sidebar_menu.png");

    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&conn_ini);
    let _ = std::fs::remove_file(&vault);
    let _ = std::fs::remove_file(&recent);
    println!("sidebar_menu_smoke: OK");
}
