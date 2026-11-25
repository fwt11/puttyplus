//! 阶段 151 冒烟：**关掉终端 ⇒ 新建终端 ⇒ 再打开文件编辑 ⇒ 编辑器要出现**。
//!
//! ## 复现的是哪个 bug（用户 2026-11-25 报的）
//!
//! 用户原话：「如果我关掉终端，然后新建终端，之后再在文件中打开文件编辑，
//! 编辑器就不会出现了」。
//!
//! ## 根因
//!
//! `PanelState::sub_tabs` 以前是**建页时记一次的快照**
//! （`tabs.rs` 里只有 `*sp.st.sub_tabs.borrow_mut() = Some(sub.clone())` 一处
//! 赋值 ⇒ 之后再也不更新）。
//!
//! 而**格子树是会变的**：关掉某一格里的**最后一个页签** ⇒ 那一格**收拢**
//! （从树上摘掉）⇒ 快照指着的那个容器**已经不在树上了**。
//!
//! ⇒ 编辑器页被挂进一个**看不见**的 notebook ⇒ **用户点了毫无反应**
//! （页签在、编辑器正文不在屏幕上）。
//!
//! ## 现在怎么修的
//!
//! 面板**动态问格子树**「家是哪一格」（装着终端页的那一格 ⇒ 最宽）；
//! 那一格收拢了就退到当前格子 ⇒ **挂上去的页一定看得见**。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example editor_home_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::layout::tree::CellTree;
use puttyplus::panel::subtabs::{PageKind, SubTabs};

fn pump(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// **这一格还在树上吗**（= 那一格的页签栏是不是挂在界面上）。
///
/// ⚠️ **判据不能只看「树里还有这个格子编号」**：格子被关掉时编号会从树里
///   摘掉 ⇒ `tree.cell(id)` 返回 `None` ⇒ 这就是「已经不在树上了」。
fn is_live(tree: &CellTree, id: puttyplus::layout::cell::CellId) -> bool {
    tree.cell(id).is_some()
}

/// 某一个容器里**装着终端页**吗（= 那一格是不是终端那一格）。
fn has_terminal(sub: &SubTabs) -> bool {
    !sub.page_ids_of_kind(PageKind::Terminal).is_empty()
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpEditorHomeSmoke")
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
        .join(format!("pp-editor-home-{}.ini", std::process::id()));
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
    let tree = tabs.cell_tree_for(&page).expect("连接页应当有格子树");

    // ---- 1. 正常场景：编辑器挂在**终端那一格**（与终端并列）----
    let term_cell = {
        let ids = tree.ids();
        let mut found = None;
        for id in &ids {
            if let Some(c) = tree.cell(*id) {
                if has_terminal(&c.tabs) {
                    found = Some(*id);
                    break;
                }
            }
        }
        found.expect("应当有一格装着终端页")
    };
    assert!(
        is_live(&tree, term_cell),
        "终端那一格应当在树上"
    );

    let eid = puttyplus::panel::fileedit::open_placeholder_for_smoke(&st, "/tmp/正常.txt", "正文");
    pump(200);
    let ecell = tree
        .cell_of_page(eid)
        .expect("编辑器应当挂在树上某一格（正常场景）");
    assert_eq!(
        ecell, term_cell,
        "编辑器应当与终端并列（挂在终端那一格）"
    );
    println!("[1] 正常场景：编辑器挂在终端那一格 ✔");

    // 关掉这个编辑器（后面要重来一遍）。
    assert!(
        puttyplus::panel::fileedit::close_editor_by_id(&st, eid),
        "应当能关掉编辑器"
    );
    pump(200);

    // ---- 2. 关掉终端 ⇒ 终端那一格**收拢**（从树上摘掉）----
    let term_sub = tree
        .cell(term_cell)
        .expect("终端那一格还在")
        .tabs
        .clone();
    let term_page = term_sub
        .page_ids_of_kind(PageKind::Terminal)
        .first()
        .copied()
        .expect("应当有终端页");
    assert!(term_sub.close_page(term_page), "应当能关掉终端页签");
    pump(300);
    assert!(
        !is_live(&tree, term_cell),
        "终端那一格空了 ⇒ 应当收拢（从树上摘掉）"
    );
    println!("[2] 关掉终端 ⇒ 那一格收拢 ✔");

    // ---- 3. 新建终端（落在还活着的那一格）----
    let alive = tree
        .ids()
        .into_iter()
        .find(|id| is_live(&tree, *id))
        .expect("应当还剩一格");
    let alive_sub = tree.cell(alive).expect("还活着的那格").tabs.clone();
    let new_term = alive_sub.add_page_icon_kind(
        Some("utilities-terminal-symbolic"),
        "终端",
        PageKind::Terminal,
        gtk::Label::new(Some("新终端")).upcast(),
        None,
    );
    alive_sub.switch_to(new_term);
    pump(200);
    assert!(
        has_terminal(&alive_sub),
        "新终端应当落在还活着的那一格"
    );
    println!("[3] 新建终端 ⇒ 落在还活着的那一格 ✔");

    // ---- 4. 再从文件面板打开文件编辑 ⇒ **编辑器必须出现** ----
    //
    // ⚠️ **这就是用户报的那个场景**：以前这一页会挂到**已经收拢的那一格**
    //   （看不见的地方）⇒ 用户点了毫无反应。
    let eid2 = puttyplus::panel::fileedit::open_placeholder_for_smoke(&st, "/tmp/新建后.txt", "正文");
    pump(300);
    let ecell2 = tree
        .cell_of_page(eid2)
        .expect("编辑器必须挂在树上某一格（关掉终端 + 新建终端之后）");
    assert!(
        is_live(&tree, ecell2),
        "编辑器挂的那一格必须还在树上（否则用户看不见它）"
    );
    assert_eq!(
        ecell2, alive,
        "编辑器应当落在**当前那一格**（新终端所在的那一格）"
    );
    // 再验一次「看得见」：那一格的页签栏当前页就是编辑器。
    let sub_now = tree.cell(ecell2).expect("编辑器那一格").tabs.clone();
    assert_eq!(
        sub_now.current_editor(),
        Some(eid2),
        "编辑器应当是那一格**当前显示**的页（用户要一眼看见它）"
    );
    println!("[4] 关掉终端 + 新建终端之后打开编辑器 ⇒ 编辑器出现了 ✔");

    // 收尾。
    let _ = tabs.close_page_forced(&page);
    let _ = std::fs::remove_file(&settings_ini);

    println!("editor_home_smoke: OK");
}
