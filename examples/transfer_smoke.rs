//! 阶段 72 冒烟：**「表数据导入 / 导出」入口**（审计 P1-7 / P1-8）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验六件事：
//! 1. 工作台状态行上的入口叫「表数据导入/导出…」（**不再是「数据传输…」**——
//!    那个词在 SFTP 那边指文件，DB 这边指表数据，两回事共用一个词会分不清）；
//! 2. 入口是**菜单**，里面就两项：导入数据（CSV）…、导出表…
//!    （**没有「方向」下拉**——那是审计说的"多余的中间步骤"）；
//! 3. 导出向导里「格式」下拉是 CSV / SQL / JSON 三项（**id 认的是 id**）；
//! 4. 导出向导的「对象」下拉列的是**对象树上已经加载出来的表**（跟着树走）；
//! 5. 导入向导里有**目标表**下拉 + **文件**输入框（一个对话框搞定，
//!    不再是"先选方向 → 再选文件"两层）；
//! 6. 对象树右键菜单里「导入数据（CSV）…」/「导出表…」都还在。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example transfer_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::db::DbTable;
use puttyplus::panel::dbtree::DbTree;
use puttyplus::panel::worker::{DbMetaPayload, DbMetaReq};
use puttyplus::panel::{self, SftpPanel, dbtransfer};

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

/// 往树上灌一份假数据（一个模式 + 一张表 + 一个视图）。
fn fill_tree(tree: &DbTree) {
    tree.fill(
        &DbMetaReq::Schemas,
        &DbMetaPayload::Schemas(vec!["public".to_string()]),
    );
    tree.fill(
        &DbMetaReq::Tables {
            schema: "public".to_string(),
        },
        &DbMetaPayload::Tables(vec![
            DbTable {
                schema: "public".to_string(),
                name: "demo_users".to_string(),
                is_view: false,
            },
            DbTable {
                schema: "public".to_string(),
                name: "v_users".to_string(),
                is_view: true,
            },
        ]),
    );
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpTransfer")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini =
            std::env::temp_dir().join(format!("pp-transfer-smoke-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 灌假数据到对象树 ----
        let tree = puttyplus::panel::dbui::tree(&panel.st).expect("应当能拿到对象树");
        tree.reset("pg 127.0.0.1:5432/postgres", "PostgreSQL");
        pump(200);
        fill_tree(&tree);
        pump(200);

        // ---- 断言 1：入口的名字 ----
        let btn_label = puttyplus::panel::dbui::transfer_button_label(&panel.st);
        println!("入口按钮：{btn_label}");
        assert_eq!(
            btn_label, "表数据导入/导出…",
            "入口该叫「表数据导入/导出…」（不该再叫「数据传输…」）"
        );
        assert!(
            !btn_label.contains("数据传输"),
            "「数据传输」这个词该退休了（跟 SFTP 的文件传输打架）：{btn_label}"
        );

        // ---- 断言 2：菜单里就两项，没有「方向」 ----
        let menu_items = puttyplus::panel::dbui::transfer_menu_labels(&panel.st);
        println!("入口菜单：{menu_items:?}");
        assert_eq!(
            menu_items.len(),
            2,
            "入口菜单该就两项（导入 / 导出）：{menu_items:?}"
        );
        assert!(
            menu_items.iter().any(|t| t.contains("导入数据（CSV）")),
            "菜单里没有「导入数据（CSV）…」：{menu_items:?}"
        );
        assert!(
            menu_items.iter().any(|t| t.contains("导出表")),
            "菜单里没有「导出表…」：{menu_items:?}"
        );
        assert!(
            !menu_items.iter().any(|t| t.contains("方向")),
            "菜单里不该再有「方向」（那是多余的中间步骤）：{menu_items:?}"
        );

        // ---- 断言 3/4：导出向导 ----
        puttyplus::panel::dbui::open_transfer_now(&panel.st);
        pump(500);
        assert!(
            !dbtransfer::last_is_import(),
            "open_transfer_now 打开的该是**导出**向导"
        );
        let hint_export = dbtransfer::last_hint();
        assert!(
            hint_export.contains("整张表"),
            "导出的提示没说要导整张表：{hint_export}"
        );

        let fmts = dbtransfer::last_formats();
        println!("格式下拉：{fmts:?}");
        assert_eq!(fmts.len(), 3, "格式下拉该有 3 项：{fmts:?}");
        assert_eq!(fmts[0].0, "csv");
        assert_eq!(fmts[1].0, "sql");
        assert_eq!(fmts[2].0, "json");

        let objs = dbtransfer::last_objects();
        println!("对象下拉：{objs:?}");
        assert!(
            objs.iter().any(|(_, t)| t.contains("demo_users")),
            "对象下拉里没有 demo_users：{objs:?}"
        );
        assert!(
            objs.iter().any(|(_, t)| t.contains("v_users")),
            "对象下拉里没有视图：{objs:?}"
        );
        println!("导出向导 ✓");
        dbtransfer::close_last();
        pump(300);

        // ---- 断言 5：导入向导（一个对话框：目标表 + 文件）----
        puttyplus::panel::dbui::open_import_now(&panel.st);
        pump(500);
        assert!(
            dbtransfer::last_is_import(),
            "open_import_now 打开的该是**导入**向导"
        );
        let hint_import = dbtransfer::last_hint();
        println!("导入向导提示：{hint_import}");
        assert!(
            hint_import.contains("字段映射"),
            "导入的提示该说下一步是字段映射：{hint_import}"
        );
        let objs = dbtransfer::last_objects();
        assert!(
            objs.iter().any(|(_, t)| t.contains("demo_users")),
            "导入向导的目标表下拉里没有 demo_users：{objs:?}"
        );
        assert!(
            dbtransfer::last_formats().is_empty(),
            "导入向导不该有「格式」下拉（CSV 是唯一格式）"
        );
        // 文件输入框：能写进去
        dbtransfer::last_set_path("/tmp/pp_import_demo.csv");
        pump(200);
        assert_eq!(
            dbtransfer::last_path(),
            "/tmp/pp_import_demo.csv",
            "导入向导的文件输入框写不进去"
        );
        println!("导入向导 ✓");
        dbtransfer::close_last();
        pump(300);

        // ---- 断言 6：对象树右键菜单 ----
        let menu_items = puttyplus::panel::dbui::tree_menu_labels(&panel.st);
        println!("对象树右键菜单：{menu_items:?}");
        assert!(
            menu_items.iter().any(|t| t.contains("导出表")),
            "对象树右键菜单里没有「导出表…」：{menu_items:?}"
        );
        assert!(
            menu_items.iter().any(|t| t.contains("导入数据（CSV）")),
            "对象树右键菜单里没有「导入数据（CSV）…」：{menu_items:?}"
        );
        println!("对象树右键菜单 ✓");

        // ---- 截图：向导长什么样 ----
        {
            puttyplus::panel::dbui::open_import_now(&panel.st);
            pump(700);
            println!(
                "导入向导尺寸：{:?}，位置：{:?}",
                dbtransfer::last_geometry(),
                dbtransfer::last_position()
            );
            pump(400);
            let _ = std::fs::remove_file("/tmp/transfer_wizard.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("-o")
                .arg("/tmp/transfer_wizard.png")
                .status();
            println!("截图 /tmp/transfer_wizard.png：{st:?}");
        }

        println!("transfer_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
