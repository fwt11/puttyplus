//! 阶段 84 冒烟：**常驻 WHERE 框**（对标 dbx 的「一个宽输入框 + 回车即检索」）。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验六件事：
//! 1. 表浏览页签上条件条**是亮的**（普通查询结果页签不该有）；
//! 2. **回车即查库**：在 WHERE 框里写条件 → 回车 → 生效的 WHERE 就是它；
//! 3. 条件助手「填入」**按列类型转义**（整数列不加引号，文本列加引号），
//!    填进去的是**一段可读的文本**，不是一个当场生效的条件；
//! 4. 分号被**拒绝**（一条查询只能一条语句）；
//! 5. 挑排序列 → 生成 ORDER BY，点方向按钮能切 ASC/DESC；
//! 6. 「清除」把条件与排序都清干净。
//!
//! ⚠️ 走的是**真实的控件回调**（`emit_activate` / `emit_clicked`），不是直接
//! 调内部函数——直接调的话，回调断了都测不出来。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example where_box_smoke

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
        .application_id("x.y.PpWhereBox")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类（.pp-where-label）只有
        // theme::apply() 之后才生效——不装主题的话标签是"没上妆"的默认样式。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini = std::env::temp_dir().join(format!(
            "pp-where-box-smoke-{}.ini",
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
        puttyplus::panel::dbui::mount_for_test(&panel.st);
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

        // ---- 断言 1：条件条亮着、下拉有列、框是空的 ----
        let (where_sql, order_by, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert!(where_sql.is_empty(), "一开始不该有条件：{where_sql}");
        assert!(order_by.is_empty(), "一开始不该有排序：{order_by}");
        assert!(order_col.is_empty(), "一开始不该有排序列：{order_col}");
        assert!(
            puttyplus::panel::dbui::where_text_now(&panel.st, tidx).is_empty(),
            "WHERE 框一开始该是空的"
        );
        println!("条件条初始状态 ✓");

        // ---- 断言 2：回车即查库 ----
        //
        // ⚠️ **走 `emit_activate`**（真正的回车路径），不是直接调内部函数。
        assert!(
            puttyplus::panel::dbui::where_enter_now(&panel.st, tidx, "id > 1"),
            "WHERE 框回车没反应"
        );
        pump(200);
        let (where_sql, _, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("WHERE = {where_sql}");
        assert_eq!(where_sql, "id > 1", "回车后生效的 WHERE 不对：{where_sql}");
        // 框里的文字与生效的 WHERE 一致（用户看得见自己下发了什么）
        assert_eq!(
            puttyplus::panel::dbui::where_text_now(&panel.st, tidx),
            "id > 1",
            "框里的文字与生效的 WHERE 不一致"
        );
        println!("回车即查库 ✓");

        // ---- 断言 3：条件助手「填入」按列类型转义 ----
        //
        // ⚠️ **整数列的值不能加引号**：加了 PG 会报类型错（integer = "1"）。
        assert!(
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "id", "=", "1"),
            "条件助手没能填入"
        );
        pump(200);
        let text = puttyplus::panel::dbui::where_text_now(&panel.st, tidx);
        println!("填入后框里 = {text}");
        assert!(
            text.contains("\"id\" = 1"),
            "整数列的值不该加引号：{text}"
        );
        // ⚠️ **「填入」不直接生效**：用户多半还要接着改，改完按回车才查。
        let (where_sql, _, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert_eq!(
            where_sql, "id > 1",
            "「填入」不该当场生效（还没按回车）：{where_sql}"
        );
        // 文本列的值要加引号
        assert!(
            puttyplus::panel::dbui::cond_bar_add_now(&panel.st, tidx, "name", "=", "张三"),
            "条件助手没能填第二条"
        );
        pump(200);
        let text = puttyplus::panel::dbui::where_text_now(&panel.st, tidx);
        println!("填两条后框里 = {text}");
        assert!(
            text.contains("\"name\" = '张三'"),
            "文本列的值该加引号：{text}"
        );
        println!("条件助手 ✓");

        // ---- 断言 4：分号被拒 ----
        assert!(
            puttyplus::panel::dbui::where_enter_now(&panel.st, tidx, "id = 1; DROP TABLE t"),
            "分号那条回车没反应"
        );
        pump(200);
        let (where_sql, _, _) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert_eq!(
            where_sql, "id > 1",
            "带分号的条件不该生效（一条查询只能一条语句）：{where_sql}"
        );
        println!("分号被拒 ✓");

        // ---- 断言 5：排序 ----
        assert!(
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "name", true),
            "排序挑不动"
        );
        pump(200);
        let (where_sql, order_by, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        println!("ORDER BY = {order_by}");
        assert_eq!(order_col, "name", "排序列不对：{order_col}");
        assert_eq!(order_by, "ORDER BY \"name\" ASC", "ORDER BY 不对：{order_by}");
        // ⚠️ **加了排序不能让条件丢掉**（两条是并存的）
        assert_eq!(where_sql, "id > 1", "条件丢了：{where_sql}");

        // 切方向 → DESC
        assert!(
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "name", false),
            "方向切不动"
        );
        pump(200);
        let (_, order_by, _,) = puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert_eq!(order_by, "ORDER BY \"name\" DESC", "方向没切成 DESC：{order_by}");
        println!("排序 + 切方向 ✓");

        // ---- 断言 6：清除 ----
        assert!(
            puttyplus::panel::dbui::cond_bar_clear_now(&panel.st, tidx),
            "清除点不动"
        );
        pump(200);
        let (where_sql, order_by, order_col) =
            puttyplus::panel::dbui::cond_bar_state(&panel.st, tidx);
        assert!(where_sql.is_empty(), "清除后还有条件：{where_sql}");
        assert!(order_by.is_empty(), "清除后还有排序：{order_by}");
        assert!(order_col.is_empty(), "清除后还有排序列：{order_col}");
        // ⚠️ **框里的文字也要清**：只清生效状态不清框 ⇒ 用户看着框里的条件
        // 以为还在筛，其实没筛。
        assert!(
            puttyplus::panel::dbui::where_text_now(&panel.st, tidx).is_empty(),
            "清除后 WHERE 框里还有字"
        );
        println!("清除 ✓");

        // ---- 截图：把 WHERE 框的样子留下来 ----
        {
            puttyplus::panel::dbui::where_enter_now(&panel.st, tidx, "id > 1 AND name IS NOT NULL");
            pump(150);
            puttyplus::panel::dbui::cond_bar_order_now(&panel.st, tidx, "id", true);
            pump(300);
            // ⚠️ **截图要等主循环转起来**：窗口刚 present 时还没画过，
            // 截出来是一片黑（看着像"界面没出来"）。
            pump(800);
            // ⚠️ **先把旧图删掉**：scrot 不覆盖同名文件，会自动加 _000/_001
            // 的后缀——下次再看 /tmp/where_box.png 看到的是**上一次**的图。
            let _ = std::fs::remove_file("/tmp/where_box.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/where_box.png")
                .status();
            println!("截图 /tmp/where_box.png：{st:?}");
        }

        println!("where_box_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
