//! 阶段 72 冒烟：**过滤改名 + 列设置单一入口**（审计 P1-7 / P2-10）。
//!
//! **不连真库**——纯界面验证（造一个空结果页签专门验工具栏）。
//!
//! 验五件事：
//! 1. 快速筛选框的提示是「**筛选已加载行**」（本地筛行，不查库）；
//! 2. 工具栏那颗按钮的提示是「**数据库过滤条件**」（生成 WHERE 重新查库）——
//!    两个都叫"过滤"时用户点错了才发现不是一回事；
//! 3. 工具栏里**没有**「列显示设置」按钮了（列设置只留一个入口）；
//! 4. 列头的「⋮」菜单里有「列显示设置…」；
//! 5. 两个名字**不会**互相串（筛选框里没有「过滤条件」、过滤按钮里没有
//!    「筛选已加载行」）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbfilter_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::dbgrid::GridData;
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
        .application_id("x.y.PpDbFilter")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini =
            std::env::temp_dir().join(format!("pp-dbfilter-smoke-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 造一个空结果页签（不连库）----
        let idx = dbui::new_result_tab_now(&panel.st, "demo_users")
            .expect("应当能造出结果页签");
        pump(300);
        // 给网格灌两列（列头菜单要有列才有内容）
        let grid = dbui::result_grid_now(&panel.st, idx).expect("应当能拿到网格");
        grid.set_data(GridData {
            columns: vec!["id".into(), "email".into()],
            types: vec![ColType::Int, ColType::Text],
            rows: vec![vec![Some("1".into()), Some("a@b.c".into())]],
            pk: vec![0],
        });
        pump(200);

        // ---- 断言 1/2/5：两个过滤的名字分清了 ----
        let (ph, tip) = dbui::result_filter_ui_now(&panel.st, idx);
        println!("筛选框提示：{ph} / 过滤按钮提示：{tip}");
        assert_eq!(
            ph, "筛选已加载行",
            "快速筛选框的提示该是「筛选已加载行」（本地筛行）"
        );
        assert!(
            tip.contains("数据库过滤条件"),
            "过滤按钮的提示该说「数据库过滤条件」：{tip}"
        );
        assert!(
            !ph.contains("过滤条件"),
            "筛选框不该叫「过滤条件」（那是重新查库的那个）：{ph}"
        );
        assert!(
            !tip.contains("筛选已加载行"),
            "过滤按钮不该叫「筛选已加载行」（那是本地筛行的）：{tip}"
        );
        println!("过滤改名 ✓");

        // ---- 断言 3：工具栏里没有「列显示设置」按钮了 ----
        let tips = dbui::result_toolbar_tips(&panel.st, idx);
        println!("工具栏按钮提示：{tips:?}");
        assert!(
            !tips.iter().any(|t| t.contains("列显示设置")),
            "工具栏里还有「列显示设置」入口（列设置要只留一个）：{tips:?}"
        );
        println!("列设置单一入口 ✓");

        // ---- 断言 4：列头菜单里有「列显示设置…」 ----
        let items = grid.col_menu_titles_for(0);
        println!("列头菜单：{items:?}");
        assert!(
            items.iter().any(|t| t.contains("列显示设置")),
            "列头菜单里该有「列显示设置…」：{items:?}"
        );
        assert!(
            items.iter().any(|t| t.contains("隐藏这一列")),
            "列头菜单里该保留「隐藏这一列」：{items:?}"
        );
        println!("列头菜单 ✓");

        // ---- 截图：结果页签工具栏 ----
        {
            let _ = std::fs::remove_file("/tmp/dbfilter_toolbar.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("-o")
                .arg("/tmp/dbfilter_toolbar.png")
                .status();
            println!("截图 /tmp/dbfilter_toolbar.png：{st:?}");
        }

        println!("dbfilter_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
