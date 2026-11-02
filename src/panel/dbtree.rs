//! 数据库对象浏览器（8.2，Rust-only）：模式 / 表 / 列 / 索引的懒加载树。
//!
//! **懒加载**：展开哪个节点才查哪个节点（`connect_row_expanded` → 发元数据
//! 请求 → 应答回来再挂子节点）。一上来就把整棵树查出来，在大库上是几百条
//! 元数据查询，慢且没用——用户通常只想看一两张表。
//!
//! **节点身份**：树里每行都记一个节点 id（`COL_NODE`），Rust 侧另有一张
//! 节点表（`nodes`）保存"这一行是哪个模式/哪张表"。这样：
//! - 展开时能凭节点 id 找到"该查什么"；
//! - 应答回来时能凭请求内容找到"该填到哪一行"（`node_id_for`）。
//!
//! **父行定位**：请求发出时把父行的 `TreeIter` 克隆下来存在节点里
//! （`Node::iter`）。`TreeIter` 在行不被删除的前提下一直有效——我们只追加
//! 子节点，不删行，所以这个引用是安全的。

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use glib::translate::{ToGlibPtr, ToGlibPtrMut, Uninitialized};
use gtk::gdk;
use gtk::prelude::*;

use super::worker::{DbMetaPayload, DbMetaReq};
use crate::backend::db::DbProto;
use crate::backend::dbmeta;
use std::cell::Cell;

/// 树节点的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// 连接（根）
    Root,
    /// 模式（PG）/ 库（MySQL）
    Schema,
    /// 表
    Table,
    /// 视图
    View,
    /// 列
    Column,
    /// 索引
    Index,
    /// 函数
    Function,
    /// 存储过程
    Procedure,
    /// 触发器
    Trigger,
    /// 事件（定时任务）
    Event,
    /// 占位子节点（"载入中…"）
    ///
    /// 为什么需要它：GTK4 里**没有子节点的行画不出展开箭头**，
    /// 对这样的行调 `expand_row()` 也**不会**发 "row-expanded"（实测）。
    /// 而本树的懒加载正是"展开才查"，于是第一层永远等不到那次触发，
    /// 树会一直停在根节点上（用户看到的就是"连上了却什么都没显示"）。
    /// 办法是：先给可展开的节点挂一个占位子节点，真数据回来再把它换掉。
    Placeholder,
}

impl NodeKind {
    /// 树里显示的短标签（第一列）。
    pub fn badge(self) -> &'static str {
        match self {
            NodeKind::Root => "连接",
            NodeKind::Schema => "模式",
            NodeKind::Table => "表",
            NodeKind::View => "视图",
            NodeKind::Column => "列",
            NodeKind::Index => "索引",
            NodeKind::Function => "函数",
            NodeKind::Procedure => "存储过程",
            NodeKind::Trigger => "触发器",
            NodeKind::Event => "事件",
            NodeKind::Placeholder => "",
        }
    }

    /// 这一类对象的**图标**（Adwaita symbolic 系列）。
    ///
    /// ⚠️ **为什么每类一个图标**（用户 2026-11-02 给的 dbx 截图）：
    /// dbx 的对象树里，表 / 视图 / 函数 / 序列 / 类型各配一个图标 ⇒
    /// **一眼就分得出是什么对象**，不用去读「说明」列里的字。
    /// 我们以前只有一列字（"表" / "视图" / "模式"…）⇒ 得逐行读。
    ///
    /// **图标名必须是系统里确实有的**（`/usr/share/icons/*/symbolic/`），
    /// 写错了 GTK 静默不画 ⇒ 图标列一片空白（看着像"没做"）。
    pub fn icon(self) -> &'static str {
        match self {
            // 连接（= 那个库）⇒ 磁盘（dbx 用的是数据库圆柱，GTK 没有）
            NodeKind::Root => "drive-harddisk-symbolic",
            // 模式（PG）/ 库（MySQL）⇒ 文件夹（跟 dbx 一致）
            NodeKind::Schema => "folder-symbolic",
            // 表 ⇒ 网格（跟 dbx 一致）
            NodeKind::Table => "view-grid-symbolic",
            // 视图 ⇒ 眼睛（跟 dbx 一致）
            NodeKind::View => "view-reveal-symbolic",
            // 列 ⇒ 列表
            NodeKind::Column => "view-list-symbolic",
            // 索引 ⇒ 有序列表
            NodeKind::Index => "view-list-ordered-symbolic",
            // 函数 ⇒ 齿轮（例程/机制）
            NodeKind::Function => "preferences-system-symbolic",
            // 存储过程 ⇒ 文本（跟函数区分开）
            NodeKind::Procedure => "text-x-generic-symbolic",
            // 触发器 ⇒ 闪电（跟 dbx 的事件触发器一致）
            NodeKind::Trigger => "weather-storm-symbolic",
            // 事件（定时任务）⇒ 日历
            NodeKind::Event => "x-office-calendar-symbolic",
            NodeKind::Placeholder => "",
        }
    }

    /// 这一层要不要挂占位子节点（= 能不能展开 = 要不要懒加载）。
    fn lazy(self) -> bool {
        matches!(
            self,
            NodeKind::Root | NodeKind::Schema | NodeKind::Table | NodeKind::View
        )
    }
}

/// 树里显示哪些类型的对象（阶段 51 的「显示」下拉）。
///
/// **对标 dbx 的对象类型过滤**：dbx 的树可以只显示某几类对象。
///
/// ⚠️ **dbx 那个「只看已连接」开关我们没有对应物**——它是因为 dbx 的树
/// 里同时挂着**多个连接**，才需要挑"哪些是连着的"。我们这边**一个工作台
/// 页签只挂一个连接**（连接归连接层管，阶段 13 起），树上永远只有一条，
/// 那个开关就是摆设。换成按类型过滤更有用：大库里几百个存储过程时，
/// 用户常常只想看表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeFilter {
    All,
    Tables,
    Views,
    Routines,
    Triggers,
    Events,
}

/// 过滤下拉里的选项（`id`, 过滤器）。
///
/// **为什么放成常量**：下拉里显示的文字和程序里认的 id 必须一一对应。
/// 抄两份（界面一份、判断一份）迟早会漂——阶段 48 的「并且/或者」下拉
/// 就因为 id 和文字写反了，那个选项形同虚设。
pub const TREE_FILTERS: [(&str, TreeFilter); 6] = [
    ("all", TreeFilter::All),
    ("tables", TreeFilter::Tables),
    ("views", TreeFilter::Views),
    ("routines", TreeFilter::Routines),
    ("triggers", TreeFilter::Triggers),
    ("events", TreeFilter::Events),
];

impl TreeFilter {
    /// 按下拉里的 id 找过滤器（认不出来就当"全部"）。
    pub fn from_id(id: &str) -> TreeFilter {
        TREE_FILTERS
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, f)| *f)
            .unwrap_or(TreeFilter::All)
    }

    /// 下拉框里显示的文字。
    pub fn label(self) -> &'static str {
        match self {
            TreeFilter::All => "全部",
            TreeFilter::Tables => "表",
            TreeFilter::Views => "视图",
            TreeFilter::Routines => "例程",
            TreeFilter::Triggers => "触发器",
            TreeFilter::Events => "事件",
        }
    }

    /// 这个过滤下，这种节点要不要显示。
    ///
    /// **结构节点永远显示**（连接 / 模式 / 列 / 索引 / 占位）——它们是
    /// "路径"，过滤掉树就断了。过滤只作用于**模式下的那一层对象**。
    pub fn allows(self, kind: NodeKind) -> bool {
        match kind {
            NodeKind::Root
            | NodeKind::Schema
            | NodeKind::Column
            | NodeKind::Index
            | NodeKind::Placeholder => true,
            NodeKind::Table => matches!(self, TreeFilter::All | TreeFilter::Tables),
            NodeKind::View => matches!(self, TreeFilter::All | TreeFilter::Views),
            NodeKind::Function | NodeKind::Procedure => {
                matches!(self, TreeFilter::All | TreeFilter::Routines)
            }
            NodeKind::Trigger => matches!(self, TreeFilter::All | TreeFilter::Triggers),
            NodeKind::Event => matches!(self, TreeFilter::All | TreeFilter::Events),
        }
    }
}

const COL_KIND: u32 = 0;
const COL_NAME: u32 = 1;
const COL_DETAIL: u32 = 2;
const COL_SCHEMA: u32 = 3;
const COL_TABLE: u32 = 4;
const COL_NODE: u32 = 5;
/// **图标名**（每类对象一个，见 [`NodeKind::icon`]）。
///
/// ⚠️ 放在**最后**：前面的列号是别的代码按位置取的（`COL_NAME` 等），
/// 插在中间会全体错位。
const COL_ICON: u32 = 6;

/// 这个模式是不是**系统模式**（不该被当成"默认模式"展开）。
///
/// PG 的系统模式里全是系统表 / 系统视图——展开它们对用户毫无意义，
/// 反而把真正要看的业务表挤到下面去。
fn is_system_schema(name: &str) -> bool {
    matches!(
        name,
        // PG
        "information_schema" | "pg_catalog" | "pg_toast"
        // MySQL
        | "mysql" | "performance_schema" | "sys"
    ) || name.starts_with("pg_")
}

/// 一个树节点。
struct Node {
    kind: NodeKind,
    name: String,
    detail: String,
    schema: String,
    table: String,
    /// 例程的身份参数（PG 里同名重载要靠它区分：`f(int)` 与 `f(text)`）
    args: String,
    /// 该节点在树里的行（用于往它下面挂子节点）
    iter: Option<gtk::TreeIter>,
    /// 各类请求各自是否已经查过
    ///
    /// 槽位含义见 `DbMetaReq::kind_index`：0=模式 1=表 2=列 3=索引
    /// 4=外键（树里不用）5=例程 6=触发器 7=定义文本（树里不用）8=事件
    /// 9=搜索 10=对象属性 11=表统计。
    loaded: [bool; 12],
    /// 占位子节点的 id（真数据到了就把它摘掉）
    placeholder: Option<u32>,
    /// 这行已经从树上摘掉（占位子节点被替换后）
    dead: bool,
}

/// 节点的定位信息（双击 / 右键时要知道"这是谁"）。
pub struct NodeInfo {
    pub kind: NodeKind,
    pub name: String,
    pub schema: String,
    pub table: String,
    /// 例程的身份参数（非例程为空）
    pub args: String,
}


/// 右键菜单里的按钮（更新敏感性、测试钩子都要用）。
///
/// **为什么要单独存一份**：菜单项的敏感性按当前选中行的种类变化。
/// 之前是把手势闭包里的按钮 clone 出来逐个 `set_sensitive`——闭包能改，
/// 但"按名字把菜单弹出来"这种钩子拿不到按钮。现在收成一个结构，
/// 敏感性更新和弹菜单两处共用同一份。
struct MenuBtns {
    /// 「查看数据」
    view: gtk::Button,
    /// 「表结构」
    structure: gtk::Button,
    /// 「查看定义」
    def: gtk::Button,
    /// 「对象属性…」
    stats: gtk::Button,
    /// 「导入数据（CSV）…」
    import_csv: gtk::Button,
    /// 「导出表…」（阶段 52）
    export_table: gtk::Button,
    /// 「ER 图…」
    er: gtk::Button,
    /// 「新建表…」
    new: gtk::Button,
    /// 「重命名…」
    rename: gtk::Button,
    /// 「删除表」
    drop_table: gtk::Button,
}

/// 对象浏览器树。
pub struct DbTree {
    pub view: gtk::TreeView,
    store: gtk::TreeStore,
    nodes: RefCell<Vec<Node>>,
    /// 要查元数据时回调（发 worker 命令）
    on_request: RefCell<Option<Box<dyn Fn(DbMetaReq)>>>,
    /// 双击表/视图：打开数据页签（模式、表名、是否视图）
    on_open_table: RefCell<Option<Box<dyn Fn(&str, &str, bool)>>>,
    /// 右键「表结构」：打开表结构页签（模式、表名）
    on_open_structure: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 双击函数/存储过程：打开定义页签（模式、名字、身份参数、是否存储过程）
    on_open_routine: RefCell<Option<Box<dyn Fn(&str, &str, &str, bool)>>>,
    /// 双击触发器：打开定义页签（模式、表名、触发器名）
    on_open_trigger: RefCell<Option<Box<dyn Fn(&str, &str, &str)>>>,
    /// 右键"复制名称"
    on_copy_name: RefCell<Option<Box<dyn Fn(&str)>>>,
    /// 数据库方言（**事件挂哪儿跟它有关**：PG 的事件是库级的，挂在根节点下；
    /// MySQL 的事件挂在库下）
    proto: Cell<DbProto>,
    /// 右键「新建表」：在某个模式下建表（参数 = 模式名）
    on_new_table: RefCell<Option<Box<dyn Fn(&str)>>>,
    /// 右键「重命名」：改表名（参数 = 模式、表名）
    on_rename_table: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 右键「删除表」：删表（参数 = 模式、表名；**二次确认在调用方做**）
    on_drop_table: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 双击事件：打开定义页签（参数 = 模式、事件名）
    on_open_event: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 右键「对象属性…」：看这个对象的属性（参数 = 模式、表名）
    on_object_stats: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 右键「导入数据（CSV）…」（阶段 18）：参数 = 模式、表名
    on_import_csv: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 右键「导出表…」（阶段 52）：参数 = 模式、表名
    on_export_table: RefCell<Option<Box<dyn Fn(&str, &str)>>>,
    /// 右键「ER 图…」（阶段 19）：参数 = 模式、表名（表名传空串 = 整个模式）
    on_er: RefCell<Option<Box<dyn Fn(&str, Option<&str>)>>>,
    /// 右键菜单弹层
    pop: RefCell<Option<gtk::Popover>>,
    /// 右键菜单里的按钮（更新敏感性、测试钩子都要用）
    menu: RefCell<Option<MenuBtns>>,
    /// 展开过的节点名（测试钩子）。
    ///
    /// **为什么自己记**：GTK4 **没有** `gtk_tree_view_row_is_expanded`
    /// （GTK3 有，GTK4 删了）⇒ 想知道"这一行现在展开着没"，只能听
    /// `row-expanded` 信号自己记一笔。
    expanded_nodes: RefCell<Vec<String>>,
    /// **默认要展开的模式**（阶段 72d / 审计 P2-11）。
    ///
    /// **为什么**：连接上以后树只展开到模式这一层，一张表都看不见——用户
    /// 第一眼以为"库里没有表 / 加载失败"，得再点一下模式左边的三角。
    /// PG 是 `public`，MySQL 是当前库。
    default_schema: RefCell<Option<String>>,
    /// 根节点的标签（连接名 + 方言）。
    ///
    /// **为什么单独记一份**：`clear()` 会把节点表整个清空（**根节点也没了**），
    /// 之后 `reload()` 就找不到根节点、直接返回——树再也长不回来
    /// （"CSV 导入完对象树变空白"就是这么来的）。记一份标签，随时能重建。
    root_label: RefCell<(String, String)>,
    /// 「详细模式」：表 / 视图节点上显示行数 / 大小 / 修改时间（阶段 51）
    detail_mode: Cell<bool>,
    /// 对象类型过滤（阶段 51）
    filter: Cell<TreeFilter>,
    me: RefCell<Weak<DbTree>>,
}

impl DbTree {
    pub fn new() -> Rc<DbTree> {
        Rc::new_cyclic(|weak: &Weak<DbTree>| {
            let store = gtk::TreeStore::new(&[
                glib::Type::STRING, // KIND
                glib::Type::STRING, // NAME
                glib::Type::STRING, // DETAIL
                glib::Type::STRING, // SCHEMA
                glib::Type::STRING, // TABLE
                glib::Type::U32,    // NODE
                glib::Type::STRING, // ICON
            ]);
            let view = gtk::TreeView::new();
            view.set_model(Some(&store));
            view.set_headers_visible(true);
            view.set_enable_search(false);
            view.set_show_expanders(true);

            let tree = DbTree {
                view,
                store,
                nodes: RefCell::new(Vec::new()),
                on_request: RefCell::new(None),
                on_open_table: RefCell::new(None),
                on_open_structure: RefCell::new(None),
                on_open_routine: RefCell::new(None),
                on_open_trigger: RefCell::new(None),
                on_copy_name: RefCell::new(None),
                proto: Cell::new(DbProto::Postgres),
                on_new_table: RefCell::new(None),
                on_rename_table: RefCell::new(None),
                on_drop_table: RefCell::new(None),
                on_open_event: RefCell::new(None),
                on_object_stats: RefCell::new(None),
                on_import_csv: RefCell::new(None),
                on_export_table: RefCell::new(None),
                on_er: RefCell::new(None),
                pop: RefCell::new(None),
                menu: RefCell::new(None),
                expanded_nodes: RefCell::new(Vec::new()),
                default_schema: RefCell::new(None),
                root_label: RefCell::new((String::new(), String::new())),
                detail_mode: Cell::new(false),
                filter: Cell::new(TreeFilter::All),
                me: RefCell::new(weak.clone()),
            };
            tree.build_columns();
            tree.wire();
            tree
        })
    }

    fn build_columns(&self) {
        // **只显示两列（名称 / 说明）**，类型信息放在「说明」列里
        // （"表" / "视图" / "模式" / "函数 → integer" / "BEFORE/INSERT · 表名"）。
        //
        // 为什么不单独留一列「类」：GTK4 把**展开箭头和缩进画在第一列**，
        // 树里越深的节点留给文字的宽度越少。类型列本身只有两三个字，
        // 在第二级缩进下就只剩 "…"（实测：宽 30px 的列放不下两个汉字）。
        // 把类型并进「说明」列后，第一列留给最需要空间的「名称」。
        //
        // store 里的 COL_KIND 列还在（节点种类要留着给逻辑判断），
        // 只是不作为显示列——TreeViewColumn 绑定的是 store 的列号，
        // 少显示一列不影响按列号取值。
        for (title, idx) in [("名称", COL_NAME), ("说明", COL_DETAIL)] {
            let r = gtk::CellRendererText::new();
            r.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let c = gtk::TreeViewColumn::new();
            c.set_title(title);
            c.set_resizable(true);
            c.set_expand(true);
            c.set_min_width(140);
            // ⚠️ **图标渲染器跟文字渲染器塞进同一列**（侧边栏也是这么干的）：
            //    GTK4 把**展开箭头和缩进画在第一列** ⇒ 图标要单独占一列的话，
            //    箭头会压在图标上。同列里排（图标 → 文字）就都露得出来。
            if idx == COL_NAME {
                let r_icon = gtk::CellRendererPixbuf::new();
                // 图标与文字之间留点空隙（贴太近看着挤成一坨）
                r_icon.set_padding(4, 2);
                c.pack_start(&r_icon, false);
                c.add_attribute(&r_icon, "icon-name", COL_ICON as i32);
            }
            c.pack_start(&r, true);
            // 必须显式绑定 "text" 属性：GTK4 的 pack_start **只把渲染器塞进列**，
            // 不会把某一列的文本喂给它——不绑就是一片空白（列头还在，看着像"没数据"）。
            // panel/pane.rs 的文件列表树也是这么写的。
            c.add_attribute(&r, "text", idx as i32);
            self.view.append_column(&c);
        }
    }

    fn wire(&self) {
        // 展开 → 该查什么
        let weak = self.me.borrow().clone();
        self.view.connect_row_expanded(move |_, iter, _| {
            let Some(tree) = weak.upgrade() else { return };
            tree.on_expanded(iter);
        });
        // 双击 → 打开表数据
        let weak = self.me.borrow().clone();
        self.view.connect_row_activated(move |_, path, _| {
            let Some(tree) = weak.upgrade() else { return };
            tree.on_activated(path);
        });
        self.install_context_menu();
    }

    /// 清空整棵树（断开连接 / 换连接时；不建根节点，也不发请求）。
    pub fn clear(&self) {
        self.store.clear();
        self.nodes.borrow_mut().clear();
    }

    /// 清空并建根节点（连接成功后调用）。
    pub fn reset(&self, label: &str, detail: &str) {
        self.store.clear();
        self.nodes.borrow_mut().clear();
        // 根节点的标签留一份：`clear()` 之后还能按它重建（见 `root_label`）
        *self.root_label.borrow_mut() = (label.to_string(), detail.to_string());
        let root = self.append_node(None, NodeKind::Root, label, detail, "", "", "");
        // 根节点先挂占位子节点：没有子节点的行在 GTK4 里既画不出展开箭头，
        // expand_row 也不会发 "row-expanded"，懒加载就永远触发不了。
        self.add_placeholder(root as usize);
        // 自动展开根节点 → 触发第一次元数据查询
        if let Some(it) = self.node_iter(root as usize) {
            let p = self.store.path(&it);
            self.view.expand_row(&p, false);
        }
    }

    /// 重新加载整棵树（保留根节点标签，清空子节点后重新懒加载）。
    ///
    /// 树是懒加载的：展开过的节点不会自动重查——用户（或冒烟测试）在库里
    /// 新建了对象后，得刷新一下才看得到。
    pub fn reload(&self) {
        let (label, detail) = {
            let nodes = self.nodes.borrow();
            match nodes.iter().find(|n| n.kind == NodeKind::Root && !n.dead) {
                Some(n) => (n.name.clone(), n.detail.clone()),
                // 根节点被 `clear()` 清掉了 → 按记住的标签重建。
                // **没连过库就没有标签**，那就没什么可刷新的。
                None => {
                    let (l, d) = self.root_label.borrow().clone();
                    if l.is_empty() {
                        return;
                    }
                    (l, d)
                }
            }
        };
        self.reset(&label, &detail);
    }

    /// 刷新某个**模式**节点（重查它的表 / 例程 / 触发器）。
    ///
    /// 建表 / 改名 / 删表之后要用的就是这个——改的是模式下的东西，刷新模式
    /// 这一层就够了（整树 reload 会把用户展开的层级全收起来）。
    pub fn reload_schema(&self, schema: &str) {
        let Some(iter) = self.find_schema(schema) else { return };
        let node_id = self.store.get::<u32>(&iter, COL_NODE as i32) as usize;
        self.remove_children(&iter);
        {
            let mut nodes = self.nodes.borrow_mut();
            if let Some(n) = nodes.get_mut(node_id) {
                n.loaded = [false; 12];
                n.placeholder = None;
            }
        }
        self.load_children(node_id);
    }

    /* ---------------- 阶段 51：详细模式 + 类型过滤 ---------------- */

    /// 开 / 关「详细模式」（表 / 视图节点上显示行数 / 大小 / 修改时间）。
    ///
    /// **关掉时要把说明列还原**：开着的时候表节点的说明列写的是
    /// 「表 · ≈1.2 万 行 · 1.0 MB」，关掉就得回到「表」——不然用户看到的
    /// 还是带数字的旧文案，以为开关没生效。
    pub fn set_detail_mode(&self, on: bool) {
        if self.detail_mode.get() == on {
            return;
        }
        self.detail_mode.set(on);
        if on {
            // 已经展开过的模式要补一次统计查询（没展开过的等用户展开时再查）
            self.request_stats_for_loaded_schemas();
        } else {
            self.reset_details();
        }
    }

    /// 详细模式当前开没开（测试钩子 / 界面回读）。
    pub fn detail_mode(&self) -> bool {
        self.detail_mode.get()
    }

    /// 设置对象类型过滤（阶段 51）。
    ///
    /// **改了要重建整棵树**：`GtkTreeStore` **没法把某一行隐藏起来**——
    /// 只能"当初就别添加"。所以过滤一变就重新拉一遍（`reload()`）。
    /// 代价是展开的层级会收起来：这是 GtkTreeStore 的限制，总比
    /// "下拉显示「表」、树上还挂着几百个存储过程"骗人强。
    pub fn set_filter(&self, f: TreeFilter) {
        if self.filter.get() == f {
            return;
        }
        self.filter.set(f);
        self.reload();
    }

    /// 当前过滤（测试钩子 / 界面回读）。
    pub fn filter(&self) -> TreeFilter {
        self.filter.get()
    }

    /// 给**已经查过表**的模式补发一次统计查询。
    fn request_stats_for_loaded_schemas(&self) {
        let mut schemas: Vec<String> = Vec::new();
        {
            let nodes = self.nodes.borrow();
            for n in nodes.iter() {
                // 槽位 1 = 表：查过表才谈得上"给表补统计"
                if !n.dead && n.kind == NodeKind::Schema && n.loaded[1] {
                    schemas.push(n.name.clone());
                }
            }
        }
        for s in schemas {
            self.send(DbMetaReq::TableStats { schema: s });
        }
    }

    /// 把表 / 视图节点的说明列还原成"表" / "视图"（关掉详细模式时用）。
    fn reset_details(&self) {
        let ids: Vec<(usize, gtk::TreeIter, NodeKind)> = {
            let nodes = self.nodes.borrow();
            nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| !n.dead && matches!(n.kind, NodeKind::Table | NodeKind::View))
                .filter_map(|(i, n)| n.iter.clone().map(|it| (i, it, n.kind)))
                .collect()
        };
        for (i, it, kind) in ids {
            let detail = kind.badge().to_string();
            self.store.set(&it, &[(COL_DETAIL, &detail)]);
            let mut nodes = self.nodes.borrow_mut();
            if let Some(n) = nodes.get_mut(i) {
                n.detail = detail;
            }
        }
    }

    /// 把统计填进表 / 视图节点的说明列（阶段 51）。
    fn apply_table_stats(&self, schema_iter: &gtk::TreeIter, list: &[dbmeta::TableStat]) {
        let map: std::collections::HashMap<&str, &dbmeta::TableStat> =
            list.iter().map(|s| (s.name.as_str(), s)).collect();
        let Some(mut child) = self.store.iter_children(Some(schema_iter)) else {
            return;
        };
        loop {
            let node_id = self.store.get::<u32>(&child, COL_NODE as i32) as usize;
            let (kind, name) = {
                let nodes = self.nodes.borrow();
                match nodes.get(node_id) {
                    Some(n) => (n.kind, n.name.clone()),
                    None => (NodeKind::Table, String::new()),
                }
            };
            if matches!(kind, NodeKind::Table | NodeKind::View) {
                if let Some(st) = map.get(name.as_str()) {
                    let detail = dbmeta::fmt_table_detail(kind.badge(), st);
                    self.store.set(&child, &[(COL_DETAIL, &detail)]);
                    let mut nodes = self.nodes.borrow_mut();
                    if let Some(n) = nodes.get_mut(node_id) {
                        n.detail = detail;
                    }
                }
            }
            if !self.store.iter_next(&mut child) {
                break;
            }
        }
    }

    /// 找到某个模式节点所在的位置（按名字找）。
    fn find_schema(&self, schema: &str) -> Option<gtk::TreeIter> {
        let mut it = self.store.iter_first()?;
        loop {
            let info = self.node_info(&it);
            if let Some(i) = info {
                if i.kind == NodeKind::Schema && i.schema == schema {
                    return Some(it);
                }
            }
            if let Some(mut c) = self.store.iter_children(Some(&it)) {
                loop {
                    let info = self.node_info(&c);
                    if let Some(i) = info {
                        if i.kind == NodeKind::Schema && i.schema == schema {
                            return Some(c);
                        }
                    }
                    if !self.store.iter_next(&mut c) {
                        break;
                    }
                }
            }
            if !self.store.iter_next(&mut it) {
                break;
            }
        }
        None
    }

    /// 取一行的节点信息（名字 / 种类 / 模式）。
    fn node_info(&self, it: &gtk::TreeIter) -> Option<NodeInfo> {
        let node_id = self.store.get::<u32>(it, COL_NODE as i32) as usize;
        let nodes = self.nodes.borrow();
        let n = nodes.get(node_id)?;
        Some(NodeInfo {
            kind: n.kind,
            schema: n.schema.clone(),
            name: n.name.clone(),
            table: n.table.clone(),
            args: n.args.clone(),
        })
    }

    /// 刷新当前选中节点这一层（重新查它的子对象）。
    ///
    /// **为什么不做整树 reload**：整树重建会把用户展开的层级全收起来，
    /// 还得重新点一遍；局部刷新只重查当前这一层（模式 → 表 / 例程 / 触发器），
    /// 展开状态保持不变。
    pub fn reload_current(&self) {
        let Some(path) = self.selected_path() else { return };
        let Some(model) = self.view.model() else { return };
        let Some(iter) = model.iter(&path) else { return };
        let node_id = self.store.get::<u32>(&iter, COL_NODE as i32) as usize;
        // 旧的子节点（表 / 例程 / 触发器）先摘掉
        self.remove_children(&iter);
        {
            let mut nodes = self.nodes.borrow_mut();
            if let Some(n) = nodes.get_mut(node_id) {
                n.loaded = [false; 12];
                n.placeholder = None;
            }
        }
        // 再查一遍这一层
        self.load_children(node_id);
    }

    /// 删掉某一行下面的所有子节点（并把它们的记录标成 dead）。
    fn remove_children(&self, parent: &gtk::TreeIter) {
        loop {
            let Some(child) = self.store.iter_children(Some(parent)) else {
                break;
            };
            let id = self.store.get::<u32>(&child, COL_NODE as i32) as usize;
            self.store.remove(&child);
            let mut nodes = self.nodes.borrow_mut();
            if let Some(n) = nodes.get_mut(id) {
                n.dead = true;
                n.iter = None;
            }
        }
    }

    /// 当前方言（事件挂哪儿要按它分）。
    pub fn proto(&self) -> DbProto {
        self.proto.get()
    }

    /// 设置方言（连接类型变了要跟着变）。
    pub fn set_proto(&self, p: DbProto) {
        self.proto.set(p);
    }

    /// 设「默认要展开的模式」（阶段 72d / 审计 P2-11）。
    ///
    /// 模式列表回来的时候，这一个会被自动展开 ⇒ 用户一连上就看得见表。
    /// 传 `None` = 不自动展开（跟以前一样）。
    pub fn set_default_schema(&self, name: Option<String>) {
        *self.default_schema.borrow_mut() = name.filter(|s| !s.is_empty());
    }

    /// 展开某一节（阶段 72d）。
    ///
    /// ⚠️ **要等占位子节点挂上再展开**：GTK4 里**没有子节点的行**既画不出
    /// 展开箭头，`expand_row()` 也是静默无效（不发 `row-expanded`）⇒ 懒
    /// 加载的请求永远发不出去（见 `add_placeholder` 的注释）。
    fn expand_node(&self, node_id: usize) {
        let Some(it) = self.node_iter(node_id) else { return };
        let p = self.store.path(&it);
        self.view.expand_row(&p, false);
    }

    /// **下一轮主循环**再展开（阶段 72d）。
    ///
    /// ⚠️ **为什么不能当场展开**：GTK4 的 `expand_row()` 在**模型正被改**
    /// 的时候是无效的——`fill()` 里刚把模式行挂上去就展开，`expand_row()`
    /// 直接返回 `false`，`row-expanded` 一个信号都不发 ⇒ 表永远查不出来
    /// （实测：当场展开请求列表空空如也，事后展开立刻就发 `Tables` 了）。
    ///
    /// ⚠️ **`idle_add_local_once` 只能在主线程调**（非主线程直接 panic）⇒
    /// 先问一句"当前线程是不是主循环的拥有者"，不是就当场展开（单测里没有
    /// 主循环，当场展开也一样）。
    fn expand_later(&self, node_id: usize) {
        if !glib::MainContext::default().is_owner() {
            self.expand_node(node_id);
            return;
        }
        let weak = self.me.borrow().clone();
        glib::idle_add_local_once(move || {
            // ⚠️ **抓弱引用**：闭包抓强引用就是 `DbTree → 闭包 → DbTree` 的环。
            let Some(tree) = weak.upgrade() else { return };
            tree.expand_node(node_id);
        });
    }

    /// 节点在树里的行。
    fn node_iter(&self, node_id: usize) -> Option<gtk::TreeIter> {
        self.nodes.borrow().get(node_id).and_then(|n| n.iter.clone())
    }

    /// 给节点挂一个占位子节点（"载入中…"）。
    ///
    /// 占位子节点的作用是"让这一行有孩子"，从而：
    /// - GTK4 画出展开箭头，用户能点；
    /// - `expand_row()` 会发 "row-expanded"，懒加载的请求才发得出去。
    fn add_placeholder(&self, node_id: usize) {
        let kind = self.nodes.borrow().get(node_id).map(|n| n.kind);
        if !kind.map(NodeKind::lazy).unwrap_or(false) {
            return;
        }
        let Some(parent) = self.node_iter(node_id) else {
            return;
        };
        let ph = self.append_node(Some(&parent), NodeKind::Placeholder, "载入中…", "", "", "", "");
        self.nodes.borrow_mut()[node_id].placeholder = Some(ph);
    }

    /// 摘掉占位子节点（真数据到了 / 查失败了）。
    fn drop_placeholder(&self, node_id: usize) {
        let ph = self.nodes.borrow().get(node_id).and_then(|n| n.placeholder);
        let Some(ph) = ph else { return };
        // 先摘行再清引用：remove 会释放那一行，iter 随即失效。
        if let Some(it) = self.node_iter(ph as usize) {
            self.store.remove(&it);
        }
        let mut nodes = self.nodes.borrow_mut();
        if let Some(n) = nodes.get_mut(ph as usize) {
            n.dead = true;
            n.iter = None;
        }
        if let Some(n) = nodes.get_mut(node_id) {
            n.placeholder = None;
        }
    }

    /// 这次查询失败了：占位子节点改成"加载失败"，并允许重试。
    pub fn mark_failed(&self, req: &DbMetaReq, err: &str) {
        let Some(node_id) = self.node_id_for(req) else {
            return;
        };
        // 允许重试：清掉"查过了"的标记，用户再展开一次就重发请求
        let slot = match req {
            DbMetaReq::Schemas => 0,
            DbMetaReq::Tables { .. } => 1,
            DbMetaReq::Columns { .. } => 2,
            DbMetaReq::Indexes { .. } => 3,
            // 外键不占懒加载槽位（表结构页签单独查）
            DbMetaReq::ForeignKeys { .. } => return,
            // 例程 / 触发器与表共用模式节点，失败标记落在同一个槽位上
            DbMetaReq::Routines { .. } => 5,
            DbMetaReq::Triggers { .. } => 6,
            DbMetaReq::RoutineDef { .. } | DbMetaReq::TriggerDef { .. } => return,
            DbMetaReq::EventDef { .. } => return,
            // 事件：只有 MySQL 有，挂在库下，占槽位 8
            DbMetaReq::Events { .. } => 8,
            // 搜索不挂在树上，没有"重查"的概念
            DbMetaReq::Search { .. } => return,
            // 对象属性是弹窗里看的，也不挂在树上
            DbMetaReq::ObjectStats { .. } => return,
            // 表统计：挂在模式节点上，失败只清它自己的槽位（重新展开会重发）
            DbMetaReq::TableStats { .. } => 11,
        };
        self.nodes.borrow_mut()[node_id].loaded[slot] = false;
        // 把占位子节点的文字换成失败提示（行留着，展开箭头也留着）
        let ph = self.nodes.borrow()[node_id].placeholder;
        if let Some(ph) = ph {
            if let Some(it) = self.node_iter(ph as usize) {
                let name = "加载失败".to_string();
                let detail = err.to_string();
                self.store.set(&it, &[(COL_NAME, &name), (COL_DETAIL, &detail)]);
            }
        }
    }

    /// 节点被展开：还没查过就发请求。
    ///
    /// **按槽位独立判断**：每类请求各占一个槽位（见 `DbMetaReq::kind_index`），
    /// 某一类失败时只把它自己的槽位清掉（`mark_failed`），重新展开时只重发
    /// 那一类——而不是把表、例程、触发器一起重查。
    fn on_expanded(&self, iter: &gtk::TreeIter) {
        let node_id = self.store.get::<u32>(iter, COL_NODE as i32) as usize;
        // 记一笔"这一行展开过了"（GTK4 没有 `row_is_expanded`，只能自己听信号）
        {
            let name = self
                .nodes
                .borrow()
                .get(node_id)
                .map(|n| n.name.clone())
                .unwrap_or_default();
            self.expanded_nodes.borrow_mut().push(name);
        }
        // 记下父行（应答回来时往它下面挂子节点）
        self.nodes.borrow_mut()[node_id].iter = Some(iter.clone());
        self.load_children(node_id);
    }

    /// 按节点种类发对应的元数据请求（槽位没占过才发）。
    ///
    /// **单独抽出来**：`on_expanded`（展开时）和「刷新」都要走这一段——
    /// 展开是"第一次查"，刷新是"清掉旧的再查一遍"，发请求的部分完全一样。
    fn load_children(&self, node_id: usize) {
        let (kind, schema, table) = {
            let nodes = self.nodes.borrow();
            let Some(n) = nodes.get(node_id) else { return };
            (n.kind, n.schema.clone(), n.table.clone())
        };
        match kind {
            NodeKind::Root => {
                if self.take_slot(node_id, 0) {
                    self.send(DbMetaReq::Schemas);
                }
            }
            NodeKind::Schema => {
                // 模式下有三类对象：表、函数/存储过程、触发器（Navicat 的对象
                // 浏览器同样按类目铺开）。三类各查各的，谁先回来谁先挂上去。
                //
                // ⚠️ **过滤掉的类型连查都不查**（阶段 51）：用户选了「只看表」，
                // 就不该为了几百个存储过程白跑一趟查询。
                let f = self.filter.get();
                if f.allows(NodeKind::Table) && self.take_slot(node_id, 1) {
                    self.send(DbMetaReq::Tables {
                        schema: schema.clone(),
                    });
                }
                if f.allows(NodeKind::Function) && self.take_slot(node_id, 5) {
                    self.send(DbMetaReq::Routines {
                        schema: schema.clone(),
                    });
                }
                if f.allows(NodeKind::Trigger) && self.take_slot(node_id, 6) {
                    self.send(DbMetaReq::Triggers {
                        schema: schema.clone(),
                    });
                }
                // 事件挂在库（= schema）下面，跟表平铺。
                // **只有 MySQL 有定时事件**（PostgreSQL 没有这功能），所以这里
                // 只在支持的方言上发请求——PG 展开了也不会多出这一类节点。
                if dbmeta::proto_supports_events(self.proto())
                    && f.allows(NodeKind::Event)
                    && self.take_slot(node_id, 8)
                {
                    self.send(DbMetaReq::Events {
                        schema: schema.clone(),
                    });
                }
                // 详细模式（阶段 51）：**这个模式下的表统计一次查完**。
                //
                // 放在表的请求**之后**发：worker 按命令顺序处理，表先回来，
                // 统计回来时树上的表行已经在了，填得进去。
                if self.detail_mode.get() && self.take_slot(node_id, 11) {
                    self.send(DbMetaReq::TableStats {
                        schema: schema.clone(),
                    });
                }
            }
            NodeKind::Table | NodeKind::View => {
                if self.take_slot(node_id, 2) {
                    self.send(DbMetaReq::Columns {
                        schema: schema.clone(),
                        table: table.clone(),
                    });
                }
                // 索引放在列之后发：worker 按命令顺序处理，列先回来，
                // 树里的顺序才是"列 → 索引"，反过来索引会顶到列前面。
                if self.take_slot(node_id, 3) {
                    self.send(DbMetaReq::Indexes {
                        schema: schema.clone(),
                        table: table.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// 占一个槽位：还没查过就返回 true 并记下（同类请求只发一次）。
    fn take_slot(&self, node_id: usize, slot: usize) -> bool {
        let mut nodes = self.nodes.borrow_mut();
        let Some(n) = nodes.get_mut(node_id) else { return false };
        if n.loaded[slot] {
            return false;
        }
        n.loaded[slot] = true;
        true
    }

    /// 发一次元数据请求。
    fn send(&self, req: DbMetaReq) {
        if let Some(cb) = self.on_request.borrow().as_ref() {
            cb(req);
        }
    }

    /// 双击一行：表/视图 → 打开数据；函数/存储过程/触发器 → 打开定义。
    fn on_activated(&self, path: &gtk::TreePath) {
        let Some(info) = self.node_at(path) else {
            return;
        };
        match info.kind {
            NodeKind::Table | NodeKind::View => {
                if let Some(cb) = self.on_open_table.borrow().as_ref() {
                    cb(&info.schema, &info.table, info.kind == NodeKind::View);
                }
            }
            NodeKind::Function | NodeKind::Procedure => {
                if let Some(cb) = self.on_open_routine.borrow().as_ref() {
                    cb(
                        &info.schema,
                        &info.name,
                        &info.args,
                        info.kind == NodeKind::Procedure,
                    );
                }
            }
            NodeKind::Trigger => {
                if let Some(cb) = self.on_open_trigger.borrow().as_ref() {
                    cb(&info.schema, &info.table, &info.name);
                }
            }
            NodeKind::Event => {
                if let Some(cb) = self.on_open_event.borrow().as_ref() {
                    cb(&info.schema, &info.name);
                }
            }
            _ => {}
        }
    }

    /// 节点信息（按路径）。
    fn node_at(&self, path: &gtk::TreePath) -> Option<NodeInfo> {
        // 注意：TreePath 的索引是"层级路径"，不是行号，所以要走 model 查。
        let Some(model) = self.view.model() else { return None };
        let Some(iter) = model.iter(path) else {
            return None;
        };
        let node_id = self.store.get::<u32>(&iter, COL_NODE as i32) as usize;
        let nodes = self.nodes.borrow();
        let n = nodes.get(node_id)?;
        Some(NodeInfo {
            kind: n.kind,
            name: n.name.clone(),
            schema: n.schema.clone(),
            table: n.table.clone(),
            args: n.args.clone(),
        })
    }

    /// 元数据应答填回树里。
    pub fn fill(&self, req: &DbMetaReq, payload: &DbMetaPayload) {
        let Some(node_id) = self.node_id_for(req) else {
            return;
        };
        // 请求里的模式名（例程/触发器要挂回它所属的模式）
        let req_schema = match req {
            DbMetaReq::Tables { schema }
            | DbMetaReq::Routines { schema }
            | DbMetaReq::Triggers { schema }
            | DbMetaReq::Events { schema } => schema.clone(),
            _ => String::new(),
        };
        let Some(parent) = self.nodes.borrow()[node_id].iter.clone() else {
            return;
        };
        // 真数据到了：先把"载入中…"的占位子节点摘掉
        self.drop_placeholder(node_id);
        match payload {
            DbMetaPayload::Schemas(list) => {
                // 阶段 72d / 审计 P2-11：默认模式**自动展开**——
                // 连上以后树里一张表都看不见，用户第一眼以为"库里没表"。
                let want = self.default_schema.borrow().clone();
                // 名字 → 节点 id（用来在列表里找"该展开哪个"）
                let mut ids: Vec<(String, usize)> = Vec::new();
                let mut hit: Option<usize> = None;
                for s in list {
                    let id = self.append_node(
                        Some(&parent),
                        NodeKind::Schema,
                        s,
                        "模式",
                        s,
                        "",
                        "",
                    );
                    // 模式/表/视图都要能展开 → 挂占位子节点（见 NodeKind::Placeholder）
                    self.add_placeholder(id as usize);
                    if hit.is_none() && want.as_deref() == Some(s.as_str()) {
                        hit = Some(id as usize);
                    }
                    ids.push((s.clone(), id as usize));
                }
                // 默认模式在列表里就展开它；不在（PG 删过 `public`、MySQL 库名
                // 对不上）就展开**第一个不是系统模式的**——宁可展开一个也别让
                // 用户对着空树发愣，但 `pg_catalog` / `information_schema` 这种
                // 系统模式展开了就是一屏系统表，比不展开还糟。
                let fallback = ids
                    .iter()
                    .find(|(name, _)| !is_system_schema(name))
                    .map(|(_, id)| *id);
                if let Some(n) = hit.or(fallback) {
                    self.expand_later(n);
                }
            }
            DbMetaPayload::Tables(list) => {
                let f = self.filter.get();
                for t in list {
                    let kind = if t.is_view {
                        NodeKind::View
                    } else {
                        NodeKind::Table
                    };
                    // 过滤掉的类型**根本不挂节点**（阶段 51）：
                    // GtkTreeStore 没法隐藏行，只能"别添加"。
                    if !f.allows(kind) {
                        continue;
                    }
                    let id = self.append_node(
                        Some(&parent),
                        kind,
                        &t.name,
                        if t.is_view { "视图" } else { "表" },
                        &t.schema,
                        &t.name,
                        "",
                    );
                    self.add_placeholder(id as usize);
                }
            }
            DbMetaPayload::Columns(list) => {
                for c in list {
                    let mut detail = c.data_type.clone();
                    if c.is_pk {
                        detail.push_str(" · 主键");
                    }
                    if !c.nullable {
                        detail.push_str(" · 非空");
                    }
                    self.append_node(
                        Some(&parent),
                        NodeKind::Column,
                        &c.name,
                        &detail,
                        "",
                        "",
                        "",
                    );
                }
            }
            DbMetaPayload::Routines(list) => {
                let f = self.filter.get();
                for r in list {
                    let kind = match r.kind {
                        crate::backend::dbmeta::RoutineKind::Function => NodeKind::Function,
                        crate::backend::dbmeta::RoutineKind::Procedure => NodeKind::Procedure,
                    };
                    if !f.allows(kind) {
                        continue;
                    }
                    let mut detail = r.kind.label().to_string();
                    if !r.args.is_empty() {
                        detail.push_str(&format!("({})", r.args));
                    }
                    if !r.returns.is_empty() {
                        detail.push_str(&format!(" → {}", r.returns));
                    }
                    self.append_node(
                        Some(&parent),
                        kind,
                        &r.name,
                        &detail,
                        &req_schema,
                        "",
                        &r.args,
                    );
                }
            }
            DbMetaPayload::Triggers(list) => {
                let f = self.filter.get();
                for t in list {
                    if !f.allows(NodeKind::Trigger) {
                        continue;
                    }
                    let detail = format!("{}/{} · {}", t.timing, t.event, t.table);
                    self.append_node(
                        Some(&parent),
                        NodeKind::Trigger,
                        &t.name,
                        &detail,
                        &req_schema,
                        &t.table,
                        "",
                    );
                }
            }
            DbMetaPayload::Events(list) => {
                let f = self.filter.get();
                for e in list {
                    if !f.allows(NodeKind::Event) {
                        continue;
                    }
                    // 说明列写清楚"什么时候跑、什么状态"（事件本身没有列）
                    let mut detail = String::from("事件");
                    if !e.schedule.is_empty() {
                        detail.push_str(&format!(" · {}", e.schedule));
                    }
                    if !e.status.is_empty() {
                        detail.push_str(&format!(" · {}", e.status));
                    }
                    self.append_node(
                        Some(&parent),
                        NodeKind::Event,
                        &e.name,
                        &detail,
                        &req_schema,
                        "",
                        "",
                    );
                }
            }
            // 定义文本不进树（双击时单独查，见 dbui 的定义页签）
            DbMetaPayload::Def(_) => {}
            // 表统计（阶段 51）：填进表 / 视图节点的说明列
            DbMetaPayload::TableStats(list) => {
                self.apply_table_stats(&parent, &list);
            }
            // 搜索结果不进树：它不是"这个库有什么"，单独一个结果窗口
            DbMetaPayload::SearchHits(_) => {}
            // 对象属性不进树：它在属性窗口里显示
            DbMetaPayload::ObjectStats(_) => {}
            // 外键不进对象树：它是表结构页签的内容
            DbMetaPayload::ForeignKeys(_) => {}
            DbMetaPayload::Indexes(list) => {
                for i in list {
                    // 说明列写「列名（唯一）」——索引节点在树上只是给人看的，
                    // 结构页签里才让它可编辑
                    let desc = if i.unique {
                        format!("{}（唯一）", i.columns.join(", "))
                    } else {
                        i.columns.join(", ")
                    };
                    self.append_node(
                        Some(&parent),
                        NodeKind::Index,
                        &i.name,
                        &desc,
                        "",
                        "",
                        "",
                    );
                }
            }
        }
        // 把父行重新展开。
        // 为什么必须补这一步：摘掉占位子节点时，这一行在 GTK 眼里会变成
        // "没有孩子"，行就被折叠了；真子节点挂上来之后如果不重新展开，
        // 用户看到的还是空的（而且展开子行也没用——父行是折叠的，
        // GTK4 里展开一个父行折叠的行是**静默无效**的）。
        // 重新展开会再发一次 row-expanded，但 loaded 标记已置位，不会重复发请求。
        if let Some(it) = self.node_iter(node_id) {
            let p = self.store.path(&it);
            self.view.expand_row(&p, false);
        }
    }

    /// 请求 → 目标节点 id。
    fn node_id_for(&self, req: &DbMetaReq) -> Option<usize> {
        let nodes = self.nodes.borrow();
        match req {
            DbMetaReq::Schemas => nodes.iter().position(|n| !n.dead && n.kind == NodeKind::Root),
            DbMetaReq::Tables { schema }
            | DbMetaReq::Routines { schema }
            | DbMetaReq::Triggers { schema } => nodes
                .iter()
                .position(|n| !n.dead && n.kind == NodeKind::Schema && &n.name == schema),
            DbMetaReq::Columns { schema, table }
            | DbMetaReq::Indexes { schema, table }
            | DbMetaReq::ForeignKeys { schema, table } => nodes.iter().position(|n| {
                !n.dead
                    && matches!(n.kind, NodeKind::Table | NodeKind::View)
                    && &n.name == table
                    && &n.schema == schema
            }),
            // 事件：只有 MySQL 有，挂在库（= schema）节点下
            DbMetaReq::Events { schema } => nodes
                .iter()
                .position(|n| !n.dead && n.kind == NodeKind::Schema && &n.name == schema),
            // 定义文本不进树：双击例程/触发器/事件时单独查，填进"定义页签"
            DbMetaReq::RoutineDef { .. } | DbMetaReq::TriggerDef { .. } | DbMetaReq::EventDef { .. } => {
                None
            }
            // 搜索不属于任何树节点（结果单独一个窗口）
            DbMetaReq::Search { .. } => None,
            // 对象属性也不属于任何树节点（属性窗口里显示）
            DbMetaReq::ObjectStats { .. } => None,
            // 表统计：挂在**模式**节点上（一次查完整个模式，阶段 51）
            DbMetaReq::TableStats { schema } => nodes
                .iter()
                .position(|n| !n.dead && n.kind == NodeKind::Schema && &n.name == schema),
        }
    }

    /// 右键菜单上的所有文字（测试钩子）。
    pub fn menu_labels(&self) -> Vec<String> {
        let b = self.menu.borrow();
        let Some(m) = b.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for btn in [
            &m.view,
            &m.structure,
            &m.def,
            &m.stats,
            &m.import_csv,
            &m.export_table,
            &m.er,
            &m.new,
            &m.rename,
            &m.drop_table,
        ] {
            // ⚠️ **菜单项按钮的 child 是一层 Box**（图标 + 文字 Label），
            // 直接 `child().downcast_ref::<Label>()` 取不到文字——
            // 得用 `menu::item_label`（它会钻进去找带 `pp-menu-label` 的那个）。
            if let Some(t) = crate::menu::item_label(btn) {
                out.push(t);
            }
        }
        out
    }

    /// 树上**已经加载出来**的表 / 视图（`(模式, 名字, 是视图)`）。
    ///
    /// 导入 / 导出向导（阶段 52，阶段 72 改名）用它来填"选哪张表"的下拉。
    ///
    /// ⚠️ **只给已经加载出来的**：树是懒加载的，用户没展开的模式这里没有。
    /// 这是有意的——向导是**跟着对象树用的**，用户想导哪张表就先去把它
    /// 展开出来。（要"列全库所有表"得再发一轮元数据查询，那会让向导
    /// 多一个"加载中"状态，不值当。）
    pub fn objects(&self) -> Vec<(String, String, bool)> {
        let mut out = Vec::new();
        for n in self.nodes.borrow().iter() {
            if n.dead {
                continue;
            }
            match n.kind {
                NodeKind::Table => out.push((n.schema.clone(), n.name.clone(), false)),
                NodeKind::View => out.push((n.schema.clone(), n.name.clone(), true)),
                _ => {}
            }
        }
        out
    }

    /// 树上的所有行（`名字`, `说明`）——测试钩子 / 诊断用。
    ///
    /// **为什么按路径走而不是 iter_next**：GTK4 的 `TreeIter` 在对子行调
    /// `iter_children` 之后**上层那个 iter 就失效了**，再 `iter_next` 直接
    /// 返回 false——树只走了一半（AGENTS.md 记的老坑）。按路径走最保险。
    pub fn rows(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let n = self.store.iter_n_children(None);
        for i in 0..n {
            let p = gtk::TreePath::from_indices(&[i]);
            self.collect_rows(&p, &mut out);
        }
        out
    }

    fn collect_rows(&self, path: &gtk::TreePath, out: &mut Vec<(String, String)>) {
        let Some(iter) = self.store.iter(path) else { return };
        let name = self.store.get::<String>(&iter, COL_NAME as i32);
        let detail = self.store.get::<String>(&iter, COL_DETAIL as i32);
        out.push((name, detail));
        let n = self.store.iter_n_children(Some(&iter));
        for i in 0..n {
            let mut p = path.clone();
            p.append_index(i);
            self.collect_rows(&p, out);
        }
    }

    /// 挂一个子节点。
    #[allow(clippy::too_many_arguments)]
    fn append_node(
        &self,
        parent: Option<&gtk::TreeIter>,
        kind: NodeKind,
        name: &str,
        detail: &str,
        schema: &str,
        table: &str,
        args: &str,
    ) -> u32 {
        let kind_s = kind.badge().to_string();
        let icon_s = kind.icon().to_string();
        let name_s = name.to_string();
        let detail_s = detail.to_string();
        let schema_s = schema.to_string();
        let table_s = table.to_string();
        let args_s = args.to_string();
        let id = self.nodes.borrow().len() as u32;
        let it = self.store.append(parent);
        self.store.set(
            &it,
            &[
                (COL_KIND, &kind_s),
                (COL_NAME, &name_s),
                (COL_DETAIL, &detail_s),
                (COL_SCHEMA, &schema_s),
                (COL_TABLE, &table_s),
                (COL_NODE, &id),
                (COL_ICON, &icon_s),
            ],
        );
        self.nodes.borrow_mut().push(Node {
            kind,
            name: name_s,
            detail: detail_s,
            schema: schema_s,
            table: table_s,
            args: args_s,
            iter: Some(it),
            loaded: [false; 12],
            placeholder: None,
            dead: false,
        });
        id
    }

    /* ---------------- 回调注册 ---------------- */

    pub fn set_request_handler(&self, f: impl Fn(DbMetaReq) + 'static) {
        *self.on_request.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_open_table_handler(&self, f: impl Fn(&str, &str, bool) + 'static) {
        *self.on_open_table.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「表结构」：打开表结构页签（Navicat 的设计器）。
    pub fn set_open_structure_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_open_structure.borrow_mut() = Some(Box::new(f));
    }

    /// 双击函数 / 存储过程：打开定义页签。
    ///
    /// 参数是 `(模式, 名字, 身份参数, 是否存储过程)`。身份参数（PG 的
    /// `pg_get_function_identity_arguments`）用来区分同名重载。
    pub fn set_open_routine_handler(&self, f: impl Fn(&str, &str, &str, bool) + 'static) {
        *self.on_open_routine.borrow_mut() = Some(Box::new(f));
    }

    /// 双击触发器：打开定义页签。参数是 `(模式, 表名, 触发器名)`。
    pub fn set_open_trigger_handler(&self, f: impl Fn(&str, &str, &str) + 'static) {
        *self.on_open_trigger.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_copy_handler(&self, f: impl Fn(&str) + 'static) {
        *self.on_copy_name.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「新建表」：在某个模式下建表。
    pub fn set_new_table_handler(&self, f: impl Fn(&str) + 'static) {
        *self.on_new_table.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「重命名」：改表名。
    pub fn set_rename_table_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_rename_table.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「删除表」：删表（**二次确认由调用方做**）。
    pub fn set_drop_table_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_drop_table.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「对象属性…」：看对象的属性（参数 = 模式、表名）
    pub fn set_object_stats_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_object_stats.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「导入数据（CSV）…」（阶段 18）：把 CSV 导进这张表。
    pub fn set_import_csv_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_import_csv.borrow_mut() = Some(Box::new(f));
    }

    /// 右键「导出表…」（阶段 52）：把这张表（**整张**，不止当前页）导出到文件。
    pub fn set_export_table_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_export_table.borrow_mut() = Some(Box::new(f));
    }

    /// 注册「ER 图…」的回调（阶段 19）。
    ///
    /// 参数 = `(模式, 表名)`；表名传 `None` 表示画**整个模式**。
    pub fn set_er_handler(&self, f: impl Fn(&str, Option<&str>) + 'static) {
        *self.on_er.borrow_mut() = Some(Box::new(f));
    }

    /// 双击事件：打开定义页签。
    pub fn set_open_event_handler(&self, f: impl Fn(&str, &str) + 'static) {
        *self.on_open_event.borrow_mut() = Some(Box::new(f));
    }

    /* ---------------- 右键菜单 ---------------- */

    fn install_context_menu(&self) {
        // ⚠️ 统一走 `crate::menu`：之前这里是手搓的 `Button::with_label` +
        // `flat` 类——没有图标、没有 `pp-menu` 卡片样式，**鼠标划过也不高亮**
        // （用户 2026-10-28 报的「左侧列表菜单划过没高亮」就是这里）。
        // `menu::popover` 会把弹层挂到 `self.view` 上，跟原来一样。
        let pop = crate::menu::popover(&self.view);
        let vb = crate::menu::body();

        let b_view = crate::menu::item("view-list-symbolic", "查看数据", None);
        // 表结构（设计器）：Navicat 里双击表看数据、设计器在右键菜单里
        let b_struct = crate::menu::item("view-dual-symbolic", "表结构", None);
        // 查看定义：函数 / 存储过程 / 触发器
        let b_def = crate::menu::item("text-x-generic-symbolic", "查看定义", None);
        let b_copy = crate::menu::item("edit-copy-symbolic", "复制名称", None);
        // 对象属性（13.5）：Navicat 的 Details 面板
        let b_stats = crate::menu::item("dialog-information-symbolic", "对象属性…", None);
        // 导入 CSV（阶段 18）：Navicat 的 Import Wizard
        let b_import = crate::menu::item("document-import-symbolic", "导入数据（CSV）…", None);
        // 导出表（阶段 52）：整张表写进本地文件（dbx 那边叫「数据传输」）
        let b_export = crate::menu::item("document-export-symbolic", "导出表…", None);
        // ER 图（阶段 19）：Navicat 的 Database Model
        let b_er = crate::menu::item("network-transmit-receive-symbolic", "ER 图…", None);
        // DDL 操作（9.6）：新建表 / 重命名 / 删除表 / 刷新
        let b_new = crate::menu::item("list-add-symbolic", "新建表…", None);
        let b_rename = crate::menu::item("document-edit-symbolic", "重命名…", None);
        let b_drop = crate::menu::item("user-trash-symbolic", "删除表", None);
        // 删除表不可逆：红字 + 红图标（与终端菜单里的「关闭分屏」同一套）
        crate::menu::danger(&b_drop);
        let b_refresh = crate::menu::item("view-refresh-symbolic", "刷新", None);
        // 分组：看数据 / 数据工具 / DDL / 刷新
        vb.append(&b_view);
        vb.append(&b_struct);
        vb.append(&b_def);
        vb.append(&b_copy);
        vb.append(&b_stats);
        vb.append(&crate::menu::sep());
        vb.append(&b_import);
        vb.append(&b_export);
        vb.append(&b_er);
        vb.append(&crate::menu::sep());
        vb.append(&b_new);
        vb.append(&b_rename);
        vb.append(&b_drop);
        vb.append(&crate::menu::sep());
        vb.append(&b_refresh);
        pop.set_child(Some(&vb));

        let weak = self.me.borrow().clone();
        b_view.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else {
                return;
            };
            if matches!(info.kind, NodeKind::Table | NodeKind::View) {
                if let Some(cb) = tree.on_open_table.borrow().as_ref() {
                    cb(&info.schema, &info.table, info.kind == NodeKind::View);
                }
            }
            tree.close_popover();
        });
        let weak = self.me.borrow().clone();
        b_struct.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else {
                return;
            };
            // 视图没有字段/索引这一套（结构是查询定义的），不给表结构页签
            if info.kind == NodeKind::Table {
                if let Some(cb) = tree.on_open_structure.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        let weak = self.me.borrow().clone();
        b_def.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else {
                return;
            };
            tree.open_def_for(&info);
            tree.close_popover();
        });
        let weak = self.me.borrow().clone();
        b_copy.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            if let Some(name) = tree.current_name() {
                if let Some(cb) = tree.on_copy_name.borrow().as_ref() {
                    cb(&name);
                }
            }
            tree.close_popover();
        });
        // 对象属性（表 / 视图）
        let weak = self.me.borrow().clone();
        b_stats.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            if matches!(info.kind, NodeKind::Table | NodeKind::View) {
                if let Some(cb) = tree.on_object_stats.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        // 导入 CSV（表节点上）
        let weak = self.me.borrow().clone();
        b_import.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            // 只有表能导入（视图是查询定义，导进去没有意义）
            if info.kind == NodeKind::Table {
                if let Some(cb) = tree.on_import_csv.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        // 导出表（阶段 52）：表 / 视图都能导
        let weak = self.me.borrow().clone();
        b_export.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            if matches!(info.kind, NodeKind::Table | NodeKind::View) {
                if let Some(cb) = tree.on_export_table.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        // ER 图（表节点上：看这张表的关系；模式节点上：看整个模式）
        let weak = self.me.borrow().clone();
        b_er.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            let table = match info.kind {
                NodeKind::Table => Some(info.table.clone()),
                NodeKind::Schema => None,
                _ => return,
            };
            if let Some(cb) = tree.on_er.borrow().as_ref() {
                cb(&info.schema, table.as_deref());
            }
            tree.close_popover();
        });
        // 新建表（模式节点上）
        let weak = self.me.borrow().clone();
        b_new.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            if info.kind == NodeKind::Schema {
                if let Some(cb) = tree.on_new_table.borrow().as_ref() {
                    cb(&info.schema);
                }
            }
            tree.close_popover();
        });
        // 重命名（表节点上）
        let weak = self.me.borrow().clone();
        b_rename.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            if info.kind == NodeKind::Table {
                if let Some(cb) = tree.on_rename_table.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        // 删除表（表节点上；二次确认在调用方）
        let weak = self.me.borrow().clone();
        b_drop.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            let Some(info) = tree.current_node() else { return };
            if info.kind == NodeKind::Table {
                if let Some(cb) = tree.on_drop_table.borrow().as_ref() {
                    cb(&info.schema, &info.table);
                }
            }
            tree.close_popover();
        });
        // 刷新（重新查一遍这一层）
        let weak = self.me.borrow().clone();
        b_refresh.connect_clicked(move |_| {
            let Some(tree) = weak.upgrade() else { return };
            tree.reload_current();
            tree.close_popover();
        });

        // 把菜单按钮收进 `self.menu`：敏感性更新和"按名字弹菜单"都要用
        *self.menu.borrow_mut() = Some(MenuBtns {
            view: b_view.clone(),
            structure: b_struct.clone(),
            def: b_def.clone(),
            stats: b_stats.clone(),
            import_csv: b_import.clone(),
            export_table: b_export.clone(),
            er: b_er.clone(),
            new: b_new.clone(),
            rename: b_rename.clone(),
            drop_table: b_drop.clone(),
        });

        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let view = self.view.clone();
        let pop2 = pop.clone();
        let weak = self.me.borrow().clone();
        gesture.connect_pressed(move |g, _n, x, y| {
            if g.current_button() != 3 {
                return;
            }
            let (bx, by) = view.convert_widget_to_bin_window_coords(x as i32, y as i32);
            if let Some((Some(path), _, _, _)) = view.path_at_pos(bx, by) {
                let sel = view.selection();
                sel.unselect_all();
                sel.select_path(&path);
            }
            // 菜单项按当前选中行开：表能看数据/结构，例程与触发器能看定义
            if let Some(tree) = weak.upgrade() {
                let kind = tree.current_node().map(|i| i.kind);
                tree.update_menu_sensitivity(kind);
            }
            pop2.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            pop2.popup();
        });
        self.view.add_controller(gesture);
        *self.pop.borrow_mut() = Some(pop);
    }

    /// 按当前选中行的种类更新右键菜单各项的敏感性。
    ///
    /// 不是所有节点都能用所有菜单项（表才能看结构、模式才能建表……），
    /// **灰掉比点了没反应好**——用户一眼就知道这个操作在这儿不适用。
    fn update_menu_sensitivity(&self, kind: Option<NodeKind>) {
        // 借用要先落到变量里：`self.menu.borrow().as_ref()` 那样写，
        // 临时值在语句结束就没了（编译器直接报错 E0716）。
        let menu = self.menu.borrow();
        let Some(m) = menu.as_ref() else {
            return;
        };
        // 查看数据：表与视图
        m.view
            .set_sensitive(matches!(kind, Some(NodeKind::Table) | Some(NodeKind::View)));
        // 表结构（设计器）：只有表有字段 / 索引这一套（视图是查询定义）
        m.structure
            .set_sensitive(matches!(kind, Some(NodeKind::Table)));
        // 查看定义：函数 / 存储过程 / 触发器 / 事件
        m.def.set_sensitive(matches!(
            kind,
            Some(NodeKind::Function)
                | Some(NodeKind::Procedure)
                | Some(NodeKind::Trigger)
                | Some(NodeKind::Event)
        ));
        // 对象属性：表与视图（视图是查询，没有大小但有行数估算）
        m.stats
            .set_sensitive(matches!(kind, Some(NodeKind::Table) | Some(NodeKind::View)));
        // ER 图：表（看这张表跟谁有关系）或模式（看整个模式）
        m.er
            .set_sensitive(matches!(kind, Some(NodeKind::Table) | Some(NodeKind::Schema)));
        // 新建表只在模式节点上；改名 / 删表只在表节点上
        m.new.set_sensitive(matches!(kind, Some(NodeKind::Schema)));
        m.rename.set_sensitive(matches!(kind, Some(NodeKind::Table)));
        m.drop_table.set_sensitive(matches!(kind, Some(NodeKind::Table)));
    }

    /// 找到名字匹配的那一行，弹出它的右键菜单（测试钩子）。
    ///
    /// **为什么要有这个钩子**：用户是**右键**点表才看得到「ER 图…」，
    /// 光测 `open_er_now` 那种"直接调"的路子，测不到右键菜单这一段
    /// （菜单项建没建、敏感性对不对、点了会不会真开图）。
    ///
    /// 返回 false = 树里没有叫这个名字的行。
    pub fn popup_menu_for(&self, name: &str) -> bool {
        let Some(path) = self.find_path_by_name(name) else {
            return false;
        };
        // 选中它：菜单项的敏感性是按**当前选中行**算的
        let sel = self.view.selection();
        sel.unselect_all();
        sel.select_path(&path);
        let kind = self.node_at(&path).map(|i| i.kind);
        self.update_menu_sensitivity(kind);
        // 菜单本体 clone 出来再弹：popup() 可能触发回调，
        // 拿着 RefCell 的借用不放容易撞上双重借用。
        let pop = self.pop.borrow().clone();
        let Some(pop) = pop else { return false };
        pop.set_pointing_to(Some(&self.row_rect(&path)));
        pop.popup();
        true
    }

    /// 冒烟测试用：右键菜单的弹层本身。
    ///
    /// 样式检查要问弹层有没有 `pp-menu` 卡片类；hover 验证要拿它算
    /// 菜单项在弹层里的位置。
    pub fn menu_popover(&self) -> Option<gtk::Popover> {
        self.pop.borrow().clone()
    }

    /// 冒烟测试用：右键菜单里的按钮（文字 + 控件）。
    ///
    /// **为什么要有**：菜单风格统一（图标、`pp-menu-item` 类、hover 高亮、
    /// 危险项标红）得能**从外面验**——不然只能靠肉眼看截图。
    /// 返回的按钮可以直接查类、也可以拿去做 hover 验证。
    pub fn menu_items(&self) -> Vec<(String, gtk::Button)> {
        let pop = self.pop.borrow().clone();
        let Some(pop) = pop else { return Vec::new() };
        let Some(body) = pop.child().and_then(|c| c.downcast::<gtk::Box>().ok()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut cur = body.first_child();
        while let Some(c) = cur {
            if let Some(b) = c.downcast_ref::<gtk::Button>() {
                let label = crate::menu::item_label(b).unwrap_or_default();
                out.push((label, b.clone()));
            }
            cur = c.next_sibling();
        }
        out
    }

    /// 冒烟测试用：按名字展开一行（走真实的展开 → 懒加载链路）。
    pub fn expand_row_by_name(&self, name: &str) -> bool {
        let Some(path) = self.find_path_by_name(name) else {
            return false;
        };
        self.view.expand_row(&path, false);
        true
    }

    /// 某一节现在是不是**展开着**（测试钩子）。
    ///
    /// ⚠️ **查的是"视图"不是"模型"**：模型里挂了子节点 ≠ 屏幕上展开着
    /// （审计 P2-11 说的就是"展开到模式就停住了"，模型里其实有表）。
    pub fn is_expanded_by_name(&self, name: &str) -> bool {
        self.expanded_nodes.borrow().iter().any(|n| n == name)
    }

    /// 按名字找一行（**按路径逐层走**）。
    ///
    /// **为什么不能深遍历 `GtkTreeStore`**：在子行上调 `iter_children` 会让
    /// **上层**的 iter 失效，递归两层后顶层 `iter_next` 直接返回 false，
    /// 树只走了一半（测试钩子据此误报"树上没挂出子节点"）。
    /// 可靠写法是**按路径走**：`TreePath::from_indices` + `append_index`，
    /// 每步从根重新定位。
    fn find_path_by_name(&self, name: &str) -> Option<gtk::TreePath> {
        let model = self.view.model()?;
        let mut i: i32 = 0;
        loop {
            let path = gtk::TreePath::from_indices(&[i]);
            if model.iter(&path).is_none() {
                break;
            }
            if let Some(found) = self.find_path_under(&model, &path, name) {
                return Some(found);
            }
            i += 1;
        }
        None
    }

    /// 在某一行的子树里按名字找（递归，按路径走）。
    fn find_path_under(
        &self,
        model: &gtk::TreeModel,
        path: &gtk::TreePath,
        name: &str,
    ) -> Option<gtk::TreePath> {
        let iter = model.iter(path)?;
        let row_name = self.store.get::<String>(&iter, COL_NAME as i32);
        if row_name == name {
            return Some(path.clone());
        }
        let mut j: i32 = 0;
        loop {
            let mut child = path.clone();
            child.append_index(j);
            if model.iter(&child).is_none() {
                break;
            }
            if let Some(found) = self.find_path_under(model, &child, name) {
                return Some(found);
            }
            j += 1;
        }
        None
    }

    /// 取某一行的位置（菜单弹在那一行旁边）。
    ///
    /// GTK4 的 `gtk_tree_view_get_background_area` 没有 Rust 绑定
    /// （gtk4-rs 只自动绑了一部分 API），这里直接调 ffi。
    /// 取不到就退回树控件左上角——**位置不对不影响菜单功能**。
    fn row_rect(&self, path: &gtk::TreePath) -> gdk::Rectangle {
        let Some(col) = self.view.column(0) else {
            return gdk::Rectangle::new(4, 4, 1, 1);
        };
        // 未初始化的矩形：GTK 会往里写 x/y/width/height
        let mut rect = unsafe { gdk::Rectangle::uninitialized() };
        unsafe {
            gtk::ffi::gtk_tree_view_get_background_area(
                self.view.to_glib_none().0,
                path.to_glib_none().0,
                col.to_glib_none().0,
                rect.to_glib_none_mut().0,
            );
        }
        if rect.width() <= 0 || rect.height() <= 0 {
            return gdk::Rectangle::new(4, 4, 1, 1);
        }
        rect
    }

    /// 打开某个节点的定义页签（函数 / 存储过程 / 触发器）。
    fn open_def_for(&self, info: &NodeInfo) {
        match info.kind {
            NodeKind::Function | NodeKind::Procedure => {
                if let Some(cb) = self.on_open_routine.borrow().as_ref() {
                    cb(
                        &info.schema,
                        &info.name,
                        &info.args,
                        info.kind == NodeKind::Procedure,
                    );
                }
            }
            NodeKind::Trigger => {
                if let Some(cb) = self.on_open_trigger.borrow().as_ref() {
                    cb(&info.schema, &info.table, &info.name);
                }
            }
            NodeKind::Event => {
                if let Some(cb) = self.on_open_event.borrow().as_ref() {
                    cb(&info.schema, &info.name);
                }
            }
            _ => {}
        }
    }

    fn close_popover(&self) {
        if let Some(p) = self.pop.borrow().as_ref() {
            p.popdown();
        }
    }

    /// 当前选中的路径。
    fn selected_path(&self) -> Option<gtk::TreePath> {
        self.view.selection().selected_rows().0.into_iter().next()
    }

    /// 光标所在的节点。
    fn current_node(&self) -> Option<NodeInfo> {
        let path = self.selected_path()?;
        self.node_at(&path)
    }

    /// 光标所在节点的名称。
    fn current_name(&self) -> Option<String> {
        let path = self.selected_path()?;
        let model = self.view.model()?;
        let iter = model.iter(&path)?;
        let id = self.store.get::<u32>(&iter, COL_NODE as i32) as usize;
        self.nodes.borrow().get(id).map(|n| n.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **每类对象一个图标，而且互不相同**（用户 2026-11-02 给的 dbx 截图：
    /// "它为表、数据库、视图等都提供了不同的图标，一眼就能分辨"）。
    #[test]
    fn 每类对象一个不重样的图标() {
        let kinds = [
            NodeKind::Root,
            NodeKind::Schema,
            NodeKind::Table,
            NodeKind::View,
            NodeKind::Column,
            NodeKind::Index,
            NodeKind::Function,
            NodeKind::Procedure,
            NodeKind::Trigger,
            NodeKind::Event,
        ];
        let mut seen: Vec<&str> = Vec::new();
        for k in kinds {
            let ic = k.icon();
            assert!(!ic.is_empty(), "{:?} 没有图标", k);
            assert!(
                ic.ends_with("-symbolic"),
                "{:?} 的图标名不像 symbolic 系列：{ic}",
                k
            );
            assert!(!seen.contains(&ic), "{:?} 的图标跟别人重了：{ic}", k);
            seen.push(ic);
        }
        // 占位节点（"载入中…"）不占图标位
        assert!(NodeKind::Placeholder.icon().is_empty());
    }
}
