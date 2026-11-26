//! 传输队列窗口（对应 C 版 queue_window_ensure / xfer_flush_idle / qwin_*）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::util::{human_size, path_basename, path_parent};

use super::worker::{XferKind, XferShared, XferState};
use super::PanelState;
use crate::i18n::tr;
use crate::trf;

pub const QCOL_DIR: i32 = 0;
pub const QCOL_FILE: i32 = 1;
pub const QCOL_PCT: i32 = 2;
pub const QCOL_SIZE: i32 = 3;
pub const QCOL_SPEED: i32 = 4;
pub const QCOL_STATE: i32 = 5;
pub const QCOL_ID: i32 = 6;
/// 状态列前景色（失败 = 红，其余 = NULL 走主题色）
pub const QCOL_FAIL: i32 = 7;
/// **这一行正在传**（上传或下载）⇒ 方向列用 accent 色（随呼吸变亮/变暗）
pub const QCOL_ACTIVE: i32 = 8;
/// **状态码列**（阶段 175 / UI-11）：给筛选用的**机器可读**状态。
///
/// ⚠️ **为什么不能靠状态列的文字筛**：那一列在失败时放的是**错误原文**
///   （比如「权限不足」）⇒ 拿文字判断"是不是失败"不可靠 ⇒ 加一列整数
///   状态码，筛选只认它。
pub const QCOL_CODE: i32 = 9;

/// 状态码（`QCOL_CODE` 的取值）。
pub const ST_WAIT: i32 = 0;
pub const ST_RUN: i32 = 1;
pub const ST_DONE: i32 = 2;
pub const ST_SKIP: i32 = 3;
pub const ST_ERR: i32 = 4;
pub const ST_CANCEL: i32 = 5;

/// 筛选档位（阶段 175 / UI-11）。
pub const FILTER_ALL: u8 = 0;
pub const FILTER_RUNNING: u8 = 1;
pub const FILTER_FAILED: u8 = 2;

/// 当前筛选档（**进程级**：队列窗口只有一个）。
thread_local! {
    static FILTER_MODE: std::cell::Cell<u8> = const { std::cell::Cell::new(FILTER_ALL) };
}

/// 当前筛选档（读）。
pub fn filter_mode() -> u8 {
    FILTER_MODE.with(|c| c.get())
}

/// 切到某个筛选档（**只改可见性，不动顺序** ⇒ 正在传的行不会被挪走）。
///
/// ⚠️ **顺序稳定**：筛选是 `GtkTreeModelFilter` 的可见性 ⇒ 底层模型顺序
///   一行没动 ⇒ 清除筛选后**原顺序原样回来**（审核验收）。
pub fn set_filter_mode(st: &Rc<PanelState>, mode: u8) {
    FILTER_MODE.with(|c| c.set(mode));
    let slot = st.queue_ui.borrow();
    let Some(qu) = slot.as_ref() else { return };
    qu.filter.refilter();
}

/// 视图里选中的行 → **模型行**（阶段 175 / UI-11）。
///
/// ⚠️ **视图显示的是筛选模型**（`GtkTreeModelFilter`）⇒ 视图里的行号与
///   模型行号**不一定一样** ⇒ 拿视图行号直接去 `model.iter()` 会读到
///   **别的行**（一筛选就明显）⇒ 一律先换回模型路径。
fn selected_model_paths(qu: &QueueUi) -> Vec<gtk::TreePath> {
    let (rows, _) = qu.view.selection().selected_rows();
    rows.into_iter()
        .filter_map(|p| qu.filter.convert_path_to_child_path(&p))
        .collect()
}

/// 队列顶部的**只读摘要**（阶段 175 / UI-11）。
///
/// ⚠️ **只读**：这里**只说清楚**有多少成多少败，不让人改（改设置的地方
///   只留一处）。
pub struct QueueSummary {
    /// 完成。
    pub done: gtk::Label,
    /// 失败。
    pub failed: gtk::Label,
    /// 跳过。
    pub skipped: gtk::Label,
    /// 取消。
    pub canceled: gtk::Label,
}

impl QueueSummary {
    /// 按四个计数刷新文案。
    pub fn set(&self, done: usize, failed: usize, skipped: usize, canceled: usize) {
        self.done.set_text(&trf!("完成 {n}", n = done));
        self.failed.set_text(&trf!("失败 {n}", n = failed));
        self.skipped.set_text(&trf!("跳过 {n}", n = skipped));
        self.canceled.set_text(&trf!("取消 {n}", n = canceled));
    }
}

/// 一个传输项在 UI 侧的记录。
pub struct QueueItem {
    pub id: u64,
    pub src: String,
    pub dst_dir: String,
    pub upload: bool,
    pub kind: XferKind,
    pub shared: std::sync::Arc<std::sync::Mutex<XferShared>>,
}

pub struct QueueUi {
    pub win: gtk::Window,
    pub view: gtk::TreeView,
    pub model: gtk::ListStore,
    /// **筛选模型**（阶段 175 / UI-11）：视图显示的是它，不是 `model`。
    ///
    /// ⚠️ **为什么**：筛选只改「哪些行可见」，**不动顺序** ⇒ 正在传的
    ///   行不会被重排（审核验收：「不自动重排用户正在传输的行」）。
    pub filter: gtk::TreeModelFilter,
    /// 顶部的只读摘要。
    pub summary: Option<QueueSummary>,
    /// id → 行引用（模型行删除后自动失效）
    pub rows: RefCell<HashMap<u64, gtk::TreeRowReference>>,
}

/// 懒创建队列窗口。
/// 传输设置的**只读摘要**（队列窗口顶部用）。
///
/// ⚠️ 真正的设置在**首选项中心 →「传输」页**（阶段 69 / 审计 P1-4）——
/// 这里只把当前值说清楚，不让用户在这儿改（改设置的地方只留一处）。
pub fn xfer_summary() -> String {
    trf!("断点续传 {v0}　校验 SHA-1 {v1}　并发 {v2}", v0 = if crate::settings::xfer_resume() { tr("开") } else { tr("关") }, v1 = if crate::settings::xfer_checksum() { tr("开") } else { tr("关") }, v2 = crate::settings::xfer_parallel())
}

pub fn ensure(st: &Rc<PanelState>) {
    if st.queue_ui.borrow().is_some() {
        return;
    }
    let win = gtk::Window::new();
    win.set_title(Some(tr("传输队列 — PuTTYPlus")));
    let (qw, qh) = crate::settings::window_size("queue").unwrap_or((720, 320));
    win.set_default_size(qw, qh);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);

    let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let bc = crate::a11y::icon_button("process-stop-symbolic");
    bc.set_tooltip_text(Some(tr("取消选中的传输")));
    bc.add_css_class("flat");
    {
        let st2 = Rc::clone(st);
        bc.connect_clicked(move |_| cancel_selected(&st2));
    }
    let bcl = crate::a11y::icon_button("edit-clear-all-symbolic");
    bcl.set_tooltip_text(Some(tr("清除已完成/已取消/失败的项")));
    bcl.add_css_class("flat");
    {
        let st2 = Rc::clone(st);
        bcl.connect_clicked(move |_| clear_finished(&st2));
    }
    bbox.append(&bc);
    bbox.append(&bcl);

    // 3.5 传输选项：⚠️ 阶段 69（审计 P1-4）**搬走了**——断点续传 / SHA-1
    // 校验 / 并发数现在住在**首选项中心 →「传输」页**。
    // 队列窗口顶部改成**只读摘要 + 「去设置」链接**：改设置的地方只有一处。
    let sp = gtk::Separator::new(gtk::Orientation::Vertical);
    bbox.append(&sp);
    let summary = gtk::Label::new(Some(&xfer_summary()));
    summary.set_xalign(0.0);
    summary.add_css_class("dim-label");
    summary.add_css_class("pp-hint");
    summary.set_tooltip_text(Some(
        tr("这些是全局设置，对之后新建的传输任务生效。\n\
         要改点「去设置」——设置只在首选项中心里改一处。"),
    ));
    bbox.append(&summary);
    let b_set = gtk::Button::with_label(tr("去设置…"));
    b_set.set_tooltip_text(Some(tr("打开首选项中心的「传输」页")));
    b_set.connect_clicked(|_| {
        crate::settingsui::open(crate::settingsui::Page::Transfer);
    });
    bbox.append(&b_set);

    vbox.append(&bbox);

    // ---- 摘要条 ＋ 筛选（阶段 175 / UI-11）----
    //
    // ⚠️ **摘要是只读的**：只说清楚「多少成、多少败」，不让人在这儿改
    //   （改设置的地方只留一处）。
    let summary = {
        let srow = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        srow.set_margin_top(crate::theme::space::XS);
        let mut mk = |text: &str, cls: &str| {
            let l = gtk::Label::new(Some(text));
            l.add_css_class(cls);
            l.set_xalign(0.0);
            srow.append(&l);
            l
        };
        let done = mk(&trf!("完成 {n}", n = 0), "pp-pill-ok");
        let failed = mk(&trf!("失败 {n}", n = 0), "pp-pill-err");
        let skipped = mk(&trf!("跳过 {n}", n = 0), "pp-pill-dim");
        let canceled = mk(&trf!("取消 {n}", n = 0), "pp-pill-dim");
        let summary = QueueSummary {
            done,
            failed,
            skipped,
            canceled,
        };
        // ---- 筛选：全部 / 进行中 / 失败 ----
        // ⚠️ **一排 ToggleButton 当分段控件用**（同一个 group ⇒ 互斥）。
        let frow = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        frow.set_halign(gtk::Align::End);
        frow.set_hexpand(true);
        let mut group: Option<gtk::ToggleButton> = None;
        for (i, (label, tip)) in [
            (tr("全部"), tr("显示所有传输项")),
            (tr("进行中"), tr("只看还在排队或正在传的")),
            (tr("失败"), tr("只看失败的")),
        ]
        .iter()
        .enumerate()
        {
            let b = gtk::ToggleButton::with_label(label);
            b.set_tooltip_text(Some(tip));
            b.add_css_class("flat");
            if i == 0 {
                b.set_active(true);
            }
            if let Some(g) = &group {
                b.set_group(Some(g));
            } else {
                group = Some(b.clone());
            }
            let st2 = Rc::clone(st);
            let mode = match i {
                1 => FILTER_RUNNING,
                2 => FILTER_FAILED,
                _ => FILTER_ALL,
            };
            b.connect_toggled(move |b| {
                if b.is_active() {
                    set_filter_mode(&st2, mode);
                }
            });
            frow.append(&b);
        }
        srow.append(&frow);
        vbox.append(&srow);
        summary
    };

    let model = gtk::ListStore::new(&[
        glib::Type::STRING, // DIR
        glib::Type::STRING, // FILE
        glib::Type::I32,    // PCT
        glib::Type::STRING, // SIZE
        glib::Type::STRING, // SPEED
        glib::Type::STRING, // STATE
        glib::Type::U64,    // ID
        glib::Type::STRING, // 失败态前景色（空 = 用主题默认色）
        glib::Type::BOOL,   // 正在传（方向列用 accent 色 + 呼吸）
        glib::Type::I32,    // 状态码（筛选用，阶段 175 / UI-11）
    ]);

    // ⚠️ **阶段 175 / UI-11：筛选模型**。
    //   用 `GtkTreeModelFilter`（**只改可见性、不动顺序**）⇒ 正在传的
    //   行不会被重排（审核验收明写这条）；清除筛选后原顺序原样回来。
    let filter = gtk::TreeModelFilter::new(&model, None);
    filter.set_visible_func(|m, it| {
        let code: i32 = m.get(it, QCOL_CODE);
        match FILTER_MODE.with(|c| c.get()) {
            FILTER_RUNNING => code == ST_WAIT || code == ST_RUN,
            FILTER_FAILED => code == ST_ERR,
            _ => true,
        }
    });

    let view = gtk::TreeView::with_model(&filter);
    let cols: [(&str, bool, bool); 5] = [
        (tr("方向"), false, false),
        (tr("文件"), true, false),
        (tr("进度"), false, true),
        (tr("大小"), false, false),
        (tr("速度"), false, false),
    ];
    for (i, (title, expand, progress)) in cols.iter().enumerate() {
        let col = gtk::TreeViewColumn::new();
        col.set_title(title);
        col.set_resizable(true);
        if *expand {
            col.set_expand(true);
            col.set_min_width(120);
        }
        if i == 0 {
            col.set_min_width(84); // 「⬆ 上传」完整显示
        }
        let r: gtk::CellRenderer = if *progress {
            gtk::CellRendererProgress::new().upcast()
        } else {
            gtk::CellRendererText::new().upcast()
        };
        // renderer 必须 pack 进列才会渲染（此前漏掉导致除状态列外全空白）
        col.pack_start(&r, true);
        if *progress {
            col.add_attribute(&r, "value", QCOL_PCT);
        } else if i == 0 {
            // **方向列**：正在传的行用 accent 色（随呼吸变亮/变暗）
            // ⇒ 任务列表里一眼看出"现在在传哪几个"（用户 2026-11-10：
            // 「任务列表等图标也可以有一些动态效果」）。
            //
            // ⚠️ 颜色从**视图**的样式上下文取（跟着主题走）⇒ 闭包抓一份
            //    视图的克隆（视图是控件树里的强引用，不会成环）。
            let view_for_color = view.clone();
            col.set_cell_data_func(&r, move |_col, cell, model, iter| {
                let Some(rt) = cell.downcast_ref::<gtk::CellRendererText>() else {
                    return;
                };
                let dir: String = model.get(iter, QCOL_DIR);
                rt.set_text(Some(&dir));
                let active: bool = model.get(iter, QCOL_ACTIVE);
                if active {
                    let mut c = super::pulse::accent(&view_for_color);
                    c.set_alpha(super::pulse::alpha(0.55, 1.0));
                    // ⚠️ GTK4 的 CellRendererText「前景色」是**颜色字符串**
                    //    （不是 RGBA）⇒ 用 rgba() 的 CSS 写法（带透明度）。
                    rt.set_foreground(Some(&c.to_string()));
                } else {
                    // ⚠️ **要显式清掉**：否则上一行留下的 accent 色会粘着
                    rt.set_foreground(None);
                }
            });
        } else {
            col.add_attribute(&r, "text", i as i32);
        }
        view.append_column(&col);
    }
    let col_state = gtk::TreeViewColumn::new();
    col_state.set_title(tr("状态"));
    let rs = gtk::CellRendererText::new();
    col_state.pack_start(&rs, true);
    col_state.add_attribute(&rs, "text", QCOL_STATE);
    // 4.3 失败醒目态：状态列染红。失败项在列表里一眼能挑出来，
    // 不用逐行读到"Err: xxx"才知道出了事
    col_state.add_attribute(&rs, "foreground", QCOL_FAIL);
    view.append_column(&col_state);

    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_child(Some(&view));
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    vbox.append(&sw);
    win.set_child(Some(&vbox));

    // 关闭即隐藏（对应 C 版 qwin_on_delete）
    win.connect_close_request(|w| {
        crate::settings::save_window_size("queue", w);
        w.hide();
        glib::Propagation::Stop
    });

    install_context_menu(st, &view);

    // 4.3：汇总窗口要挂在队列窗口上（notify.rs 反查它）
    crate::notify::set_queue_win(win.clone());

    st.queue_ui.replace(Some(QueueUi {
        win,
        view,
        model,
        filter,
        summary: Some(summary),
        rows: RefCell::new(HashMap::new()),
    }));
}

/// 刷新文件面板工具栏上「传输队列」按钮的忙态与数量角标（阶段 102）。
///
/// ⚠️ **什么时候调**：队列每次刷新（`flush`）都调 ⇒ 数量自己跟着变，
/// 不用各处手动同步（入队 / 传完 / 取消都会走到 flush）。
///
/// ⚠️ **别在 `st.items` 借用的期间调它**：它要读队列条目算数量 ⇒
/// `flush` 里必须先 `drop(items)` 再调（否则 `RefCell` 双重借用 panic）。
pub fn refresh_btn(st: &Rc<PanelState>) {
    let Some(b) = st.queue_btn.borrow().clone() else { return };
    let running = super::xfer_running(st);
    let n = running.total();
    // ⚠️ **转圈 = 「还有东西在传」**（用户 2026-11-11 要的动态效果）
    //    ⇒ 队列每次刷新都调这里 ⇒ 数量与转圈自己就同步了。
    set_btn_spinning(st, n > 0);
    // 角标数字（0 ⇒ 数字清空、蓝圆不画）。
    set_badge(st, n);

    if n > 0 {
        // tooltip 说清方向：「上传中 2 个」比「2 个进行中」有用
        let what = running.label().unwrap_or_default();
        b.set_tooltip_text(Some(&trf!("传输队列（{x}）", x = what)));
        // ⚠️ **忙态必须去掉 `flat`**：Adwaita 的 `button.flat` 是
        // `background-color: transparent`，优先级比自定义类高 —— 底色没了
        // 就还是那个灰图标（`.pp-chip` 踩过同一个坑，见 theme.rs）。
        b.remove_css_class("flat");
        b.add_css_class("pp-xfer-busy");
    } else {
        b.set_tooltip_text(Some(tr("传输队列")));
        b.remove_css_class("pp-xfer-busy");
        b.add_css_class("flat");
    }

    // 状态行上的「传输中 N 个」也跟着变（数量变了要重画）
    super::render_status(st);
}

/// 设角标上的数字（0 = 没有东西在传 ⇒ 数字清空、蓝圆不画）。
///
/// ⚠️ **数字和蓝圆一起管**：只清数字不画圆 ⇒ 传完了还剩个空蓝圆挂在
///    按钮上（用户 2026-10-07 报过「结束了就不要这个蓝色的块了吧」）。
pub fn set_badge(st: &Rc<PanelState>, n: usize) {
    if let Some(b) = st.xfer_badge.borrow().as_ref() {
        b.set_count(n);
    }
}

/// 冒烟用：队列按钮角上那个**转动的圈**现在转不转。
///
/// ⚠️ 为什么要单独开一个口：忙态（`pp-xfer-busy` 那个底色）与角标是
///    **两回事**——角标有数字、按钮变底色，都不代表「有东西在传」；
///    转圈才是「正在传」的唯一信号（用户 2026-11-11 要的就是它）。
pub fn btn_ring_spinning(st: &Rc<PanelState>) -> bool {
    st.xfer_badge
        .borrow()
        .as_ref()
        .map(|b| b.ring_spinning())
        .unwrap_or(false)
}

/// 冒烟用：队列按钮上那个圈现在转到哪个角度了（`None` = 面板还没建）。
pub fn btn_ring_angle(st: &Rc<PanelState>) -> Option<f64> {
    st.xfer_badge.borrow().as_ref().map(|b| b.ring_angle())
}

/// 冒烟用：队列按钮的当前状态
/// （**角标文字**, 是否忙态, 角标**有没有底色**）。
///
/// `None` = 面板上还没有这个按钮（面板还没建）。
///
/// ⚠️ 第三个值（有没有底色）是给「传完了不该剩个蓝胶囊」这条钉住的
/// （用户 2026-10-07 报）—— 蓝圆只在真有数字时才画。
pub fn btn_state(st: &Rc<PanelState>) -> Option<(String, bool, bool)> {
    let b = st.queue_btn.borrow().clone()?;
    // ⚠️ **别把角标 clone 出来**（`XferBadge` 不是 `Clone`）⇒ 借出来
    //    读两个数就还回去。
    let (badge, badge_on) = {
        let g = st.xfer_badge.borrow();
        let badge = g.as_ref()?;
        let n = badge.count();
        (
            if n > 0 { n.to_string() } else { String::new() },
            badge.has_dot(),
        )
    };
    Some((badge, b.has_css_class("pp-xfer-busy"), badge_on))
}

/// 新传输项入队：建行 + 保存行引用。
pub fn add_item(st: &Rc<PanelState>, it: &QueueItem) {
    // 传输开始了 ⇒ 文件面板工具栏上的「传输队列」按钮变忙态（阶段 102）
    refresh_btn(st);
    ensure(st);
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return };
    let it_ui = it;
    // 镜像的建目录/删除也在队列里（共用进度、取消、汇总），
    // 所以这里要按 kind 显示，别一律写成"上传"
    let (dir, file) = match it_ui.kind {
        XferKind::Transfer => (
            if it_ui.upload { tr("⬆ 上传") } else { tr("⬇ 下载") },
            format!("{} → {}", it_ui.src, it_ui.dst_dir),
        ),
        XferKind::Mkdir => (tr("＋ 建目录"), it_ui.src.clone()),
        XferKind::Remove => (tr("－ 删除"), it_ui.src.clone()),
        XferKind::MirrorUpload => (tr("⬆ 镜像"), format!("{} → {}", it_ui.src, it_ui.dst_dir)),
        XferKind::Relay => (
            tr("⇄ 跨服务器"),
            format!("{} → {}", it_ui.src, it_ui.dst_dir),
        ),
    };
    let pct: i32 = 0;
    let dash = "-";
    let waiting = tr("等待");
    let id = it_ui.id;
    let iter = qu.model.append();
    qu.model.set(
        &iter,
        &[
            (QCOL_DIR as u32, &dir),
            (QCOL_FILE as u32, &file),
            (QCOL_PCT as u32, &pct),
            (QCOL_SIZE as u32, &dash),
            (QCOL_SPEED as u32, &dash),
            (QCOL_STATE as u32, &waiting),
            (QCOL_ID as u32, &id),
            (QCOL_FAIL as u32, &""),
            (QCOL_ACTIVE as u32, &false),
            (QCOL_CODE as u32, &ST_WAIT),
        ],
    );
    let path = qu.model.path(&iter);
    qu.rows
        .borrow_mut()
        .insert(it_ui.id, gtk::TreeRowReference::new(&qu.model, &path).expect("fresh row"));
    // 不主动 present：传输列表只在用户点「传输队列」时出现。
    // 进度已经画在文件面板的文件名后面（flush 里更新）。
}

/// 工具栏「传输队列」按钮：**传输中 ⇒ 角标外面转一个圈**。
///
/// 干什么用（用户 2026-11-11）：「上传下载文件时传输队列图标可以改为原来
/// 静态的图标上加个转动的圈」⇒ 底下还是原来那个队列图标，角标外面套一个
/// 在转的圈。
///
/// ⚠️ **比以前的「透明度呼吸」好在哪**：呼吸是「整张图标一起明暗」⇒ 看不出
///    「在转」还是「卡住了」；转圈是**进度语义**（业界通用）⇒ 一眼知道
///    还在忙。
pub fn set_btn_spinning(st: &Rc<PanelState>, on: bool) {
    // ⚠️ 直接拿 `PanelState` 里存的那个角标 ⇒ **不翻控件树**
    //    （翻树要认 `Overlay`/`DrawingArea` 的层级 ⇒ 改布局就静默失效）。
    let guard = st.xfer_badge.borrow();
    let Some(badge) = guard.as_ref() else {
        return;
    };
    badge.set_spinning(on);
}

/// 刷新所有行的进度显示（对应 C 版 xfer_flush_idle）。
pub fn flush(st: &Rc<PanelState>) {
    // ⚠️ **先同步动画**：有东西在传就起呼吸定时器（传完自动停）。
    //    放前面是因为下面有一堆提前 return 的路径。
    super::sync_xfer_anim(st);
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return };
    let items = st.items.borrow();
    // 失败色按主题取一次就够：这个函数每个传输 tick 都会跑，别在循环里反复取
    let fail_color = crate::theme::state_hex(crate::theme::State::Err);
    for it in items.iter() {
        let rows = qu.rows.borrow();
        let Some(rr) = rows.get(&it.id) else { continue };
        let Some(path) = rr.path() else { continue };
        drop(rows);
        let Some(ti) = qu.model.iter(&path) else { continue };
        let g = it.shared.lock().unwrap();
        // 建目录/删除没有字节可报，done/total 恒为 0：
        // 这时别显示 "0% / 0 B / 0 B"（看着像坏了），只给状态
        let (pct, sz) = if g.total > 0 {
            (
                (g.done * 100 / g.total) as i32,
                format!("{} / {}", human_size(g.done), human_size(g.total)),
            )
        } else {
            (if g.state == XferState::Done { 100 } else { 0 }, "-".into())
        };
        let sp = if g.speed > 1.0 {
            human_size(g.speed as u64)
        } else {
            "-".into()
        };
        let state = match g.state {
            XferState::Done => match &g.msg {
                Some(m) => trf!("完成（{m}）", m = m),
                None => tr("完成").to_string(),
            },
            XferState::Skip => tr("已跳过").to_string(),
            XferState::Err => g.msg.clone().unwrap_or_else(|| tr("错误").into()),
            XferState::Cancel => tr("已取消").to_string(),
            XferState::Run => {
                if g.cur.is_empty() { tr("传输中").into() } else { g.cur.clone() }
            }
            XferState::Wait => tr("等待").to_string(),
        };
        // 失败色跟着主题走（CellRenderer 只吃颜色字符串，不经过 CSS）
        let fail = if g.state == XferState::Err {
            fail_color
        } else {
            ""
        };
        // **正在传**（只有真在跑的那些才算；排队等待的不算——否则一长串
        // 等待项都在呼吸，反而看不出"现在在传哪个"）。
        let active = g.state == XferState::Run;
        // ⚠️ **状态码**（阶段 175 / UI-11）：筛选只认它（状态列在失败时
        //   放的是**错误原文**，没法拿来判断"是不是失败"）。
        let code = match g.state {
            XferState::Wait => ST_WAIT,
            XferState::Run => ST_RUN,
            XferState::Done => ST_DONE,
            XferState::Skip => ST_SKIP,
            XferState::Err => ST_ERR,
            XferState::Cancel => ST_CANCEL,
        };
        qu.model.set(
            &ti,
            &[
                (QCOL_PCT as u32, &pct),
                (QCOL_SIZE as u32, &sz),
                (QCOL_SPEED as u32, &sp),
                (QCOL_STATE as u32, &state),
                (QCOL_FAIL as u32, &fail),
                (QCOL_ACTIVE as u32, &active),
                (QCOL_CODE as u32, &code),
            ],
        );

        // 文件面板里在文件名后画进度：上传看本地源文件、下载看远程源文件。
        // 源文件不在当前目录时找不到行，静默跳过；完整明细仍可开队列窗口。
        if it.kind == XferKind::Transfer {
            let remote_src = !it.upload;
            let cwd = st.pane_cwd(remote_src);
            // ⚠️ **源路径有两种**：上传 = 本地路径（Windows 上 `C:\dir\a.txt`），
            //    下载 = 远端 POSIX 路径 ⇒ 取父目录/取名要分语义。
            let (src_parent, src_name) = if remote_src {
                (path_parent(&it.src), path_basename(&it.src).to_string())
            } else {
                (
                    crate::platform::localpath::parent(&it.src),
                    crate::platform::localpath::basename(&it.src),
                )
            };
            if src_parent == cwd {
                // ⚠️ **xfer = 正在传**（只有 Run 才算）：文件列表里这一行
                // 画呼吸背景（用户 2026-11-10）。排队等待的不算——一长串
                // 等待项都在呼吸，反而看不出"现在在传哪个"。
                //
                // ⚠️ **终态文案（「完成」/「失败」…）走另一条路**：
                //    `set_terminal` 让它**显示一会儿再渐隐**（用户 2026-11-10：
                //    「下载完了，就显示一下完成，然后就慢慢消失吧，不要把
                //    「完成」一直显示在那里」）。
                let terminal = matches!(
                    g.state,
                    XferState::Done | XferState::Err | XferState::Skip | XferState::Cancel
                );
                if terminal {
                    let (tpct, ttext) = match g.state {
                        XferState::Done => (100, tr("完成").to_string()),
                        XferState::Skip => (0, tr("跳过").to_string()),
                        XferState::Err => (0, tr("失败").to_string()),
                        _ => (0, tr("取消").to_string()),
                    };
                    super::pane::set_terminal(
                        &st.pane(remote_src),
                        &src_name,
                        tpct,
                        &ttext,
                        super::pane::now_ms(),
                    );
                } else {
                    let (ppct, ptext, pxfer) = match g.state {
                        XferState::Wait => (0, tr("等待").to_string(), false),
                        _ => (pct, format!("{pct}%"), true),
                    };
                    super::pane::set_progress(&st.pane(remote_src), &src_name, ppct, &ptext, pxfer);
                }
            }
        }
    }
    // ⚠️ **先放掉 `st.items` 的借用**再刷按钮：刷新要读队列算"还在传几个"
    // ⇒ 借用没放掉就是 RefCell 双重借用 panic。
    //
    // ⚠️ **摘要条**（阶段 175 / UI-11）：完成 / 失败 / 跳过 / 取消各多少。
    {
        let mut done = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;
        let mut canceled = 0usize;
        for it in items.iter() {
            let g = it.shared.lock().unwrap();
            match g.state {
                XferState::Done => done += 1,
                XferState::Err => failed += 1,
                XferState::Skip => skipped += 1,
                XferState::Cancel => canceled += 1,
                _ => {}
            }
        }
        if let Some(su) = qu.summary.as_ref() {
            su.set(done, failed, skipped, canceled);
        }
    }
    drop(items);
    refresh_btn(st);
    // ⚠️ **再同步一次动画**：这一轮可能刚给某一行写上终态文案（「完成」）
    //    ⇒ 得让动画定时器**继续跑**，把那行慢慢淡掉（不然它会一直挂着）。
    super::sync_xfer_anim(st);
}

/// 取消选中行：运行中的置取消标志，等待中的直接标记取消。
fn cancel_selected(st: &Rc<PanelState>) {
    let ids: Vec<u64> = {
        let qu = st.queue_ui.borrow();
        let Some(qu) = qu.as_ref() else { return };
        let mut ids = Vec::new();
        for path in selected_model_paths(qu) {
            let Some(ti) = qu.model.iter(&path) else { continue };
            let id: u64 = qu.model.get(&ti, QCOL_ID);
            ids.push(id);
        }
        ids
    };
    let items = st.items.borrow();
    for it in items.iter() {
        if !ids.contains(&it.id) {
            continue;
        }
        let mut g = it.shared.lock().unwrap();
        match g.state {
            XferState::Run => {
                g.cancel = true;
                g.cancel_flag
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            XferState::Wait => {
                // 调度器仍会把它取出来跑；cancel=true 让 run_transfer
                // 开头立即退出，不会真的开始传
                g.cancel = true;
                g.state = XferState::Cancel;
            }
            _ => {}
        }
    }
    drop(items);
    flush(st);
}

/// 清除已完成/取消/失败的行与项。
fn clear_finished(st: &Rc<PanelState>) {
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return };
    let mut ids_to_remove: Vec<u64> = Vec::new();
    {
        let items = st.items.borrow();
        for it in items.iter() {
            let g = it.shared.lock().unwrap();
            if !matches!(g.state, XferState::Run | XferState::Wait) {
                ids_to_remove.push(it.id);
            }
        }
    }
    for id in &ids_to_remove {
        let rows = qu.rows.borrow();
        if let Some(rr) = rows.get(id) {
            if let Some(path) = rr.path() {
                if let Some(ti) = qu.model.iter(&path) {
                    qu.model.remove(&ti);
                }
            }
        }
        drop(rows);
        qu.rows.borrow_mut().remove(id);
    }
    st.items.borrow_mut().retain(|it| !ids_to_remove.contains(&it.id));
    flush(st);
}


/* ---------------- 右键菜单：取消 / 删除 / 清除已完成 ---------------- */

/// 选中项里**失败的那一条**（id ＋ 错误原文）。
///
/// ⚠️ 「查看原因」「复制错误」「重新传输」都只对**失败项**有意义
///   （审核 P2-4：「失败行提供『查看原因』『复制路径/错误』等低风险动作」）。
fn selected_failed(st: &Rc<PanelState>) -> Option<(u64, String)> {
    let qu = st.queue_ui.borrow();
    let qu = qu.as_ref()?;
    let mut ids = Vec::new();
    for path in selected_model_paths(qu) {
        let Some(ti) = qu.model.iter(&path) else { continue };
        ids.push(qu.model.get::<u64>(&ti, QCOL_ID));
    }
    drop(qu);
    let items = st.items.borrow();
    for it in items.iter() {
        if !ids.contains(&it.id) {
            continue;
        }
        let g = it.shared.lock().unwrap();
        if g.state == XferState::Err {
            let msg = g.msg.clone().unwrap_or_else(|| tr("错误").into());
            return Some((it.id, msg));
        }
    }
    None
}

/// 「重新传输」能不能用（**能不能用由三件事决定**）。
///
/// ⚠️ **审核 P2-4**：「仅在源、目标、连接仍有效时可用」⇒
///   · 选中的那一项**是失败的**；
///   · 那一项**还在队列里**（源、目标信息还在 ⇒ 知道该重传什么）；
///   · **连接还活着**（`conn_ok`）⇒ 否则重传立刻又失败。
/// ⇒ 不可用时菜单项**灰掉**，并在 tooltip 里**说明为什么**。
pub fn retry_availability(st: &Rc<PanelState>) -> (bool, String) {
    if !st.conn_ok.get() {
        return (
            false,
            tr("连接已经断了 —— 重新连上以后才能重新传输").to_string(),
        );
    }
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else {
        return (false, tr("队列还没建起来").to_string());
    };
    let mut ids = Vec::new();
    for path in selected_model_paths(qu) {
        let Some(ti) = qu.model.iter(&path) else { continue };
        ids.push(qu.model.get::<u64>(&ti, QCOL_ID));
    }
    drop(qu);
    let items = st.items.borrow();
    for it in items.iter() {
        if !ids.contains(&it.id) {
            continue;
        }
        let g = it.shared.lock().unwrap();
        if g.state != XferState::Err {
            continue;
        }
        if it.kind != XferKind::Transfer {
            return (
                false,
                tr("只有文件传输能重新传（建目录、删除不能重做）").to_string(),
            );
        }
        if it.src.is_empty() || it.dst_dir.is_empty() {
            return (
                false,
                tr("这一项的源或目标已经找不到了 —— 重新选一次文件吧").to_string(),
            );
        }
        return (true, String::new());
    }
    (
        false,
        tr("选中一个失败的传输项才能重新传输").to_string(),
    )
}

/// 重新传输选中的那一项（阶段 175 / UI-11）。
///
/// ⚠️ **执行前说明覆盖风险**（审核验收：「重试不会隐式覆盖文件」）：
///   先弹一个确认框说明「目标文件会被覆盖」⇒ 用户点了才继续。
/// ⚠️ **不自动重试**：失败了就摆在那儿，用户决定要不要重来 ⇒ 程序**不会**
///   自己偷偷再传一遍。
/// ⚠️ 真正提交时走**既有**的 `submit_transfers` ⇒ 冲突预检照旧会问
///   （覆盖 / 跳过 / 逐项）⇒ 不会悄悄盖掉文件。
fn retry_selected(st: &Rc<PanelState>) {
    let (ok, why) = retry_availability(st);
    if !ok {
        super::dialogs::info(st.topwin().as_ref(), tr("不能重新传输"), &why);
        return;
    }
    // 找到那一项的源 / 目标 / 方向
    let Some((id, _)) = selected_failed(st) else { return };
    let Some((src, dst_dir, upload)) = (
        {
            let items = st.items.borrow();
            let mut found = None;
            for it in items.iter() {
                if it.id == id {
                    found = Some((it.src.clone(), it.dst_dir.clone(), it.upload));
                    break;
                }
            }
            found
        }
    ) else {
        return;
    };
    let st2 = Rc::clone(st);
    let msg = trf!(
        "重新传这一项：\n{src} → {dst}\n\n目标位置已经有同名文件的话会被覆盖。确定继续吗？",
        src = src,
        dst = dst_dir
    );
    super::dialogs::confirm_title(
        st.topwin().as_ref(),
        tr("重新传输"),
        &msg,
        tr("重新传输"),
        move |yes| {
            if !yes {
                return;
            }
            super::retry_transfer(&st2, &src, &dst_dir, upload);
        },
    );
}

/// 选中行里各状态的统计（决定菜单项可用性）。
fn selected_states(st: &Rc<PanelState>) -> (bool, bool, bool, bool) {
    // (有选中, 有运行中, 有等待中, 有可删除的已结束项)
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return (false, false, false, false) };
    let mut ids = Vec::new();
    for path in selected_model_paths(qu) {
        let Some(ti) = qu.model.iter(&path) else { continue };
        ids.push(qu.model.get::<u64>(&ti, QCOL_ID));
    }
    drop(qu);
    let items = st.items.borrow();
    let mut any = false;
    let mut running = false;
    let mut waiting = false;
    let mut finished = false;
    for it in items.iter() {
        if !ids.contains(&it.id) {
            continue;
        }
        any = true;
        let g = it.shared.lock().unwrap();
        match g.state {
            XferState::Run => running = true,
            XferState::Wait => waiting = true,
            _ => finished = true,
        }
    }
    (any, running, waiting, finished)
}

fn install_context_menu(st: &Rc<PanelState>, view: &gtk::TreeView) {
    let pop = crate::menu::popover(view);
    let vb = crate::menu::body();

    let b_cancel = crate::menu::item("process-stop-symbolic", tr("取消"), None);
    crate::menu::danger(&b_cancel);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_cancel.connect_clicked(move |_| {
            cancel_selected(&st2);
            pop2.popdown();
        });
    }
    vb.append(&b_cancel);

    let b_remove = crate::menu::item("edit-delete-symbolic", tr("删除"), None);
    crate::menu::danger(&b_remove);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_remove.connect_clicked(move |_| {
            remove_selected(&st2);
            pop2.popdown();
        });
    }
    vb.append(&b_remove);

    // ---- 失败项的动作（阶段 175 / UI-11）----
    //
    // ⚠️ **都是低风险动作**：「查看原因」「复制错误」只是读。
    //   「重新传输」是**独立子项**（不是「删除」那种顺手就干的）⇒
    //   它**单独确认**，而且**不隐式覆盖**（走既有的冲突预检）。
    let b_reason = crate::menu::item("dialog-information-symbolic", tr("查看原因"), None);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_reason.connect_clicked(move |_| {
            if let Some((_, msg)) = selected_failed(&st2) {
                super::dialogs::info(st2.topwin().as_ref(), tr("失败原因"), &msg);
            }
            pop2.popdown();
        });
    }
    vb.append(&b_reason);

    let b_copy = crate::menu::item("edit-copy-symbolic", tr("复制错误"), None);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_copy.connect_clicked(move |_| {
            if let Some((_, msg)) = selected_failed(&st2) {
                let top = st2.topwin();
                if let Some(w) = top.as_ref() {
                    w.clipboard().set_text(&msg);
                }
            }
            pop2.popdown();
        });
    }
    vb.append(&b_copy);

    let b_retry = crate::menu::item("view-refresh-symbolic", tr("重新传输"), None);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_retry.connect_clicked(move |_| {
            pop2.popdown();
            retry_selected(&st2);
        });
    }
    vb.append(&b_retry);

    vb.append(&crate::menu::sep());

    let b_clear = crate::menu::item("edit-clear-all-symbolic", tr("清除已完成"), None);
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        b_clear.connect_clicked(move |_| {
            clear_finished(&st2);
            pop2.popdown();
        });
    }
    vb.append(&b_clear);
    pop.set_child(Some(&vb));

    let st = Rc::clone(st);
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    let view2 = view.clone();
    let (cancel_btn, remove_btn) = (b_cancel.clone(), b_remove.clone());
    let (reason_btn, copy_btn, retry_btn) = (b_reason.clone(), b_copy.clone(), b_retry.clone());
    gesture.connect_pressed(move |g, _n, x, y| {
        if g.current_button() != 3 {
            return;
        }
        // 点在未选中的行上时，先选中它（文件面板同款惯例）
        if let Some((Some(path), _, _, _)) = view2.path_at_pos(x as i32, y as i32) {
            let sel = view2.selection();
            if !sel.path_is_selected(&path) {
                sel.unselect_all();
                sel.select_path(&path);
            }
        }
        // 按选中项状态启停菜单项
        let (any, running, waiting, finished) = selected_states(&st);
        cancel_btn.set_sensitive(any && (running || waiting));
        // 运行中的项不允许删除（进度行还在被 worker 刷新）；
        // 等待中的删除 = 标记取消后立即移除行
        remove_btn.set_sensitive(any && !running);
        let _ = finished;
        // ⚠️ **阶段 175 / UI-11**：失败项的动作 —— 不可用时**灰掉**，
        //   并在 tooltip 里**说明为什么**（审核验收：「不可用时按钮灰掉
        //   并说明原因」）。
        let failed = selected_failed(&st).is_some();
        reason_btn.set_sensitive(failed);
        copy_btn.set_sensitive(failed);
        let (can_retry, why) = retry_availability(&st);
        retry_btn.set_sensitive(can_retry);
        retry_btn.set_tooltip_text(Some(&why));
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.popup();
    });
    view.add_controller(gesture);
}

/// 删除选中行：等待中的先标记取消（worker 轮到时快速跳过）再移除；
/// 运行中的不可删（菜单已禁用，这里防御性跳过）。
fn remove_selected(st: &Rc<PanelState>) {
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return };
    let mut ids: Vec<u64> = Vec::new();
    for path in selected_model_paths(qu) {
        let Some(ti) = qu.model.iter(&path) else { continue };
        ids.push(qu.model.get::<u64>(&ti, QCOL_ID));
    }
    drop(qu);

    let mut removed: Vec<u64> = Vec::new();
    {
        let items = st.items.borrow();
        for it in items.iter() {
            if !ids.contains(&it.id) {
                continue;
            }
            let mut g = it.shared.lock().unwrap();
            match g.state {
                XferState::Run => continue, // 防御：运行中不删
                XferState::Wait => {
                    // 未开始的直接作废：worker 轮到时按取消跳过
                    g.cancel = true;
                    g.cancel_flag
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    g.state = XferState::Cancel;
                }
                _ => {}
            }
            removed.push(it.id);
        }
    }
    if removed.is_empty() {
        return;
    }
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return };
    for id in &removed {
        let rows = qu.rows.borrow();
        if let Some(rr) = rows.get(id) {
            if let Some(path) = rr.path() {
                if let Some(ti) = qu.model.iter(&path) {
                    qu.model.remove(&ti);
                }
            }
        }
        drop(rows);
        qu.rows.borrow_mut().remove(id);
    }
    drop(qu);
    st.items.borrow_mut().retain(|it| !removed.contains(&it.id));
    flush(st);
}
