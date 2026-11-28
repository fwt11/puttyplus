//! 阶段 83 冒烟：**结果网格的空结果态 + SQL 编辑器占位提示**（审计 P2-10 / P2-11）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验六件事：
//! 1. SQL 编辑器**空着**时显示占位提示（GTK4 的 TextView 没有
//!    `set_placeholder_text`，是自己拿 Overlay 叠的）；
//! 2. 一打字，占位提示就藏起来；清空又回来；
//! 3. 新的查询页签（还没跑）显示「**还没有结果**」；
//! 4. 跑了但零行 ⇒「**没有结果行**」，且**没有**主 CTA（只读结果没动作可做，
//!    硬塞一个按钮是噪声）；
//! 5. 表浏览页签零行 ⇒「**这张表还没有数据**」，且**有**「插入一行」按钮
//!    （审计 §4.1：能用按钮就别用文字叙述点击路径）；
//! 6. 有行了 ⇒ 网格显示、空结果态收起。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_empty_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::ColType;
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

/// 退出时把临时 ini 删掉（别往用户配置里塞假连接）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpDbEmpty")
        .build();
    app.connect_activate(|_app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini =
            std::env::temp_dir().join(format!("pp-db-empty-smoke-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());
        // ⚠️ **设置也要隔离**（阶段 188）：点「展开 SQL 编辑器」会**写设置**
        //   （`[db] sql_collapsed`）⇒ 不隔离就会把用户的真实配置改掉。
        let set_ini = std::env::temp_dir()
            .join(format!("pp-db-empty-smoke-settings-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&set_ini);
        unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &set_ini) };
        let _guard2 = IniGuard(set_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        puttyplus::panel::dbui::mount_for_test(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        /* ---- 断言 1/2：SQL 编辑器的占位提示 ---- */
        //
        // ⚠️⚠️ **阶段 188：编辑区默认是「收着的」**（用户 2026-11-28：「sql 编辑的
        //   区域还是很大 ⇒ 应该尽量显示数据，用户不一定需要编辑 sql」）⇒
        //   编辑区整个藏起来 ⇒ **屏幕上没有占位提示**（GTK4 的 `is_visible()`
        //   连**祖先**一起算 ⇒ 父级一藏，子控件就不算"可见"了）。
        //   ⇒ 先断言这个新行为，再**展开**编辑区验占位提示本身。
        assert!(
            dbui::sql_editor_collapsed(&panel.st),
            "SQL 编辑区默认应当是收着的（用户要的是「尽量显示数据」）"
        );
        assert!(
            !dbui::sql_placeholder_visible(&panel.st),
            "编辑区收着时不该显示占位提示（屏幕上根本没有编辑区）"
        );
        dbui::set_sql_editor_collapsed(&panel.st, false);
        pump(300);
        assert!(
            !dbui::sql_editor_collapsed(&panel.st),
            "展开 SQL 编辑区失败"
        );
        assert!(
            dbui::sql_placeholder_visible(&panel.st),
            "编辑器空着时该显示占位提示（用户看着一片空白，不知道能干嘛）"
        );
        {
            let slot = panel.st.db_ui.borrow();
            let ui = slot.as_ref().expect("工作台在");
            ui.sql.buffer().set_text("SELECT 1");
        }
        pump(120);
        assert!(
            !dbui::sql_placeholder_visible(&panel.st),
            "有字了还显示占位提示 ⇒ 提示会盖在 SQL 上面"
        );
        {
            let slot = panel.st.db_ui.borrow();
            let ui = slot.as_ref().expect("工作台在");
            ui.sql.buffer().set_text("");
        }
        pump(120);
        assert!(
            dbui::sql_placeholder_visible(&panel.st),
            "清空了又该把占位提示放回来"
        );

        /* ---- 断言 3：还没跑查询 ⇒「还没有结果」 ---- */
        let qidx = dbui::new_result_tab_now(&panel.st, "SELECT * FROM demo_users")
            .expect("应当能造出结果页签");
        pump(200);
        let (shown, title, desc, cta) = dbui::result_grid_empty_state(&panel.st, qidx)
            .expect("应当能读到空结果态");
        println!("[还没跑] 显示={shown} 标题={title} 说明={desc} CTA={cta}");
        assert!(shown, "新页签该显示空结果态（不是一片空白）");
        assert_eq!(title, "还没有结果");
        assert!(desc.contains("执行"), "{desc}");
        assert!(!cta, "还没跑查询时没有可做的动作 ⇒ 不该有 CTA");

        /* ---- 断言 4：跑了但零行 ⇒「没有结果行」，没有 CTA ---- */
        assert!(
            dbui::set_result_grid_rows(
                &panel.st,
                qidx,
                vec!["id".into(), "name".into()],
                Vec::new(),
            ),
            "应当能灌假结果集"
        );
        pump(120);
        let (shown, title, desc, cta) = dbui::result_grid_empty_state(&panel.st, qidx)
            .expect("应当能读到空结果态");
        println!("[零行] 显示={shown} 标题={title} 说明={desc} CTA={cta}");
        assert!(shown, "零行时该显示空结果态");
        assert_eq!(title, "没有结果行");
        assert!(desc.contains("没有返回任何行"), "{desc}");
        assert!(!cta, "只读的查询结果没动作可做 ⇒ 不该有 CTA");

        /* ---- 断言 6：有行了 ⇒ 网格显示、空结果态收起 ---- */
        assert!(
            dbui::set_result_grid_rows(
                &panel.st,
                qidx,
                vec!["id".into(), "name".into()],
                vec![vec![Some("1".into()), Some("甲".into())]],
            ),
            "应当能灌假结果集"
        );
        pump(120);
        let (shown, title, _desc, _cta) = dbui::result_grid_empty_state(&panel.st, qidx)
            .expect("应当能读到空结果态");
        println!("[有行] 显示={shown} 标题={title}");
        assert!(!shown, "有行了还显示空结果态 ⇒ 用户以为没查到数据");

        /* ---- 断言 5：表浏览页签零行 ⇒ 有「插入一行」 ---- */
        let tidx = dbui::make_test_result_tab(
            &panel.st,
            "demo_users",
            &["id", "name"],
            &[ColType::Int, ColType::Text],
            Vec::new(),
        )
        .expect("应当能造出表浏览页签");
        pump(200);
        let (shown, title, desc, cta) = dbui::result_grid_empty_state(&panel.st, tidx)
            .expect("应当能读到空结果态");
        println!("[表浏览零行] 显示={shown} 标题={title} 说明={desc} CTA={cta}");
        assert!(shown, "表浏览零行时该显示空结果态");
        assert_eq!(title, "这张表还没有数据");
        assert!(
            cta,
            "表浏览页签能插行 ⇒ 该给一颗真按钮（审计 §4.1：别用文字叙述点击路径）"
        );

        println!("✔ 空结果态与占位提示都对");
    });
    app.run();
}
