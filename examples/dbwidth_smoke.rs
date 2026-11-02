//! 阶段 72 冒烟：**网格列宽分配 + 只读标志**（审计 P2-10）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验四件事：
//! 1. 普通查询结果的页签上有「**只读**」标志（工具栏上「编辑 / 插入行 /
//!    删除行」三颗按钮平时是灰的，页面上却没一处说明，用户不知道是坏了
//!    还是本来就这样）；
//! 2. 表浏览页签的标志是「**可编辑**」；
//! 3. 列宽**按内容 / 可用宽度分配**：右侧不留空白，列宽是按内容的比例分
//!    的（长内容列比短内容列宽），不是平分；
//! 4. 总宽**撑满**可用宽度（审计原话："邮箱/时间列截断而右侧还有空白"）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbwidth_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::dbgrid::{DbGrid, GridData};
use puttyplus::panel::{self, SftpPanel, dbui};

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
        .application_id("x.y.PpDbWidth")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini =
            std::env::temp_dir().join(format!("pp-dbwidth-smoke-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        puttyplus::panel::dbui::mount_for_test(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 断言 1：普通查询结果 ⇒ 「只读」 ----
        let qidx = dbui::new_result_tab_now(&panel.st, "SELECT * FROM demo_users")
            .expect("应当能造出结果页签");
        pump(300);
        let chip = dbui::result_mode_chip_now(&panel.st, qidx);
        println!("查询结果的标志：{chip}");
        assert_eq!(
            chip, "只读",
            "普通查询结果的页签该标「只读」（编辑按钮是灰的，得有说明）"
        );

        // ---- 断言 2：表浏览页签 ⇒ 「可编辑」 ----
        dbui::fake_connected_now(&panel.st);
        let tidx = dbui::open_table_now(&panel.st, "public", "demo_users")
            .expect("应当能打开表页签");
        pump(300);
        let chip = dbui::result_mode_chip_now(&panel.st, tidx);
        println!("表浏览页签的标志：{chip}");
        assert_eq!(
            chip, "可编辑",
            "表浏览页签该标「可编辑」（它能进编辑模式改数据）"
        );

        // ---- 断言 3/4：列宽按内容 / 可用宽度分配 ----
        //
        // 用一个**独立的**网格（跟 db_ui_smoke 一样）：列宽分配是 `DbGrid`
        // 自己的事，跟工作台没关系；网格已经在页签里的话没法再塞进别的窗口。
        {
            let grid = DbGrid::new();
            let win = gtk::ApplicationWindow::builder()
                .application(app)
                .title("阶段 72c 列宽冒烟")
                .default_width(900)
                .default_height(300)
                .build();
            let sw = gtk::ScrolledWindow::new();
            sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
            sw.set_vexpand(true);
            sw.set_child(Some(&grid.view));
            win.set_child(Some(&sw));
            win.present();
            pump(600);

            // 三列：id 很短、email 很长、created_at 中等
            grid.set_data(GridData {
                columns: vec!["id".into(), "email".into(), "created_at".into()],
                types: vec![ColType::Int, ColType::Text, ColType::Text],
                rows: vec![vec![
                    Some("1".into()),
                    Some("zhang.san@example.com".into()),
                    Some("2026-10-20 13:45:01".into()),
                ]],
                pk: vec![0],
            });
            pump(600);

            let planned = grid.planned_widths();
            let actual = grid.column_widths();
            let avail = grid.view.width();
            println!("可用宽度：{avail} / 计划列宽：{planned:?} / 实际列宽：{actual:?}");
            assert_eq!(
                planned.len(),
                3,
                "计划列宽的条数应当跟列数一样：{planned:?}"
            );
            // 撑满：各列加起来不应当比可用宽度少一大截（少 30px 以内算舍入）
            let sum: i32 = actual.iter().sum();
            assert!(
                sum >= avail - 30,
                "各列加起来没撑满可用宽度（{sum} < {avail}）：{actual:?}"
            );
            // 按内容比例：长内容列更宽（不是平分）
            assert!(
                actual[1] > actual[0] * 2,
                "email 列（长内容）应当明显比 id 列（短内容）宽：{actual:?}"
            );
            assert!(
                actual[0] > 0 && actual[2] > 0,
                "每列都得有宽度：{actual:?}"
            );
            println!("列宽分配 ✓");

            // ---- 截图 ----
            let _ = std::fs::remove_file("/tmp/dbwidth.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("-o")
                .arg("/tmp/dbwidth.png")
                .status();
            println!("截图 /tmp/dbwidth.png：{st:?}");
        }

        println!("dbwidth_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
