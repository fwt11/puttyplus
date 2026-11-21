//! 阶段 142 冒烟：**界面密度三档的像素级递减**。
//!
//! **为什么必须测实际像素**：单测能验"CSS 文本里写了这条规则"，
//! 但**写了不等于生效**——GTK 里 CSS 优先级、`min-height` 与
//! `padding` 的区别、内容高度对 `min-height` 的钳制…
//! 中间隔着好几环。用户看到的是像素，不是 CSS 字符串。
//!
//! 尤其要注意：这一轮把**代码层写死的留白迁到了 CSS**（`set_margin_*`
//! ⇒ `.pp-form-box` 之类）—— 迁之前，CSS 压不动它们（优先级低于
//! widget 的 inline style），迁之后才压得动。这个冒烟就是钉住
//! "迁完之后，三档确实是**越来越小**"。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example density_smoke

use gtk::prelude::*;
use puttyplus::density::{self, Density};
use puttyplus::panel::namecell::NameCellRenderer;

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpDensitySmoke")
        .build();
    app.connect_activate(|app| {
        check(app);
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}

/// 泵事件，**等帧**。
///
/// ⚠️ **必须等帧**（阶段 142 实测踩坑）：GTK 只在**帧时钟 tick** 时
/// 重算布局与测量 ⇒ 只 `while ctx.pending()` 空转的话**一帧都没跑**，
/// 样式改了也不会体现在 `measure()` 上。
/// ⇒ 之前这个冒烟一直报"CSS 没生效"，查下来是**没等到重算**，
/// 不是 CSS 没加载（真实机制是好的）。
fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 自然高度（第二个返回值 = natural；第一个是 minimum）。
fn natural_h(w: &impl IsA<gtk::Widget>) -> i32 {
    let (_, nat, _, _) = w.measure(gtk::Orientation::Vertical, -1);
    nat
}

/// 一棵**只有名称列**的树（行高由自绘的名称渲染器决定 ⇒ 与真面板一致）。
fn make_tree(rows: usize) -> gtk::TreeView {
    let model = gtk::ListStore::new(&[glib::Type::STRING]);
    for i in 0..rows {
        let iter = model.append();
        model.set_value(&iter, 0, &format!("文件 {i}").into());
    }
    let view = gtk::TreeView::new();
    view.set_model(Some(&model));
    let col = gtk::TreeViewColumn::new();
    col.set_title("名称");
    let r = NameCellRenderer::new();
    col.pack_start(&r, true);
    col.set_cell_data_func(&r, |_col, cell, model, iter| {
        if let Some(r) = cell.downcast_ref::<NameCellRenderer>() {
            r.set_text(model.get::<String>(iter, 0));
        }
    });
    view.append_column(&col);
    view.set_vexpand(false);
    view
}

/// 一档密度的三组测量值。
///
/// 返回 (按钮自然高, 文件行高, 顶栏自然高)。
#[derive(Debug, Clone, Copy)]
struct Sizes {
    button: i32,
    row: i32,
    bar: i32,
}

fn measure(
    d: Density,
    btn: &gtk::Button,
    one: &gtk::TreeView,
    three: &gtk::TreeView,
    bar: &gtk::HeaderBar,
) -> Sizes {
    // 走**真实**的密度入口：它会给所有顶层窗口换 CSS 类，
    // 并催所有树重排行高（行高是代码层的，CSS 压不动）。
    density::apply_density(d);
    settle(400);
    // 行高 =（3 行的树高 − 1 行的树高）÷ 2（扣掉列头）
    let row = (natural_h(three) - natural_h(one)) / 2;
    Sizes {
        button: natural_h(btn),
        row,
        bar: natural_h(bar),
    }
}

fn check(app: &gtk::Application) {
    density::install();

    // ⚠️ **窗口必须挂在同一个 Application 上**（GTK4 的硬要求：
    //    「New application windows must be added after the GApplication::startup
    //    signal has been emitted」⇒ 自己再造一个 Application 会炸）。
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(800)
        .default_height(600)
        .build();

    let header = gtk::HeaderBar::new();
    let btn = gtk::Button::with_label("按钮");
    header.pack_start(&btn);
    win.set_titlebar(Some(&header));

    // ⚠️ **树必须在窗口里**（阶段 142 实测踩坑）：密度类挂在**窗口**上，
    //    CSS 靠后代选择器（`.pp-compact treeview.view header button`）生效
    //    ⇒ 量一个游离在窗口外的树，永远量不到紧凑效果。
    let one = make_tree(1);
    let three = make_tree(3);
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.append(&one);
    holder.append(&three);
    win.set_child(Some(&holder));

    win.present();
    settle(600);

    let normal = measure(Density::Normal, &btn, &one, &three, &header);
    let compact = measure(Density::Compact, &btn, &one, &three, &header);
    let xcompact = measure(Density::ExtraCompact, &btn, &one, &three, &header);

    println!("标准  ：{normal:?}（按钮, 文件行高, 顶栏）");
    println!("紧凑  ：{compact:?}");
    println!("超紧凑：{xcompact:?}");

    // ---- 按钮：逐档变矮 ----
    assert!(
        compact.button < normal.button,
        "紧凑档的按钮应变矮：标准 {normal:?} vs 紧凑 {compact:?}"
    );
    assert!(
        xcompact.button < compact.button,
        "超紧凑档的按钮应更矮：紧凑 {compact:?} vs 超紧凑 {xcompact:?}"
    );

    // ---- 文件列表行高：逐档变矮 ----
    // ⚠️ 这条是**这一轮的核心**：行高在 GTK 4.18 里**只能走代码层**
    //    （CSS 的 `treeview.view cell` 节点压根不存在）。
    assert!(
        compact.row < normal.row,
        "紧凑档的文件列表行高应变矮：标准 {normal:?} vs 紧凑 {compact:?}"
    );
    assert!(
        xcompact.row < compact.row,
        "超紧凑档的文件列表行高应更矮：紧凑 {compact:?} vs 超紧凑 {xcompact:?}"
    );

    // ---- 顶栏：逐档变矮 ----
    assert!(
        compact.bar < normal.bar,
        "紧凑档的顶栏应变矮：标准 {normal:?} vs 紧凑 {compact:?}"
    );
    assert!(
        xcompact.bar < compact.bar,
        "超紧凑档的顶栏应更矮：紧凑 {compact:?} vs 超紧凑 {xcompact:?}"
    );

    println!("density_smoke: OK");
}
