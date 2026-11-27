//! 侧边栏外壳（阶段 177）：**主窗口左栏** = 页签 `[连接 | 文件]`。
//!
//! **为什么要有这一层**（计划见 `docs/SIDEBAR_PLAN_2026-11-27.md`）：
//!! 屏幕左边原来**分属两处** ——「会话侧边栏」（连接管理，230px）在主窗口
//! 那一层，文件面板（340px）在连接页的格子树里 ⇒ 同一个功能分居两处，
//! 而且**左侧恒占 570px** ⇒ 1200 宽的窗口里终端只剩 ≈630px。
//!
//! ⇒ 合并成**一栏**（页签切换）⇒ 终端占满整页宽（≈860px）。
//!
//! **为什么用 `gtk::Notebook` 而不是 `Stack` + `StackSwitcher`**：
//! 主题里 `notebook > header > tabs > tab` 已经有一整套样式
//! （`src/theme.rs`）⇒ 用 Notebook 才跟项目里其它页签栏**长得一样**；
//! `StackSwitcher` 是另一套外观 ⇒ 同一个窗口里两套页签样式。
//!
//! ⚠️ **本阶段（177）只加外壳、不改行为**：「文件」页先放占位，
//! 文件面板搬进来是**阶段 178**。

use std::rc::Rc;

use gtk::prelude::*;

use crate::i18n::tr;
use crate::sidebar::Sidebar;

/// 侧边栏外壳：**页签栏 ＋ 两个页**。
///
/// | 页 | 内容 |
/// |---|---|
/// | 「连接」 | 现有的会话侧边栏（`Sidebar::widget`）—— **一个字不改** |
/// | 「文件」 | 文件面板的槽位（阶段 178 起才有内容） |
///
/// ⚠️ **连接管理不做第二套**（阶段 77a/77b 的决定，审计 P1-1）：
/// 侧栏里已经有全套 CRUD ⇒ 这里**只是把它换了个位置**，
/// 绝不复制一份增删改。
pub struct SidebarHost {
    /// 外层容器（竖向）：Notebook 本体。
    ///
    /// ⚠️ **不给它加 `pp-sidebar` 类**：`sidebar_menu_smoke` 会数
    /// `pp-sidebar` 里的控件 ⇒ 加了会让"只有一个"的断言失真。
    pub root: gtk::Box,
    /// 页签容器（两页：连接 / 文件）。
    pub nb: gtk::Notebook,
    /// 「文件」页的**页面内容**（里面就一个槽位）。
    pub files_page: gtk::Box,
    /// 「文件」页的槽位——文件面板（`PanelState::root`）塞这里。
    ///
    /// ⚠️ **槽位里永远只有一个孩子**：要么是文件面板，要么是占位标签。
    pub files_slot: gtk::Box,
    /// 两个页签的标题标签（冒烟要读文字 ⇒ 存着，别去控件树里翻）。
    pub conn_label: gtk::Label,
    pub files_label: gtk::Label,
}

/// 「连接」页在 Notebook 里的页号。
pub const PAGE_CONN: i32 = 0;
/// 「文件」页在 Notebook 里的页号。
pub const PAGE_FILES: i32 = 1;

/// 「文件」页的槽位里**放什么**。
///
/// - `Some(w)`：把 `w`（一条会话的文件面板）挂进槽位；
/// - `None`：没有连接 ⇒ 显示占位（「还没有连接」）。
///
/// ⚠️ **为什么是自由函数、不是 `SidebarHost` 的方法**（2026-11-27 修的真 bug）：
///    `Tabs` 要在**切连接标签**时挂文件面板 ⇒ 它得拿到槽位；
///    但它**不能**持有 `Rc<SidebarHost>` ——
///    `build_main_window()` 返回 `(tabs, win)` 时那个 `Rc` **当场被 drop**
///    ⇒ 存 `Weak<SidebarHost>` 就**永远 upgrade 不到**
///    ⇒ 文件面板永远挂不进侧边栏（用户 2026-11-27 截图报的现象：
///    终端连上了，文件页还是「还没有连接」）。
///    ⇒ 改成只依赖**槽位控件** ⇒ 与外壳结构体的生死**无关**。
///
/// ⚠️ **GTK4：一个 widget 只能有一个父级** ⇒ 挂之前**必须**把它从旧
///   父级上摘下来（否则 GTK 会报断言 / 直接抢走）。
///   ⇒ 切换连接时就是靠这一步把 A 的面板换成 B 的。
pub fn show_files_in(slot: &gtk::Box, w: Option<&gtk::Widget>) {
    // 先把槽位里现在挂着的**摘掉**（文件面板只有一个 ⇒ 换人时先腾位）。
    let mut c = slot.first_child();
    while let Some(ch) = c {
        let next = ch.next_sibling();
        slot.remove(&ch);
        c = next;
    }
    match w {
        Some(w) => {
            if let Some(p) = w.parent() {
                p.unparent();
            }
            slot.append(w);
        }
        None => {
            // ⚠️ **占位每次现建**：槽位里只留一个孩子 ⇒ 不需要"藏起来
            //   再显示出来"那套 ⇒ 也就不必把占位当成一个**字段**存着
            //   （存着它就等于把外壳结构体的生死绑到同步逻辑上）。
            let l = gtk::Label::new(Some(tr("还没有连接")));
            l.add_css_class("pp-hint");
            l.set_halign(gtk::Align::Center);
            l.set_valign(gtk::Align::Center);
            l.set_vexpand(true);
            slot.append(&l);
        }
    }
}

/// 槽位里现在挂着的是不是这个 widget（冒烟用）。
pub fn files_shows(slot: &gtk::Box, w: &gtk::Widget) -> bool {
    slot
        .first_child()
        .map(|c| c == *w.upcast_ref::<gtk::Widget>())
        .unwrap_or(false)
}

/// 侧边栏外壳的**两个关键控件**的弱引用。
///
/// ⚠️ **为什么是弱引用、而且是「控件」而不是「外壳结构体」**：
///    `build_main_window()` 返回 `(tabs, win)` ⇒ 里面的
///    `Rc<SidebarHost>` **当场被 drop** ⇒ 拿它的弱引用**永远 upgrade 不到**
///    ⇒ 文件面板永远挂不进侧边栏（2026-11-27 用户报的真 bug）。
///    ⇒ 只依赖**控件** ⇒ 控件在树上就够用 ⇒ 与结构体生死无关。
///
/// ⚠️ **弱引用 ⇒ 不构成引用环**（AGENTS.md 引用环红线）：这些控件的树里
///   没有 `Rc<Tabs>` ⇒ `Tabs → 控件` 不是环。
pub struct SidebarRefs {
    /// 页签容器（切页、看当前页）。
    pub nb: glib::WeakRef<gtk::Notebook>,
    /// 「文件」页的槽位（挂文件面板）。
    pub slot: glib::WeakRef<gtk::Box>,
}

impl SidebarRefs {
    /// 从外壳里取这两个控件的弱引用。
    pub fn of(host: &Rc<SidebarHost>) -> SidebarRefs {
        let mut nb: glib::WeakRef<gtk::Notebook> = glib::WeakRef::new();
        nb.set(Some(&host.nb));
        let mut slot: glib::WeakRef<gtk::Box> = glib::WeakRef::new();
        slot.set(Some(&host.files_slot));
        SidebarRefs { nb, slot }
    }
}

impl SidebarHost {
    /// 建外壳：把现有的侧栏（`sb.widget`）放进「连接」页。
    ///
    /// ⚠️ **不接管 `sb` 的生命周期**：`Sidebar` 实例仍然在外面活着
    /// （顶栏要拿它的凭据库状态标志 ⇒ `topbar::build_header` 收的是
    /// `&Rc<Sidebar>`）⇒ 这里只是**换个挂载位置**。
    pub fn new(sb: &Rc<Sidebar>) -> Rc<SidebarHost> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("pp-sidebar-host");

        let nb = gtk::Notebook::new();
        nb.set_scrollable(true);
        nb.set_vexpand(true);
        nb.set_hexpand(true);

        // ---- 「连接」页：现有的会话侧边栏 ----
        let l_conn = gtk::Label::new(Some(tr("连接")));
        nb.append_page(&sb.widget, Some(&l_conn));

        // ---- 「文件」页：槽位（现在放占位，有了连接放文件面板）----
        let files_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let files_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        show_files_in(&files_slot, None);
        files_page.append(&files_slot);
        let l_files = gtk::Label::new(Some(tr("文件")));
        nb.append_page(&files_page, Some(&l_files));

        root.append(&nb);

        Rc::new(SidebarHost {
            root,
            nb,
            files_page,
            files_slot,
            conn_label: l_conn,
            files_label: l_files,
        })
    }

    /// 「文件」页显示**哪个**文件面板（见 [`show_files_in`]）。
    pub fn show_files(&self, w: Option<&gtk::Widget>) {
        show_files_in(&self.files_slot, w);
    }

    /// 「文件」页现在挂着的是不是这个 widget（冒烟用）。
    pub fn files_shows(&self, w: &gtk::Widget) -> bool {
        files_shows(&self.files_slot, w)
    }

    /// 切到指定页（`PAGE_CONN` / `PAGE_FILES`）。
    pub fn switch_to(&self, page: i32) {
        if page >= 0 && (page as u32) < self.nb.n_pages() {
            self.nb.set_current_page(Some(page as u32));
        }
    }

    /// 当前页（`PAGE_CONN` / `PAGE_FILES`）。
    pub fn current_page(&self) -> i32 {
        self.nb.current_page().map(|p| p as i32).unwrap_or(PAGE_CONN)
    }

    /// 页签标题（冒烟用）。
    pub fn page_titles(&self) -> Vec<String> {
        vec![
            self.conn_label.text().to_string(),
            self.files_label.text().to_string(),
        ]
    }
}
