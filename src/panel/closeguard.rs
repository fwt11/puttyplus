//! 统一关闭流程（阶段 63 / 审计 P0-1）。
//!
//! **要治的病**：同一个「未保存的工作」，从不同入口关闭得到不同结果 ——
//! 文件编辑器子标签上的 × 会问「未保存的修改」，但关**连接标签**、关**窗口**、
//! 关**数据库子标签**都是直接关：用户改了半天的文件 / 数据说没就没，界面
//! 还不吭声。
//!
//! **做法**：所有关闭路径先过 [`collect_dirty`] 收集脏状态，再由
//! [`request_close`] 弹**同一个**确认框（保存并关闭 / 不保存关闭 / 取消），
//! 默认焦点落在「取消」——手快按回车不会把东西删了。
//!
//! ## 为什么「保存并关闭」是异步的
//!
//! 保存要走 worker 往返，而关闭会把 worker 通道丢掉（`panel::shutdown` 里
//! `tx.take()`）⇒ 不等回包齐了就关 = 白存。所以「保存并关闭」走
//! [`SaveBarrier`]：登记"还差几次回包"，等齐了再真正关。
//!
//! ⚠️ 关闭流程**只在主线程**跑（GUI 状态不许跨线程），worker 不受影响。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use super::PanelState;
use crate::i18n::tr;
use crate::trf;

/// 关闭前的脏状态统计。
///
/// 三类东西都可能"关掉就没了"：
/// - 文件编辑器里的未保存修改；
/// - 数据库里待保存的改动（数据行 / 表结构）；
/// - 正在跑的传输（关窗口会把它取消掉）。
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyInfo {
    /// 有未保存修改的文件编辑器（个数）
    pub edited_files: usize,
    /// 有待保存改动的数据表（个数）
    pub db_edits: usize,
    /// 有待保存改动的表结构（个数）
    pub db_struct_edits: usize,
    /// 正在跑或排队的传输（个数）
    pub running_xfers: usize,
}

impl DirtyInfo {
    /// 合并多个面板的统计（关窗口时要一次看完所有标签）。
    pub fn merge(&mut self, o: DirtyInfo) {
        self.edited_files += o.edited_files;
        self.db_edits += o.db_edits;
        self.db_struct_edits += o.db_struct_edits;
        self.running_xfers += o.running_xfers;
    }

    /// 什么都没有 ⇒ 不用打扰用户，直接关。
    pub fn is_clean(&self) -> bool {
        self.total() == 0
    }

    /// 需要提醒的项数。
    pub fn total(&self) -> usize {
        self.edited_files + self.db_edits + self.db_struct_edits + self.running_xfers
    }

    /// 有"能保存"的东西 ⇒ 才给「保存并关闭」这个选项。
    ///
    /// ⚠️ 传输**不算**：传输不是"未保存的工作"，它只是会被取消。
    pub fn savable(&self) -> bool {
        self.edited_files + self.db_edits + self.db_struct_edits > 0
    }

    /// 确认框正文（中文，逐条列清楚）。
    pub fn describe(&self) -> String {
        let mut s = String::new();
        if self.edited_files > 0 {
            s.push_str(&trf!("· {v0} 个文件有未保存的修改\n", v0 = self.edited_files));
        }
        if self.db_edits > 0 {
            s.push_str(&trf!("· {v0} 张数据表有待保存的改动\n", v0 = self.db_edits));
        }
        if self.db_struct_edits > 0 {
            s.push_str(&trf!("· {v0} 张表的结构有待保存的改动\n", v0 = self.db_struct_edits));
        }
        if self.running_xfers > 0 {
            s.push_str(&trf!("· 会取消 {v0} 个正在进行的传输\n", v0 = self.running_xfers));
        }
        s
    }
}

/// 用户在统一关闭确认框里的选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseChoice {
    /// 取消：什么都不做
    Cancel,
    /// 不保存，直接关
    Discard,
    /// 先保存再关
    Save,
}

/* ---------------- 保存闸门 ---------------- */

/// 「保存并关闭」的等待闸门。
///
/// 保存是 worker 往返 ⇒ 关之前必须等回包。等齐了才跑"继续关闭"。
pub struct SaveBarrier {
    /// 还差几次回包
    pending: Cell<usize>,
    /// 全齐之后要做什么（关窗口 / 关页签…）
    cont: RefCell<Option<Box<dyn FnOnce()>>>,
    /// 有保存**失败**了 ⇒ 不关（数据还在页面上，用户能救）
    failed: Cell<bool>,
}

impl SaveBarrier {
    fn new(n: usize) -> Rc<SaveBarrier> {
        Rc::new(SaveBarrier {
            pending: Cell::new(n),
            cont: RefCell::new(None),
            failed: Cell::new(false),
        })
    }

    fn note_done(&self) {
        if self.failed.get() {
            return;
        }
        let p = self.pending.get();
        if p == 0 {
            return;
        }
        let p = p - 1;
        self.pending.set(p);
        if p == 0 {
            // ⚠️ "继续关闭"在这里**同步**跑：调用方（文件编辑器的本地保存、
            // 数据库的回包）负责决定要不要延后 —— 它们各自有不能重入的理由
            // （本地保存在批量循环里、数据库回包里正借着 `db_ui`）。
            let c = self.cont.borrow_mut().take();
            if let Some(c) = c {
                c();
            }
        }
    }

    fn note_failed(&self) {
        self.failed.set(true);
    }
}

thread_local! {
    /// 当前正在等的保存闸门（同一时刻只有一个关闭流程在跑）。
    static BARRIER: RefCell<Option<Rc<SaveBarrier>>> = const { RefCell::new(None) };
}

/// 登记"还差 n 次回包"，并挂上"都回来之后要做什么"。
///
/// n == 0 时直接跑 `cont`（没什么好等的）。
pub fn arm_save(n: usize, cont: Box<dyn FnOnce()>) {
    if n == 0 {
        cont();
        return;
    }
    let b = SaveBarrier::new(n);
    *b.cont.borrow_mut() = Some(cont);
    BARRIER.with(|slot| *slot.borrow_mut() = Some(b));
}

/// 一个保存回来了。
pub fn note_save_done() {
    let b = BARRIER.with(|slot| slot.borrow().clone());
    if let Some(b) = b {
        if b.pending.get() == 0 {
            // 链走完了 ⇒ 撤掉闸门（后面再有回包也不该再触发关闭）
            BARRIER.with(|slot| *slot.borrow_mut() = None);
        }
        b.note_done();
    }
}

/// 一个保存回来了，但**延后一拍**再记。
///
/// ⚠️ 本地文件的保存是**同步**完成的（写盘就在 `save()` 里），报告若同步发，
/// 会插进"批量保存"的循环中间：面板先被关掉，循环还在往已经关掉的编辑器
/// 上写。延后到下一个主循环迭代 ⇒ 循环已经跑完了。
pub fn note_save_done_deferred() {
    glib::idle_add_local_once(|| {
        note_save_done();
    });
}

/// 一个保存**失败**了 ⇒ 这次关闭作废（页签留着，用户能救数据）。
pub fn note_save_failed() {
    let b = BARRIER.with(|slot| slot.borrow().clone());
    if let Some(b) = b {
        b.note_failed();
        BARRIER.with(|slot| *slot.borrow_mut() = None);
    }
}

/// 闸门是否还挂着（测试钩子用）。
pub fn save_barrier_pending() -> usize {
    BARRIER.with(|slot| slot.borrow().as_ref().map(|b| b.pending.get()).unwrap_or(0))
}

/* ---------------- 脏状态收集 ---------------- */

/// 收集面板的脏状态（只读，不改任何东西）。
pub fn collect_dirty(st: &Rc<PanelState>) -> DirtyInfo {
    let mut d = DirtyInfo::default();
    d.edited_files = super::fileedit::dirty_count(st);
    d.db_edits = super::dbui::pending_edit_count(st);
    d.db_struct_edits = super::dbui::pending_struct_edit_count(st);
    d.running_xfers = super::running_xfer_count(st);
    d
}

/// 收集多个面板的脏状态（关窗口时一次看完所有标签）。
pub fn collect_dirty_many(panels: &[Rc<PanelState>]) -> DirtyInfo {
    let mut d = DirtyInfo::default();
    for p in panels {
        d.merge(collect_dirty(p));
    }
    d
}

/* ---------------- 确认框 ---------------- */

const DISCARD: gtk::ResponseType = gtk::ResponseType::Other(1);
const SAVE: gtk::ResponseType = gtk::ResponseType::Other(2);

/// 弹统一确认框（**同步**：模态 + 嵌套主循环）。
pub fn ask(parent: Option<&gtk::Window>, info: &DirtyInfo) -> CloseChoice {
    let mut buttons: Vec<(&str, gtk::ResponseType)> =
        vec![(tr("取消"), gtk::ResponseType::Cancel)];
    if info.savable() {
        buttons.push((tr("不保存关闭"), DISCARD));
        buttons.push((tr("保存并关闭"), SAVE));
    } else {
        buttons.push((tr("关闭"), DISCARD));
    }
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(tr("确认关闭"), parent, &buttons);
    let body = trf!("有未保存的工作：\n\n{v0}", v0 = info.describe());
    let lbl = gtk::Label::new(Some(&body));
    lbl.set_wrap(true);
    lbl.set_xalign(0.0);
    lbl.set_max_width_chars(48);
    let ca = dlg.content_area();
    ca.set_margin_top(12);
    ca.set_margin_bottom(12);
    ca.set_margin_start(12);
    ca.set_margin_end(12);
    ca.append(&lbl);
    // ⚠️ 默认焦点落在「取消」：手快按回车不该把没保存的东西丢掉
    dlg.set_default_response(gtk::ResponseType::Cancel);
    super::dialogs::style_dialog(&dlg);
    // ⚠️ `style_dialog` 按**文案**认主操作：「不保存关闭」里也有"保存"两个字
    // ⇒ 它和「保存并关闭」一起被涂成主色（两个蓝按钮）。这里自己定角色：
    // 「不保存关闭」= 丢东西 ⇒ 危险色；「保存并关闭」= 保留工作 ⇒ 主色。
    if let Some(b) = dlg.widget_for_response(DISCARD).and_downcast::<gtk::Button>() {
        b.remove_css_class("suggested-action");
        b.add_css_class("destructive-action");
    }
    let resp = super::dialogs::run_modal(&dlg);
    dlg.destroy();
    match resp {
        SAVE => CloseChoice::Save,
        DISCARD => CloseChoice::Discard,
        _ => CloseChoice::Cancel,
    }
}

/* ---------------- 统一入口 ---------------- */

/// 关闭时要保存**哪些**东西。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveScope {
    /// 全部（文件编辑器的改动 + 数据库的改动）
    All,
    /// 只有数据库的改动（关数据库子标签时：文件编辑器不归这条路径管）
    DbOnly,
}

/// 关闭流程的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseFlow {
    /// 可以继续关（窗口可以关了 / 目标已经处理掉）
    Proceed,
    /// 别关：用户取消了
    Stop,
    /// 正在保存：等保存的回包齐了再关（回调 `later` 会再来一次）
    Saving,
}

/// 关闭前的统一确认（自己算脏状态）。
pub fn request_close<F: FnOnce() + 'static>(st: &Rc<PanelState>, proceed: F) -> bool {
    request_close_with(st, collect_dirty(st), SaveScope::All, proceed)
}

/// 关闭前的统一确认（脏状态由调用方给 —— 只想问其中一部分时用）。
pub fn request_close_with<F: FnOnce() + 'static>(
    st: &Rc<PanelState>,
    info: DirtyInfo,
    scope: SaveScope,
    proceed: F,
) -> bool {
    if info.is_clean() {
        proceed();
        return true;
    }
    let parent = st.topwin();
    match ask(parent.as_ref(), &info) {
        CloseChoice::Cancel => false,
        CloseChoice::Discard => {
            proceed();
            true
        }
        CloseChoice::Save => {
            // 先发起保存（拿到"实际发起了几次"），再挂闸门。
            //
            // ⚠️ 顺序说明：保存的**回包**要么是 worker 往返（异步），
            // 要么被 `note_save_done_deferred` 延后到下一个主循环迭代 ⇒
            // 这里"先发起、后挂闸门"不会漏掉回包。
            let nf = match scope {
                SaveScope::All => super::fileedit::start_save_all_dirty(st),
                SaveScope::DbOnly => 0,
            };
            let nd = super::dbui::start_save_all_pending(st);
            let n = nf + nd;
            if n == 0 {
                // 一个都存不了（只读 / 没连上库）⇒ 别假装保存过
                proceed();
                return true;
            }
            arm_save(n, Box::new(proceed));
            true
        }
    }
}

/// 多个面板的版本（关窗口：一次问完所有标签，别弹一串框）。
///
/// 返回 [`CloseFlow`]：**窗口**这一类"由外面决定要不要关"的调用方要看它
/// ——选「保存并关闭」时保存还没完，窗口不能现在就关（关了 worker 就死了，
/// 保存全丢）。
pub fn request_close_many<F: FnOnce() + 'static>(
    panels: &[Rc<PanelState>],
    proceed: F,
) -> CloseFlow {
    let info = collect_dirty_many(panels);
    if info.is_clean() {
        proceed();
        return CloseFlow::Proceed;
    }
    let parent = panels.first().and_then(|p| p.topwin());
    match ask(parent.as_ref(), &info) {
        CloseChoice::Cancel => CloseFlow::Stop,
        CloseChoice::Discard => {
            proceed();
            CloseFlow::Proceed
        }
        CloseChoice::Save => {
            let mut n = 0;
            for p in panels {
                n += super::fileedit::start_save_all_dirty(p);
                n += super::dbui::start_save_all_pending(p);
            }
            if n == 0 {
                proceed();
                return CloseFlow::Proceed;
            }
            arm_save(n, Box::new(proceed));
            // 保存还没完 ⇒ 别关
            CloseFlow::Stop
        }
    }
}

/// 关**窗口**用：一次问完所有标签。
///
/// 与 [`request_close_many`] 的区别：`now` 与 `later` 分开。
/// - `now`：本来就干净、或者用户选了「不保存关闭」⇒ 立刻跑；
/// - `later`：用户选了「保存并关闭」⇒ 等保存的回包齐了再跑。
///
/// ⚠️ 分开是必要的：窗口这一路，"立刻关"是**让窗口自己关**（返回 Proceed），
/// 而"保存完再关"得**主动**去关窗口（返回 Stop，等回调里再关一次）。
/// 混成一个回调的话，主动关窗会递归回自己。
pub fn request_close_window<F: FnOnce() + 'static, G: FnOnce() + 'static>(
    panels: &[Rc<PanelState>],
    now: F,
    later: G,
) -> CloseFlow {
    let info = collect_dirty_many(panels);
    if info.is_clean() {
        now();
        return CloseFlow::Proceed;
    }
    let parent = panels.first().and_then(|p| p.topwin());
    match ask(parent.as_ref(), &info) {
        CloseChoice::Cancel => CloseFlow::Stop,
        CloseChoice::Discard => {
            now();
            CloseFlow::Proceed
        }
        CloseChoice::Save => {
            let mut n = 0;
            for p in panels {
                n += super::fileedit::start_save_all_dirty(p);
                n += super::dbui::start_save_all_pending(p);
            }
            if n == 0 {
                now();
                return CloseFlow::Proceed;
            }
            arm_save(n, Box::new(later));
            CloseFlow::Saving
        }
    }
}

/* ---------------- 单测 ---------------- */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 干净状态不用打扰用户() {
        let d = DirtyInfo::default();
        assert!(d.is_clean());
        assert_eq!(d.total(), 0);
        assert!(!d.savable());
        assert_eq!(d.describe(), "");
    }

    #[test]
    fn 传输不算可保存项() {
        let d = DirtyInfo {
            running_xfers: 3,
            ..Default::default()
        };
        assert!(!d.is_clean());
        assert!(!d.savable());
        assert!(d.describe().contains("会取消 3 个正在进行的传输"));
    }

    #[test]
    fn 统计能合并() {
        let mut a = DirtyInfo {
            edited_files: 1,
            running_xfers: 2,
            ..Default::default()
        };
        a.merge(DirtyInfo {
            edited_files: 2,
            db_edits: 1,
            ..Default::default()
        });
        assert_eq!(a.edited_files, 3);
        assert_eq!(a.db_edits, 1);
        assert_eq!(a.running_xfers, 2);
        assert_eq!(a.total(), 6);
        assert!(a.savable());
    }

    #[test]
    fn 正文逐条列清楚() {
        let d = DirtyInfo {
            edited_files: 2,
            db_edits: 1,
            db_struct_edits: 1,
            running_xfers: 4,
        };
        let s = d.describe();
        assert!(s.contains("2 个文件有未保存的修改"));
        assert!(s.contains("1 张数据表有待保存的改动"));
        assert!(s.contains("1 张表的结构有待保存的改动"));
        assert!(s.contains("会取消 4 个正在进行的传输"));
    }

    #[test]
    fn 闸门等齐了才继续() {
        let hit = Rc::new(Cell::new(false));
        let h2 = Rc::clone(&hit);
        arm_save(2, Box::new(move || h2.set(true)));
        assert_eq!(save_barrier_pending(), 2);
        note_save_done();
        assert!(!hit.get(), "还没齐就不该继续");
        note_save_done();
        assert!(hit.get(), "齐了就该继续");
        assert_eq!(save_barrier_pending(), 0);
    }

    #[test]
    fn 保存失败就不继续() {
        let hit = Rc::new(Cell::new(false));
        let h2 = Rc::clone(&hit);
        arm_save(1, Box::new(move || h2.set(true)));
        note_save_failed();
        note_save_done();
        assert!(!hit.get(), "保存失败了不该关");
        assert_eq!(save_barrier_pending(), 0);
    }
}
