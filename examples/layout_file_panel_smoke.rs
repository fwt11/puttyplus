//! 阶段 146b 冒烟：**「显示文件面板」**（把关掉的文件面板接回来）。
//!
//! **为什么这个阶段要冒烟**：用户 2026-11-22 报的两件事：
//! ① 「文件」那个页签可以**手动关掉**，关掉之后文件面板**就没了**，
//!    **没有再打开的入口**；
//! ② 把编辑器从一格拖走之后，原来那一格**没收拢** ⇒ 中间留一大片空白。
//!
//! 冒烟走的是**真路径**（关掉 → 收拢 → 接回来）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_file_panel_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;

fn pump(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutFilePanelSmoke")
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
        .join(format!("pp-layout-file-panel-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    let tabs = puttyplus::tabs::Tabs::new();
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(1200)
        .default_height(800)
        .build();
    win.set_child(Some(&tabs.notebook));
    win.present();

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        ..Default::default()
    };
    tabs.open(&opts);
    pump(500);

    let page = tabs.current_page_widget().expect("应当有当前页");
    let st = tabs.current_panel_state().expect("应当有当前面板");
    let tree = tabs
        .cell_tree_for(&page)
        .expect("连接页应当有格子树");

    // ---- 1. 默认：文件面板在自己那一格 ----
    let file_pid = st.file_page_id.get();
    assert_ne!(file_pid, 0, "应当记着文件面板那一页的 id");
    let file_cell = tree.cell_of_page(file_pid).expect("文件面板应当在一个格子里");
    assert!(
        st.root.parent().is_some(),
        "文件面板应当挂在界面上"
    );
    assert!(
        !puttyplus::tabs::file_panel_hidden(),
        "默认文件面板是显示着的"
    );
    println!("[1] 默认：文件面板在自己那一格 ✔");

    // ---- 2. 关掉「文件」那个页签 ⇒ 那一格收拢 ⇒ 面板没了 ----
    let file_sub = st
        .file_sub
        .borrow()
        .clone()
        .expect("应当留着文件面板那个容器");
    assert!(
        file_sub.close_page(file_pid),
        "应当能关掉「文件」那个页签"
    );
    pump(200);
    assert!(
        tree.cell(file_cell).is_none(),
        "文件面板那一格空了 ⇒ 应当收拢（不然就是一个空灰壳）"
    );
    assert!(
        puttyplus::tabs::file_panel_hidden(),
        "文件面板关掉了 ⇒ 菜单里那一项应当可用"
    );
    println!("[2] 关掉「文件」页签 ⇒ 那一格收拢、文件面板没了 ✔");

    // ---- 3. 「显示文件面板」把它接回来 ----
    tabs.show_file_panel(&page);
    pump(200);
    assert!(
        !puttyplus::tabs::file_panel_hidden(),
        "接回来之后就不该再是「隐藏」状态"
    );
    let new_pid = st.file_page_id.get();
    assert_ne!(new_pid, 0, "接回来之后要重新记着那一页的 id");
    let back_cell = tree
        .cell_of_page(new_pid)
        .expect("文件面板应当又回到某个格子里");
    assert!(
        st.root.parent().is_some(),
        "文件面板应当又挂在界面上"
    );
    assert_ne!(
        back_cell, file_cell,
        "接回来的格子是**新**的（原来那个已经收拢了）"
    );
    println!("[3] 「显示文件面板」把面板接回来了（自己一格）✔");

    // ---- 4. 再关一次 ⇒ 再接回来 ⇒ 幂等 ----
    let file_sub2 = st.file_sub.borrow().clone().unwrap();
    assert!(file_sub2.close_page(new_pid));
    pump(200);
    assert!(puttyplus::tabs::file_panel_hidden());
    tabs.show_file_panel(&page);
    pump(200);
    assert!(!puttyplus::tabs::file_panel_hidden());
    assert!(st.root.parent().is_some());
    println!("[4] 关了再接、再关再接 ⇒ 都能接回来 ✔");

    // 收尾。
    let _ = tabs.close_page_forced(&page);
    let _ = std::fs::remove_file(&settings_ini);

    println!("layout_file_panel_smoke: OK");
}
