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

    let model = gtk::ListStore::new(&[
        glib::Type::STRING, // DIR
        glib::Type::STRING, // FILE
        glib::Type::I32,    // PCT
        glib::Type::STRING, // SIZE
        glib::Type::STRING, // SPEED
        glib::Type::STRING, // STATE
        glib::Type::U64,    // ID
        glib::Type::STRING, // 失败态前景色（空 = 用主题默认色）
    ]);

    let view = gtk::TreeView::with_model(&model);
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

    // 角标 + 图标
    if let Some(child) = b.child() {
        if let Some(row) = child.downcast_ref::<gtk::Box>() {
            let mut cur = row.first_child();
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    // ⚠️ **不 hide，只是把文字清空**：角标 `width_chars(1)`
                    // 已经预留了一格宽 ⇒ 出现/消失时工具栏不会左右跳。
                    l.set_text(&if n > 0 { n.to_string() } else { String::new() });
                }
                if let Some(i) = c.downcast_ref::<gtk::Image>() {
                    i.set_icon_name(Some(if n > 0 {
                        "content-loading-symbolic"
                    } else {
                        "view-list-symbolic"
                    }));
                }
                cur = c.next_sibling();
            }
        }
    }

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

/// 冒烟用：队列按钮的当前状态（**角标文字**, 是否忙态）。
///
/// `None` = 面板上还没有这个按钮（面板还没建）。
pub fn btn_state(st: &Rc<PanelState>) -> Option<(String, bool)> {
    let b = st.queue_btn.borrow().clone()?;
    let mut badge = String::new();
    if let Some(child) = b.child() {
        if let Some(row) = child.downcast_ref::<gtk::Box>() {
            let mut cur = row.first_child();
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>() {
                    badge = l.text().to_string();
                    break;
                }
                cur = c.next_sibling();
            }
        }
    }
    Some((badge, b.has_css_class("pp-xfer-busy")))
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
        ],
    );
    let path = qu.model.path(&iter);
    qu.rows
        .borrow_mut()
        .insert(it_ui.id, gtk::TreeRowReference::new(&qu.model, &path).expect("fresh row"));
    // 不主动 present：传输列表只在用户点「传输队列」时出现。
    // 进度已经画在文件面板的文件名后面（flush 里更新）。
}

/// 刷新所有行的进度显示（对应 C 版 xfer_flush_idle）。
pub fn flush(st: &Rc<PanelState>) {
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
        qu.model.set(
            &ti,
            &[
                (QCOL_PCT as u32, &pct),
                (QCOL_SIZE as u32, &sz),
                (QCOL_SPEED as u32, &sp),
                (QCOL_STATE as u32, &state),
                (QCOL_FAIL as u32, &fail),
            ],
        );

        // 文件面板里在文件名后画进度：上传看本地源文件、下载看远程源文件。
        // 源文件不在当前目录时找不到行，静默跳过；完整明细仍可开队列窗口。
        if it.kind == XferKind::Transfer {
            let remote_src = !it.upload;
            let cwd = st.pane_cwd(remote_src);
            if path_parent(&it.src) == cwd {
                let (ppct, ptext) = match g.state {
                    XferState::Wait => (0, tr("等待").to_string()),
                    XferState::Run => (pct, format!("{pct}%")),
                    XferState::Done => (100, tr("完成").to_string()),
                    XferState::Skip => (0, tr("跳过").to_string()),
                    XferState::Err => (0, tr("失败").to_string()),
                    XferState::Cancel => (0, tr("取消").to_string()),
                };
                super::pane::set_progress(
                    &st.pane(remote_src),
                    path_basename(&it.src),
                    ppct,
                    &ptext,
                );
            }
        }
    }
    // ⚠️ **先放掉 `st.items` 的借用**再刷按钮：刷新要读队列算"还在传几个"
    // ⇒ 借用没放掉就是 RefCell 双重借用 panic。
    drop(items);
    refresh_btn(st);
}

/// 取消选中行：运行中的置取消标志，等待中的直接标记取消。
fn cancel_selected(st: &Rc<PanelState>) {
    let ids: Vec<u64> = {
        let qu = st.queue_ui.borrow();
        let Some(qu) = qu.as_ref() else { return };
        let (rows, _) = qu.view.selection().selected_rows();
        let mut ids = Vec::new();
        for path in rows {
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

/// 选中行里各状态的统计（决定菜单项可用性）。
fn selected_states(st: &Rc<PanelState>) -> (bool, bool, bool, bool) {
    // (有选中, 有运行中, 有等待中, 有可删除的已结束项)
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return (false, false, false, false) };
    let (rows, _) = qu.view.selection().selected_rows();
    let mut ids = Vec::new();
    for path in rows {
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
    let (rows, _) = qu.view.selection().selected_rows();
    let mut ids: Vec<u64> = Vec::new();
    for path in rows {
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
