//! 阶段 84 冒烟：**底栏**（对标 dbx 的底部状态栏）。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验四件事：
//! 1. 底栏中间那一格**常驻显示程序发出去的 SQL**（dbx 的「当前 SQL」）；
//! 2. 底栏左边有**行数 / 耗时 / 待保存项数**；
//! 3. 「统计总行数」按钮**只在表浏览页签可点**（手写查询不知道是哪张表，
//!    数不了）；
//! 4. 点「统计总行数」会**发请求**（离线时状态变成「正在统计总行数…」，
//!    说明按钮接了线，不是摆设）。
//!
//! ⚠️ 走的是**真实的控件回调**（`emit_activate` / `emit_clicked`），不是直接
//! 调内部函数——直接调的话，回调断了都测不出来。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example foot_bar_smoke

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
        .application_id("x.y.PpFootBar")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类只有 theme::apply() 之后才生效。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini = std::env::temp_dir().join(format!(
            "pp-foot-bar-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        // ⚠️ **不给面板另开窗口**：工作台在"没有子标签容器"时会**自己开一个
        // 窗口**，那才是要截图的东西。
        panel::on_open_db(&panel.st);
        puttyplus::panel::dbui::mount_for_test(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 造一个表浏览页签（离线灌数据）----
        let tidx = puttyplus::panel::dbui::make_test_result_tab(
            &panel.st,
            "demo_users",
            &["id", "name"],
            &[ColType::Int, ColType::Text],
            vec![
                vec![Some("1".into()), Some("张三".into())],
                vec![Some("2".into()), Some("李四".into())],
            ],
        )
        .expect("应当能离线造一个结果页签");
        pump(200);

        // ---- 断言 1：底栏左边有行数 / 耗时 ----
        let (info, page) = puttyplus::panel::dbui::result_foot_now(&panel.st, tidx);
        println!("底栏左边 = {info}；页码 = {page}");
        assert!(info.contains("行"), "底栏左边该有行数：{info}");
        assert!(info.contains("ms"), "底栏左边该有耗时：{info}");
        assert!(page.contains("页"), "底栏该有页码：{page}");
        println!("底栏左边（行数 · 耗时 · 页码）✓");

        // ---- 断言 2：底栏中间常驻显示「程序发了什么 SQL」----
        //
        // ⚠️ **还没查的时候是空的**：没发过 SQL 就没有可显示的。
        assert!(
            puttyplus::panel::dbui::result_foot_sql_now(&panel.st, tidx).is_empty(),
            "还没查过库，底栏中间不该有 SQL"
        );
        // 写个 WHERE 条件回车 ⇒ 程序会拼一条 SELECT 发出去
        assert!(
            puttyplus::panel::dbui::where_enter_now(&panel.st, tidx, "id > 1"),
            "WHERE 框回车没反应"
        );
        pump(250);
        let sql = puttyplus::panel::dbui::result_foot_sql_now(&panel.st, tidx);
        println!("底栏中间 = {sql}");
        assert!(sql.contains("SELECT"), "底栏中间该显示当前 SQL：{sql}");
        assert!(sql.contains("WHERE"), "底栏中间的 SQL 该带 WHERE：{sql}");
        assert!(sql.contains("id > 1"), "底栏中间的 SQL 该带条件：{sql}");
        println!("底栏常驻当前 SQL ✓");

        // ---- 断言 3：「总行数」按钮：表浏览页签能点 ----
        let btn = puttyplus::panel::dbui::result_total_button_now(&panel.st, tidx)
            .expect("表浏览页签该有「统计总行数」按钮");
        assert!(
            btn.is_sensitive(),
            "表浏览页签该能统计总行数（知道表名）"
        );
        assert_eq!(btn.label().as_deref(), Some("总行数"), "按钮文案不对");
        println!("表浏览页签：总行数按钮可点 ✓");

        // ---- 断言 4：手写查询页签**不能**数总行数 ----
        //
        // ⚠️ **手写查询不知道是哪张表**（可能是 JOIN、可能是聚合）⇒
        // 「这张表多少行」这个问题对它没有意义 ⇒ 按钮必须灰掉，
        // 不能让用户点出一个假答案。
        let qidx = puttyplus::panel::dbui::new_result_tab_now(&panel.st, "SELECT 1")
            .expect("应当能造一个查询结果页签");
        pump(150);
        let qbtn = puttyplus::panel::dbui::result_total_button_now(&panel.st, qidx)
            .expect("查询页签也该有这颗按钮（只是灰着）");
        assert!(
            !qbtn.is_sensitive(),
            "手写查询页签不该能统计总行数（不知道表名）"
        );
        println!("手写查询页签：总行数按钮灰着 ✓");

        // ---- 断言 5：点「总行数」会发请求（离线也该有反应）----
        let before = puttyplus::panel::dbui::result_tab_status_now(&panel.st, tidx);
        btn.emit_clicked();
        pump(250);
        let after = puttyplus::panel::dbui::result_tab_status_now(&panel.st, tidx);
        println!("状态：{before} → {after}");
        assert_ne!(before, after, "点了「总行数」一点反应都没有");
        assert!(
            after.contains("统计"),
            "点「总行数」后状态该说在统计：{after}"
        );
        println!("总行数按钮接了线 ✓");

        println!("foot_bar_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
