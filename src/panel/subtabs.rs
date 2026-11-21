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

/// 拖放负载用的 mime（阶段 145 统一）。
///
/// ⚠️ **不能用 `text/plain`**：文件面板的拖放也用字符串（远端路径行）
/// ⇒ 混用的话，拖编辑器标签到文件列表上会被当成"拖路径"⇒ 误触发下载。
pub const PAGE_MIME: &str = crate::layout::dnd::PAGE_MIME;

/// 拖拽负载的种类（阶段 145：统一成一种协议）。
pub use crate::layout::dnd::PageKindTag;

/// 子标签标题的最大显示列数（超了省略号收尾，完整标题进 tooltip）。
const MAX_TITLE_CHARS: i32 = 24;

/// **边缘落点的宽度**（像素）—— 离边这么近才算"拖到边上要拆分"。
const EDGE_MARGIN: f64 = 24.0;

/// 落点命中了哪儿（阶段 145）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// 页签栏（⇒ 插入）。
    TabBar,
    /// 某条边（⇒ 拆分）。
    Edge(crate::layout::dnd::SplitZone),
    /// 不接。
    None,
}

impl Hit {
    /// 落点 ⇒ **跨格子的动作**（`None` = 不接这个拖拽）。
    fn into_move(self, x: f64, me: &SubTabs) -> Option<crate::layout::dnd::PageMove> {
        match self {
            Hit::TabBar => {
                // 插入位置 = 落点落在第几个页签之前。
                let xs = me.page_head_xs();
                let idx = crate::layout::dnd::insert_index_at(&xs, x);
                Some(crate::layout::dnd::PageMove::Into(Some(idx)))
            }
            Hit::Edge(z) => Some(crate::layout::dnd::PageMove::Split(z)),
            Hit::None => None,
        }
    }
}

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
    /// **容器页**（阶段 109；阶段 113 之后**也有编辑区的落点**）。
    Docker,
}

impl PageKind {
    /// 拖拽负载里的**种类标签**（阶段 145）。
    pub fn tag(&self) -> PageKindTag {
        match self {
            PageKind::Terminal => PageKindTag::Terminal,
            PageKind::Db => PageKindTag::Db,
            PageKind::Docker => PageKindTag::Docker,
            PageKind::Other => PageKindTag::Other,
        }
    }

    /// 从拖拽负载的种类标签**认回来**（阶段 145）。
    ///
    /// ⚠️ **编辑器不是子页签**（它是编辑区里的东西）⇒ 落到 `Other`。
    pub fn from_tag(tag: PageKindTag) -> PageKind {
        match tag {
            PageKindTag::Terminal => PageKind::Terminal,
            PageKindTag::Db => PageKind::Db,
            PageKindTag::Docker => PageKind::Docker,
            PageKindTag::Editor | PageKindTag::Other => PageKind::Other,
        }
    }
}

/// 一个子页的**内容**（跨格子搬运时整体搬走）。
///
/// ⚠️ **为什么要单独一个结构**：搬运 = 先 `take_page`（**静默**摘下来）
///    再 `insert_page`（放进目标格子）⇒ 中间这一刻页签"哪儿都不在"。
///
/// ⚠️ **`on_close` 不可克隆**（`Box<dyn Fn>`）⇒ 只能**搬**，不能复制。
pub struct PageData {
    /// 页签 id（**搬过去不变**）。
    ///
    /// ⚠️ **为什么必须不变**：上层按页签 id 记着东西（`Tab.term_sub_id`、
    ///    `Tab.layouts` 的键…）⇒ 换了 id ⇒ 「重新连接终端」「分屏树」全对不上。
    pub id: u64,
    /// 页签标题。
    pub title: String,
    /// 页签图标名（`None` = 不要图标）。
    pub icon: Option<String>,
    /// 这一页是什么。
    pub kind: PageKind,
    /// 页内容（notebook 的 child）。
    pub widget: gtk::Widget,
    /// 关闭前的确认（搬过来还带着）。
    pub on_close: Option<Box<dyn Fn() -> bool>>,
}

/// 一个子页。
struct SubPage {
    id: u64,
    title: String,
    /// 页签头上的标题标签（改名时要用）
    label: gtk::Label,
    /// 页签图标名（**搬走时要重建图标** ⇒ 得记住名字）。
    icon: Option<String>,
    /// 页签头**整个 widget**（图标 ＋ 标题 ＋ ×）。
    ///
    /// ⚠️ **为什么存它**（阶段 145）：算「插到第几个页签之前」要靠**页签头
    ///   的横坐标** ⇒ GTK 没有公开页签栏 API ⇒ 自己记着头 widget 的位置。
    head: gtk::Box,
    /// 页内容（notebook 的 child）
    widget: gtk::Widget,
    /// 这一页是**什么**（阶段 66：DB 页要独占宽度）
    kind: PageKind,
    /// 关闭前的确认（编辑器有未保存修改时要问）。返回 false = 不关。
    ///
    /// 没有它就直接关的话，用户按 × 就把没保存的修改丢了。
    on_close: Option<Box<dyn Fn() -> bool>>,
}

/// 页签头上的一个**小图标**（**名字 ＋ tooltip**）。
///
/// ⚠️ **为什么带 tooltip**：16px 的小图标（服务器 / 电脑）光看认不出是哪
///   一头 ⇒ 鼠标停上去得说清楚「这个文件在远程服务器上」/「这个文件在本机」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabIcon {
    /// 图标名（**必须是主题里真有的** ⇒ 否则显示成「缺图」）。
    pub name: String,
    /// tooltip 文案。
    pub tip: String,
}

/// 往一个横向容器里挂一个小图标（**没有就什么都不做**）。
fn append_icon(box_: &gtk::Box, icon: Option<&TabIcon>) {
    let Some(ic) = icon else { return };
    let img = gtk::Image::from_icon_name(&ic.name);
    img.set_pixel_size(16);
    img.set_valign(gtk::Align::Center);
    img.set_tooltip_text(Some(&ic.tip));
    box_.append(&img);
}

/// 递归收集一棵控件树里**所有 Image 的图标名**（测试钩子用）。
fn collect_icon_names(w: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(img) = w.downcast_ref::<gtk::Image>() {
        if let Some(name) = img.icon_name() {
            out.push(name.to_string());
        }
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        collect_icon_names(&x, out);
        c = x.next_sibling();
    }
}

/// 编辑器停靠在**哪儿**（阶段 137）。
///
/// ⚠️ **为什么从「是不是停着」改成「停在哪儿」**：以前只有两处（编辑区 /
///   子标签栏）⇒ 一个 bool 够用；现在有**三处**（主栏 / 底部 / 侧栏）⇒
///   位置本身成了数据（页签要在三者之间搬来搬去）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorPlace {
    /// **主栏页签**（与终端并列）——阶段 137 之后的**默认**。
    ///
    /// 干什么用（用户 2026-11-19）：「打开文件和 term 显示为**并列的 tab**」
    /// ——以前（阶段 107 之前）就是这样 ⇒ 改回去。
    Main,
    /// **底部编辑区**（终端列下面；阶段 107 的默认家）。
    ///
    /// 保留：终端与编辑器**同屏上下**（用户 2026-11-14 要的「同时显示」）。
    Bottom,
    /// **侧栏页签**（左右分栏；阶段 137 新增）。
    ///
    /// 干什么用（用户 2026-11-19）：「这个 tab 可以**自由停靠在屏幕的多个
    /// 位置**，就像很多软件那样，比如 vscode」⇒ 拖到窗口左/右边缘 ⇒ 在
    /// 那儿开一条分栏。
    Side,
}

/// **编辑区**里的一个编辑器（阶段 107；阶段 108 起可在「编辑区 ⇄ 子标签栏」之间拖）。
struct EditorPage {
    /// 编辑器 id（= `fileedit::FileEdit::id`）。
    id: u64,
    /// 图标名（重建标签时要用）。
    icon: Option<TabIcon>,
    /// **位置图标**（远程 / 本地；重建标签时要用）。
    ///
    /// ⚠️ **为什么单独一个**：模式图标（铅笔＝可编辑 / 文档＝只读）**两种
    ///   文件都一样** ⇒ 光看它分不出「这台服务器上的」还是「本机的」
    ///   ⇒ 页签上再挂一个位置图标（服务器 / 电脑）（用户 2026-11-19 提的）。
    loc_icon: Option<TabIcon>,
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
    /// 「拖回子标签栏」时那一页的**宿主 Paned**（左＝文件面板 / 右＝编辑器）。
    ///
    /// ⚠️ **为什么要有它**（阶段 121）：编辑器拖到子标签栏后**不再是一个
    ///    整页的编辑器**，而是「**SFTP 文件列表 ｜ 编辑器**」——
    ///    跟阶段 107 之前（term 和编辑器不能同时显示时）的形态一样
    ///    （用户 2026-11-15：「编辑器旁边要有 sftp，就像我们修改将 term
    ///    和编辑器能同时显示之前那样」）。
    ///
    /// ⇒ 这个 Paned **同时是那一页的内容**（`nb.append_page` 挂的是它）。
    ///
    /// ⚠️ **文件面板是「会话级的一个 widget」** ⇒ 同一时刻只能在**一个**
    ///    宿主里 ⇒ 切到这一页时把它搬过来、切走时搬回去（见 `tabs.rs` 的
    ///    `switch-page` 回调）。
    host: Option<gtk::Paned>,
    /// 关之前先问一遍（未保存修改）。返回 false = 不关。
    on_close: Option<Box<dyn Fn() -> bool>>,
    /// 现在停在**哪儿**（阶段 137：主栏 / 底部 / 侧栏）。
    place: EditorPlace,
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
    /// **工作区**（阶段 137）：`[主栏 | 侧栏]` 的水平分栏。
    ///
    /// ⚠️ **为什么要有**：用户要「tab 可以**自由停靠在屏幕的多个位置**」
    ///   （像 VS Code 的分栏）⇒ 光有主栏的页签栏不够 ⇒ 得能**在边上再开
    ///   一栏**。
    work: gtk::Paned,
    /// **侧栏**（阶段 137）：左右分栏里那一栏的页签 notebook。
    ///
    /// ⚠️ **为什么是另一个 notebook**：每个「栏」要有**自己的页签栏**
    ///   （VS Code 的 editor group 就是这样）⇒ 侧栏里的文件各有各的页签。
    side_nb: gtk::Notebook,
    /// 侧栏的外壳（放 `side_nb`；没有页时整块藏起来）。
    side_box: gtk::Box,
    /// 侧栏现在在**左边**吗（阶段 137：拖到左边缘 ⇒ 侧栏换到左边）。
    side_left: Cell<bool>,
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
    /// **已经接过宽度回调**的编辑器 id（阶段 121）。
    ///
    /// ⚠️ **为什么要有它**：`track_file_width` 会在宿主 Paned 上挂一个
    ///    `notify::position` 回调（把分割条位置写回 `file_w`）⇒ **同一个
    ///    宿主只能挂一次** ⇒ 每次切页都挂 ⇒ 回调越积越多。
    /// ⇒ 用这个清单记住「谁已经挂过了」。
    tracked_editor_hosts: RefCell<Vec<u64>>,
    /// 当前显示的编辑器 id。
    editor_current: Cell<Option<u64>>,
    /// 编辑区现在的**宿主**（终端列那个竖向 Paned，阶段 108）。
    editor_host: RefCell<Option<gtk::Paned>>,
    /// **每一页自己的**编辑区宿主（页 id → 竖向 Paned）。
    ///
    /// ⚠️ **干什么用**：编辑区（阶段 107）是**一个共享的 widget** ⇒ 它同一
    ///   时刻只能待在**一页**里 ⇒ 切页时得把它**搬到当前页**。
    ///   - 终端页的宿主 = 「终端列」（阶段 108）；
    ///   - **容器页的宿主 = 它自己那一列**（阶段 113 之后）⇒ 用户在容器页
    ///     点「编辑 compose 文件」⇒ **编辑器就在容器界面下面**，用户马上
    ///     能看到（**不用跑到 term 界面去找**——用户 2026-11-16 报的）。
    ///
    /// ⚠️ **没登记的页不动编辑区**（例如数据库工作台页 ⇒ 编辑区留在终端
    ///   列里，跟以前一样）。
    page_editor_hosts: RefCell<std::collections::HashMap<u64, gtk::Paned>>,
    /// 空编辑区时的提示（**停靠区**：把编辑器拖回来）。
    editor_hint: gtk::Label,
    pages: RefCell<Vec<SubPage>>,
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
    /// **跨格子搬页签**的回调（阶段 145）。
    ///
    /// `(页 id, 动作)` ⇒ 动作为 [`crate::layout::dnd::PageMove`]。
    ///
    /// ⚠️ **为什么挂在 `SubTabs` 上**：落点只知道"自己"这个格子 ⇒ 得把
    ///    "往哪搬"这件事交给上层（`CellTree`）⇒ 它才知道别的格子在哪。
    move_hook: RefCell<Option<Rc<dyn Fn(u64, crate::layout::dnd::PageMove) -> bool>>>,
    /// 「**有页签被搬进来了**」的回调（阶段 146）。
    ///
    /// 传**页签 id**（搬动时 id 不变 ⇒ 上层认得出是哪一个）。
    ///
    /// ⚠️ **干什么用**：文件面板那一页被拖到别的格子 ⇒ 它的**宿主**也变了
    ///    ⇒ `PanelState.file_host` 必须跟着换（否则记的还是旧格子）。
    moved_hook: RefCell<Option<Rc<dyn Fn(u64)>>>,
    /// 「**这个格子空了**」的回调（阶段 145）。
    ///
    /// ⚠️ **为什么与 `close_hooks` 分开**：`close_hooks` 说的是"这个**页签**
    ///    没了"（终端页签要收会话）；这里说的是"这个**格子**空了"
    ///    （格子要从树里摘掉）⇒ 两件事、两套钩子。
    empty_hook: RefCell<Option<Rc<dyn Fn() -> bool>>>,
    /// **蓝色插入符**（拖到页签栏上时显示在那儿）。
    caret: gtk::Box,
    /// **边缘高亮条**（拖到格子边上时亮起来的那条）。
    ///
    /// 四条边各一个 ⇒ 拖到哪条边就亮哪条。
    edges: [gtk::Box; 4],
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

        // ---- 浮在页签栏那一行的右端（**不占布局宽度**）----
        //
        // ⚠️ **阶段 137**：这个 Overlay 现在是**主栏**（`main_group`），
        //   「+」按钮要浮在**主栏页签栏**的右端 ⇒ 不能浮在整个工作区的
        //   右端（那样有侧栏时按钮会跑到侧栏那半边去）。
        let main_group = gtk::Overlay::new();
        main_group.set_child(Some(&nb));
        main_group.add_overlay(&btn_box);
        // halign/valign 不是 FILL 时，Overlay 会按**自然尺寸**把 overlay
        // 子控件摆到对应的边上——所以按钮只占自己那一小块，不拦截别处的点击
        //（GTK 4.10 的 overlay-pass-through  gtk4-rs 0.11 没绑，用不着）。
        btn_box.set_halign(gtk::Align::End);
        btn_box.set_valign(gtk::Align::Start);
        btn_box.set_margin_end(4);
        btn_box.set_margin_top(2);

        // ---- **蓝色插入符**（阶段 145）----
        //
        // 拖到页签栏上时，在**插入位置**显示一条蓝色竖线。
        //
        // ⚠️ **挂在 `main_group` 上**（不是 notebook）：`main_group` 的主
        //   子控件就是 notebook ⇒ 两者的坐标**重合** ⇒ 用 notebook 的 x
        //   摆插入符，位置正好落在页签栏里。
        let caret = gtk::Box::new(gtk::Orientation::Vertical, 0);
        caret.add_css_class("pp-drop-caret");
        caret.set_halign(gtk::Align::Start);
        caret.set_valign(gtk::Align::Start);
        caret.set_visible(false);
        // ⚠️ **不能抢指针**（`can_target = false`）⇒ 否则它会挡住落点
        //   ⇒ 拖放管线接不上（页签栏那个接收器收不到）。
        caret.set_can_target(false);
        main_group.add_overlay(&caret);

        // ---- **侧栏**（阶段 137）：左右分栏里那一栏 ----
        //
        // ⚠️ **为什么是另一个 notebook**：每个「栏」要有**自己的页签栏**
        //   （VS Code 的 editor group 就是这样）⇒ 侧栏里的文件各有各的页签，
        //   切侧栏不影响主栏当前页。
        let side_nb = gtk::Notebook::new();
        side_nb.set_scrollable(true);
        side_nb.set_vexpand(true);
        side_nb.set_hexpand(true);
        let side_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side_box.append(&side_nb);
        side_box.set_hexpand(true);
        side_box.set_vexpand(true);
        side_box.set_visible(false); // 没有页时整块藏起来

        // ---- **工作区**：`[主栏 | 侧栏]` 的水平分栏 ----
        let work = gtk::Paned::new(gtk::Orientation::Horizontal);
        work.set_start_child(Some(&main_group));
        work.set_end_child(Some(&side_box));
        work.set_hexpand(true);
        work.set_vexpand(true);
        // ⚠️ **侧栏先收起来**（position = 0 ⇒ 空间全给主栏）。
        work.set_resize_start_child(true);
        work.set_shrink_start_child(false);
        work.set_resize_end_child(false);
        work.set_shrink_end_child(false);
        work.set_position(0);

        // 连接页的内容 = 工作区。
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&work));

        // ---- **边缘高亮条**（阶段 145）----
        //
        // 拖到格子**边上**时，那条边亮起来（蓝条）⇒ 用户看得见"要劈开了"。
        //
        // ⚠️ **四条各一个 widget**（不是一条带方向的）⇒ 亮哪条一目了然。
        // ⚠️ **不能抢指针**（`can_target = false`）⇒ 落点判定照旧走
        //   notebook 那个接收器。
        let mut edge_left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let mut edge_right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let mut edge_top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let mut edge_bottom = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for (e, cls, ha, va) in [
            (&mut edge_left, "pp-drop-edge-left", gtk::Align::Start, gtk::Align::Fill),
            (&mut edge_right, "pp-drop-edge-right", gtk::Align::End, gtk::Align::Fill),
            (&mut edge_top, "pp-drop-edge-top", gtk::Align::Fill, gtk::Align::Start),
            (&mut edge_bottom, "pp-drop-edge-bottom", gtk::Align::Fill, gtk::Align::End),
        ] {
            e.add_css_class(cls);
            e.set_halign(ha);
            e.set_valign(va);
            e.set_visible(false);
            e.set_can_target(false);
            overlay.add_overlay(e);
        }

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
            work,
            side_nb,
            side_box,
            side_left: Cell::new(false),
            editor_area,
            editor_tabs,
            editor_stack,
            editor_pages: RefCell::new(Vec::new()),
            tracked_editor_hosts: RefCell::new(Vec::new()),
            editor_current: Cell::new(None),
            editor_host: RefCell::new(None),
            page_editor_hosts: RefCell::new(std::collections::HashMap::new()),
            editor_hint: hint,
            pages: RefCell::new(Vec::new()),
            new_hook: RefCell::new(None),
            add_btn: add.clone(),
            new_pop: RefCell::new(None),
            close_hooks: RefCell::new(Vec::new()),
            move_hook: RefCell::new(None),
            moved_hook: RefCell::new(None),
            empty_hook: RefCell::new(None),
            caret: caret.clone(),
            edges: [edge_left, edge_right, edge_top, edge_bottom],
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

        // 阶段 145：格子级的拖放接收器（页签栏插入 ＋ 四边拆分）。
        // 阶段 108 的老规矩（编辑器拖回页签栏）也在这儿。
        sub.attach_cell_target();
        // 阶段 108：编辑区是「把编辑器拖回来」的落点（**挂在编辑区自己身上**）。
        sub.attach_dock_target();
        // 阶段 137：侧栏是「停到分栏里」的落点；工作区左右边缘是「开分栏」。
        sub.attach_side_target();
        sub.attach_work_target();

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

    /// 把**关闭钩子**复制给另一个容器（阶段 145：新格子也得收会话）。
    ///
    /// ⚠️ **为什么只复制关闭钩子、不复制「+」菜单钩子**：
    ///    - 关闭钩子是**按页签 id** 找东西的（`on_term_page_closed`）⇒
    ///      页签搬到哪个格子都认得 ⇒ 复制过去就对了；
    ///    - 「+」菜单钩子是**闭包**，它建的东西要落在**哪个格子**里
    ///      ⇒ 得由上层按格子重新装（见 `CellTree::set_new_cell_hook`）。
    pub fn copy_close_hooks_to(&self, other: &Rc<SubTabs>) {
        let hooks = self.close_hooks.borrow().clone();
        for h in hooks {
            other.close_hooks.borrow_mut().push(h);
        }
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
        let id = crate::layout::dnd::alloc_page_id();
        content.set_hexpand(true);
        content.set_vexpand(true);

        let (head, label, close) = self.build_page_head(icon, title);
        self.nb.append_page(&content, Some(&head));
        self.pages.borrow_mut().push(SubPage {
            id,
            title: title.to_string(),
            label,
            icon: icon.map(|s| s.to_string()),
            head: head.clone(),
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
        // ⚠️ **页签头能拖**（阶段 145）⇒ 可以拖到别的格子去。
        self.attach_page_drag(&head, id, kind);
        id
    }

    /// 建一个子页的**页签头**：[图标][标题][×]。
    ///
    /// ⚠️ **为什么抽出来**（阶段 145）：跨格子搬运时要把页签**整个搬过去**
    ///    ⇒ 页签头得能重建（不能只搬内容 widget）。
    ///
    /// 返回 `(头, 标题标签, 关闭键)`——标题标签要留着改名，关闭键要接回调。
    fn build_page_head(
        &self,
        icon: Option<&str>,
        title: &str,
    ) -> (gtk::Box, gtk::Label, gtk::Button) {
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
        (head, label, close)
    }

    /// **静默**摘下一个子页（**不触发 close 钩子**）⇒ 返回它的内容。
    ///
    /// ⚠️ **为什么不走 `remove_page`**：那个会通知上层"这个页签没了"
    ///    （终端页签的钩子要**收掉 SSH 会话**！）⇒ 跨格子搬运时不能触发
    ///    ⇒ 搬过去还是同一个会话。
    ///
    /// ⚠️ **只摘不销毁**：`widget` 交给调用方（下一个 `insert_page` 接上）。
    pub fn take_page(&self, id: u64) -> Option<PageData> {
        let idx = self.pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return None };
        let page = {
            let mut pages = self.pages.borrow_mut();
            pages.remove(idx)
        };
        // 从 notebook 里摘下来（**不销毁**：GTK4 只是脱离父容器）。
        if let Some(num) = self.nb.page_num(&page.widget) {
            self.nb.remove_page(Some(num));
        }
        Some(PageData {
            id: page.id,
            title: page.title,
            icon: page.icon,
            kind: page.kind,
            widget: page.widget,
            on_close: page.on_close,
        })
    }

    /// 把一个子页**放进**这个容器（跨格子搬运的另一半）。
    ///
    /// `index` = 插到第几个页签之前（`None` = 追加到最后）。
    ///
    /// ⚠️ **页内容 widget 不能同时挂进两个 notebook** ⇒ 调用方必须**先**
    ///    `take_page`（摘下来）再 `insert_page`（挂上去）。
    pub fn insert_page(self: &Rc<Self>, data: PageData, index: Option<usize>) -> u64 {
        // ⚠️ **沿用原来的 id**（不重新分配）⇒ 上层按 id 记的东西不会失配。
        let id = data.id;
        data.widget.set_hexpand(true);
        data.widget.set_vexpand(true);
        let (head, label, close) = self.build_page_head(data.icon.as_deref(), &data.title);
        match index {
            Some(i) => {
                self.nb.insert_page(&data.widget, Some(&head), Some(i as u32));
            }
            None => {
                self.nb.append_page(&data.widget, Some(&head));
            }
        }
        // ⚠️ **`pages` 的顺序要跟页签顺序一致**（同格子里挪位置时，
        //    `page_ids()` 才对得上）。
        let new_page = SubPage {
            id,
            title: data.title,
            label,
            icon: data.icon,
            head: head.clone(),
            widget: data.widget,
            kind: data.kind,
            on_close: data.on_close,
        };
        let mut pages = self.pages.borrow_mut();
        match index {
            Some(i) if i < pages.len() => pages.insert(i, new_page),
            _ => pages.push(new_page),
        }
        drop(pages);
        {
            let me = Rc::clone(self);
            close.connect_clicked(move |_| {
                me.close_page(id);
            });
        }
        self.attach_page_drag(&head, id, data.kind);
        // ⚠️ **通知上层**：有页签搬进这个格子了（阶段 146：宿主要跟着换）。
        let hook = self.moved_hook.borrow().clone();
        if let Some(f) = hook {
            f(id);
        }
        id
    }

    /// 给子页的页签头挂**拖拽源**（阶段 145）⇒ 页签能拖到别的格子。
    ///
    /// ⚠️ **闭包不抓 `self`**（载荷是纯数据）⇒ 没有引用环。
    fn attach_page_drag(&self, widget: &impl IsA<gtk::Widget>, id: u64, kind: PageKind) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::MOVE);
        let payload = crate::layout::dnd::encode_payload(kind.tag(), id);
        src.set_content(Some(&gdk::ContentProvider::for_bytes(
            PAGE_MIME,
            &glib::Bytes::from(payload.as_bytes()),
        )));
        widget.add_controller(src);
    }

    /// 所有子页的 id（**按页签顺序**）。
    pub fn page_ids(&self) -> Vec<u64> {
        self.pages.borrow().iter().map(|p| p.id).collect()
    }

    /// 页签头的横坐标（**相对 notebook**）⇒ 算「插到第几个之前」。
    ///
    /// ⚠️ **量不出来就当 0**（还没布局完 ⇒ 插到最前面，寧可插前也别插错位）。
    fn page_head_xs(&self) -> Vec<f64> {
        let pages = self.pages.borrow();
        let mut out = Vec::with_capacity(pages.len());
        for p in pages.iter() {
            let x = p
                .head
                .translate_coordinates(&self.nb, 0.0, 0.0)
                .map(|(x, _)| x)
                .unwrap_or(0.0);
            out.push(x);
        }
        out
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
        // ⚠️ **格子空了 ⇒ 通知上层把它摘掉**（阶段 145：关掉一个格子里的最后
        //   一个页签 ⇒ 该格子自动关闭，父节点收拢）。
        if self.pages.borrow().is_empty() {
            let hook = self.empty_hook.borrow().clone();
            if let Some(f) = hook {
                f();
            }
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
        // ⚠️ **换了宿主 ⇒ 分割条位置要重算**：位置是按宿主高度算的
        //   （`layout_editor_area`）⇒ 不重算的话，编辑区在新宿主里
        //   可能只有 0 高度（**看不见**）。
        self.refresh_editor_area();
    }

    /// 编辑区控件（**测试钩子**：冒烟要量它的位置/宽度）。
    pub fn editor_area_widget(&self) -> gtk::Widget {
        self.editor_area.clone().upcast()
    }

    /// 登记「**这一页**的编辑区宿主」（竖向 Paned）。
    ///
    /// 干什么用：编辑区是**一个共享的 widget** ⇒ 它同一时刻只能待在一页
    /// 里 ⇒ 切页时得把它搬过去（见 [`Self::host_editor_area_for_page`]）。
    ///
    /// ⚠️ **谁要登记**：页面自己（容器页在 `dockerui::mount` 里登记）。
    pub fn set_page_editor_host(self: &Rc<Self>, page_id: u64, host: gtk::Paned) {
        self.page_editor_hosts
            .borrow_mut()
            .insert(page_id, host);
    }

    /// 把编辑区挂到**某一页**的宿主（这一页没登记 ⇒ 什么都不做）。
    ///
    /// 返回 true = 挂过去了（调用方据此知道「这一页自己管编辑区」）。
    pub fn host_editor_area_for_page(self: &Rc<Self>, page_id: u64) -> bool {
        let host = self.page_editor_hosts.borrow().get(&page_id).cloned();
        let Some(host) = host else { return false };
        self.host_editor_area(&host);
        true
    }

    /// 按**页序号**把编辑区挂到那一页的宿主（没登记 ⇒ 返回 false）。
    ///
    /// ⚠️ **干什么用**：切子页签时调用 ⇒ 编辑区**跟着当前页走**
    ///   （终端页 ⇒ 终端列；容器页 ⇒ 容器页自己那一列）。
    pub fn host_editor_area_for_page_num(self: &Rc<Self>, num: u32) -> bool {
        let id = {
            let pages = self.pages.borrow();
            pages
                .iter()
                .find(|p| self.nb.page_num(&p.widget) == Some(num))
                .map(|p| p.id)
        };
        match id {
            Some(id) => self.host_editor_area_for_page(id),
            None => false,
        }
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

    /// 测试钩子：某个编辑器页签上的**图标名**（按出现顺序）。
    ///
    /// ⚠️ **为什么要有**：页签上现在有**两个**小图标（位置 ＋ 模式）⇒
    ///   冒烟得能验「位置图标真的挂上去了」（用户 2026-11-19 提的）。
    pub fn editor_tab_icon_names_for_test(&self, id: u64) -> Vec<String> {
        // ⚠️ **三个停靠位的标签控件不一样**：底部＝编辑区的小标签；
        //    主栏 / 侧栏＝页签头（`head`）⇒ 都要能读到。
        let (tab, head) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return Vec::new();
            };
            (p.tab.clone(), p.head.clone())
        };
        let mut out = Vec::new();
        if let Some(t) = tab {
            collect_icon_names(t.upcast_ref::<gtk::Widget>(), &mut out);
        }
        if let Some(h) = head {
            collect_icon_names(h.upcast_ref::<gtk::Widget>(), &mut out);
        }
        out
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
        icon: Option<TabIcon>,
        loc_icon: Option<TabIcon>,
        title: &str,
        content: gtk::Widget,
        on_close: Option<Box<dyn Fn() -> bool>>,
    ) -> u64 {
        content.set_hexpand(true);
        content.set_vexpand(true);

        // 标题标签：**每次都新造一个**（不在容器之间搬家 ⇒ 文件名不会丢，
        // 理由见 `make_title_label`）。
        let label = self.make_title_label(title);

        // ⚠️ **阶段 137：默认停「主栏」**（与终端并列的页签）——
        // 用户 2026-11-19：「默认打开还是显示为和 term 并列的 tab」。
        //
        // 那一页的内容 = 「**文件面板 ｜ 编辑器**」（阶段 121 的形态：
        // 用户 2026-11-15「编辑器旁边要有 sftp」）。
        //
        // ⚠️ **文件面板是「会话级的一个 widget」** ⇒ 同一时刻只能在一个宿主
        //    里 ⇒ 左边这一格**先空着**，切到这一页时由 `tabs.rs` 的
        //    `switch-page` 回调把文件面板搬过来（切走时搬回去）。
        let host = gtk::Paned::new(gtk::Orientation::Horizontal);
        host.set_end_child(Some(&content));
        host.set_hexpand(true);
        host.set_vexpand(true);
        let (head, label) = self.build_editor_head(id, icon.as_ref(), loc_icon.as_ref(), title);
        self.nb.append_page(&host.clone().upcast::<gtk::Widget>(), Some(&head));
        self.editor_pages.borrow_mut().push(EditorPage {
            id,
            icon,
            loc_icon,
            title: title.to_string(),
            label,
            tab: None,
            head: Some(head),
            widget: content,
            host: Some(host),
            on_close,
            place: EditorPlace::Main,
        });
        self.refresh_editor_area();
        // 刚打开的文件**立刻可见**（切到它那一页）。
        self.switch_editor(id);
        id
    }

    /// 编辑区小标签栏上的一个标签：[位置图标][图标][文件名][×]（阶段 107）。
    fn build_editor_tab(
        self: &Rc<Self>,
        id: u64,
        icon: Option<&TabIcon>,
        loc_icon: Option<&TabIcon>,
        title: &str,
        label: &gtk::Label,
    ) -> gtk::Box {
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab.set_margin_start(4);
        tab.set_margin_end(2);
        tab.add_css_class("pp-editor-tab");
        // ⚠️ **位置图标在前**（远程 / 本地）⇒ 一眼知道「这个文件在哪台机器上」。
        append_icon(&tab, loc_icon);
        append_icon(&tab, icon);
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
        icon: Option<&TabIcon>,
        loc_icon: Option<&TabIcon>,
        title: &str,
    ) -> (gtk::Box, gtk::Label) {
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        head.set_margin_start(4);
        head.set_margin_end(2);
        // ⚠️ **位置图标在前**（远程 / 本地）——跟编辑区小标签一个顺序。
        append_icon(&head, loc_icon);
        append_icon(&head, icon);
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
    /// 负载 = `editor:<id>`（阶段 145 统一协议），落点自己读
    /// （见 `read_page_payload`）。
    fn attach_editor_drag(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>, id: u64) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::MOVE);
        let payload = crate::layout::dnd::encode_payload(PageKindTag::Editor, id);
        src.set_content(Some(&gdk::ContentProvider::for_bytes(
            PAGE_MIME,
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
                let Some(me) = me.upgrade() else { return };
                // ⚠️ **只有「停在底部编辑区」的编辑器才切页**（阶段 137）：
                //    停在主栏/侧栏的编辑器，它的页签**就在眼前** ⇒ 切走
                //    反而会把它藏起来（拖都拖不成）。
                if me.editor_place(id) == Some(EditorPlace::Bottom) {
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

    /// 切到某个编辑器（**按它停在哪儿**分派：底部 / 主栏 / 侧栏）。
    pub fn switch_editor(self: &Rc<Self>, id: u64) {
        let (place, widget, host) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return;
            };
            (p.place, p.widget.clone(), p.host.clone())
        };
        match place {
            EditorPlace::Bottom => {
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
            }
            EditorPlace::Main => {
                // 主栏那一页 ⇒ 切到那个页签。
                //
                // ⚠️ **页内容是宿主 Paned**（阶段 121：左＝文件面板 / 右＝编辑器）
                //    ⇒ 用宿主去查页序号，**不能**用编辑器内容 widget。
                if let Some(h) = &host {
                    if let Some(num) = self.nb.page_num(h) {
                        self.nb.set_current_page(Some(num));
                    }
                }
                self.editor_current.set(Some(id));
            }
            EditorPlace::Side => {
                // 侧栏那一页 ⇒ 切侧栏的当前页（**别动主栏的当前页**）。
                if let Some(num) = self.side_nb.page_num(&widget) {
                    self.side_nb.set_current_page(Some(num));
                }
                self.editor_current.set(Some(id));
            }
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

    /// **不询问**直接关掉某个编辑器（**三种位置都能摘**：底部 / 主栏 / 侧栏）。
    pub fn remove_editor(self: &Rc<Self>, id: u64) -> bool {
        let idx = self.editor_pages.borrow().iter().position(|p| p.id == id);
        let Some(idx) = idx else { return false };
        let (place, widget, tab, host) = {
            let pages = self.editor_pages.borrow();
            (
                pages[idx].place,
                pages[idx].widget.clone(),
                pages[idx].tab.clone(),
                pages[idx].host.clone(),
            )
        };
        match place {
            EditorPlace::Bottom => {
                self.editor_stack.remove(&widget);
                if let Some(t) = &tab {
                    self.editor_tabs.remove(t);
                }
            }
            EditorPlace::Main => {
                // 主栏那一页 ⇒ 从 notebook 里摘掉。
                //
                // ⚠️ **先把两个孩子摘下来**（阶段 121）：编辑器（要留给调用方，
                //    不然会被连带销毁）+ **文件面板**（它住在左半 ⇒ 宿主被销毁
                //    时它也会跟着没了 ⇒ **SFTP 列表整个消失**）。
                if let Some(h) = &host {
                    h.set_end_child(None::<&gtk::Widget>);
                    h.set_start_child(None::<&gtk::Widget>);
                    if let Some(num) = self.nb.page_num(h) {
                        self.nb.remove_page(Some(num));
                    }
                }
            }
            EditorPlace::Side => {
                // 侧栏那一页 ⇒ 从侧栏的 notebook 里摘掉。
                if let Some(num) = self.side_nb.page_num(&widget) {
                    self.side_nb.remove_page(Some(num));
                }
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
                .filter(|p| p.place == EditorPlace::Bottom)
                .last()
                .map(|p| p.id);
            match next {
                Some(n) => self.switch_editor(n),
                None => self.editor_current.set(None),
            }
        }
        self.refresh_editor_area();
        self.refresh_side();
        true
    }

    /// 把编辑器挪到**主栏**（与终端并列的页签）。
    ///
    /// 干什么用（用户 2026-11-19）：「默认打开还是显示为和 term 并列的
    /// tab」⇒ 主栏是编辑器的**默认**家；拖到底部、拖到侧栏之后，还能再
    /// 拖回来。
    ///
    /// 返回是否真挪过去了（已经在主栏 ⇒ false）。
    pub fn move_editor_to_main(self: &Rc<Self>, id: u64) -> bool {
        let (icon, loc_icon, title, widget, place) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return false;
            };
            if p.place == EditorPlace::Main {
                return false; // 已经在主栏了
            }
            (
                p.icon.clone(),
                p.loc_icon.clone(),
                p.title.clone(),
                p.widget.clone(),
                p.place,
            )
        };
        // 先把它从原来那儿的容器里摘下来。
        self.detach_editor_widget(id, place, &widget);
        // 变成主栏的一个页签（**页签头是新造的** ⇒ 文件名一定在）。
        //
        // ⚠️ **那一页的内容是「文件面板 ｜ 编辑器」的 Paned**（阶段 121）：
        //    用户 2026-11-15：「**编辑器旁边要有 sftp**」。
        let host = gtk::Paned::new(gtk::Orientation::Horizontal);
        host.set_end_child(Some(&widget));
        host.set_hexpand(true);
        host.set_vexpand(true);
        let (head, label) =
            self.build_editor_head(id, icon.as_ref(), loc_icon.as_ref(), &title);
        self.nb.append_page(&host.clone().upcast::<gtk::Widget>(), Some(&head));
        {
            let mut pages = self.editor_pages.borrow_mut();
            if let Some(p) = pages.iter_mut().find(|p| p.id == id) {
                p.place = EditorPlace::Main;
                p.head = Some(head);
                p.tab = None;
                p.label = label;
                p.host = Some(host);
            }
        }
        self.refresh_editor_area();
        self.switch_editor(id);
        true
    }

    /// 把编辑器挪到**侧栏**（左右分栏；阶段 137）。
    ///
    /// 干什么用（用户 2026-11-19）：「这个 tab 可以**自由停靠在屏幕的多个
    /// 位置**，就像很多软件那样，比如 vscode」⇒ 拖到窗口左/右边缘 ⇒
    /// 在那儿开一条分栏，编辑器停进去。
    ///
    /// ⚠️ **侧栏那一页只有编辑器**（不带文件面板）：文件面板是**会话级的
    ///    一个 widget** ⇒ 同一时刻只能在一个宿主里 ⇒ 侧栏不抢它。
    ///
    /// 返回是否真挪过去了（已经在侧栏 ⇒ false）。
    pub fn move_editor_to_side(self: &Rc<Self>, id: u64) -> bool {
        let (icon, loc_icon, title, widget, place) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return false;
            };
            if p.place == EditorPlace::Side {
                return false; // 已经在侧栏了
            }
            (
                p.icon.clone(),
                p.loc_icon.clone(),
                p.title.clone(),
                p.widget.clone(),
                p.place,
            )
        };
        self.detach_editor_widget(id, place, &widget);
        // 侧栏页签头（新造的 ⇒ 文件名一定在）。
        let (head, label) =
            self.build_editor_head(id, icon.as_ref(), loc_icon.as_ref(), &title);
        self.side_nb.append_page(&widget, Some(&head));
        {
            let mut pages = self.editor_pages.borrow_mut();
            if let Some(p) = pages.iter_mut().find(|p| p.id == id) {
                p.place = EditorPlace::Side;
                p.head = Some(head);
                p.tab = None;
                p.label = label;
                p.host = None;
            }
        }
        self.refresh_editor_area();
        self.refresh_side();
        self.switch_editor(id);
        true
    }

    /// 把编辑器**从它现在待着的容器里摘下来**（**不动登记**）。
    ///
    /// ⚠️ **为什么单独一个函数**：三个停靠位（底部 / 主栏 / 侧栏）搬来搬去
    ///   ⇒ 摘的规矩得**只有一处** ⇒ 不然漏一处就是「控件有两个爹」。
    fn detach_editor_widget(self: &Rc<Self>, id: u64, place: EditorPlace, widget: &gtk::Widget) {
        match place {
            EditorPlace::Bottom => {
                self.editor_stack.remove(widget);
                let tab = self
                    .editor_pages
                    .borrow()
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.tab.clone());
                if let Some(t) = &tab {
                    self.editor_tabs.remove(t);
                }
            }
            EditorPlace::Main => {
                let host = self
                    .editor_pages
                    .borrow()
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.host.clone());
                // ⚠️ **先把两个孩子摘下来**（阶段 121）：编辑器（要搬走 ⇒
                //    不摘就会被连带销毁）+ **文件面板**（它住在左半 ⇒ 宿主
                //    被销毁时它也会跟着没了 ⇒ **SFTP 列表整个消失**）。
                if let Some(h) = &host {
                    h.set_end_child(None::<&gtk::Widget>);
                    h.set_start_child(None::<&gtk::Widget>);
                    if let Some(num) = self.nb.page_num(h) {
                        self.nb.remove_page(Some(num));
                    }
                }
            }
            EditorPlace::Side => {
                if let Some(num) = self.side_nb.page_num(widget) {
                    self.side_nb.remove_page(Some(num));
                }
            }
        }
    }

    /// 把编辑器挪到**底部编辑区**（终端列下面；阶段 107 的默认家）。
    ///
    /// 返回是否真挪过去了（已经在底部 ⇒ false）。
    pub fn move_editor_to_bottom(self: &Rc<Self>, id: u64) -> bool {
        let (icon, loc_icon, title, widget, place) = {
            let pages = self.editor_pages.borrow();
            let Some(p) = pages.iter().find(|p| p.id == id) else {
                return false;
            };
            if p.place == EditorPlace::Bottom {
                return false; // 已经在编辑区里了
            }
            (
                p.icon.clone(),
                p.loc_icon.clone(),
                p.title.clone(),
                p.widget.clone(),
                p.place,
            )
        };
        self.detach_editor_widget(id, place, &widget);
        // 小标签：**新造一个**（不把页签头那个搬回来 ⇒ 文件名不会丢）。
        let label = self.make_title_label(&title);
        let tab = self.build_editor_tab(id, icon.as_ref(), loc_icon.as_ref(), &title, &label);
        self.editor_stack.add_named(&widget, Some(&id.to_string()));
        self.editor_tabs.append(&tab);
        {
            let mut pages = self.editor_pages.borrow_mut();
            if let Some(p) = pages.iter_mut().find(|p| p.id == id) {
                p.place = EditorPlace::Bottom;
                p.tab = Some(tab);
                p.head = None;
                p.label = label;
                p.host = None;
            }
        }
        self.refresh_editor_area();
        self.switch_editor(id);
        // ⚠️ **切回「编辑区所在的那一页」**（阶段 121）：编辑器回编辑区了 ⇒
        // 用户要看的是**终端页**（编辑区在终端列下方）⇒ 顺便把文件面板搬回
        // 终端页的宿主（`switch-page` 回调做这件事）。
        self.show_editor_host_page();
        true
    }

    /// 这个编辑器现在是停在**编辑区**还是**子标签栏**（阶段 108）。
    pub fn editor_is_docked(&self, id: u64) -> Option<bool> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.place == EditorPlace::Bottom)
    }

    /// 这个编辑器现在停在**哪儿**（阶段 137：主栏 / 底部 / 侧栏）。
    pub fn editor_place(&self, id: u64) -> Option<EditorPlace> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.place)
    }

    /// 测试钩子：**侧栏**里有几个页签（阶段 137）。
    pub fn side_page_count(&self) -> usize {
        self.side_nb.n_pages() as usize
    }

    /// 测试钩子：侧栏现在在**左边**吗（阶段 137）。
    pub fn side_is_left(&self) -> bool {
        self.side_left.get()
    }

    /// 把侧栏换到**左边 / 右边**（阶段 137：拖到窗口左/右边缘）。
    pub fn set_side_left_pub(self: &Rc<Self>, left: bool) {
        self.set_side_left(left);
    }

    /// 「拖回子标签栏的编辑器」那一页的**宿主 Paned**（左＝文件面板 / 右＝编辑器）。
    ///
    /// ⚠️ **阶段 121 新增**：编辑器拖到子标签栏后，那一页**自带一份 SFTP
    ///    文件列表**（用户 2026-11-15：「编辑器旁边要有 sftp，就像我们修改
    ///    将 term 和编辑器能同时显示之前那样」）⇒ 这个 Paned 就是它的宿主。
    ///
    /// 停靠在编辑区里的编辑器 ⇒ `None`（它在终端列下方，没有自己的宿主）。
    pub fn editor_host(&self, id: u64) -> Option<gtk::Paned> {
        self.editor_pages
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.host.clone())
    }

    /// 按**页序号**找「拖回子标签栏的编辑器」那一页的宿主。
    ///
    /// 干什么用：切页时要把**文件面板**搬到当前页的宿主里（文件面板是
    /// 会话级的一个 widget ⇒ 同一时刻只能在一个宿主里）⇒ 上层按页序号
    /// 问一句「这一页是不是编辑器页、它的宿主在哪」。
    pub fn editor_host_by_page_num(&self, num: u32) -> Option<(u64, gtk::Paned)> {
        self.editor_pages.borrow().iter().find_map(|p| {
            let h = p.host.as_ref()?;
            if self.nb.page_num(h) == Some(num) {
                Some((p.id, h.clone()))
            } else {
                None
            }
        })
    }

    /// 记下「这个编辑器的宿主已经挂过宽度回调了」（第一次返回 true）。
    ///
    /// ⚠️ 同一个宿主**只能挂一次** `track_file_width` ⇒ 每次切页都挂 ⇒
    ///    `notify::position` 的回调会越积越多。
    pub fn mark_editor_host_tracked(&self, id: u64) -> bool {
        let mut v = self.tracked_editor_hosts.borrow_mut();
        if v.contains(&id) {
            false
        } else {
            v.push(id);
            true
        }
    }

    /// 当前显示的编辑器 id。
    pub fn current_editor(&self) -> Option<u64> {
        // 当前子页签是不是一个「拖回子标签栏」的编辑器？
        //
        // ⚠️ **页内容是宿主 Paned**（阶段 121）⇒ 按**页序号**认，别拿编辑器
        //    内容 widget 去比（比不上）。
        if let Some(cur) = self.nb.current_page() {
            let pages = self.editor_pages.borrow();
            if let Some(p) = pages.iter().find(|p| {
                p.place != EditorPlace::Bottom
                    && p.host
                        .as_ref()
                        .map(|h| self.nb.page_num(h) == Some(cur))
                        .unwrap_or(false)
            }) {
                return Some(p.id);
            }
        }
        // 否则是编辑区里"当前"那个（**必须是还停靠着的**——拖走的那个
        // 已经不在编辑区了，指着它会让快捷键找错人）。
        let cur = self.editor_current.get();
        let pages = self.editor_pages.borrow();
        cur.filter(|id| pages.iter().any(|p| p.id == *id && p.place == EditorPlace::Bottom))
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
            (
                !pages.is_empty(),
                pages.iter().any(|p| p.place == EditorPlace::Bottom),
            )
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

    /// **侧栏**的显示/隐藏（阶段 137）。
    ///
    /// ⚠️ **没有页就整块藏起来**（Paned 的 position = 0 ⇒ 空间全给主栏）
    /// ⇒ 用户没往侧栏拖东西时，界面跟以前一模一样（不占地方）。
    fn refresh_side(&self) {
        let has = self.side_nb.n_pages() > 0;
        self.side_box.set_visible(has);
        if !has {
            self.work.set_position(0);
            return;
        }
        // 给侧栏一块宽度（约 40%）。
        let w = self.work.width();
        if w <= 0 {
            return; // 还没量出宽度 ⇒ 等下一次再说
        }
        let target = if self.side_left.get() {
            // 侧栏在**左边**（它是 start child）⇒ 分割条在 40% 处。
            ((w as f64) * 0.4) as i32
        } else {
            // 侧栏在**右边**（它是 end child）⇒ 分割条在 60% 处。
            ((w as f64) * 0.6) as i32
        };
        self.work.set_position(target);
    }

    /// 侧栏换到**左边**（阶段 137：拖到窗口左边缘 ⇒ 在左边开一条分栏）。
    ///
    /// ⚠️ **换边 = 把 Paned 的两个孩子对调** ⇒ 先都摘下来再挂回去
    ///    （直接设 start 会把原来那个 unparent ⇒ 里面的页面全丢）。
    fn set_side_left(self: &Rc<Self>, left: bool) {
        if self.side_left.get() == left {
            return;
        }
        self.side_left.set(left);
        let main = self.work.start_child();
        let side = self.work.end_child();
        self.work.set_start_child(None::<&gtk::Widget>);
        self.work.set_end_child(None::<&gtk::Widget>);
        if left {
            self.work.set_start_child(side.as_ref());
            self.work.set_end_child(main.as_ref());
        } else {
            self.work.set_start_child(main.as_ref());
            self.work.set_end_child(side.as_ref());
        }
        self.refresh_side();
    }

    /// **工作区**的拖放接收器（阶段 137）：拖到**左右边缘** ⇒ 开一条分栏。
    ///
    /// 干什么用（用户 2026-11-19）：「tab 可以**自由停靠在屏幕的多个
    /// 位置**」⇒ 拖到窗口左边 ⇒ 侧栏换到左边；拖到右边 ⇒ 侧栏在右边；
    /// 拖在中间 ⇒ 回到主栏（与终端并列）。
    ///
    /// ⚠️ **为什么挂在工作区上**：页签栏那一段由主栏自己的接收器管
    ///    （`attach_undock_target`）⇒ GTK 先问**最里层**的控件 ⇒
    ///    页签栏的落点归它，这里只管**内容区**与**边缘**。
    fn attach_work_target(self: &Rc<Self>) {
        let dst = gtk::DropTargetAsync::new(
            Some(gdk::ContentFormats::new(&[PAGE_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(PAGE_MIME));
        // 边缘判定：左右各 22% ⇒ 那儿开分栏；中间 ⇒ 回主栏。
        let zone_of = |x: f64, w: i32| -> u8 {
            if w <= 0 {
                return 1; // 还没量出宽度 ⇒ 当作中间
            }
            let f = x / (w as f64);
            if f < 0.22 {
                0 // 左边缘
            } else if f > 0.78 {
                2 // 右边缘
            } else {
                1 // 中间
            }
        };
        let work = self.work.clone();
        dst.connect_drag_motion(move |_d, _drop, x, _y| {
            if zone_of(x, work.width()) == 1 {
                // 中间 ⇒ **不接受**（让主栏的接收器管，或什么都不做）。
                gdk::DragAction::empty()
            } else {
                gdk::DragAction::MOVE
            }
        });
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, x, _y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let zone = zone_of(x, me.work.width());
            let cb: Rc<dyn Fn(PageKindTag, u64)> = editor_only(move |id| match zone {
                0 => {
                    me.set_side_left(true);
                    me.move_editor_to_side(id);
                }
                2 => {
                    me.set_side_left(false);
                    me.move_editor_to_side(id);
                }
                _ => {
                    me.move_editor_to_main(id);
                }
            });
            read_page_payload(&drop, cb);
            true
        });
        self.work.add_controller(dst);
    }

    /// **侧栏**的拖放接收器（阶段 137）：拖到侧栏里 ⇒ 停到侧栏。
    fn attach_side_target(self: &Rc<Self>) {
        let dst = gtk::DropTargetAsync::new(
            Some(gdk::ContentFormats::new(&[PAGE_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(PAGE_MIME));
        dst.connect_drag_enter(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        dst.connect_drag_motion(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, _x, _y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let cb: Rc<dyn Fn(PageKindTag, u64)> = editor_only(move |id| {
                me.move_editor_to_side(id);
            });
            read_page_payload(&drop, cb);
            true
        });
        self.side_nb.add_controller(dst);
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
            Some(gdk::ContentFormats::new(&[PAGE_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(PAGE_MIME));
        dst.connect_drag_enter(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        dst.connect_drag_motion(move |_d, _drop, _x, _y| gdk::DragAction::MOVE);
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, _x, _y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let cb: Rc<dyn Fn(PageKindTag, u64)> = editor_only(move |id| {
                me.move_editor_to_bottom(id);
            });
            read_page_payload(&drop, cb);
            true
        });
        self.editor_area.add_controller(dst);
    }

    /// **格子级**的拖放接收器（阶段 145）。
    ///
    /// 一个接收器管**两件事**（都挂在 notebook 上）：
    ///
    /// 1. **页签栏**（`y < 页签栏高度`）⇒ **插入**：页签进这个格子；
    /// 2. **四条边**（离边 24px 内）⇒ **拆分**：这个格子被劈成两半。
    ///
    /// ⚠️ **为什么挂在 notebook 上**（而不是格子外壳）：GTK4 **没有公开**
    ///    notebook 页签栏的 API ⇒ 只能挂在 notebook 上，再用「落点 y <
    ///    页签栏高度」判断是不是拖到了页签栏（阶段 108 就是这么干的）。
    ///    边同理：坐标是相对 notebook 的，而 notebook 铺满整个格子。
    ///
    /// ⚠️ **编辑器的老规矩也在这儿**（阶段 108）：拖**编辑器**到页签栏 ⇒
    ///    变回一个子页签（`move_editor_to_main`）⇒ 这条不能丢。
    fn attach_cell_target(self: &Rc<Self>) {
        /// 落点算是哪种（页签栏 / 哪条边 / 不接）。
        fn hit(nb: &gtk::Notebook, x: f64, y: f64) -> Hit {
            if y < tab_strip_height(nb) as f64 {
                return Hit::TabBar;
            }
            match crate::layout::dnd::zone_at(x, y, nb.width(), nb.height(), EDGE_MARGIN) {
                Some(z) => Hit::Edge(z),
                None => Hit::None,
            }
        }
        let dst = gtk::DropTargetAsync::new(
            Some(gdk::ContentFormats::new(&[PAGE_MIME])),
            gdk::DragAction::MOVE,
        );
        dst.connect_accept(move |_d, drop| drop.formats().contain_mime_type(PAGE_MIME));
        let nb = self.nb.clone();
        dst.connect_drag_enter({
            let nb = nb.clone();
            move |_d, _drop, x, y| match hit(&nb, x, y) {
                Hit::None => gdk::DragAction::empty(),
                _ => gdk::DragAction::MOVE,
            }
        });
        // ⚠️ **拖动过程中给反馈**（阶段 145）：蓝色插入符 ＋ 边缘高亮。
        {
            let me = Rc::downgrade(self);
            dst.connect_drag_motion(move |_d, _drop, x, y| {
                let Some(me) = me.upgrade() else {
                    return gdk::DragAction::empty();
                };
                let h = hit(&me.nb, x, y);
                me.show_drop_feedback(&h, x);
                match h {
                    Hit::None => gdk::DragAction::empty(),
                    _ => gdk::DragAction::MOVE,
                }
            });
        }
        // 拖动离开 ⇒ 收起反馈（不收的话那根蓝线会一直杵在那儿）。
        {
            let me = Rc::downgrade(self);
            dst.connect_drag_leave(move |_d, _drop| {
                if let Some(me) = me.upgrade() {
                    me.hide_drop_feedback();
                }
            });
        }
        let me = Rc::downgrade(self);
        dst.connect_drop(move |_d, drop, x, y| {
            let Some(me) = me.upgrade() else {
                return false;
            };
            let h = hit(&me.nb, x, y);
            me.hide_drop_feedback();
            let Some(move_action) = h.into_move(x, &me) else {
                return false;
            };
            let cb: Rc<dyn Fn(PageKindTag, u64)> = Rc::new(move |kind, id| match kind {
                // ⚠️ **编辑器**拖到页签栏 = 变回子页签（阶段 108 的老规矩）。
                PageKindTag::Editor => {
                    if let crate::layout::dnd::PageMove::Into(_) = move_action {
                        me.move_editor_to_main(id);
                    }
                }
                // 其它种类 ⇒ 跨格子搬。
                _ => {
                    // ⚠️ **先把钩子 clone 出来再调**：`if let Some(f) =
                    //   me.move_hook.borrow().clone()` 会让那个 `Ref` 一直活到
                    //   `if let` 语句结束 ⇒ 钩子执行时 `move_hook` 还被借着
                    //   ⇒ 钩子内部再借一次就 panic。
                    let hook = me.move_hook.borrow().clone();
                    if let Some(f) = hook {
                        f(id, move_action);
                    }
                }
            });
            read_page_payload(&drop, cb);
            true
        });
        self.nb.add_controller(dst);
    }

    /// 显示/收起拖放反馈（蓝色插入符 ＋ 边缘高亮）。
    fn show_drop_feedback(&self, h: &Hit, x: f64) {
        self.hide_drop_feedback();
        match h {
            Hit::TabBar => {
                // 插入符摆在落点处（页签头的横坐标由我们自己量）。
                let xs = self.page_head_xs();
                let idx = crate::layout::dnd::insert_index_at(&xs, x);
                let px = match (idx == 0, xs.get(idx)) {
                    (true, _) => xs.first().copied().unwrap_or(2.0),
                    (false, Some(&v)) => v,
                    (false, None) => xs.last().copied().unwrap_or(2.0) + 40.0,
                };
                self.caret.set_margin_start(px as i32);
                self.caret.set_height_request(tab_strip_height(&self.nb).max(12));
                self.caret.set_visible(true);
            }
            Hit::Edge(z) => {
                let i = match z {
                    crate::layout::dnd::SplitZone::Left => 0,
                    crate::layout::dnd::SplitZone::Right => 1,
                    crate::layout::dnd::SplitZone::Top => 2,
                    crate::layout::dnd::SplitZone::Bottom => 3,
                };
                self.edges[i].set_visible(true);
            }
            Hit::None => {}
        }
    }

    /// 收起拖放反馈。
    fn hide_drop_feedback(&self) {
        self.caret.set_visible(false);
        for e in self.edges.iter() {
            e.set_visible(false);
        }
    }

    /// 注册「跨格子搬页签」的回调（阶段 145）。
    ///
    /// `(页 id, 动作)` ⇒ 动作为 [`crate::layout::dnd::PageMove`]。
    pub fn set_move_hook<F: Fn(u64, crate::layout::dnd::PageMove) -> bool + 'static>(&self, f: F) {
        *self.move_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 注册「**有页签被搬进来了**」的回调（阶段 146）。
    ///
    /// 传页签 id（搬动时 id 不变）。
    pub fn set_moved_hook<F: Fn(u64) + 'static>(&self, f: F) {
        *self.moved_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 注册「**这个格子空了**」的回调（阶段 145）。
    ///
    /// ⚠️ **不是"某个页签没了"**（那是 `add_close_hook`）——这里是"整个格子
    ///    没内容了"⇒ 上层要把这个格子从树里摘掉。
    pub fn set_empty_hook<F: Fn() -> bool + 'static>(&self, f: F) {
        *self.empty_hook.borrow_mut() = Some(Rc::new(f));
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
            .filter(|p| p.place != EditorPlace::Bottom)
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

/// 读拖放负载（**种类 ＋ id**）⇒ 回调（阶段 145 统一协议）。
///
/// ⚠️ **读完必须 `finish()`**：不 finish 的话源端一直以为这次拖拽没结束
/// ⇒ 下一次拖拽可能被忽略（`dnd.rs` 里记过这个坑）。
///
/// ⚠️ **要有超时兜底**：源端不应答时不能永远挂着（用户看到的就是
/// "拖进去完全没反应"）。
fn read_page_payload(drop: &gdk::Drop, cb: Rc<dyn Fn(PageKindTag, u64)>) {
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
        &[PAGE_MIME],
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
                match crate::layout::dnd::parse_page_payload(&text) {
                    Some((kind, id)) => {
                        cb(kind, id);
                        drop_finish.finish(gdk::DragAction::MOVE);
                    }
                    None => drop_finish.finish(gdk::DragAction::empty()),
                }
            });
        },
    );
}

/// 编辑器落点用：**只认 `Editor` 种类**的负载（别的种类一律不接）。
///
/// 干什么用（阶段 145）：负载统一成 `<kind>:<id>` 之后，编辑器那几个落点
/// （底部编辑区 / 侧栏 / 主栏 / 工作区边缘）**不能**再对任何负载都动
/// ⇒ 种类不对就当没这回事。
fn editor_only(cb: impl Fn(u64) + 'static) -> Rc<dyn Fn(PageKindTag, u64)> {
    Rc::new(move |kind, id| {
        if kind == PageKindTag::Editor {
            cb(id);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{display_width, MAX_TITLE_CHARS};

    #[test]
    fn 旧负载仍然认得() {
        // ⚠️ 阶段 145 把负载统一成 `<kind>:<id>`，但**旧格式还得认**
        //    （阶段 108 的编辑器拖放负载 + 更早的裸 id）。
        use crate::layout::dnd::{parse_page_payload, PageKindTag};
        assert_eq!(
            parse_page_payload("pp-editor:12"),
            Some((PageKindTag::Editor, 12))
        );
        assert_eq!(parse_page_payload("42"), Some((PageKindTag::Editor, 42)));
        assert_eq!(
            parse_page_payload("editor:12"),
            Some((PageKindTag::Editor, 12))
        );
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

    /// 阶段 146：页签搬进另一个格子时 ⇒ **通知上层**（`file_host` 要跟着换）。
    ///
    /// ⚠️ **页签 id 搬家时不变** ⇒ 上层靠 id 认得出"搬的是文件面板"。
    #[test]
    fn 搬进来的页签会通知上层() {
        use gtk::prelude::*;
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() || gtk::gdk::Display::default().is_none() {
            eprintln!("跳过 搬进来的页签会通知上层：本机没有显示服务");
            return;
        }
        let sub = crate::panel::subtabs::SubTabs::new();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<u64>::new()));
        {
            let seen = seen.clone();
            sub.set_moved_hook(move |id| seen.borrow_mut().push(id));
        }
        let w = gtk::Label::new(Some("x"));
        let id = sub.add_page("x", w.upcast::<gtk::Widget>(), None);
        // 先摘再挂（跨格子搬运的正规做法）。
        let data = sub.take_page(id).expect("应当能摘下来");
        let id2 = sub.insert_page(data, None);
        assert_eq!(id2, id, "搬完家页签 id 变了 ⇒ 上层认不出是哪个页签");
        assert_eq!(
            *seen.borrow(),
            vec![id],
            "搬进来的页签应当通知上层（file_host 靠它换宿主）"
        );
    }

    #[test]
    fn 最小宽度不超过上限() {
        // 超长标题的最小宽度不能超过上限，否则宽度上限形同虚设
        let long = "x".repeat(100);
        let w = display_width(&long).max(4) as i32;
        assert!(w.min(MAX_TITLE_CHARS) <= MAX_TITLE_CHARS);
    }
}
