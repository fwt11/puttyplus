//! 单向目录镜像的界面（6.2）：本地目录 → 远端目录。
//!
//! 三步：**扫描 → 预览 → 执行**。删除永远要多点一次。
//!
//! - 「删除远端多出的文件」默认**不勾**，勾上之后预览里才出现删除行；
//!   而且执行前还会再确认一次（带文件数和名字），因为这一步不可逆。
//! - 勾选/取消勾选、切换「显示未变化」都**只重算不重扫**——两份扫描结果
//!   缓存在本地，重扫一次大目录要好几秒，点两下开关不该让人等。
//! - 执行不是自己造一套进度系统，而是把「建目录 / 上传 / 删除」按顺序
//!   塞进现成的传输队列（`worker::XferKind`），进度、取消、批量汇总
//!   （4.3）全都白拿。
//!
//! 方向只有这一条：不提供"远端 → 本地"，也不做冲突合并。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::mirror::{self, Action, Plan, Scan};
use crate::panel::queue::{self, QueueItem};
use crate::panel::worker::{self, XferKind, XferSpec};
use crate::panel::PanelState;
use crate::i18n::tr;
use crate::trf;

const MCOL_ACT: i32 = 0;
const MCOL_REL: i32 = 1;
const MCOL_SIZE: i32 = 2;
const MCOL_LMTIME: i32 = 3;
const MCOL_RMTIME: i32 = 4;

/// 目录镜像窗口的控件。
#[derive(Clone)]
pub struct MirrorUi {
    pub win: gtk::Window,
    pub view: gtk::TreeView,
    pub model: gtk::ListStore,
    /// 上次扫描的两份结果（切开关时重算用，不重扫）
    pub scan: RefCell<Option<(String, String, Scan, Scan)>>,
    /// 当前计划（on_start 直接取）
    pub plan: RefCell<Option<Plan>>,
    pub summary: gtk::Label,
    pub delete_cb: gtk::CheckButton,
    pub same_cb: gtk::CheckButton,
    pub status: gtk::Label,
    pub start_btn: gtk::Button,
    /// 忽略规则输入框（开跑时存进 settings.ini）
    pub ignore_entry: gtk::Entry,
}

/// 打开（或聚焦）目录镜像窗口。
pub fn open(st: &Rc<PanelState>) {
    if !st.conn_ok.get() {
        crate::panel::dialogs::show_error(
            st.topwin().as_ref(),
            tr("未连接"),
            tr("SFTP 尚未连接，无法扫描远程目录。"),
        );
        return;
    }
    if let Some(ui) = st.mirror_ui.borrow().as_ref() {
        ui.win.present();
        return;
    }
    build(st);
}

fn build(st: &Rc<PanelState>) {
    let top = st.topwin();
    // 阶段 67（审计 P2-17）：目录镜像是**工具窗**——
    // 以前是「非模态 GtkDialog」（审计点名的那种混合形态）⇒ 改成 Window。
    let dlg = gtk::Window::new();
    dlg.set_title(Some(tr("目录镜像（本地 → 远程）")));
    dlg.set_transient_for(top.as_ref());
    let (mw, mh) = crate::settings::window_size("mirror").unwrap_or((860, 560));
    dlg.set_default_size(mw, mh);
    dlg.connect_close_request(|d| {
        crate::settings::save_window_size("mirror", d);
        gtk::glib::Propagation::Proceed
    });

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(10);
    root.set_margin_end(10);

    /* ---- 两端目录 ---- */
    let local_entry = gtk::Entry::new();
    local_entry.set_hexpand(true);
    local_entry.set_text(&*st.local_cwd.borrow());
    let remote_entry = gtk::Entry::new();
    remote_entry.set_hexpand(true);
    remote_entry.set_text(&*st.remote_cwd.borrow());

    let browse = gtk::Button::with_label(tr("浏览…"));
    let root_row = dir_row(
        tr("本地目录"),
        &local_entry,
        Some(&browse),
        st.clone(),
        dlg.clone(),
    );
    root.append(&root_row);
    root.append(&dir_row(tr("远程目录"), &remote_entry, None, st.clone(), dlg.clone()));

    let scan_btn = gtk::Button::with_label(tr("扫描差异"));
    scan_btn.add_css_class("suggested-action");
    {
        let st2 = Rc::clone(st);
        let a = local_entry.clone();
        let b = remote_entry.clone();
        scan_btn.connect_clicked(move |_| {
            let l = a.text().to_string();
            let r = b.text().to_string();
            if l.trim().is_empty() || r.trim().is_empty() {
                crate::panel::dialogs::show_error(
                    st2.topwin().as_ref(),
                    tr("目录没填"),
                    tr("本地目录与远程目录都要填。"),
                );
                return;
            }
            send_scan(&st2, &l, &r);
        });
    }

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.append(&scan_btn);
    let ignore_entry = gtk::Entry::new();
    ignore_entry.set_hexpand(true);
    ignore_entry.set_placeholder_text(Some(tr("忽略规则，逗号分隔（改完要重新扫描）")));
    ignore_entry.set_tooltip_text(Some(
        tr("按条目名匹配（不含路径），支持 * 与 ?。命中目录则整棵子树跳过。\n\
         默认忽略 .git、node_modules、*.swp 之类；清空则什么都不忽略。"),
    ));
    ignore_entry.set_text(&crate::settings::mirror_ignore());
    bar.append(&gtk::Label::new(Some(tr("忽略"))));
    bar.append(&ignore_entry);
    root.append(&bar);

    /* ---- 开关 ---- */
    let delete_cb = gtk::CheckButton::with_label(tr("删除远程多出的文件"));
    delete_cb.set_active(false);
    delete_cb.set_tooltip_text(Some(
        tr("默认不删。勾上之后，远程存在而本地没有的文件/目录会被删除。\n\
         不可撤销——执行前还会再确认一次。"),
    ));
    let same_cb = gtk::CheckButton::with_label(tr("显示未变化的文件"));
    same_cb.set_active(false);
    let opt_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    opt_bar.append(&delete_cb);
    opt_bar.append(&same_cb);
    root.append(&opt_bar);

    /* ---- 预览 ---- */
    let summary = gtk::Label::new(Some(tr("尚未扫描。选择两端目录后点「扫描差异」。")));
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("heading");
    root.append(&summary);

    let model = gtk::ListStore::new(&[
        gtk::glib::Type::STRING, // 动作
        gtk::glib::Type::STRING, // 相对路径
        gtk::glib::Type::STRING, // 大小
        gtk::glib::Type::STRING, // 本地时间
        gtk::glib::Type::STRING, // 远端时间
    ]);
    let view = gtk::TreeView::with_model(&model);
    view.append_column(&col(tr("动作"), MCOL_ACT, false));
    view.append_column(&col(tr("相对路径"), MCOL_REL, true));
    view.append_column(&col(tr("大小"), MCOL_SIZE, false));
    view.append_column(&col(tr("本地修改时间"), MCOL_LMTIME, false));
    view.append_column(&col(tr("远程修改时间"), MCOL_RMTIME, false));
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_child(Some(&view));
    sw.set_vexpand(true);
    root.append(&sw);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("dim-label");
    root.append(&status);

    let tip = gtk::Label::new(Some(
        tr("单向镜像：只把本地推向远程，不拉取、不合并冲突。\n\
         两边类型不同（一目录一文件）的路径一律不动，列在预览里等你处理。"),
    ));
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    tip.add_css_class("dim-label");
    root.append(&tip);

    /* ---- 底部按钮 ---- */
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let start_btn = gtk::Button::with_label(tr("开始镜像"));
    start_btn.add_css_class("suggested-action");
    start_btn.set_sensitive(false);
    buttons.append(&start_btn);
    let close_btn = gtk::Button::with_label(tr("关闭"));
    {
        let d2 = dlg.clone();
        close_btn.connect_clicked(move |_| d2.close());
    }
    buttons.append(&close_btn);
    root.append(&buttons);

    dlg.set_child(Some(&root));

    let ui = MirrorUi {
        win: dlg.clone(),
        view,
        model,
        scan: RefCell::new(None),
        plan: RefCell::new(None),
        summary,
        delete_cb: delete_cb.clone(),
        same_cb: same_cb.clone(),
        status,
        start_btn: start_btn.clone(),
        ignore_entry: ignore_entry.clone(),
    };
    // 先塞回 PanelState 再连回调：回调里要用 mirror_ui 读扫描结果
    *st.mirror_ui.borrow_mut() = Some(ui);

    // 开关改动 → 本地重算（不重扫）
    for cb in [&delete_cb, &same_cb] {
        let st2 = Rc::clone(st);
        cb.connect_toggled(move |_| replan(&st2));
    }
    {
        let st2 = Rc::clone(st);
        start_btn.connect_clicked(move |_| on_start(&st2));
    }
    {
        let st2 = Rc::clone(st);
        dlg.connect_close_request(move |_| {
            *st2.mirror_ui.borrow_mut() = None;
            glib::Propagation::Proceed
        });
    }
    // 浏览本地目录
    {
        let st2 = Rc::clone(st);
        let e = local_entry.clone();
        browse.connect_clicked(move |_| {
            let top = st2.topwin();
            let mut b = gtk::FileChooserNative::builder()
                .title(tr("选本地目录"))
                .action(gtk::FileChooserAction::SelectFolder)
                .accept_label(tr("选定"))
                .cancel_label(tr("取消"))
                .modal(true);
            if let Some(p) = top.as_ref() {
                b = b.transient_for(p);
            }
            let ch = b.build();
            let ee = e.clone();
            ch.connect_response(move |c, r| {
                if r == gtk::ResponseType::Accept {
                    if let Some(f) = c.file() {
                        if let Some(p) = f.path() {
                            ee.set_text(&p.to_string_lossy());
                        }
                    }
                }
                c.destroy();
            });
            ch.show();
        });
    }

    dlg.present();
}

fn dir_row(
    title: &str,
    entry: &gtk::Entry,
    extra: Option<&gtk::Button>,
    _st: Rc<PanelState>,
    _dlg: gtk::Window,
) -> gtk::Box {
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let l = gtk::Label::new(Some(title));
    l.set_width_chars(8);
    l.set_xalign(0.0);
    h.append(&l);
    h.append(entry);
    if let Some(b) = extra {
        h.append(b);
    }
    h
}

fn col(title: &str, id: i32, expand: bool) -> gtk::TreeViewColumn {
    let c = gtk::TreeViewColumn::new();
    c.set_title(title);
    c.set_expand(expand);
    c.set_resizable(true);
    c.add_attribute(&gtk::CellRendererText::new(), "text", id);
    c
}

/// 发扫描命令给 worker。
fn send_scan(st: &Rc<PanelState>, local: &str, remote: &str) {
    let ignore = crate::settings::mirror_ignore();
    let ui = st.mirror_ui.borrow();
    if let Some(ui) = ui.as_ref() {
        ui.status.set_text(tr("正在扫描本地与远程目录…"));
        ui.start_btn.set_sensitive(false);
    }
    drop(ui);
    let tx = st.tx.borrow().clone();
    if let Some(tx) = tx {
        let _ = tx.send(worker::WorkerCmd::MirrorScan {
            local: local.to_string(),
            remote: remote.to_string(),
            ignore,
        });
    }
}

/// worker 回填扫描结果。
pub fn on_scan_result(
    st: &Rc<PanelState>,
    local: String,
    remote: String,
    result: Result<(Scan, Scan), String>,
) {
    let Some(ui) = st.mirror_ui.borrow().clone() else {
        return; // 窗口已经关了，结果丢弃
    };
    match result {
        Err(e) => {
            ui.status.set_text(&trf!("扫描失败：{e}", e = e));
            ui.start_btn.set_sensitive(false);
            ui.summary.set_text(tr("扫描失败"));
        }
        Ok((l, r)) => {
            ui.status.set_text(&skipped_text(&l, &r));
            *ui.scan.borrow_mut() = Some((local, remote, l, r));
            replan(st);
        }
    }
}

/// 被忽略/跳过的条目如实报出来，不静默吞。
fn skipped_text(l: &Scan, r: &Scan) -> String {
    let mut s = String::new();
    if !l.skipped.is_empty() {
        s.push_str(&trf!("本地跳过 {v0} 项（忽略规则/软链接）", v0 = l.skipped.len()));
    }
    if !r.skipped.is_empty() {
        if !s.is_empty() {
            s.push('，');
        }
        s.push_str(&trf!("远程跳过 {v0} 项", v0 = r.skipped.len()));
    }
    if !l.too_deep.is_empty() || !r.too_deep.is_empty() {
        if !s.is_empty() {
            s.push('，');
        }
        s.push_str(&trf!("超过 {v0} 层未处理", v0 = mirror::MAX_DEPTH));
    }
    if s.is_empty() {
        tr("两侧都已完整扫描。").to_string()
    } else {
        s
    }
}

/// 用缓存的扫描结果重算计划并刷新预览（不重扫）。
fn replan(st: &Rc<PanelState>) {
    let Some(ui) = st.mirror_ui.borrow().clone() else {
        return;
    };
    let Some((_local_root, _remote_root, l, r)) = ui.scan.borrow().clone() else {
        return;
    };
    let o = mirror::Opts {
        delete: ui.delete_cb.is_active(),
        ..mirror::Opts::default()
    };
    let plan = mirror::build_plan(&l.entries, &r.entries, &o, !r.root_exists);
    let show_same = ui.same_cb.is_active();
    fill_preview(&ui, &plan, show_same);
    ui.summary.set_text(&plan.summary(!r.root_exists));
    ui.start_btn.set_sensitive(!plan.is_empty());
    // 计划直接挂在窗口上：on_start 从这儿取，省掉一个全局单例
    // （多标签各开各的镜像窗口时全局变量必然串味）
    *ui.plan.borrow_mut() = Some(plan);
}

fn fill_preview(ui: &MirrorUi, plan: &Plan, show_same: bool) {
    ui.model.clear();
    for it in plan.visible(show_same) {
        let iter = ui.model.append();
        ui.model.set(
            &iter,
            &[
                (MCOL_ACT as u32, &it.action.label()),
                (MCOL_REL as u32, &display_rel(&it.rel)),
                (
                    MCOL_SIZE as u32,
                    // 目录没有"大小"可言；未变化/冲突行显示的是同一个数，
                    // 重复一遍只会让人以为是两处不同的量
                    &if it.action == Action::Mkdir {
                        String::new()
                    } else {
                        crate::util::human_size(it.size)
                    },
                ),
                (MCOL_LMTIME as u32, &fmt_time(it.local_mtime)),
                (MCOL_RMTIME as u32, &fmt_time(it.remote_mtime)),
            ],
        );
    }
}

fn display_rel(rel: &str) -> String {
    if rel.is_empty() {
        tr("（远程根目录本身）").into()
    } else {
        rel.to_string()
    }
}

fn fmt_time(t: i64) -> String {
    if t <= 0 {
        String::new()
    } else {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    }
}

/// 点「开始镜像」。
fn on_start(st: &Rc<PanelState>) {
    let Some(ui) = st.mirror_ui.borrow().clone() else {
        return;
    };
    let Some((local_root, remote_root, _, _)) = ui.scan.borrow().clone() else {
        return;
    };
    let Some(plan) = ui.plan.borrow().clone() else { return };
    if plan.is_empty() {
        return;
    }
    // 忽略规则只在开跑时落库：用户可能扫着扫着改主意，那就不该留下痕迹
    crate::settings::set_mirror_ignore(ui.ignore_entry.text().as_str());

    if plan.n_delete > 0 {
        // 删除不可逆：再来一次显式确认，并把要删的东西列出来
        let names: Vec<String> = plan
            .items
            .iter()
            .filter(|i| i.action == Action::Delete)
            .map(|i| i.rel.clone())
            .collect();
        let shown: Vec<String> = names
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>();
        let more = if names.len() > shown.len() {
            trf!("\n… 等共 {v0} 项", v0 = names.len())
        } else {
            String::new()
        };
        let msg = trf!("将在远程删除 {v0} 项本地没有的文件/目录：\n\n{v1}{more}\n\n确定要删吗？此操作不可撤销。", v0 = names.len(), v1 = shown.join("\n"), more = more);
        let st2 = Rc::clone(st);
        crate::panel::dialogs::confirm(st.topwin().as_ref(), &msg, move |ok| {
            if ok {
                apply(&st2, &plan, &local_root, &remote_root);
            }
        });
        return;
    }
    apply(st, &plan, &local_root, &remote_root);
}

/// 把计划拆成队列项，按「建目录 → 上传 → 删除」的顺序提交。
fn apply(st: &Rc<PanelState>, plan: &Plan, local_root: &str, remote_root: &str) {
    let steps = mirror::order(plan);
    if steps.is_empty() {
        return;
    }
    let mut items_ui: Vec<QueueItem> = Vec::new();
    let mut specs: Vec<XferSpec> = Vec::new();
    for it in steps {
        let id = st.next_id.get();
        st.next_id.set(id + 1);
        let shared = worker::XferShared::new();
        let (src, dst_dir, upload, kind) = match it.action {
            Action::Mkdir => (
                mirror::remote_path(remote_root, &it.rel),
                String::new(),
                true,
                XferKind::Mkdir,
            ),
            Action::Delete => (
                mirror::remote_path(remote_root, &it.rel),
                String::new(),
                true,
                XferKind::Remove,
            ),
            Action::Upload => (
                mirror::local_path(std::path::Path::new(local_root), &it.rel)
                    .to_string_lossy()
                    .into_owned(),
                mirror::remote_path(remote_root, &it.rel),
                true,
                XferKind::MirrorUpload,
            ),
            _ => continue,
        };
        items_ui.push(QueueItem {
            id,
            src: src.clone(),
            dst_dir: dst_dir.clone(),
            upload,
            kind,
            shared: shared.clone(),
        });
        specs.push(XferSpec {
            id,
            src,
            dst_dir,
            upload,
            kind,
            conflict: worker::XferConflict::Overwrite,
            shared,
        });
    }

    if specs.is_empty() {
        return;
    }
    {
        let mut items = st.items.borrow_mut();
        items.extend(items_ui.iter().map(|it| QueueItem {
            id: it.id,
            src: it.src.clone(),
            dst_dir: it.dst_dir.clone(),
            upload: it.upload,
            kind: it.kind,
            shared: it.shared.clone(),
        }));
    }
    for it in &items_ui {
        queue::add_item(st, it);
    }

    // 关掉窗口：进度、取消、汇总都在队列窗口里
    if let Some(ui) = st.mirror_ui.borrow().as_ref() {
        ui.win.close();
    }

    if let Some(tx) = st.tx.borrow().as_ref() {
        let _ = tx.send(worker::WorkerCmd::Transfers { items: specs });
    }
}
