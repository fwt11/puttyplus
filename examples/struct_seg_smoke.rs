//! 阶段 49 冒烟：表设计器的**分段控件**（字段 | 索引 | 外键 | 触发器 | DDL）。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验四件事：
//! 1. 设计器里有 **5 段**（dbx 是 `字段 | 索引 | 外键 | 触发器` + 我们的 DDL）；
//! 2. 切段能真的切（`visible_child_name` 跟着变）；
//! 3. **触发器页有内容**（阶段 49 新增的那一段）；
//! 4. 切段**不影响别的段**（切回字段页还在）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example struct_seg_smoke

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
        .application_id("x.y.PpStructSeg")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：分段控件的自定义 CSS 类（.pp-segmented）
        // 只有 theme::apply() 之后才生效——不装的话就是一排默认按钮。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        let db_ini = std::env::temp_dir().join(format!(
            "pp-struct-seg-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        // ⚠️ **不给面板另开窗口**：工作台在"没有子标签容器"时会自己开一个
        // 窗口（`dbui::ensure` 的兜底路径），那才是要截图的东西。
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 离线造一个表结构页签 ----
        let sidx = puttyplus::panel::dbui::make_test_struct_tab(&panel.st, "public", "demo_users")
            .expect("应当能离线造一个结构页签");
        pump(300);

        // ---- 断言 1：分段数 ----
        let n = puttyplus::panel::dbui::struct_segment_count(&panel.st, sidx);
        println!("分段数：{n}");
        assert_eq!(
            n, 5,
            "设计器应当有 5 段（字段 / 索引 / 外键 / 触发器 / DDL）"
        );

        // 分段控件上那排按钮的文字（空 = 按钮根本没建出来）
        println!("switcher width = {}", puttyplus::panel::dbui::struct_segment_width(&panel.st, sidx));
        // ⚠️ **别让分段控件摊满整行**：GTK4 的 GtkStackSwitcher 内部按钮是
        // homogeneous 的（实测摊满 640px，像一排导航链接）。自己画的
        // ToggleButton 排出来的宽度应当按内容来。
        let w = puttyplus::panel::dbui::struct_segment_width(&panel.st, sidx);
        assert!(
            w > 0 && w < 400,
            "分段控件宽度不对（{w}px）：摊满整行就不是分段控件了"
        );
        let titles = puttyplus::panel::dbui::struct_segment_titles(&panel.st, sidx);
        println!("分段按钮：{titles:?}");
        assert_eq!(
            titles,
            vec!["字段", "索引", "外键", "触发器", "DDL"],
            "分段控件上的按钮不对"
        );

        // ---- 断言 2：切段 ----
        for (page, want) in [
            (0usize, "字段"),
            (1, "索引"),
            (2, "外键"),
            (3, "触发器"),
            (4, "DDL"),
        ] {
            puttyplus::panel::dbui::struct_show_page(&panel.st, sidx, page);
            pump(120);
            let cur = puttyplus::panel::dbui::struct_current_page(&panel.st, sidx);
            println!("切到第 {page} 段 → {cur}");
            assert_eq!(cur, want, "第 {page} 段切不过去（当前 {cur}）");
        }

        // ---- 断言 3：触发器页有内容 ----
        puttyplus::panel::dbui::struct_show_page(&panel.st, sidx, 3);
        pump(200);
        let rows = puttyplus::panel::dbui::struct_trigger_rows(&panel.st, sidx);
        println!("触发器：{rows:?}");
        assert_eq!(rows.len(), 2, "触发器表应当有两行：{rows:?}");
        assert_eq!(rows[0][0], "trg_audit", "触发器名字不对：{rows:?}");
        assert_eq!(rows[0][1], "BEFORE", "时机不对：{rows:?}");

        // ---- 断言 4：切段不影响别的段 ----
        puttyplus::panel::dbui::struct_show_page(&panel.st, sidx, 0);
        pump(120);
        assert_eq!(
            puttyplus::panel::dbui::struct_current_page(&panel.st, sidx),
            "字段",
            "切回字段页失败"
        );

        // ---- 断言 5：**点按钮**也能切（用户走的就是这条路）----
        //
        // ⚠️ 前面的断言走的是 `struct_show_page`（程序切段）——那只验了
        // "程序能切"，没验"用户点按钮能切"。两条路都得通。
        for (idx, want) in [(3usize, "触发器"), (1, "索引"), (0, "字段")] {
            assert!(
                puttyplus::panel::dbui::struct_click_segment(&panel.st, sidx, idx),
                "点第 {idx} 个分段按钮点不动"
            );
            pump(150);
            let cur = puttyplus::panel::dbui::struct_current_page(&panel.st, sidx);
            println!("点第 {idx} 个按钮 → {cur}");
            assert_eq!(cur, want, "点第 {idx} 个按钮没切过去（当前 {cur}）");
            // 按钮的选中态也要跟着变（不然用户看着选中的还是上一段）
            assert_eq!(
                puttyplus::panel::dbui::struct_segment_active(&panel.st, sidx),
                Some(idx),
                "按钮的选中态没跟上"
            );
        }

        // ---- 截图 ----
        {
            let _ = std::fs::remove_file("/tmp/struct_seg.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/struct_seg.png")
                .status();
            println!("截图 /tmp/struct_seg.png：{st:?}");
        }

        println!("struct_seg_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
