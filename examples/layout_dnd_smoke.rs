//! 阶段 145 冒烟：**跨格子拖拽**（页签能从格子 A 搬到格子 B ＋ 边缘拆分）。
//!
//! **为什么这个阶段要冒烟**：跨格子搬运是最容易出事的地方——
//! ① **页内容 widget 不能同时挂两个父**（不先摘就会 "already has a parent"）；
//! ② **搬运时不能触发关闭钩子**（否则终端的 SSH 会话被收掉）；
//! ③ **终端分屏树不能被拆散**（搬的是页签，不是终端）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example layout_dnd_smoke

use gtk::prelude::*;
use puttyplus::layout::cell::CellId;
use puttyplus::layout::dnd::{self, PageKindTag, SplitZone};
use puttyplus::layout::tree::CellTree;
use puttyplus::panel::subtabs::{PageKind, SubTabs};

fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 造一个「终端页」：里面是**分屏树**（两个终端的 Paned）。
///
/// ⚠️ **为什么要造分屏**：验收要求「**终端分屏树仍完整**（分屏里的两个终端
///   都还在）」⇒ 光搬一个空盒子验不出来。
fn make_term_page() -> (gtk::Widget, gtk::Widget, gtk::Widget) {
    let t1 = gtk::Label::new(Some("终端一"));
    let t2 = gtk::Label::new(Some("终端二"));
    let col = gtk::Paned::new(gtk::Orientation::Vertical);
    col.set_start_child(Some(&t1));
    col.set_end_child(Some(&t2));
    (col.upcast(), t1.upcast(), t2.upcast())
}

/// 这个 widget 上挂了几个拖放接收器（证明落点**真的注册了**）。
fn drop_target_count(w: &gtk::Widget) -> usize {
    let mut n = 0;
    let model = w.observe_controllers();
    for i in 0..model.n_items() {
        let Some(c) = model.item(i) else { continue };
        if c.downcast_ref::<gtk::DropTargetAsync>().is_some() {
            n += 1;
        }
    }
    n
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpLayoutDndSmoke")
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

    // ================= 1. 负载协议 =================
    // 统一成 `<kind>:<id>`，旧的 `pp-editor:<id>` / 裸 id 也认。
    assert_eq!(
        dnd::parse_page_payload(&dnd::encode_payload(PageKindTag::Terminal, 7)),
        Some((PageKindTag::Terminal, 7))
    );
    assert_eq!(
        dnd::parse_page_payload("pp-editor:42"),
        Some((PageKindTag::Editor, 42))
    );
    assert_eq!(
        dnd::parse_page_payload("42"),
        Some((PageKindTag::Editor, 42))
    );
    println!("[1] 负载协议：`terminal:7` 往返 ✔ 旧格式仍认 ✔");

    // ================= 2. 落点注册 =================
    let sub = SubTabs::new();
    let tree = CellTree::new(sub.clone());
    let cell1 = CellId(1);
    assert_eq!(tree.cell_count(), 1);
    // ⚠️ **格子的页签栏上必须有拖放接收器**（插入 ＋ 拆分都靠它）。
    assert!(
        drop_target_count(sub.nb.upcast_ref()) >= 1,
        "格子的页签栏上没有拖放接收器 ⇒ 拖不过去"
    );
    println!("[2] 落点已注册（页签栏上有 DropTargetAsync）✔");

    // ================= 3. 造两个带分屏树的终端页 =================
    let (page1_w, t1, t2) = make_term_page();
    let pid1 = sub.add_page_icon_kind(
        Some("utilities-terminal-symbolic"),
        "终端一",
        PageKind::Terminal,
        page1_w.clone(),
        None,
    );
    let (page2_w, _u1, _u2) = make_term_page();
    let pid2 = sub.add_page_icon_kind(
        Some("utilities-terminal-symbolic"),
        "终端二",
        PageKind::Terminal,
        page2_w.clone(),
        None,
    );
    assert_eq!(sub.page_count(), 2);
    assert_eq!(tree.cell_of_page(pid1), Some(cell1));
    println!("[3] 初始格子里有两个终端页 ✔");

    win.set_child(Some(tree.root()));
    win.present();
    settle(400);

    // ================= 4. 拆格子（边缘语义） =================
    // 把当前格子往**右边**拆 ⇒ 新格子在右（空着）。
    let cell2 = tree
        .split_cell(cell1, SplitZone::Right)
        .expect("当前格子应当能拆");
    assert_eq!(tree.cell_count(), 2);
    assert_eq!(tree.ids(), vec![cell1, cell2]);
    settle(300);
    println!("[4] 拆分格子：1 → 2（新格子在右）✔");

    // ================= 5. 跨格搬运（走落点那条路） =================
    // 把「终端一」从格子 1 搬到格子 2 的页签栏。
    assert!(
        tree.move_page_into(pid1, cell2, None),
        "页签应当能从格子 1 搬进格子 2"
    );
    assert_eq!(
        tree.cell_of_page(pid1),
        Some(cell2),
        "页签搬完应当在新格子里"
    );
    assert_eq!(
        tree.cell(cell1).unwrap().tabs.page_ids(),
        vec![pid2],
        "源格子应当只剩另一个页签"
    );
    assert_eq!(
        tree.cell(cell2).unwrap().tabs.page_ids(),
        vec![pid1],
        "目标格子应当收到那个页签"
    );
    // ⚠️ **页内容 widget 只能有一个父**：它现在**只在**目标格子的 notebook 里。
    //
    // ⚠️ **不能比 `parent()`**：GTK4 里 notebook 的页内容挂在 notebook 的
    //    **内部容器**上（不是 notebook 自己）⇒ 比"在不在这个 notebook 里"才对。
    let cell1_tabs = tree.cell(cell1).unwrap().tabs.clone();
    let cell2_tabs = tree.cell(cell2).unwrap().tabs.clone();
    assert!(
        cell2_tabs.nb.page_num(&page1_w).is_some(),
        "页内容应当已经在目标格子的 notebook 里"
    );
    assert!(
        cell1_tabs.nb.page_num(&page1_w).is_none(),
        "页内容还留在源格子的 notebook 里 ⇒ 它同时挂了两个父"
    );
    // ⚠️ **终端分屏树仍完整**（分屏里的两个终端都还在）。
    let col = page1_w.downcast_ref::<gtk::Paned>().expect("页内容还是那个竖向 Paned");
    assert!(col.start_child().is_some(), "分屏的上半没了");
    assert!(col.end_child().is_some(), "分屏的下半没了");
    assert_eq!(t1.parent().is_some(), true, "第一个终端被拆散了");
    assert_eq!(t2.parent().is_some(), true, "第二个终端被拆散了");
    settle(300);
    println!("[5] 跨格搬运：页签在新格子、只有一个父、分屏树完整 ✔");

    // ================= 6. 边缘拆分（拖到格子边上） =================
    // 6a. 格子 2 被劈成两半（新格子在**左**）。
    let cell3 = tree
        .split_cell(cell2, SplitZone::Left)
        .expect("格子 2 应当能劈开");
    assert_eq!(tree.cell_count(), 3, "劈开之后应当有 3 个格子");
    assert_eq!(
        tree.ids(),
        vec![cell1, cell3, cell2],
        "劈开之后**顺序**不能变（老格子不能被挤到后面去）"
    );
    settle(200);

    // 6b. 把「终端二」从格子 1 拖进**新拆出来**的格子。
    assert!(
        tree.move_page_into(pid2, cell3, None),
        "页签应当能搬进新格子"
    );
    assert_eq!(
        tree.cell_of_page(pid2),
        Some(cell3),
        "页签应当落在新拆出来的格子里"
    );
    // 格子 1 空了 ⇒ 自动关闭（父节点收拢）⇒ 回到 2 个格子。
    assert_eq!(tree.cell_count(), 2, "源格子空了应当自动收掉");
    settle(300);
    println!("[6] 边缘拆分：格子被劈开、顺序不变、页签进新格子、源格子自动收拢 ✔");

    // 6c. **一步到位**的那条路（拖放落点实际走的就是它）：
    //     「拆格子 ＋ 把页签搬进新格子」一次做完。
    let (page3_w, _v1, _v2) = make_term_page();
    let pid3 = tree
        .cell(cell3)
        .unwrap()
        .tabs
        .add_page_icon_kind(
            Some("utilities-terminal-symbolic"),
            "终端三",
            PageKind::Terminal,
            page3_w.clone(),
            None,
        );
    let cell4 = tree
        .split_cell(cell2, SplitZone::Bottom)
        .expect("格子 2 应当能上下劈开");
    let cell5 = tree
        .split_cell_with_page(pid3, cell4, SplitZone::Right)
        .expect("一步到位（拆 ＋ 搬）应当成功");
    assert_eq!(
        tree.cell_of_page(pid3),
        Some(cell5),
        "一步到位之后页签应当落在**新拆出来**的格子里"
    );
    assert_ne!(cell5, cell4, "新格子不能就是被拆的那个格子");
    settle(200);
    println!("[6c] 一步到位（拆 ＋ 搬）✔");

    // ================= 7. 关掉最后一个页签 ⇒ 格子自动关闭 =================
    // 现在的格子：cell3（pid2）/ cell2（pid1）/ cell4（空）/ cell5（pid3）。
    assert_eq!(tree.cell_count(), 4);
    // 关掉 cell2 的最后一个页签 ⇒ cell2 自动关闭。
    assert!(
        tree.cell(cell2).unwrap().tabs.close_page(pid1),
        "关页签应当成功"
    );
    settle(200);
    assert!(
        tree.cell(cell2).is_none(),
        "格子里的最后一个页签关掉 ⇒ 该格子应当自动关闭"
    );
    assert_eq!(tree.cell_count(), 3, "应当剩 3 个格子");
    // 再关一个 ⇒ 2 个。
    assert!(
        tree.cell(cell5).unwrap().tabs.close_page(pid3),
        "关页签应当成功"
    );
    settle(200);
    assert_eq!(tree.cell_count(), 2, "应当剩 2 个格子");

    // ⚠️ **最后一个格子不关**（关掉它连接页就空了 ⇒ 用户连「+」按钮都
    //    看不到 ⇒ 没法再开终端 ⇒ 死路）。
    let (p4, _x, _y) = make_term_page();
    let pid4 = tree
        .cell(cell4)
        .unwrap()
        .tabs
        .add_page_icon_kind(
            Some("utilities-terminal-symbolic"),
            "终端四",
            PageKind::Terminal,
            p4,
            None,
        );
    assert!(
        tree.cell(cell4).unwrap().tabs.close_page(pid4),
        "关页签应当成功"
    );
    settle(200);
    // cell4 不是最后一个格子 ⇒ 它**会**被关掉。
    assert!(
        tree.cell(cell4).is_none(),
        "格子里的最后一个页签关掉 ⇒ 该格子应当自动关闭"
    );
    assert_eq!(tree.cell_count(), 1, "应当只剩一个格子");

    // ⚠️ **最后一个格子不关**：把 cell3 的最后一个页签关掉 ⇒ 格子**留着**
    //    （否则连接页空了 ⇒ 用户连「+」按钮都看不到 ⇒ 没法再开终端）。
    assert!(
        tree.cell(cell3).unwrap().tabs.close_page(pid2),
        "关页签应当成功"
    );
    settle(200);
    assert!(
        tree.cell(cell3).is_some(),
        "最后一个格子不该被关掉（否则用户没法再开终端）"
    );
    assert_eq!(tree.cell_count(), 1, "格子数不该变");
    println!("[7] 关掉最后一个页签 ⇒ 格子自动关闭（最后一个格子留着）✔");

    // ================= 8. 反向验证：内容 widget 只能有一个父 =================
    // ⚠️ **故意**犯这个错（不先摘就挂）⇒ GTK 会拒绝 ⇒ 断言"第二个 notebook
    //    并没有真的拿到这个 widget"。
    //    这条断言的意义：**证明"先 remove_page 再 add_page"不是多余的**。
    let nb_a = gtk::Notebook::new();
    let nb_b = gtk::Notebook::new();
    let w = gtk::Label::new(Some("x"));
    nb_a.append_page(&w, None::<&gtk::Widget>);
    let parent_before = w.parent();
    nb_b.append_page(&w, None::<&gtk::Widget>);
    assert!(
        parent_before.is_some(),
        "第一个 notebook 应当已经拿着它"
    );
    assert_eq!(
        w.parent(),
        parent_before,
        "GTK 竟然允许一个 widget 同时挂两个父 ⇒ 那就少了一次 remove_page"
    );
    println!("[8] 反向验证：一个 widget 不能同时挂两个父（不先摘就挂会被拒）✔");

    println!("layout_dnd_smoke: OK");
}
