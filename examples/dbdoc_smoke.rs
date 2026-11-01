//! 阶段 72 冒烟：**表文档**（数据 / 结构 / 属性 / ER 合一）（审计 P1-7）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验五件事：
//! 1. 打开一张表 ⇒ 外层 notebook 里**一个**页（表文档），页内四个视图：
//!    数据 / 结构 / 属性 / ER（以前是"数据页 + 结构页 + 属性窗 + ER 页"
//!    四个平级入口，审计说"它们的边界对普通用户不清晰"）；
//! 2. **非数据视图是懒建的**——开一张表**不会**顺手查结构 / ER
//!    （ER 图要查整个模式，一开表就查是白花钱）；
//! 3. 对象树右键「表结构」⇒ **切到同一个文档**的「结构」视图，
//!    **不再另开一个平级页**（外层页数不变）；
//! 4. 对象树右键「对象属性…」/「ER 图…」同理；
//! 5. 再打开一次同一张表 ⇒ **还是同一个文档**（不重复开）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbdoc_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, dbui, SftpPanel};

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
    let app = gtk::Application::builder()
        .application_id("x.y.PpDbDoc")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini =
            std::env::temp_dir().join(format!("pp-dbdoc-smoke-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // 假装连上了（不真的连库）：开表要查"连上了没"
        dbui::fake_connected_now(&panel.st);

        // ---- 断言 1：打开一张表 ⇒ 一个表文档，四个视图 ----
        let before_pages = dbui::outer_page_count(&panel.st);
        assert!(
            dbui::open_table_now(&panel.st, "public", "demo_users").is_some(),
            "应当能打开 demo_users 的表文档"
        );
        pump(400);
        assert_eq!(
            dbui::doc_count(&panel.st),
            1,
            "打开一张表该只有**一个**表文档"
        );
        assert_eq!(
            dbui::outer_page_count(&panel.st),
            before_pages + 1,
            "表文档只占外层 notebook 的**一个**页"
        );
        let labels = dbui::doc_view_labels(&panel.st, 0);
        println!("表文档的视图：{labels:?}");
        assert_eq!(
            labels,
            vec!["数据", "结构", "属性", "ER"],
            "表文档里该有这四个视图"
        );
        assert_eq!(
            dbui::doc_current_view(&panel.st, 0),
            0,
            "刚打开该停在「数据」视图"
        );
        println!("表文档骨架 ✓");

        // ---- 断言 2：非数据视图是懒建的 ----
        assert_eq!(
            dbui::struct_tabs_info(&panel.st).len(),
            0,
            "刚开表不该顺手建出结构页签（懒建）"
        );
        assert_eq!(
            dbui::er_tab_count(&panel.st),
            0,
            "刚开表不该顺手建出 ER 图（ER 要查整个模式，不能一开表就查）"
        );
        println!("懒建 ✓");

        // ---- 断言 3/4：树右键的入口切到同一个文档的视图 ----
        let pages_before = dbui::outer_page_count(&panel.st);
        // 「表结构」：切到「结构」视图
        dbui::open_structure_now(&panel.st, "public", "demo_users");
        pump(600);
        assert_eq!(
            dbui::outer_page_count(&panel.st),
            pages_before,
            "「表结构」不该另开一个平级页（要切到同一个表文档里）"
        );
        assert_eq!(
            dbui::doc_current_view(&panel.st, 0),
            1,
            "「表结构」该切到表文档的「结构」视图"
        );
        assert_eq!(
            dbui::struct_tabs_info(&panel.st).len(),
            1,
            "切到「结构」视图时该把结构页签建出来"
        );
        // ⚠️ **结构视图是最"重"的视图**（200px 的字段表 + 80px 的 DDL 预览），
        // 它的最小高度能顶到 506 ⇒ 右侧竖直分栏（只分 320 给页签区）会**溢出**
        // ⇒ **页签栏被挤成 0 高**（用户看不到「数据/结构/属性/ER」，看着像
        // "页签整个不见了"）。这条断言就是钉死这个坑。
        assert!(
            !dbui::right_paned_overflows(&panel.st),
            "结构视图把右侧分栏撑爆了 ⇒ 页签栏会被挤没"
        );
        // 「对象属性…」：切到「属性」视图
        dbui::open_object_stats_now(&panel.st, "public", "demo_users");
        pump(600);
        assert_eq!(
            dbui::outer_page_count(&panel.st),
            pages_before,
            "「对象属性…」不该另开一个平级页"
        );
        assert_eq!(
            dbui::doc_current_view(&panel.st, 0),
            2,
            "「对象属性…」该切到表文档的「属性」视图"
        );
        assert!(
            !dbui::right_paned_overflows(&panel.st),
            "属性视图把右侧分栏撑爆了 ⇒ 页签栏会被挤没"
        );
        // 「ER 图…」：表级也切到同一个文档的「ER」视图
        dbui::doc_focus_now(&panel.st, 0, 3);
        pump(600);
        assert_eq!(
            dbui::doc_current_view(&panel.st, 0),
            3,
            "表级 ER 图该切到表文档的「ER」视图"
        );
        assert!(
            !dbui::right_paned_overflows(&panel.st),
            "ER 图把右侧分栏撑爆了 ⇒ 页签栏会被挤没"
        );
        println!("入口收敛 ✓");

        // ---- 断言 5：再打开一次同一张表 ⇒ 还是同一个文档 ----
        let again = dbui::open_table_now(&panel.st, "public", "demo_users")
            .expect("应当能再打开一次这张表（切回已有文档）");
        pump(400);
        assert_eq!(again, 0, "再打开同一张表该还是同一个「数据」视图");
        assert_eq!(
            dbui::doc_count(&panel.st),
            1,
            "同一张表不该开出第二个表文档"
        );
        assert_eq!(
            dbui::outer_page_count(&panel.st),
            pages_before,
            "同一张表不该占第二个外层页"
        );
        println!("同一张表一个文档 ✓");

        // ---- 截图 ----
        for (view, name) in [(0i32, "data"), (1i32, "struct")] {
            dbui::doc_focus_now(&panel.st, 0, view);
            pump(800);
            let path = format!("/tmp/dbdoc_{name}.png");
            let _ = std::fs::remove_file(&path);
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("-o")
                .arg(&path)
                .status();
            println!("截图 {path}：{st:?}");
        }

        println!("dbdoc_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
