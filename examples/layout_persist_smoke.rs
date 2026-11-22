//! 阶段 147 冒烟：**布局持久化**（按会话记住格子布局）。
//!
//! **为什么这个阶段要冒烟**：存盘/还原是**跨进程**的事 ⇒ 光看单测不够：
//! 单测只能验「编解码对不对」，验不了「关掉连接页真的写了」「重开真的
//! 还原了」。
//!
//! 冒烟走的是**真路径**：
//! ① 开连接页 ⇒ 拆一个格子 ⇒ 关连接页（**同步写盘**）
//! ② 重开同一个会话 ⇒ 格子数应当**跟上次一样**（而不是退回默认的两格）
//! ③ 还原不出来的那一格 ⇒ 给的是**提示页**，不是假装恢复了内容
//! ④ 存的文件坏了 ⇒ **回退默认布局**（绝不白屏）
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
    win.set_child(Some(&tabs.notebook));
    win.present();

    /* ================= ① 存：拆一个格子 ⇒ 关页写盘 ================= */

    let o = opts("pp-persist-smoke");
    tabs.open(&o);
    pump(400);
    let page1 = tabs.current_page_widget().expect("应当有当前页");
    let tree1 = tabs.cell_tree_for(&page1).expect("连接页应当有格子树");
    assert_eq!(tree1.cell_count(), 2, "默认布局 = 两个格子（文件 | 终端）");

    // 拆一个格子 ⇒ 三个。
    tabs.split_cell(&page1, false);
    pump(200);
    assert_eq!(tree1.cell_count(), 3, "拆完应当是三个格子");
    println!("[1] 拆出一个格子 ⇒ 3 格 ✔");

    // 关连接页 ⇒ **同步写盘**（节流存盘的定时器可能还没到）。
    assert!(
        tabs.close_page_forced(&page1),
        "应当能关掉连接页"
    );
    pump(200);

    let saved = puttyplus::layout::persist::load(&o.session_key())
        .expect("关连接页时应当把布局写进设置文件");
    println!("[2] 存下来的布局：{saved}");
    assert!(
        saved.contains("\"terminal\""),
        "存下来的布局里应当有终端那一格：{saved}"
    );
    assert_eq!(
        saved.matches("\"type\":\"leaf\"").count(),
        3,
        "应当存了三个叶子：{saved}"
    );

    /* ================= ② 还原：重开同一个会话 ================= */

    tabs.open(&o);
    pump(600);
    let page2 = tabs.current_page_widget().expect("应当有当前页");
    let tree2 = tabs.cell_tree_for(&page2).expect("连接页应当有格子树");
    assert_eq!(
        tree2.cell_count(),
        3,
        "重开应当还原成三个格子（而不是退回默认的两格）"
    );
    println!("[3] 重开 ⇒ 还原成 3 个格子 ✔");

    // ⚠️ **文件面板还在**（还原不能把活的东西弄丢）。
    let st2 = tabs.current_panel_state().expect("应有当前面板");
    let file_pid = st2.file_page_id.get();
    assert_ne!(file_pid, 0, "应当记着文件面板那一页的 id");
    assert!(
        tree2.cell_of_page(file_pid).is_some(),
        "文件面板那一页应当还在某个格子里"
    );
    assert!(
        st2.root.parent().is_some(),
        "文件面板应当还挂在界面上"
    );
    println!("[4] 文件面板没丢（还在格子里、还挂在界面上）✔");

    // ⚠️ **还原不出来的那一格 ⇒ 提示页**，不是假装恢复了内容。
    let empty_cell = hint_cell(&tree2);
    let hint_kind = tree2.cell(empty_cell).unwrap().tabs.current_kind();
    assert_eq!(
        hint_kind,
        puttyplus::panel::subtabs::PageKind::Other,
        "空格子应当给一张**提示页**（不是假装恢复了终端）"
    );
    println!("[5] 还原不出来的格子 ⇒ 提示页 ✔");

    /* ================= ③ 合并：文件面板拖进终端格 ================= */

    // 把文件面板那一页搬进终端格 ⇒ 旧格子收拢 ⇒ 只剩一格（两页）。
    let term_cell = terminal_cell(&tree2);
    assert!(
        tree2.move_page_into(file_pid, term_cell, None),
        "文件面板那一页应当能搬进终端格"
    );
    pump(200);
    // ⚠️ 还原出来的是**三格**（文件格 ＋ 提示格 ＋ 终端格）⇒ 文件面板
    //    搬走之后**文件格收拢** ⇒ 剩两格（提示格 ＋ 终端格）。
    assert_eq!(
        tree2.cell_count(),
        2,
        "文件格空了 ⇒ 应当收拢（剩提示格 ＋ 终端格）"
    );
    assert!(
        tree2.cell(tree2.ids()[0]).is_none()
            || tree2.cell_of_page(file_pid) != Some(tree2.ids()[0]),
        "文件面板那一页应当已经不在旧格子里"
    );
    let merged = tree2.cell(term_cell).unwrap().tabs.clone();
    assert_eq!(merged.page_count(), 2, "那一格应当有终端页 ＋ 文件面板页");
    println!("[6] 文件面板并进终端格 ⇒ 旧格收拢、那一格两页 ✔");

    // 关页 ⇒ 写盘。
    assert!(tabs.close_page_forced(&page2), "应当能关掉连接页");
    pump(200);
    let saved2 = puttyplus::layout::persist::load(&o.session_key()).expect("应当写盘");
    assert!(
        saved2.contains("\"files\""),
        "存下来的布局里应当有文件面板那一页：{saved2}"
    );
    println!("[7] 合并后的布局也存下来了：{saved2}");

    /* ================= ④ 还原：合并过的那一格也要还原 ================= */

    tabs.open(&o);
    pump(600);
    let page3 = tabs.current_page_widget().expect("应当有当前页");
    let tree3 = tabs.cell_tree_for(&page3).expect("连接页应当有格子树");
    assert_eq!(
        tree3.cell_count(),
        2,
        "重开应当还原成两格（提示格 ＋ 终端格）"
    );
    let st3 = tabs.current_panel_state().expect("应有当前面板");
    let file_pid3 = st3.file_page_id.get();
    let merged_cell = terminal_cell(&tree3);
    assert_eq!(
        tree3.cell_of_page(file_pid3),
        Some(merged_cell),
        "文件面板那一页应当回到终端那一格"
    );
    assert_eq!(
        page_count(&tree3, merged_cell),
        2,
        "那一格应当有终端页 ＋ 文件面板页"
    );
    println!("[8] 合并过的那一格也还原对了 ✔");

    /* ================= ⑤ 存坏了 ⇒ 回退默认布局（绝不白屏） ================= */

    assert!(tabs.close_page_forced(&page3), "应当能关掉连接页");
    pump(200);
    puttyplus::layout::persist::save(&o.session_key(), "这不是 JSON{{{");
    tabs.open(&o);
    pump(600);
    let page4 = tabs.current_page_widget().expect("应当有当前页");
    let tree4 = tabs.cell_tree_for(&page4).expect("连接页应当有格子树");
    assert_eq!(
        tree4.cell_count(),
        2,
        "存坏了 ⇒ 回退**默认布局**（两个格子），而不是白屏"
    );
    let st4 = tabs.current_panel_state().expect("应有当前面板");
    assert!(
        st4.root.parent().is_some(),
        "回退之后文件面板照样在"
    );
    println!("[9] 存坏了 ⇒ 回退默认布局（没白屏）✔");

    // 收尾：关掉连接页，把临时设置文件删掉。
    let _ = tabs.close_page_forced(&page4);
    let _ = std::fs::remove_file(&settings_ini);

    println!("layout_persist_smoke: OK");
}
