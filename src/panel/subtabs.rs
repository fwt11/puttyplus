//! 连接页里的**子标签**（对标 XTerminal 的第二层标签栏）。
//!
//! **为什么要这一层**：文件编辑器、数据库工作台原来都是**独立弹窗**——
//! 弹窗有自己的标题栏、自己的窗口大小，把屏幕切碎了；数据库工作台那点
//! 宽度更是不够用（对象树 + 结果网格挤在 1100px 的一半里）。
//!
//! 改成子标签后，它们都活在**连接页里**，能占满整块区域，形状跟 XTerminal
//! 一致：
//!
//! ```text
//! 主窗口
//! ├── 标签栏：连接 tab（srv-01 ×   +）
//! └── 当前连接页（= 子标签区，整页宽）
//!     ├── 子标签栏（终端 / 数据库 / .zshrc …）
//!     └── 终端子页：横向 Paned
//!         ├── 左：文件面板（远程/本地，**跟着当前终端页签走**）
//!         └── 右：终端列（竖向 Paned，阶段 108）
//!             ├── 上：终端
//!             └── 下：编辑区（文件编辑器，阶段 107）
//! ```
//!
//! **终端也是子标签**：XTerminal 里「终端」跟「.zshrc」是平级的子标签，
//! 我们照做——终端是第一个子页。
//!
//! ⚠️ **阶段 108**：编辑区住在**终端列**里（不是横跨整页）⇒
//! **编辑器与终端同宽**、**不占文件列表的空间**（用户 2026-11-15：
//! 「文本编辑器和 term 的宽度一样，就是不要占用文件列表的空间」）。
//! 编辑器还能在「编辑区 ⇄ 子标签栏」之间**拖动**（拖回子标签栏 =
//! 变回一个子页签，跟阶段 107 之前一样）。
//!
//! ⚠️ **阶段 97**：文件面板从「连接页左边的常驻列」搬进了**终端子页内部**
//! ⇒ 子标签栏不再因为文件面板的显隐而左右跳位（用户 2026-11-05 报）。
//! 文件面板仍是**会话级的一个 widget**，切到别的终端页签时会**搬**过去。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::prelude::*;
use crate::i18n::tr;

/// 编辑器拖动用的自定义 mime（阶段 108）。
///
/// ⚠️ **不能用 `text/plain`**：文件面板的拖放也用字符串（远端路径行）
/// ⇒ 混用的话，拖编辑器标签到文件列表上会被当成"拖路径"⇒ 误触发下载。
pub const EDITOR_MIME: &str = "application/x-puttyplus-editor";

/// 拖动负载：`pp-editor:<id>`。
fn editor_payload(id: u64) -> String {
    format!("pp-editor:{id}")
}

/// 解析拖动负载 → 编辑器 id（阶段 108）。
pub fn parse_editor_payload(s: &str) -> Option<u64> {
    s.trim()
        .strip_prefix("pp-editor:")
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// 子标签标题的最大显示列数（超了省略号收尾，完整标题进 tooltip）。
const MAX_TITLE_CHARS: i32 = 24;

/// 标题的**显示列宽**（中日韩字符按两列算，其余按一列）。
///
/// **为什么不能直接用字符个数**：等宽字体里一个汉字占两列，
/// `width_chars(4)` 只放得下两个汉字——「数据库」会被截成「数…」，
/// 用户一眼认不出哪个页签是什么。
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            // 汉字/假名/谚文/全角标点/全角字母：按两列算
            let wide = (0x1100..=0x115F).contains(&cp)   // 谚文字母
                || (0x2E80..=0xA4CF).contains(&cp)       // CJK 部首…注音…汉字
                || (0xAC00..=0xD7A3).contains(&cp)       // 谚文音节
                || (0xF900..=0xFAFF).contains(&cp)       // 兼容汉字
                || (0xFE30..=0xFE6F).contains(&cp)       // CJK 兼容形式
                || (0xFF00..=0xFF60).contains(&cp)       // 全角 ASCII
                || (0xFFE0..=0xFFE6).contains(&cp);      // 全角符号
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 子页的**种类**（阶段 66 / 审计 P0-2；阶段 97 起含义有变）。
///
/// **为什么分种类**：上层要能一眼看出"当前这一页是不是终端"——
/// 阶段 97 起，**文件面板跟着当前终端页签走**（切到终端页就把文件面板
/// 搬过去），所以这个判断是搬家的入口条件。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageKind {
    /// 其它（不属于下面几类）
    #[default]
    Other,
    /// 终端页（**文件面板的家**）
    Terminal,
    /// 数据库工作台页
    Db,
}

/// 一个子页。
struct SubPage {
    id: u64,
    title: String,
    /// 页签头上的标题标签（改名时要用）
    label: gtk::Label,
    /// 页内容（notebook 的 child）
    widget: gtk::Widget,
    /// 这一页是**什么**（阶段 66：DB 页要独占宽度）
    kind: PageKind,
    /// 关闭前的确认（编辑器有未保存修改时要问）。返回 false = 不关。
    ///
    /// 没有它就直接关的话，用户按 × 就把没保存的修改丢了。
    on_close: Option<Box<dyn Fn() -> bool>>,
}

/// **编辑区**里的一个编辑器（阶段 107；阶段 108 起可在「编辑区 ⇄ 子标签栏」之间拖）。
struct EditorPage {
    /// 编辑器 id（= `fileedit::FileEdit::id`）。
    id: u64,
    /// 图标名（重建标签时要用）。
    icon: Option<String>,
    /// 标题（重建标签时要用）。
    title: String,
    /// 标题标签（**当前活着的那一个**：搬家时换新的 ⇒ 改标题只改一处，
    /// 理由见 `make_title_label`）。
    label: gtk::Label,
    /// **编辑区小标签栏**上那一块（停靠时存在）。
    tab: Option<gtk::Box>,
    /// **子页签头**（拖回子标签栏时存在）。
    head: Option<gtk::Box>,
    /// 编辑器的内容（`fileedit::FileEdit::page`）。
    widget: gtk::Widget,
    /// 关之前先问一遍（未保存修改）。返回 false = 不关。
    on_close: Option<Box<dyn Fn() -> bool>>,
    /// 现在停在「编辑区」还是「子标签栏」（阶段 108）。
    docked: bool,
}

/// 连接页的子标签容器。
pub struct SubTabs {
    /// 子标签 notebook
    pub nb: gtk::Notebook,
    /// 外层容器：notebook + 浮在页签栏右端的「+」按钮。
    ///
    /// **为什么要包一层**：GTK4 的 Notebook 没有"页签栏末尾放个按钮"的
    /// 位置（那是 GtkHeaderBar 的功能），而用户要的就是子标签栏**最右边**
    /// 一个「+」（新建终端 / 新建数据库连接）。
    ///
    /// **为什么用 Overlay 而不是 Box**：Box 排 [notebook][按钮] 会让按钮
    /// **独占一列**——那一列在内容区是空的，看着像"软件右边多了一列"
    /// （用户明确提过）。Overlay 把按钮**浮**在页签栏右端，不占布局宽度，
    /// 内容能一直铺到窗口右边缘。
    ///
    /// ⚠️ **阶段 108**：这里**不再**是「上下分」（编辑区搬走了）⇒
    /// 连接页的内容就是子标签区本身。
    root: gtk::Overlay,
    /// **编辑区**（阶段 107）：文件编辑器住的地方。
    ///
    /// 干什么用（用户 2026-11-14）：「终端和文件编辑可以放在同一个窗口中，
    /// 同时显示」⇒ 编辑一个配置、再到终端里跑它 ⇒ 两个都得看得见。
    ///
    /// ⚠️ **阶段 108：它住在终端列的下面**（不是横跨整页）⇒ 编辑器与
    /// 终端**同宽**、**不占文件列表的空间**（用户 2026-11-15 的原话）。
    ///
    /// 结构 = 竖排的 `[小标签栏][内容栈]`：小标签栏里每个打开的文件一个
    /// 标签（点标签切换），内容栈里一次只显示一个编辑器。
    editor_area: gtk::Box,
    /// 编辑区的小标签栏（横向；文件多了可以横向滚）。
    editor_tabs: gtk::Box,
    /// 编辑区的内容栈（一次显示一个编辑器）。
    editor_stack: gtk::Stack,
    /// 编辑区的页（id → 标题 label / 内容 / 关闭回调）。
    editor_pages: RefCell<Vec<EditorPage>>,
    /// 当前显示的编辑器 id。
    editor_current: Cell<Option<u64>>,
    /// 编辑区现在的**宿主**（终端列那个竖向 Paned，阶段 108）。
    editor_host: RefCell<Option<gtk::Paned>>,
    /// 空编辑区时的提示（**停靠区**：把编辑器拖回来）。
    editor_hint: gtk::Label,
    pages: RefCell<Vec<SubPage>>,
    next_id: Cell<u64>,
    /// 「+」按钮的菜单由外部提供（建终端 / 建数据库连接是上层的业务）。
    ///
    /// 回调拿到菜单容器与弹层本身：菜单项点完要自己把弹层收起。
    new_hook: RefCell<Option<Rc<dyn Fn(&gtk::Box, &gtk::Popover)>>>,
    /// 尾部的「+」按钮（**测试钩子**：冒烟要点真按钮）。
    add_btn: gtk::Button,
    /// 「+」的弹层（**测试钩子**）。
    ///
    /// ⚠️ 弹层是 `set_parent` 挂到按钮上的，**不进容器的 children 链**
    /// ⇒ 从控件树里找不到 ⇒ 只能从这儿拿。
    new_pop: RefCell<Option<gtk::Popover>>,
    /// 子页被关掉时的通知（传页 id）。
    ///
    /// **可以有多个**：终端页签要收掉自己的 SSH 会话，数据库页签要断开
    /// 连接——各管各的，谁的东西谁自己注册（`add_close_hook`）。
    close_hooks: RefCell<Vec<Rc<dyn Fn(u64)>>>,
}

impl SubTabs {
    pub fn new() -> Rc<SubTabs> {
        let nb = gtk::Notebook::new();
        nb.set_scrollable(true);
        nb.set_vexpand(true);
        nb.set_hexpand(true);

        // 尾部的「+」按钮（新建终端 / 新建数据库连接）
        //
        // ⚠️ **图标与 tooltip 都从「新建」动作表取**（阶段 68）：子标签的
        // `+` 是「新建**页**」，顶栏的 `+` 是「新建连接」——长得一样、干的
        // 活不一样，用户分不清（审计 P1-6）⇒ 这里换成 `tab-new-symbolic`，
        // tooltip 也写清"只作用于当前会话"。
        let add = crate::a11y::icon_button(crate::newmenu::entry_icon(
            crate::topmenus::NewEntry::SubTab,
        ));
        add.add_css_class("flat");
        add.set_focusable(false);
        add.set_tooltip_text(Some(crate::newmenu::entry_tooltip(
            crate::topmenus::NewEntry::SubTab,
        )));

        // 尾部的「+」按钮浮在页签栏右端（**不占布局宽度**）
        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        btn_box.append(&add);

        // 浮在页签栏那一行的右端（**不占布局宽度**）
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&nb));
        overlay.add_overlay(&btn_box);
        // halign/valign 不是 FILL 时，Overlay 会按**自然尺寸**把 overlay
        // 子控件摆到对应的边上——所以按钮只占自己那一小块，不拦截别处的点击
        //（GTK 4.10 的 overlay-pass-through  gtk4-rs 0.11 没绑，用不着）。
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_valign(gtk::Align::Start);
        btn_box.set_margin_end(4);
        btn_box.set_margin_top(2);

        // ---- 编辑区（阶段 107/108）：终端与文件编辑器同窗同时显示 ----
        //
        // 结构 = 竖排的 `[小标签栏][内容栈]`。
        //
        // ⚠️ **没有文件时整块藏起来**（Paned 会把空间全给上面那半）⇒
        // 用户没开编辑器时，界面跟以前一模一样（不占地方）。
        let editor_tabs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        editor_tabs.set_margin_start(4);
        editor_tabs.set_margin_end(4);
        editor_tabs.set_margin_top(2);
        editor_tabs.set_margin_bottom(2);
        editor_tabs.add_css_class("pp-editor-tabs");
        let tabs_scroll = gtk::ScrolledWindow::new();
        tabs_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
        tabs_scroll.set_hexpand(true);
        tabs_scroll.set_child(Some(&editor_tabs));
        // 空编辑区时的提示（阶段 108：编辑区也是「停靠区」）
        let hint = gtk::Label::new(Some(tr("把编辑器拖回这里停靠")));
        hint.add_css_class("pp-editor-hint");
        hint.set_visible(false);
        editor_tabs.append(&hint);
        let editor_stack = gtk::Stack::new();
        let editor_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
        editor_area.append(&tabs_scroll);
        editor_area.append(&editor_stack);
        editor_area.set_hexpand(true);
        editor_area.set_vexpand(true);
        editor_area.set_visible(false); // 没有编辑器时不占地方
        editor_area.add_css_class("pp-editor-area");

        // ⚠️ **阶段 108：连接页不再上下分**。
        //
        // 以前是「上半＝子标签区、下半＝编辑区」⇒ 编辑区**横跨整页**，
        // 压在文件列表上方（用户 2026-11-15 报：「文本编辑器和 term 的
        // 宽度一样，就是不要占用文件列表的空间」）。
        //
        // 现在编辑区搬进**终端列**（终端子页里那个竖向 Paned 的下半）⇒
        // **与终端同宽**、**不碰文件列表**。这里只剩子标签区。

        let sub = Rc::new(SubTabs {
            nb,
            root: overlay,
            editor_area,
            editor_tabs,
            editor_stack,
            editor_pages: RefCell::new(Vec::new()),
            editor_current: Cell::new(None),
            editor_host: RefCell::new(None),
            editor_hint: hint,
            pages: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            new_hook: RefCell::new(None),
            add_btn: add.clone(),
            new_pop: RefCell::new(None),
            close_hooks: RefCell::new(Vec::new()),
        });

        // 点击「+」→ 弹菜单（内容由上层给）
        {
            let s = Rc::clone(&sub);
            add.connect_clicked(move |b| {
                let pop = crate::menu::popover(b);
                // 按钮在窗口最右端，默认（向下向右）展开会让菜单**探出窗口**。
                // 往左展开正好落在内容区里。
                pop.set_position(gtk::PositionType::Left);
                let vb = crate::menu::body();
                pop.set_child(Some(&vb));
                let hook = s.new_hook.borrow().clone();
                if let Some(f) = hook {
                    f(&vb, &pop);
                }
                // 记一份（测试钩子）：弹层不进 children 链，外部拿不到
                *s.new_pop.borrow_mut() = Some(pop.clone());
                pop.popup();
            });
        }

        // 阶段 108：页签栏是「把编辑器拖回去」的落点。
        sub.attach_undock_target();
        // 阶段 108：编辑区是「把编辑器拖回来」的落点（**挂在编辑区自己身上**）。
        sub.attach_dock_target();

        sub
    }

    /// 外层容器（notebook + 尾部按钮）——挂到连接页右侧用这个。
    pub fn root(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// 注册「+」按钮的菜单内容（调用方往里塞菜单项）。
    pub fn set_new_hook<F: Fn(&gtk::Box, &gtk::Popover) + 'static>(&self, f: F) {
        *self.new_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 尾部的「+」按钮（**测试钩子**）。
    ///
    /// 冒烟要点**真按钮**（跟用户点的是同一条路径）。
    pub fn new_button(&self) -> &gtk::Button {
        &self.add_btn
    }

    /// 「+」的弹层（**测试钩子**）：上一次点「+」弹出来的那个。
    pub fn new_popover(&self) -> Option<gtk::Popover> {
        self.new_pop.borrow().clone()
    }

    /// 注册"子页被关掉"的回调（传页 id）。
    ///
    /// 终端子页关掉时要把它的 SSH 会话收掉（不然会留下一个野进程）；
    /// 数据库页签关掉时要断开数据库连接。**可以注册多个**。
    pub fn add_close_hook<F: Fn(u64) + 'static>(&self, f: F) {
        self.close_hooks.borrow_mut().push(Rc::new(f));
    }

    /// 加一个子页，返回它的 id。
    ///
    /// `on_close`：关之前先问一遍（返回 false 就不关）。**必须有它**——
    /// 否则用户按 × 就把没保存的修改丢了。
    pub fn add_page(
        self: &Rc<Self>,
        title: &str,
        content: gtk::Widget,
        on_close: Option<Box<dyn Fn() -> bool>>,
    ) -> u64 {
        self.add_page_icon(None, title, content, on_close)
    }

    /// 加一个**带图标**的子页，返回它的 id。
    ///
    /// **为什么要图标**：主标签头上已经不放图标了（用户要求标签头干净），
    /// 于是"这一页是什么"只能靠子页签自己说。图标比文字省地方，也不怕
    /// 中文标题被压成省略号（"数据库"两个字在窄页签里就只剩「数…」）。
    ///
    /// 图标在 `set_title` 改名时不受影响——它是独立的 widget。
    pub fn add_page_icon(
        self: &Rc<Self>,
        icon: Option<&str>,
        title: &str,
        content: gtk::Widget,
        on_close: Option<Box<dyn Fn() -> bool>>,
    ) -> u64 {
        self.add_page_icon_kind(icon, title, PageKind::Other, content, on_close)
    }

    /// 加一个**带种类**的带图标子页，返回它的 id（阶段 66）。
    ///
    /// `kind` 告诉上层"这一页是什么"——DB 页激活时要独占宽度，
    /// 所以上层得能分辨出来（见 [`PageKind`]）。
    pub fn add_page_icon_kind(
        self: &Rc<Self>,
        icon: Option<&str>,
        title: &str,
        kind: PageKind,
        content: gtk::Widget,
        on_close: Option<Box<dyn Fn() -> bool>>,
    ) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        content.set_hexpand(true);
        content.set_vexpand(true);

        // 页签头：[图标][标题][×]，跟主标签栏一个样式
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        head.set_margin_start(4);
        head.set_margin_end(2);
        // 图标：固定 16px，别让它跟着主题缩放把标题挤掉
        if let Some(name) = icon {
            let img = gtk::Image::from_icon_name(name);
            img.set_pixel_size(16);
            img.set_valign(gtk::Align::Center);
            head.append(&img);
        }
        let label = gtk::Label::new(Some(title));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // **必须给最小宽度**：光设 `max_width_chars` 的话，label 的最小宽度
        // 是 0，notebook 分页签时把它压到只剩省略号——三个子标签全变成
        // 「… ×」，用户根本认不出哪个是终端、哪个是数据库。
        // （主标签栏 `tabs.rs` 里踩过同一个坑，用的是 `width_chars(10)`。）
        //
        // ⚠️ **宽度按显示列算，不按字符个数**：中日韩字符在等宽字体里占
        // **两列**，`width_chars(4)` 只够放下两个汉字——「数据库」会被截成
        // 「数…」。所以自己数一遍显示列（CJK 记 2、其余记 1）。
        let w = display_width(title).max(4) as i32;
        label.set_width_chars(w.min(MAX_TITLE_CHARS));
        label.set_max_width_chars(MAX_TITLE_CHARS);
        label.set_tooltip_text(Some(title));
        head.append(&label);
        let close = crate::a11y::icon_button("window-close-symbolic");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        close.set_focusable(false);
        close.set_tooltip_text(Some(tr("关闭这个子标签")));
        head.append(&close);

        self.nb.append_page(&content, Some(&head));
        self.pages.borrow_mut().push(SubPage {
            id,
            title: title.to_string(),
            label,
            widget: content.clone(),
            kind,
            on_close,
        });

        // × → 关页（先问 on_close）
        {
            let me = Rc::clone(self);
            close.connect_clicked(move |_| {
                me.close_page(id);
            });
        }
        id
    }

    /// 关掉某个子页。返回是否真关了。
    ///
    /// **关之前先问 `on_close`**：编辑器有未保存修改时要问「保存并关闭 /
    /// 不保存关闭 / 取消」，用户选取消就不关。
    pub fn close_page(&self, id: u64) -> bool {
        let idx = self.pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return false };
        // 先问（问的时候不能持着 pages 的借用：on_close 可能反过来动页面）
        let ask = {
            let pages = self.pages.borrow();
            pages[idx].on_close.as_ref().map(|f| f())
        };
        if let Some(false) = ask {
            return false;
        }
        self.remove_page(id)
    }

    /// **不询问**直接关掉某个子页。
    ///
    /// 用于"已经问过了"的路径：比如用户选了「保存并关闭」，保存完成后
    /// 再关——这时不能再问一遍。
    pub fn remove_page(&self, id: u64) -> bool {
        let idx = self.pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return false };
        let page = {
            let mut pages = self.pages.borrow_mut();
            pages.remove(idx)
        };
        if let Some(num) = self.nb.page_num(&page.widget) {
            self.nb.remove_page(Some(num));
        }
        // 页面从 notebook 里摘下来**不会**被销毁（GTK4 只是脱离父容器）。
        // 但 `page` 一出作用域，我们手里这个引用就没了——如果它是最后一个
        // 引用，widget 立刻被 finalize（连带它的子控件一起销毁）。
        // gtk4-rs 0.11 的 `Widget` 没有 `destroy()`，销毁就是这样来的。
        drop(page);
        // 通知上层：这个页签没了（终端页签要收掉会话、数据库页签要断开连接）
        let hooks = self.close_hooks.borrow().clone();
        for f in hooks {
            f(id);
        }
        true
    }

    /// 改子页的标题（页签头上显示的字）。
    pub fn set_title(&self, id: u64, title: &str) {
        let pages = self.pages.borrow();
        let Some(p) = pages.iter().find(|p| p.id == id) else {
            return;
        };
        p.label.set_text(title);
        p.label.set_tooltip_text(Some(title));
        // ⚠️ **宽度要跟着新标题走**：建页签时按标题算过 `width_chars`
        // （"数据库" 算出来很窄），后来把标题改成"数据库 · 本地 PG"，
        // label 的宽度还停在旧值 → 标题被压成「数据库 · 本…」。
        let w = display_width(title).max(4) as i32;
        p.label.set_width_chars(w.min(MAX_TITLE_CHARS));
    }

    /// 切到某个子页。
    pub fn switch_to(&self, id: u64) {
        let widget = {
            let pages = self.pages.borrow();
            pages.iter().find(|p| p.id == id).map(|p| p.widget.clone())
        };
        let Some(w) = widget else { return };
        if let Some(num) = self.nb.page_num(&w) {
            self.nb.set_current_page(Some(num));
        }
    }

    /// 当前显示的是哪个子页。
    pub fn current_id(&self) -> Option<u64> {
        let cur = self.nb.current_page()?;
        let w = self.nb.nth_page(Some(cur))?;
        let pages = self.pages.borrow();
        pages.iter().find(|p| p.widget == w).map(|p| p.id)
    }

    /* ---------------- 编辑区（阶段 107 / 108） ---------------- */

    /// 把编辑区挂进**终端列**（阶段 108）。
    ///
    /// 干什么用：编辑器要**与终端同宽**、**不占文件列表的空间** ⇒
    /// 它不能横跨整个连接页，只能待在终端那一列的下面。
    ///
    /// ⚠️ **换终端页签时也要再调一次**（编辑区跟着当前终端页走，跟
    /// 文件面板一个规矩）——不然编辑区会留在上一个终端页里（切过去
    /// 就看不见了）。
    pub fn host_editor_area(self: &Rc<Self>, host: &gtk::Paned) {
        let w = self.editor_area.clone();
        // ⚠️ **已经在那个宿主里了就别动**（阶段 108）。
        //
        // 干什么用：切页时上层会再调一次 `host_editor_area`（编辑区跟着
        // 当前终端页走）⇒ 每次都「摘下来再挂上去」的话，编辑区会在控件
        // 树里闪一下 ⇒ **正在进行的拖放会断**（落点的 crossing 被清掉）。
        if self
            .editor_host
            .borrow()
            .as_ref()
            .map(|h| h == host)
            .unwrap_or(false)
            && w.parent().and_downcast::<gtk::Paned>() == Some(host.clone())
        {
            return;
        }
        // 先从旧宿主摘下来（同一个 widget 不能有两个父级）。
        if let Some(old) = w.parent().and_downcast::<gtk::Paned>() {
            let me = w.upcast_ref::<gtk::Widget>();
            if old.start_child().as_ref() == Some(me) {
                old.set_start_child(None::<&gtk::Widget>);
            } else if old.end_child().as_ref() == Some(me) {
                old.set_end_child(None::<&gtk::Widget>);
            }
        } else if w.parent().is_some() {
            w.unparent();
        }
        host.set_end_child(Some(w.upcast_ref::<gtk::Widget>()));
        *self.editor_host.borrow_mut() = Some(host.clone());
    }

    /// 编辑区控件（**测试钩子**：冒烟要量它的位置/宽度）。
    pub fn editor_area_widget(&self) -> gtk::Widget {
        self.editor_area.clone().upcast()
    }

    /// 编辑器的正文控件（**测试钩子**：冒烟要靠它找页签头）。
    pub fn editor_widget(&self, id: u64) -> Option<gtk::Widget> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.widget.clone())
    }

    /// 编辑器在编辑区小标签栏上的那块标签（**测试钩子**：冒烟要按它的
    /// 位置做一次**真的**拖拽）。
    pub fn editor_tab_widget(&self, id: u64) -> Option<gtk::Widget> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.tab.clone())
            .map(|t| t.upcast())
    }

    /// 加一个编辑器到**编辑区**（不是子页签），返回它的 id。
    ///
    /// 干什么用（用户 2026-11-14）：「终端和文件编辑可以放在同一个窗口中，
    /// 同时显示」⇒ 编辑器住在**终端列的下面**（上面就是终端）
    /// ⇒ 两个同时看得见。
    ///
    /// `on_close`：关之前先问一遍（未保存修改）——返回 false 就不关。
    pub fn add_editor(
        self: &Rc<Self>,
        id: u64,
        icon: Option<&str>,
        title: &str,
        content: gtk::Widget,
        on_close: Option<Box<dyn Fn() -> bool>>,
    ) -> u64 {
        content.set_hexpand(true);
        content.set_vexpand(true);
        self.editor_stack.add_named(&content, Some(&id.to_string()));

        // 标题标签：**每次都新造一个**（不在容器之间搬家 ⇒ 文件名不会丢，
        // 理由见 `make_title_label`）。
        let label = self.make_title_label(title);

        let tab = self.build_editor_tab(id, icon, title, &label);
        self.editor_tabs.append(&tab);
        self.editor_pages.borrow_mut().push(EditorPage {
            id,
            icon: icon.map(|s| s.to_string()),
            title: title.to_string(),
            label,
            tab: Some(tab),
            head: None,
            widget: content,
            on_close,
            docked: true,
        });
        // 有编辑器了 ⇒ 编辑区露出来（Paned 会把空间分给它）。
        self.refresh_editor_area();
        id
    }

    /// 编辑区小标签栏上的一个标签：[图标][文件名][×]（阶段 107）。
    fn build_editor_tab(
        self: &Rc<Self>,
        id: u64,
        icon: Option<&str>,
        title: &str,
        label: &gtk::Label,
    ) -> gtk::Box {
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab.set_margin_start(4);
        tab.set_margin_end(2);
        tab.add_css_class("pp-editor-tab");
        if let Some(name) = icon {
            let img = gtk::Image::from_icon_name(name);
            img.set_pixel_size(16);
            img.set_valign(gtk::Align::Center);
            tab.append(&img);
        }
        // 文件名：点一下就切到这个编辑器。
        //
        // ⚠️ **不能整块做成一个 Button 再塞 ×**：GTK 里按钮不能嵌套按钮
        // （点 × 会连着触发外层）⇒ 名字与 × 分成两个按钮。
        let name_btn = gtk::Button::new();
        name_btn.set_child(Some(label));
        name_btn.add_css_class("flat");
        name_btn.add_css_class("pp-editor-tab-name");
        name_btn.set_tooltip_text(Some(title));
        tab.append(&name_btn);
        let close = crate::a11y::icon_button("window-close-symbolic");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        close.set_focusable(false);
        close.set_tooltip_text(Some(tr("关闭这个文件")));
        tab.append(&close);

        // 阶段 108：标签**能拖**（拖到子标签栏 ⇒ 变回子页签）。
        //
        // ⚠️ **挂在整块标签上，不挂名字按钮**：GTK4 的拖拽源是**捕获阶段**
        // 的手势 ⇒ 挂在**外层**的话，按文字、按图标、按空白都能起拖；
        // 挂在按钮上的话，按钮自己的点击手势会先把序列吃掉。
        self.attach_editor_drag(&tab, id);

        // 点名字 ⇒ 切到那个编辑器。
        //
        // ⚠️ 抓**弱引用**：闭包存在控件里，控件挂在 `self` 里面 ⇒
        // 抓强引用就是引用环（面板永远释放不掉，AGENTS.md 的红线）。
        {
            let me = Rc::downgrade(self);
            name_btn.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.switch_editor(id);
                }
            });
        }
        // 点 × ⇒ 关掉那个编辑器（先问 on_close）。
        {
            let me = Rc::downgrade(self);
            close.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.close_editor(id);
                }
            });
        }
        tab
    }

    /// 编辑器在**子标签栏**里时的页签头：[图标][文件名][×]（阶段 108）。
    ///
    /// 返回 `(页签头, 文件名标签)`——**文件名标签是新造的**（理由见
    /// [`SubTabs::make_title_label`]）。
    fn build_editor_head(
        self: &Rc<Self>,
        id: u64,
        icon: Option<&str>,
        title: &str,
    ) -> (gtk::Box, gtk::Label) {
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        head.set_margin_start(4);
        head.set_margin_end(2);
        if let Some(name) = icon {
            let img = gtk::Image::from_icon_name(name);
            img.set_pixel_size(16);
            img.set_valign(gtk::Align::Center);
            head.append(&img);
        }
        // ⚠️ **新建一个文件名标签**（不是把小标签栏那个搬过来）——
        // 搬运会**丢文件名**，见 `make_title_label` 的说明。
        let label = self.make_title_label(title);
        head.append(&label);
        let close = crate::a11y::icon_button("window-close-symbolic");
        close.add_css_class("flat");
        close.set_valign(gtk::Align::Center);
        close.set_focusable(false);
        close.set_tooltip_text(Some(tr("关闭这个文件")));
        head.append(&close);

        // 阶段 108：整个页签头都能拖（拖回终端列下面 ⇒ 重新停靠）。
        self.attach_editor_drag(&head, id);

        {
            let me = Rc::downgrade(self);
            close.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.close_editor(id);
                }
            });
        }
        (head, label)
    }

    /// 造一个**文件名标签**（编辑区小标签 / 子页签头里显示的那个）。
    ///
    /// ⚠️ **每次都新造一个，绝不在容器之间搬家**（阶段 117，2026-11-15）。
    ///
    /// 干什么用：用户报「编辑器拖到子标签栏后，**页签头上不显示文件名了**」。
    ///
    /// **根因**（GTK 4 的 `gtkbutton.c`）：`GtkButton` 私下记着
    /// `priv->child`。我们原来把 Label 从名字按钮里 `unparent()` 出来、
    /// 搬进页签头 ⇒ 按钮**还记着**那个 Label ⇒ 等旧标签框被销毁时，
    /// 按钮的 dispose 会
    /// `g_clear_pointer (&priv->child, gtk_widget_unparent)`
    /// ⇒ **把已经搬进页签头的 Label 又摘一次** ⇒ 文件名凭空消失
    /// （页签头上只剩图标和「×」）。
    ///
    /// ⇒ 改成「**旧控件随旧容器一起销毁，新控件给新容器**」⇒
    /// 任何时候都没有"控件被两个容器抢"的情况 ⇒ 文件名不会丢。
    fn make_title_label(&self, title: &str) -> gtk::Label {
        let label = gtk::Label::new(Some(title));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let w = display_width(title).max(4) as i32;
        label.set_width_chars(w.min(MAX_TITLE_CHARS));
        label.set_max_width_chars(MAX_TITLE_CHARS);
        label.set_tooltip_text(Some(title));
        label
    }

    /// 给编辑器的小标签 / 页签头挂**拖拽源**（阶段 108）。
    ///
    /// 负载 = `pp-editor:<id>`，落点自己读（见 `read_editor_payload`）。
    fn attach_editor_drag(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>, id: u64) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::MOVE);
        let payload = editor_payload(id);
        src.set_content(Some(&gdk::ContentProvider::for_bytes(
            EDITOR_MIME,
            &glib::Bytes::from(payload.as_bytes()),
        )));
        // ⚠️ **拖动一开始就把「编辑区所在的那一页」切到前台**（阶段 108）。
        //
        // 干什么用：编辑区（停靠区）住在**终端页**里 ⇒ 用户在别的子页签
        // （比如「编辑器自己那一页」）上按住它的页签头拖动时，**落点是
        // 看不见的** ⇒ 拖不回去。
        //
        // ⚠️ **为什么必须在这儿切页**：GTK 的 Notebook **在鼠标按下页签的
        // 那一刻就切页**（`gtknotebook.c: gtk_notebook_switch_focus_tab`
        // → `gtk_notebook_switch_page`）⇒ 用户一按编辑器的页签头，
        // notebook 就切到「编辑器」那一页 ⇒ **编辑器正文盖住整页** ⇒
        // 终端列（连同编辑区）被隐藏 ⇒ 落点没了、拖放管线根本接不上
        // （实测：指针下的控件从编辑区的提示 Label 变成编辑器正文的
        // TextView，接收器连 `accept` 都不触发）。
        //
        // ⇒ 拖动**开始**时切回终端页 ⇒ 编辑区一定在屏幕上 ⇒ 拖得回去。
        {
            let me = Rc::downgrade(self);
            src.connect_drag_begin(move |_src, _drag| {
                if let Some(me) = me.upgrade() {
                    me.show_editor_host_page();
                }
            });
        }
        widget.add_controller(src);
    }

    /// 把**编辑区所在的那一页**切到前台（阶段 108）。
    ///
    /// 编辑区住在终端列（竖向 Paned）里 ⇒ 它的父级就是终端子页的内容
    /// ⇒ 拿这个内容去 notebook 里查页序号。
    ///
    /// 切不到就**静默不动**：界面不崩、不空半个页面。
    fn show_editor_host_page(&self) {
        let Some(host) = self.editor_host.borrow().clone() else {
            return;
        };
        let Some(page_content) = host.parent() else {
            return;
        };
        let Some(num) = self.nb.page_num(&page_content) else {
            return;
        };
        if self.nb.current_page() == Some(num) {
            return; // 已经是这一页了
        }
        self.nb.set_current_page(Some(num));
    }

    /// 切到某个编辑器（停靠的切内容栈；拖回子标签栏的切页签）。
    pub fn switch_editor(self: &Rc<Self>, id: u64) {
        let (docked, widget) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return;
            };
            (p.docked, p.widget.clone())
        };
        if docked {
            self.editor_stack.set_visible_child_name(&id.to_string());
            self.editor_current.set(Some(id));
            // 「当前」那个标签加个高亮类（别的去掉）。
            for p in self.editor_pages.borrow().iter() {
                if let Some(t) = &p.tab {
                    if p.id == id {
                        t.add_css_class("pp-editor-tab-active");
                    } else {
                        t.remove_css_class("pp-editor-tab-active");
                    }
                }
            }
        } else {
            // 已经拖回子标签栏了 ⇒ 切到那个页签。
            if let Some(num) = self.nb.page_num(&widget) {
                self.nb.set_current_page(Some(num));
            }
            self.editor_current.set(Some(id));
        }
    }

    /// 关掉某个编辑器（**先问 `on_close`**）。返回是否真关了。
    pub fn close_editor(self: &Rc<Self>, id: u64) -> bool {
        let idx = self.editor_pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return false };
        // 先问（问的时候不能持着借用：on_close 可能反过来动页面）。
        let ask = {
            let pages = self.editor_pages.borrow();
            pages[idx].on_close.as_ref().map(|f| f())
        };
        if let Some(false) = ask {
            return false;
        }
        self.remove_editor(id)
    }

    /// **不询问**直接关掉某个编辑器（**两种位置都能摘**：编辑区 / 子标签栏）。
    pub fn remove_editor(self: &Rc<Self>, id: u64) -> bool {
        let idx = self.editor_pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return false };
        let (docked, widget, tab) = {
            let pages = self.editor_pages.borrow();
            (
                pages[idx].docked,
                pages[idx].widget.clone(),
                pages[idx].tab.clone(),
            )
        };
        if docked {
            self.editor_stack.remove(&widget);
            if let Some(t) = &tab {
                self.editor_tabs.remove(t);
            }
        } else {
            // 拖回子标签栏的那个 ⇒ 从 notebook 里摘掉那一页。
            if let Some(num) = self.nb.page_num(&widget) {
                self.nb.remove_page(Some(num));
            }
        }
        // 摘登记（顺带把那一块标签控件的引用放掉）。
        {
            let mut pages = self.editor_pages.borrow_mut();
            pages.remove(idx);
        }
        // 关掉的是**当前**那个 ⇒ 切到还剩着的最后一个（没有就清空）。
        if self.editor_current.get() == Some(id) {
            let next = self
                .editor_pages
                .borrow()
                .iter()
                .filter(|p| p.docked)
                .last()
                .map(|p| p.id);
            match next {
                Some(n) => self.switch_editor(n),
                None => self.editor_current.set(None),
            }
        }
        self.refresh_editor_area();
        true
    }

    /// 把编辑器**拖回子标签栏**（阶段 108：变回一个子页签）。
    ///
    /// 干什么用：用户 2026-11-15：「我希望也可以拖动它的 tab，把它拖动到
    /// 原来修改之前的位置」——阶段 107 之前编辑器就是子页签 ⇒ 拖回那儿
    /// 就是"回到老地方"。
    ///
    /// 返回是否真拖回去了（本来就在编辑区 ⇒ false）。
    pub fn undock_editor(self: &Rc<Self>, id: u64) -> bool {
        let (icon, title, widget) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return false;
            };
            if !p.docked {
                return false; // 已经在子标签栏里了
            }
            (
                p.icon.clone(),
                p.title.clone(),
                p.widget.clone(),
            )
        };
        // 从编辑区摘掉（内容 + 小标签）。
        self.editor_stack.remove(&widget);
        let tab = {
            let pages = self.editor_pages.borrow();
            pages.iter().find(|p| p.id == id).and_then(|p| p.tab.clone())
        };
        if let Some(t) = &tab {
            self.editor_tabs.remove(t);
        }
        // 变成一个子页签（**页签头是新造的** ⇒ 文件名一定在）。
        let (head, label) = self.build_editor_head(id, icon.as_deref(), &title);
        self.nb.append_page(&widget, Some(&head));
        {
            let mut pages = self.editor_pages.borrow_mut();
            if let Some(p) = pages.iter_mut().find(|p| p.id == id) {
                p.docked = false;
                p.head = Some(head);
                p.tab = None;
                p.label = label;
            }
        }
        // 编辑区可能空了 ⇒ 变成一条「停靠区」（用户还能把编辑器拖回来）。
        self.refresh_editor_area();
        // ⚠️ **拖出去后切到「编辑器」那一页**（阶段 117，用户 2026-11-15：
        // 「拖动后就只能看到一个，**应该是显示编辑器**，因为鼠标肯定点的是
        // 编辑器」）。
        //
        // ⚠️ 阶段 108 当初**故意不切**（怕终端页被藏起来 ⇒ 停靠区看不见 ⇒
        // 拖不回去）。现在看：**页签头一直在子标签栏上** ⇒ 想拖回去时从
        // 页签头拖走即可（拖动开始时 `show_editor_host_page` 会把终端页
        // 切回来 ⇒ 落点重新出现）⇒ 切页不影响「拖回去」。
        self.switch_editor(id);
        // 「当前编辑器」指针留给**还停靠着的**那个（拖走的那个不在编辑区里）。
        if self.editor_current.get() == Some(id) {
            let next = self
                .editor_pages
                .borrow()
                .iter()
                .filter(|p| p.docked)
                .last()
                .map(|p| p.id);
            self.editor_current.set(next);
        }
        true
    }

    /// 把编辑器**停靠**回编辑区（阶段 108：从子标签栏拖回来）。
    ///
    /// 返回是否真停回去了（本来就在编辑区 ⇒ false）。
    pub fn dock_editor(self: &Rc<Self>, id: u64) -> bool {
        let (icon, title, widget) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return false;
            };
            if p.docked {
                return false; // 已经在编辑区里了
            }
            (
                p.icon.clone(),
                p.title.clone(),
                p.widget.clone(),
            )
        };
        // 从 notebook 里摘掉那一页。
        if let Some(num) = self.nb.page_num(&widget) {
            self.nb.remove_page(Some(num));
        }
        // 小标签：**新造一个**（不把页签头那个搬回来 ⇒ 文件名不会丢）。
        let label = self.make_title_label(&title);
        let tab = self.build_editor_tab(id, icon.as_deref(), &title, &label);
        self.editor_stack.add_named(&widget, Some(&id.to_string()));
        self.editor_tabs.append(&tab);
        {
            let mut pages = self.editor_pages.borrow_mut();
            if let Some(p) = pages.iter_mut().find(|p| p.id == id) {
                p.docked = true;
                p.tab = Some(tab);
                p.head = None;
                p.label = label;
            }
        }
        self.refresh_editor_area();
        self.switch_editor(id);
        true
    }

    /// 这个编辑器现在是停在**编辑区**还是**子标签栏**（阶段 108）。
    pub fn editor_is_docked(&self, id: u64) -> Option<bool> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.docked)
    }

    /// 当前显示的编辑器 id。
    pub fn current_editor(&self) -> Option<u64> {
        // 当前子页签是不是一个「拖回子标签栏」的编辑器？
        if let Some(cur) = self.nb.current_page() {
            if let Some(w) = self.nb.nth_page(Some(cur)) {
                let pages = self.editor_pages.borrow();
                if let Some(p) = pages.iter().find(|p| !p.docked && p.widget == w) {
                    return Some(p.id);
                }
            }
        }
        // 否则是编辑区里"当前"那个（**必须是还停靠着的**——拖走的那个
        // 已经不在编辑区了，指着它会让快捷键找错人）。
        let cur = self.editor_current.get();
        let pages = self.editor_pages.borrow();
        cur.filter(|id| pages.iter().any(|p| p.id == *id && p.docked))
    }

    /// 一共开着几个编辑器（**两种位置都算**：编辑区 + 子标签栏）。
    pub fn editor_count(&self) -> usize {
        self.editor_pages.borrow().len()
    }

    /// 编辑区现在是不是**露在外面**（有停靠的编辑器且可见）。
    ///
    /// 冒烟用：用户开着编辑器却看不见编辑区 = 功能等于没有。
    pub fn editor_area_visible(&self) -> bool {
        self.editor_area.is_visible()
    }

    /// 改编辑器的标题（**两种位置都改**：小标签栏 / 子页签头）。
    pub fn set_editor_title(&self, id: u64, title: &str) {
        let mut pages = self.editor_pages.borrow_mut();
        let Some(p) = pages.iter_mut().find(|p| p.id == id) else {
            return;
        };
        p.title = title.to_string();
        p.label.set_text(title);
        p.label.set_tooltip_text(Some(title));
        let w = display_width(title).max(4) as i32;
        p.label.set_width_chars(w.min(MAX_TITLE_CHARS));
    }

    /// 编辑区露不露出来、留多高（阶段 108）。
    ///
    /// ⚠️ **有编辑器但都被拖到子标签栏去了 ⇒ 编辑区仍然留着**，
    /// 但只留**一条小标签栏**（当「停靠区」用）——不然用户把编辑器拖
    /// 出去之后就**没地方拖回来**了（编辑区藏起来 = 没有落点）。
    fn refresh_editor_area(&self) {
        let (any, docked) = {
            let pages = self.editor_pages.borrow();
            (!pages.is_empty(), pages.iter().any(|p| p.docked))
        };
        self.editor_area.set_visible(any);
        self.editor_hint.set_visible(any && !docked);
        if any {
            self.layout_editor_area(docked);
        }
    }

    /// 编辑区露出来时，给它一个合适的高度（终端别被挤没）。
    fn layout_editor_area(&self, docked: bool) {
        let Some(col) = self.editor_host.borrow().clone() else {
            return;
        };
        let h = col.height();
        if h <= 0 {
            return; // 还没量出高度 ⇒ 等下一次再说
        }
        let want = if docked {
            // 编辑区要终端列的 40%，最少 120px、最多 360px。
            (((h as f64) * 0.4) as i32).clamp(120, 360)
        } else {
            // 编辑器都被拖走了 ⇒ 只留一条小标签栏（停靠区）
            34
        };
        col.set_position((h - want).max(0));
    }

    /// 编辑区是「把编辑器拖回来」的落点（阶段 108）。
    ///
    /// ⚠️ **挂在编辑区自己身上**（不是它的祖先）。
    ///
    /// ⚠️ **别被一个假结论带偏**（2026-11-15 踩过）：当时以为「GTK4 的落点
    /// 靠 crossing 事件 ⇒ 挂在祖先上的接收器收不到」⇒ 一度把接收器从编辑区
    /// 挪到终端列、又挪回来。**查了 GTK 源码（4.25）：这个说法是错的**——
    /// `gtk_synthesize_crossing_events()` 是**从 root 往下**把 IN crossing
    /// 逐个派发给「指针下的控件**及其所有祖先**」⇒ **祖先照样收得到**。
    ///
    /// ⚠️ **真正的坑是「落点压根不在这个控件上」**：GTK 的 Notebook
    /// **在鼠标按下页签的那一刻就切页**（`gtknotebook.c`:
    /// `gtk_notebook_switch_focus_tab` → `gtk_notebook_switch_page`）⇒
    /// 按住编辑器的页签头一拖，notebook 切到「编辑器」那一页 ⇒ 编辑器正文
    /// 盖住整页 ⇒ **终端列（连同编辑区）被隐藏** ⇒ 指针下的控件变成了编辑器
    /// 正文（TextView）⇒ 接收器连 `accept` 都不触发 ⇒ 拖放管线接不上。
    /// ⇒ 真正的解法是 `attach_editor_drag` 里的 **drag-begin 切回终端页**。
    fn attach_dock_target(self: &Rc<Self>) {
        let dst = gtk::DropTargetAsync::new(
            Some(gdk::ContentFormats::new(&[EDITOR_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(EDITOR_MIME));
        dst.connect_drag_enter(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        dst.connect_drag_motion(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, _x, _y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let cb: Rc<dyn Fn(u64)> = Rc::new(move |id| {
                me.dock_editor(id);
            });
            read_editor_payload(&drop, cb);
            true
        });
        self.editor_area.add_controller(dst);
    }

    /// 页签栏（把编辑器拖回去）的拖放接收器（阶段 108）。
    ///
    /// ⚠️ GTK4 **没有公开** notebook 页签栏的 API ⇒ 只能把接收器挂在
    /// notebook 上，再用「落点 y < 页签栏高度」判断是不是拖到了页签栏。
    fn attach_undock_target(self: &Rc<Self>) {
        let dst = gtk::DropTargetAsync::new(
            Some(gdk::ContentFormats::new(&[EDITOR_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(EDITOR_MIME));
        let nb = self.nb.clone();
        dst.connect_drag_enter({
            let nb = nb.clone();
            move |_d, _drop, x, y| {
                if y < tab_strip_height(&nb) as f64 {
                    gdk::DragAction::MOVE
                } else {
                    gdk::DragAction::empty()
                }
            }
        });
        dst.connect_drag_motion(move |_d, _drop, _x, y| {
            if y < tab_strip_height(&nb) as f64 {
                gdk::DragAction::MOVE
            } else {
                gdk::DragAction::empty()
            }
        });
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, _x, _y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let cb: Rc<dyn Fn(u64)> = Rc::new(move |id| {
                me.undock_editor(id);
            });
            read_editor_payload(&drop, cb);
            true
        });
        self.nb.add_controller(dst);
    }

    /// 子页标题（页签头用）。
    pub fn title_of(&self, id: u64) -> Option<String> {
        let pages = self.pages.borrow();
        pages.iter().find(|p| p.id == id).map(|p| p.title.clone())
    }

    /// 某个子页是**什么**（阶段 66）。
    pub fn kind_of(&self, id: u64) -> PageKind {
        let pages = self.pages.borrow();
        pages
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.kind)
            .unwrap_or_default()
    }

    /// **当前**显示的这一页是**什么**（阶段 66）。
    pub fn current_kind(&self) -> PageKind {
        match self.current_id() {
            Some(id) => self.kind_of(id),
            None => PageKind::Other,
        }
    }

    /// 按**页序号**（notebook 的 page num）找子页的种类（阶段 66）。
    ///
    /// **为什么需要这个**：`switch-page` 信号是在**切换之前**发的 ⇒
    /// 那时 `current_kind()` 还是**旧页**的 ⇒ 得按传进来的页序号查。
    pub fn kind_by_page_num(&self, num: u32) -> PageKind {
        for p in self.pages.borrow().iter() {
            if let Some(n) = self.nb.page_num(&p.widget) {
                if n == num {
                    return p.kind;
                }
            }
        }
        PageKind::Other
    }

    /// 按**页序号**取这一页的**宿主 Paned**（阶段 97）。
    ///
    /// 终端子页的内容**就是**那个「文件面板｜终端」的 Paned ⇒ 上层要把
    /// 文件面板搬到某个终端页签时，直接拿它当宿主，不用再记一份映射。
    ///
    /// 不是 Paned 的页（数据库工作台 / 文件编辑器）⇒ `None`。
    pub fn paned_by_page_num(&self, num: u32) -> Option<gtk::Paned> {
        for p in self.pages.borrow().iter() {
            if let Some(n) = self.nb.page_num(&p.widget) {
                if n == num {
                    return p.widget.clone().downcast::<gtk::Paned>().ok();
                }
            }
        }
        None
    }

    /// 有没有这个子页。
    pub fn has(&self, id: u64) -> bool {
        self.pages.borrow().iter().any(|p| p.id == id)
    }

    /// 按**标题**找子页 id（没有则 None）。
    ///
    /// 「终端」页签是可以被用户关掉的（子标签上的 ×），关掉之后
    /// 「重新连接终端」要靠它知道该重建哪一页。
    pub fn id_by_title(&self, title: &str) -> Option<u64> {
        self.pages
            .borrow()
            .iter()
            .find(|p| p.title == title)
            .map(|p| p.id)
    }

    /// 当前有几个子页（测试用）。
    ///
    /// ⚠️ **包括「拖回子标签栏」的编辑器**（阶段 108）——它在页签栏上占
    /// 一个位置 ⇒ 数页签时该算它。
    pub fn page_count(&self) -> usize {
        let normal = self.pages.borrow().len();
        let undocked = self
            .editor_pages
            .borrow()
            .iter()
            .filter(|p| !p.docked)
            .count();
        normal + undocked
    }
}

/* ---------------- 阶段 108 的拖放辅助 ---------------- */

/// 找 notebook 的**页签栏**（装页签头那个盒子）。
///
/// GTK4 **没有公开**这个控件（`gtk_notebook_get_tab_area` 是 GTK3 的，
/// GTK4 连 C API 都没导出）⇒ 只能从控件树里找：notebook 的第一个孩子
/// （一个 `GtkBox`）就是它（实测 GTK 4.18：notebook → [GtkBox(页签栏),
/// GtkStack(内容)]）。
///
/// 找不到就返回 `None` ⇒ 调用方退回「按 y 判断」。
fn find_tab_strip(nb: &gtk::Notebook) -> Option<gtk::Widget> {
    fn walk(w: &gtk::Widget, depth: u32) -> Option<gtk::Widget> {
        if depth > 3 {
            return None;
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            let name = x.type_().name();
            if name == "GtkBox" {
                return Some(x);
            }
            if name == "GtkScrolledWindow" || name == "GtkViewport" {
                if let Some(f) = walk(&x, depth + 1) {
                    return Some(f);
                }
            }
            c = x.next_sibling();
        }
        None
    }
    walk(nb.upcast_ref::<gtk::Widget>(), 0)
}

/// notebook 页签栏有多高（像素）。
///
/// ⚠️ **量不到就按 40px 算**（页签栏本来就那么高）。
fn tab_strip_height(nb: &gtk::Notebook) -> i32 {
    find_tab_strip(nb)
        .map(|w| w.height())
        .filter(|h| *h > 0)
        .unwrap_or(40)
}

/// 读拖放负载（编辑器 id）⇒ 回调（阶段 108）。
///
/// ⚠️ **读完必须 `finish()`**：不 finish 的话源端一直以为这次拖拽没结束
/// ⇒ 下一次拖拽可能被忽略（`dnd.rs` 里记过这个坑）。
///
/// ⚠️ **要有超时兜底**：源端不应答时不能永远挂着（用户看到的就是
/// "拖进去完全没反应"）。
fn read_editor_payload(drop: &gdk::Drop, cb: Rc<dyn Fn(u64)>) {
    let drop_read = drop.clone();
    let drop_finish = drop.clone();
    // 「读完没有」：读回调与超时回调**谁先到谁算数**。
    let done = Rc::new(Cell::new(false));
    {
        let done = Rc::clone(&done);
        let drop_finish = drop_finish.clone();
        glib::timeout_add_local_once(
            Duration::from_secs(crate::panel::dnd::READ_TIMEOUT_SECS),
            move || {
                if done.replace(true) {
                    return; // 已经读到了
                }
                drop_finish.finish(gdk::DragAction::empty());
            },
        );
    }
    drop_read.read_async(
        &[EDITOR_MIME],
        glib::Priority::DEFAULT,
        None::<&gtk::gio::Cancellable>,
        move |r| {
            let Ok((stream, _mime)) = r else {
                if !done.replace(true) {
                    drop_finish.finish(gdk::DragAction::empty());
                }
                return;
            };
            crate::panel::dnd::slurp(&stream, move |text| {
                if done.replace(true) {
                    return;
                }
                match parse_editor_payload(&text) {
                    Some(id) => {
                        cb(id);
                        drop_finish.finish(gdk::DragAction::MOVE);
                    }
                    None => drop_finish.finish(gdk::DragAction::empty()),
                }
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{display_width, parse_editor_payload, MAX_TITLE_CHARS};

    #[test]
    fn 拖放负载能解析出编辑器id() {
        // 阶段 108：编辑器标签的拖放负载。
        assert_eq!(parse_editor_payload("pp-editor:12"), Some(12));
        assert_eq!(parse_editor_payload("pp-editor:0"), Some(0));
        // 前后空白不算错（拖放数据偶尔带换行）
        assert_eq!(parse_editor_payload(" pp-editor:7\n"), Some(7));
        // 不是这个格式 ⇒ None
        assert_eq!(parse_editor_payload(""), None);
        assert_eq!(parse_editor_payload("/srv/a.txt"), None);
        assert_eq!(parse_editor_payload("pp-editor:"), None);
        assert_eq!(parse_editor_payload("pp-editor:abc"), None);
        // 前缀对但后面是别的 ⇒ 不能瞎解析
        assert_eq!(parse_editor_payload("editor:12"), None);
    }

    #[test]
    fn 汉字按两列算() {
        assert_eq!(display_width("终端"), 4);
        assert_eq!(display_width("数据库"), 6);
        assert_eq!(display_width("编辑"), 4);
        // 混排：汉字两列、英文一列（破折号按一列算）
        assert_eq!(display_width("编辑 — a.sh"), 4 + 1 + 1 + 1 + 4);
    }

    #[test]
    fn 纯英文按一列算() {
        assert_eq!(display_width("terminal"), 8);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn 最小宽度不超过上限() {
        // 超长标题的最小宽度不能超过上限，否则宽度上限形同虚设
        let long = "x".repeat(100);
        let w = display_width(&long).max(4) as i32;
        assert!(w.min(MAX_TITLE_CHARS) <= MAX_TITLE_CHARS);
    }
}
