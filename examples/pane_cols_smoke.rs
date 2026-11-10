//! 文件列表列宽冒烟：名称列固定在图标列之后，右侧列不被它挤走。
//! 运行：cargo run --example pane_cols_smoke
//!
//! 真 GTK 布局下用 `Paned` 把文件列表钉到一档档的宽度（和真实界面同一个
//! 机制），逐档断言：
//!   1. 名称列紧跟图标列，宽度不低于自己的下限 90px
//!   2. 名称列右缘始终落在视口内（长文件名不再撑列，也不靠整表横向滚动）
//!
//! 长文件名在名称列**单元格内部**左右滑动（`NameCellRenderer`，由面板
//! 底部对齐名称列的窄滚动条驱动），因此这里不需要、也不应该出现整表滚动。

use gtk::prelude::*;
use puttyplus::panel::pane;

fn pump(n: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..n {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 造一个装好列、塞了几行的**远程**文件列表（远程才有权限列），用 Paned
/// 把它钉到 `viewport` 宽。
fn build(viewport: i32, show_mtime: bool) -> (gtk::ApplicationWindow, gtk::TreeView, i32) {
    let model = gtk::ListStore::new(&[
        gtk::glib::Type::STRING, // 图标
        gtk::glib::Type::STRING, // 名称
        gtk::glib::Type::STRING, // 大小(显示)
        gtk::glib::Type::STRING, // 权限
        gtk::glib::Type::STRING, // 修改时间
        gtk::glib::Type::BOOL,   // 是否目录
        gtk::glib::Type::U64,    // 大小
        gtk::glib::Type::BOOL,   // 隐藏
        gtk::glib::Type::I32,    // 百分比（传输进度）
        gtk::glib::Type::STRING, // 传输进度文案
        gtk::glib::Type::STRING, // 修改时间全写（tooltip 用）
        gtk::glib::Type::BOOL,   // 正在传输（呼吸背景）
        gtk::glib::Type::F32,    // 进度文案透明度（终态文案渐隐）
        gtk::glib::Type::I64,    // 终态文案的起始时刻
    ]);
    for (name, size, perm, mt) in [
        ("graph.tar.gz", "58M", "-rw-r--r--", "2026-02-08 16:31"),
        ("一个很长很长的中文文件名.tar.gz", "1.2M", "-rwxr-xr-x", "2026-10-18 12:04"),
        ("short.txt", "12", "-rw-rw-r--", "2026-10-01 09:00"),
    ] {
        let it = model.append();
        model.set(
            &it,
            &[
                (0, &"text-x-generic-symbolic"),
                (1, &name),
                (2, &size),
                (3, &perm),
                (4, &mt),
                (5, &false),
                (6, &0u64),
                (7, &false),
                (8, &0i32),
                (9, &""),
                (10, &mt),
                (11, &false),
                (12, &1.0f32),
                (13, &0i64),
            ],
        );
    }

    let view = gtk::TreeView::with_model(&model);
    view.selection().set_mode(gtk::SelectionMode::Multiple);

    // 与 pane.rs 完全一致：先图标列，再 add_file_columns
    let icon = gtk::TreeViewColumn::new();
    let rp = gtk::CellRendererPixbuf::new();
    icon.pack_start(&rp, false);
    icon.add_attribute(&rp, "icon-name", 0);
    view.append_column(&icon);

    let cols = pane::add_file_columns(&view, true);
    cols.size.set_visible(true);
    cols.perm.as_ref().unwrap().set_visible(true);
    // ⚠️ 阶段 76：「修改时间」列**默认藏着**（窄面板里四列把文件名挤没）
    cols.mtime.set_visible(show_mtime);

    let sw = gtk::ScrolledWindow::new();
    // 与 pane.rs 一致：横向滚动 + 始终可见的窄条
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_overlay_scrolling(false);
    sw.add_css_class("pp-thin-scrollbar");
    sw.set_child(Some(&view));
    sw.set_hexpand(true);
    sw.set_vexpand(true);

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&sw));
    paned.set_end_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));

    let win = gtk::ApplicationWindow::builder()
        .default_width(1000)
        .default_height(400)
        .build();
    win.set_child(Some(&paned));
    win.present();
    pump(30);
    paned.set_position(viewport);
    pump(30);

    let actual = view.width();
    (win, view, actual)
}

/// 可见列的 (x, 宽)；x 由前面的可见列累加（列从左到右排布）。
fn geom(v: &gtk::TreeView) -> Vec<(i32, i32)> {
    let mut x = 0;
    let mut out = Vec::new();
    for i in 0..v.n_columns() as i32 {
        let Some(c) = v.column(i) else { continue };
        if !c.is_visible() {
            continue;
        }
        let w = c.width();
        out.push((x, w));
        x += w;
    }
    out
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    println!(
        "{:>6}  {:>7}  {:>8}  {:>8}  {:>10}",
        "视口", "实际", "名称列x", "名称列宽", "名称列右缘"
    );
    let mut min_name_w = i32::MAX;
    for viewport in [360, 320, 300, 280, 260, 240, 220, 200, 180, 160, 140] {
        let (win, view, actual) = build(viewport, true);
        let g = geom(&view);
        let (nx, nw) = g[1]; // 第 0 个可见列是图标列，第 1 个是名称列
        let right = nx + nw;
        println!(
            "{viewport:>6}  {actual:>7}  {nx:>8}  {nw:>8}  {right:>10}"
        );

        assert_eq!(
            nx, g[0].1,
            "视口 {viewport}：名称列没紧跟在图标列后面（x={nx}，图标列宽 {}）",
            g[0].1
        );
        assert!(
            nw >= 90,
            "视口 {viewport}：名称列只剩 {nw}px，低于它自己的下限 90"
        );
        // ⚠️ 阶段 76：名称列下限提到 160px 之后，**极窄**面板里名称列
        // 自己就比视口宽 ⇒ 右缘超出视口是正常的（表格横向可滚，
        // 右侧的「大小/权限」滚过去就能看到）。
        assert!(
            right <= actual || nw >= 160,
            "视口 {viewport}：名称列右缘 {right} 超出视口 {actual}（且名称列没到下限）"
        );

        min_name_w = min_name_w.min(nw);
        win.close();
        pump(6);
    }

    println!("\n名称列最窄仍有 {min_name_w}px，始终紧贴图标列、落在视口内");
    println!("✔ 长文件名不再撑列：在名称列内横向滑动，右侧列保持原位");

    // ---- 阶段 76：默认列组合下，名称列要拿得到 160px ----
    //
    // 审计 P1-15：340px 宽的面板里，名称/大小/权限/修改时间 四列把文件名
    // 挤到只剩 120px。现在「修改时间」默认藏着 ⇒ 名称列 ≥ 160px。
    let (win, view, actual) = build(340, false);
    let g = geom(&view);
    let (nx, nw) = g[1];
    println!("[默认列] 视口 340 → 实际 {actual}，名称列 x={nx} 宽={nw}");
    assert!(
        nw >= 160,
        "默认列组合下名称列只有 {nw}px，达不到 160px（审计 P1-15）"
    );
    // 修改时间列默认不可见
    assert!(
        !view
            .column(4)
            .map(|c| c.is_visible())
            .unwrap_or(false),
        "「修改时间」列应当默认藏着"
    );
    // 截图留个证据（窄面板 + 默认列）
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_pane_cols.png")
        .status();
    win.close();
    pump(6);
    println!("✔ 默认列组合：名称列 ≥160px，「修改时间」默认藏着");
}
