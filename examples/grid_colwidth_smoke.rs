//! 阶段 85 冒烟：**列宽拖拽柄**（GTK4 没有可拖的列边框 ⇒ 自己实现）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验四件事：
//! 1. 拖拽柄真的能改列宽（**xdotool 真拖**：按住列头右边缘往右挪）；
//! 2. 拖过的列**保持**那个宽（不被"按可用宽度重排"覆盖掉），其余列分剩下的；
//! 3. 列头的**单击照常排序**（拖拽手势不能把单击抢走）；
//! 4. 列菜单里能「恢复默认宽度」；**列头右键菜单作用到正确的列**
//!    （藏过列之后"显示位置"跟"原始列下标"会错位）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example grid_colwidth_smoke

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

/// 找到窗口 id（xdotool 的 `mousemove --window` 要用）。
fn win_id(title: &str) -> String {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
        .expect("xdotool 跑不起来");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn xdo(args: &[&str]) {
    let _ = std::process::Command::new("xdotool").args(args).status();
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpGridColWidth")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);
        let grid = dbgrid::DbGrid::new();
        grid.set_data(dbgrid::GridData {
            columns: vec![
                "id".into(),
                "name".into(),
                "score".into(),
                "active".into(),
            ],
            // ⚠️ **四种不同类型**：藏掉第 1 列之后，光看菜单底下的
            // 「类型：…」那行就能认出菜单是给哪一列开的（文本 / 数字 / 布尔）。
            types: vec![
                ColType::Int,
                ColType::Text,
                ColType::Float,
                ColType::Bool,
            ],
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("张三".into()),
                    Some("98.5".into()),
                    Some("t".into()),
                ],
                vec![
                    Some("2".into()),
                    Some("李四".into()),
                    Some("88.0".into()),
                    Some("f".into()),
                ],
            ],
            pk: vec![0],
        });

        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("阶段 85 列宽拖拽柄冒烟")
            .default_width(1000)
            .default_height(320)
            .build();
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);
        sw.set_child(Some(&grid.view));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_start(10);
        root.set_margin_end(10);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        let hint = gtk::Label::new(Some(
            "把鼠标移到列头的**右边缘** ⇒ 光标变成一条竖线 ⇒ 按住往右拖就能改列宽",
        ));
        hint.add_css_class("dim-label");
        hint.set_xalign(0.0);
        root.append(&hint);
        root.append(&sw);
        win.set_child(Some(&root));
        win.present();
        pump(600);

        // ---- ① 初始宽度是"按内容分"的 ----
        let planned = grid.planned_widths();
        println!("初始列宽：{planned:?}");
        assert_eq!(planned.len(), 4);
        assert!(
            planned.iter().all(|w| *w > 0),
            "初始列宽不对：{planned:?}"
        );

        // ---- ② 程序化设宽：钉住这一列，其余列分剩下的 ----
        let before_planned = grid.planned_widths();
        grid.set_user_width(1, 400);
        pump(120);
        let planned = grid.planned_widths();
        println!("设宽后：{planned:?}（user_widths={:?}）", grid.user_widths());
        assert_eq!(planned[1], 400, "钉住的列宽被覆盖了：{planned:?}");
        assert_eq!(grid.user_width(1), Some(400));
        // 其余列得**让出空间**（不然钉住的那列就是从别人身上抢的）
        let rest_before = before_planned[0] + before_planned[2] + before_planned[3];
        let rest_after = planned[0] + planned[2] + planned[3];
        assert!(
            rest_after < rest_before,
            "其余列没让出空间：{before_planned:?} → {planned:?}"
        );
        // 总宽应当跟"没钉之前"一样（⚠️ 允许差 1~2px：分富余是整除，零头会丢）
        let total = planned.iter().sum::<i32>();
        let total_before = before_planned.iter().sum::<i32>();
        assert!(
            (total - total_before).abs() <= 2,
            "总宽变了：{before_planned:?} → {planned:?}"
        );

        // 恢复默认 ⇒ 回到"按内容分"
        assert!(grid.clear_user_width(1), "清掉用户宽度失败");
        pump(120);
        assert_eq!(grid.user_width(1), None);
        assert_ne!(grid.planned_widths()[1], 400, "恢复默认宽度没生效");

        // ---- ③ 真拖（xdotool）：按住列头右边缘往右挪 ----
        let winid = win_id("阶段 85 列宽拖拽柄冒烟");
        assert!(!winid.is_empty(), "找不到窗口 id（xdotool search 没结果）");
        let (vx, vy) = grid
            .view
            .translate_coordinates(&win, 0.0, 0.0)
            .expect("换算坐标失败");
        // 第 2 列（name）的列头
        let col = grid.view.columns()[1].clone();
        let header = col.widget().expect("列头没有自定义 widget");
        let (hx, hy) = header
            .translate_coordinates(&win, 0.0, 0.0)
            .expect("换算列头坐标失败");
        let hh = header.height().max(4) as f64;
        // **贴着右边缘**（拖拽柄只有这么宽）
        let x0 = vx + col.x_offset() as f64 + col.width() as f64 - 3.0;
        let y0 = hy + hh / 2.0;
        let before = col.width();
        println!("拖之前第 2 列宽 = {before}；拖拽起点 = ({x0:.0}, {y0:.0})");

        // 先把指针移过去（触发 motion ⇒ 光标变 col-resize），再按下拖
        xdo(&["mousemove", "--window", &winid, &format!("{x0:.0}"), &format!("{y0:.0}")]);
        pump(120);
        xdo(&["mousedown", "1"]);
        pump(60);
        for step in [10i32, 30, 60] {
            xdo(&[
                "mousemove",
                "--window",
                &winid,
                &format!("{:.0}", x0 + step as f64),
                &format!("{y0:.0}"),
            ]);
            pump(60);
        }
        xdo(&["mouseup", "1"]);
        pump(300);

        let after = grid.view.columns()[1].width();
        let user = grid.user_width(1);
        println!("拖之后第 2 列宽 = {after}（user_width = {user:?}）");
        assert!(
            user.is_some(),
            "拖了没反应（GTK4 的 set_resizable 自 4.10 起废弃 ⇒ 拖拽柄得自己实现）"
        );
        assert!(
            after > before + 30,
            "列宽没被拖宽（{before} → {after}）"
        );
        assert_eq!(after, user.unwrap(), "实际宽度与记录的用户宽度对不上");
        assert_eq!(
            grid.planned_widths()[1],
            after,
            "拖完的宽度被『按可用宽度重排』覆盖了：{:?} / {after}",
            grid.planned_widths()
        );
        assert!(
            grid.sort_state().is_none(),
            "拖拽顺带排序了（拖不是点）"
        );

        // ---- ④ 列头的单击照常排序（拖拽手势不能把单击抢走）----
        {
            let col = grid.view.columns()[1].clone();
            let x = vx + col.x_offset() as f64 + 12.0;
            let y = hy + hh / 2.0;
            xdo(&["mousemove", "--window", &winid, &format!("{x:.0}"), &format!("{y:.0}")]);
            pump(80);
            xdo(&["click", "1"]);
            pump(300);
            println!("单击列头后排序状态：{:?}", grid.sort_state());
            assert!(
                grid.sort_state().is_some(),
                "点列头不排序了（拖拽手势把单击抢走了？）"
            );
        }

        // ---- ⑤ 列菜单：拖过的列才有「恢复默认宽度」----
        {
            assert!(
                grid.col_menu_popup_now(1),
                "列菜单没弹出来"
            );
            let titles = grid.column_menu_titles();
            println!("列菜单：{titles:?}");
            assert!(
                titles.iter().any(|t| t == "恢复默认宽度"),
                "列菜单里没有「恢复默认宽度」：{titles:?}"
            );
            // 点它 ⇒ 宽度回到自动
            let pop = grid.col_menu_popup_now(1);
            assert!(pop);
            let _ = std::process::Command::new("xdotool")
                .args(["key", "Escape"])
                .status();
            pump(200);
            // 用钩子点菜单项（菜单项是按钮 ⇒ 直接 emit_clicked）
            grid.click_col_menu_item("恢复默认宽度");            pump(200);
            assert_eq!(grid.user_width(1), None, "「恢复默认宽度」没生效");
        }

        // ---- ⑥ 藏过列之后，列头右键菜单要作用到**正确的列** ----
        {
            // 藏掉第 1 列（id）⇒ 可见列变成 name / email / created_at
            grid.set_column_layout(vec![
                (0, false),
                (1, true),
                (2, true),
                (3, true),
            ]);
            pump(200);
            // 右键**第 2 个可见列**（email，原始下标 2）
            assert!(grid.col_menu_popup_now(1), "列菜单没弹出来");
            let col = grid.column_menu_col();
            println!("藏过列之后右键第 2 个可见列 ⇒ 菜单作用在原始列 {col:?}");
            assert_eq!(
                col,
                Some(2),
                "菜单作用到别的列上了（显示位置 1 ≠ 原始下标 2）"
            );
            let titles = grid.column_menu_titles();
            println!("列菜单文字：{titles:?}");
            // ⚠️ **类型那行也要对**：菜单底部写的是"类型：…" ⇒ 认得出是哪一列
            assert!(
                titles.iter().any(|t| t.contains("类型：数字")),
                "列菜单的类型不是第 2 个可见列（数字）：{titles:?}"
            );
            assert!(
                !titles.iter().any(|t| t.contains("类型：文本")),
                "列菜单作用到 name 列上了（显示位置 1 ≠ 原始下标 2）：{titles:?}"
            );
        }

        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("/tmp/grid_colwidth.png")
            .status();
        println!("截图 /tmp/grid_colwidth.png");
        // 退出（不然 app.run() 一直转）
        let app2 = app.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            app2.quit();
        });
    });
    app.run_with_args::<&str>(&[]);
    println!("grid_colwidth_smoke: OK");
}
