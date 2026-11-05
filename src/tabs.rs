//! 多标签管理：窗口主体 = 会话侧边栏 + Notebook；每个标签 = 一套
//! SFTP 面板 + 终端。零业务标签时显示欢迎页（空状态）。

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::gdk;
use gtk::prelude::*;
use vte4::TerminalExt;

use crate::args::ConnOptions;
use crate::connmgr;
use crate::panel;
use crate::recent;
use crate::term::{self, TermPane};
use crate::i18n::tr;
use crate::trf;

/// 标签头的标题 Label（阶段 75 / 审计 P1-14）。
///
/// ⚠️ **不要用 `width_chars(10)`**（原来那么写）：GTK4 里 `width-chars`
/// 把标签的**自然宽**也定成 10 个字符 ⇒ 标题再短也占 10 个字的宽、再长也
/// 只显示 10 个字（看着像 `ops@127.0...`）——单标签、满屏空白时也是那样，
/// `max_width_chars(18)` 根本没机会生效。
///
/// 现在改成**上限 + 显式最小宽**：
/// - `max_width_chars(18)`：再长也只到 18 个字，超出用省略号；
/// - `set_size_request(64, -1)`：显式最小宽（px）——Label 的最小宽度
///   默认是 0，页签挤一挤就会整个变成「… ×」，用户认不出哪个是哪个。
///
/// 单独抽成函数，冒烟才量得到它的宽度（标签本身要真会话才能建出来）。
pub fn tab_title_label(title: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(title));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(18);
    // ⚠️ **尺寸请求要跟着文字走**：页签里给标签分的宽度**就是**这个尺寸
    //    请求（实测：自然宽 80px 的 "demo-server" 只分到 64px ⇒ 被截成
    //    「demo-s…」）。固定 64px ⇒ 长标题一律只剩几个字。
    //    ⇒ 先量自然宽（`max_width_chars(18)` 已经把上限卡住了），再拿它
    //    当尺寸请求；下限 64px：页签挤一挤也不至于整个变成「… ×」。
    let (_, nat, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
    label.set_size_request(nat.max(64), -1);
    label.set_xalign(0.0);
    label
}

/// 一个业务标签（欢迎页不在其中）。
pub struct Tab {
    pub page: gtk::Widget,
    pub panel_st: Rc<panel::PanelState>,
    /// 该标签的终端（Ctrl+F 搜索用）
    pub term: vte4::Terminal,
    /// 标签头（远程经 OSC 0/2 设置标题时更新文字）
    pub label: gtk::Label,
    /// 标签头左侧的连接状态点（颜色跟面板状态走）
    pub dot: gtk::Label,
    /// **主终端子页内**的左右分割（左＝SFTP 文件面板 / 右＝终端）。
    ///
    /// 阶段 97 起，连接页本身不再是 Paned（只有子标签区）⇒ 这个字段指的是
    /// 终端子页里那个「文件面板｜终端」的 Paned。
    pub paned: gtk::Paned,
    /// 3.1 分屏：首个终端 pane（日志/录制/跟随目录默认作用于它；
    /// 它被关掉时会顺延到剩下的第一个 pane）
    pub term_pane: TermPane,
    /// 面板树（3.1）：叶子是终端，Split 是左右/上下切分。
    /// 每个面板都能再分，所以这里是一棵二叉树而不是"一个分屏容器"。
    ///
    /// ⚠️ **阶段 37：每个终端子页签一棵**（key = 子页签 id）。
    /// 之前只有 Tab 级的一棵，额外终端子页签的面板压根不在树里——
    /// 于是那些页签上点「左右分屏」永远没反应（不在树里就被守卫拦下）。
    pub layouts: RefCell<Vec<(u64, PaneLayout)>>,
    /// 「终端」子页签的 id（阶段 25）。
    ///
    /// **为什么要记**：用户可以关掉「终端」子页签（子标签上的 ×）。这时
    /// 终端对象还在，但**看不见**——「重新连接终端」得靠这个 id 知道
    /// 该重建哪一页，否则重连的是一个用户看不到的会话（点了像没反应）。
    pub term_sub_id: Cell<u64>,
    /// 额外的终端子页签（子标签栏「+」新建的那些）。
    ///
    /// **和 `layout` 的区别**：`layout` 是"这一页里左右/上下分屏"的树，
    /// 终端都在同一块地方；这里记的是**平级的另一个终端页签**，各自一条
    /// 独立 SSH 会话（像 XTerminal 那样一个终端一页）。
    pub term_pages: RefCell<Vec<(u64, TermPane)>>,
    /// 当前聚焦的面板（决定"往哪块分"以及高亮哪块）
    pub active: Rc<RefCell<Option<TermPane>>>,
}

/// 终端子页签的内容：一个 **Paned 容器**。
///
/// **为什么要容器**（阶段 37）：分屏要把目标面板塞进一个新的 Paned，而
/// `split_index` 要求「目标面板的父容器本身就是一个 Paned」——
/// 阶段 12 之后终端被放进**子标签**，父容器是 notebook 的页面容器（Box），
/// 于是分屏**静默失败**：点「左右分屏」看着毫无反应（`split_index` 里
/// `parent.and_downcast::<gtk::Paned>()` 拿不到就 `return`）。
///
/// 从一开始就给它一个 Paned 父容器，分屏那条路径就不用改。
///
/// GTK4 的 `Paned` 只有一个子控件时，子控件占满整个区域（实测），
/// 所以外面多这一层看不出来。
fn term_container(pane: &TermPane) -> gtk::Paned {
    let p = gtk::Paned::new(gtk::Orientation::Horizontal);
    p.set_start_child(Some(pane.widget()));
    p
}

/// 把文件面板**搬到**某个终端子页的 Paned 里（阶段 97）。
///
/// ⚠️ **GTK4：一个 widget 只能有一个父级** ⇒ 搬家必须先把旧框里的槽位
/// 清掉，再挂新的框，否则 GTK 报 `gtk_paned_set_start_child: assertion
/// 'child->parent == NULL' failed`（AGENTS.md 记过同类坑：「同一个 widget
/// 别 append 进两个 Box」）。
///
/// **为什么走 Paned 的槽位 API**（而不是直接 `w.unparent()`）：
/// `set_start_child(None)` 会让 GTK **自己**把旧孩子解除父级并清掉槽位
/// ⇒ 旧框不会留着一个野指针（它析构时不会顺手把孩子销毁掉）。
///
/// **为什么文件面板要搬家**：它是**会话级的一个 widget**（`PanelState::root`），
/// 而一个连接页里可以有好几个终端页签 ⇒ 文件面板跟着**当前**终端页签走，
/// 切到哪个终端页签，它就站哪个页签的左边。
///
/// **宽度**：搬家后按 `st.file_w`（跨宿主同步的那个值）复位。
fn host_file_panel(st: &Rc<panel::PanelState>, host: &gtk::Paned) {
    let w = &st.root; // 文件面板 widget
    if let Some(old) = w.parent().and_downcast::<gtk::Paned>() {
        // 旧宿主先把槽位让出来（GTK 顺带把文件面板解除父级）
        let me = w.upcast_ref::<gtk::Widget>();
        if old.start_child().as_ref() == Some(me) {
            old.set_start_child(None::<&gtk::Widget>);
        } else if old.end_child().as_ref() == Some(me) {
            old.set_end_child(None::<&gtk::Widget>);
        }
    } else if w.parent().is_some() {
        // 不是 Paned 的父级（少见）⇒ 直接解除父级
        w.unparent();
    }
    host.set_start_child(Some(w));       // 放进新框
    host.set_position(st.file_w.get());  // 宽度按记忆值复位
}

/// 让宿主的分割条位置**同步回** `file_w`（搬家之后宽度得跟着走）。
///
/// ⚠️ **只有当前正托管文件面板的那个框才算数**：别的框 position=0，
/// 照样写回会把记住的宽度冲成 0（用户下次打开就变成默认宽度了）。
fn track_file_width(st: &Rc<panel::PanelState>, host: &gtk::Paned) {
    let stw = Rc::downgrade(st);
    host.connect_notify_local(Some("position"), move |p, _| {
        let Some(st) = stw.upgrade() else { return };
        if st.root.parent().as_ref() == Some(p.upcast_ref::<gtk::Widget>()) {
            st.file_w.set(p.position());
        }
    });
}

/// 装"会话存活"钩子（阶段 65）：终端起来 / 结束 → 面板状态跟着变。
///
/// ⚠️ 闭包只抓**弱引用**（AGENTS.md 的引用环红线）。
fn install_alive_hook(tp: &TermPane, st: &Rc<panel::PanelState>) {
    let stw = Rc::downgrade(st);
    tp.set_alive_hook(move |alive| {
        if let Some(st) = stw.upgrade() {
            panel::note_term_alive(&st, alive);
        }
    });
}

/// 算一个面板的状态点（级别 + tooltip）。
///
/// **聚合**三个维度：文件面板（SFTP）、终端、数据库（阶段 65）。
fn dot_state(st: &Rc<panel::PanelState>) -> (Option<crate::theme::State>, String) {
    let sftp = panel::current_state(st);
    let term = panel::term_state(st);
    let db = st.db_active.borrow().clone();
    (
        crate::active::aggregate(sftp, term, db.is_some()),
        crate::active::state_tooltip(sftp, term, db.as_deref()),
    )
}

/// 刷新一个标签的状态点（**聚合** SFTP + 终端 + 数据库，阶段 65）。
///
/// 终端已结束 ⇒ 不再是绿点；tooltip 说清是哪一部分出的问题。
fn update_dot(dot: &gtk::Label, st: &Rc<panel::PanelState>) {
    let (level, tip) = dot_state(st);
    match level {
        // 形状跟着状态走：● 已连 / ◐ 连接中（不只靠颜色，审计 P2-6）
        Some(level) => dot.set_markup(&format!("<span foreground='{}'>{}</span>",
            crate::theme::state_hex(level),
            crate::active::dot_char(Some(level))
        )),
        // 还没有任何状态 ⇒ 灰的**空心**圈（"还没连上"）
        None => dot.set_markup("<span foreground='#9aa0a6'>○</span>"),
    }
    dot.set_tooltip_text(Some(&tip));
}

/// 冒烟 / e2e 用：算某个面板的状态点（级别 + tooltip）。
pub fn dot_state_for(st: &Rc<panel::PanelState>) -> (Option<crate::theme::State>, String) {
    dot_state(st)
}

/// 冒烟 / e2e 用：把聚合状态画到状态点标签上（**真实**的那段逻辑）。
pub fn apply_dot(dot: &gtk::Label, st: &Rc<panel::PanelState>) {
    update_dot(dot, st);
}

/// 刷新**所有窗口**里标签的状态点（阶段 65）。
pub fn refresh_dots() {
    ALL_TABS.with(|v| {
        let list: Vec<std::rc::Weak<Tabs>> = v.borrow().clone();
        for w in list {
            let Some(t) = w.upgrade() else { continue };
            for tab in t.tabs.borrow().iter() {
                update_dot(&tab.dot, &tab.panel_st);
            }
        }
    });
}

/// 一个标签里的终端面板布局（3.1）。
pub enum PaneLayout {
    /// 终端面板（各自独立的 SSH 会话）
    Leaf(TermPane),
    /// 左右/上下切分：起点=左/上，终点=右/下
    Split {
        paned: gtk::Paned,
        a: Box<PaneLayout>,
        b: Box<PaneLayout>,
        /// true = 上下堆叠
        stacked: bool,
    },
    /// 只在摘除节点时短暂出现的占位
    Empty,
}

fn same_pane(a: &TermPane, b: &TermPane) -> bool {
    Rc::ptr_eq(a.handle(), b.handle())
}

fn leaf_is(layout: &PaneLayout, target: &TermPane) -> bool {
    matches!(layout, PaneLayout::Leaf(p) if same_pane(p, target))
}

impl PaneLayout {
    pub fn leaves(&self, out: &mut Vec<TermPane>) {
        match self {
            PaneLayout::Leaf(p) => out.push(p.clone()),
            PaneLayout::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
            PaneLayout::Empty => {}
        }
    }

    pub fn count(&self) -> usize {
        let mut v = Vec::new();
        self.leaves(&mut v);
        v.len()
    }

    /// 树里有没有这块面板。
    pub fn contains(&self, target: &TermPane) -> bool {
        let mut v = Vec::new();
        self.leaves(&mut v);
        v.iter().any(|p| same_pane(p, target))
    }

    pub fn first(&self) -> Option<TermPane> {
        let mut v = Vec::new();
        self.leaves(&mut v);
        v.into_iter().next()
    }

    pub fn widget(&self) -> Option<gtk::Widget> {
        match self {
            PaneLayout::Leaf(p) => Some(p.widget().clone()),
            PaneLayout::Split { paned, .. } => Some(paned.clone().upcast()),
            PaneLayout::Empty => None,
        }
    }
}

impl Tab {
    /// 某个面板属于**哪个终端子页签**（= 它在哪棵布局树里）。
    ///
    /// 阶段 37：布局树按子页签分组之后，「这个面板归谁管」就得现查——
    /// 右键菜单按的是"右键哪块"，不能假设它一定是主终端。
    fn sub_id_of_pane(&self, pane: &TermPane) -> Option<u64> {
        self.layouts
            .borrow()
            .iter()
            .find(|(_, l)| l.contains(pane))
            .map(|(id, _)| *id)
    }

    /// 某棵布局树（某个终端子页签）的叶子。
    fn leaves_of(&self, sub_id: u64) -> Vec<TermPane> {
        let mut v = Vec::new();
        if let Some((_, l)) = self.layouts.borrow().iter().find(|(id, _)| *id == sub_id) {
            l.leaves(&mut v);
        }
        v
    }

    /// 全部布局树的叶子（广播、重连、整页关闭用）。
    fn all_leaves(&self) -> Vec<TermPane> {
        let mut v = Vec::new();
        for (_, l) in self.layouts.borrow().iter() {
            l.leaves(&mut v);
        }
        v
    }
}

/// 把 `target` 叶子一分为二：`second` 放进新 Split。
fn split_in_tree(
    layout: &mut PaneLayout,
    target: &TermPane,
    paned: &gtk::Paned,
    second: TermPane,
    stacked: bool,
) -> bool {
    match layout {
        PaneLayout::Leaf(p) if same_pane(p, target) => {
            let old = std::mem::replace(layout, PaneLayout::Empty);
            let PaneLayout::Leaf(leaf) = old else {
                return false;
            };
            *layout = PaneLayout::Split {
                paned: paned.clone(),
                a: Box::new(PaneLayout::Leaf(leaf)),
                b: Box::new(PaneLayout::Leaf(second)),
                stacked,
            };
            true
        }
        PaneLayout::Split { a, b, .. } => {
            split_in_tree(a, target, paned, second.clone(), stacked)
                || split_in_tree(b, target, paned, second, stacked)
        }
        _ => false,
    }
}

/// 从树里摘掉 `target` 叶子，返回：被拆掉的容器、兄弟控件、
/// target 是否在起点侧、兄弟子树里的第一个叶子。
fn take_sibling(
    layout: &mut PaneLayout,
    target: &TermPane,
) -> Option<(gtk::Paned, gtk::Widget, bool, Option<TermPane>)> {
    let direct = match layout {
        PaneLayout::Split { a, b, .. } => leaf_is(a, target) || leaf_is(b, target),
        _ => false,
    };
    if direct {
        let PaneLayout::Split { paned, a, b, .. } =
            std::mem::replace(layout, PaneLayout::Empty)
        else {
            return None;
        };
        let target_is_a = leaf_is(&a, target);
        let keep = if target_is_a { *b } else { *a };
        let widget = keep.widget()?;
        let first = keep.first();
        *layout = keep;
        return Some((paned, widget, target_is_a, first));
    }
    match layout {
        PaneLayout::Split { a, b, .. } => {
            take_sibling(a, target).or_else(|| take_sibling(b, target))
        }
        _ => None,
    }
}

pub struct Tabs {
    pub notebook: gtk::Notebook,
    pub tabs: RefCell<Vec<Tab>>,
    /// 欢迎页是否挂载（有业务标签时移除，全部关闭时恢复）
    welcome_attached: Cell<bool>,
    /// 3.2 输入广播总开关（进程内，不持久化——重启后没人想被意外劫持按键）
    broadcast: Cell<bool>,
    /// 标签数量变化回调（顶栏主按钮据此在空状态隐藏，避免与欢迎页按钮重复）
    pages_hook: RefCell<Option<Box<dyn Fn(bool)>>>,
    /// 等着打开的数据库连接（阶段 13）：`(会话键, 连接名)`。
    ///
    /// **为什么要有这个**：数据库通道复用 SSH 连接，所以"打开数据库连接"
    /// 得先有那条 SSH 会话。用户在连接管理器里双击一个数据库连接时，
    /// 会话还没连上——连上之后再把工作台开出来。
    pending_db: RefCell<Option<(String, String)>>,
    /// 自弱引用（阶段 63：统一关闭流程要 `'static` 的回调）。
    ///
    /// **为什么不能抓强引用**：关闭确认的回调是 `'static` 的闭包，抓强引用
    /// 就成了 `Rc<Tabs> → 闭包 → Rc<Tabs>` 的环 ⇒ 窗口永远释放不掉。
    me: Weak<Tabs>,
}

impl Tabs {
    pub fn new() -> Rc<Tabs> {
        let notebook = gtk::Notebook::new();
        // pending_db 见字段注释
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);
        // 标签太多时可横向滚动（不挤成一团）
        notebook.set_scrollable(true);

        // 拖拽排序：notebook 的页序要与 tabs 向量一致——`close_page` /
        // `current_index` 全靠这个顺序换算页号
        {
            let nb = notebook.clone();
            nb.connect_page_reordered(move |nb, child, num| {
                let Some(t) = TABS.with(|w| w.borrow().clone()) else {
                    return;
                };
                let mut v = t.tabs.borrow_mut();
                let Some(from) = v.iter().position(|x| x.page == *child) else {
                    return;
                };
                // 欢迎页占第 0 页时，业务标签的页号要减 1
                let off = usize::from(nb.n_pages() as usize == v.len() + 1);
                let to = (num as usize)
                    .saturating_sub(off)
                    .min(v.len().saturating_sub(1));
                if from != to {
                    let item = v.remove(from);
                    v.insert(to, item);
                }
            });
        }

        let tabs = Rc::new_cyclic(|me: &Weak<Tabs>| {
            Tabs {
                notebook,
                tabs: RefCell::new(Vec::new()),
                welcome_attached: Cell::new(true),
                broadcast: Cell::new(false),
                pages_hook: RefCell::new(None),
                pending_db: RefCell::new(None),
                me: me.clone(),
            }
        });

        // 欢迎页（零业务标签时的空状态页；开首个标签时移除）
        let welcome_label = gtk::Label::new(Some(tr("欢迎")));
        tabs.notebook
            .append_page(&welcome_page(), Some(&welcome_label));

        // 标签栏不再放「+」：顶栏的全局主按钮就是"新建连接"，
        // 同屏只保留一个带文字的入口，标签栏保持干净。

        TABS.with(|w| *w.borrow_mut() = Some(Rc::clone(&tabs)));
        // 注册进全局表：连接管理器对话框要知道别的窗口里哪些连接是活的
        ALL_TABS.with(|v| v.borrow_mut().push(Rc::downgrade(&tabs)));
        tabs
    }

    /// 注册"是否有业务标签"的监听（顶栏主按钮的显隐）。
    pub fn set_pages_hook<F: Fn(bool) + 'static>(&self, f: F) {
        *self.pages_hook.borrow_mut() = Some(Box::new(f));
    }

    fn notify_pages(&self) {
        if let Some(f) = self.pages_hook.borrow().as_ref() {
            f(!self.tabs.borrow().is_empty());
        }
    }

    /// 打开一个数据库连接（阶段 13）：先把它的 SSH 会话连上，连上后开工作台。
    ///
    /// **为什么绕这一圈**：数据库通道是**复用** SSH 连接的（direct-tcpip
    /// 转发，不新建 SSH 连接、不占本地端口），所以没有那条 SSH 会话就没有
    /// 数据库通道。用户在连接管理器里双击数据库连接时，会话往往还没连上。
    pub fn open_db_conn(self: &Rc<Tabs>, key: &str, name: &str) {
        // 找这条会话键对应的 SSH 条目
        let Some(opts) = connmgr::find_opts_by_session_key(key) else {
            crate::panel::dialogs::show_error(
                None,
                tr("数据库连接"),
                tr("找不到这条数据库连接对应的 SSH 会话（那条会话不在连接列表里了）。\n\n先在会话树里连上它，再打开数据库工作台。"),
            );
            return;
        };
        // 记下"连上之后要开它"
        *self.pending_db.borrow_mut() = Some((key.to_string(), name.to_string()));
        self.open(&opts);
    }

    /// SSH 连上了 → 如果有等着开的数据库连接，现在把它开出来。
    fn maybe_open_pending_db(&self, st: &Rc<panel::PanelState>) {
        let pending = self.pending_db.borrow().clone();
        let Some((key, name)) = pending else { return };
        // 只认同一个会话：别的标签连上时不该在这里开工作台
        if crate::backend::dbstore::session_key(&st.opts) != key {
            return;
        }
        *self.pending_db.borrow_mut() = None;
        // 只开页面，**不连**：连哪条由 use_profile 决定（双击哪条连哪条）
        panel::on_open_db_page(st);
        crate::panel::dbui::use_profile(st, &key, &name);
    }

    /// 用给定连接参数直接开新标签。
    pub fn open(&self, opts: &ConnOptions) {
        // 会话级覆盖（1.5）：先落到连接参数，再分发给终端与 SFTP 面板。
        // 这里是唯一的总入口——命令行、侧边栏、连接管理器都走它。
        let mut opts = opts.clone();
        crate::settings::apply_session_override(&opts.session_key(), &mut opts);
        let opts = &opts;
        // 有业务标签后欢迎页没有存在价值——开首个标签时移除
        if self.welcome_attached.get() {
            self.notebook.remove_page(Some(0));
            self.welcome_attached.set(false);
        }
        // **阶段 97：连接页 = 子标签区（整页宽）**——
        // 以前这里是个横向 Paned：左＝常驻文件面板、右＝子标签区，
        // 切到数据库页时把左边的文件面板藏起来 ⇒ GTK4 的 Paned 把空间全给
        // 右边 ⇒ **子标签栏整体左移**（用户 2026-11-05 截图报的就是这个）。
        // 现在文件面板进了**终端子页内部**，切页 = 整页替换 ⇒ 子标签栏不动。
        let sub = panel::subtabs::SubTabs::new();

        let sp = panel::SftpPanel::new(opts);
        // 每个标签一个独立终端：spawn 的就是 ssh/plink 本身，
        // 子进程环境与 C 版基线一致（compat_check 第 3 段比对的就是它）。
        let tp = TermPane::new(opts);
        {
            let st = sp.st.clone();
            tp.set_cwd_callback(move |uri| panel::notify_cwd_uri(&st, uri));
        }
        // 阶段 65：会话起来 / 结束 → 状态点跟着变（终端掉了不再是绿点）
        install_alive_hook(&tp, &sp.st);
        panel::set_term_spawn(&sp.st, tp.spawn_time());
        panel::set_term(&sp.st, &tp.terminal());
        // 拖文件到终端 = 上传到当前远程目录
        panel::enable_drop_upload(&sp.st, tp.terminal());

        // 终端子页的内容 = 「文件面板｜终端」的横向 Paned。
        //
        // ⚠️ 终端面板的父容器**仍然是** `term_container()` 那层 Paned
        //（分屏逻辑要求「目标面板的父容器是 Paned」——AGENTS.md 记过这个
        // 隐式契约），所以外面这层 Paned 只管文件面板的宽度。
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        let term_wrap = term_container(&tp);
        term_page.set_end_child(Some(term_wrap.upcast_ref::<gtk::Widget>()));
        // 面板宽度：用上次会话记忆（默认 340：280 时工具栏 + 路径框挤在一起，
        // 路径永远只显示后半截）
        let pos = crate::settings::sftp_pane_pos();
        let init_w = if pos > 120 { pos } else { 340 };
        sp.st.file_w.set(init_w);
        // 文件列表的宽度上限：终端才是主角（最多 38% 或 520px）
        crate::settings::clamp_paned_start(&term_page, 0.38, 520);
        term_page.set_hexpand(true);
        term_page.set_vexpand(true);
        // 文件面板的家：先记它的宿主（宽度变化要同步回 file_w），再挂进去
        track_file_width(&sp.st, &term_page);
        host_file_panel(&sp.st, &term_page);
        // 终端页签带图标：主标签头已经不放图标了，"这一页是终端"由子页签
        // 自己的图标说（标签头只留状态点 + 标题 + ×）。
        let term_sub_id = sub.add_page_icon_kind(
            Some("utilities-terminal-symbolic"),
            tr("终端"),
            panel::subtabs::PageKind::Terminal,
            term_page.clone().upcast(),
            None,
        );
        *sp.st.sub_tabs.borrow_mut() = Some(sub.clone());
        // 阶段 97：**切到「终端」页就把文件面板搬过去**。
        //
        // 文件面板是**会话级的一个 widget**，但一个连接页里可以有好几个终端
        // 页签 ⇒ 它跟着**当前**终端页签走（这样每个终端页签左边都有文件
        // 面板，关掉一个终端页签也不会把文件面板弄丢）。
        //
        // ⚠️ 抓**弱引用**：notebook 的信号处理器不能抓着 SubTabs / 面板的
        // 强引用（引用环 ⇒ 面板永远释放不掉）。
        {
            let subw = Rc::downgrade(&sub);
            // ⚠️ 面板状态也只抓**弱引用**：面板状态里挂着子标签容器，
            // 抓强引用就成环（面板永远释放不掉）。
            let stw = Rc::downgrade(&sp.st);
            // ⚠️ 用 **`switch-page`** 信号，不是 `notify::current-page`——
            // GTK4 的 `GtkNotebook:current-page` **改了也不发通知**
            // （实测：set_current_page 之后 notify 回调一次都不跑）。
            //
            // ⚠️ `switch-page` 是**切换之前**发的 ⇒ 那时 `current_kind()`
            // 还是旧页的 ⇒ 得按传进来的**页序号**查种类。
            //
            // ⚠️ gtk4-rs 0.11 的 `connect_switch_page` 回调**没有返回值**
            // （GTK 那边返回 false 可以挡住切换，这套用不上—we 不需要挡）。
            sub.nb.connect_switch_page(move |_nb, _page, num| {
                let Some(sub) = subw.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                // 只有切到「终端」页才搬文件面板；切到数据库/编辑器页**不动**它
                //（它留在上一个终端页里，随那一页一起隐藏）。
                if sub.kind_by_page_num(num) != panel::subtabs::PageKind::Terminal {
                    return;
                }
                // 拿不到宿主就**静默不动**：界面不崩、不空半个页面。
                let Some(host) = sub.paned_by_page_num(num) else { return };
                host_file_panel(&st, &host);
            });
        }
        // 连接页控件 = **子标签区的外层容器**（notebook + 尾部的「+」按钮），
        // 不是裸的 notebook —— 「+」要落在子标签栏那一行的最右端。
        let page = sub.root().clone();
        page.set_hexpand(true);
        page.set_vexpand(true);

        let title = match opts.title.as_deref() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => format!("{}@{}", opts.user, opts.host),
        };

        let page_w = page.clone();

        // ---- 子标签栏尾部的「+」：新建终端 / 新建数据库连接 ----
        //
        // 用户要求把「建立新的 term 或数据库连接」放在**子标签栏同一行的
        // 最右端**（不是主标签头上——那儿已经不放图标了）。
        {
            let t = TABS.with(|w| w.borrow().clone());
            let t_hook = t.clone();
            // ⚠️ **弱引用**（AGENTS.md 引用环红线）：钩子存在 SubTabs 里，
            // 而 SubTabs 又挂在 `sp.st` 里 ⇒ 抓强引用 = PanelState ⇄ SubTabs
            // 互相引用，面板永远释放不掉。
            let st_w = Rc::downgrade(&sp.st);
            // ⚠️ 页面控件（Paned）**也**只能抓弱引用：Paned 的右孩子就是
            // SubTabs 的根控件 ⇒ 抓强引用 = Paned ⇄ SubTabs 的环，
            // 连接页关了也不回收。
            let pg_w = page_w.downgrade();
            sub.set_new_hook(move |vb, pop| {
                let Some(t) = t_hook.as_ref() else { return };
                let Some(pg) = pg_w.upgrade() else { return };
                // 会话预置死（从哪儿来就属于哪儿）⇒ 动作落在**它自己的**会话里，
                // 不是"当前会话"（用户切到别的标签页再点就不该建到那边去）。
                let Some(st) = st_w.upgrade() else { return };
                // ⚠️ **菜单项从「新建」动作表取**（阶段 68）：子标签的 `+`
                // 只给"会话内能做"的（新建终端 / 新建数据库连接），
                // 动作的**实现**只有一份（`newmenu::run_in`）。
                let t2 = t.clone();
                let pg2 = pg.clone();
                let pop2 = pop.clone();
                crate::newmenu::build_menu(
                    crate::topmenus::NewEntry::SubTab,
                    vb,
                    crate::newmenu::NewCtx::with_session(),
                    move |action| {
                        pop2.popdown();
                        match action {
                            // 新建终端：在**这个连接**里再开一条独立的 SSH 会话
                            "new_term" => t2.new_term_page(&pg2),
                            // 新建数据库连接：落在**这个连接**的会话里
                            other => crate::newmenu::run_in_session(&t2, &st, other),
                        }
                    },
                );
            });
            // 子页签被 × 关掉时通知：**终端页签 → 收掉它的 SSH 会话**。
            // （数据库页签的断开由 dbui 自己注册——谁的东西谁管。）
            let t2 = t.clone();
            let pg2 = page_w.clone();
            sub.add_close_hook(move |id| {
                if let Some(t) = t2.as_ref() {
                    t.on_term_page_closed(&pg2, id);
                }
            });
        }

        // ---- 标签头：[状态点][标题][关闭] ----
        // 状态点让"哪个标签还连着"一眼可见；关闭按钮补上 GUI 里本该有的 ×，
        // 右键菜单给关闭其它/重连，中键也能关。
        let dot = gtk::Label::new(None);
        dot.set_markup("<span foreground='#9aa0a6'>●</span>");
        dot.set_tooltip_text(Some(tr("连接中…")));

        let label = tab_title_label(&title);
        label.set_tooltip_text(Some(&title));
        // 远程设置标题（OSC 0/2）→ 更新标签头文字（tooltip 保留连接基础信息）
        {
            let label2 = label.clone();
            tp.set_title_callback(move |t| label2.set_text(t));
        }

        let close = crate::a11y::icon_button("window-close-symbolic");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        close.set_focusable(false);
        close.set_tooltip_text(Some(tr("关闭标签（Ctrl+Shift+W）")));

        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        hb.set_margin_start(4);
        hb.set_margin_end(2);
        hb.append(&dot);
        hb.append(&label);

        // ⚠️ **标签头上不再放图标按钮**（2026-10-27 用户要求）。
        //
        // 原先这里有「快速命令」和「数据库」两个图标：标签头只有几十像素，
        // 两个图标挤在标题后面，既把标题挤成省略号，又跟子标签的图标
        // 抢注意力（"终端"和"数据库"本来就是**子标签**）。
        // 现在这两个图标改挂在**子标签**上（终端页签 / 数据库页签），
        // 一眼就知道右侧那些页签是什么。
        //
        // 两个功能的入口都还在：
        // - 数据库 → 标签头**右键菜单**里的「数据库…」「新建数据库连接…」；
        // - 快速命令 → 主菜单「终端 → 快速命令…」/ Ctrl+Shift+K /
        //   终端区域右键菜单。

        hb.append(&close);

        self.wire_tab_menu(&hb, &page_w);
        {
            let t = TABS.with(|w| w.borrow().clone());
            let pg = page_w.clone();
            close.connect_clicked(move |_| {
                if let Some(t) = &t {
                    t.close_page(&pg);
                }
            });
        }
        // 状态点跟随面板状态（连接中/已连接/失败）
        {
            let dot2 = dot.clone();
            let st2 = sp.st.clone();
            let t = TABS.with(|w| w.borrow().clone());
            crate::panel::set_status_hook(&sp.st, move |level| {
                // ⚠️ **别直接用 level 画点**（阶段 65）：那个 level 只反映
                // **文件面板**；终端掉了、数据库断连都看不出来 ⇒ 改走聚合。
                update_dot(&dot2, &st2);
                // SSH 连上了 → 如果有等着开的数据库连接，现在开
                // （阶段 13：从连接管理器双击数据库连接走这条路）
                if level == crate::theme::State::Ok {
                    if let Some(t) = &t {
                        t.maybe_open_pending_db(&st2);
                    }
                }
            });
        }

        // ⚠️ **顺序不能反**（阶段 86 / 复查 N1）：GTK 要求页**先在 notebook
        // 里**才谈得上"可重排"。先前写的是 `set_tab_reorderable` 在前、
        // `append_page` 在后 ⇒ GTK 报
        // `gtk_notebook_set_tab_reorderable: assertion 'list != NULL' failed`
        // 并**静默丢弃**这个标记 ⇒ **所有页签都拖不动**，而且不报错。
        self.notebook.append_page(&page_w, Some(&hb));
        self.notebook.set_tab_reorderable(&page_w, true);

        // 记录最近连接（label 与条目名对齐，便于重开时回查密码）
        //
        // ⚠️ **命令行启动的连接默认不记**（堡垒机场景：launcher 每次拉起
        // 都是一条一次性连接，全记下来「最近」列表就没法看了）——
        // 界面上点开的连接照记。判定见 `args::ConnOptions::record_recent`。
        if opts.record_recent {
            recent::push(recent::RecentConn {
                label: title.clone(),
                host: opts.host.clone(),
                // 端口归一化：0 与 22 是同一台机器，别在"最近"里存成两条
                port: if opts.port > 0 { opts.port } else { 22 },
                user: opts.user.clone(),
                ref_name: opts.title.clone().filter(|_| opts.session_name.is_some()),
            });
        }

        // 第一个面板即"活动面板"；后续分屏以它为起点
        let active: Rc<RefCell<Option<TermPane>>> = Rc::new(RefCell::new(Some(tp.clone())));
        self.wire_pane(&tp, &page_w, &active);
        self.tabs.borrow_mut().push(Tab {
            page: page_w.clone(),
            panel_st: sp.st.clone(),
            term: tp.terminal().clone(),
            label,
            dot,
            // 阶段 97：`Tab.paned` 指的是**主终端子页内**那个 Paned
            //（左＝文件面板、右＝终端），不再是连接页的 Paned。
            paned: term_page,
            term_pane: tp.clone(),
            // 布局树按子页签存：主终端子页签一棵
            layouts: RefCell::new(vec![(term_sub_id, PaneLayout::Leaf(tp.clone()))]),
            term_sub_id: Cell::new(term_sub_id),
            term_pages: RefCell::new(Vec::new()),
            active,
        });
        // 终端右键菜单（分屏入口 + 广播开关，3.1/3.2）
        {
            let page = page_w.clone();
            term::attach_split_menu(&tp, &page);
        }

        self.notebook.next_page();
        tp.terminal().grab_focus();
        // 广播开着时新开的终端也要能收/发按键
        self.sync_broadcast();
        self.notify_pages();
    }

    /// 标签头右键菜单（关闭 / 关闭其它 / 重新连接）+ 中键关闭。
    fn wire_tab_menu(&self, anchor: &gtk::Box, page: &gtk::Widget) {
        let pop = crate::menu::popover(anchor);
        let vb = crate::menu::body();
        let mut mk = |icon: &str, label: &str| -> gtk::Button {
            let b = crate::menu::item(icon, label, None);
            let pop2 = pop.clone();
            b.connect_clicked(move |_| pop2.popdown());
            vb.append(&b);
            b
        };

        {
            let b = mk("view-refresh-symbolic", tr("重新连接 SFTP"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                    t.reconnect_page(&pg);
                }
            });
        }
        {
            // 「重新连接终端」：与上面的「重新连接 SFTP」是**两条链路**。
            //
            // SFTP 走进程内的通道，终端是另起的 ssh 子进程——用户敲了 exit
            // 之后终端已经死了，点「重新连接 SFTP」一点反应都没有，
            // 于是"重建会话"这件事根本没地方做。
            let b = mk("view-refresh-symbolic", tr("重新连接终端"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                    t.reconnect_term_page(&pg);
                }
            });
        }
        {
            let b = mk("network-transmit-receive-symbolic", tr("端口转发…"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(st) = TABS
                    .with(|w| w.borrow().clone())
                    .and_then(|t| t.panel_state_for(&pg))
                {
                    panel::on_open_forwards(&st);
                }
            });
        }
        {
            let b = mk("drive-harddisk-symbolic", tr("数据库…"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(st) = TABS
                    .with(|w| w.borrow().clone())
                    .and_then(|t| t.panel_state_for(&pg))
                {
                    panel::on_open_db(&st);
                }
            });
        }
        {
            // 「新建数据库连接…」：给**当前会话**建一条数据库连接。
            //
            // 阶段 24：会话不一定在列表里（堡垒机场景），列表里没有它就
            // 没地方建连接 —— 这个入口跟着当前标签走。
            let b = mk("list-add-symbolic", tr("新建数据库连接…"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                let Some(st) = TABS
                    .with(|w| w.borrow().clone())
                    .and_then(|t| t.panel_state_for(&pg))
                else {
                    return;
                };
                let key = crate::backend::dbstore::session_key(&st.opts);
                let parent = st.topwin();
                let st2 = Rc::clone(&st);
                crate::panel::dbconn::ui_new(
                    parent.as_ref(),
                    &key,
                    Some(Rc::clone(&st)),
                    move |k, name| {
                        // 建好之后：打开工作台并连上（用户刚建的就是要用的那条）
                        panel::on_open_db_page(&st2);
                        crate::panel::dbui::use_profile(&st2, &k, &name);
                    },
                );
            });
        }
        {
            let b = mk("preferences-system-symbolic", tr("会话选项…"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(st) = TABS
                    .with(|w| w.borrow().clone())
                    .and_then(|t| t.panel_state_for(&pg))
                {
                    panel::on_open_session_opts(&st);
                }
            });
        }

        vb.append(&crate::menu::sep());
        {
            let b = mk("window-close-symbolic", tr("关闭其它标签"));
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                    t.close_others(&pg);
                }
            });
        }
        {
            let b = mk("window-close-symbolic", tr("关闭标签"));
            crate::menu::danger(&b);
            let pg = page.clone();
            b.connect_clicked(move |_| {
                if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                    t.close_page(&pg);
                }
            });
        }
        pop.set_child(Some(&vb));

        let g = gtk::GestureClick::new();
        g.set_button(3);
        let pop2 = pop.clone();
        g.connect_pressed(move |_, _, x, y| {
            pop2.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop2.popup();
        });
        anchor.add_controller(g);

        // 中键关闭：浏览器/终端用户的肌肉记忆
        let g2 = gtk::GestureClick::new();
        g2.set_button(2);
        let t2 = TABS.with(|w| w.borrow().clone());
        let pg2 = page.clone();
        g2.connect_pressed(move |_, _, _, _| {
            if let Some(t) = &t2 {
                t.close_page(&pg2);
            }
        });
        anchor.add_controller(g2);
    }

    /// 关闭除 `keep` 之外的所有业务标签。
    ///
    /// ⚠️ 一次问完（阶段 63）：别一个标签弹一次框。
    pub fn close_others(&self, keep: &gtk::Widget) {
        let pages: Vec<gtk::Widget> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| t.page.clone())
            .filter(|p| p != keep)
            .collect();
        if pages.is_empty() {
            return;
        }
        let panels: Vec<Rc<panel::PanelState>> =
            pages.iter().filter_map(|p| self.panel_of(p)).collect();
        let me = self.me.clone();
        panel::closeguard::request_close_many(&panels, move || {
            let Some(t) = me.upgrade() else { return };
            for p in &pages {
                t.close_page_forced(p);
            }
        });
    }

    /// 重连某个标签的 SFTP 面板（标签右键菜单用）。
    pub fn reconnect_page(&self, page: &gtk::Widget) {
        let st = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.iter().find(|t| t.page == *page) else {
                return;
            };
            t.panel_st.clone()
        };
        crate::panel::on_reconnect(&st);
    }

    /// 在当前连接里**新开一个终端子页签**（一条独立的 SSH 会话）。
    ///
    /// 子标签栏尾部的「+」用它。平级的终端页签跟分屏的区别：分屏把两块
    /// 终端挤在同一页里，这个是**另一个页签**——想同时看两个会话时用。
    pub fn new_term_page(&self, page: &gtk::Widget) {
        self.new_term_page_cwd(page, None);
    }

    /// 新开终端子页签；`dir` 给定时让新终端**进到那个目录**。
    ///
    /// 「在此处打开终端」用（见 `Tabs::open_term_at`）。
    pub fn new_term_page_cwd(&self, page: &gtk::Widget, dir: Option<&str>) {
        let (sub, opts, active, panel_st) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.iter().find(|t| t.page == *page) else {
                return;
            };
            let Some(sub) = t.panel_st.sub_tabs.borrow().clone() else {
                return;
            };
            (
                sub,
                t.panel_st.opts.clone(),
                Rc::clone(&t.active),
                Rc::clone(&t.panel_st),
            )
        };
        let tp = TermPane::new(&opts);
        // 阶段 65：额外终端页签也算进"这个会话有几个终端活着"
        install_alive_hook(&tp, &panel_st);
        // 进到指定目录：等**远端真的就绪了**（ready 钩子）再往 pty 写 `cd`。
        //
        // ⚠️ **不能早写**：pty 带行规程（本地回显 + 规范模式），ssh 还没
        //    切到 raw 模式时写进去的字节会被**本地回显**（屏幕上凭空多一
        //    行命令）并**缓冲**在本地，等 ssh 切 raw 后才转发给远端 ——
        //    那会儿远端可能还停在**登录流程**（堡垒机常见）里，这行就被
        //    它吃掉，`cd` 根本不执行。钩子自己会等（见 `term.rs` 的
        //    `feed_when_remote_ready`）。
        // ⚠️ 闭包**不捕获 TermPane**（自引用 = 引用环，面板释放不掉），
        //    钩子把终端传进来，往里写字节就行。
        if let Some(d) = dir {
            if !d.is_empty() {
                let cmd = cwd_cmd(d);
                tp.set_ready_hook(move |term, what| {
                    let bytes: &[u8] = match what {
                        // 初始命令：落进 shell 里
                        crate::term::ReadyCmd::Cmd => cmd.as_bytes(),
                        // 探路：只发一个空行（喂给登录脚本的「按回车」）
                        crate::term::ReadyCmd::Probe => b"\n",
                    };
                    term.feed_child(bytes);
                });
            }
        }
        // 阶段 97：额外终端页签也是「文件面板｜终端」的 Paned，
        // **左格先空着**（文件面板只有一个父级，用户切过来时才搬进来）。
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        let term_wrap = term_container(&tp);
        term_page.set_end_child(Some(term_wrap.upcast_ref::<gtk::Widget>()));
        term_page.set_hexpand(true);
        term_page.set_vexpand(true);
        // 宽度跟着记忆走（切过去时文件面板会按它复位）
        track_file_width(&panel_st, &term_page);
        let id = sub.add_page_icon_kind(
            Some("utilities-terminal-symbolic"),
            tr("终端"),
            panel::subtabs::PageKind::Terminal,
            term_page.upcast(),
            None,
        );
        sub.switch_to(id);
        self.wire_pane(&tp, page, &active);
        term::attach_split_menu(&tp, page);
        self.attach_term_drop(page, &tp);
        {
            let mut tabs = self.tabs.borrow_mut();
            if let Some(t) = tabs.iter_mut().find(|t| t.page == *page) {
                t.term_pages.borrow_mut().push((id, tp.clone()));
                // ⚠️ 阶段 37：额外终端子页签**自己一棵布局树**——
                // 这样在它上面点「左右分屏」也有地方放新面板（之前它不在
                // 任何树里，守卫直接拦下，看着就是"点了没反应"）。
                t.layouts
                    .borrow_mut()
                    .push((id, PaneLayout::Leaf(tp.clone())));
            }
        }
        self.sync_broadcast();
        tp.terminal().grab_focus();
    }

    /// 在**某个标签**里新开一个终端页签，并让它 `cd` 到指定目录。
    ///
    /// 「在此处打开终端」用：右键远程文件列表的某个目录 → 在**当前标签**里
    /// 开一个新终端页签（**新 SSH 会话**，跟子标签栏「+」→新建终端一样），
    /// 并让它进到那个目录。
    ///
    /// **为什么新开页签而不是复用当前终端**：当前终端可能正跑着命令，
    /// 往里塞 `cd` 会打断用户手上的事。
    pub fn open_term_at(&self, st: &Rc<panel::PanelState>, dir: &str) {
        let page = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.iter().find(|t| Rc::ptr_eq(&t.panel_st, st)) else {
                return;
            };
            t.page.clone()
        };
        self.new_term_page_cwd(&page, Some(dir));
    }

    /// 子页签被关掉时的清理：终端子页要把会话收掉（否则留下野 ssh 进程）。
    fn on_term_page_closed(&self, page: &gtk::Widget, id: u64) {
        let mut tabs = self.tabs.borrow_mut();
        let Some(t) = tabs.iter_mut().find(|t| t.page == *page) else {
            return;
        };
        // 阶段 97：**文件面板不会被这一页带走**——
        //
        // 文件面板是**会话级的一个 widget**（`PanelState` 一直持有它），
        // 所以这一页没了它也不会被销毁；等用户点「重新连接终端」重建出
        // 新终端页时，`rebuild_term` 会把它搬回去。
        //
        // ⚠️ **这里不要主动 `unparent()`**：关掉的可能是**别的**终端页签，
        // 而文件面板此刻正托管在另一个页签里 ⇒ 主动解除父级会把那边的
        // 文件面板也弄掉。搬家统一由 `host_file_panel` 负责。
        //
        // ⚠️ 阶段 37：这个子页签的**布局树**要一起摘掉——树里除了它自己
        // 那个面板，还有它分屏出来的那些（分屏的副面板不在 `term_pages` 里）。
        // 漏掉的话，关掉页签后那些面板还在跑 ssh 子进程（野进程）。
        let mut to_stop = Vec::new();
        {
            let mut layouts = t.layouts.borrow_mut();
            if let Some(pos) = layouts.iter().position(|(lid, _)| *lid == id) {
                let (_, l) = layouts.remove(pos);
                l.leaves(&mut to_stop);
            }
        }
        {
            let mut pages = t.term_pages.borrow_mut();
            if let Some(pos) = pages.iter().position(|(pid, _)| *pid == id) {
                let (_pid, p) = pages.remove(pos);
                // 停日志/录制并摘掉定时器（不然定时器把面板钉住）
                if !to_stop.iter().any(|q| same_pane(q, &p)) {
                    to_stop.push(p);
                }
            }
        }
        for p in to_stop {
            p.shutdown();
        }
    }

    /// 重连某个标签里的**终端**（标签右键菜单用）。
    ///
    /// 与 `reconnect_page`（SFTP）是两条链路：终端是另起的 ssh 子进程，
    /// 会话结束（用户敲 exit / 网络断）之后必须重开子进程才能用。
    ///
    /// **三种情况都要管**（阶段 25）：
    /// 1. 终端子页签还在 → 重连它，并切到终端页签（用户可能在看数据库页签）；
    /// 2. **终端子页签被用户关掉了** → 重建一个终端页签出来。
    ///    不重建的话，重连的是那个已经看不见的会话，用户看着就是"点了没反应"；
    /// 3. 分屏过 → 每个 pane 都重连，否则只救回来一半。
    pub fn reconnect_term_page(&self, page: &gtk::Widget) {
        let Some(i) = self.index_of(page) else { return };
        let (sub, sub_id) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(i) else { return };
            (
                t.panel_st.sub_tabs.borrow().clone(),
                t.term_sub_id.get(),
            )
        };
        let Some(sub) = sub else { return };
        if !sub.has(sub_id) {
            // 终端页签被关掉了 → 重建（新的 TermPane 自己会 spawn，
            // 不用再重连一次）
            self.rebuild_term(i);
            return;
        }
        // 切到终端页签：用户点的是"重新连接终端"，得看得见结果
        sub.switch_to(sub_id);
        let panes: Vec<TermPane> = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(i) else { return };
            // 所有终端子页签的面板（含各自分屏出来的）都要重连
            t.all_leaves()
        };
        for p in panes {
            p.reconnect_term();
        }
        // **顺带把断了的文件面板也救回来**（2026-11-05，用户原话：
        // 「重连终端后，sftp仍然没有重连」）。
        //
        // 终端和 SFTP 是**两条独立的连接**——终端那条断了，SFTP 那条可能还活着
        // （那就别动它：重建连接会把正在跑的传输打断），也可能一起断了
        // （服务端把会话关了的时候就是这样）⇒ 只在**它确实断了**的时候重连。
        let st = {
            let tabs = self.tabs.borrow();
            tabs.get(i).map(|t| t.panel_st.clone())
        };
        if let Some(st) = st {
            if !st.conn_ok.get() {
                crate::panel::on_reconnect(&st);
            }
        }
    }

    /// 重建某个标签的**终端子页签**（阶段 25）。
    ///
    /// 用户把「终端」子页签关掉之后，终端对象还在但看不见。要"重新连接
    /// 终端"，就得**重新建一个页签**出来——新页签自带一条新的 SSH 会话
    /// （`TermPane::new` 自己 spawn），所以建好就是活的。
    fn rebuild_term(&self, i: usize) {
        let (sub, st, opts, page, active) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(i) else { return };
            let Some(sub) = t.panel_st.sub_tabs.borrow().clone() else {
                return;
            };
            (
                sub,
                t.panel_st.clone(),
                t.panel_st.opts.clone(),
                t.page.clone(),
                Rc::clone(&t.active),
            )
        };

        let tp = TermPane::new(&opts);
        {
            let st2 = st.clone();
            tp.set_cwd_callback(move |uri| panel::notify_cwd_uri(&st2, uri));
        }
        // 阶段 65：重建的终端也算一个
        install_alive_hook(&tp, &st);
        panel::set_term_spawn(&st, tp.spawn_time());
        panel::set_term(&st, &tp.terminal());
        // 拖文件到终端 = 上传到当前远程目录
        panel::enable_drop_upload(&st, tp.terminal());

        // 阶段 97：重建的主终端页签也是「文件面板｜终端」的 Paned，
        // **把文件面板搬回来**（用户 × 掉终端页签时它只是"寄存"着，
        // 没被销毁——`PanelState` 一直持有它）。
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        let term_wrap = term_container(&tp);
        term_page.set_end_child(Some(term_wrap.upcast_ref::<gtk::Widget>()));
        term_page.set_hexpand(true);
        term_page.set_vexpand(true);
        track_file_width(&st, &term_page);
        host_file_panel(&st, &term_page);
        let id = sub.add_page_icon_kind(
            Some("utilities-terminal-symbolic"),
            tr("终端"),
            panel::subtabs::PageKind::Terminal,
            term_page.clone().upcast(),
            None,
        );
        sub.switch_to(id);
        self.wire_pane(&tp, &page, &active);
        term::attach_split_menu(&tp, &page);

        {
            let mut tabs = self.tabs.borrow_mut();
            let Some(t) = tabs.get_mut(i) else { return };
            let old_id = t.term_sub_id.get();
            t.term_sub_id.set(id);
            t.term = tp.terminal().clone();
            t.term_pane = tp.clone();
            // `Tab.paned` 指的是主终端页签里那个 Paned ⇒ 跟着换
            t.paned = term_page;
            // 原来的 pane 已经跟着子页签一起被丢掉了，布局重置成单块
            //
            // ⚠️ 阶段 37：布局树**按子页签存**，重建后是新的一棵（新 id）；
            // 旧的那棵（连同它上面的分屏）随旧页签一起没了，要从表里摘掉，
            // 否则表里会留下一棵指向已销毁面板的死树。
            {
                let mut layouts = t.layouts.borrow_mut();
                layouts.retain(|(sid, _)| *sid != old_id);
                layouts.push((id, PaneLayout::Leaf(tp.clone())));
            }
            *t.active.borrow_mut() = Some(tp.clone());
        }
        self.sync_broadcast();
        tp.terminal().grab_focus();
    }

    /// 用给定的连接参数**重新连接终端**（会话树右键菜单用）。
    ///
    /// **为什么要在会话这一层做**：终端页签可以被关掉、整个连接标签也可以
    /// 被关掉——关掉之后标签头上就没有「重新连接终端」可点了。会话树是
    /// 按会话组织的，它还在，所以"重建终端"的入口要挂在这儿：
    /// - 这个会话的标签还在 → 切过去，重连/重建它的终端；
    /// - 标签已经关掉了 → 用同一份连接参数**开一个新标签**（新标签自带终端）。
    pub fn reconnect_term_with_opts(self: &Rc<Tabs>, opts: &ConnOptions) {
        // 已有这个会话的标签 → 切过去，重连它的终端
        let key = crate::backend::dbstore::session_key(opts);
        let page = {
            let tabs = self.tabs.borrow();
            tabs.iter()
                .find(|t| crate::backend::dbstore::session_key(&t.panel_st.opts) == key)
                .map(|t| t.page.clone())
        };
        if let Some(page) = page {
            if let Some(n) = self.notebook.page_num(&page) {
                self.notebook.set_current_page(Some(n));
            }
            self.reconnect_term_page(&page);
            return;
        }
        // 标签没了 → 用同一份连接参数重建一个（连 SFTP 面板一起）
        self.open(opts);
    }

    /// 关闭指定页（欢迎页返回 false）。
    ///
    /// ⚠️ **关之前先问**（阶段 63 / 审计 P0-1）：这个标签里可能有
    /// 没保存的文件、没提交的数据库改动、正在跑的传输。统一走
    /// `panel::closeguard`，别再各关各的。
    pub fn close_page(&self, page: &gtk::Widget) -> bool {
        let Some(st) = self.panel_of(page) else {
            return false;
        };
        let me = self.me.clone();
        let pg = page.clone();
        panel::closeguard::request_close(&st, move || {
            // 保存可能还没完（延后到回包齐了才跑这儿）⇒ 用弱引用找回来
            let Some(t) = me.upgrade() else { return };
            t.close_page_forced(&pg);
        })
    }

    /// 找某个页对应的面板状态。
    fn panel_of(&self, page: &gtk::Widget) -> Option<Rc<panel::PanelState>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.page == *page)
            .map(|t| Rc::clone(&t.panel_st))
    }

    /// **不询问**直接关掉指定页。
    ///
    /// 用于"已经问过了"的路径：用户选了「保存并关闭」，保存完成后再关——
    /// 这时不能再问一遍。
    pub fn close_page_forced(&self, page: &gtk::Widget) -> bool {
        let mut tabs = self.tabs.borrow_mut();
        let Some(idx) = tabs.iter().position(|t| t.page == *page) else {
            return false;
        };
        let tab = tabs.remove(idx);
        // 记住用户拖拽的面板宽度
        //
        // ⚠️ 阶段 97：取 `st.file_w`（跨宿主同步的那个值），**不是**
        // `tab.paned.position()`——文件面板会跟着当前终端页签搬家，
        // `tab.paned` 未必是它现在的宿主（position 可能是 0）。
        crate::settings::set_sftp_pane_pos(tab.panel_st.file_w.get());
        // 记住两侧文件列表的列宽（隐藏的列不记）
        for remote in [true, false] {
            let pane = tab.panel_st.pane(remote);
            let grab = |c: &gtk::TreeViewColumn| {
                c.is_visible().then(|| c.width()).filter(|w| *w >= 24)
            };
            crate::settings::set_col_widths(
                remote,
                crate::settings::ColWidths {
                    size: grab(&pane.col_size),
                    perm: pane.col_perm.as_ref().and_then(|c| grab(c)),
                    mtime: grab(&pane.col_mtime),
                },
            );
        }
        panel::shutdown(&tab.panel_st); // 取消传输、销毁队列/转发/数据库窗、停 worker
        // 面板退出前摘日志定时器：它闭包抓着 TermPane，不摘会把终端钉住不放
        let leaves = tab.all_leaves();
        for p in leaves {
            p.shutdown();
        }
        drop(tabs);
        let num = self.notebook.page_num(page).unwrap_or(0);
        self.notebook.remove_page(Some(num));
        // 全部关闭：恢复欢迎页（空状态）
        if self.tabs.borrow().is_empty() {
            let label = gtk::Label::new(Some(tr("欢迎")));
            self.notebook
                .insert_page(&welcome_page(), Some(&label), Some(0));
            self.welcome_attached.set(true);
        }
        // 终端 ssh 子进程随 PTY 关闭退出（与 C 版一致）
        self.sync_broadcast();
        self.notify_pages();
        true
    }

    /// 关闭当前业务标签（欢迎页/无标签时不动）。
    pub fn close_current(&self) -> bool {
        let Some(cur) = self.notebook.current_page() else { return false };
        let Some(page) = self.notebook.nth_page(Some(cur)) else { return false };
        self.close_page(&page)
    }

    /// 切分当前标签的**活动面板**（stacked=true 上下，false 左右）。
    pub fn split_current(&self, stacked: bool) {
        let Some(i) = self.current_index() else { return };
        self.split_index(i, stacked);
    }

    /// 分屏 = 给活动面板再开一个**独立终端**（各自一条 SSH 连接）。
    /// 每个面板都能再分（2×2、三栏都行）；不做输入/显示同步，
    /// 要同步按键用广播（3.2）。
    fn split_index(&self, i: usize, stacked: bool) {
        let (opts, target, active, page, sub_id) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(i) else { return };
            let target = t
                .active
                .borrow()
                .clone()
                .unwrap_or_else(|| t.term_pane.clone());
            // ⚠️ 阶段 37：**找它属于哪个终端子页签**，后面就在那棵布局树上
            // 分屏。之前这里只有 Tab 级的一棵树，额外终端子页签的面板不在
            // 任何树里 → 守卫直接拦下 → 点了「左右分屏」看着毫无反应。
            let Some(sub_id) = t.sub_id_of_pane(&target) else {
                return;
            };
            (
                t.panel_st.opts.clone(),
                target,
                Rc::clone(&t.active),
                t.page.clone(),
                sub_id,
            )
        };

        let second = TermPane::new(&opts);
        self.wire_pane(&second, &page, &active);
        term::attach_split_menu(&second, &page);
        self.attach_term_drop(&page, &second);

        let paned = gtk::Paned::new(if stacked {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        paned.set_hexpand(true);
        paned.set_vexpand(true);
        paned.add_css_class("pp-split");

        // 新容器塞进目标面板原来的位置（父容器可能是页面 Paned，
        // 也可能是上一层的分屏 Paned）
        let pw: gtk::Widget = target.widget().clone();
        let Some(parent) = pw.parent().and_downcast::<gtk::Paned>() else {
            return;
        };
        if parent.start_child().as_ref() == Some(&pw) {
            parent.set_start_child(Some(&paned));
        } else if parent.end_child().as_ref() == Some(&pw) {
            parent.set_end_child(Some(&paned));
        } else {
            return;
        }
        paned.set_start_child(Some(target.widget()));
        paned.set_end_child(Some(second.widget()));
        paned.show();

        {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.get(i) else { return };
            // 记到**它自己那棵**布局树上（阶段 37）
            {
                let mut layouts = t.layouts.borrow_mut();
                if let Some((_, l)) = layouts.iter_mut().find(|(id, _)| *id == sub_id) {
                    split_in_tree(l, &target, &paned, second.clone(), stacked);
                }
            }
            *t.active.borrow_mut() = Some(second.clone());
        }
        // 分隔线摆中间（等尺寸算出来再摆）
        let p2 = paned.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(30), move || {
            let len = if stacked { p2.height() } else { p2.width() };
            if len > 0 {
                p2.set_position(len / 2);
            }
        });
        self.sync_broadcast();
        self.refresh_pane_highlight(&page);
        second.terminal().grab_focus();
    }

    /// 关闭当前活动面板（只剩一个面板时不动）。
    pub fn close_pane_current(&self) -> bool {
        let Some(i) = self.current_index() else {
            return false;
        };
        self.close_pane_index(i, None)
    }

    /// 关闭指定面板（右键哪块关哪块）。
    pub fn close_pane(&self, page: &gtk::Widget, pane: &TermPane) -> bool {
        let Some(i) = self.index_of(page) else {
            return false;
        };
        self.close_pane_index(i, Some(pane.clone()))
    }

    /// 「关闭当前终端」——右键哪块关哪块。
    ///
    /// **三种目标**（阶段 37）：
    /// 1. **分屏中**（那一页有 2 块以上）→ 关掉**这一半**，兄弟顶上来；
    /// 2. **只剩一块** → 这个终端就是整个子页签 → **关掉那个子页签**
    ///    （跟子页签上的 × 一个意思）；
    /// 3. 目标是**额外终端子页签**里的面板 → 关掉那个额外子页签。
    ///
    /// ⚠️ 阶段 37 之前 `count() <= 1` 直接 `return false`：用户点「关闭当前
    /// 终端」看着就是"没反应"（顶栏那个「关闭当前终端」也是同一条路）。
    fn close_pane_index(&self, i: usize, target: Option<TermPane>) -> bool {
        let mut tabs = self.tabs.borrow_mut();
        let Some(t) = tabs.get_mut(i) else { return false };
        let target = target
            .or_else(|| t.active.borrow().clone())
            .unwrap_or_else(|| t.term_pane.clone());

        // 目标属于哪个终端子页签？
        //   在布局树里 → 它是那个子页签的面板（含它分屏出来的副面板）
        //   不在       → 额外终端子页签（`term_pages` 里那条）
        let (sub_id, in_tree) = match t.sub_id_of_pane(&target) {
            Some(id) => (id, true),
            None => match t
                .term_pages
                .borrow()
                .iter()
                .find(|(_, p)| same_pane(p, &target))
            {
                Some((id, _)) => (*id, false),
                None => return false,
            },
        };

        // ② 只剩一块 / ③ 额外终端子页签 → **关掉那个终端子页签**
        if !(in_tree && t.leaves_of(sub_id).len() > 1) {
            let sub = t.panel_st.sub_tabs.borrow().clone();
            // ⚠️ 必须先放掉 tabs 的借用：`close_page` 会触发子页签的关闭钩子，
            // 钩子回头要 `self.tabs.borrow_mut()`——还持着就是双重借用 panic。
            drop(tabs);
            let Some(sub) = sub else {
                return false;
            };
            // 先停会话再关页签：页签一关，控件就没了
            target.shutdown();
            return sub.close_page(sub_id);
        }

        // ① 分屏中 → 拆掉这一半，兄弟顶上来
        let taken = {
            let mut layouts = t.layouts.borrow_mut();
            let l = match layouts.iter_mut().find(|(id, _)| *id == sub_id) {
                Some((_, l)) => l,
                None => return false,
            };
            take_sibling(l, &target)
        };
        let Some((paned, sibling, target_is_a, first)) = taken else {
            return false;
        };

        // 兄弟先摘出来，再把旧容器从父容器里换成兄弟
        if target_is_a {
            paned.set_end_child(None::<&gtk::Widget>);
        } else {
            paned.set_start_child(None::<&gtk::Widget>);
        }
        let pw: gtk::Widget = paned.clone().upcast();
        if let Some(parent) = pw.parent().and_downcast::<gtk::Paned>() {
            if parent.start_child().as_ref() == Some(&pw) {
                parent.set_start_child(Some(&sibling));
            } else {
                parent.set_end_child(Some(&sibling));
            }
        } else {
            t.paned.set_end_child(Some(&sibling));
        }
        // 关掉的那半边的控件
        if target_is_a {
            paned.set_start_child(None::<&gtk::Widget>);
        } else {
            paned.set_end_child(None::<&gtk::Widget>);
        }
        target.shutdown();

        // 关掉的是主终端时，顺延到剩下的第一个面板
        let first = first.or_else(|| t.leaves_of(sub_id).first().cloned());
        if let Some(first) = first {
            if same_pane(&t.term_pane, &target) {
                t.term_pane = first.clone();
                t.term = first.terminal().clone();
                let lbl = t.label.clone();
                first.set_title_callback(move |s| lbl.set_text(s));
                crate::panel::set_term(&t.panel_st, &first.terminal());
                crate::panel::set_term_spawn(&t.panel_st, first.spawn_time());
            }
            *t.active.borrow_mut() = Some(first);
        }
        let page = t.page.clone();
        drop(tabs);
        self.sync_broadcast();
        self.refresh_pane_highlight(&page);
        true
    }

    /// 当前标签是不是分屏状态。
    ///
    /// 阶段 37：布局树按子页签分组，所以要看**活动面板所在那一页**是不是
    /// 分着的——另一个终端子页签分着屏，不该影响这里。
    pub fn current_is_split(&self) -> bool {
        let Some(i) = self.current_index() else {
            return false;
        };
        let tabs = self.tabs.borrow();
        let Some(t) = tabs.get(i) else {
            return false;
        };
        let active = t.active.borrow().clone();
        let Some(active) = active else {
            return false;
        };
        match t.sub_id_of_pane(&active) {
            Some(id) => t.leaves_of(id).len() > 1,
            None => false,
        }
    }

    /// 其它已连接 SFTP 的标签（跨服务器传输的目标候选）。
    pub fn remote_targets(
        &self,
        exclude: &Rc<panel::PanelState>,
    ) -> Vec<crate::panel::relayui::RemoteTarget> {
        self.tabs
            .borrow()
            .iter()
            .filter(|t| t.panel_st.conn_ok.get() && !Rc::ptr_eq(&t.panel_st, exclude))
            .map(|t| crate::panel::relayui::RemoteTarget {
                label: format!("{} — {}@{}:{}",
                    t.label.text(),
                    t.panel_st.opts.user,
                    t.panel_st.opts.host,
                    t.panel_st.opts.port
                ),
                st: Rc::clone(&t.panel_st),
                cwd: t.panel_st.remote_cwd.borrow().clone(),
            })
            .collect()
    }

    pub fn current_index(&self) -> Option<usize> {
        let cur = self.notebook.current_page()? as usize;
        // 有欢迎页时页码比 tabs 索引大 1
        let off = if self.welcome_attached.get() { 1 } else { 0 };
        let idx = cur.checked_sub(off)?;
        if idx < self.tabs.borrow().len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.borrow().is_empty()
    }

    /// 当前业务标签的标题（欢迎页/无标签 → None）。
    pub fn current_title(&self) -> Option<String> {
        let cur = self.notebook.current_page()?;
        let page = self.notebook.nth_page(Some(cur))?;
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.page == page)
            .map(|t| t.label.text().to_string())
    }

    /// 当前业务标签的页控件（欢迎页 None）。菜单里的分屏/日志要按页操作。
    pub fn current_page_widget(&self) -> Option<gtk::Widget> {
        let cur = self.notebook.current_page()?;
        self.notebook.nth_page(Some(cur))
    }

    /// 当前业务标签的面板状态（欢迎页 None）。
    /// 菜单栏里的会话级动作（端口转发 / 会话选项）按它解析作用对象。
    pub fn current_panel_state(&self) -> Option<Rc<panel::PanelState>> {
        let page = self.current_page_widget()?;
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.page == page)
            .map(|t| Rc::clone(&t.panel_st))
    }

    /// 这个窗口里所有已打开会话的 `(会话键, 面板状态)`。
    ///
    /// 两棵树用它判断"这条连接是不是正连着"（阶段 57）。
    pub fn sessions(&self) -> Vec<(String, Rc<panel::PanelState>)> {
        self.tabs
            .borrow()
            .iter()
            .map(|t| {
                (
                    crate::backend::dbstore::session_key(&t.panel_st.opts),
                    Rc::clone(&t.panel_st),
                )
            })
            .collect()
    }

    /// 指定标签页的面板状态（标签右键菜单用：对着哪个标签操作就是哪一份）。
    pub fn panel_state_for(&self, page: &gtk::Widget) -> Option<Rc<panel::PanelState>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.page == *page)
            .map(|t| Rc::clone(&t.panel_st))
    }

    /// 按**会话键**找面板状态（`dbstore::session_key` 那套键）。
    ///
    /// **为什么需要**：用户在连接管理器 / 会话树里建了一条数据库连接，
    /// 可真正连库的是**某个标签页**里的工作台——两边得按会话键对上，
    /// 才能把"当场输入的密码"放进**那个会话**的口袋里。
    pub fn panel_state_by_session_key(&self, key: &str) -> Option<Rc<panel::PanelState>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| crate::backend::dbstore::session_key(&t.panel_st.opts) == key)
            .map(|t| Rc::clone(&t.panel_st))
    }

    /// 按会话键找面板状态（**不切标签页**）。
    ///
    /// 「启用/停用转发」这种操作不该把用户拽到别的标签页去。
    pub fn find_session(&self, key: &str) -> Option<Rc<panel::PanelState>> {
        let tabs = self.tabs.borrow();
        tabs.iter()
            .find(|t| crate::backend::dbstore::session_key(&t.panel_st.opts) == key)
            .map(|t| Rc::clone(&t.panel_st))
    }

    /// 找到已打开的**同一会话**的标签并切过去，返回它的面板状态。
    ///
    /// 侧边栏的会话树上有"数据库连接"子节点（阶段 10），双击它要打开
    /// 数据库窗口——而数据库通道是复用这条 SSH 连接的，所以得先找到这条
    /// 会话对应的标签（已经开着就切过去，别重复开一个）。
    pub fn activate_session(&self, key: &str) -> Option<Rc<panel::PanelState>> {
        let found = {
            let tabs = self.tabs.borrow();
            tabs.iter()
                .find(|t| crate::backend::dbstore::session_key(&t.panel_st.opts) == key)
                .map(|t| (t.page.clone(), Rc::clone(&t.panel_st)))
        };
        let (page, st) = found?;
        if let Some(n) = self.notebook.page_num(&page) {
            self.notebook.set_current_page(Some(n));
        }
        Some(st)
    }

    /// 当前标签的终端（欢迎页无终端）。
    pub fn current_terminal(&self) -> Option<vte4::Terminal> {
        let cur = self.notebook.current_page()?;
        let page = self.notebook.nth_page(Some(cur))?;
        let tabs = self.tabs.borrow();
        let t = tabs.iter().find(|t| t.page == page)?;
        // 搜索/复制/粘贴作用在**活动面板**上，而不是固定第一个
        match t.active.borrow().clone() {
            Some(p) => Some(p.terminal().clone()),
            None => Some(t.term.clone()),
        }
    }

    /// 给面板挂焦点回调：聚焦时更新活动面板并刷新高亮。
    /// 给一个终端挂上「**拖文件进来 = 上传到当前远程目录**」。
    ///
    /// 挂之前先从 `tabs` 里找出这个标签的面板状态——终端本身不知道
    /// 自己属于哪个标签。
    fn attach_term_drop(&self, page: &gtk::Widget, pane: &TermPane) {
        let st = {
            let tabs = self.tabs.borrow();
            tabs.iter()
                .find(|t| t.page == *page)
                .map(|t| t.panel_st.clone())
        };
        if let Some(st) = st {
            panel::enable_drop_upload(&st, pane.terminal());
        }
    }

    fn wire_pane(
        &self,
        pane: &TermPane,
        page: &gtk::Widget,
        active: &Rc<RefCell<Option<TermPane>>>,
    ) {
        let p = pane.clone();
        let pg = page.clone();
        let act = Rc::clone(active);
        pane.set_focus_callback(move || {
            *act.borrow_mut() = Some(p.clone());
            if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                t.mark_active(&pg, &p);
            }
        });
    }

    fn mark_active(&self, page: &gtk::Widget, pane: &TermPane) {
        let tabs = self.tabs.borrow();
        let Some(t) = tabs.iter().find(|t| t.page == *page) else {
            return;
        };
        *t.active.borrow_mut() = Some(pane.clone());
        // 高亮只看**活动面板所在那一页**（阶段 37：布局树按子页签分组，
        // 另一个终端页签分着屏，不该给这边也描边）。
        let leaves = match t.sub_id_of_pane(pane) {
            Some(id) => t.leaves_of(id),
            None => vec![pane.clone()],
        };
        let multi = leaves.len() > 1;
        for p in leaves {
            if multi && same_pane(&p, pane) {
                p.widget().add_css_class("pp-pane-active");
            } else {
                p.widget().remove_css_class("pp-pane-active");
            }
        }
    }

    /// 分屏数量变化后重刷高亮（只有一块面板时不描边）。
    fn refresh_pane_highlight(&self, page: &gtk::Widget) {
        let (active, leaves) = {
            let tabs = self.tabs.borrow();
            let Some(t) = tabs.iter().find(|t| t.page == *page) else {
                return;
            };
            let active = t.active.borrow().clone();
            // 同样只看活动面板所在那一页
            let leaves = match active.as_ref().and_then(|a| t.sub_id_of_pane(a)) {
                Some(id) => t.leaves_of(id),
                None => Vec::new(),
            };
            (active, leaves)
        };
        let multi = leaves.len() > 1;
        for p in leaves {
            let on = multi && active.as_ref().map(|a| same_pane(&p, a)).unwrap_or(false);
            if on {
                p.widget().add_css_class("pp-pane-active");
            } else {
                p.widget().remove_css_class("pp-pane-active");
            }
        }
    }

    /* ---- 输入广播（3.2） ---- */

    /// 当前所有终端面板（每标签的主 pane + 分屏副 pane + 额外终端页签），扁平化。
    fn panes(&self) -> Vec<TermPane> {
        let mut out = Vec::new();
        {
            let tabs = self.tabs.borrow();
            for t in tabs.iter() {
                // 布局树已经覆盖全部终端子页签（主终端 + 额外页签 + 各自的
                // 分屏副面板），不用再单独遍历 `term_pages`（那样会重复一遍）。
                out.extend(t.all_leaves());
            }
        }
        out
    }

    /// 能收到 `src` 按键的面板：除 `src` 自己之外的所有 pane。
    /// （分屏的两个 pane 现在是各自独立的会话，所以都该收到。）
    fn broadcast_targets(&self, src: &Rc<term::TermInner>) -> Vec<TermPane> {
        self.panes()
            .into_iter()
            .filter(|p| !Rc::ptr_eq(p.handle(), src))
            .collect()
    }

    /// 把一次按键广播给所有目标面板。
    fn broadcast_from(&self, src: &Rc<term::TermInner>, bytes: &[u8]) {
        for p in self.broadcast_targets(src) {
            p.feed_keys(bytes);
        }
    }

    /// 把全局广播状态铺到每个面板：装/卸钩子，刷新指示条与菜单勾选。
    /// 开标签、关标签、合并/切分分屏后都要调一次。
    pub(crate) fn sync_broadcast(&self) {
        let on = self.broadcast.get();
        for p in self.panes() {
            if on {
                p.set_broadcast_hook(Some(Box::new(move |src, bytes| {
                    // 走全局单例反查，避免闭包捕获 Rc<Tabs> 与面板成环
                    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
                        t.broadcast_from(src, bytes);
                    }
                })));
            } else {
                p.set_broadcast_hook(None);
            }
            let n = self.broadcast_targets(p.handle()).len();
            p.sync_broadcast_ui(on, n);
        }
    }

    pub fn broadcast_on(&self) -> bool {
        self.broadcast.get()
    }

    /// 当前标签的**主**终端面板（分屏时取主面板；菜单项针对主终端）。
    fn current_term_pane(&self) -> Option<TermPane> {
        let page = self.current_page_widget()?;
        let i = self.index_of(&page)?;
        Some(self.tabs.borrow()[i].term_pane.clone())
    }

    /// 主终端是否在记日志（阶段 74：菜单开关项的勾选状态）。
    pub fn logging_on(&self) -> bool {
        self.current_term_pane()
            .map(|p| p.log_path().is_some())
            .unwrap_or(false)
    }

    /// 主终端是否在录制（阶段 74：菜单开关项的勾选状态）。
    pub fn recording_on(&self) -> bool {
        self.current_term_pane().map(|p| p.is_recording()).unwrap_or(false)
    }

    pub fn set_broadcast(&self, on: bool) {
        if self.broadcast.get() == on {
            return;
        }
        self.broadcast.set(on);
        self.sync_broadcast();
    }

    pub fn toggle_broadcast(&self) {
        self.set_broadcast(!self.broadcast.get());
    }

    /* ---- 终端日志（3.4） ---- */

    /// 切换主终端的日志开关（分屏的副终端有它自己的右键开关）。
    pub fn toggle_logging(&self, page: &gtk::Widget) {
        let Some(i) = self.index_of(page) else { return };
        let pane = self.tabs.borrow()[i].term_pane.clone();
        match pane.toggle_log() {
            Ok(_) => self.sync_log_ui(),
            Err(e) => {
                panel::dialogs::show_error(self.tabs.borrow()[i].panel_st.topwin().as_ref(), tr("无法记录日志"), &e);
                pane.sync_log_ui(); // 保持未勾选
            }
        }
    }

    pub fn stop_logging(&self, page: &gtk::Widget) {
        let Some(i) = self.index_of(page) else { return };
        let pane = self.tabs.borrow()[i].term_pane.clone();
        pane.stop_log();
        self.sync_log_ui();
    }

    fn sync_log_ui(&self) {
        for p in self.panes() {
            p.sync_log_ui();
        }
    }

    /* ---- 会话录制（6.1） ---- */

    /// 切换主终端的录制开关（分屏的副终端有它自己的右键开关）。
    pub fn toggle_recording(&self, page: &gtk::Widget) {
        let Some(i) = self.index_of(page) else {
            return;
        };
        let pane = self.tabs.borrow()[i].term_pane.clone();
        match pane.toggle_recording() {
            Ok(_) => self.sync_rec_ui(),
            Err(e) => {
                panel::dialogs::show_error(
                    self.tabs.borrow()[i].panel_st.topwin().as_ref(),
                    tr("无法开始录制"),
                    &e,
                );
                pane.sync_rec_ui(); // 保持未勾选
            }
        }
    }

    pub fn stop_recording(&self, page: &gtk::Widget) {
        let Some(i) = self.index_of(page) else {
            return;
        };
        let pane = self.tabs.borrow()[i].term_pane.clone();
        pane.stop_recording();
        self.sync_rec_ui();
    }

    fn sync_rec_ui(&self) {
        for p in self.panes() {
            p.sync_rec_ui();
        }
    }

    /// 主题切换后重刷主题相关的状态色（面板状态点 / 队列失败色）。
    fn refresh_theme_colors(&self) {
        let sts: Vec<Rc<panel::PanelState>> = self
            .tabs
            .borrow()
            .iter()
            .map(|t| Rc::clone(&t.panel_st))
            .collect();
        for st in sts {
            panel::reapply_status(&st);
            panel::queue::flush(&st);
            // 内置查看/编辑器的语法高亮颜色是写死在 tag 上的，得重画
            panel::fileedit::refresh_theme_colors(&st);
            // 数据库窗口的 SQL 编辑器同理
            panel::dbui::refresh_theme_colors(&st);
        }
    }

    /// 换终端配色方案（4.1）：所有已开的面板立即生效，不用重连。
    pub fn sync_term_palettes(&self) {
        for p in self.panes() {
            p.apply_palette();
        }
    }
}

/// 欢迎页（空状态）：图标 + 定位 + 主操作 + 快捷键速查。
///
/// 原来只有三行居中文字，看着像"页面没加载完"；这里给一个明确的主行动
/// （新建连接）和一个次行动（快捷键），并把常用键位做成两列速查表。
pub fn welcome_page() -> gtk::Widget {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.set_valign(gtk::Align::Center);
    box_.set_halign(gtk::Align::Center);

    // ⚠️ 阶段 80（审计 P2-9）：原来这儿摆着 **88px 大图标 + 一行大字
    // 「PuTTYPlus」**——可品牌在**顶栏**和**标签页**已经各出现一次，
    // 三处重复；用户第一眼看到的是"应用名"，不是"能干什么"。
    // ⇒ 去掉图标与大标题，只留一句定位 + 「最近连接 / 新建连接」两块。
    //
    // 阶段 91：定位句**提成标题**（正常色、稍大），下面加一句"下一步干什么"
    // 的副标题 ⇒ 一眼知道这是干什么的、也知道自己该点哪儿。
    let recents = crate::recent::load();
    let title = gtk::Label::new(Some(tr("SSH 终端 · SFTP 文件 · 数据库工作台")));
    title.add_css_class("pp-welcome-title");
    title.set_margin_bottom(4);
    box_.append(&title);
    let sub = gtk::Label::new(Some(if recents.is_empty() {
        tr("从左侧建一个连接就能开工")
    } else {
        tr("选一个最近连接就能开工")
    }));
    sub.add_css_class("dim-label");
    sub.set_margin_bottom(18);
    box_.append(&sub);

    // ---- 最近连接：做成**卡片**（阶段 91）----
    //
    // ⚠️ **为什么不是一排只写名字的按钮**（用户 2026-11-01）：
    // 那样看着像"一列文字"，**看不出能点**——首次打开软件的人根本不知道
    // 那些名字是干什么的。卡片有边框、有图标、有 `user@host:port` 的副标题、
    // 右侧还有一个箭头 ⇒ 一眼知道是什么，也知道点它会发生什么。
    //
    // 侧栏里有「最近」，欢迎页原来没有 ⇒ 两处说法不一致（审计 P2-9）。
    // 同一份数据、同一套转换（`sidebar::opts_for_recent`）⇒ 两处一致。
    if !recents.is_empty() {
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.set_halign(gtk::Align::Center);
        let h = gtk::Label::new(Some(tr("最近连接")));
        h.add_css_class("pp-section");
        head.append(&h);
        let hint = gtk::Label::new(Some(tr("点一下就连接")));
        hint.add_css_class("dim-label");
        head.append(&hint);
        box_.append(&head);

        let grid = gtk::Grid::new();
        grid.set_column_spacing(10);
        grid.set_row_spacing(10);
        grid.set_halign(gtk::Align::Center);
        grid.set_margin_top(4);
        let entries = crate::connmgr::load_all();
        for (i, r) in recents.iter().take(6).enumerate() {
            let card = recent_card(r, &entries);
            grid.attach(&card, (i % 3) as i32, (i / 3) as i32, 1, 1);
        }
        box_.append(&grid);
    } else {
        // 没有最近记录：**不摆**「最近连接」块（摆个空块更没用，审计 P2-9），
        // 只留一句提示，让人知道「新建连接」在哪儿。
        let hint = gtk::Label::new(Some(tr("还没有连接记录 —— 从左侧建一个连接")));
        hint.add_css_class("dim-label");
        hint.set_margin_bottom(4);
        box_.append(&hint);
    }

    // ⚠️ **这里原来有一颗「新建连接」大按钮，现在删了**
    // （用户 2026-11-02：「欢迎页面不要有『新建连接』按钮了。
    // 和侧栏的连接管理器中的重复了。」）。
    //
    // 理由跟顶栏那个「＋」一样（阶段 92c）：**同一个入口出现两遍** ——
    // 侧栏标题栏的「＋」（统一的新建菜单）和侧栏自己的空态 CTA
    // （`emptystate::sidebar()`，一条连接都没有时摆「新建连接」）都管这事，
    // 欢迎页再摆一颗就是第三份 ⇒ 功能一点不丢，屏幕干净了。
    //
    // ⚠️ 顶栏的「＋」早就删了（阶段 92c），所以**欢迎页上能点到的入口
    // 就是侧栏那个「＋」** ⇒ 文案要跟着改，不能再让人去找一个不存在的
    // 按钮（上面两句提示已经改过）。

    // ---- 常用键位只留一行 ----
    //
    // ⚠️ 原来这里是一张**两列六行**的速查表，跟 F1 的快捷键一览**重复**
    // （审计 P2-9）⇒ 收成一行，剩下的按 F1 看。
    let d = |k: &str| {
        crate::shortcuts::by_display(k)
            .map(|b| b.display.to_string())
            .unwrap_or_else(|| k.to_string())
    };
    let tip = gtk::Label::new(Some(&trf!("常用键位：{v0} 新建标签 · {v1} 查找 · {v2} 快捷键一览", v0 = d("Ctrl+Shift+T"), v1 = d("Ctrl+F"), v2 = d("F1"))));
    tip.add_css_class("dim-label");
    tip.set_margin_top(22);
    // ⚠️ 必须限宽 + 开 wrap：这一行字挺长，窄窗口里会被**裁掉半截**
    // （"F1 快捷键一览" 只剩一个"览"）。
    tip.set_wrap(true);
    tip.set_max_width_chars(44);
    box_.append(&tip);

    box_.upcast()
}

/// 欢迎页的**最近连接卡片**：图标 + 名字 + `user@host:port` + 右侧箭头。
///
/// ⚠️ **别加 `flat`**（AGENTS.md 记过）：Adwaita 的 `button.flat` 是
/// `background-color: transparent`，优先级比自定义类高 ⇒ 加了卡片的底色和
/// 边框就没了（看着像没卡片，只剩一行字）。
fn recent_card(r: &crate::recent::RecentConn, entries: &[crate::connmgr::ConnEntry]) -> gtk::Widget {
    let port = if r.port > 0 { r.port } else { 22 };
    let meta = format!("{}@{}:{}", r.user, r.host, port);

    let b = gtk::Button::new();
    b.add_css_class("pp-recent-card");
    b.set_tooltip_text(Some(&trf!("连到 {meta} —— 点一下就开", meta = meta)));

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let icon = gtk::Image::from_icon_name("network-server-symbolic");
    icon.set_pixel_size(20);
    icon.set_valign(gtk::Align::Center);
    row.append(&icon);

    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let name = gtk::Label::new(Some(&r.label));
    name.set_xalign(0.0);
    name.add_css_class("pp-card-title");
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(20);
    texts.append(&name);
    let sub = gtk::Label::new(Some(&meta));
    sub.set_xalign(0.0);
    sub.add_css_class("pp-card-sub");
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(26);
    texts.append(&sub);
    row.append(&texts);

    // 右侧箭头：明确"点进去"（阶段 91）
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    arrow.set_pixel_size(14);
    arrow.set_valign(gtk::Align::Center);
    arrow.add_css_class("pp-card-arrow");
    row.append(&arrow);

    b.set_child(Some(&row));

    let r2 = r.clone();
    let ents = entries.to_vec();
    b.connect_clicked(move |_| {
        let o = crate::sidebar::opts_for_recent(&r2, &ents);
        if let Some(t) = current_tabs() {
            t.open(&o);
        }
    });
    b.upcast()
}

/// 欢迎页「新建连接」：走连接管理器（选已有或新建）。
pub fn prompt_new() {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        TabsHandle(t).prompt_new();
    }
}

/// 弹连接管理器，选中后开新标签（取消则不打扰）。
/// 按标签页控件切分/合屏（菜单回调用；TermPane 不知道自己在哪个标签）。
pub fn split_page(page: &gtk::Widget, stacked: bool) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.split_page(page, stacked);
    }
}

/// 从指定面板分（右键菜单：右键哪块分哪块）。
pub fn split_pane(page: &gtk::Widget, pane: &TermPane, stacked: bool) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.split_pane(page, pane, stacked);
    }
}

/// 关闭当前活动面板（"关闭当前终端"）。
pub fn close_pane_page(page: &gtk::Widget) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.close_pane_current();
    }
}

/// 关闭指定面板（右键菜单：右键哪块关哪块）。
pub fn close_pane(page: &gtk::Widget, pane: &TermPane) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.close_pane(page, pane);
    }
}

/// 指定标签页的面板状态（终端右键菜单用：对着哪个标签操作就是哪一份）。
pub fn panel_state_for(page: &gtk::Widget) -> Option<Rc<panel::PanelState>> {
    TABS.with(|w| w.borrow().clone()).and_then(|t| t.panel_state_for(page))
}

/// 会话树右键「重新连接终端」：标签还在就重连它，标签没了就重建一个。
pub fn reconnect_term_with_opts(opts: &ConnOptions) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.reconnect_term_with_opts(opts);
    }
}

/// 广播总开关（右键菜单勾选框 / Alt+Shift+B / 指示条）。
pub fn set_broadcast(on: bool) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.set_broadcast(on);
    }
}

pub fn broadcast_on() -> bool {
    TABS.with(|w| w.borrow().clone()).is_some_and(|t| t.broadcast_on())
}

/// 当前标签有没有终端（顶栏菜单里终端相关的项据此置灰，阶段 74）。
///
/// 欢迎页没有终端 ⇒ 「复制/粘贴/查找/分屏/关闭当前终端」这些点了没反应的
/// 项应该**灰掉**，而不是亮着让用户去试（审计 P1-13）。
pub fn has_terminal() -> bool {
    current_terminal().is_some()
}

/// 当前标签有没有会话（连接级动作据此置灰，阶段 74）。
pub fn has_session() -> bool {
    current_panel_state().is_some()
}

/// 当前有没有标签页（欢迎页没有 ⇒ 「关闭标签」没什么可关的，阶段 74）。
pub fn has_tab() -> bool {
    current_page().is_some()
}

/// 当前标签的主终端是否在记日志（菜单开关项的勾选状态，阶段 74）。
pub fn logging_on() -> bool {
    TABS.with(|w| w.borrow().clone()).is_some_and(|t| t.logging_on())
}

/// 当前标签的主终端是否在录制（菜单开关项的勾选状态，阶段 74）。
pub fn recording_on() -> bool {
    TABS.with(|w| w.borrow().clone()).is_some_and(|t| t.recording_on())
}

/// 主题切换后重刷所有依赖主题色的东西（面板状态点、传输队列失败色）。
/// 这些颜色走 `theme::state_hex`（Pango markup / CellRenderer 只吃颜色字符串），
/// 不经过 CSS，不会自动跟着变。
pub fn refresh_theme_colors() {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.refresh_theme_colors();
    }
}

/// 换终端配色：所有已开面板立即重刷（经单例反查，theme 模块不该知道 Tabs 的类型）。
pub fn sync_term_palettes() {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.sync_term_palettes();
    }
}

/// 在**某个标签**里开一个终端并进到指定目录（「在此处打开终端」）。
pub fn open_term_at(st: &Rc<panel::PanelState>, dir: &str) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.open_term_at(st, dir);
    }
}

/// 「在此处打开终端」往 pty 里写的初始命令。
///
/// ⚠️ **这里只有命令本身，不带前面的空行**：空行是**探路**用的
///    （`term::ReadyCmd::Probe`），由就绪钩子按需发 —— 看见 shell 提示符
///    时说明 shell 已经起来了，不用再喂空行（免得多出一个空提示符行）。
///
/// 空行为什么重要：堡垒机那类登录脚本会打印
/// "Access granted. Press Return to begin session." 然后**读掉一整行**
/// 才往下走 —— 空行正好当它那次回车，后面的 `cd` 才能落到真 shell 上
/// （用户实测：少了它，`cd` 被登录脚本吃掉，目录没进去）。
pub fn cwd_cmd(dir: &str) -> String {
    format!("cd {}\n", shell_quote(dir))
}

/// 给远端 shell 的路径加引号（空格/特殊字符安全的 `cd`）。
///
/// 单引号里不能有单引号 → 用 `'\''`（反斜杠 + 两个单引号）的经典转义。
fn shell_quote(p: &str) -> String {
    // ⚠️ Rust 里 `"'\''"` 是**两个**单引号，不是 shell 经典的 `\'\''`
    //    （反斜杠 + 两个单引号）—— 写错的话，路径里有单引号时
    //    `cd '/it''s'` 会被 shell 读成 `/its` ⇒ 静默进错目录。
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// 当前标签的终端（菜单动作的公共前置）。
/// 其它已连接 SFTP 的标签（跨服务器传输用）。
pub fn remote_targets(
    exclude: &Rc<panel::PanelState>,
) -> Vec<crate::panel::relayui::RemoteTarget> {
    TABS.with(|w| w.borrow().clone())
        .map(|t| t.remote_targets(exclude))
        .unwrap_or_default()
}

pub fn current_terminal() -> Option<vte4::Terminal> {
    TABS.with(|w| w.borrow().clone())
        .and_then(|t| t.current_terminal())
}

/// 当前标签页控件（菜单动作的公共前置）。
pub fn current_page() -> Option<gtk::Widget> {
    TABS.with(|w| w.borrow().clone())
        .and_then(|t| t.current_page_widget())
}

/// 当前窗口的 `Tabs`（阶段 68：「新建」入口要知道往哪儿建）。
pub fn current_tabs() -> Option<Rc<Tabs>> {
    TABS.with(|w| w.borrow().clone())
}

/// 当前标签的面板状态（菜单栏会话级动作的公共前置）。
pub fn current_panel_state() -> Option<Rc<panel::PanelState>> {
    TABS.with(|w| w.borrow().clone())
        .and_then(|t| t.current_panel_state())
}

/// 按**会话键**找面板状态（见 `Tabs::panel_state_by_session_key`）。
pub fn panel_state_by_session_key(key: &str) -> Option<Rc<panel::PanelState>> {
    TABS.with(|w| w.borrow().clone())
        .and_then(|t| t.panel_state_by_session_key(key))
}

pub fn toggle_broadcast() {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.toggle_broadcast();
    }
}

/// 日志开关（终端右键菜单勾选项）。开着时先起文件，起不来把原因报给用户。
pub fn toggle_logging(page: &gtk::Widget) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.toggle_logging(page);
    }
}

pub fn stop_logging(page: &gtk::Widget) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.stop_logging(page);
    }
}

/// 录制开关（终端右键菜单勾选项）。开着时先起文件，起不来把原因报给用户。
pub fn toggle_recording(page: &gtk::Widget) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.toggle_recording(page);
    }
}

pub fn stop_recording(page: &gtk::Widget) {
    if let Some(t) = TABS.with(|w| w.borrow().clone()) {
        t.stop_recording(page);
    }
}

thread_local! {
    /// 当前 Tabs 实例：右键菜单回调需要从标签页控件反查回去
    static TABS: RefCell<Option<Rc<Tabs>>> = const { RefCell::new(None) };
    /// **所有**窗口的 Tabs 实例（弱引用）。
    ///
    /// 连接管理器是独立对话框，它不属于某个窗口——要显示"这条连接正连着"
    /// 就得跨窗口找（多窗口同时开着是堡垒机场景的常态）。
    static ALL_TABS: RefCell<Vec<std::rc::Weak<Tabs>>> = const { RefCell::new(Vec::new()) };
}

/// 跨窗口按会话键找面板状态（**不切标签页**）。
///
/// 连接管理器是独立对话框，不属于某个窗口 —— 它要操作某条会话的转发规则时
/// 用这个找（找不到 = 那个会话没开着，只改配置即可）。
pub fn find_session(key: &str) -> Option<Rc<panel::PanelState>> {
    live_sessions()
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, st)| st)
}

/// 所有窗口里已打开的会话：`(会话键, 面板状态)`。
///
/// 会话键跟连接管理器/会话树用的是同一套（`dbstore::session_key`），
/// 两边才能对上同一条连接。
pub fn live_sessions() -> Vec<(String, Rc<panel::PanelState>)> {
    let mut out = Vec::new();
    ALL_TABS.with(|v| {
        let list: Vec<std::rc::Weak<Tabs>> = v.borrow().clone();
        let mut keep = Vec::new();
        for w in list {
            // 窗口已经关了 → 弱引用升不上去 → 顺手把这行扔掉（不然这张表只增不减）
            let Some(t) = w.upgrade() else { continue };
            keep.push(w.clone());
            out.extend(t.sessions());
        }
        *v.borrow_mut() = keep;
    });
    out
}

impl Tabs {
    fn index_of(&self, page: &gtk::Widget) -> Option<usize> {
        self.tabs.borrow().iter().position(|t| t.page == *page)
    }

    pub fn split_page(&self, page: &gtk::Widget, stacked: bool) {
        let Some(i) = self.index_of(page) else { return };
        self.split_index(i, stacked);
    }

    /// 从指定面板分（右键"左右/上下分屏"用：右键哪块就分哪块）。
    pub fn split_pane(&self, page: &gtk::Widget, pane: &TermPane, stacked: bool) {
        let Some(i) = self.index_of(page) else { return };
        if let Some(t) = self.tabs.borrow().get(i) {
            *t.active.borrow_mut() = Some(pane.clone());
        }
        self.split_index(i, stacked);
    }
}

#[cfg(test)]
mod tests {
    use super::cwd_cmd;

    /// 「在此处打开终端」喂给 pty 的初始命令。
    ///
    /// ⚠️ **命令本身不带前面的空行**：空行是**探路**用的
    ///    （`term::ReadyCmd::Probe`）—— 堡垒机的登录脚本会
    ///    "Press Return to begin session."（**读掉一整行**才往下走），
    ///    空行正好当它那次回车；命令本身得落在 shell 里才有效。
    #[test]
    fn cwd_cmd_is_plain_cd() {
        assert_eq!(cwd_cmd("/tmp"), "cd '/tmp'\n");
    }

    /// 路径里有空格/单引号时也要安全（单引号包起来 + 经典转义）。
    #[test]
    fn cwd_cmd_quotes_path() {
        assert_eq!(cwd_cmd("/a b"), "cd '/a b'\n");
        assert_eq!(cwd_cmd("/it's"), "cd '/it'\\''s'\n");
    }
}

pub struct TabsHandle(pub Rc<Tabs>);

impl TabsHandle {
    /// 新建连接：**直接开新建表单**，不再先弹连接管理器。
    /// 已有连接在侧边栏里点（那边才是"选择器"）。
    pub fn prompt_new(&self) {
        let t = Rc::clone(&self.0);
        // ⚠️ **带上父窗口**（审计 P2-16）：以前是 `None` ⇒ 多窗口时这个
        // 对话框可能浮在别的窗口上面。
        let parent = self.0.current_panel_state().and_then(|st| st.topwin());
        connmgr::ui_new_connection(parent.as_ref(), move |o| t.open(&o));
    }
}
