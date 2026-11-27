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
    /// 「文件」页的**页面内容**（占位 ＋ 面板槽位）。
    pub files_page: gtk::Box,
    /// 「文件」页的槽位——文件面板（`PanelState::root`）塞这里。
    pub files_slot: gtk::Box,
    /// 占位（还没有任何连接时显示）。
    files_placeholder: gtk::Label,
    /// 两个页签的标题标签（冒烟要读文字 ⇒ 存着，别去控件树里翻）。
    pub conn_label: gtk::Label,
    pub files_label: gtk::Label,
}

/// 「连接」页在 Notebook 里的页号。
pub const PAGE_CONN: i32 = 0;
/// 「文件」页在 Notebook 里的页号。
pub const PAGE_FILES: i32 = 1;

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

        // ---- 「文件」页：先放占位（阶段 178 起换成真的文件面板）----
        //
        // ⚠️ **页面内容 = [占位, 面板槽位]** 两个控件：
        //   占位是「还没有连接」时的提示；槽位是文件面板的家。
        //   ⇒ 有了面板就把占位**藏起来**（不删）⇒ 切回「没有连接」时
        //   还能再显示出来。
        let files_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let files_placeholder = gtk::Label::new(Some(tr("还没有连接")));
        files_placeholder.add_css_class("pp-hint");
        files_placeholder.set_halign(gtk::Align::Center);
        files_placeholder.set_valign(gtk::Align::Center);
        files_placeholder.set_vexpand(true);
        files_page.append(&files_placeholder);
        let files_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        files_page.append(&files_slot);
        let l_files = gtk::Label::new(Some(tr("文件")));
        nb.append_page(&files_page, Some(&l_files));

        root.append(&nb);

        Rc::new(SidebarHost {
            root,
            nb,
            files_page,
            files_slot,
            files_placeholder,
            conn_label: l_conn,
            files_label: l_files,
        })
    }

    /// 「文件」页显示**哪个**文件面板。
    ///
    /// - `Some(w)`：把 `w`（一条会话的文件面板）挂进槽位；
    /// - `None`：没有连接 ⇒ 显示占位。
    ///
    /// ⚠️ **GTK4：一个 widget 只能有一个父级** ⇒ 挂之前**必须**把它从旧
    ///   父级上摘下来（否则 GTK 会报断言 / 直接抢走）。
    ///   ⇒ 切换连接时就是靠这一步把 A 的面板换成 B 的。
    pub fn show_files(&self, w: Option<&gtk::Widget>) {
        // 先把槽位里现在挂着的**摘掉**（文件面板只有一个 ⇒ 换人时先腾位）。
        let mut c = self.files_slot.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            self.files_slot.remove(&ch);
            c = next;
        }
        match w {
            Some(w) => {
                if let Some(p) = w.parent() {
                    p.unparent();
                }
                self.files_slot.append(w);
                self.files_placeholder.set_visible(false);
            }
            None => {
                self.files_placeholder.set_visible(true);
            }
        }
    }

    /// 「文件」页现在挂着的是不是这个 widget（冒烟用）。
    pub fn files_shows(&self, w: &gtk::Widget) -> bool {
        self.files_slot
            .first_child()
            .map(|c| c == *w.upcast_ref::<gtk::Widget>())
            .unwrap_or(false)
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
