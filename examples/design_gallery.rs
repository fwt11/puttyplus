//! 设计令牌 v2 的**活画廊**（美化·地基 5/5）：把原语类全摆一窗，明暗各截一张。
//!
//! 干什么用：
//! 1. 调样式时的**对照图**——改 `theme.rs` 前后各跑一遍，肉眼比差异；
//! 2. 样式回归基准——原语类丢了（被人删了 CSS）时这里一眼看出来；
//! 3. 新界面的**选型表**——要用药丸/卡片/键帽时先看这里长什么样。
//!
//! 摆的内容（与 `theme.rs` 里令牌 v2 的原语一一对应）：
//! 按钮层级 / 徽章（chip）与状态药丸（pill）/ 键值卡片（pp-card）/
//! 键帽（pp-kbd）/ 分段控件（pp-segmented）/ 样例表格（药丸列 +
//! 等宽 ID + 右对齐数字——容器表那一套的缩小版）。
//!
//! ```text
//! xvfb-run -a cargo run --example design_gallery
//! # 截图：/tmp/pp-gallery-light.png、/tmp/pp-gallery-dark.png
//! ```

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use puttyplus::panel::pillcell::{PillCellRenderer, PillState};
use puttyplus::theme::{self, Theme};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("截图：{path}");
}

/// 一个小节：返回**摆控件的横排**（标题自己挂进 root）。
fn section(root: &gtk::Box, title: &str) -> gtk::Box {
    let h = gtk::Label::new(Some(title));
    h.add_css_class("pp-section");
    h.set_xalign(0.0);
    root.append(&h);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Start);
    root.append(&row);
    row
}

/// 一张**键值卡片**（跟 dockerui 的 mk_kv_card 同款：pp-card + 键值 Grid）。
fn kv_card(title: &str, rows: &[(&str, &str)]) -> gtk::Box {
    let head = gtk::Label::new(Some(title));
    head.set_xalign(0.0);
    head.add_css_class("heading");
    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(4);
    grid.set_hexpand(true);
    for (i, (k, v)) in rows.iter().enumerate() {
        let kl = gtk::Label::new(Some(k));
        kl.set_xalign(0.0);
        kl.add_css_class("dim-label");
        let vl = gtk::Label::new(Some(v));
        vl.set_xalign(0.0);
        vl.set_hexpand(true);
        grid.attach(&kl, 0, i as i32, 1, 1);
        grid.attach(&vl, 1, i as i32, 1, 1);
    }
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.add_css_class("pp-card");
    b.append(&head);
    b.append(&grid);
    b
}

/// 样例表格：**药丸状态列 + 等宽 ID + 右对齐数字**（容器表那一套的缩小版）。
fn sample_table() -> gtk::Widget {
    let model = gtk::ListStore::new(&[
        gtk::glib::Type::STRING, // 名称
        gtk::glib::Type::STRING, // ID
        gtk::glib::Type::STRING, // 数字
        gtk::glib::Type::STRING, // 状态（docker 原文）
    ]);
    for (name, id, num, state) in [
        ("langfuse-postgres", "34548bd9147c", "1.81%", "running"),
        ("eng-tut-server", "200f9d6b59be", "0.00%", "exited"),
        ("cache-redis", "9fe20c41a033", "0.42%", "paused"),
        ("dead-worker", "5e11cf02ab77", "0.00%", "dead"),
        ("pg-compose", "", "2.23%", "1/2 运行中"),
    ] {
        let it = model.append();
        model.set(&it, &[
            (0u32, &name.to_string()),
            (1u32, &id.to_string()),
            (2u32, &num.to_string()),
            (3u32, &state.to_string()),
        ]);
    }
    let view = gtk::TreeView::with_model(&model);
    let titles = ["名称", "ID", "CPU", "状态"];
    for (i, t) in titles.iter().enumerate() {
        let col = gtk::TreeViewColumn::new();
        col.set_title(t);
        if i == 3 {
            // 状态列：药丸渲染器（跟容器表同一条路：cell_data_func 现喂数据）
            let pr = PillCellRenderer::new();
            col.pack_start(&pr, false);
            col.set_cell_data_func(&pr, move |_col, cell, m, iter| {
                let raw: String = m.get(iter, 3);
                let p = cell
                    .downcast_ref::<PillCellRenderer>()
                    .expect("渲染器必须是 PillCellRenderer");
                let st = match raw.as_str() {
                    "running" => PillState::Ok,
                    "paused" => PillState::Warn,
                    "dead" => PillState::Err,
                    _ => PillState::Dim,
                };
                p.set_text(raw);
                p.set_state(st);
            });
        } else {
            let r = gtk::CellRendererText::new();
            if i == 1 {
                r.set_family(Some("monospace"));
            }
            if i == 2 {
                gtk::prelude::CellRendererExt::set_alignment(&r, 1.0, 0.5);
                col.set_alignment(1.0);
            }
            col.pack_start(&r, false);
            col.add_attribute(&r, "text", i as i32);
        }
        col.set_min_width(if i == 0 { 170 } else { 120 });
        view.append_column(&col);
    }
    view.upcast::<gtk::Widget>()
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 画廊本体（建一次，明暗两轮共用：apply 换 provider ⇒ 控件自动跟）----
    theme::apply(Theme::Light);
    let win = gtk::Window::new();
    win.set_title(Some("设计令牌 v2 画廊"));
    win.set_default_size(900, 660);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(20);
    root.set_margin_end(20);
    scroll.set_child(Some(&root));
    win.set_child(Some(&scroll));

    // 1) 按钮层级
    let row = section(&root, "按钮");
    row.append(&gtk::Button::with_label("普通按钮"));
    let primary = gtk::Button::with_label("主操作");
    primary.add_css_class("pp-primary");
    row.append(&primary);
    let flat = gtk::Button::with_label("扁平按钮");
    flat.add_css_class("flat");
    row.append(&flat);
    let icon = puttyplus::a11y::icon_button("view-refresh-symbolic");
    icon.set_tooltip_text(Some("图标按钮（tooltip 必须有）"));
    row.append(&icon);

    // 2) 徽章（chip）与状态药丸（pill）
    let row = section(&root, "徽章与状态药丸");
    let mk = |cls: &str, label: &str| -> gtk::Label {
        let l = gtk::Label::new(Some(label));
        for c in cls.split_whitespace() {
            l.add_css_class(c);
        }
        l
    };
    row.append(&mk("pp-chip", "计数 7"));
    row.append(&mk("pp-chip pp-chip-ok", "已启用"));
    row.append(&mk("pp-chip pp-chip-warn", "已暂停"));
    row.append(&mk("pp-chip pp-chip-err", "错误"));
    for (cls, label) in [
        ("pp-pill-ok", "运行中"),
        ("pp-pill-warn", "已暂停"),
        ("pp-pill-err", "已失效"),
        ("pp-pill-dim", "已停止"),
    ] {
        row.append(&mk(&format!("pp-pill {cls}"), label));
    }

    // 3) 卡片（暗色三层：窗底 < 面板 < 卡片，看这张卡就知道分层对没对）
    let _ = section(&root, "卡片（暗色下看三层：窗底 / 面板 / 卡片）");
    // ---- 阶段 175 / UI-08：表面层级 ＋ 输入面 ＋ 选中面 ----
    //
    // ⚠️ **为什么加这一块**：以前画廊只有「卡片」，看不到**输入面**与
    //   **选中面** ⇒ 那两层令牌加了也没人看得见 ⇒ 等于没加。
    {
        let row = section(&root, "表面层级（窗底 / 面板 / 卡片 / 输入 / 选中）");
        // 窗底（不画东西，只是这块区域的底）
        let bg = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bg.add_css_class("pp-surface-bg");
        bg.set_size_request(120, 26);
        row.append(&bg);
        // 面板面
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.add_css_class("pp-surface-panel");
        panel.set_size_request(120, 26);
        row.append(&panel);
        // 卡片面
        let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
        card.add_css_class("pp-card");
        card.set_size_request(120, 26);
        row.append(&card);
        // 输入面（真输入框 ⇒ 看的就是 `pp_input`）
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some("输入面"));
        entry.set_size_request(120, -1);
        row.append(&entry);
        // 选中面（真选中的一行 ⇒ 看的就是 `pp_selected`）
        let list = gtk::ListBox::new();
        let r = gtk::ListBoxRow::new();
        r.set_child(Some(&gtk::Label::new(Some("选中面"))));
        list.append(&r);
        list.select_row(Some(&r));
        list.set_size_request(120, -1);
        row.append(&list);
    }
    let cards = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    cards.set_hexpand(true);
    cards.append(&kv_card(
        "运行配置",
        &[("镜像", "postgis/postgis:17"), ("重启策略", "always")],
    ));
    cards.append(&kv_card(
        "网络与端口",
        &[("发布端口", "127.0.0.1:5432→5432"), ("网络", "bridge")],
    ));
    root.append(&cards);

    // 4) 键帽与等宽
    let row = section(&root, "键帽与等宽");
    for key in ["Ctrl+Shift+T", "Ctrl+F", "F1"] {
        let l = gtk::Label::new(Some(key));
        l.add_css_class("pp-kbd");
        row.append(&l);
    }
    let mono = gtk::Label::new(Some("200f9d6b59be · 127.0.0.1:5432"));
    mono.add_css_class("pp-mono");
    row.append(&mono);

    // 5) 分段控件（互斥的 ToggleButton 组）
    let row = section(&root, "分段控件");
    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg.add_css_class("pp-segmented");
    let mut group_head: Option<gtk::ToggleButton> = None;
    for (i, name) in ["全部", "运行中", "已停止"].iter().enumerate() {
        let b = gtk::ToggleButton::with_label(name);
        if i == 0 {
            b.set_active(true);
            group_head = Some(b.clone());
        } else if let Some(h) = &group_head {
            b.set_group(Some(h));
        }
        seg.append(&b);
    }
    row.append(&seg);

    // 6) 样例表格
    let _ = section(&root, "表格（药丸列 · 等宽 ID · 右对齐数字）");
    root.append(&sample_table());

    win.present();
    pump(500);

    // ---- 三轮截图：Light → Dark → HighContrast（apply 换 provider，控件自动跟）----
    // 美化·二批 8：高对比也进画廊——令牌 v2 的原语必须**三套主题都站得住**。
    for (t, name) in [
        (Theme::Light, "light"),
        (Theme::Dark, "dark"),
        (Theme::HighContrast, "contrast"),
    ] {
        assert!(theme::apply(t), "{t:?} 主题的 CSS 没装上");
        pump(400);
        shot(&format!("/tmp/pp-gallery-{name}.png"));
    }

    println!("design_gallery: OK");
    // 截完就撤（不留挂着的窗口；ui_shot 那种跑不死的是给人眼手动看的）
    std::process::exit(0);
}
