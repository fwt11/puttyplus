//! 阶段 144 冒烟：**格子树外壳**（界面零变化 ＋ 树形正确）。
//!
//! **为什么这个阶段要冒烟**：格子树是**整个改造的地基** ⇒
//! ① 初始状态必须**跟今天一模一样**（否则一上线就变了样）；
//! ② 树的操作（拆分/关闭）必须**几何上真的收拢**（不能留空缝）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_tree_smoke

use gtk::prelude::*;
use puttyplus::layout::cell::CellId;
use puttyplus::layout::tree::{self, CellTree, TreeNode};
use puttyplus::panel::subtabs::SubTabs;

fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// widget 树里**第一层**是不是某个方向的 Paned。
fn is_paned(w: &gtk::Widget, vertical: bool) -> bool {
    match w.downcast_ref::<gtk::Paned>() {
        Some(p) => p.orientation() == if vertical {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        },
        None => false,
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutTreeSmoke")
        .build();
    app.connect_activate(|app| {
        check(app);
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}

fn check(app: &gtk::Application) {
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(900)
        .default_height(600)
        .build();

    // ---- 1. 初始状态 = 一个叶子 ⇒ 界面与今天一致 ----
    let sub = SubTabs::new();
    let tree = CellTree::new(sub.clone());
    assert_eq!(tree.cell_count(), 1);
    assert_eq!(tree.tree(), TreeNode::Leaf(CellId(1)));
    // 根容器里装的就是那个格子的 `SubTabs` 外层容器 ⇒ **零变化**
    let root = tree.root();
    let first = root.first_child().expect("根容器里应有格子");
    assert!(
        first == *sub.root(),
        "初始格子里装的不是现有的 `SubTabs` ⇒ 界面会变样"
    );
    println!("[1] 初始树 = 1 个叶子，根容器里就是现有的 SubTabs ✔");

    // ---- 2. 2×2 网格：四个叶子都在，Paned 嵌套正确 ----
    let t = CellTree::new(SubTabs::new());
    let a = t.add_cell(SubTabs::new());
    let b = t.add_cell(SubTabs::new());
    let c = t.add_cell(SubTabs::new());
    let d = t.add_cell(SubTabs::new());
    // 上排 a|b，下排 c|d
    t.set_tree(TreeNode::Split {
        stacked: true,
        a: Box::new(TreeNode::Split {
            stacked: false,
            a: Box::new(TreeNode::Leaf(a)),
            b: Box::new(TreeNode::Leaf(b)),
        }),
        b: Box::new(TreeNode::Split {
            stacked: false,
            a: Box::new(TreeNode::Leaf(c)),
            b: Box::new(TreeNode::Leaf(d)),
        }),
    });
    assert_eq!(t.cell_count(), 4);
    assert_eq!(t.ids(), vec![a, b, c, d]);

    win.set_child(Some(t.root()));
    win.present();
    settle(500);

    let grid = t.root().first_child().expect("网格根");
    assert!(
        is_paned(&grid, true),
        "2×2 的根应是**竖向** Paned（上排/下排）"
    );
    let top = grid.downcast_ref::<gtk::Paned>().unwrap().start_child().unwrap();
    assert!(is_paned(&top, false), "上排应是**横向** Paned（左/右）");
    println!("[2] 2×2 网格：4 个叶子、Paned 嵌套（竖 → 横）✔");

    // ---- 3. 关掉一个格子 ⇒ 父节点收拢（不留空缝） ----
    let tree2 = tree::close(&t.tree(), a).unwrap();
    assert_eq!(tree::leaf_count(&tree2), 3, "关掉一个应剩 3 个格子");
    t.set_tree(tree2);
    settle(500);
    let grid2 = t.root().first_child().expect("网格根");
    // 上排只剩 b ⇒ 那个横向 Paned 应当**收掉**（不再是一个 Paned）
    let top2 = grid2.downcast_ref::<gtk::Paned>().unwrap().start_child().unwrap();
    assert!(
        !is_paned(&top2, false),
        "上排只剩一个格子了，那个 Paned 没收掉 ⇒ 界面上多一条空缝"
    );
    println!("[3] 关掉一格后父节点收拢（上排不再是一个 Paned）✔");

    // ---- 4. 关掉最后一个格子 ⇒ 树空 ----
    let empty = tree::close(&TreeNode::Leaf(CellId(1)), CellId(1)).unwrap();
    assert_eq!(empty, TreeNode::Empty);
    println!("[4] 关掉最后一个格子 ⇒ 树 = Empty ✔");

    println!("layout_tree_smoke: OK");
}
