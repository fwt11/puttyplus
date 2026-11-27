//! 阶段 178 冒烟：**布局持久化**（按会话记住格子布局）＋
//! **老布局里的「文件」那一格被摘掉**。
//!
//! **为什么这个阶段要冒烟**：存盘/还原是**跨进程**的事 ⇒ 光看单测不够：
//! 单测只能验「编解码对不对」，验不了「关掉连接页真的写了」「重开真的
//! 还原了」。
//!
//! ⚠️ **阶段 178 改了布局的形状**：文件面板搬去**主窗口左栏的侧边栏**了
//!    ⇒ 格子树里**不再有**「文件」那一格 ⇒ 默认布局 = **单格**（终端）。
//!    ⇒ 老用户存下来的布局里**还有**「文件」那一格 ⇒ 还原时**必须摘掉**
//!    （那一格没有内容可放）。
//!
//! 冒烟走的是**真路径**：
//! ① 开连接页 ⇒ 默认 **1 格** ⇒ 拆一个格子 ⇒ 关连接页（**同步写盘**）
//! ② 重开同一个会话 ⇒ 格子数应当**跟上次一样**
//! ③ 存下来的布局里**不该再有 "files"**（文件面板不在格子树里）
//! ④ **老布局**（带 "files"）⇒ 重开 ⇒ 那一格被**摘掉**、不白屏
//! ⑤ 存的文件坏了 ⇒ **回退默认布局**（绝不白屏）
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_persist_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::layout::cell::CellId;
use puttyplus::layout::tree::CellTree;

fn pump(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 这一格里的页签数。
fn page_count(tree: &CellTree, id: CellId) -> usize {
    tree.cell(id).map(|c| c.tabs.page_count()).unwrap_or(0)
}

/// 装着**终端页**的那一格。
fn terminal_cell(tree: &CellTree) -> CellId {
    tree.ids()
        .into_iter()
        .find(|id| {
            tree.cell(*id)
                .map(|c| {
                    c.tabs.page_ids().iter().any(|p| {
                        c.tabs.kind_of(*p) == puttyplus::panel::subtabs::PageKind::Terminal
                    })
                })
                .unwrap_or(false)
        })
        .expect("应当有装终端页的那一格")
}

/// 装着**提示页**的那一格（还原不出来的那一格）。
fn hint_cell(tree: &CellTree) -> CellId {
    tree.ids()
        .into_iter()
        .find(|id| {
            tree.cell(*id)
                .map(|c| c.tabs.current_kind() == puttyplus::panel::subtabs::PageKind::Other)
                .unwrap_or(false)
        })
        .expect("应当有给提示页的那一格")
}

/// 连不上也行的连接参数（这里只关心**布局**，不关心连上没连上）。
fn opts(session_name: &str) -> ConnOptions {
    ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        session_name: Some(session_name.to_string()),
        ..Default::default()
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutPersistSmoke")
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
        .join(format!("pp-layout-persist-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    let tabs = puttyplus::tabs::Tabs::new();
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(1200)
        .default_height(800)
        .build();
    
    win.present();

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

    /* ================= ① 存：拆一个格子 ⇒ 关页写盘 ================= */

    let o = opts("pp-persist-smoke");
    tabs.open(&o);
    pump(400);
    let page1 = tabs.current_page_widget().expect("应当有当前页");
    let tree1 = tabs.cell_tree_for(&page1).expect("连接页应当有格子树");
    // ⚠️ **阶段 178**：默认 = **1 格**（终端）——文件面板搬去侧边栏了。
    assert_eq!(tree1.cell_count(), 1, "默认布局 = **一个格子**（只有终端）");
    assert_eq!(
        page_count(&tree1, tree1.ids()[0]),
        1,
        "那一格应当只有终端页（文件面板不在格子里了）"
    );
    println!("[1] 默认布局 = 1 格（文件面板不在格子树里）✔");

    // 拆一个格子 ⇒ 两个。
    tabs.split_cell(&page1, false);
    pump(200);
    assert_eq!(tree1.cell_count(), 2, "拆完应当是两个格子");
    println!("[2] 拆出一个格子 ⇒ 2 格 ✔");

    // 关连接页 ⇒ **同步写盘**（节流存盘的定时器可能还没到）。
    assert!(
        tabs.close_page_forced(&page1),
        "应当能关掉连接页"
    );
    pump(200);

    let saved = puttyplus::layout::persist::load(&o.session_key())
        .expect("关连接页时应当把布局写进设置文件");
    println!("[3] 存下来的布局：{saved}");
    assert!(
        saved.contains("\"terminal\""),
        "存下来的布局里应当有终端那一格：{saved}"
    );
    // ⚠️ **阶段 178**：文件面板**不在格子树里** ⇒ 布局里**不该有 "files"**。
    assert!(
        !saved.contains("\"files\""),
        "布局里不该再有「文件」那一格（文件面板在侧边栏里）：{saved}"
    );
    assert_eq!(
        saved.matches("\"type\":\"leaf\"").count(),
        2,
        "应当存了两个叶子：{saved}"
    );

    /* ================= ② 还原：重开同一个会话 ================= */

    tabs.open(&o);
    pump(600);
    let page2 = tabs.current_page_widget().expect("应当有当前页");
    let tree2 = tabs.cell_tree_for(&page2).expect("连接页应当有格子树");
    assert_eq!(tree2.cell_count(), 2, "重开应当还原成两个格子");
    println!("[4] 重开 ⇒ 还原成 2 个格子 ✔");

    // ⚠️ **还原不出来的那一格 ⇒ 提示页**，不是假装恢复了内容。
    let empty_cell = hint_cell(&tree2);
    let hint_kind = tree2.cell(empty_cell).unwrap().tabs.current_kind();
    assert_eq!(
        hint_kind,
        puttyplus::panel::subtabs::PageKind::Other,
        "空格子应当给一张**提示页**（不是假装恢复了终端）"
    );
    println!("[5] 还原不出来的格子 ⇒ 提示页 ✔");

    // 关页 ⇒ 写盘。
    assert!(tabs.close_page_forced(&page2), "应当能关掉连接页");
    pump(200);

    /* ============ ③ **老布局**（带 "files"）⇒ 那一格被**摘掉** ============ */
    //
    // ⚠️ **这是阶段 178 最关键的兼容点**：老用户存下来的布局里**有**
    //    「文件」那一格（阶段 146 写进去的）⇒ 文件面板搬走之后那一格
    //    **没有内容可放** ⇒ 还原时**必须摘掉** ⇒ 否则会多出一个空灰壳，
    //    甚至把整页搞坏。
    puttyplus::layout::persist::save(
        &o.session_key(),
        "{\"type\":\"split\",\"stacked\":false,\
         \"a\":{\"type\":\"leaf\",\"tabs\":[\"files\"]},\
         \"b\":{\"type\":\"leaf\",\"tabs\":[\"terminal\"]}}",
    );
    tabs.open(&o);
    pump(600);
    let page3 = tabs.current_page_widget().expect("应当有当前页");
    let tree3 = tabs.cell_tree_for(&page3).expect("连接页应当有格子树");
    // 「文件」那一格被摘掉 ⇒ **只剩终端那一格**（不是两格）。
    assert_eq!(
        tree3.cell_count(),
        1,
        "老布局里的「文件」那一格应当被**摘掉**（它没有内容可放）"
    );
    assert_eq!(
        page_count(&tree3, tree3.ids()[0]),
        1,
        "剩下的那一格应当只有终端页"
    );
    // ⚠️ **终端格还在**（不能因为剥文件格把终端也剥了）。
    let term_cell = terminal_cell(&tree3);
    assert!(
        tree3.cell(term_cell).is_some(),
        "终端那一格必须还在（剥文件格不能连终端一起剥）"
    );
    println!("[6] 老布局里的「文件」格被摘掉、终端格还在 ✔");

    /* ================= ④ 存坏了 ⇒ 回退默认布局（绝不白屏） ================= */

    assert!(tabs.close_page_forced(&page3), "应当能关掉连接页");
    pump(200);
    puttyplus::layout::persist::save(&o.session_key(), "这不是 JSON{{{");
    tabs.open(&o);
    pump(600);
    let page4 = tabs.current_page_widget().expect("应当有当前页");
    let tree4 = tabs.cell_tree_for(&page4).expect("连接页应当有格子树");
    assert_eq!(
        tree4.cell_count(),
        1,
        "存坏了 ⇒ 回退**默认布局**（一个格子），而不是白屏"
    );
    let st4 = tabs.current_panel_state().expect("应有当前面板");
    assert!(
        st4.root.parent().is_some(),
        "回退之后文件面板照样在（它在侧边栏里，不受布局影响）"
    );
    println!("[7] 存坏了 ⇒ 回退默认布局（没白屏）✔");

    // 收尾：关掉连接页，把临时设置文件删掉。
    let _ = tabs.close_page_forced(&page4);
    let _ = std::fs::remove_file(&settings_ini);

    println!("layout_persist_smoke: OK");
}
