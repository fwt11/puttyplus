//! 界面密度与界面字号（4.5）。
//!
//! **用 CSS 而不是改代码**：`set_margin_*` / `set_size_request` 在本项目里
//! 有 119 处，逐个改成"按密度取值"既易错又难 review。GTK 的正经做法是给
//! 窗口挂一个 CSS 类，用规则统一压 padding / min-height —— 一处规则覆盖
//! 全树，改密度不用碰任何业务代码。
//!
//! **与主题正交**：紧凑模式的 CSS 走**独立的 CssProvider**，哪怕主题是
//! "跟随系统"（那套不注入任何 CSS）紧凑模式照样生效。两者各管各的，跟
//! 终端配色一样。
//!
//! **界面字号与终端字号互不影响**：界面走 `gtk-xft-dpi`（GTK 全局字体
//! 缩放），终端字号是 1.5 那个会话级覆盖（绝对磅值，不吃 xft-dpi）。
//! 两边独立，正是这里要的效果。

use gtk::gdk;
use gtk::prelude::*;

/// 界面密度。
///
/// ⚠️ **三档**（阶段 142）：标准 / 紧凑 / 超紧凑。
///
/// **默认是「紧凑」**（用户 2026-11-21 拍板）：这个界面信息密度很高，
/// 标准档的留白在 1366×768 的笔记本上吃掉太多可视面积。
/// ⇒ **老用户已显式存过值的设置不动**（见 [`density`]），
/// 只有「从没碰过这个设置」的才走紧凑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Normal,
    Compact,
    /// **超紧凑**：在紧凑基础上再压一档（给极客 / 小屏）。
    ExtraCompact,
}

impl Density {
    /// 三档的全集（设置界面按这个顺序摆分段控件）。
    pub const ALL: [Density; 3] = [Density::Normal, Density::Compact, Density::ExtraCompact];

    pub fn label(self) -> &'static str {
        match self {
            Density::Normal => tr("标准"),
            Density::Compact => tr("紧凑"),
            Density::ExtraCompact => tr("超紧凑"),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Density::Normal => tr("常规留白，工具栏与文件列表舒展"),
            Density::Compact => tr("压缩留白，同屏显示更多文件行"),
            Density::ExtraCompact => tr("最小留白，一屏能看到的行数最多"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Density::Normal => "normal",
            Density::Compact => "compact",
            Density::ExtraCompact => "xcompact",
        }
    }

    pub fn parse(s: &str) -> Density {
        match s {
            "compact" => Density::Compact,
            "xcompact" => Density::ExtraCompact,
            _ => Density::Normal,
        }
    }

    /// 这个密度要挂的 CSS 类名（**超紧凑同时带紧凑的类**）。
    ///
    /// ⚠️ **为什么超紧凑要同时带 `pp-compact`**：CSS 是"越往后越具体"，
    /// 超紧凑那一档只写**比紧凑更极端的差异**（几行），
    /// 不用把紧凑的规则抄一遍 ⇒ 以后加规则只改一处。
    pub fn css_classes(self) -> &'static [&'static str] {
        match self {
            Density::Normal => &[],
            Density::Compact => &["pp-compact"],
            Density::ExtraCompact => &["pp-compact", "pp-xcompact"],
        }
    }
}

/// 界面字号档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiFont {
    Small,
    #[default]
    Normal,
    Large,
}

impl UiFont {
    pub const ALL: [UiFont; 3] = [UiFont::Small, UiFont::Normal, UiFont::Large];

    pub fn label(self) -> &'static str {
        match self {
            UiFont::Small => tr("小"),
            UiFont::Normal => tr("中"),
            UiFont::Large => tr("大"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            UiFont::Small => "small",
            UiFont::Normal => "normal",
            UiFont::Large => "large",
        }
    }

    pub fn parse(s: &str) -> UiFont {
        match s {
            "small" => UiFont::Small,
            "large" => UiFont::Large,
            _ => UiFont::Normal,
        }
    }

    /// 相对 96dpi 的缩放倍率。
    pub fn scale(self) -> f64 {
        match self {
            UiFont::Small => 0.9,
            UiFont::Normal => 1.0,
            UiFont::Large => 1.15,
        }
    }
}

/// 紧凑模式的 CSS 规则。挂在主窗口上的 `pp-compact` 类下。
///
/// 只压**留白与控件高度**，不动字号、不动颜色——字号归 `gtk-xft-dpi` 管，
/// 颜色归主题管，三者各司其职。
///
/// ⚠️ **阶段 142 补齐了一批"漏网"元素**（UI 普查结论）：
/// 原先这 8 组规则压不到下列元素 ⇒ 紧凑模式"看起来没怎么变"：
/// - **`headerbar`（顶栏）**：`theme.rs` 里**根本没有** `headerbar` 的
///   padding 规则 ⇒ 用的是 Adwaita 默认值 ⇒ 压不动。
/// - **`.dialog-action-area` 及其按钮**：按钮 `min-width: 64px` 压不掉
///   （紧凑只设了 padding / min-height）。
/// - **`button.pp-menu-item`**：紧凑只覆盖了 `modelbutton` / `menuname`，
///   **漏了这条**（自绘右键菜单项全是它）。
/// - `.pp-card` / `.pp-empty-inner` / `.pp-chip` / `.pp-crumb` /
///   `.pp-toolbar` / `.pp-findbar` / `.pp-segmented` / `.pp-plus` /
///   `button.pp-recent-card` / `listbox > row` / `separator.pp-menu-sep`。
const COMPACT_CSS: &str = r#"
.pp-compact button,
.pp-compact .circular,
.pp-compact .flat,
.pp-compact modelbutton,
.pp-compact menuname {
  padding: 1px 6px;
  min-height: 24px;
}

.pp-compact entry,
.pp-compact spinbutton {
  padding: 1px 6px;
  min-height: 24px;
}

/* ⚠️ **文件列表的行高不在 CSS 里**（阶段 142 实测）：
   GTK 4.18 的 TreeView **行高由单元格渲染器自己的 padding 决定**，
   而 `treeview.view cell` 这个 **CSS 节点压根不存在**
   （横向 padding 也不生效 ⇒ 以前那条规则是**空转**的）。
   ⇒ 行高走代码层：`density::row_padding()`（名称列的自绘渲染器）。
   下面这条只管**列头**（列头按钮是真节点，`min-height` 能压下去）。 */
.pp-compact treeview.view header button {
  min-height: 20px;
  padding: 0 4px;
}

.pp-compact notebook > header {
  min-height: 28px;
}
.pp-compact notebook > header > tabs > tab {
  min-height: 26px;
  padding: 0 8px;
}

.pp-compact popover > contents,
.pp-compact menu > contents {
  padding: 2px;
}

/* 滚动条拖块：小一点，密排布局里才不挤地方 */
.pp-compact scrollbar slider {
  min-height: 20px;
  min-width: 20px;
}

/* ⚠️ **顶栏**（阶段 142 补）：theme.rs 里**没有** headerbar 的 padding
   规则 ⇒ 这里压的是 Adwaita 默认值 ⇒ 顶栏高度明显下来了。 */
.pp-compact headerbar {
  padding: 2px 6px;
  min-height: 32px;
}
.pp-compact headerbar .title {
  padding: 0 4px;
}

/* ⚠️ **顶栏菜单项**（阶段 143）：菜单栏标题是"入口"、字号不能压过正文
   ⇒ 这里只收**高度**（1.8em → 1.5em，约 32px），不动字号。 */
.pp-compact button.pp-menubar-item {
  min-height: 1.5em;
  padding: 0 3px;
}
.pp-xcompact button.pp-menubar-item {
  min-height: 1.4em;
  padding: 0 2px;
}

/* ⚠️ **对话框底部动作区**（阶段 142 补）：
   按钮的 `min-width: 64px` 是**最小宽度**——只设 padding 压不掉 ⇒
   这里必须显式 `min-width: 0`，否则「确定/取消」还是一排大按钮。 */
.pp-compact .dialog-action-area {
  padding: 6px 8px;
}
.pp-compact .dialog-action-area button {
  min-width: 0;
  min-height: 26px;
  padding: 3px 10px;
}

/* ⚠️ **自绘右键菜单项**（阶段 142 补）：紧凑只覆盖了 GTK 原生的
   `modelbutton` / `menuname`，**漏了 `button.pp-menu-item`** ⇒
   右键菜单在紧凑档下还是老高度。 */
.pp-compact button.pp-menu-item {
  padding: 2px 8px;
  min-height: 1.5em;
}

/* 卡片 / 空态 / 徽标：留白大户 */
.pp-compact .pp-card {
  padding: 6px 8px;
}
.pp-compact .pp-empty-inner {
  padding: 10px 12px;
}
.pp-compact .pp-chip {
  padding: 1px 6px;
}
.pp-compact .pp-crumb {
  padding: 0 6px;
}

/* 工具条 / 查找条自身的内边距（里面的按钮另有一组规则） */
.pp-compact .pp-toolbar {
  padding: 2px 4px;
}
.pp-compact .pp-findbar {
  padding: 2px 6px;
}

/* 欢迎页的「最近连接」卡片：两行字 + 余量，压成 2.6em */
.pp-compact button.pp-recent-card {
  padding: 5px 8px;
  min-height: 2.6em;
}

/* ⚠️ **侧栏「＋新建」**（阶段 142 补）：18pt 的全角加号是**整个界面
   最粗大的单点**（约 30px 高）⇒ 压到 13pt。 */
.pp-compact .pp-plus {
  font-size: 13pt;
  min-height: 1.1em;
}

/* ListBox 的行（设置项、快捷键、转发规则、通知中心都在用） */
.pp-compact listbox > row,
.pp-compact list > row {
  padding: 1px 4px;
  min-height: 20px;
}

/* 菜单分组线：上下 5px 在紧凑档下太奢侈 */
.pp-compact separator.pp-menu-sep {
  margin: 2px 4px;
}

/* 分段控件 */
.pp-compact .pp-segmented {
  padding: 1px;
}

/* 开关 / 勾选框 / 单选框 */
.pp-compact switch,
.pp-compact checkbutton,
.pp-compact radiobutton {
  min-height: 20px;
}

/* ⚠️ **阶段 142：代码层留白迁到 CSS 之后，紧凑档才有办法压它们**。
   （以前这些 margin 是 `set_margin_*` 写死的 ⇒ CSS 压不动 ⇒
   「紧凑模式切了跟没切一样」。现在基础值在 theme.rs，这里是覆盖。） */
.pp-compact .pp-form-box { margin: 8px; }
.pp-compact .pp-settings-root { margin: 8px 9px; }
.pp-compact .pp-setting-row { margin: 3px 6px; }
.pp-compact .pp-pane-toolbar { margin: 3px 2px 2px 3px; }
.pp-compact .pp-panel-head { margin: 3px 3px 1px 4px; }
.pp-compact .pp-term-status { margin: 2px 4px 1px 4px; }
.pp-compact .pp-sidebar { margin: 4px 2px 4px 3px; }
.pp-compact .pp-sidebar-head { margin: 0 1px; }
.pp-compact .pp-dialog-root { margin: 7px 8px; }
"#;

/// **超紧凑**档的 CSS 规则。挂在 `pp-xcompact` 类下。
///
/// ⚠️ **只写「比紧凑更极端的差异」**（不重复紧凑的规则）：
/// `pp-xcompact` 与 `pp-compact` **同时**挂在窗口上 ⇒
/// 紧凑的规则自动生效，这里只覆盖最要紧的几条。
const XCOMPACT_CSS: &str = r#"
.pp-xcompact button,
.pp-xcompact .circular,
.pp-xcompact .flat,
.pp-xcompact modelbutton,
.pp-xcompact menuname {
  padding: 0 5px;
  min-height: 20px;
}

.pp-xcompact entry,
.pp-xcompact spinbutton {
  padding: 0 5px;
  min-height: 20px;
}

/* ⚠️ **行高不在 CSS 里**（见紧凑档那段注释）——
   `treeview.view cell` 节点在 GTK 4.18 不存在，别再写这种规则了。 */

/* 列头：比紧凑档再矮 2px（`min-height` 才是有效的那个属性） */
.pp-xcompact treeview.view header button {
  min-height: 18px;
  padding: 0 2px;
}

.pp-xcompact headerbar {
  padding: 1px 4px;
  min-height: 26px;
}

.pp-xcompact notebook > header {
  min-height: 24px;
}
.pp-xcompact notebook > header > tabs > tab {
  min-height: 22px;
  padding: 0 6px;
}

.pp-xcompact .dialog-action-area {
  padding: 4px 6px;
}
.pp-xcompact .dialog-action-area button {
  min-height: 22px;
  padding: 2px 8px;
}

.pp-xcompact button.pp-menu-item {
  padding: 1px 6px;
  min-height: 1.4em;
}

.pp-xcompact .pp-plus {
  font-size: 12pt;
}

.pp-xcompact listbox > row,
.pp-xcompact list > row {
  padding: 0 3px;
  min-height: 18px;
}

.pp-xcompact .pp-empty-inner {
  padding: 6px 8px;
}

/* 代码层留白（阶段 142 迁到 CSS 的那批）在超紧凑档再压一档 */
.pp-xcompact .pp-form-box { margin: 5px; }
.pp-xcompact .pp-settings-root { margin: 5px 6px; }
.pp-xcompact .pp-setting-row { margin: 1px 4px; }
.pp-xcompact .pp-pane-toolbar { margin: 2px 1px 1px 2px; }
.pp-xcompact .pp-panel-head { margin: 2px 2px 0 3px; }
.pp-xcompact .pp-term-status { margin: 1px 3px 0 3px; }
.pp-xcompact .pp-sidebar { margin: 3px 1px 3px 2px; }
.pp-xcompact .pp-sidebar-head { margin: 0; }
.pp-xcompact .pp-dialog-root { margin: 4px 5px; }
"#;

/// 独立于主题的 provider：主题选"跟随系统"时紧凑模式照样有效。
struct Loaded {
    provider: gtk::CssProvider,
}

thread_local! {
    static LOADED: RefCell<Option<Loaded>> = const { RefCell::new(None) };
    /// 当前密度（缓存：watcher 每 150ms 用一次，不能每次都读盘）
    static CURRENT: std::cell::Cell<Density> = const { std::cell::Cell::new(Density::Normal) };
    /// 紧凑模式下的"新窗口自动收紧"巡检器
    static WATCH: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

use std::cell::RefCell;
use crate::i18n::tr;

/// 给单个窗口套/摘密度类。
fn tag_with(widget: &gtk::Widget, d: Density) {
    // ⚠️ **先摘后挂**（阶段 142）：三档会共用 `pp-compact`
    // ⇒ 换档时必须先把**旧档的类**摘干净，否则会出现
    // 「超紧凑的类还在、但密度已经退回标准」的鬼状态。
    for c in ["pp-compact", "pp-xcompact"] {
        if widget.has_css_class(c) {
            widget.remove_css_class(c);
        }
    }
    for c in d.css_classes() {
        if !widget.has_css_class(c) {
            widget.add_css_class(c);
        }
    }
}

/// 给任意窗口补一次密度类（对话框自己也可以主动调；不调也行，巡检器会补）。
pub fn tag<W: IsA<gtk::Window>>(w: &W) {
    let win: &gtk::Window = w.upcast_ref();
    let widget: &gtk::Widget = win.upcast_ref();
    tag_with(widget, CURRENT.with(|c| c.get()));
}

/// 显式登记一个窗口（可选；保留给主窗，语义上"这是我们的主窗口"）。
pub fn set_window(_w: gtk::Window) {}

/// 紧凑模式下，新弹出的对话框也要跟着收紧——GTK4 没有"窗口创建"的全局钩子，
/// 所以用一个 150ms 的巡检器给还没套类的顶层窗口补上（只在紧凑模式运行，
/// 顶层窗口通常只有个位数，开销可以忽略）。
fn start_watch() {
    WATCH.with(|w| {
        if w.borrow().is_some() {
            return;
        }
        let id = gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), || {
            let d = CURRENT.with(|c| c.get());
            if d != Density::Compact {
                return gtk::glib::ControlFlow::Continue;
            }
            for win in gtk::Window::list_toplevels() {
                tag_with(&win, d);
            }
            gtk::glib::ControlFlow::Continue
        });
        *w.borrow_mut() = Some(id);
    });
}

fn stop_watch() {
    WATCH.with(|w| {
        if let Some(id) = w.borrow_mut().take() {
            id.remove();
        }
    });
}

/// 装载紧凑模式的 CSS（幂等）。启动时调一次。
pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    LOADED.with(|l| {
        if l.borrow().is_some() {
            return;
        }
        let p = gtk::CssProvider::new();
        // ⚠️ **必须一次装完整段**（阶段 142 实测踩坑）：
        // `load_from_string` 是**替换**语义 —— 连着调两次的话，
        // 第二段会把第一段**整个盖掉**（不是追加！）⇒ 紧凑档那
        // 大半规则整段丢失，界面看起来"根本没变紧"。
        p.load_from_string(&all_css_text());
        gtk::StyleContext::add_provider_for_display(&display, &p, PRIORITY);
        *l.borrow_mut() = Some(Loaded { provider: p });
    });
}

/// 密度 CSS 的优先级。
///
/// ⚠️ **必须高于主题**（`theme::PRIORITY_USER` = 800）——
/// GTK4 里**跨 provider 不比特异性**：同一个属性由**优先级最高的那个
/// provider** 说了算，哪怕它的选择器更笼统。
///
/// 而主题的 `BASE` 里有一条 `treeview.view cell { padding: 4px 7px }`
/// ⇒ 密度要是也用 800，就成了"谁后装谁赢"：启动时密度后装（碰巧赢），
/// 但**用户切一次主题**（主题 provider 会重新装一遍）就换成主题赢
/// ⇒ 紧凑档的文件列表行高**当场失效**。
pub const PRIORITY: u32 = 810;

/// 当前生效的密度（**不读盘**；watcher 与 tag 用）。
pub fn current() -> Density {
    CURRENT.with(|c| c.get())
}

/// 按当前密度折算一个**留白数值**（像素）。
///
/// ⚠️ **什么时候用**：CSS 压不到的地方 —— 比如
/// `CellRenderer::set_padding`（渲染器自己的内边距，
/// CSS 的 `cell { padding }` 管不到它）。
///
/// ⚠️ **什么时候别用**：`gtk::Box::set_spacing` 这种
/// **能改成 CSS 的**就别用它 —— CSS 是**立即生效**的，
/// 而这个函数的值是**控件建好时就定死**的
/// （用户改密度后要重建界面才生效）。
///
/// 折算规则：标准 = 原值，紧凑 = 一半，超紧凑 = 三分之一。
pub fn margin(n: i32) -> i32 {
    let scaled = match current() {
        Density::Normal => n,
        Density::Compact => n / 2,
        Density::ExtraCompact => n / 3,
    };
    scaled.max(0)
}

/// [`margin`] 的别名（语义上是"两个控件之间的距离"）。
pub fn spacing(n: i32) -> i32 {
    margin(n)
}

/// 按密度从三个值里挑一个（标准 / 紧凑 / 超紧凑）。
///
/// ⚠️ **什么时候用**：**代码层的固定尺寸**（列宽下限、控件最小尺寸…）——
/// 这些没法用 CSS 表达（或者 CSS 表达不了"这个控件的**下限**"）。
///
/// ⚠️ **跟 [`margin`] 一样**：值是**控件建好时定死**的 ⇒
/// 改密度后要**重建那个控件**才生效。
pub fn pick(normal: i32, compact: i32, xcompact: i32) -> i32 {
    match current() {
        Density::Normal => normal,
        Density::Compact => compact,
        Density::ExtraCompact => xcompact,
    }
}

/// 文件列表**每行**的纵向内边距（**每边**，像素）。
///
/// ⚠️ **为什么行高只能走代码层**（阶段 142 实测，GTK 4.18.6）：
/// - TreeView 的**行高由单元格渲染器自己的 padding 决定**；
/// - CSS 的 `treeview.view cell { padding }` **压不动** ——
///   那个 `cell` 节点在 GTK 4.18 **压根不存在**（横向也不生效）；
/// - 改 CSS 类、改渲染器 padding、`queue_resize()` 全都**不会**让
///   已经算好的行布局重排 ⇒ 只有"列变了"才会重算
///   （见 [`refresh_tree_rows`]）。
///
/// ⇒ 三档：标准每边 2px（行高 = 文字高 + 4）、紧凑 1px、超紧凑 0px。
pub fn row_padding() -> i32 {
    row_padding_for(current())
}

/// [`row_padding`] 的纯函数版（单测直接测它，不依赖当前档位）。
fn row_padding_for(d: Density) -> i32 {
    match d {
        Density::Normal => 2,
        Density::Compact => 1,
        Density::ExtraCompact => 0,
    }
}

/// 强制**所有树**重排行高（密度变了之后调）。
///
/// ⚠️ **为什么必须显式催一下**（阶段 142 实测）：
/// GTK 4.18 的 TreeView **一旦算出行布局就不会自己重排** ——
/// 改单元格渲染器的 padding、改 CSS 类、`queue_resize()`
/// **全都无效**（实测三种都不动）。
/// 只有**列发生变化**时 GTK 才重算行布局
/// ⇒ 这里加一列再删掉（当场发生，用户看不见）。
///
/// ⚠️ **为什么用"加一列再删掉"而不是"重设模型"**：
/// 重设模型会把**选中行与滚动位置**一起弄丢（用户正在看目录中间时
/// 切个密度，列表弹回顶部 ⇒ 体验很差）；加删列不动这两样。
fn refresh_tree_rows() {
    for win in gtk::Window::list_toplevels() {
        let root: gtk::Widget = win.upcast_ref::<gtk::Widget>().clone();
        let mut stack = vec![root];
        while let Some(w) = stack.pop() {
            if let Some(view) = w.downcast_ref::<gtk::TreeView>() {
                let tmp = gtk::TreeViewColumn::new();
                view.append_column(&tmp);
                view.remove_column(&tmp);
            }
            let mut child = w.first_child();
            while let Some(c) = child {
                stack.push(c.clone());
                child = c.next_sibling();
            }
        }
    }
}

/// 当前生效的密度（读盘）。
///
/// ⚠️ **缺省是「紧凑」**（阶段 142，用户 2026-11-21 拍板）：
/// 信息密度高的工作台，标准档的留白在小屏上太奢侈。
///
/// ⇒ 用 [`crate::settings::get_str_opt`] 而不是 `get_str`：
/// **区分「没存过」与「存了 normal」** ——
/// 后者是老用户显式选的标准档，**不许动**；
/// 前者才是"从没碰过这个设置"的 ⇒ 走紧凑。
pub fn density() -> Density {
    match crate::settings::get_str_opt("ui", "density") {
        Some(v) => Density::parse(&v),
        None => Density::Compact,
    }
}

pub fn ui_font() -> UiFont {
    UiFont::parse(&crate::settings::get_str("ui", "ui_font"))
}

/// 应用密度：给当前所有顶层窗口加/去 `pp-compact` 类，并开启/关闭巡检器。
pub fn apply_density(d: Density) {
    CURRENT.with(|c| c.set(d));
    for win in gtk::Window::list_toplevels() {
        tag_with(&win, d);
    }
    // ⚠️ **行高要单独催一下**（阶段 142）：CSS 改不动行高，
    //    而 GTK 的树算完行布局就不自己重排 ⇒ 显式让所有树重算一次。
    refresh_tree_rows();
    match d {
        Density::Normal => stop_watch(),
        // ⚠️ **紧凑与超紧凑都要巡检**（阶段 142）：巡检器是给新弹出的
        // 顶层窗口补密度类的 ⇒ 只有"有密度"的档位才需要它。
        Density::Compact | Density::ExtraCompact => start_watch(),
    }
}

/// 应用界面字号（gtk-xft-dpi 是 1024 为 96dpi 的定点数）。
pub fn apply_ui_font(f: UiFont) {
    let Some(s) = gtk::Settings::default() else { return };
    s.set_gtk_xft_dpi((96.0 * f.scale() * 1024.0).round() as i32);
}

pub fn set_density(d: Density) {
    crate::settings::set_str("ui", "density", d.key());
    apply_density(d);
}

pub fn set_ui_font(f: UiFont) {
    crate::settings::set_str("ui", "ui_font", f.key());
    apply_ui_font(f);
}

/// 启动时套用已保存的设置。
pub fn load_saved() {
    install();
    apply_density(density());
    apply_ui_font(ui_font());
}

/// 供 CSS 解析校验用（theme.rs 那条测试同款做法：GTK 只在 stderr 打警告，
/// 语法错了界面照样起得来）。
pub fn css_text() -> &'static str {
    COMPACT_CSS
}

/// **超紧凑**那段的 CSS 文本（同款用途）。
pub fn xcompact_css_text() -> &'static str {
    XCOMPACT_CSS
}

/// 两段合起来（**一个 provider 装的就是这个**）。
pub fn all_css_text() -> String {
    format!("{COMPACT_CSS}\n{XCOMPACT_CSS}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 密度键值往返() {
        for d in Density::ALL {
            assert_eq!(Density::parse(d.key()), d);
        }
        assert_eq!(Density::parse("不存在"), Density::Normal);
    }

    /// **超紧凑必须比紧凑更紧**（阶段 142）。
    ///
    /// ⚠️ 这条钉的是**方向**，不是具体数值 —— 数值允许在实施中微调，
    /// 但「超紧凑比宽松」这个关系不能反（反了就等于多一档没用的设置）。
    #[test]
    fn 超紧凑比紧凑更紧() {
        assert!(margin_for(Density::ExtraCompact, 12) < margin_for(Density::Compact, 12));
        assert!(margin_for(Density::Compact, 12) < margin_for(Density::Normal, 12));
        // 超紧凑也不能压到 0 以下
        assert_eq!(margin_for(Density::ExtraCompact, 1), 0);
    }

    /// 折算规则（纯函数，可直接测）。
    fn margin_for(d: Density, n: i32) -> i32 {
        match d {
            Density::Normal => n,
            Density::Compact => n / 2,
            Density::ExtraCompact => n / 3,
        }
        .max(0)
    }

    /// **超紧凑同时带紧凑的类**（阶段 142）—— 否则超紧凑会漏掉紧凑的规则。
    #[test]
    fn 超紧凑同时带紧凑的类() {
        assert!(Density::ExtraCompact.css_classes().contains(&"pp-compact"));
        assert!(Density::ExtraCompact.css_classes().contains(&"pp-xcompact"));
        assert!(Density::Compact.css_classes().contains(&"pp-compact"));
        assert!(!Density::Compact.css_classes().contains(&"pp-xcompact"));
        assert!(Density::Normal.css_classes().is_empty());
    }

    /// **每档"独占"的类不能重名**（阶段 142）。
    ///
    /// ⚠️ `pp-compact` 是**紧凑与超紧凑共用**的（超紧凑 = 紧凑 + 更极端，
    /// 见 [`Density::css_classes`]）⇒ 这里只查**独占类**：
    /// `pp-xcompact` 只能属于超紧凑。
    #[test]
    fn 密度独占类不重名() {
        let mut seen: Vec<&str> = Vec::new();
        for d in Density::ALL {
            for c in d.css_classes() {
                // 共用类（紧凑档引入、超紧凑也带）不参与重名检查
                if *c == "pp-compact" {
                    continue;
                }
                assert!(!seen.contains(c), "类名 {c} 被两档同时「独占」用了");
                seen.push(c);
            }
        }
        // 超紧凑那个独占类必须是它自己的
        assert!(seen.contains(&"pp-xcompact"));
    }

    /// **类名必须合法**（阶段 142）：以 `pp-` 开头、不含空格
    /// ⇒ 否则 CSS 规则压根匹配不上（而且不会报错，只是静默失效）。
    #[test]
    fn 密度类名合法() {
        for d in Density::ALL {
            for c in d.css_classes() {
                assert!(c.starts_with("pp-"), "类名 {c} 少了 pp- 前缀");
                assert!(!c.contains(' '), "类名 {c} 含空格（CSS 类名不能有）");
            }
        }
    }

    #[test]
    fn 字号键值往返() {
        for f in UiFont::ALL {
            assert_eq!(UiFont::parse(f.key()), f);
        }
        assert_eq!(UiFont::parse("不存在"), UiFont::Normal);
    }

    #[test]
    fn 字号档位单调且默认居中() {
        assert!(UiFont::Small.scale() < UiFont::Normal.scale());
        assert!(UiFont::Normal.scale() < UiFont::Large.scale());
        assert_eq!(UiFont::default(), UiFont::Normal);
    }

    #[test]
    fn xft_dpi_换算落在合理区间() {
        for f in UiFont::ALL {
            let dpi = (96.0 * f.scale() * 1024.0).round() as i32;
            // 90dpi 太小会糊、133dpi 太大像放大镜；越界说明换算写错了
            assert!(
                (82 * 1024..=140 * 1024).contains(&dpi),
                "{f:?} 算出 {dpi}，超出合理区间"
            );
        }
    }

    #[test]
    fn 紧凑_css_能被_gtk_真解析() {
        // ⚠️ **不能在这里 `gtk::init()`**（2026-11-23 修 Windows CI 卡死）：
        //   libtest 把每个测试跑在**各自的 worker 线程**上，而 GTK4 要求
        //   `gtk_init` **与控件创建都在主线程** ⇒ 在 worker 线程上初始化，
        //   **Windows 的 Win32 后端会永久阻塞**（`catch_unwind` 只防 panic，
        //   **防不住死锁**）⇒ v0.9.3 那轮 Windows CI 的单测卡了 109 分钟。
        //   ⇒ 单测里**只认「主线程已经初始化好了」**（worker 线程上永远是
        //   false ⇒ 干净地跳过）；**真跑这些检查的地方是 example 冒烟**
        //   （`examples/` 里的冒烟跑在主线程上 ⇒ 安全）。
        if !gtk::is_initialized_main_thread() {
            eprintln!("跳过紧凑_css_能被_gtk_真解析：本机没有显示服务");
            return;
        }
        let p = gtk::CssProvider::new();
        let errs: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let errs = errs.clone();
            p.connect_parsing_error(move |_, sec, err| {
                let line = sec.start_location().lines() + 1;
                errs.borrow_mut().push(format!("第 {line} 行：{err}"));
            });
        }
        p.load_from_string(css_text());
        assert!(errs.borrow().is_empty(), "紧凑 CSS 有解析错误：{:?}", errs.borrow());
    }

    /// **超紧凑那段 CSS 也要能被 GTK 真解析**（阶段 142）。
    #[test]
    fn 超紧凑_css_能被_gtk_真解析() {
        // ⚠️ **不能在这里 `gtk::init()`**（2026-11-23 修 Windows CI 卡死）：
        //   libtest 把每个测试跑在**各自的 worker 线程**上，而 GTK4 要求
        //   `gtk_init` **与控件创建都在主线程** ⇒ 在 worker 线程上初始化，
        //   **Windows 的 Win32 后端会永久阻塞**（`catch_unwind` 只防 panic，
        //   **防不住死锁**）⇒ v0.9.3 那轮 Windows CI 的单测卡了 109 分钟。
        //   ⇒ 单测里**只认「主线程已经初始化好了」**（worker 线程上永远是
        //   false ⇒ 干净地跳过）；**真跑这些检查的地方是 example 冒烟**
        //   （`examples/` 里的冒烟跑在主线程上 ⇒ 安全）。
        if !gtk::is_initialized_main_thread() {
            eprintln!("跳过超紧凑_css_能被_gtk_真解析：本机没有显示服务");
            return;
        }
        let p = gtk::CssProvider::new();
        let errs: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let errs = errs.clone();
            p.connect_parsing_error(move |_, sec, err| {
                let line = sec.start_location().lines() + 1;
                errs.borrow_mut().push(format!("第 {line} 行：{err}"));
            });
        }
        p.load_from_string(xcompact_css_text());
        assert!(errs.borrow().is_empty(), "超紧凑 CSS 有解析错误：{:?}", errs.borrow());
    }

    #[test]
    fn 紧凑_css_覆盖了主要控件() {
        let css = css_text();
        for sel in [
            "button",
            "entry",
            "notebook > header",
            "popover > contents",
            "scrollbar slider",
            // ⚠️ **阶段 142 补的漏网元素**（原先这批全都没压到 ⇒
            // 「紧凑模式看起来没怎么变」）：顶栏、对话框动作区与其按钮、
            // 自绘右键菜单项、卡片、空态、徽标、面包屑、工具条、查找条、
            // 欢迎页卡片、侧栏大加号、ListBox 行、菜单分组线、分段控件。
            "headerbar",
            ".dialog-action-area",
            ".dialog-action-area button",
            "button.pp-menu-item",
            ".pp-card",
            ".pp-empty-inner",
            ".pp-chip",
            ".pp-crumb",
            ".pp-toolbar",
            ".pp-findbar",
            "button.pp-recent-card",
            ".pp-plus",
            "listbox > row",
            "separator.pp-menu-sep",
            ".pp-segmented",
            "treeview.view header button",
        ] {
            assert!(css.contains(sel), "紧凑 CSS 漏了 {sel}");
        }
    }

    /// ⚠️ **不许再写 `treeview.view cell`**（阶段 142 实测，GTK 4.18）。
    ///
    /// GTK 4.18 的 TreeView **没有 `cell` 这个 CSS 节点** ——
    /// 写了**不报错、也不生效**（横向纵向都不生效）⇒
    /// 以前那条 `treeview.view cell { padding: 4px 7px }` 是**空转**的，
    /// 而单测只比对文本 ⇒ 一直是绿的。
    ///
    /// ⇒ 这里钉死：**行高只能走代码层**（`row_padding()`）。
    #[test]
    fn 密度_css_不许用不存在的_cell_节点() {
        for (name, css) in [("紧凑", css_text()), ("超紧凑", xcompact_css_text())] {
            // ⚠️ **先把注释去掉再查**：注释里可以（也应该）解释
            //    「为什么不用这个节点」⇒ 查的是**真规则**。
            let rules: String = css
                .split("/*")
                .map(|s| s.split_once("*/").map(|(_, r)| r).unwrap_or(s))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !rules.contains("treeview.view cell"),
                "{name} CSS 里又有 `treeview.view cell` 规则了 —— \
                 GTK 4.18 没有这个节点，写了也不生效（行高走 row_padding()）"
            );
        }
    }

    /// **密度 CSS 的优先级必须高于主题**（阶段 142）。
    ///
    /// GTK4 里**跨 provider 不比特特殊性**：同一个属性由**优先级最高的
    /// 那个 provider** 说了算 ⇒ 主题与密度同优先级的话，
    /// 「谁后装谁赢」⇒ 用户切一次主题，紧凑档的规则就被顶掉。
    #[test]
    fn 密度优先级高于主题() {
        assert!(
            PRIORITY > crate::theme::PRIORITY_USER,
            "密度 CSS 的优先级（{}）必须高于主题（{}）",
            PRIORITY,
            crate::theme::PRIORITY_USER
        );
    }

    /// **文件列表的行高逐档变矮**（阶段 142）。
    ///
    /// ⚠️ 这是**代码层**的值（GTK 4.18 的 CSS 压不动行高）。
    #[test]
    fn 行高随密度递减() {
        assert_eq!(row_padding_for(Density::Normal), 2);
        assert_eq!(row_padding_for(Density::Compact), 1);
        assert_eq!(row_padding_for(Density::ExtraCompact), 0);
    }
}
