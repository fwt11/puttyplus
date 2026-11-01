//! 阶段 48 冒烟：可视化 **WHERE / ORDER BY 条件条**（对标 dbx 的条件条）。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验五件事：
//! 1. 表浏览页签上条件条**是亮的**（普通查询结果页签不该有）；
//! 2. 两个下拉都填上了列（条件列 + 排序列）；
//! 3. 点「+ 条件」→ 生成 WHERE（**按列类型转义**：整数列不加引号），
//!    且条件条上出现 chip；
//! 4. 挑排序列 → 生成 ORDER BY，点方向按钮能切 ASC/DESC；
//! 5. 「清除」把条件与排序都清干净。
//!
//! ⚠️ 走的是**真实的按钮回调**（`emit_clicked`），不是直接调内部函数——
//! 直接调的话，按钮灰了、回调断了都测不出来。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example cond_bar_smoke

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::{self, SftpPanel};

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

/// 退出时把临时 ini 删掉（别往用户配置里塞假连接）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpCondBar")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类（.pp-cond-chip）只有 theme::apply()
        // 之后才生效——不装主题的话 chip 是"没上妆"的默认样式。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini = std::env::temp_dir().join(format!(
            "pp-cond-bar-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        // ⚠️ **不给面板另开窗口**：工作台在"没有子标签容器"时会**自己开一个
        // 窗口**（`dbui::ensure` 的兜底路径），那才是要截图的东西；面板再
        // 开一个窗口只会盖在它前面（截出来一片黑，还以为功能没做出来）。
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 造一个表浏览页签（离线灌数据）----
        let tidx = puttyplus::panel::dbui::make_test_result_tab(
            &panel.st,
            "demo_users",
            &["id", "name", "email", "age"],
            &[ColType::Int, ColType::Text, ColType::Text, ColType::Int],
            vec![
                vec![
                    Some("1".into()),
                    Some("张三".into()),
                    Some("zhang@example.com".into()),
                    Some("31".into()),
                ],
                vec![
                    Some("2".into()),
                    Some("李四".into()),
                    Some("li@example.com".into()),
                    Some("25".into()),
                ],
            ],
        )
        .expect("应当能离线造一个结果页签");
        pump(200);


        // ---- 断言 1/2：条件条亮着、下拉有列 ----
        let (where_sql, order_by, chips, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert!(where_sql.is_empty(), "一开始不该有条件：{where_sql}");
        assert!(order_by.is_empty(), "一开始不该有排序：{order_by}");
        assert!(chips.is_empty(), "一开始不该有 chip：{chips:?}");
        assert!(order_col.is_empty(), "一开始不该有排序列：{order_col}");
        println!("条件条初始状态 ✓");

        // ---- 断言 3：加一条条件（整数列 = 1）----
        assert!(
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "id", "=", "1"),
            "条件条没能加条件"
        );
        pump(200);
        let (where_sql, _, chips, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("WHERE = {where_sql}");
        println!("chip  = {chips:?}");
        // ⚠️ **整数列的值不能加引号**：加了 PG 会报类型错（integer = "1"）
        assert_eq!(where_sql, "\"id\" = 1", "WHERE 生成不对：{where_sql}");
        assert_eq!(chips, vec!["id = 1"], "chip 不对：{chips:?}");
        println!("加条件 ✓");

        // ---- 断言 3b：文本列的值要加引号（类型不同，转义不同）----
        assert!(
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "name", "=", "张三"),
            "第二条条件加不进去"
        );
        pump(200);
        let (where_sql, _, chips, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("WHERE = {where_sql}");
        assert_eq!(
            where_sql, "\"id\" = 1 AND \"name\" = '张三'",
            "两条条件的 WHERE 不对：{where_sql}"
        );
        assert_eq!(chips, vec!["id = 1", "name = 张三"], "chip 不对：{chips:?}");
        println!("两条条件 ✓");

        // ---- 断言 3c：「或者」要真的生效 ----
        //
        // ⚠️ 这一条专门盯 `append(id, text)` 的顺序：写成
        // `append("并且", "AND")` 的话，界面上显示的是 "AND"，
        // 而 `active_id()` 回来的是"并且"——下面的 "OR" 判断永远不成立，
        // 「或者」这个选项就形同虚设（看着能选，选了没反应）。
        assert!(
            puttyplus::panel::dbui::cond_bar_set_join_now(&panel.st, tidx, true),
            "连接词挑不动"
        );
        assert!(
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "email", "=", "a@b.c"),
            "第三条条件加不进去"
        );
        pump(200);
        let (where_sql, _, chips, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("WHERE = {where_sql}");
        assert_eq!(
            where_sql, "\"id\" = 1 AND \"name\" = '张三' OR \"email\" = 'a@b.c'",
            "「或者」没生效：{where_sql}"
        );
        assert_eq!(
            chips,
            vec!["id = 1", "name = 张三", "email = a@b.c"],
            "chip 不对：{chips:?}"
        );
        // 复位成「并且」，后面的断言好读
        puttyplus::panel::dbui::cond_bar_set_join_now(&panel.st, tidx, false);
        println!("「或者」✓");

        // ---- 断言 4：排序 ----
        assert!(
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "name", true),
            "排序挑不动"
        );
        pump(200);
        let (where_sql, order_by, _, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("ORDER BY = {order_by}");
        assert_eq!(order_col, "name", "排序列不对：{order_col}");
        assert_eq!(order_by, "ORDER BY \"name\" ASC", "ORDER BY 不对：{order_by}");
        // ⚠️ **加了排序不能让条件丢掉**（两条是并存的）
        assert_eq!(
            where_sql, "\"id\" = 1 AND \"name\" = '张三' OR \"email\" = 'a@b.c'",
            "条件丢了：{where_sql}"
        );

        // 切方向 → DESC
        assert!(
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "name", false),
            "方向切不动"
        );
        pump(200);
        let (_, order_by, _, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert_eq!(order_by, "ORDER BY \"name\" DESC", "方向没切成 DESC：{order_by}");
        println!("排序 + 切方向 ✓");

        // ---- 断言 5：清除 ----
        assert!(
            puttyplus::panel::dbui::cond_bar_clear_now(&panel.st, tidx),
            "清除点不动"
        );
        pump(200);
        let (where_sql, order_by, chips, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert!(where_sql.is_empty(), "清除后还有条件：{where_sql}");
        assert!(order_by.is_empty(), "清除后还有排序：{order_by}");
        assert!(chips.is_empty(), "清除后还有 chip：{chips:?}");
        assert!(order_col.is_empty(), "清除后还有排序列：{order_col}");
        println!("清除 ✓");

        // ---- 截图：把条件条的样子留下来 ----
        {
            // 再加两条，让截图里能看到 chip
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "id", ">=", "1");
            pump(150);
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "age", "<", "40");
            pump(150);
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "id", true);
            pump(300);
            let (where_sql, order_by, chips, _) =
                puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
            println!("截图前的状态：WHERE={where_sql} ORDER={order_by} chips={chips:?}");
            // ⚠️ **截图要等主循环转起来**：窗口刚 present 时还没画过，
            // 截出来是一片黑（看着像"界面没出来"）。
            pump(800);
            // ⚠️ **先把旧图删掉**：scrot 不覆盖同名文件，会自动加 _000/_001
            // 的后缀——下次再看 /tmp/cond_bar.png 看到的是**上一次**的图。
            let _ = std::fs::remove_file("/tmp/cond_bar.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/cond_bar.png")
                .status();
            println!("截图 /tmp/cond_bar.png：{st:?}");
        }

        println!("cond_bar_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
