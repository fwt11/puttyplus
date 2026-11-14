//! 阶段 50 冒烟：字段**行内操作** + **类型下拉** + **实时 DDL 预览**。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验五件事：
//! 1. 字段表有「操作」列（dbx 每行都有行内操作入口）；
//! 2. **类型下拉**按方言给类型清单（PG 是 integer / jsonb，MySQL 是 int / json），
//!    挑一个「应用」到选中行，类型真的改了；
//! 3. **实时 DDL 预览**：改完字段立刻看到会发出什么 `ALTER TABLE`，
//!    旁边还有「✓ 可执行 / ✗ 不能执行」的标记；
//! 4. **改坏了当场就能看出来**（预览标「✗ 不能执行」，不用等点保存被服务器骂）；
//! 5. 字段表的**右键菜单**关掉了（结果网格那一套「设为 NULL / 克隆为新行」
//!    出现在字段表上根本不对）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example struct_field_smoke

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

/// 退出时把临时 ini 删掉（别往用户配置里塞假连接）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpStructField")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        let db_ini = std::env::temp_dir().join(format!(
            "pp-struct-field-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        puttyplus::panel::dbui::mount_for_test(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        let sidx = puttyplus::panel::dbui::make_test_struct_tab(&panel.st, "public", "demo_users")
            .expect("应当能离线造一个结构页签");
        pump(300);

        // ---- 断言 1：有「操作」列 ----
        let cols = puttyplus::panel::dbui::struct_field_columns(&panel.st, sidx);
        println!("字段表列：{cols:?}");
        assert_eq!(
            cols,
            vec!["字段", "类型", "可空", "主键", "默认值", "操作"],
            "字段表应当有「操作」列"
        );

        // ---- 断言 2：类型下拉 ----
        let types = puttyplus::panel::dbui::struct_type_choices(&panel.st, sidx);
        println!("类型清单：{types:?}");
        assert!(types.contains(&"integer".to_string()), "类型清单里没有 integer");
        assert!(
            types.contains(&"jsonb".to_string()),
            "类型清单里没有 jsonb（PG 的类型）"
        );
        // 选中第 2 行（name），把类型改成 bigint
        assert!(
            puttyplus::panel::dbui::struct_select_field(&panel.st, sidx, 1),
            "选不中字段行"
        );
        assert!(
            puttyplus::panel::dbui::struct_apply_type(&panel.st, sidx, "bigint"),
            "类型下拉「应用」点不动"
        );
        pump(300);

        // ---- 断言 3：实时预览 ----
        let preview = puttyplus::panel::dbui::struct_preview_text(&panel.st, sidx);
        let mark = puttyplus::panel::dbui::struct_preview_mark(&panel.st, sidx);
        println!("预览：{preview}");
        println!("标记：{mark}");
        assert!(
            preview.contains("ALTER TABLE"),
            "预览里应当有 ALTER TABLE：{preview}"
        );
        assert!(
            preview.contains("bigint"),
            "预览里应当看得到刚改的类型：{preview}"
        );
        assert!(
            mark.contains("✓ 可执行") || mark.contains("⚠ 有风险"),
            "标记应当说明这条语句能不能执行：{mark}"
        );

        // ---- 断言 4：改坏了当场就能看出来 ----
        // 把第 1 行的名字改成一个非法标识符 → 预览应当说"不能执行"
        {
            let st = &panel.st;
            // 直接改网格里的名字（模拟用户双击编辑）
            puttyplus::panel::dbui::struct_set_field_cell(st, sidx, 0, 0, "1bad-name");
            pump(300);
            let mark = puttyplus::panel::dbui::struct_preview_mark(&panel.st, sidx);
            println!("改坏之后的标记：{mark}");
            assert!(
                mark.contains("不能执行"),
                "字段名非法时预览应当说「不能执行」：{mark}"
            );
        }

        // ---- 断言 5：字段表的右键菜单关掉了 ----
        // （结果网格那一套「设为 NULL / 克隆为新行」出现在字段表上根本不对）
        assert!(
            !puttyplus::panel::dbui::struct_row_menu_enabled(&panel.st, sidx),
            "字段表的右键菜单应当是关掉的"
        );

        /* ================================================================
         * 阶段 88：行内操作菜单里的「上移一行 / 下移一行」
         * ================================================================ */

        // ---- 断言 6：菜单里有上移 / 下移 ----
        let ops = puttyplus::panel::dbui::struct_field_ops_labels(&panel.st, sidx);
        println!("行内操作菜单：{ops:?}");
        assert!(
            ops.iter().any(|x| x.contains("上移")),
            "行内操作菜单里没有「上移一行」：{ops:?}"
        );
        assert!(
            ops.iter().any(|x| x.contains("下移")),
            "行内操作菜单里没有「下移一行」：{ops:?}"
        );

        // ---- 断言 7：真能挪行（换个干净的页签：前面那几条断言改过字段了）----
        let sidx2 = puttyplus::panel::dbui::make_test_struct_tab(&panel.st, "public", "demo_users")
            .expect("应当能再离线造一个结构页签");
        pump(300);
        let before = puttyplus::panel::dbui::struct_field_rows(&panel.st, sidx2);
        println!("挪之前：{before:?}");
        assert_eq!(before.len(), 2, "测试字段表应当有两行");
        assert_eq!(before[0][0], "id", "第 1 行应当是 id");
        assert_eq!(before[1][0], "name", "第 2 行应当是 name");

        assert!(
            puttyplus::panel::dbui::struct_move_field(&panel.st, sidx2, 1, true),
            "第 2 行上移失败"
        );
        pump(300);
        let after = puttyplus::panel::dbui::struct_field_rows(&panel.st, sidx2);
        println!("挪之后：{after:?}");
        assert_eq!(after[0][0], "name", "第 2 行没上移到最前面：{after:?}");
        assert_eq!(after[1][0], "id", "第 1 行没被挤到后面：{after:?}");

        // ---- 断言 8：预览说清楚「只有顺序变了」----
        // （已有列的物理顺序数据库改不了 ⇒ 不能让用户以为白改了）
        let preview = puttyplus::panel::dbui::struct_preview_text(&panel.st, sidx2);
        println!("挪之后的预览：{preview}");
        assert!(
            preview.contains("顺序"),
            "挪了行之后预览应当说清楚顺序的事：{preview}"
        );

        // ---- 断言 9：已经在最上面时再上移 ⇒ 挪不动（别装成成功了）----
        assert!(
            !puttyplus::panel::dbui::struct_move_field(&panel.st, sidx2, 0, true),
            "最上面那一行不该还能上移"
        );
        // 往下移到底也不该再能移
        // （此时顺序是 [name, id] ⇒ 下标 1 已在最下面 ⇒ 下移应当失败）
        assert!(
            !puttyplus::panel::dbui::struct_move_field(&panel.st, sidx2, 1, false),
            "移到底之后不该还能下移"
        );
        let still = puttyplus::panel::dbui::struct_field_rows(&panel.st, sidx2);
        assert_eq!(still[0][0], "name", "挪不动时顺序不该变：{still:?}");
        assert_eq!(still[1][0], "id", "挪不动时顺序不该变：{still:?}");

        // ---- 截图 ----
        {
            let _ = std::fs::remove_file("/tmp/struct_field.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/struct_field.png")
                .status();
            println!("截图 /tmp/struct_field.png：{st:?}");
        }

        println!("struct_field_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
