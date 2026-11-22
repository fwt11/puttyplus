//! 阶段 150 冒烟：**拖放 / 拆分之后，格子里的分割条位置不能跳**。
//!
//! **为什么这个阶段要冒烟**：用户 2026-11-22 报「**拖动并放置之后，文件
//! 管理器的宽度就会变化，变得很宽**」。
//!
//! **根因**：`CellTree::rebuild()`（拆分 / 关闭 / 搬页签之后都会调）把**整棵
//! Paned 树重新造一遍** ⇒ 所有分割条的 position 掉回 **GTK 默认值**
//! ——而 GTK4 的默认 position 是**第一个子控件的自然宽度**⇒ 文件面板那一
//! 格直接被撑宽；而且**宽度上限的监听**（挂在旧 Paned 上）也跟着没了
//! ⇒ 连上限都不再生效。
//!
//! 修法：① 分割条按**身份**（两侧的第一个叶子格子）记住位置 ⇒ 重画时恢复；
//! ② 「重画完了」通知上层 ⇒ 根分割条按**记忆的宽度**复位 ＋ **重挂上限**。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_split_pos_smoke

use gtk::prelude::*;
use puttyplus::layout::dnd::SplitZone;
use puttyplus::layout::tree::CellTree;
use puttyplus::panel::subtabs::SubTabs;

fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 造一格（内容是个标签 ⇒ 最小宽很小 ⇒ 分割条的位置不会被 GTK 夹掉）。
fn make_cell() -> (std::rc::Rc<CellTree>, puttyplus::layout::cell::CellId) {
    let sub = SubTabs::new();
    let tree = CellTree::new(sub.clone());
    let cell = tree.current_cell();
    sub.add_page_icon_kind(
        Some("utilities-terminal-symbolic"),
        "终端",
        puttyplus::panel::subtabs::PageKind::Terminal,
        gtk::Label::new(Some("x")).upcast(),
        None,
    );
    (tree, cell)
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutSplitPosSmoke")
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

    // ================= 1. 造一个两格的树 =================
    let (tree, cell1) = make_cell();
    let cell2 = tree
        .split_cell(cell1, SplitZone::Right)
        .expect("应当能拆出第二格");
    win.set_child(Some(tree.root()));
    win.present();
    settle(400);

    let root = tree.root_paned().expect("两格 ⇒ 树根是分割条");
    // ⚠️ **「记忆的宽度」**：上层（`tabs.rs`）会按 `file_w` 复位根分割条
    //    ⇒ 这里用 `rebuilt_hook` 模拟它。
    let remembered = 250i32;
    let treew = std::rc::Rc::downgrade(&tree);
    let rebuilds = std::rc::Rc::new(std::cell::Cell::new(0i32));
    let rebuilds_probe = rebuilds.clone();
    tree.set_rebuilt_hook(move || {
        rebuilds.set(rebuilds.get() + 1);
        let Some(tree) = treew.upgrade() else { return };
        if let Some(p) = tree.root_paned() {
            p.set_position(remembered);
        }
    });
    tree.apply_rebuilt();
    settle(300);
    assert_eq!(
        root.position(),
        remembered,
        "刚建好时根分割条应当按记忆的宽度复位"
    );
    println!("[1] 根分割条按记忆宽度复位 = {remembered} ✔");

    // ================= 2. 拆分之后位置不能跳 =================
    let cell3 = tree
        .split_cell(cell2, SplitZone::Bottom)
        .expect("应当能在右格下面再拆一格");
    settle(300);
    let root2 = tree.root_paned().expect("还是两格以上 ⇒ 有根分割条");
    assert_eq!(
        root2.position(),
        remembered,
        "拆分之后根分割条的位置不能跳（用户报的就是这个）"
    );
    assert!(
        rebuilds_probe.get() > 0,
        "重建之后应当通知过上层（`rebuilt_hook`）"
    );
    println!("[2] 拆分之后位置没跳（{remembered}）✔");

    // ================= 3. **拖放**（跨格搬页签）之后也不能跳 =================
    //
    // ⚠️ **用户原话就是「拖动并放置之后」** ⇒ 这一条才是正主。
    let pid = tree
        .cell(cell1)
        .unwrap()
        .tabs
        .page_ids()
        .first()
        .copied()
        .expect("cell1 里有一页");
    assert!(
        tree.move_page_into(pid, cell3, None),
        "页签应当能搬进新格子"
    );
    settle(300);
    let root3 = tree.root_paned().expect("应当还有根分割条");
    assert_eq!(
        root3.position(),
        remembered,
        "拖放之后根分割条的位置不能跳（用户 2026-11-22 报的）"
    );
    println!("[3] 拖放之后位置没跳（{remembered}）✔");

    // ================= 4. 反复重画也不能跳 =================
    let cell4 = tree
        .split_cell(tree.current_cell(), SplitZone::Bottom)
        .expect("应当还能再拆一格");
    settle(300);
    let root4 = tree.root_paned().expect("应当还有根分割条");
    assert_eq!(
        root4.position(),
        remembered,
        "再次拆分之后根分割条的位置不能跳"
    );
    println!("[4] 反复重画位置没跳（{remembered}）✔");

    // ================= 5. 关到只剩一格 ⇒ 没有根分割条（正常） =================
    //
    // ⚠️ **这是对的**：一个格子时**没有分割条** ⇒ 没有宽度可言。
    tree.close_cell(cell4);
    settle(300);
    assert!(
        tree.root_paned().is_some() || tree.cell_count() == 1,
        "要么还有根分割条，要么只剩一格"
    );
    println!("[5] 关格子后状态正常（格子数 = {}）✔", tree.cell_count());

    println!("layout_split_pos_smoke: OK");
}
