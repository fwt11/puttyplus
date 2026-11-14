//! 阶段 89 冒烟：**用户拖出来的列宽要落盘**（`columns.ini` 的 `widths` 键）。
//!
//! **不连真库**——纯界面 + 文件验证。
//!
//! 验四件事：
//! 1. 列宽一改就会**落盘**（去抖 1.5 秒后）：`columns.ini` 里出现 `widths=`，
//!    且读回来的宽度 == 网格里记的宽度；
//! 2. 宽度是**按列名**存的（藏过列、换过列顺序都不会错位）；
//! 3. 「恢复默认宽度」之后**存的那份也被删掉**（不然下次打开又套上旧宽度，
//!    用户会以为「恢复没生效」）；
//! 4. **重开页签**（重新灌数据）宽度还在（这才是用户要的「重开不丢」）。
//!
//! ⚠️ **为什么不用 xdotool 真拖**：「真拖能改列宽」由阶段 85 的
//! `grid_colwidth_smoke` 负责（那是 GTK 自己拖列宽）；这里要验的是
//! **「宽度变了 ⇒ 落盘 ⇒ 读回来」**这条链 ⇒ 用 `set_user_width`
//! （它跟真拖走的是**同一个通知路径**：`notify_widths_changed` ⇒ hook）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example grid_colwidth_persist_smoke

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
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

/// 退出时把临时 ini 删掉（别往用户配置里塞东西）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpGridColWidthPersist")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // ⚠️ **临时 ini**：测试不许写用户真实配置
        // （`columns.ini` 存列显示设置与列宽；`databases.ini` 存连接）。
        let cols_ini = std::env::temp_dir().join(format!(
            "pp-colwidth-persist-smoke-{}.ini",
            std::process::id()
        ));
        let db_ini = std::env::temp_dir().join(format!(
            "pp-colwidth-persist-db-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&cols_ini);
        let _ = std::fs::remove_file(&db_ini);
        unsafe {
            std::env::set_var("PUTTYPLUS_COLS_INI", &cols_ini);
            std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
        }
        let _guard_cols = IniGuard(cols_ini.clone());
        let _guard_db = IniGuard(db_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        puttyplus::panel::dbui::mount_for_test(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 造一个**表浏览**结果页签（带 col_key ⇒ 列宽会落盘）----
        let tidx =
            puttyplus::panel::dbui::make_test_table_result_tab(&panel.st, "public", "demo_users")
                .expect("应当能离线造一个表浏览页签");
        pump(500);
        let grid = puttyplus::panel::dbui::test_result_tab_grid(&panel.st, tidx)
            .expect("应当能拿到结果网格");

        // ---- 断言 1：改了宽度 ⇒ 会落盘（去抖 1.5 秒后）----
        //
        // ⚠️ **要去抖**：网格那边每 100ms 比一次列宽 ⇒ 不去抖就是每 100ms
        // 写一次文件 ⇒ 拖一次能写几百次。
        grid.set_user_width(1, 300);
        pump(2500);
        let stored = puttyplus::panel::dbui::test_result_tab_stored_widths(&panel.st, tidx);
        println!("存着的宽度：{stored:?}");
        assert_eq!(
            stored.len(),
            1,
            "应当只存了用户拖过的那一列：{stored:?}"
        );
        // ⚠️ **按列名存**（下标会变，名字不会）
        assert_eq!(stored[0].0, "name", "宽度要按**列名**存：{stored:?}");
        assert_eq!(stored[0].1, 300, "存下来的宽度不对：{stored:?}");
        // 文件里确实有 `widths=`
        let raw = std::fs::read_to_string(&cols_ini).unwrap_or_default();
        println!("columns.ini：\n{raw}");
        assert!(raw.contains("widths="), "文件里没有 widths 键：\n{raw}");

        // ---- 断言 2：恢复默认宽度 ⇒ 存的那份也被删掉 ----
        //
        // ⚠️ **不然下次打开又套上旧宽度** ⇒ 用户看到「恢复了默认，下次又变回去」。
        assert!(grid.clear_user_width(1), "恢复默认宽度失败");
        pump(2500);
        let stored2 = puttyplus::panel::dbui::test_result_tab_stored_widths(&panel.st, tidx);
        assert!(
            stored2.is_empty(),
            "恢复默认之后存的那份没删掉：{stored2:?}"
        );

        // ---- 断言 3：再拖一次 ⇒ 重开页签（重灌数据）宽度还在 ----
        grid.set_user_width(1, 260);
        pump(2500);
        let stored3 = puttyplus::panel::dbui::test_result_tab_stored_widths(&panel.st, tidx);
        println!("第二次存着的宽度：{stored3:?}");
        assert_eq!(stored3.len(), 1, "第二次没存上：{stored3:?}");

        // 模拟「关掉页签又打开」：重新灌数据 ⇒ 真路径会套上存好的宽度
        assert!(
            puttyplus::panel::dbui::test_result_tab_reload(&panel.st, tidx),
            "重灌数据失败"
        );
        pump(300);
        let back = grid.user_width(1);
        println!("重开页签之后的用户宽度：{back:?}（存的是 260）");
        assert!(
            back.is_some(),
            "重开页签后宽度丢了（这正是要修的那个问题）"
        );
        assert_eq!(back.unwrap(), 260, "重开页签后的宽度跟存的不一致");

        // ---- 断言 4：只存拖过的那一列，且按列名 ----
        let stored4 = puttyplus::panel::dbui::test_result_tab_stored_widths(&panel.st, tidx);
        assert_eq!(stored4.len(), 1, "只该存拖过的那一列：{stored4:?}");
        assert_eq!(stored4[0].0, "name", "宽度应当按列名存：{stored4:?}");

        println!("grid_colwidth_persist_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
