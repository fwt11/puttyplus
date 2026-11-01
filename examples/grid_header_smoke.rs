//! 阶段 46 冒烟：dbx 风格**列头**（列名 + 类型小字 + ⋮ 菜单入口）+ 多选。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验三件事：
//! 1. 列头是自定义 widget：**列名 + 下面一行小字类型**（dbx 的 `TEXT` 那种）；
//! 2. `title` 属性**还在**（导出 / 过滤条件编辑器靠它拿列名）；
//! 3. 网格能**多选**（dbx 的"批量修改所选"的前置）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example grid_header_smoke

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::dbgrid;

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
        .application_id("x.y.PpGridHeader")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类（.pp-menu-*）只有 theme::apply()
        // 之后才生效——不装主题的话，菜单是"没上妆"的默认样式
        // （图标和文字会挤在一起，看着像布局坏了）。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);
        let grid = dbgrid::DbGrid::new();
        grid.set_multi_select(true);
        grid.set_data(dbgrid::GridData {
            columns: vec![
                "id".into(),
                "name".into(),
                "email".into(),
                "age".into(),
                "active".into(),
            ],
            types: vec![
                ColType::Int,
                ColType::Text,
                ColType::Text,
                ColType::Int,
                ColType::Bool,
            ],
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("张三".into()),
                    Some("zhang@example.com".into()),
                    Some("31".into()),
                    Some("t".into()),
                ],
                vec![
                    Some("2".into()),
                    Some("李四".into()),
                    Some("li@example.com".into()),
                    Some("25".into()),
                    Some("f".into()),
                ],
                vec![
                    Some("3".into()),
                    Some("王五".into()),
                    Some("wang@example.com".into()),
                    Some("41".into()),
                    Some("t".into()),
                ],
            ],
            pk: vec![0],
        });
        pump(100);

        // ---- 断言 1/2：列头 ----
        let cols = grid.view.columns();
        assert_eq!(cols.len(), 5, "列数不对");
        // ⚠️ `title` 必须还在：导出、过滤条件、列类型映射都靠它拿列名
        assert_eq!(cols[0].title(), "id", "列标题丢了：{}", cols[0].title());
        assert_eq!(cols[3].title(), "age");
        let mut types = Vec::new();
        for c in cols.iter() {
            let Some(w) = c.widget() else {
                panic!("列头没有自定义 widget（dbx 风格列头没生效）");
            };
            let hb = w.downcast_ref::<gtk::Box>().expect("列头不是 Box").clone();
            let vb = hb
                .first_child()
                .and_then(|x| x.downcast_ref::<gtk::Box>().cloned())
                .expect("列头里没有竖向 Box");
            let mut labels = Vec::new();
            let mut child = vb.first_child();
            while let Some(c) = child {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    labels.push(l.text().to_string());
                }
                child = c.next_sibling();
            }
            assert_eq!(labels.len(), 2, "列头应当是「列名 + 类型小字」两行：{labels:?}");
            types.push(labels[1].clone());
        }
        assert_eq!(
            types,
            vec!["整数", "文本", "文本", "整数", "布尔"],
            "类型小字不对：{types:?}"
        );
        println!("列头类型小字：{types:?} ✓");

        // ---- 断言 3：多选 ----
        assert_eq!(
            grid.view.selection().mode(),
            gtk::SelectionMode::Multiple,
            "结果网格没开多选"
        );
        let sel = grid.view.selection();
        sel.select_path(&gtk::TreePath::from_indices(&[0]));
        sel.select_path(&gtk::TreePath::from_indices(&[2]));
        pump(50);
        assert_eq!(
            grid.selected_display_rows(),
            vec![0, 2],
            "多选没生效（dbx 的批量操作靠它）"
        );
        println!("多选：{:?} ✓", grid.selected_display_rows());

        // ---- 截图 ----
        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("阶段 46 列头冒烟")
            .default_width(920)
            .default_height(320)
            .build();
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        // ⚠️ **必须 vexpand**：不撑的话滚动窗口只按**内容高度**占位，
        // 里面的行会被裁掉（模型里有、屏幕上没有，看着像"数据没加载出来"）。
        sw.set_vexpand(true);
        sw.set_child(Some(&grid.view));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_start(10);
        root.set_margin_end(10);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        let hint = gtk::Label::new(Some("demo_users —— 列头 = 列名 + 类型小字 + ⋮ 列操作菜单"));
        hint.add_css_class("dim-label");
        hint.set_xalign(0.0);
        root.append(&hint);
        root.append(&sw);
        win.set_child(Some(&root));
        win.present();
        pump(600);

        // ---- 真点击「⋮」（xdotool 走真实事件路径）----        //
        // ⚠️ **不能只调内部函数**：那样测的是"我写的代码"，不是"用户点得到"。
        // 这里模拟真实鼠标点击，验两件事：
        // ① 列操作菜单真的弹出来了；
        // ② **点三角不会顺带触发排序**（GTK4 里点子控件仍会冒泡到列头按钮）。
        {
            let tris = grid.column_tris();
            assert_eq!(tris.len(), 5, "每列都该有一个「⋮」按钮");
            let tri = &tris[0];
            let (wx, wy) = tri
                .translate_coordinates(&win, 0.0, 0.0)
                .expect("换算坐标失败");
            // 找窗口 id（xdotool 的 mousemove --window 用）
            let out = std::process::Command::new("xdotool")
                .args(["search", "--name", "阶段 46 列头冒烟"])
                .output()
                .expect("xdotool 跑不起来");
            let winid = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            assert!(!winid.is_empty(), "找不到窗口 id（xdotool search 没结果）");
            let x = wx + 5.0;
            let y = wy + 5.0;
            let st = std::process::Command::new("xdotool")
                .args([
                    "mousemove",
                    "--window",
                    &winid,
                    &format!("{x:.0}"),
                    &format!("{y:.0}"),
                ])
                .status();
            println!("xdotool mousemove: {st:?}");
            std::process::Command::new("xdotool")
                .args(["click", "1"])
                .status();
            pump(600);
            println!("列菜单开着吗：{}", grid.column_menu_is_open());
            println!("列菜单条目：{:?}", grid.column_menu_titles());
            assert!(
                grid.column_menu_is_open(),
                "点「⋮」没弹出列操作菜单（dbx 的列头小三角入口）"
            );
            let titles = grid.column_menu_titles();
            assert!(
                titles.iter().any(|t| t.contains("升序")),
                "列菜单里应当有「升序排列」：{titles:?}"
            );
            assert!(
                titles.iter().any(|t| t.contains("复制列名")),
                "列菜单里应当有「复制列名」：{titles:?}"
            );
            assert!(
                grid.sort_state().is_none(),
                "点「⋮」**顺带排序了**（点子控件冒泡到列头按钮，得吃掉那次排序）"
            );
            println!("点 ⋮ 弹菜单且没顺带排序 ✓");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/grid_col_menu.png")
                .status();
            println!("截图 /tmp/grid_col_menu.png：{st:?}");
        }

        // ---- 阶段 47：行右键菜单（xdotool 真右键）----
        {
            // 可编辑：不然「设为 NULL」这类改数据的项是灰的
            grid.set_editable(true);
            let winid = {
                let out = std::process::Command::new("xdotool")
                    .args(["search", "--name", "阶段 46 列头冒烟"])
                    .output()
                    .expect("xdotool 跑不起来");
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            assert!(!winid.is_empty(), "找不到窗口 id");
            // 网格在窗口里的原点（列头 + 提示行的高度不好算 → 多试几个 y）
            let (vx, vy) = grid
                .view
                .translate_coordinates(&win, 0.0, 0.0)
                .expect("换算坐标失败");
            // 第 2 列（name）的 x
            let x0 = grid.view.columns()[1].x_offset() as f64 + vx + 10.0;
            // ⚠️ **多试几个 y**：表头是两行（列名 + 类型小字），高度不好算，
            // 猜错就点到列头上去了（列头右键弹的是**列**菜单）。
            println!("网格在窗口里的原点：({vx:.0}, {vy:.0})，第 2 列 x = {}", grid.view.columns()[1].x_offset());
            // ⚠️ **先按 ESC 关掉还开着的弹层**：GTK4 的 Popover 打开时会
            // **grab 住指针**，后面的右键全被它吃掉（点哪儿都是"关菜单"）。
            let _ = std::process::Command::new("xdotool")
                .args(["key", "Escape"])
                .status();
            pump(300);
            let mut opened = false;
            for dy in (28i32..140).step_by(4) {
                let y = vy + dy as f64;
                let _ = std::process::Command::new("xdotool")
                    .args([
                        "mousemove",
                        "--window",
                        &winid,
                        &format!("{x0:.0}"),
                        &format!("{y:.0}"),
                    ])
                    .status();
                let _ = std::process::Command::new("xdotool")
                    .args(["click", "3"])
                    .status();
                pump(250);
                if grid.row_menu_is_open() {
                    println!("真右键命中数据行（y 偏移 {dy}）");
                    opened = true;
                    break;
                }
                if dy == 28 {
                    println!("  （第 1 次右键没开菜单：y={:.0}）", vy + dy as f64);
                }
            }
            assert!(opened, "右键数据行没弹出行菜单（坐标没落在数据行上）");
            let titles = grid.row_menu_titles();
            println!("行菜单条目：{titles:?}");
            for want in [
                "设为 NULL",
                "批量修改所选…",
                "克隆为新行",
                "单元格详情…",
                "行详情…",
                "列详情…",
                "转置查看…",
                "复制单元格",
                "复制整行",
            ] {
                assert!(
                    titles.iter().any(|t| t == want),
                    "行菜单里应当有「{want}」：{titles:?}"
                );
            }
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/grid_row_menu.png")
                .status();
            println!("截图 /tmp/grid_row_menu.png：{st:?}");

            // 「设为 NULL」：把选中行的 name 设成 NULL → 待保存的改动里出现
            assert!(
                grid.row_menu_click_item("设为 NULL"),
                "「设为 NULL」点不动"
            );
            pump(200);
            let edits = grid.pending_edits();
            println!("设为 NULL 后的改动：{edits:?}");
            assert!(
                edits.values().any(|m| m.values().any(|v| v == "NULL")),
                "「设为 NULL」没写进待保存的改动：{edits:?}"
            );
            // 网格里被改的那一格显示成 NULL
            // （原始数据里没有 NULL 的 name，所以唯一显示 NULL 的就是刚改的那格）
            let hit = (0..3).find(|r| grid.cell_text(*r, 1) == "NULL");
            assert!(hit.is_some(), "被改成 NULL 的那一格没显示成 NULL");

            // 「克隆为新行」：多出一行待插入
            let before = grid.pending_inserts().len();
            assert!(
                grid.row_menu_click_item("克隆为新行"),
                "「克隆为新行」点不动"
            );
            pump(200);
            let after = grid.pending_inserts().len();
            println!("克隆前 {before} 行待插入 → 克隆后 {after} 行");
            assert_eq!(after, before + 1, "克隆没多出一行");
        }

        // 截图
        let app2 = app.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/grid_header.png")
                .status();
            println!("截图 /tmp/grid_header.png：{st:?}");
            app2.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    println!("grid_header_smoke: OK");
}
