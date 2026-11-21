//! 阶段 146 冒烟：**文件面板独立成格**（默认布局：文件在左、终端在右）。
//!
//! **为什么这个阶段要冒烟**：文件面板是**会话级的一个 widget**，以前住在
//! 「文件面板｜终端列」那个 Paned 的左半 ⇒ 切终端页签时它要**搬家**（会闪）。
//! 阶段 146 把它变成**自己一格** ⇒ 搬家这件事彻底没了 ⇒ 但也带来新风险：
//! ① 面板会不会"人间蒸发"（搬家后宿主没跟着换）；
//! ② 旧格子会不会留下一个空壳；
//! ③ **终端分屏树会不会被拆散**。
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
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(600);

    let st = tabs.current_panel_state().expect("应有当前面板");
    let page = tabs.current_page_widget().expect("应当有当前页");
    let tree = tabs
        .cell_tree_for(&page)
        .expect("连接页应当有格子树");

    // ---- 1. 默认布局 = 两个格子（文件格子 | 终端格子）----
    assert_eq!(tree.cell_count(), 2, "默认应当是 2 个格子（文件 | 终端）");
    let ids = tree.ids();
    let file_cell = ids[0];
    let term_cell = ids[1];
    let file_pid = st.file_page_id.get();
    assert_ne!(file_pid, 0, "应当记着文件面板那一页的 id");
    assert_eq!(
        tree.cell_of_page(file_pid),
        Some(file_cell),
        "文件面板那一页应当在**左边**那个格子里"
    );
    assert_eq!(page_count(&tree, file_cell), 1, "文件格子应当只有文件面板一页");
    // ⚠️ **文件面板真的在屏幕上**（不是"在树上但看不见"）。
    assert!(
        st.root.parent().is_some(),
        "文件面板应当挂在一个格子里"
    );
    println!("[1] 默认布局 = 2 个格子（文件在左、终端在右）✔");

    // ---- 2. 宽度：文件格子的宽度 = 树根分割条的位置 ----
    let root_paned = tree.root_paned().expect("两个格子 ⇒ 树根是分割条");
    assert!(
        root_paned.position() > 0,
        "文件格子应当有宽度（分割条位置 = {}）",
        root_paned.position()
    );
    println!(
        "[2] 文件格子的宽度 = 树根分割条位置（{}px）✔",
        root_paned.position()
    );

    // ---- 3. 把文件面板那一页**搬到终端格子** ----
    assert!(
        tree.move_page_into(file_pid, term_cell, None),
        "文件面板那一页应当能搬进终端格子"
    );
    pump(200);
    assert_eq!(
        tree.cell_of_page(file_pid),
        Some(term_cell),
        "文件面板应当已经在终端格子里"
    );
    // ⚠️ **旧格子收拢**（空了 ⇒ 自动关闭）。
    assert!(
        tree.cell(file_cell).is_none(),
        "旧的文件格子空了 ⇒ 应当自动收拢"
    );
    assert_eq!(tree.cell_count(), 1, "应当只剩一个格子");
    // ⚠️ **面板没"人间蒸发"**：宿主跟着换了。
    let host = st.file_host.borrow().clone();
    assert!(
        host.is_some(),
        "搬家之后 `PanelState.file_host` 应当跟着换（否则面板会人间蒸发）"
    );
    assert!(
        st.root.parent().is_some(),
        "搬家之后文件面板应当还在界面上"
    );
    println!("[3] 文件面板搬到别的格子：面板仍在、旧格子收拢、file_host 跟着换 ✔");

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
    assert!(
        term_col.end_child().is_some(),
        "终端分屏的下半（编辑区）没了"
    );
    println!("[4] 终端分屏树不受影响（终端列上下两半都在）✔");

    println!("layout_panel_move_smoke: OK");
}
