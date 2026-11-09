//! 会话侧边栏：最近连接 / 自定义连接 / ssh config 别名 三组树，
//! 搜索过滤 + 双击（或回车）在新标签打开。
//!
//! 版式（阶段 93 重设计）：
//! - **组头** = 粗体标题 + 计数徽章（chip），一眼分得清"章节"和"条目"；
//! - **条目** = 名字（正文色）+ 副标题（小号淡色），中间有分隔点——
//!   以前只隔两个空格，名字和地址挤成一团（用户 2026-11-01：「有点丑」）。
//!
//! 地址以前只藏在 tooltip 里，等于没有。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::args::ConnOptions;
use crate::connmgr::{self, ConnEntry};
use crate::recent;
use crate::tabs::Tabs;
use crate::i18n::tr;
use crate::trf;

const COL_TEXT: i32 = 0;
const COL_ICON: i32 = 1;
/// 0=分组节点 1=最近项 2=连接项 3=数据库连接项 4=端口转发行（阶段 56）
const COL_KIND: i32 = 2;
/// 连接项在条目列表 / 最近项在 recent 列表 / 数据库连接项在 db 列表的索引
const COL_IDX: i32 = 3;
const COL_TIP: i32 = 4;
/// **主标题**（纯文本）：组头=分组名，条目=连接名 / 规则标签。
///
/// ⚠️ **为什么要单独存一份纯文本**：渲染用的 `COL_TEXT` 是 **markup**
/// （带 `<b>` / `<span …>` 标签）——拿它去过滤会把标签当内容搜
/// （搜 "span" 命中每一行！），而尖括号早被转义成 `&lt;` ⇒ 用户搜
/// 「<」又搜不到。主/副标题分开存，过滤和测试都干净。
const COL_NAME: i32 = 5;
/// **副标题**（纯文本）：地址 / 计数。与 [`COL_NAME`] 一起构成这一行。
const COL_SUB: i32 = 6;

/// 「最近」组的**组名**。
///
/// ⚠️ 既是**显示**用的名字，也是**判定**用的标识（组头右键有没有
/// 「清空最近连接」就看它）⇒ 用常量，别在两处各写一个字面量。
const RECENT_GROUP: &str = "最近";

/// 活着的侧边栏实例（主题切换后要重画行内灰字；weak 避免拖住窗口）。
thread_local! {
    static LIVE: RefCell<Vec<std::rc::Weak<Sidebar>>> = const { RefCell::new(Vec::new()) };
}

/// 让活着的侧边栏重读列表（连接增删改之后调用）。
pub fn refresh() {
    LIVE.with(|l| {
        let mut v = l.borrow_mut();
        v.retain(|w| {
            if let Some(sb) = w.upgrade() {
                sb.reload();
                true
            } else {
                false
            }
        });
    });
}

/// 主题切换后重刷侧边栏（行内地址用的是主题色 markup）。
pub fn refresh_theme() {
    refresh();
}

pub struct Sidebar {
    pub widget: gtk::Box,
    store: gtk::TreeStore,
    view: gtk::TreeView,
    filter: gtk::TreeModelFilter,
    plus_btn: gtk::Button,
    /// 头部「＋」的弹层（阶段 68）。
    ///
    /// **为什么要存着**：弹层是 `set_parent` 挂到按钮上的，**不会进容器的
    /// children 链** ⇒ 从控件树里找不到它 ⇒ 冒烟没法看菜单里到底有什么。
    plus_pop: RefCell<Option<gtk::Popover>>,
    refresh_btn: gtk::Button,
    /// 凭据库状态标志（阶段 77a）——冒烟要断言它在。
    vault_btn: gtk::Button,
    entries: RefCell<Vec<ConnEntry>>,
    recents: RefCell<Vec<recent::RecentConn>>,
    /// 数据库连接项：`(会话键, 连接名, 所属 SSH 条目下标)`
    /// ——侧边栏树上挂在对应 SSH 连接下面
    dbs: RefCell<Vec<(String, String, usize)>>,
    /// 端口转发项：`(所属 SSH 条目下标, 规则)`（阶段 56）
    /// ——跟数据库连接一样挂在对应 SSH 连接下面。
    /// 规则本体留着，右键菜单才能知道「是哪条规则、哪套机制」（阶段 58）。
    fwds: RefCell<Vec<(usize, connmgr::FwdLeaf)>>,
    /// **重入保护**：状态变化钩子会从别处调 `reload()`，重入就是二次借
    /// `entries` 的 RefCell → panic（阶段 57）。
    reloading: Cell<bool>,
    /// 空态面板（一条连接都没有时显示；阶段 80 / 审计 P2-1）。
    empty_box: gtk::Box,
    /// 树的滚动容器（空态时藏起来，反之亦然）。
    tree_sw: gtk::ScrolledWindow,
    /// 树里**图标渲染器**——测试用（阶段 93）。
    ///
    /// ⚠️ 为什么单独存：渲染器挂在 `TreeViewColumn` 上，而
    /// `TreeViewColumn` **不是 Widget** ⇒ 控件树里走不到 ⇒
    /// 冒烟没法量"图标与文字之间留没留空隙"。
    icon_renderer: gtk::CellRendererPixbuf,
    tabs: Rc<Tabs>,
}

/// 侧栏右键菜单的一项（纯逻辑，冒烟可断言）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowMenuItem {
    /// 分隔线
    Sep,
    /// 一个动作
    Act {
        label: &'static str,
        icon: &'static str,
    },
}

/// 右键菜单要显示哪些项的**全部上下文**（纯数据）。
#[derive(Clone)]
pub struct RowMenuCtx {
    /// 行类型：1=最近 2=连接条目 3=数据库连接 4=转发叶子。
    pub kind: i32,
    /// 自定义连接（ssh config 条目是只读的，没有编辑/删除）。
    pub custom: bool,
    /// 这一行是**数据库连接**（菜单里是「编辑/删除这个数据库连接」）。
    pub has_db: bool,
    /// 转发规则本体：`kind==4` 且**取得到**才有。
    ///
    /// ⚠️ 取不到就不摆「启用转发」——摆一个点了没反应的项比不摆更糟。
    pub fwd: Option<(ConnEntry, connmgr::FwdLeaf)>,
    /// 选中的是**组头**（kind==0）时的组名；不是组头就是 None。
    ///
    /// ⚠️ 组头没有 idx ⇒ [`Sidebar::selected_row`] 对它返回 None
    /// ⇒ 「最近」组的右键菜单（清空最近连接）得单独取。
    pub group: Option<String>,
}

/// 某一行的右键菜单**有哪些项**（纯函数，不碰 GTK）。
///
/// ⚠️ **为什么要抽出来**：侧栏的右键菜单是**每次右键重建**的（不是建
/// 一次反复用）⇒ 没法靠"这个控件在不在树里"来测 ⇒ 清单与实际显示的
/// 两份代码很容易漂移。现在 UI 照这份清单建、测试照这份清单断言，
/// **只有一份事实来源**。
///
/// 阶段 77a：「导出为 PuTTY 会话…」加在这里——连接管理器删掉后，
/// 导出**选中这一条**的入口只剩侧栏（文件菜单那个导出的是全部）。
pub fn row_menu_items(ctx: &RowMenuCtx) -> Vec<RowMenuItem> {
    use RowMenuItem::{Act, Sep};
    let mut v: Vec<RowMenuItem> = Vec::new();
    // **组头**（kind==0）：组头不是连接，不能点「连接」——
    // 以前这里会落进下面的 `_ => "连接"` ⇒ 右键组头弹出一个**点了没反应**
    // 的菜单。现在组头只给「最近」组一个清理动作。
    if ctx.kind == 0 {
        if ctx.group.as_deref() == Some(RECENT_GROUP) {
            v.push(Act {
                label: tr("清空最近连接"),
                icon: "edit-clear-all-symbolic",
            });
        }
        return v;
    }
    // 连接 / 打开数据库 / 打开端口转发（三行共用第一个项）
    v.push(Act {
        label: match ctx.kind {
            3 => tr("打开数据库"),
            4 => tr("打开端口转发"),
            _ => tr("连接"),
        },
        icon: "go-next-symbolic",
    });
    if ctx.fwd.is_some() {
        v.push(Act {
            label: tr("启用转发"),
            icon: "network-transmit-receive-symbolic",
        });
    }
    if ctx.kind == 2 {
        v.push(Act {
            label: tr("在新窗口打开"),
            icon: "window-new-symbolic",
        });
    }
    if ctx.custom {
        v.push(Sep);
        v.push(Act {
            label: tr("编辑…"),
            icon: "document-edit-symbolic",
        });
        v.push(Act {
            label: tr("删除"),
            icon: "edit-delete-symbolic",
        });
        v.push(Act {
            label: tr("导出为 PuTTY 会话…"),
            icon: "document-save-symbolic",
        });
    }
    if ctx.kind == 1 || ctx.kind == 2 {
        v.push(Sep);
        v.push(Act {
            label: tr("重新连接终端"),
            icon: "view-refresh-symbolic",
        });
        v.push(Act {
            label: tr("新建数据库连接…"),
            icon: "list-add-symbolic",
        });
    }
    if ctx.has_db {
        v.push(Sep);
        v.push(Act {
            label: tr("编辑这个数据库连接…"),
            icon: "document-edit-symbolic",
        });
        v.push(Act {
            label: tr("删除这个数据库连接"),
            icon: "edit-delete-symbolic",
        });
    }
    if ctx.kind == 1 {
        v.push(Sep);
        v.push(Act {
            label: tr("从最近中移除"),
            icon: "list-remove-symbolic",
        });
    }
    v.push(Sep);
    v.push(Act {
        label: tr("复制地址"),
        icon: "edit-copy-symbolic",
    });
    v
}

impl Sidebar {
    pub fn new(tabs: Rc<Tabs>) -> Rc<Sidebar> {
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
        box_.add_css_class("pp-sidebar");
        box_.set_margin_top(8);
        box_.set_margin_bottom(8);
        box_.set_margin_start(6);
        box_.set_margin_end(4);

        // ---- 标题 + 刷新 ----
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        head.set_margin_start(2);
        head.set_margin_end(2);
        let title = gtk::Label::new(None);
        // ⚠️ 阶段 81（审计 P2-8）：侧栏列的是**连接**（最近 / 自定义 /
        // ssh config 三组都是连接），叫「会话」跟菜单里的「连接」对不上
        // （用户得自己猜这俩是不是一回事）⇒ 统一叫「连接」。
        title.set_markup(tr("<b>连接</b>"));
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        // ⚠️ 阶段 93：标题要能**省略**。侧栏窄的时候（用户存的是 203px）
        // 标题栏里「连接 + ＋ + ⟳ + 凭据库 已锁定」一行放不下 ⇒ 整条
        // 侧栏的内容溢出，**左边被裁掉**（标题、树的展开箭头、图标全没了）。
        // 标题可省略 ⇒ 它不再撑住最小宽度，窄的时候先省略标题。
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // ⚠️ `width_chars` 是**最小**宽度（不是上限）：给 4 列 = 刚好
        // 放得下「连接」两个汉字（一个汉字占两列）⇒ 侧栏再窄，标题
        // 也还认得出来，而不是缩成一个「…」。
        title.set_width_chars(4);
        head.append(&title);

        // 阶段 68（审计 P1-2、P1-6）：侧栏的「＋」也是**同一个菜单**——
        // 动作表只有一份（`topmenus::NEW_ITEMS`）⇒ 从顶栏或侧栏进来，
        // 看到的东西一模一样。
        //
        // ⚠️ 用**普通 Button**（不是 `MenuButton`）：MenuButton 自带的小三角
        // 压不掉，而且它的弹层走 `set_popover()` ⇒ 内容一次性建好、上下文
        // 变了菜单不跟着变。普通 Button 自己 `popup()`，每次点都重建。
        // ⚠️ **不用 `list-add-symbolic`**（阶段 92）：symbolic 图标是细线条，
        // 浅色底上几乎看不见（用户 2026-11-01：「『＋』号是否可以搞得粗一点、
        // 明显一点」）。改用**全角文字加号**（`＋`，U+FF0B）——和欢迎页
        // 「＋ 新建连接…」同一个字形，又粗又显眼，各主题下都一样。
        let b_plus = gtk::Button::new();
        {
            let lbl = gtk::Label::new(Some(tr("＋")));
            lbl.add_css_class("pp-plus");
            b_plus.set_child(Some(&lbl));
        }
        b_plus.add_css_class("pp-plus-btn");
        b_plus.add_css_class("flat");
        b_plus.set_tooltip_text(Some(crate::newmenu::entry_tooltip(
            crate::topmenus::NewEntry::Sidebar,
        )));
        // 文字按钮的可访问名要靠 tooltip（图标按钮是 `icon_button` 自带的）
        crate::a11y::watch_tooltip(&b_plus);
        head.append(&b_plus);

        // 弹层**自己挂父**（普通 Button 的弹层不走 `set_popover()`）。
        // 侧栏在窗口左边 ⇒ 菜单往**右**开（往下开会盖住连接列表）。
        let plus_pop = crate::menu::popover(&b_plus);
        plus_pop.set_position(gtk::PositionType::Right);

        let b_refresh = crate::a11y::icon_button("view-refresh-symbolic");
        b_refresh.set_tooltip_text(Some(tr("重新加载连接列表")));
        b_refresh.add_css_class("flat");
        head.append(&b_refresh);

        // 凭据库状态标志（阶段 77a）：连接管理器删掉后，这个"我到底开没开
        // 凭据库"的常驻标志挪到**侧栏标题栏**——侧栏是配连接、勾「记住
        // 密码」的地方，状态在这儿一眼就该看见（原来它挂在连接管理器顶栏，
        // 而连接管理器本身跟侧栏是重复的）。
        let b_vault = crate::vaultui::badge_button();
        {
            b_vault.connect_clicked(move |btn| {
                // ⚠️ **点击时**才找父窗口：构造时侧栏还没挂到窗口上，
                // ancestor 那会儿是 None。
                let parent = btn
                    .ancestor(gtk::Window::static_type())
                    .and_downcast::<gtk::Window>();
                // ⚠️ **只解锁，不开凭据库管理器**（用户 2026-11-09）：
                // 「点了这个按钮，输入密码，如果正确了还会跳出一个对话框，
                // 显示所有凭据库里面有什么东西，这个没有必要。菜单里有个
                // 『凭据库』，那里显示就好了。」⇒ 徽标解锁完就变成
                // 「已解锁」的图标；管理器在菜单「凭据库…」里。
                crate::vaultui::badge_clicked(parent.as_ref(), crate::connmgr::load_all());
            });
        }
        head.append(&b_vault);

        box_.append(&head);

        // ---- 过滤框（带搜索图标 + 清空按钮）----
        let filter_entry = gtk::Entry::new();
        filter_entry.set_placeholder_text(Some(tr("过滤连接…")));
        filter_entry.set_icon_from_icon_name(
            gtk::EntryIconPosition::Primary,
            Some("system-search-symbolic"),
        );
        filter_entry.set_icon_from_icon_name(
            gtk::EntryIconPosition::Secondary,
            Some("edit-clear-symbolic"),
        );
        // 没内容时藏起清空按钮
        filter_entry.set_icon_sensitive(gtk::EntryIconPosition::Secondary, false);
        box_.append(&filter_entry);
        {
            let e = filter_entry.clone();
            e.connect_changed(move |e| {
                let has = !e.text().is_empty();
                e.set_icon_sensitive(gtk::EntryIconPosition::Secondary, has);
            });
        }
        {
            let e = filter_entry.clone();
            filter_entry.connect_icon_release(move |_, pos| {
                if pos == gtk::EntryIconPosition::Secondary {
                    e.set_text("");
                }
            });
        }

        let store = gtk::TreeStore::new(&[
            glib::Type::STRING, // TEXT（markup）
            glib::Type::STRING, // ICON
            glib::Type::I32,    // KIND
            glib::Type::I32,    // IDX
            glib::Type::STRING, // TIP
            glib::Type::STRING, // NAME（主标题，纯文本）
            glib::Type::STRING, // SUB（副标题，纯文本）
        ]);

        let filter = gtk::TreeModelFilter::new(&store, None);
        {
            let entry = filter_entry.clone();
            filter.set_visible_func(move |model, iter| {
                let text = entry.text().to_lowercase();
                if text.is_empty() {
                    return true;
                }
                // store.append 后、set 前会被 refilter 扫到，此时列值未初始化，
                // m.get 会 panic（UnexpectedNone）——必须用 get_value 容错读取
                //
                // ⚠️ 阶段 93：**搜纯文本的主/副标题，不搜 markup**。
                // 以前搜的是 COL_TEXT（markup）⇒ 搜 "span" 会命中每一行，
                // 搜 "<" 永远搜不到（已被转义成 `&lt;`）。
                fn row_text(m: &gtk::TreeModel, it: &gtk::TreeIter) -> String {
                    let n = m.get_value(it, COL_NAME);
                    let sub = m.get_value(it, COL_SUB);
                    let mut t = n.get::<String>().ok().unwrap_or_default();
                    t.push(' ');
                    t.push_str(&sub.get::<String>().ok().unwrap_or_default());
                    t
                }
                fn contains(m: &gtk::TreeModel, it: &gtk::TreeIter, q: &str) -> bool {
                    // 行里现在是 markup，过滤按纯文本比较更稳
                    if row_text(m, it).to_lowercase().contains(q) {
                        return true;
                    }
                    if let Some(mut child) = m.iter_children(Some(it)) {
                        loop {
                            if contains(m, &child, q) {
                                return true;
                            }
                            if !m.iter_next(&mut child) {
                                break;
                            }
                        }
                    }
                    false
                }
                contains(model, iter, &text)
            });
        }

        let view = gtk::TreeView::with_model(&filter);
        view.set_headers_visible(false);
        view.selection().set_mode(gtk::SelectionMode::Single);
        view.set_tooltip_column(COL_TIP);
        view.set_enable_tree_lines(false);

        let col = gtk::TreeViewColumn::new();
        // ⚠️ 阶段 93：**给行加内边距**。CellRenderer 的 padding 会把
        // **行高**撑开——以前行与行之间贴得死紧，二十来条看着像一堵墙。
        let r_icon = gtk::CellRendererPixbuf::new();
        // ⚠️ 阶段 93：**图标和文字之间要留空隙**。以前 padding 是 0
        // ⇒ 图标直接贴在字上（"🕐最近"），一眼看去挤成一坨。
        r_icon.set_padding(5, 3);
        col.pack_start(&r_icon, false);
        col.add_attribute(&r_icon, "icon-name", COL_ICON);
        let r_txt = gtk::CellRendererText::new();
        r_txt.set_ellipsize(gtk::pango::EllipsizeMode::End);
        r_txt.set_padding(0, 3);
        col.pack_start(&r_txt, true);
        col.add_attribute(&r_txt, "markup", COL_TEXT);
        view.append_column(&col);

        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        // 与文件列表一致：滚动条 overlay，不占会话名的宽度
        sw.set_overlay_scrolling(true);
        sw.add_css_class("pp-thin-scrollbar");
        sw.set_child(Some(&view));
        sw.set_vexpand(true);
        sw.set_hexpand(true);
        box_.append(&sw);

        // ---- 空态（一条连接都没有时）----
        //
        // ⚠️ 阶段 80 / 审计 P2-1：以前空配置时侧栏照样摆三个空组
        // （「最近 / 自定义 / ssh config」），看着像"功能坏了"。
        // 现在走**共用**的空态构件：标题 + 解释 + 一个主 CTA。
        let (empty_inner, empty_cta) = crate::emptystate::build(&crate::emptystate::sidebar());
        let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty_box.set_vexpand(true);
        empty_box.append(&empty_inner);
        {
            // 主 CTA：走**同一个**新建连接入口（顶栏「＋」也是它）
            let tabs = Rc::clone(&tabs);
            empty_cta.connect_clicked(move |_| {
                crate::tabs::TabsHandle(Rc::clone(&tabs)).prompt_new();
            });
        }
        box_.append(&empty_box);

        // 输入即过滤；过滤时自动展开组（否则命中子行藏在折叠组里点不到）
        {
            let filter2 = filter.clone();
            let view2 = view.clone();
            filter_entry.connect_changed(move |_| {
                filter2.refilter();
                view2.expand_all();
            });
        }

        let sb = Rc::new(Sidebar {
            widget: box_,
            store,
            view,
            filter,
            plus_btn: b_plus.clone(),
            plus_pop: RefCell::new(Some(plus_pop.clone())),
            refresh_btn: b_refresh.clone(),
            vault_btn: b_vault.clone(),
            entries: RefCell::new(Vec::new()),
            recents: RefCell::new(Vec::new()),
            dbs: RefCell::new(Vec::new()),
            fwds: RefCell::new(Vec::new()),
            reloading: Cell::new(false),
            empty_box,
            tree_sw: sw.clone(),
            icon_renderer: r_icon.clone(),
            tabs,
        });
        LIVE.with(|l| l.borrow_mut().push(Rc::downgrade(&sb)));
        sb
    }

    /// 完成需要 Rc 自引用的信号接线并装载初始数据。
    pub fn setup(self: &Rc<Self>) {
        // 双击 / 回车 → 新标签连接（打开后刷新最近列表）
        {
            let sb = Rc::clone(self);
            self.view.connect_row_activated(move |_v, path, _col| {
                let Some(child_path) = sb.filter.convert_path_to_child_path(path) else {
                    return;
                };
                let Some(it) = sb.store.iter(&child_path) else { return };
                let kind: i32 = sb.store.get(&it, COL_KIND);
                let idx: i32 = sb.store.get(&it, COL_IDX);
                sb.open_row(kind, idx as usize);
            });
        }

        self.install_row_menu();

        // 头部两个图标：新建 / 刷新。
        // 侧边栏不再放一整条"＋ 新建连接"大按钮——顶栏已经有全局主操作，
        // 列表底部留白给条目用；全局工具（私钥/凭据库/外观/关于）都在顶栏菜单。
        {
            let sb = Rc::clone(self);
            self.refresh_btn.connect_clicked(move |_| sb.reload());
        }
        {
            // ⚠️ **弱引用**（引用环红线）：弹层是按钮的子控件、侧栏在窗口里
            // ⇒ 闭包抓 `Rc<Tabs>` 强引用 = 窗口 → 侧栏 → 按钮 → 弹层 →
            // 闭包 → Tabs → notebook → 窗口，窗口永远回收不掉。
            let tw = Rc::downgrade(&self.tabs);
            // 弹层在 `new()` 里建好（记在字段里，冒烟要看它里面的内容）。
            let Some(pop) = self.plus_pop.borrow().clone() else {
                return;
            };
            self.plus_btn.connect_clicked(move |_| {
                let Some(t) = tw.upgrade() else { return };
                // 每次点都重建：有没有会话会变。
                let vb = crate::menu::body();
                // ⚠️ 上下文按当前窗口算（没有会话时相关项置灰）。
                let ctx = crate::newmenu::NewCtx::of(&t);
                let pop2 = pop.clone();
                let tw2 = tw.clone();
                crate::newmenu::build_menu(
                    crate::topmenus::NewEntry::Sidebar,
                    &vb,
                    ctx,
                    move |action| {
                        pop2.popdown();
                        if let Some(t) = tw2.upgrade() {
                            crate::newmenu::run_in(&t, action);
                        }
                    },
                );
                pop.set_child(Some(&vb));
                pop.popup();
            });
        }
        self.reload();
    }

    /// 侧栏头部的「＋」（阶段 68：点开是**统一的新建菜单**）。
    ///
    /// **给冒烟用**：验"它的图标跟顶栏一致、tooltip 写清作用域"（审计 P1-6）。
    pub fn plus_button(&self) -> &gtk::Button {
        &self.plus_btn
    }

    /// 侧栏标题栏的**凭据库状态标志**（测试钩子，阶段 77a）。
    ///
    /// 连接管理器删掉后，这个标志挪到侧栏——它是用户唯一能一眼看到
    /// "我到底开没开凭据库"的地方，冒烟得盯着它还在。
    pub fn vault_button(&self) -> &gtk::Button {
        &self.vault_btn
    }

    /// 空态面板（**测试钩子**，阶段 80）。
    ///
    /// 一条连接都没有时，侧栏显示它（标题 + 解释 + 主 CTA），树藏起来。
    pub fn empty_box(&self) -> &gtk::Box {
        &self.empty_box
    }

    /// 树的滚动容器（**测试钩子**，阶段 80）。
    ///
    /// 空态时它该**藏起来**（不然上面是空态、下面还挂着三个空组）。
    pub fn tree_sw(&self) -> &gtk::ScrolledWindow {
        &self.tree_sw
    }

    /// 侧栏「＋」的弹层（**测试钩子**）。
    ///
    /// ⚠️ 弹层是 `set_parent` 挂到按钮上的，**不进容器的 children 链**
    /// ⇒ 从控件树里找不到 ⇒ 只能从这儿拿。
    pub fn plus_popover(&self) -> Option<gtk::Popover> {
        self.plus_pop.borrow().clone()
    }

    pub fn reload(&self) {
        // ⚠️ **重入保护**：状态变化钩子（阶段 57）会从别处调 `reload()`，
        // 万一它落在一次 reload 还没跑完的时候，就会二次借 `entries` 的
        // RefCell → 直接 panic。正在重画就跳过这一次（外层那次画出来就是
        // 最新的）。
        if self.reloading.get() {
            return;
        }
        self.reloading.set(true);
        self.reload_inner();
        self.reloading.set(false);
    }

    fn reload_inner(&self) {
        self.store.clear();
        let recents = recent::load();
        let entries = connmgr::load_all();
        *self.recents.borrow_mut() = recents.clone();
        *self.entries.borrow_mut() = entries.clone();

        let dim = crate::theme::muted_hex();
        let fg = crate::theme::fg_hex();
        let chip_bg = crate::theme::chip_bg_hex();
        // 活跃标记（阶段 57）：连上了的连接/数据库/转发在树上点亮
        let act = crate::active::snapshot();
        let empty = String::new();
        let add_group =
            |store: &gtk::TreeStore, title: &str, icon: &str, n: usize, tip: &str| -> gtk::TreeIter {
            let it = store.append(None);
            let (kind, idx): (i32, i32) = (0, -1);
            // ⚠️ 阶段 93：**组头一律粗体 + 计数徽章**。
            // 以前「有条目时」不加粗、只跟一个小灰字计数 ⇒ 组头和条目
            // 长得一模一样，列表层次全糊了（用户 2026-11-01：「有点丑」）。
            // ⚠️ 组头标题在 const 表里 ⇒ 显示时翻译；**识别用的仍是中文
            //    原文**（COL_NAME，右键菜单靠它认组）。
            let label = group_markup(&crate::i18n::tr_dyn(title), n, &chip_bg, &fg);
            store.set(
                &it,
                &[
                    (COL_TEXT as u32, &label),
                    (COL_ICON as u32, &icon.to_string()),
                    (COL_KIND as u32, &kind),
                    (COL_IDX as u32, &idx),
                    (COL_TIP as u32, &tip.to_string()),
                    (COL_NAME as u32, &title.to_string()),
                    (COL_SUB as u32, &n.to_string()),
                ],
            );
            let _ = &empty;
            it
        };

        // ⚠️ 阶段 80 / 审计 P2-1：**空组不摆**。
        // 以前「最近 / 自定义 / ssh config」三个组头永远在那儿（哪怕 0 项），
        // 看着像"功能坏了"。现在只有真有内容的组才出现；
        // 一个连接都没有时整棵树让位给**空态构件**。
        let n_recent = recents.len();
        let g_recent = if n_recent > 0 {
            Some(add_group(
                &self.store,
                RECENT_GROUP,
                "document-open-recent-symbolic",
                n_recent,
                tr("同一个 用户/主机/端口 只保留最近一次（双击重开）"),
            ))
        } else {
            None
        };
        for (i, r) in recents.iter().enumerate() {
            let Some(g_recent) = g_recent else { break };
            let it = self.store.append(Some(&g_recent));
            let meta = format!("{}@{}:{}", r.user, r.host, if r.port > 0 { r.port } else { 22 });
            let text = row_markup(&r.label, &meta, &fg, &dim);
            let (kind, idx) = (1i32, i as i32);
            self.store.set(
                &it,
                &[
                    (COL_TEXT as u32, &text),
                    // ⚠️ 阶段 93：最近项也是 **SSH 主机**，用主机图标；
                    // 时钟留给"最近"这个**组头**——一排全是时钟太吵。
                    (COL_ICON as u32, &"network-server-symbolic".to_string()),
                    (COL_KIND as u32, &kind),
                    (COL_IDX as u32, &idx),
                    (COL_TIP as u32, &meta),
                    (COL_NAME as u32, &r.label.clone()),
                    (COL_SUB as u32, &meta.clone()),
                ],
            );
        }

        // 数据库连接项：`(会话键, 连接名, 所属 SSH 条目下标)`
        let mut dbs: Vec<(String, String, usize)> = Vec::new();
        // 端口转发项：`(所属 SSH 条目下标, 规则)`（阶段 56）
        let mut fwds: Vec<(usize, connmgr::FwdLeaf)> = Vec::new();

        let n_custom = entries.iter().filter(|e| e.custom).count();
        let g_custom = if n_custom > 0 {
            Some(add_group(
                &self.store,
                tr("自定义"),
                "user-bookmarks-symbolic",
                n_custom,
                "",
            ))
        } else {
            None
        };
        for (i, e) in entries.iter().enumerate() {
            if e.custom {
                let Some(g_custom) = g_custom else { break };
                let it = self.add_entry_row(&g_custom, e, i as i32, &fg, &dim, &act);
                self.add_db_rows(&it, e, i, &fg, &dim, &mut dbs, &act);
                self.add_fwd_rows(&it, e, i, &fg, &dim, &mut fwds, &act);
            }
        }
        let n_ssh = entries.iter().filter(|e| !e.custom).count();
        let g_ssh = if n_ssh > 0 {
            Some(add_group(
                &self.store,
                "ssh config",
                // ⚠️ 阶段 93：组图标跟**条目**图标分开——组头用
                // 「一组机器」，条目用「一台服务器」；以前两边都是
                // network-server-symbolic，看着像同一层的两行。
                "network-workgroup-symbolic",
                n_ssh,
                "",
            ))
        } else {
            None
        };
        for (i, e) in entries.iter().enumerate() {
            if !e.custom {
                let Some(g_ssh) = g_ssh else { break };
                let it = self.add_entry_row(&g_ssh, e, i as i32, &fg, &dim, &act);
                self.add_db_rows(&it, e, i, &fg, &dim, &mut dbs, &act);
                self.add_fwd_rows(&it, e, i, &fg, &dim, &mut fwds, &act);
            }
        }
        *self.dbs.borrow_mut() = dbs;
        *self.fwds.borrow_mut() = fwds;
        // append 阶段行值未就绪，visible_func 会保守判为不可见——值就位后重算
        self.filter.refilter();
        self.view.expand_all();

        // ---- 空态 ↔ 树：二选一 ----
        //
        // 一个连接都没有 ⇒ 藏树、显示空态（标题 + 解释 + 主 CTA）。
        let has_any = !recents.is_empty() || !entries.is_empty();
        self.tree_sw.set_visible(has_any);
        self.empty_box.set_visible(!has_any);
    }

    fn add_entry_row(
        &self,
        parent: &gtk::TreeIter,
        e: &ConnEntry,
        idx: i32,
        fg: &str,
        dim: &str,
        act: &crate::active::ActiveInfo,
    ) -> gtk::TreeIter {
        let it = self.store.append(Some(parent));
        let port = if e.port > 0 { e.port } else { 22 };
        let meta = match e.user.as_deref() {
            Some(u) if !u.is_empty() => format!("{u}@{}:{port}", e.host),
            _ => format!("{}:{port}", e.host),
        };
        // 自定义连接用「机器」图标，ssh config 用「服务器」图标，一眼分得开
        let icon = if e.custom {
            "computer-symbolic"
        } else {
            "network-server-symbolic"
        };
        // 行首状态点：连上了（绿）/ 正在连（黄）/ 终端掉了（黄，阶段 65）
        // ⚠️ 用**聚合**状态：那个 level 只反映文件面板，终端掉了看不出来
        let key = connmgr::entry_session_key(e);
        let state = act.session_state_agg(&key);
        let text = row_markup_st(&e.name, &meta, fg, dim, state);
        let kind = 2i32;
        // 悬停提示说清**哪一部分**出的问题（阶段 65）
        // ⚠️ 什么都没发生（从没打开过的存档连接）⇒ 沿用短提示，不挂细分
        let tip = match act.session_tooltip(&key) {
            Some(t) => format!("{meta}\n{t}"),
            None => meta.clone(),
        };
        self.store.set(
            &it,
            &[
                (COL_TEXT as u32, &text),
                (COL_ICON as u32, &icon.to_string()),
                (COL_KIND as u32, &kind),
                (COL_IDX as u32, &idx),
                (COL_TIP as u32, &tip),
                (COL_NAME as u32, &e.name.clone()),
                (COL_SUB as u32, &meta.clone()),
            ],
        );
        it
    }

    /// 在某个 SSH 连接下面挂它的数据库连接（阶段 10）。
    ///
    /// **为什么挂在 SSH 连接下面**：数据库通道是**复用这条 SSH 连接**的
    /// （`direct-tcpip` 转发，不新建 SSH 连接），所以数据库连接天然属于
    /// 这条 SSH 会话——一个会话下可以挂多个数据库连接（Navicat 的连接树
    /// 就是这个形状）。
    fn add_db_rows(
        &self,
        parent: &gtk::TreeIter,
        e: &ConnEntry,
        entry_idx: usize,
        fg: &str,
        dim: &str,
        dbs: &mut Vec<(String, String, usize)>,
        act: &crate::active::ActiveInfo,
    ) {
        for (key, name, meta) in db_profiles_for(e) {
            let idx = dbs.len() as i32;
            dbs.push((key.clone(), name.clone(), entry_idx));
            let it = self.store.append(Some(parent));
            // 连着库就点亮（阶段 57）
            let live = act.db_connected(&key, &name);
            let meta = crate::active::meta_with(&meta, live, tr("已连接"));
            let tip = if live {
                trf!("{meta}\n已连接", meta = meta)
            } else {
                meta.clone()
            };
            let text = row_markup_st(
                &name,
                &meta,
                fg,
                dim,
                if live {
                    Some(crate::theme::State::Ok)
                } else {
                    None
                },
            );
            self.store.set(
                &it,
                &[
                    (COL_TEXT as u32, &text),
                    (COL_ICON as u32, // preferences-system-database-symbolic 系统里不存在 → 用硬盘柱体
                    &"drive-harddisk-symbolic".to_string()),
                    (COL_KIND as u32, &3i32),
                    (COL_IDX as u32, &idx),
                    (COL_TIP as u32, &tip),
                    (COL_NAME as u32, &name.clone()),
                    (COL_SUB as u32, &meta.clone()),
                ],
            );
        }
    }

    /// 在某个 SSH 连接下面挂它的端口转发规则（阶段 56）。
    ///
    /// **为什么挂在 SSH 连接下面**：转发规则是这条连接的配置，跟数据库连接
    /// 一个道理——用户想看「这条连接会带哪些转发」，在连接下面一眼看到，
    /// 不用去翻连接配置里那个文本框。
    fn add_fwd_rows(
        &self,
        parent: &gtk::TreeIter,
        e: &ConnEntry,
        entry_idx: usize,
        fg: &str,
        dim: &str,
        fwds: &mut Vec<(usize, connmgr::FwdLeaf)>,
        act: &crate::active::ActiveInfo,
    ) {
        let key = connmgr::entry_session_key(e);
        for leaf in connmgr::forward_leaves_for(e) {
            let idx = fwds.len() as i32;
            fwds.push((entry_idx, leaf.clone()));
            let it = self.store.append(Some(parent));
            // ⚠️ **只有进程内规则才可能是"运行中"**（终端规则由 ssh 进程承接，
            // 运行态不在本应用里）；规则键碰巧相同也不能混（阶段 57）。
            let running = leaf.source == connmgr::FwdSource::InProc
                && act.fwd_running(&key, &leaf.key);
            let meta = crate::active::meta_with(&leaf.meta, running, tr("运行中"));
            let tip = if running {
                trf!("{v0}\n运行中", v0 = leaf.tip)
            } else {
                leaf.tip.clone()
            };
            // ⚠️ **来源标签放前面**（侧边栏只有 ~235px 宽，行尾会被截断）：
            // 规则描述又长（"本地 127.0.0.1:9000 → 10.0.0.1:80"），标签在后面
            // 就**整个看不见**了——用户分不清这条是"进程内"还是"终端"承接的。
            // 前面放短标签（进程内 / 终端 / 运行中），后面才是被截断的细节。
            //
            // ⚠️ 阶段 93：**标签当主标题、细节当副标题**——跟别的行
            // 一个版式（名字 = 正文色，副标题 = 小号淡色）。以前这里是
            // "淡色小字标签 + 正常色细节"，跟其它行反着来，一眼就出戏。
            let text = row_markup_st(
                &meta,
                &leaf.text,
                fg,
                dim,
                if running {
                    Some(crate::theme::State::Ok)
                } else {
                    None
                },
            );
            self.store.set(
                &it,
                &[
                    (COL_TEXT as u32, &text),
                    // 图标名必须是系统里确实有的（network-transmit-receive）
                    (COL_ICON as u32, &"network-transmit-receive-symbolic".to_string()),
                    (COL_KIND as u32, &4i32),
                    (COL_IDX as u32, &idx),
                    (COL_TIP as u32, &tip),
                    (COL_NAME as u32, &meta.clone()),
                    (COL_SUB as u32, &leaf.text.clone()),
                ],
            );
        }
    }

    /// 当前选中的业务行 → (kind, idx)；分组行返回 None。
    fn selected_row(&self) -> Option<(i32, i32)> {
        let (rows, _m) = self.view.selection().selected_rows();
        let path = rows.first()?;
        let child = self.filter.convert_path_to_child_path(path)?;
        let it = self.store.iter(&child)?;
        let kind: i32 = self.store.get(&it, COL_KIND);
        let idx: i32 = self.store.get(&it, COL_IDX);
        (kind > 0 && idx >= 0).then_some((kind, idx))
    }

    /// 选中行对应的**组头**（仅 kind==0）：返回组名。
    ///
    /// ⚠️ 组头没有 idx ⇒ [`Self::selected_row`] 对它返回 None
    /// ⇒ 「最近」组的右键菜单（清空最近连接）得单独取。
    fn selected_group(&self) -> Option<String> {
        let (rows, _m) = self.view.selection().selected_rows();
        let path = rows.first()?;
        let child = self.filter.convert_path_to_child_path(path)?;
        let it = self.store.iter(&child)?;
        let kind: i32 = self.store.get(&it, COL_KIND);
        if kind != 0 {
            return None;
        }
        let name: String = self.store.get(&it, COL_NAME);
        Some(name)
    }

    /// 选中行对应的自建/ssh config 条目（仅 kind=2）。
    fn selected_entry(&self) -> Option<ConnEntry> {
        let (kind, idx) = self.selected_row()?;
        if kind != 2 {
            return None;
        }
        self.entries.borrow().get(idx as usize).cloned()
    }

    /// 选中行对应的最近项（仅 kind=1）。
    fn selected_recent(&self) -> Option<recent::RecentConn> {
        let (kind, idx) = self.selected_row()?;
        if kind != 1 {
            return None;
        }
        self.recents.borrow().get(idx as usize).cloned()
    }

    fn parent_window(&self) -> Option<gtk::Window> {
        self.widget
            .ancestor(gtk::Window::static_type())
            .and_downcast::<gtk::Window>()
    }

    /// 第 idx 条转发规则（`(所属条目下标, 规则)`）。
    fn fwd_item(&self, idx: usize) -> Option<(usize, connmgr::FwdLeaf)> {
        self.fwds.borrow().get(idx).cloned()
    }

    /// entries 里的第 idx 条连接。
    fn entry_at(&self, idx: usize) -> Option<ConnEntry> {
        self.entries.borrow().get(idx).cloned()
    }


    /// 条目/最近项右键菜单：连接 / 在新窗口打开 / 编辑 / 删除 / 复制地址 /
    /// 从最近中移除。只读的 ssh config 条目不给编辑与删除。
    ///
    /// 用自建 `Popover` + 按钮而不是 `PopoverMenu` + action：右键菜单挂在
    /// `TreeView` 上时，action 版菜单在下一次点击里不会被激活（点击只把弹层
    /// 关掉）——按钮版把回调直接连在控件上，行为可预期。
    fn install_row_menu(self: &Rc<Self>) {
        // 弹层建一次、反复用：每次点击都新建的话，GTK 在右键手势里
        // 现建的 popover 拿不到后续点击（点菜单项只会把弹层关掉）。
        let pop = crate::menu::popover(&self.view);
        let vb = crate::menu::body();
        pop.set_child(Some(&vb));

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let view = self.view.clone();
        let sb = Rc::clone(self);

        gesture.connect_pressed(move |g, _n, x, y| {
            if g.current_button() != 3 {
                return;
            }
            // 右键先选中指针下的行（文件面板同款惯例）
            if let Some((Some(path), _, _, _)) = view.path_at_pos(x as i32, y as i32) {
                let sel = view.selection();
                if !sel.path_is_selected(&path) {
                    sel.unselect_all();
                    sel.select_path(&path);
                }
            }
            // ⚠️ **组头**（kind==0）没有 idx ⇒ `selected_row()` 给 None。
            //    组头不是连接，不能点「连接」；「最近」组的组头右键 =
            //    「清空最近连接」。
            let group = sb.selected_group();
            let (kind, idx) = sb.selected_row().unwrap_or((0, -1));
            let entry = sb.selected_entry();
            let recent = sb.selected_recent();
            let custom = entry.as_ref().map(|e| e.custom).unwrap_or(false);
            // 数据库连接项（阶段 10）：`(会话键, 连接名, …)`
            let db_item = if kind == 3 {
                let dbs = sb.dbs.borrow();
                dbs.get(idx as usize).cloned()
            } else {
                None
            };

            while let Some(c) = vb.first_child() {
                vb.remove(&c);
            }

            // ⚠️ **菜单项清单是唯一来源**（`row_menu_items`）：这个菜单是
            // **每次右键重建**的 ⇒ 靠"控件在不在树里"既测不到、清单和
            // 实际显示的也容易漂移。UI 照清单建、测试照清单断言。
            let fwd_ctx = if kind == 4 {
                sb.fwd_item(idx as usize)
                    .and_then(|(entry_idx, leaf)| sb.entry_at(entry_idx).map(|e| (e, leaf)))
            } else {
                None
            };
            // 「新建数据库连接…」要用的会话键（阶段 13）：连接条目用它自己的
            // 会话键，最近项用 user@host:port 拼一个。
            let sess_key = if kind == 2 {
                entry.as_ref().map(connmgr::entry_session_key)
            } else {
                recent.as_ref().map(|r| {
                    format!("{}@{}:{}",
                        r.user,
                        r.host,
                        if r.port > 0 { r.port } else { 22 }
                    )
                })
            };
            // 「复制地址」复制什么
            let addr = if let Some(e) = &entry {
                connmgr::entry_addr(e)
            } else if let Some((_, name, _)) = &db_item {
                name.clone()
            } else if let Some(r) = &recent {
                format!("{}@{}:{}",
                    r.user,
                    r.host,
                    if r.port > 0 { r.port } else { 22 }
                )
            } else {
                String::new()
            };
            let parent = sb.parent_window();
            let ctx = RowMenuCtx {
                kind,
                custom,
                has_db: db_item.is_some(),
                fwd: fwd_ctx,
                group,
            };

            for item in row_menu_items(&ctx) {
                let RowMenuItem::Act { label, icon } = item else {
                    // ⚠️ **每次新建分隔线**：同一个 widget append 两次会被
                    //   GTK 拒掉（"gtk_widget_get_parent (child) == NULL"）。
                    vb.append(&crate::menu::sep());
                    continue;
                };
                let b = match label {
                    // 「启用转发」是**勾选式**（阶段 59）：勾 = 已启用，一眼
                    // 看得出当前是开是关，不用去猜文字的意思。
                    "启用转发" => {
                        let Some((e, leaf)) = ctx.fwd.clone() else { continue };
                        let enabled =
                            connmgr::fwd_enabled(&e, leaf.source, &leaf.key).unwrap_or(false);
                        let t = crate::menu::toggle(icon, label, None, enabled);
                        t.set_tooltip_text(Some(
                            tr("勾选 = 这条转发已启用（连接时自动带上）。点一下切换。\n\
                             进程内规则当场生效；终端承接的规则要下次连接才生效。"),
                        ));
                        let pop2 = pop.clone();
                        let sb2 = Rc::clone(&sb);
                        let parent2 = parent.clone();
                        t.connect_clicked(move |_| {
                            pop2.popdown();
                            match crate::panel::fwdmgr::toggle_rule(&e, leaf.source, &leaf.key) {
                                Ok(_) => {
                                    sb2.reload();
                                }
                                Err(err) => {
                                    crate::panel::dialogs::show_error(
                                        parent2.as_ref(),
                                        tr("启停转发失败"),
                                        &err,
                                    );
                                }
                            }
                        });
                        // ToggleButton 是 Button 的子类，但 match 的两个分支
                        // 要**同一个类型** ⇒ 显式上转。
                        let t: gtk::Button = t.upcast();
                        t
                    }
                    _ => {
                        let b = crate::menu::item(icon, label, None);
                        if label == "删除" || label == tr("删除这个数据库连接") {
                            crate::menu::danger(&b);
                        }
                        b
                    }
                };
                match label {
                    // 连接（数据库连接项是"打开数据库窗口"，端口转发行是"打开端口转发"）
                    "连接" | "打开数据库" | "打开端口转发" => {
                        let (sb2, pop2) = (Rc::clone(&sb), pop.clone());
                        b.connect_clicked(move |_| {
                            pop2.popdown();
                            sb2.open_row(kind, idx as usize);
                        });
                    }
                    "在新窗口打开" => {
                        if let Some(e) = entry.clone() {
                            let pop2 = pop.clone();
                            let parent2 = parent.clone();
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                if let Err(err) = connmgr::spawn_entry(&e) {
                                    crate::panel::dialogs::show_error(
                                        parent2.as_ref(),
                                        tr("启动新窗口失败"),
                                        &err,
                                    );
                                }
                            });
                        }
                    }
                    "编辑…" => {
                        if let Some(e) = entry.clone() {
                            let pop2 = pop.clone();
                            let sb2 = Rc::clone(&sb);
                            let parent2 = parent.clone();
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                let e2 = e.clone();
                                let sb3 = Rc::clone(&sb2);
                                connmgr::ui_edit_connection(
                                    parent2.as_ref(),
                                    e2,
                                    move || sb3.reload(),
                                );
                            });
                        }
                    }
                    "删除" => {
                        if let Some(e) = entry.clone() {
                            let pop2 = pop.clone();
                            let sb2 = Rc::clone(&sb);
                            let parent2 = parent.clone();
                            let name = e.name.clone();
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                let name2 = name.clone();
                                let sb3 = Rc::clone(&sb2);
                                crate::panel::dialogs::confirm_with(
                                    parent2.as_ref(),
                                    &trf!("删除连接「{name}」？\n\n此操作不可撤销；该条目在 connections.ini 里的配置（含端口转发规则）会一并删除。", name = name),
                                    tr("删除"),
                                    move |ok| {
                                        if ok && connmgr::delete_entry(&name2) {
                                            sb3.reload();
                                        }
                                    },
                                );
                            });
                        }
                    }
                    // 阶段 77a：导出**选中这一条**（文件菜单那个导出的是全部）
                    "导出为 PuTTY 会话…" => {
                        if let Some(e) = entry.clone() {
                            let pop2 = pop.clone();
                            let parent2 = parent.clone();
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                crate::connmgr::ui_export_putty(
                                    parent2.as_ref(),
                                    vec![e.name.clone()],
                                );
                            });
                        }
                    }
                    "重新连接终端" => {
                        let pop2 = pop.clone();
                        let sb2 = Rc::clone(&sb);
                        let kind2 = kind;
                        let idx2 = idx as usize;
                        b.connect_clicked(move |_| {
                            pop2.popdown();
                            sb2.reconnect_term_for_row(kind2, idx2);
                        });
                    }
                    "新建数据库连接…" => {
                        if let Some(sk) = sess_key.clone() {
                            let pop2 = pop.clone();
                            let parent2 = parent.clone();
                            let sb2 = Rc::clone(&sb);
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                let sk2 = sk.clone();
                                let sb3 = Rc::clone(&sb2);
                                // ⚠️ **测连要用这条会话的面板状态**（数据库通道
                                //    复用这条 SSH 连接 ⇒ 得找得到对应的会话）。
                                //    找不到（那个会话没开标签页）⇒ 传 None，
                                //    对话框会照旧存好、打开工作台。
                                let st = sb2
                                    .tabs
                                    .sessions()
                                    .into_iter()
                                    .find(|(k, _)| *k == sk2)
                                    .map(|(_, st)| st);
                                crate::panel::dbconn::ui_new(
                                    parent2.as_ref(),
                                    &sk2,
                                    st,
                                    move |_k, _name| sb3.reload(),
                                );
                            });
                        }
                    }
                    "编辑这个数据库连接…" => {
                        if let Some((key, name, _)) = db_item.clone() {
                            let pop2 = pop.clone();
                            let parent2 = parent.clone();
                            let sb2 = Rc::clone(&sb);
                            let (k1, n1) = (key.clone(), name.clone());
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                let sb3 = Rc::clone(&sb2);
                                crate::panel::dbconn::ui_edit(parent2.as_ref(), &k1, &n1);
                                sb3.reload();
                            });
                        }
                    }
                    "删除这个数据库连接" => {
                        if let Some((key, name, _)) = db_item.clone() {
                            let pop2 = pop.clone();
                            let sb2 = Rc::clone(&sb);
                            let parent2 = parent.clone();
                            b.connect_clicked(move |_| {
                                pop2.popdown();
                                let (key2, name2) = (key.clone(), name.clone());
                                let sb3 = Rc::clone(&sb2);
                                crate::panel::dialogs::confirm_with(
                                    parent2.as_ref(),
                                    &trf!("删除数据库连接「{name2}」？\n\n只删这条连接的配置（地址、账号、记住的密码），不动 SSH 连接，也不动库里的数据。", name2 = name2),
                                    tr("删除"),
                                    move |ok| {
                                        if ok && crate::backend::dbstore::remove(&key2, &name2) {
                                            sb3.reload();
                                        }
                                    },
                                );
                            });
                        }
                    }
                    "从最近中移除" => {
                        let (sb2, pop2) = (Rc::clone(&sb), pop.clone());
                        b.connect_clicked(move |_| {
                            pop2.popdown();
                            recent::remove(idx as usize);
                            sb2.reload();
                        });
                    }
                    // 「最近」**组头**右键 = 清空整份最近连接
                    // （堡垒机场景：一次性连接太多，一条条删太累）
                    "清空最近连接" => {
                        let pop2 = pop.clone();
                        let sb2 = Rc::clone(&sb);
                        let parent2 = parent.clone();
                        b.connect_clicked(move |_| {
                            pop2.popdown();
                            // ⚠️ 外层是 `Fn` 闭包 ⇒ 不能把捕获的 Rc **移**进
                            //    内层闭包（那是 FnOnce 才干的事）⇒ 这里克隆一份。
                            let sb3 = Rc::clone(&sb2);
                            crate::panel::dialogs::confirm_with(
                                parent2.as_ref(),
                                tr("清空最近连接？\n\n\
                                 「最近连接」里的记录会被全部删掉（已保存的连接配置不受影响）。"),
                                tr("清空"),
                                move |ok| {
                                    if ok {
                                        crate::recent::clear();
                                        sb3.reload();
                                    }
                                },
                            );
                        });
                    }
                    "复制地址" => {
                        let pop2 = pop.clone();
                        let addr2 = addr.clone();
                        b.connect_clicked(move |_| {
                            pop2.popdown();
                            if let Some(d) = gtk::gdk::Display::default() {
                                d.clipboard().set_text(&addr2);
                            }
                        });
                    }
                    _ => {}
                }
                vb.append(&b);
            }

            let pos = sb
                .view
                .compute_point(&sb.widget, &gtk::graphene::Point::new(x as f32, y as f32));
            if let Some(p) = pos {
                pop.set_pointing_to(Some(&gdk::Rectangle::new(
                    p.x() as i32,
                    p.y() as i32,
                    1,
                    1,
                )));
            }
            pop.popup();
        });
        self.view.add_controller(gesture);
    }

    /// 会话树里的数据库连接节点（`(名称, 摘要)`）——测试用。
    ///
    /// 走的是**树里的真实行**（不是重新算一遍），所以树上挂错位置、
    /// 挂丢了，这里都能看出来。
    pub fn db_rows(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in 0.. {
            let path = gtk::TreePath::from_indices(&[i]);
            if self.store.iter(&path).is_none() {
                break;
            }
            collect_db_rows(&self.store, &path, &mut out);
        }
        out
    }

    /// 树上的每一行：`(kind, 主标题, 副标题, 渲染用 markup)`——测试用。
    ///
    /// ⚠️ **为什么要把主/副标题和 markup 一起给出去**：版式（阶段 93）
    /// 有两半——「模型里存的是什么」（主标题/副标题分开放，过滤搜纯文本）
    /// 和「屏幕上画成什么样」（markup）。冒烟要同时盯住这两半，
    /// 只看 markup 会漏掉"过滤搜的是 markup"这类回归。
    pub fn tree_rows(&self) -> Vec<(i32, String, String, String)> {
        let mut out = Vec::new();
        collect_tree_rows(&self.store, &mut out);
        out
    }

    /// 树里**图标渲染器**——测试用（阶段 93）。
    pub fn icon_renderer(&self) -> gtk::CellRendererPixbuf {
        self.icon_renderer.clone()
    }

    /// 屏幕上**看得见**的行（过滤之后）——测试用。
    ///
    /// ⚠️ 查模型 ≠ 查视图（AGENTS.md）：`tree_rows()` 给的是**模型里
    /// 全部的行**（过滤掉了的也在里头），这边走的是**过滤器模型**
    /// ⇒ 拿到的就是屏幕上真能看见的那批行。
    pub fn visible_rows(&self) -> Vec<(i32, String, String, String)> {
        let mut out = Vec::new();
        for i in 0.. {
            let path = gtk::TreePath::from_indices(&[i]);
            if self.filter.iter(&path).is_none() {
                break;
            }
            collect_visible_rows(&self.filter, &self.store, &path, &mut out);
        }
        out
    }

    /// 会话树里的端口转发行（`(名称, 摘要)`）——测试用（阶段 56）。
    ///
    /// 走的是**树里的真实行**（跟 db_rows 一个路数）。
    pub fn fwd_rows(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in 0.. {
            let path = gtk::TreePath::from_indices(&[i]);
            if self.store.iter(&path).is_none() {
                break;
            }
            collect_fwd_rows(&self.store, &path, &mut out);
        }
        out
    }

    /// 会话树里的转发规则：`(所属 SSH 连接名, 规则键, 规则显示名)`——测试用。
    ///
    /// ⚠️ 连接管理器删掉后（阶段 77b），转发规则的**唯一**树就是侧栏这棵
    /// ⇒ 右键菜单靠它找规则的「规则键」也得能从这儿断言。
    pub fn fwd_rule_keys(&self) -> Vec<(String, String, String)> {
        let entries = self.entries.borrow();
        self.fwds
            .borrow()
            .iter()
            .map(|(idx, leaf)| {
                let name = entries.get(*idx).map(|e| e.name.clone()).unwrap_or_default();
                (name, leaf.key.clone(), leaf.text.clone())
            })
            .collect()
    }

    /// 会话树里的 SSH 连接行（`(名称markup, 摘要)`）——测试用（阶段 57）。
    ///
    /// 名称里带**行首状态点**（连上了是绿点），活跃标记就靠它断言。
    pub fn entry_rows(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for i in 0.. {
            let path = gtk::TreePath::from_indices(&[i]);
            if self.store.iter(&path).is_none() {
                break;
            }
            collect_rows(&self.store, &path, 2, &mut out);
        }
        out
    }

    /// 取某一行对应的**连接参数**（「连接」与「重新连接终端」共用一条路径）。
    ///
    /// 两条动作必须拿到**完全一样**的参数：否则「连接」能上、
    /// 「重新连接终端」却连到别的地方去（比如别名没展开、端口没补 22）。
    fn opts_for_row(&self, kind: i32, idx: usize) -> Option<ConnOptions> {
        match kind {
            1 => {
                // 克隆后立刻释放借用：tabs.open / reload 内部会再借 recents，
                // 持借用跨调用会 RefCell 冲突 panic（曾致点「最近」条目闪退）
                let r = {
                    let recents = self.recents.borrow();
                    recents.get(idx).cloned()
                }?;
                let entries = self.entries.borrow().clone();
                Some(opts_for_recent(&r, &entries))
            }
            2 => {
                let entries = self.entries.borrow();
                let e = entries.get(idx)?.clone();
                drop(entries);
                // 与 connmgr 存储层同一条转换路径（proto/sercfg 一起搬）
                let mut o = e.to_opts();
                // ssh config 别名展开（仅对 ssh 有意义；telnet/serial 的
                // host 是主机名/设备路径，不参与 ssh config 匹配）
                if o.is_ssh() {
                    connmgr::fill_opts_defaults(&mut o);
                }
                Some(o)
            }
            _ => None,
        }
    }

    /// 「重新连接终端」：拿与「连接」一样的参数，让标签那边
    /// "标签还在就重连它，标签没了就重建一个"。
    fn reconnect_term_for_row(&self, kind: i32, idx: usize) {
        let Some(o) = self.opts_for_row(kind, idx) else { return };
        crate::tabs::reconnect_term_with_opts(&o);
    }

    fn open_row(&self, kind: i32, idx: usize) {
        match kind {
            1 => {
                let Some(o) = self.opts_for_row(kind, idx) else { return };
                self.tabs.open(&o);
                self.reload();
            }
            2 => {
                let Some(o) = self.opts_for_row(kind, idx) else { return };
                self.tabs.open(&o);
                self.reload();
            }
            3 => {
                // 数据库连接（阶段 10）：复用这条 SSH 连接开数据库窗口。
                // 已经开着同一会话的标签就切过去，别重复开一个。
                let item = {
                    let dbs = self.dbs.borrow();
                    dbs.get(idx).cloned()
                };
                let Some((key, name, entry_idx)) = item else { return };
                let st: Option<Rc<crate::panel::PanelState>> = match self.tabs.activate_session(&key) {
                    Some(st) => Some(st),
                    None => {
                        // 这条会话还没打开 → 先把它连上
                        let entry = {
                            let entries = self.entries.borrow();
                            entries.get(entry_idx).cloned()
                        };
                        let Some(e) = entry else { return };
                        let mut o = e.to_opts();
                        if o.is_ssh() {
                            connmgr::fill_opts_defaults(&mut o);
                        }
                        self.tabs.open(&o);
                        self.tabs.current_panel_state()
                    }
                };
                let Some(st) = st else { return };
                // 只开页面，**不连**：连哪条由 use_profile 决定（双击哪条连哪条）
                crate::panel::on_open_db_page(&st);
                crate::panel::dbui::use_profile(&st, &key, &name);
            }
            4 => {
                // 端口转发（阶段 56）：双击规则 → 打开这条连接的端口转发窗口。
                // 会话没开着就先把它连上（跟打开数据库连接一个路数）。
                let (entry_idx, _leaf) = {
                    let fwds = self.fwds.borrow();
                    match fwds.get(idx).cloned() {
                        Some(x) => x,
                        None => return,
                    }
                };
                let entry = {
                    let entries = self.entries.borrow();
                    entries.get(entry_idx).cloned()
                };
                let Some(e) = entry else { return };
                let key = connmgr::entry_session_key(&e);
                let st = match self.tabs.activate_session(&key) {
                    Some(st) => Some(st),
                    None => {
                        let mut o = e.to_opts();
                        if o.is_ssh() {
                            connmgr::fill_opts_defaults(&mut o);
                        }
                        self.tabs.open(&o);
                        self.tabs.current_panel_state()
                    }
                };
                let Some(st) = st else { return };
                crate::panel::on_open_forwards(&st);
            }
            _ => {}
        }
    }
}

/// 某个 SSH 连接下面的数据库连接（阶段 10）。
///
/// **纯逻辑**（不碰 GTK）：会话键怎么算、连接名怎么来、摘要怎么拼，都在这一
/// 处——侧边栏树和单测共用同一份，不会出现"树上显示的"和"存进 ini 的"对不上。
///
/// 返回 `(会话键, 连接名, 摘要)`。
pub fn db_profiles_for(e: &ConnEntry) -> Vec<(String, String, String)> {
    // 会话键要跟 connmgr 那边一致（别名展开后的主机），见 connmgr::entry_session_key
    let key = connmgr::entry_session_key(e);
    crate::backend::dbstore::list(&key)
        .into_iter()
        .map(|p| {
            // 老配置没有连接名 → 给个能看的（`pg 127.0.0.1:5432/db`）
            let name = if p.name.is_empty() {
                crate::backend::dbstore::DbProfile::default_name(p.proto, &p.host, p.port, &p.dbname)
            } else {
                p.name.clone()
            };
            let summary = p.summary();
            (key.clone(), name, summary)
        })
        .collect()
}

/// 按**路径**递归收集树里 kind==3（数据库连接）的行。
///
/// **为什么按路径走，不用 `iter_next`**：GTK4 里 `iter_children` 会让**上层**的
/// iter 失效——递归两层后顶层 `iter_next` 直接返回 false，树只走了一半
/// （当时冒烟测试误报"树上没挂出数据库连接"，差点当成真 bug 去改树）。
/// 按路径（`TreePath` + 下标）每一步都从根重新定位，不依赖"当前位置"，稳。
///
/// **为什么要递归**：数据库连接挂在 SSH 连接行的**下面**（分组 → 连接 → 数据库，
/// 两层深），只走一层看不到数据库节点。
fn collect_db_rows(store: &gtk::TreeStore, path: &gtk::TreePath, out: &mut Vec<(String, String)>) {
    collect_rows(store, path, 3, out)
}

/// 按路径递归收集树里 kind==4（端口转发）的行（阶段 56）。
fn collect_fwd_rows(store: &gtk::TreeStore, path: &gtk::TreePath, out: &mut Vec<(String, String)>) {
    collect_rows(store, path, 4, out)
}

/// 按路径递归收集某类叶子行（`(名称, 摘要)`）。
fn collect_rows(
    store: &gtk::TreeStore,
    path: &gtk::TreePath,
    want_kind: i32,
    out: &mut Vec<(String, String)>,
) {
    let Some(it) = store.iter(path) else { return };
    if store.get::<i32>(&it, COL_KIND) == want_kind {
        out.push((
            store.get::<String>(&it, COL_TEXT),
            store.get::<String>(&it, COL_TIP),
        ));
    }
    for i in 0.. {
        let mut cp = path.clone();
        cp.append_index(i);
        // 下标越界 → 没有这个子节点
        if store.iter(&cp).is_none() {
            break;
        }
        collect_rows(store, &cp, want_kind, out);
    }
}

/// 名字与副标题之间的**分隔点**。
///
/// ⚠️ 以前只隔两个空格 ⇒ 名字和地址看着是连在一起的一串
/// （用户 2026-11-01：「有点丑」）。一个淡色的间隔点就能把两段分开。
const SEP: &str = "  ·  ";

/// 把树里的**所有行**收进 `out`（含各级子节点）。
///
/// ⚠️ **按路径走**，别用 `iter_next` 深遍历：在子行上调
/// `iter_children` 会让**上层**的 iter 失效，递归两层后顶层
/// `iter_next` 直接返回 false ⇒ 树只走了一半（AGENTS.md 的坑）。
fn collect_tree_rows(store: &gtk::TreeStore, out: &mut Vec<(i32, String, String, String)>) {
    for i in 0.. {
        let path = gtk::TreePath::from_indices(&[i]);
        if store.iter(&path).is_none() {
            break;
        }
        collect_tree_rows_at(store, &path, out);
    }
}

/// 收 `path` 这一行及其子树。
fn collect_tree_rows_at(
    store: &gtk::TreeStore,
    path: &gtk::TreePath,
    out: &mut Vec<(i32, String, String, String)>,
) {
    let Some(it) = store.iter(path) else { return };
    out.push((
        store.get::<i32>(&it, COL_KIND),
        store.get::<String>(&it, COL_NAME),
        store.get::<String>(&it, COL_SUB),
        store.get::<String>(&it, COL_TEXT),
    ));
    for i in 0.. {
        let mut cp = path.clone();
        cp.append_index(i);
        if store.iter(&cp).is_none() {
            break;
        }
        collect_tree_rows_at(store, &cp, out);
    }
}

/// 收**过滤器模型**里 `path` 这一行及其可见子树。
///
/// ⚠️ 同样**按路径走**（见 [`collect_tree_rows_at`] 的说明）：`iter_next`
/// 深遍历在过滤器上一样会踩"上层 iter 失效"的坑。
fn collect_visible_rows(
    filter: &gtk::TreeModelFilter,
    store: &gtk::TreeStore,
    path: &gtk::TreePath,
    out: &mut Vec<(i32, String, String, String)>,
) {
    if let Some(child) = filter.convert_path_to_child_path(path) {
        if let Some(it) = store.iter(&child) {
            out.push((
                store.get::<i32>(&it, COL_KIND),
                store.get::<String>(&it, COL_NAME),
                store.get::<String>(&it, COL_SUB),
                store.get::<String>(&it, COL_TEXT),
            ));
        }
    }
    for i in 0.. {
        let mut cp = path.clone();
        cp.append_index(i);
        // 过滤掉的子节点在过滤器模型里**不存在** ⇒ iter 返回 None
        if filter.iter(&cp).is_none() {
            break;
        }
        collect_visible_rows(filter, store, &cp, out);
    }
}

/// 一行「名字 + 副标题（地址）」的 markup。
fn row_markup(name: &str, sub: &str, fg: &str, dim: &str) -> String {
    row_markup_st(name, sub, fg, dim, None)
}

/// 同上，但行首带**活跃状态点**（阶段 57）。
///
/// 状态点用颜色表达（绿=已连、黄=正在连），含义另有悬停提示兜底——
/// 只靠颜色说话，色弱用户看不出来。
///
/// ⚠️ **名字要显式上正文色**（阶段 91a 的教训）：不写的话用的是
/// GTK 的默认前景色，某些系统主题下那个色是白的 ⇒ 白底白字。
fn row_markup_st(
    name: &str,
    sub: &str,
    fg: &str,
    dim: &str,
    state: Option<crate::theme::State>,
) -> String {
    let name = glib::markup_escape_text(name);
    let sub = glib::markup_escape_text(sub);
    format!("{}<span foreground='{fg}'>{name}</span><span size='small' foreground='{dim}'>{SEP}{sub}</span>",
        crate::active::dot(state)
    )
}

/// 组头的 markup：**粗体标题 + 计数徽章**（阶段 93）。
///
/// 徽章 = 一小块底色 + 徽章里的数字用**正文色**（次要色压在这种底上
/// 只有 ~4.1:1，读不清——见 [`crate::theme::chip_bg_hex`]）。
fn group_markup(title: &str, n: usize, chip_bg: &str, fg: &str) -> String {
    let title = glib::markup_escape_text(title);
    if n == 0 {
        format!("<b>{title}</b>")
    } else {
        format!("<b>{title}</b> <span size='small' foreground='{fg}' background='{chip_bg}'> {n} </span>"
        )
    }
}

/// 最近项 → 连接参数（**公开**：欢迎页的「最近连接」块也要用）。
///
/// 侧栏的最近项与欢迎页的最近连接是**同一份数据、同一套转换**
/// ⇒ 抽成自由函数，两处共用（别抄一份）。
pub fn opts_for_recent(r: &recent::RecentConn, entries: &[ConnEntry]) -> ConnOptions {
    let mut o = ConnOptions::default();
    o.host = r.host.clone();
    o.port = if r.port > 0 { r.port } else { 22 };
    o.user = r.user.clone();
    o.title = Some(r.label.clone());
    // ref 名回查密码（最近记录不落密码）
    if let Some(name) = &r.ref_name {
        match entries.iter().find(|e| &e.name == name) {
            Some(e) => {
                // ⚠️ **凭证库兜底**（阶段 C12）：`entries` 是**建欢迎页那一刻**
                //    的快照——那时凭证库多半还锁着（保存密码时 ini 明文已删、
                //    进了库）⇒ 快照里 password 恒为 None，解锁了库再点最近
                //    卡片还是要输密码（用户 2026-11-09 报）。**现查一次**：
                //    键 = 连接名（`save_conn` 存的同一个键）；锁着返回 None
                //    走原路。
                o.password = e.password.clone().or_else(|| crate::vault::get(name));
                // **把会话名带上**：自定义条目的会话键是 `s:<名字>`，
                // 最近项默认算出来的是 `user@host:port`——两个键指向
                // 同一台机器，但数据库连接是按键分组的。键不一致的话，
                // 从最近项打开的工作台里就看不到这条会话下的数据库
                // 连接（"明明建过，怎么没有"）。
                if e.custom {
                    o.session_name = Some(e.name.clone());
                }
            }
            None => {
                // 快照里没有这条（建连接晚于欢迎页构建）⇒ 直接查凭证库；
                // ref 名只指向自建条目 ⇒ 会话键同样按 `s:<名字>`。
                o.password = crate::vault::get(name);
                o.session_name = Some(name.clone());
            }
        }
    }
    // 别名解析（最近记录里的 host 可能是 ssh config 别名）
    connmgr::fill_opts_defaults(&mut o);
    o
}

#[cfg(test)]
mod tests {
    use super::group_markup;
    use super::row_markup;
    use super::row_markup_st;
    use super::row_menu_items;
    use super::RowMenuCtx;
    use super::RowMenuItem;

    /// 清单里所有动作项的文案（不含分隔线）。
    fn labels(items: &[RowMenuItem]) -> Vec<&'static str> {
        items
            .iter()
            .filter_map(|i| match i {
                RowMenuItem::Act { label, .. } => Some(*label),
                RowMenuItem::Sep => None,
            })
            .collect()
    }

    fn ctx(kind: i32, custom: bool, has_db: bool) -> RowMenuCtx {
        RowMenuCtx {
            kind,
            custom,
            has_db,
            fwd: None,
            group: None,
        }
    }

    /// 组头**不是连接** ⇒ 右键不能给「连接」（那是个点了没反应的项）。
    #[test]
    fn 组头菜单不是连接菜单() {
        let v = labels(&row_menu_items(&ctx(0, false, false)));
        assert!(v.is_empty(), "普通组头不该有菜单项：{v:?}");
    }

    /// 「最近」组头右键 = 清空最近连接（堡垒机场景：一次性连接太多）。
    #[test]
    fn 最近组头有清空动作() {
        let mut c = ctx(0, false, false);
        c.group = Some(super::RECENT_GROUP.to_string());
        let v = labels(&row_menu_items(&c));
        assert!(v.contains(&"清空最近连接"), "最近组头要能清空：{v:?}");
        assert!(!v.contains(&"连接"), "组头不是连接：{v:?}");
    }

    /// 别的组头没有清理动作（自定义 / ssh config 不能清）。
    #[test]
    fn 别的组头没有清理动作() {
        let mut c = ctx(0, false, false);
        c.group = Some("自定义".to_string());
        assert!(labels(&row_menu_items(&c)).is_empty());
    }

    #[test]
    fn 自定义连接有编辑删除和导出() {
        let v = labels(&row_menu_items(&ctx(2, true, false)));
        assert!(v.contains(&"编辑…"), "自定义连接要能编辑：{v:?}");
        assert!(v.contains(&"删除"), "自定义连接要能删除：{v:?}");
        assert!(
            v.contains(&"导出为 PuTTY 会话…"),
            "连接管理器删掉后，导出选中项只剩这里：{v:?}"
        );
    }

    #[test]
    fn ssh_config_条目没有编辑和删除() {
        // ~/.ssh/config 是外部文件 ⇒ 改不得、删不得（阶段 85 的判定）
        let v = labels(&row_menu_items(&ctx(2, false, false)));
        assert!(!v.contains(&"编辑…"), "ssh config 条目不该能编辑：{v:?}");
        assert!(!v.contains(&"删除"), "ssh config 条目不该能删除：{v:?}");
        assert!(
            !v.contains(&"导出为 PuTTY 会话…"),
            "ssh config 条目不是自定义连接，导不出去：{v:?}"
        );
        assert!(v.contains(&"连接"), "但总得能连：{v:?}");
    }

    #[test]
    fn 数据库连接行有编辑和删除() {
        // 阶段 85 的成果：数据库连接行也得能改能删（连接管理器删掉后
        // 这儿就是唯一的地方）
        let v = labels(&row_menu_items(&ctx(3, false, true)));
        assert!(v.contains(&"编辑这个数据库连接…"), "{v:?}");
        assert!(v.contains(&"删除这个数据库连接"), "{v:?}");
        assert!(v.contains(&"打开数据库"), "{v:?}");
    }

    #[test]
    fn 最近项有从最近中移除() {
        let v = labels(&row_menu_items(&ctx(1, false, false)));
        assert!(v.contains(&"从最近中移除"), "{v:?}");
        assert!(!v.contains(&"编辑…"), "最近项不能编辑：{v:?}");
    }

    #[test]
    fn 取不到规则的转发行不摆启用转发() {
        // 摆一个点了没反应的项比不摆更糟
        let v = labels(&row_menu_items(&ctx(4, false, false)));
        assert!(!v.contains(&"启用转发"), "{v:?}");
        assert!(v.contains(&"打开端口转发"), "{v:?}");
    }

    #[test]
    fn 每种行都有复制地址() {
        for (kind, custom, has_db) in [(1, false, false), (2, true, false), (3, false, true)] {
            let v = labels(&row_menu_items(&ctx(kind, custom, has_db)));
            assert!(v.contains(&"复制地址"), "kind={kind} 少了复制地址：{v:?}");
        }
    }

    #[test]
    fn 行内文字必须转义() {
        let m = row_markup("a<b", "u&v", "#111111", "#888888");
        assert!(m.contains("&lt;b"), "名称里的 < 会破坏 markup：{m}");
        assert!(m.contains("u&amp;v"), "地址里的 & 会破坏 markup：{m}");
        assert!(m.contains("foreground='#888888'"), "缺副标题颜色：{m}");
    }

    #[test]
    fn 名字显式上正文色() {
        // ⚠️ 阶段 91a 的教训：不显式上色 ⇒ 用 GTK 默认前景色，
        // 某些系统主题下那个色是白的（白底白字）。
        let m = row_markup("srv-01", "root@10.0.0.1:22", "#111111", "#888888");
        assert!(
            m.contains("<span foreground='#111111'>srv-01</span>"),
            "名字要用正文色显式上色：{m}"
        );
    }

    #[test]
    fn 名字和副标题之间有分隔点() {
        // 以前只隔两个空格 ⇒ 两段看着连成一串
        let m = row_markup("srv-01", "root@10.0.0.1:22", "#111111", "#888888");
        assert!(m.contains("·"), "名字和副标题之间要有分隔点：{m}");
        assert!(
            m.find("srv-01").unwrap() < m.find("root@").unwrap(),
            "名字要在前面：{m}"
        );
    }

    #[test]
    fn 活跃时行首带状态点() {
        // 连上了 → 行首有圆点
        let m = row_markup_st(
            "堡垒机",
            "ops@10.0.0.9:22",
            "#111111",
            "#888888",
            Some(crate::theme::State::Ok),
        );
        assert!(m.starts_with("<span"), "状态点要在行首：{m}");
        assert!(m.contains('●'), "连上了要有圆点：{m}");
        assert!(m.contains("堡垒机"), "名字要还在：{m}");
        // 没状态 → 不带圆点
        let m2 = row_markup_st("堡垒机", "ops@10.0.0.9:22", "#111111", "#888888", None);
        assert!(!m2.contains('●'), "没连上不该有圆点：{m2}");
        // 名字里的尖括号照样要转义
        let m3 = row_markup_st("a<b", "x", "#111111", "#888888", Some(crate::theme::State::Warn));
        assert!(m3.contains("a&lt;b"), "状态点不能破坏转义：{m3}");
    }

    #[test]
    fn 组头是粗体标题加计数徽章() {
        // ⚠️ 阶段 93：组头以前跟条目长得一样（都不粗、计数只是小灰字）
        let m = group_markup("最近", 10, "#e4e8ec", "#1f2328");
        assert!(m.starts_with("<b>最近</b>"), "组头标题要粗体：{m}");
        assert!(m.contains("background='#e4e8ec'"), "计数要有徽章底色：{m}");
        assert!(m.contains("10"), "徽章里要有计数：{m}");
        // 徽章里的数字用**正文色**（次要色压徽章底只有 ~4.1:1，读不清）
        assert!(
            m.contains("foreground='#1f2328'"),
            "徽章里的数字要用正文色：{m}"
        );
        // 0 项也要粗体（以前有项没项两种版式，不一致）
        let m0 = group_markup("自定义", 0, "#e4e8ec", "#1f2328");
        assert!(m0.starts_with("<b>自定义</b>"), "没条目也要粗体：{m0}");
    }

    #[test]
    fn 组头标题里的尖括号要转义() {
        let m = group_markup("a<b&c", 1, "#e4e8ec", "#1f2328");
        assert!(m.contains("a&lt;b&amp;c"), "组头标题要转义：{m}");
    }
}
