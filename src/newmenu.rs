//! 「新建」入口（阶段 68 / 审计 P1-2、P1-6）。
//!
//! **一句话**：同一个"新建"以前有 5 个入口（顶栏 `+` / 侧栏 `+` /
//! 子标签 `+` / 欢迎页按钮 / 文件菜单），**各写各的** ⇒ 从哪个入口进来
//! 看到的东西不一样 ⇒ 用户以为功能不一样，得多试几个地方。
//!
//! 现在：
//! - **动作表**在 [`crate::topmenus::NEW_ITEMS`]（唯一来源）；
//! - **动作的实现**只有一份，在这里（[`run_in`] / [`run`]）；
//! - 各入口只是"从表的哪个角度看"（[`crate::topmenus::NewEntry`]）。

use gtk::prelude::*;

use crate::panel::PanelState;
use crate::tabs::Tabs;
use crate::topmenus::{NewEntry, NewItem};

/// 「新建」动作在**当前上下文**下能不能用。
///
/// **为什么要有**：「新建终端」「新建数据库连接」**得有会话才能建**——
/// 没有会话时点了没反应，用户会以为程序卡了 ⇒ 那种情况下把菜单项
/// **置灰**（而不是让它点了没反应）。
///
/// ⚠️ 顶栏的「＋」在**欢迎页上是隐藏的**（既有行为：页面自己有大按钮，
/// "同屏只留一个"）⇒ 欢迎页上能点到的统一入口是**侧栏的「＋」**。
#[derive(Clone, Copy, Debug)]
pub struct NewCtx {
    /// 有没有可用的会话（连接页）。
    pub has_session: bool,
}

impl NewCtx {
    /// 没有任何会话（欢迎页）⇒ 只有「新建连接」能用。
    pub fn empty() -> Self {
        NewCtx { has_session: false }
    }

    /// 有会话（子标签栏的「＋」永远属于某个连接）。
    pub fn with_session() -> Self {
        NewCtx { has_session: true }
    }

    /// 按**当前窗口**的会话情况算（顶栏 / 侧栏用）。
    pub fn of(tabs: &std::rc::Rc<Tabs>) -> Self {
        NewCtx {
            has_session: tabs.current_panel_state().is_some(),
        }
    }

    /// 这个动作现在能不能点。
    pub fn allows(&self, action: &str) -> bool {
        match action {
            "new_term" | "new_db" => self.has_session,
            _ => true,
        }
    }
}

/// 按 action 名跑「新建」动作（**唯一**的实现），在**指定**的窗口里跑。
///
/// 顶栏的「＋」、侧栏的「＋」、子标签的「＋」、欢迎页的按钮、文件菜单
/// 全都调它 ⇒ 改一处，四处一起变。
///
/// ⚠️ **窗口要显式传**：多窗口时"当前窗口"不是全局唯一的东西（全局那个
/// `tabs::current_tabs()` 是最后创建的那个窗口）⇒ 顶栏的菜单必须建在
/// **自己这个窗口**里。
pub fn run_in(tabs: &std::rc::Rc<Tabs>, action: &str) {
    let st = tabs.current_panel_state();
    run_in_ctx(tabs, st.as_ref(), action);
}

/// 按 action 名跑「新建」动作，**指定会话**。
///
/// 子标签栏的「＋」用它：那个「＋」属于**某个连接**，动作必须落在
/// **它自己的**会话里（会话预置死——从哪儿来就属于哪儿）。
///
/// ⚠️ 以前这里是 `tabs.current_panel_state()`（**当前**会话）⇒ 用户切到
/// 另一个标签页，再从旧标签页的「＋」建库连接 ⇒ 连接到**别的会话**去了。
pub fn run_in_session(tabs: &std::rc::Rc<Tabs>, st: &std::rc::Rc<PanelState>, action: &str) {
    run_in_ctx(tabs, Some(st), action);
}

/// 按 action 名跑「新建」动作（用**当前**窗口）。
pub fn run(action: &str) {
    if let Some(t) = crate::tabs::current_tabs() {
        run_in(&t, action);
    }
}

/// 动作分发的**唯一**地方。
fn run_in_ctx(tabs: &std::rc::Rc<Tabs>, st: Option<&std::rc::Rc<PanelState>>, action: &str) {
    match action {
        "new" => new_connection(tabs, st),
        "new_term" => new_terminal(tabs),
        "new_db" => new_db_connection(st),
        "import" => import_putty(tabs),
        _ => {}
    }
}

/// 新建连接（走新建表单）。
fn new_connection(tabs: &std::rc::Rc<Tabs>, st: Option<&std::rc::Rc<PanelState>>) {
    // ⚠️ **带上父窗口**（审计 P2-16）：以前这里是 `None` ⇒ 多窗口时
    // 这个对话框可能浮在别的窗口上面。
    let parent = st.and_then(|s| s.topwin());
    let t = tabs.clone();
    crate::connmgr::ui_new_connection(parent.as_ref(), move |o| t.open(&o));
}

/// 在**当前会话**里新建一个终端页。
fn new_terminal(tabs: &std::rc::Rc<Tabs>) {
    let Some(page) = tabs.current_page_widget() else {
        return;
    };
    tabs.new_term_page(&page);
}

/// 在**指定会话**里新建数据库连接（建好直接打开工作台并连上）。
fn new_db_connection(st: Option<&std::rc::Rc<PanelState>>) {
    let Some(st) = st else { return };
    let key = crate::backend::dbstore::session_key(&st.opts);
    let parent = st.topwin();
    let st2 = st.clone();
    crate::panel::dbconn::ui_new(parent.as_ref(), &key, move |k, name| {
        // 建好之后打开工作台并连上（用户刚建的就是要用的那条）
        crate::panel::on_open_db_page(&st2);
        crate::panel::dbui::use_profile(&st2, &k, &name);
    });
}

/// 导入 PuTTY 会话（选目录）。
fn import_putty(tabs: &std::rc::Rc<Tabs>) {
    let parent = tabs.current_panel_state().and_then(|st| st.topwin());
    crate::connmgr::ui_import_putty(parent.as_ref());
}

/// 建一个「新建」菜单的内容（挂到 `vb` 上）。
///
/// `ctx` 决定哪些项能点（没有会话时「新建终端」「新建数据库连接」置灰）。
/// `on_pick` 收到 action 名（调用方决定怎么执行——顶栏用自己窗口的
/// `Tabs`，侧栏用当前窗口的）。
pub fn build_menu<F>(entry: NewEntry, vb: &gtk::Box, ctx: NewCtx, on_pick: F)
where
    F: Fn(&'static str) + 'static,
{
    // ⚠️ 闭包要在循环里用多次 ⇒ 包一层 `Rc`（否则第一轮就被 move 走了）
    let on_pick = std::rc::Rc::new(on_pick);
    for it in entry.items() {
        let b = crate::menu::item(it.icon, it.label, None);
        let ok = ctx.allows(it.action);
        b.set_sensitive(ok);
        if !ok {
            b.set_tooltip_text(Some("当前没有会话，先新建一个连接"));
        }
        let action = it.action;
        let f = on_pick.clone();
        b.connect_clicked(move |_| f(action));
        vb.append(&b);
    }
}

/// 「新建」菜单项的图标（入口不同，图标不同——见 [`NewEntry::icon`]）。
pub fn entry_icon(entry: NewEntry) -> &'static str {
    entry.icon()
}

/// 「新建」菜单项的 tooltip（写清作用域）。
pub fn entry_tooltip(entry: NewEntry) -> &'static str {
    entry.tooltip()
}

/// 表里的一项（给想自己拼菜单的调用方用）。
pub fn item_of(entry: NewEntry, action: &str) -> Option<&'static NewItem> {
    entry.items().into_iter().find(|i| i.action == action)
}
