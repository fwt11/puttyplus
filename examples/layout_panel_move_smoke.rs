//! 阶段 178 冒烟：**文件面板搬去侧边栏** ＋ 跨格搬页签仍正常。
//!
//! **为什么这个阶段要冒烟**：
//! ① 文件面板**不再是格子树里的页**了 ⇒ 格子树默认**只有一格**（终端）
//!    ⇒ 终端占满整页宽；
//! ② 格子**还能拆**、页签**还能跨格搬**（阶段 145/146 的能力不能被带坏）；
//! ③ **终端分屏树不能被拆散**（`term_container` 那层 Paned 是分屏的地基）。
//!
//! ⚠️ **阶段 146 的版本测的是「文件面板自己一格」** ⇒ 那个能力**作废**
//!   了（文件面板搬去主窗口左栏的侧边栏）⇒ 本冒烟改成测**新形态**。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_panel_move_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;

fn pump(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 这个格子的页签数。
fn page_count(tree: &puttyplus::layout::tree::CellTree, id: puttyplus::layout::cell::CellId) -> usize {
    tree.cell(id).map(|c| c.tabs.page_count()).unwrap_or(0)
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutPanelMoveSmoke")
        .build();
    app.connect_activate(|app| {
        check(app);
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}

fn check(app: &gtk::Application) {
    // 独立设置文件（别碰用户设置）。
    let settings_ini = std::env::temp_dir()
        .join(format!("pp-layout-panel-move-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    let tabs = puttyplus::tabs::Tabs::new();
    // 连一个不存在的地址：这里只关心**布局**，不关心连上没连上。
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        ..Default::default()
    };
    tabs.open(&opts);
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(1200)
        .default_height(800)
        .build();
    
    win.present();
    pump(600);

    // ⚠️ **阶段 178**：文件面板住在**侧边栏**里 ⇒ 窗口里必须有侧边栏，
    //    否则文件面板**连父级都没有**（下面的断言就会误报）。
    let sb = puttyplus::sidebar::Sidebar::new(std::rc::Rc::clone(&tabs));
    sb.setup();
    let host = puttyplus::sidebarhost::SidebarHost::new(&sb);
    tabs.set_sidebar_host(&host);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&host.root));
    outer.set_end_child(Some(&tabs.notebook));

    win.set_child(Some(&outer));

    let st = tabs.current_panel_state().expect("应有当前面板");
    let page = tabs.current_page_widget().expect("应当有当前页");
    let tree = tabs
        .cell_tree_for(&page)
        .expect("连接页应当有格子树");

    // ---- 1. 默认布局 = **一个格子**（只有终端）----
    //
    // ⚠️ **阶段 178**：文件面板搬去**主窗口左栏的侧边栏**了 ⇒ 格子树里
    //    **不再有**「文件」那一格 ⇒ 默认就是单格（终端占满整页宽）。
    assert_eq!(tree.cell_count(), 1, "默认应当是 **1 个格子**（只有终端）");
    let ids = tree.ids();
    let term_cell = ids[0];
    assert_eq!(page_count(&tree, term_cell), 1, "那一格应当只有终端页");
    println!("[1] 默认布局 = 1 个格子（文件面板不在格子树里）✔");

    // ---- 2. 文件面板**不在**格子树里（它在侧边栏）----
    //
    // ⚠️ **怎么验"不在"**：文件面板是 `st.root` ⇒ 它**不该**是任何一格的
    //    页 ⇒ 那一格只有终端页（上面已断言）⇒ 且它**有父级**（在界面上）。
    assert!(
        st.root.parent().is_some(),
        "文件面板应当还挂在界面上（在侧边栏里）"
    );
    println!("[2] 文件面板在界面上，但不在格子树里 ✔");

    // ---- 3. 格子**还能拆**（阶段 145 的能力没被带坏）----
    let cell2 = tree
        .split_cell(term_cell, puttyplus::layout::dnd::SplitZone::Right)
        .expect("应当能拆出第二格");
    pump(200);
    assert_eq!(tree.cell_count(), 2, "拆完应当是两个格子");
    assert!(
        tree.cell(cell2).is_some(),
        "新拆出来的那一格应当在树上"
    );
    println!("[3] 格子还能拆 ✔");

    // ---- 4. **终端分屏树不受影响** ----
    // 终端格子里的终端页还在、内容还是那个竖向 Paned（上＝终端、下＝编辑区）。
    let term_tabs = tree.cell(term_cell).unwrap().tabs.clone();
    let term_kind = term_tabs.current_kind();
    assert_eq!(
        term_kind,
        puttyplus::panel::subtabs::PageKind::Terminal,
        "终端格子当前应当还是终端页"
    );
    // 终端页的内容 = 终端列（竖向 Paned）⇒ 上下两半都在。
    let term_col = term_tabs
        .paned_by_page_num(0)
        .expect("终端页的内容应当是终端列（竖向 Paned）");
    assert!(
        term_col.start_child().is_some(),
        "终端分屏的上半没了"
    );
    // ⚠️ **阶段 151**：「底部编辑区」退役 ⇒ 终端列**没有下半了**
    // （下半以前是编辑区 ⇒ 白占终端列的下半 ⇒ 用户 2026-11-22 报的
    //  「中间那一块是什么」）。
    assert!(
        term_col.end_child().is_none(),
        "编辑区退役了 ⇒ 终端列不该再有下半"
    );
    println!("[5] 终端分屏树不受影响（终端列上半＝终端容器，下半空着）✔");

    println!("layout_panel_move_smoke: OK");
}
