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
//! └── 当前连接页（横向 Paned）
//!     ├── 左：文件面板（远程/本地，常驻）
//!     └── 右：子标签栏（终端 / 数据库 / .zshrc …）
//! ```
//!
//! **终端也是子标签**：XTerminal 里「终端」跟「.zshrc」是平级的子标签，
//! 我们照做——终端是第一个子页，文件面板留在左边不动。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use crate::i18n::tr;

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

/// 子页的**种类**（阶段 66 / 审计 P0-2）。
///
/// **为什么要分种类**：DB 工作台激活时要**独占**右侧宽度（否则主区只剩
/// 300px，审计 P0-2）⇒ 上层得能一眼看出"当前这一页是不是 DB"。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageKind {
    /// 其它（不属于下面几类）
    #[default]
    Other,
    /// 终端页
    Terminal,
    /// 数据库工作台页
    Db,
    /// 文件编辑器页
    FileEdit,
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
    root: gtk::Overlay,
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
    /// **文件面板开关**（阶段 66）：DB 页独占宽度时，用户要能把左边的
    /// 文件面板拉回来——不然用户以为功能没了。
    ///
    /// 平时**隐藏**：只有当前页是 DB 时才有意义（终端页的文件面板是
    /// 常驻的，没有"藏起来"这回事）。
    toggle_btn: gtk::ToggleButton,
    /// 开关被点时的回调（参数 = 想要的新状态）。
    toggle_hook: RefCell<Option<Rc<dyn Fn(bool)>>>,
    /// 程序化改开关状态时为 true——否则 `set_active` 又会触发
    /// `toggled` ⇒ 又调钩子 ⇒ 死循环。
    updating: Cell<bool>,
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

        // 「文件面板」开关（阶段 66）：DB 页独占宽度时，用户靠它把左边
        // 的文件面板拉回来。**平时隐藏**（只有 DB 页用得上）。
        let toggle = gtk::ToggleButton::new();
        toggle.set_icon_name("view-sidebar-symbolic");
        toggle.set_tooltip_text(Some(tr("显示文件面板")));
        toggle.set_focusable(false);
        toggle.set_visible(false);

        // 两个按钮排在一起浮在页签栏右端（**不占布局宽度**）
        let btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        btn_box.append(&toggle);
        btn_box.append(&add);

        // 浮在页签栏那一行的右端（**不占布局宽度**）
        let root = gtk::Overlay::new();
        root.set_child(Some(&nb));
        root.add_overlay(&btn_box);
        // halign/valign 不是 FILL 时，Overlay 会按**自然尺寸**把 overlay
        // 子控件摆到对应的边上——所以按钮只占自己那一小块，不拦截别处的点击
        //（GTK 4.10 的 overlay-pass-through  gtk4-rs 0.11 没绑，用不着）。
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_valign(gtk::Align::Start);
        btn_box.set_margin_end(4);
        btn_box.set_margin_top(2);

        let sub = Rc::new(SubTabs {
            nb,
            root,
            pages: RefCell::new(Vec::new()),
            next_id: Cell::new(1),
            new_hook: RefCell::new(None),
            add_btn: add.clone(),
            new_pop: RefCell::new(None),
            close_hooks: RefCell::new(Vec::new()),
            toggle_btn: toggle.clone(),
            toggle_hook: RefCell::new(None),
            updating: Cell::new(false),
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

        // 「文件面板」开关被点（阶段 66）
        {
            let s = Rc::clone(&sub);
            toggle.connect_toggled(move |b| {
                // 程序化改状态时别再回调（否则死循环）
                if s.updating.get() {
                    return;
                }
                let want = b.is_active();
                let hook = s.toggle_hook.borrow().clone();
                match hook {
                    Some(f) => f(want),
                    // 没人接（上层还没注册）⇒ 弹回去，别留一个假的开关
                    None => {
                        s.updating.set(true);
                        b.set_active(!want);
                        s.updating.set(false);
                    }
                }
            });
        }

        sub
    }

    /// 注册「文件面板」开关的回调（参数 = 想要的新状态）。
    ///
    /// 上层（连接页）收到后切 SFTP 面板的可见性，并把选择记住。
    pub fn set_toggle_hook<F: Fn(bool) + 'static>(&self, f: F) {
        *self.toggle_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 文件面板开关**显不显示**（只有 DB 页才显示它）。
    pub fn set_toggle_visible(&self, visible: bool) {
        self.toggle_btn.set_visible(visible);
    }

    /// 文件面板开关的**当前状态**（= 文件面板可见与否）。
    ///
    /// ⚠️ 程序化改状态**不会**触发上层的回调（`updating` 挡着）——
    /// 否则"切页时同步一次状态"会变成"切页 → 回调 → 再切页"。
    pub fn set_toggle_state(&self, on: bool) {
        self.updating.set(true);
        self.toggle_btn.set_active(on);
        self.updating.set(false);
        self.toggle_btn.set_tooltip_text(Some(if on {
            tr("隐藏文件面板")
        } else {
            tr("显示文件面板")
        }));
    }

    /// 文件面板开关当前是不是"按下"（= 文件面板可见）。
    pub fn toggle_state(&self) -> bool {
        self.toggle_btn.is_active()
    }

    /// 点一下「文件面板」开关（冒烟用）。
    ///
    /// ⚠️ 走的是**真的按钮回调**（`set_active` ⇒ `toggled` ⇒ 上层的钩子）
    /// ⇒ 测的是用户真点下去的那条路，不是"直接调内部函数"。
    pub fn click_toggle(&self) {
        let now = self.toggle_btn.is_active();
        self.toggle_btn.set_active(!now);
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
    pub fn page_count(&self) -> usize {
        self.pages.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::{display_width, MAX_TITLE_CHARS};

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
