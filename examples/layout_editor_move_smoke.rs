//! 阶段 149 冒烟：**编辑器能拖进四个方向**（编辑器也是格子里的页签）。
//!
//! **为什么这个阶段要冒烟**：用户 2026-11-22 报「**我拖动编辑器到这4个框，
//! 都没反应**」——根因是编辑器**不在 `pages` 列表里**（它在 `editor_pages`
//! 里，有自己的一套登记）⇒ 跨格子搬运那套机制（`take_page` /
//! `insert_page`）**根本认不出它** ⇒ 拖了等于没拖。
//!
//! 另外还验「**落点高亮框**盖的是不是目标那半区」——之前上下方向拿到的是
//! **整个格子**（GTK4 Overlay 的 measure 跟着上一轮分配走）⇒ 用户看着
//! 「框有些小 / 看不出落在哪儿」。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_editor_move_smoke

use gtk::prelude::*;
use puttyplus::layout::dnd::SplitZone;
use puttyplus::layout::tree::CellTree;
use puttyplus::panel::subtabs::{PageKind, SubTabs};

fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 造一个「只有编辑器」的格子，返回 `(树, 源格子, 编辑器 id)`。
///
/// ⚠️ **不建终端页**：这个冒烟只关心「编辑器能不能被搬走」。
fn make_cell_with_editor(editor_id: u64) -> (std::rc::Rc<CellTree>, puttyplus::layout::cell::CellId) {
    let sub = SubTabs::new();
    let tree = CellTree::new(sub.clone());
    let cell = tree.current_cell();
    // 造一个编辑器（页内容 = 「文件面板｜编辑器」的 Paned）。
    let content = gtk::Label::new(Some("编辑器正文"));
    sub.add_editor(
        editor_id,
        None,
        None,
        "README.md",
        content.upcast(),
        None,
    );
    (tree, cell)
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutEditorMoveSmoke")
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

    // ================= 1. 编辑器是格子里的「页签」 =================
    let (tree, cell1) = make_cell_with_editor(7);
    let tabs = tree.cell(cell1).unwrap().tabs.clone();
    assert_eq!(tabs.page_count(), 1, "编辑器应当占一格");
    assert!(
        tabs.has(7),
        "编辑器必须能被格子认出（跨格搬运靠它找源格子）"
    );
    assert_eq!(
        tabs.kind_of(7),
        PageKind::Editor,
        "编辑器的种类应当是 Editor（存盘、搬运都靠它）"
    );
    assert_eq!(
        tabs.slot_kinds(),
        vec![PageKind::Editor],
        "按页序取出来的种类里应当有编辑器"
    );
    println!("[1] 编辑器是格子里的页签（has / kind_of / slot_kinds 都认得）✔");

    // ================= 2. 拖到四个方向 =================
    for (i, zone) in [
        SplitZone::Left,
        SplitZone::Right,
        SplitZone::Top,
        SplitZone::Bottom,
    ]
    .into_iter()
    .enumerate()
    {
        let eid = 100 + i as u64;
        let (tree, cell) = make_cell_with_editor(eid);
        let before = tree.cell_count();
        // ⚠️ **走落点那条路**：`split_cell_with_page` = 拆格子 ＋ 搬页签。
        let new_cell = tree
            .split_cell_with_page(eid, cell, zone)
            .expect("编辑器应当能拆到那一格去");
        assert_ne!(
            new_cell, cell,
            "新格子不能就是被拆的那个格子（否则等于没搬）"
        );
        assert_eq!(
            tree.cell_of_page(eid),
            Some(new_cell),
            "{zone:?}：编辑器应当落在**新拆出来**的格子里"
        );
        let dst = tree.cell(new_cell).unwrap().tabs.clone();
        assert_eq!(
            dst.page_count(),
            1,
            "{zone:?}：新格子应当只有编辑器这一页"
        );
        assert!(
            dst.has(eid),
            "{zone:?}：新格子应当认得这个编辑器"
        );
        // 源格子空了 ⇒ 自动收拢（只剩新格子）。
        assert_eq!(
            tree.cell_count(),
            before,
            "{zone:?}：源格子空了应当收掉（不留空壳）"
        );
        println!("[2] 拖到 {zone:?}：编辑器进新格子 ✔");
    }

    // ================= 3. 落点高亮框盖住的是「目标那半区」 =================
    let sub = SubTabs::new();
    win.set_child(Some(sub.root()));
    win.present();
    settle(400);
    let (nb_w, nb_h) = (sub.nb.width(), sub.nb.height());
    assert!(nb_w > 100 && nb_h > 100, "notebook 得先量到尺寸");
    for zone in [
        SplitZone::Left,
        SplitZone::Right,
        SplitZone::Top,
        SplitZone::Bottom,
    ] {
        let frame = sub.probe_drop_frame(zone);
        settle(250);
        let (fw, fh) = (frame.width(), frame.height());
        let (want_w, want_h) = match zone {
            SplitZone::Left | SplitZone::Right => (nb_w / 2, nb_h),
            SplitZone::Top | SplitZone::Bottom => (nb_w, nb_h / 2),
        };
        // ⚠️ **允许几像素的边框外溢**（CSS 的 border 加在外面）。
        assert!(
            (fw - want_w).abs() <= 8 && (fh - want_h).abs() <= 8,
            "{zone:?}：框应当盖住目标那半区（{want_w}x{want_h}），实际 {fw}x{fh}"
        );
        // ⚠️ **方向箭头**（阶段 149）：一眼看出会落在哪一边。
        let arrow = sub
            .probe_drop_arrow()
            .unwrap_or_else(|| panic!("{zone:?}：方向箭头应当显示"));
        let want_arrow = match zone {
            SplitZone::Left => "go-previous-symbolic",
            SplitZone::Right => "go-next-symbolic",
            SplitZone::Top => "go-up-symbolic",
            SplitZone::Bottom => "go-down-symbolic",
        };
        assert_eq!(
            arrow, want_arrow,
            "{zone:?}：方向箭头应当是 {want_arrow}"
        );
        // ⚠️ **图标必须真的在主题里**——不在的话 GTK 会画「缺图」占位符
        //    （用户 2026-11-20 在 Windows 版上就报过「图标全是缺图占位符」）。
        let display = sub.nb.display();
        let theme = gtk::IconTheme::for_display(&display);
        assert!(
            theme.has_icon(&arrow),
            "{zone:?}：图标 {arrow} 不在当前主题里 ⇒ 会显示成缺图占位符"
        );
        // ⚠️ **箭头要居中**（框是 Overlay 承载的 ⇒ 不设 Center 就贴左上角）。
        let (ax, ay) = sub
            .probe_drop_arrow_pos()
            .unwrap_or_else(|| panic!("{zone:?}：方向箭头应当显示"));
        let (cx, cy) = (fw as f64 / 2.0, fh as f64 / 2.0);
        assert!(
            (ax - cx).abs() < 12.0 && (ay - cy).abs() < 12.0,
            "{zone:?}：箭头应当居中（框中心 {cx:.0}x{cy:.0}），实际 {ax:.0}x{ay:.0}"
        );
        println!("[3] 落点框 {zone:?} = {fw}x{fh}（目标 {want_w}x{want_h}）+ 箭头 {arrow} ✔");
    }

    // ================= 4. 走**真实的落点链路**（drop 回调那条路） =================
    //
    // ⚠️ **为什么要单独测**：上面测的是 `CellTree` 的搬运 API；用户实际
    //    拖的是**页签** ⇒ 落点回调（`attach_cell_target` 的 drop）必须
    //    也能把编辑器搬走 ⇒ 这里用 `simulate_drop` 走同一条逻辑。
    {
        use puttyplus::layout::dnd::{PageKindTag, PageMove};

        // 4a. 把编辑器拖到**它自己格子的右边** ⇒ 右边生成新格子、编辑器
        //     进去 ⇒ **编辑器独立成格**（用户 2026-11-22 要的就是这个）。
        let (tree, cell) = make_cell_with_editor(21);
        let tabs = tree.cell(cell).unwrap().tabs.clone();
        assert!(
            tabs.simulate_drop(PageKindTag::Editor, 21, PageMove::Split(SplitZone::Right)),
            "编辑器拖到右边应当有反应（落点回调 ⇒ 拆格子 ＋ 搬页签）"
        );
        assert_eq!(
            tree.cell_of_page(21).map(|c| tree.cell(c).unwrap().tabs.page_count()),
            Some(1),
            "编辑器应当在新格子里（那一格只有它）"
        );
        assert_eq!(
            tree.cell_count(),
            1,
            "源格子空了应当收掉 ⇒ 只剩编辑器那一格"
        );
        println!("[4a] 落点回调：编辑器拖到右边 ⇒ 独立成格 ✔");

        // 4b. 把编辑器拖到**另一个格子**的下面 ⇒ 目标格子被劈开、编辑器
        //     落在新的一半里。
        let (tree, cell_a) = make_cell_with_editor(22);
        let cell_b = tree
            .split_cell(cell_a, SplitZone::Right)
            .expect("应当能造出第二个格子");
        let tabs_b = tree.cell(cell_b).unwrap().tabs.clone();
        assert!(
            tabs_b.simulate_drop(PageKindTag::Editor, 22, PageMove::Split(SplitZone::Bottom)),
            "编辑器拖到另一个格子的下面应当有反应"
        );
        assert!(
            tree.cell_of_page(22).is_some(),
            "编辑器应当还在某个格子里（没丢）"
        );
        assert!(
            tree.cell_of_page(22) != Some(cell_a),
            "编辑器应当离开原来那一格"
        );
        println!("[4b] 落点回调：编辑器拖到别的格子 ⇒ 落到新的一半里 ✔");
    }

    println!("layout_editor_move_smoke: OK");
}
