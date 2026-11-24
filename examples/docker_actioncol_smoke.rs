//! 阶段 155 冒烟：**容器页改成一张满高的列表 ＋「操作」列**。
//!
//! **背景**（用户 2026-11-23）：
//! - 「容器页面，不要下面的那部分了，整个页面都改成列表，下面那些都通过弹窗展示」；
//! - 「去掉详情列，没啥用。去掉 CPU 和内存列」；
//! - 「放个操作列，把所有目前涉及的按钮都放在操作列，用图标按钮」。
//!
//! **断言**（**不连远端**，直接往模型里灌假容器 ⇒ CI 跑得动）：
//! 1. 页面**没有下半区**了（详情不在页面里 ⇒ 整页就是一张满高的列表）；
//! 2. 「CPU」「内存」「详情」三列**退役**了；
//! 3. 多了一列「操作」；
//! 4. 容器行 5 颗图标（详情 / 进容器 / 启动 / 停止 / 重启）、
//!    compose 项目行 6 颗（详情 / 编辑 / 启动 / 停止 / 销毁 / 重建）；
//! 5. **图标名在系统主题里真的存在**（写错 ⇒ 界面上是空白方块）；
//! 6. **可用性看原始状态**：运行中 ⇒「启动」灰、「停止」亮；
//! 7. 点「详情」⇒ **弹窗**打开（详情不再在页面下半区）；
//! 8. 点「停止」⇒ **二次确认**框弹出（写操作仍然要问一句）；
//! 9. **坐标级**：点 compose 项目行的第 2 颗图标命中的是**编辑**（**不是**
//!    下一行容器的「进容器」）⇒ 抓「命中判定差一行」的老坑；
//! 10. 详情弹窗**关得掉**（内容里有「关闭」按钮）⇒ 而且**关完还能再开**
//!     （**不许**销毁弹窗里那块共享的详情内容）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example docker_actioncol_smoke

#![allow(deprecated)] // 容器页沿用 TreeView/TreeStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::docker::{ContainerInfo, DockerDuration, DurUnit};
use puttyplus::panel::{SftpPanel, dockerui};

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

/// 灌一批假容器（**不连服务器**）——用来验界面的形状。
fn fake_containers() -> Vec<ContainerInfo> {
    let mk = |name: &str, compose: &str, running: bool| ContainerInfo {
        id: format!("{name}-id"),
        name: name.to_string(),
        image: "img:1".to_string(),
        status: if running { "Up 2 hours".into() } else { "Exited (0) 1 hour ago".into() },
        state: if running { "running".into() } else { "exited".into() },
        ports: String::new(),
        compose: compose.to_string(),
        compose_files: if compose.is_empty() {
            String::new()
        } else {
            "/srv/app/docker-compose.yml".to_string()
        },
        compose_dir: if compose.is_empty() {
            String::new()
        } else {
            "/srv/app".to_string()
        },
        cpu: "1.0%".to_string(),
        mem: "10MiB".to_string(),
        // ⚠️ **阶段 158**：状态列要显示时长 ⇒ 假数据也得带时长。
        duration: Some(if running {
            DockerDuration::Num(2, DurUnit::Hour)
        } else {
            DockerDuration::Num(1, DurUnit::Hour)
        }),
    };
    vec![
        mk("solo-a", "", true),  // 独立容器（顶层行，运行中）
        mk("web", "proj", true), // compose 项目 proj 的第 1 个（运行中）
        mk("solo-b", "", false), // 另一个独立容器（已停止）
        mk("db", "proj", true),  // compose 项目 proj 的第 2 个（运行中）
    ]
}

/// 当前**顶层窗口**的个数（用来验「点动作 ⇒ 弹出确认框」）。
fn toplevel_count() -> u32 {
    use gtk::gio::prelude::ListModelExt;
    gtk::Window::toplevels().n_items()
}

/// 关掉**最后一个**顶层窗口（冒烟里用来「取消」确认框）。
fn close_last_toplevel() {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    let n = model.n_items();
    if n == 0 {
        return;
    }
    if let Some(w) = model.item(n - 1).and_downcast::<gtk::Window>() {
        w.close();
    }
}

/// 某一行的**路径**（按名字找）（阶段 156 的坐标级冒烟用）。
fn row_path(view: &gtk::TreeView, name: &str) -> Option<gtk::TreePath> {
    let model = view.model()?;
    let mut it = model.iter_first()?;
    loop {
        let n: String = model.get(&it, 0);
        if n == name {
            return Some(model.path(&it));
        }
        if !model.iter_next(&mut it) {
            return None;
        }
    }
}

/// 「操作」列里第 N 颗图标的**屏幕坐标**（widget 坐标）（阶段 156 的坐标级冒烟用）。
///
/// ⚠️ **坐标必须从 GTK 自己要**：`cell_area` 给的就是**渲染器画图标的那块
///   矩形**（GTK 文档：「等价于传给 `gtk_cell_renderer_render()` 的
///   `cell_area`」）⇒ 图标从这块矩形的**左边缘**开始排（一个 `SLOT_W` 一颗）
///   ⇒ 算出来的就是**用户眼睛看到的那颗图标**的位置。
///
/// ⚠️ **两处换算，各有各的用处**：
///   - `cell_area` 给的是 **bin window 坐标** ⇒ 要换算回 **widget 坐标**
///     （手势/tooltip 给的是 widget 坐标）；
///   - `path_at_pos` 收的**也是** bin window 坐标 ⇒ 生产代码必须先把
///     widget 坐标换算过去 ⇒ **不换算就整体差一行**（老坑见
///     `src/panel/pane.rs:1881`；用户 2026-11-23 报的「点项目行的编辑却
///     弹出 term 窗口」就是这个）。
///
/// ⚠️ **y 取行的中间**（不是行的第一像素）：列头高度（25px）和行高（27px）
///   **差不多** ⇒ 差一行的误差点在行首时**又落回同一行**（测不出来），
///   点在行中间才会暴露 ⇒ 必须点中间。
fn action_icon_pos(
    view: &gtk::TreeView,
    path: &gtk::TreePath,
    idx: i32,
    n_icons: usize,
) -> Option<(i32, i32)> {
    use puttyplus::panel::actioncell::{group_offset, ICON_SIZE, SLOT_W};
    // 「操作」列 = 最后一列（阶段 155 把它 append 在最后）。
    let col = view.column((view.n_columns() - 1) as i32)?;
    let area = view.cell_area(Some(path), Some(&col));
    if area.height() <= 0 {
        // 行没显示出来（不在屏幕上）⇒ 算不了。
        return None;
    }
    let (x0, y0) = view.convert_bin_window_to_widget_coords(area.x(), area.y());
    // ⚠️ **阶段 158：图标组是右对齐的** ⇒ 第 idx 颗的左边缘 =
    //   格子宽 − 组宽 + idx×槽宽 ⇒ 用**同一个** `group_offset`（别自己算）。
    let x = x0 + group_offset(area.width(), n_icons) + idx * SLOT_W + ICON_SIZE / 2;
    let y = y0 + area.height() / 2;
    Some((x, y))
}

fn main() {
    // ⚠️ **隔离配置目录**（不许写用户真实的 settings.ini）。
    let root = std::env::temp_dir().join(format!("pp_actioncol_smoke_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cfg = root.join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }

    gtk::init().expect("gtk init");

    let panel = SftpPanel::new(&ConnOptions::default());
    // ⚠️ **只建界面，不连远端**（`ensure` 不会去连）⇒ CI 上也跑得动。
    dockerui::ensure(&panel.st);
    let page = dockerui::page_for_test(&panel.st).expect("应当有容器页");

    // ⚠️ **页面要真的挂到窗口上**（否则 GTK 不算布局 ⇒ 命中判定测不了，
    //    而且弹窗没有父窗口 ⇒ 会落在主窗后面）。
    let win = gtk::Window::new();
    win.set_title(Some("容器操作列冒烟"));
    win.set_default_size(900, 520);
    win.set_child(Some(&page));
    win.present();
    pump(200);

    dockerui::set_rows_for_test(&panel.st, fake_containers());
    pump(200);

    // ---- ① 列：CPU / 内存 / 详情退役了，多了「操作」----
    let titles = dockerui::column_titles_for_test(&panel.st);
    println!("[信息] 现在的列：{titles:?}");
    assert!(
        !titles.iter().any(|t| t == "CPU"),
        "「CPU」列应当退役：{titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t == "内存"),
        "「内存」列应当退役：{titles:?}"
    );
    assert!(
        !titles.iter().any(|t| t == "详情"),
        "「详情」列应当退役：{titles:?}"
    );
    assert!(
        titles.iter().any(|t| t == "操作"),
        "应当有一列「操作」：{titles:?}"
    );

    // ---- ② **容器行 3 颗**（启动 / 停止 / 重启）—— 阶段 160 ----
    //
    // ⚠️ 用户 2026-11-24：「操作列只保留启动、停止、重启，把其他的都移到
    //   右键菜单里」⇒ **容器行**。
    let c_icons = dockerui::row_action_icons_for_test(&panel.st, "solo-a");
    let c_names: Vec<&str> = c_icons.iter().map(|(i, _)| i.as_str()).collect();
    println!("[信息] 容器行的操作图标：{c_names:?}");
    assert_eq!(c_icons.len(), 3, "容器行应当 3 颗图标：{c_names:?}");
    assert_eq!(
        c_names,
        vec!["pp-play", "pp-stop", "view-refresh-symbolic"],
        "容器行应当只有 启动/停止/重启：{c_names:?}"
    );
    // ⚠️ **阶段 162：compose 项目行还要 up / down**（用户 2026-11-24：
    //   「compose 项目的操作列中应该还有与 up 和 down 对应的按钮」）。
    let p_icons = dockerui::row_action_icons_for_test(&panel.st, "proj");
    let p_names: Vec<&str> = p_icons.iter().map(|(i, _)| i.as_str()).collect();
    println!("[信息] 项目行的操作图标：{p_names:?}");
    assert_eq!(p_icons.len(), 5, "项目行应当 5 颗图标：{p_names:?}");
    assert_eq!(
        p_names,
        vec![
            "pp-play",
            "pp-stop",
            "view-refresh-symbolic",
            "go-up-symbolic",
            "go-down-symbolic"
        ],
        "项目行应当是 启动/停止/重启/up/down：{p_names:?}"
    );

    // ---- ③ 图标名在系统主题里真的存在（写错 ⇒ 空白方块）----
    let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().expect("display"));
    for n in c_names.iter().chain(p_names.iter()) {
        // ⚠️ **`pp-` 开头的是自绘图标**（阶段 157：Adwaita 只有实心的 ▶ / ■
        //   ⇒ 混在一排线性图标里太扎眼 ⇒ 自己画描边版）⇒ 不在主题里。
        if n.starts_with("pp-") {
            continue;
        }
        assert!(theme.has_icon(*n), "系统主题里没有图标 {n}（会显示成空白方块）");
    }

    // ---- ④ 可用性看原始状态 ----
    assert!(
        !dockerui::row_action_enabled_for_test(&panel.st, "solo-a", "start"),
        "运行中的容器「启动」应当点不了"
    );
    assert!(
        dockerui::row_action_enabled_for_test(&panel.st, "solo-a", "stop"),
        "运行中的容器「停止」应当能点"
    );
    assert!(
        dockerui::row_action_enabled_for_test(&panel.st, "solo-b", "start"),
        "已停止的容器「启动」应当能点"
    );
    assert!(
        !dockerui::row_action_enabled_for_test(&panel.st, "solo-b", "stop"),
        "已停止的容器「停止」应当点不了"
    );
    // ⚠️ **项目全在跑 ⇒「启动」点不了**（没得启动）。
    assert!(
        !dockerui::row_action_enabled_for_test(&panel.st, "proj", "start"),
        "全在跑的项目「启动」应当点不了"
    );
    assert!(
        dockerui::row_action_enabled_for_test(&panel.st, "proj", "stop"),
        "全在跑的项目「停止」应当能点"
    );

    // ---- ⑤ 点「详情」⇒ 弹窗打开（详情不再在页面下半区）----
    assert!(
        !dockerui::detail_visible_for_test(&panel.st),
        "详情弹窗不该自己开着"
    );
    dockerui::dispatch_action_for_test(&panel.st, "solo-a", "container", "detail");
    pump(300);
    assert!(
        dockerui::detail_visible_for_test(&panel.st),
        "点「详情」应当打开详情弹窗"
    );
    let wtitle = dockerui::detail_window_title_for_test(&panel.st);
    println!("[信息] 弹窗标题：{wtitle:?}");
    assert!(wtitle.contains("solo-a"), "弹窗标题应当有容器名：{wtitle:?}");

    // ---- ⑥ 点「停止」⇒ 二次确认（写操作仍然要问一句）----
    let before = toplevel_count();
    let action_before = dockerui::last_action_for_test(&panel.st);
    dockerui::dispatch_action_for_test(&panel.st, "solo-a", "container", "stop");
    pump(300);
    let after = toplevel_count();
    println!("[信息] 顶层窗口数 {before} → {after}");
    assert!(after > before, "点「停止」应当弹出确认框（写操作有二次确认）");
    assert_eq!(
        dockerui::last_action_for_test(&panel.st),
        action_before,
        "没点确定就执行了！"
    );
    // 关掉确认框（**不是点确定**）⇒ 什么都不该发生。
    close_last_toplevel();
    pump(300);
    assert_eq!(
        dockerui::last_action_for_test(&panel.st),
        action_before,
        "取消之后动作还是跑了"
    );
    assert!(
        !puttyplus::windows::dialogs_without_parent()
            .iter()
            .any(|t| t.contains("请确认")),
        "确认框没有父窗口：{:?}",
        puttyplus::windows::dialogs_without_parent()
    );

    // ---- ⑦ 坐标级：点哪一行就是哪一行（阶段 156）----
    //
    // ⚠️ **为什么必须用真坐标**：`dispatch_action_for_test` /
    //   `click_row_action_for_test` 都**直接构造命中结果** ⇒ **完全绕开
    //   坐标** ⇒ 「点项目行的『编辑』却弹出 term 窗口」（命中**差一行**）
    //   这种 bug **根本测不出来**。
    //
    // ⚠️ **用户 2026-11-23 报的**：「在 docker compose 项目后面显示的是编辑
    //   按钮，但点击之后弹出的是 term 窗口」。
    {
        let view = dockerui::view_for_test(&panel.st).expect("应当有表格");

        // 项目行（compose 项目的父行）。
        let pp = row_path(&view, "proj").expect("模型里应当有 proj 行");
        // ⚠️ **阶段 160**：操作列只剩 3 颗 ⇒ 第 1 颗 = 启动、第 2 颗 = 停止。
        let n_proj = dockerui::row_action_icons_for_test(&panel.st, "proj").len();
        let (x0, y0) = action_icon_pos(&view, &pp, 0, n_proj).expect("应当算出 proj 行第 1 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, x0, y0).as_deref(),
            Some("start"),
            "项目行第 1 颗图标应当是「启动」"
        );
        let (x1, y1) = action_icon_pos(&view, &pp, 1, n_proj).expect("应当算出 proj 行第 2 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, x1, y1).as_deref(),
            Some("stop"),
            "项目行第 2 颗图标应当是「停止」"
        );
        // ⚠️ **阶段 162：项目行第 4 颗 = up、第 5 颗 = down**
        //   （用户 2026-11-24：「compose 项目的操作列中应该还有与 up 和
        //   down 对应的按钮」）。
        let (x3, y3) = action_icon_pos(&view, &pp, 3, n_proj).expect("应当算出 proj 行第 4 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, x3, y3).as_deref(),
            Some("up"),
            "项目行第 4 颗图标应当是「up」"
        );
        let (x4, y4) = action_icon_pos(&view, &pp, 4, n_proj).expect("应当算出 proj 行第 5 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, x4, y4).as_deref(),
            Some("down"),
            "项目行第 5 颗图标应当是「down」"
        );

        // 容器行（独立容器）：⚠️ **阶段 160** ⇒ 第 1 颗 = 启动、第 2 颗 = 停止
        let cp = row_path(&view, "solo-a").expect("模型里应当有 solo-a 行");
        let n_cont = dockerui::row_action_icons_for_test(&panel.st, "solo-a").len();
        let (cx0, cy0) = action_icon_pos(&view, &cp, 0, n_cont).expect("应当算出 solo-a 行第 1 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, cx0, cy0).as_deref(),
            Some("start"),
            "容器行第 1 颗图标应当是「启动」"
        );
        let (cx1, cy1) = action_icon_pos(&view, &cp, 1, n_cont).expect("应当算出 solo-a 行第 2 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, cx1, cy1).as_deref(),
            Some("stop"),
            "容器行第 2 颗图标应当是「停止」"
        );
        // 第 3 颗 = 重启。
        let (cx2, cy2) = action_icon_pos(&view, &cp, 2, n_cont).expect("应当算出 solo-a 行第 3 颗图标");
        assert_eq!(
            dockerui::hit_action_at_for_test(&panel.st, cx2, cy2).as_deref(),
            Some("restart"),
            "容器行第 3 颗图标应当是「重启」"
        );
    }

    // ---- ⑦ 之二：悬停有 tooltip（阶段 157）----
    //
    // ⚠️ **用户 2026-11-23 提的**：「按钮增加tooltip」。
    //
    // ⚠️ **为什么「树视图默认不查 tooltip」是个坑**：GTK4 的
    //   `GtkTreeView` **默认 `has-tooltip = false`** ⇒ 不显式打开的话
    //   `query-tooltip` **一次都不会触发** ⇒ tooltip 写了也白写。
    {
        let view = dockerui::view_for_test(&panel.st).expect("应当有表格");
        let n_proj = dockerui::row_action_icons_for_test(&panel.st, "proj").len();
        let n_cont = dockerui::row_action_icons_for_test(&panel.st, "solo-a").len();
        assert!(
            view.has_tooltip(),
            "表格没开 has-tooltip ⇒ GTK 根本不会来问 tooltip（写了也不显示）"
        );

        // ⚠️ **阶段 160**：操作列只剩 3 颗 ⇒ 第 1 颗 = 启动。
        //   项目全在跑 ⇒ 启动点不了 ⇒ tooltip 要说明原因。
        let pp = row_path(&view, "proj").expect("模型里应当有 proj 行");
        let (x1, y1) = action_icon_pos(&view, &pp, 0, n_proj).expect("应当算出 proj 行第 1 颗图标");
        let tip = dockerui::action_tooltip_at_for_test(&panel.st, x1, y1)
            .expect("项目行第 1 颗图标应当有 tooltip");
        println!("[信息] 项目行第 1 颗图标的 tooltip：{tip:?}");
        assert!(
            tip.contains("启动"),
            "项目行「启动」图标的 tooltip 应当提到启动：{tip:?}"
        );

        // 容器行第 3 颗 = 重启 ⇒ tooltip 提到重启。
        let cp = row_path(&view, "solo-a").expect("模型里应当有 solo-a 行");
        let (x2, y2) = action_icon_pos(&view, &cp, 2, n_cont).expect("应当算出 solo-a 行第 3 颗图标");
        let tip2 = dockerui::action_tooltip_at_for_test(&panel.st, x2, y2)
            .expect("容器行第 3 颗图标应当有 tooltip");
        println!("[信息] 容器行第 3 颗图标的 tooltip：{tip2:?}");
        assert!(
            tip2.contains("重启"),
            "容器行「重启」图标的 tooltip 应当提到重启：{tip2:?}"
        );

        // ⚠️ **阶段 162：项目行第 4 / 5 颗（up / down）** ⇒ tooltip 得写清
        //   跑的是**哪条命令**（不然 ⌃ / ⌄ 看不出是啥）。
        let (x3, y3) = action_icon_pos(&view, &pp, 3, n_proj).expect("应当算出 proj 行第 4 颗图标");
        let tip3 = dockerui::action_tooltip_at_for_test(&panel.st, x3, y3)
            .expect("项目行第 4 颗图标应当有 tooltip");
        println!("[信息] 项目行第 4 颗（up）的 tooltip：{tip3:?}");
        assert!(
            tip3.contains("compose up"),
            "项目行「up」图标的 tooltip 应当写明是 docker compose up：{tip3:?}"
        );
        let (x4, y4) = action_icon_pos(&view, &pp, 4, n_proj).expect("应当算出 proj 行第 5 颗图标");
        let tip4 = dockerui::action_tooltip_at_for_test(&panel.st, x4, y4)
            .expect("项目行第 5 颗图标应当有 tooltip");
        println!("[信息] 项目行第 5 颗（down）的 tooltip：{tip4:?}");
        assert!(
            tip4.contains("compose down"),
            "项目行「down」图标的 tooltip 应当写明是 docker compose down：{tip4:?}"
        );

        // ⚠️ **阶段 162：项目行的名字带标记**（加粗 ＋「compose」），
        //   **容器行没有** ⇒ 一眼就分得出「项目」和「容器」。
        let proj_mk = dockerui::row_name_markup_for_test(&panel.st, "proj");
        let solo_mk = dockerui::row_name_markup_for_test(&panel.st, "solo-a");
        println!("[信息] 项目行的名字 markup：{proj_mk:?}");
        assert!(
            proj_mk.contains("<b>proj</b>") && proj_mk.contains("compose"),
            "项目行的名字应当加粗并带「compose」标记：{proj_mk:?}"
        );
        assert!(
            solo_mk.is_empty(),
            "容器行的名字不该带标记：{solo_mk:?}"
        );

        // 悬停**不在图标上**的地方 ⇒ 没有 tooltip（不该乱弹）。
        assert!(
            dockerui::action_tooltip_at_for_test(&panel.st, 3, 3).is_none(),
            "空白处不该有 tooltip"
        );
    }

    // ---- ⑧ 弹窗**关得掉**，而且**关完还能再开**（阶段 156）----
    //
    // ⚠️ **用户 2026-11-23 报的**：「操作列点详情按钮，弹出对话框没法关闭」。
    {
        // ⚠️ **内容里必须有「关闭」按钮**（普通 `GtkWindow` 没有自带标题栏
        //   ⇒ 外面那个 X 不一定在 ⇒ 用户可能一个关闭入口都看不到）。
        assert!(
            dockerui::detail_close_button_count_for_test(&panel.st) >= 2,
            "详情弹窗里应当有「关闭」按钮（容器详情 + 项目详情各一个）"
        );

        // 点「关闭」⇒ 弹窗藏起来。
        dockerui::close_detail_for_test(&panel.st);
        pump(300);
        assert!(
            !dockerui::detail_visible_for_test(&panel.st),
            "点了「关闭」弹窗还开着"
        );
        // ⚠️ **关掉不许销毁内容**（弹窗里装着**共享的**详情控件）。
        assert!(
            dockerui::detail_window_alive_for_test(&panel.st),
            "关一次就把弹窗销毁了"
        );
        assert!(
            dockerui::detail_content_attached_for_test(&panel.st),
            "关一次就把详情内容销毁了（内容被解除父子关系了）"
        );

        // **再打开还得能用**（这条专门抓「关一次就再也开不出来」）。
        dockerui::dispatch_action_for_test(&panel.st, "solo-a", "container", "detail");
        pump(300);
        assert!(
            dockerui::detail_visible_for_test(&panel.st),
            "关掉之后再点「详情」应当还能打开"
        );
        assert!(
            dockerui::detail_body_visible_for_test(&panel.st),
            "再打开时详情内容没了"
        );

        // 模拟点窗口的 X（走 `close-request`）⇒ 隐藏 + **不销毁**。
        //
        // ⚠️ **必须关两次**：GTK 4.18 的 `gtk_window_close()` **只有第一次
        //   有效**（第一次发 `close-request` 并把窗口藏起来；之后再 `close()`
        //   就只发信号、**窗口不藏** ⇒ 「第一次能关，第二次关不掉」——
        //   项目自己的最小复现见 `examples/gtktest.rs`）⇒
        //   `close-request` 必须自己 `hide` + `Stop`（不许走 GTK 的默认处理）
        //   ⇒ **每一次**都关得掉。
        for round in 1..=2 {
            dockerui::dispatch_action_for_test(&panel.st, "solo-a", "container", "detail");
            pump(300);
            assert!(
                dockerui::detail_visible_for_test(&panel.st),
                "第 {round} 轮：弹窗应当能打开"
            );
            dockerui::request_close_detail_for_test(&panel.st);
            pump(300);
            assert!(
                !dockerui::detail_visible_for_test(&panel.st),
                "第 {round} 轮：点 X 没关掉弹窗（GTK 4.18 的 close() 只有第一次有效）"
            );
            assert!(
                dockerui::detail_window_alive_for_test(&panel.st),
                "第 {round} 轮：点 X 把弹窗销毁了（应当只是藏起来）"
            );
            assert!(
                dockerui::detail_content_attached_for_test(&panel.st),
                "第 {round} 轮：点 X 把详情内容销毁了"
            );
        }
    }

    puttyplus::panel::shutdown(&panel.st);
    drop(panel);
    let _ = std::fs::remove_dir_all(&root);
    println!("docker actioncol smoke ok");
}
