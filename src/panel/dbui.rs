//! 数据库窗口（7.2 → 8.x，Rust-only）：对象浏览器 + 多结果页签 + 数据编辑。
//!
//! 形态对标 DBeaver / Navicat：一个窗口里左边是对象树，右边是结果页签，
//! 底下是 SQL 编辑器。
//!
//! ```text
//! ┌────────────┬──────────────────────────────────────────┐
//! │ 对象浏览器  │ [结果页签 1][结果页签 2][+]               │
//! │ ▸ 模式      ├──────────────────────────────────────────┤
//! │  ▸ 表      │  筛选框        行数 / 耗时 / 事务状态     │
//! │   ▸ 列     │  ┌────┬──────┬────────┬────────────┐     │
//! │            │  │ id │ name │ email  │ created_at │     │
//! │            │  └────┴──────┴────────┴────────────┘     │
//! │            ├──────────────────────────────────────────┤
//! │            │ SQL 编辑器  [执行][执行选中][取消]        │
//! └────────────┴──────────────────────────────────────────┘
//! ```
//!
//! **通道**：连接走该标签已有的 SSH 连接（worker 的 `DbConnect`），不新建
//! SSH 连接、不占 TCP 端口（通道细节见 `backend::db`）。
//!
//! **页签**：每次执行 SQL 开一个新页签；对象树里双击表也开页签（查数据）。
//! 页签与 worker 的请求用 id 对应（`pending` 表），所以并发查询/翻页不会
//! 把结果串到别的页签上。
//!
//! **数据编辑**：只对"从对象树打开的表"开放，且必须先点「编辑」进入编辑
//! 模式（此时才开事务、才允许改单元格）。改动先落在内存，「保存修改」把
//! 每行拼成 UPDATE 执行（WHERE 用主键；没主键就用整行原值），全部成功才
//! COMMIT；任何一条失败就 ROLLBACK，绝不自动提交。
//!
//! 配置存 `databases.ini`（非敏感字段）+ 凭据库（密码，键 `db:<会话键>`）。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use gtk::prelude::*;

use super::PanelState;
use super::dbimport;
use super::dialogs;
use super::worker::{DbMetaPayload, DbMetaReq, DbTxOp, WorkerCmd};
use super::{dbgrid, dbtree, erview};
use crate::backend::db::{DbConfig, DbProto, DbRows};
use crate::backend::dbcols;
use crate::backend::dbexport;
use crate::backend::dbcsv;
use crate::backend::dbhist;
use crate::backend::dbmeta::{self, ColType};
use crate::backend::dbstore::{self, DbProfile};
use crate::vault;
use crate::i18n::tr;
use crate::trf;

/// 每页行数。
const PAGE_SIZE: usize = 200;
/// 对象浏览器宽度（像素）。
///
/// 树只有「名称 | 说明」两列（类型并进了说明列，见 dbtree.rs 的说明），
/// 每列 min_width 140 → 280 起步，留 50 给缩进、滚动条和边框。
/// 再宽就把右侧结果区挤小了——结果表工具栏一排 5-8 颗图标按钮，
/// 宽度不够右边会被切掉。
const TREE_WIDTH: i32 = 330;

/// 数据库窗口。
/// 对象树折叠开关的共享状态（阶段 66 / 审计 P0-2）。
///
/// ⚠️ **为什么要单独拎出来**：折叠动作要 `set_active()` 同步按钮 ⇒ 又触发
/// 按钮的 `toggled` 回调 ⇒ 回调里再去借 `st.db_ui`（那是 `RefCell`）就
/// **双重借用 panic**（AGENTS.md 的老坑）。
/// ⇒ 状态放这里，回调只抓**弱引用**（引用环红线）。
struct TreeToggle {
    /// 对象树现在是不是**收起来了**。
    collapsed: Cell<bool>,
    /// 程序化改按钮状态时为 true——回调看到它就直接返回（不重入）。
    updating: Cell<bool>,
    /// 顶部的开关按钮（**按下 = 对象树显示**）
    btn: gtk::ToggleButton,
    /// 对象树那一列（折叠时整列隐藏）
    col: gtk::Box,
    /// 树 + 主区之间的 Paned（折叠时 position = 0）
    paned: gtk::Paned,
}

impl TreeToggle {
    /// 折叠 / 展开对象树。
    fn apply(&self, collapsed: bool) {
        if self.collapsed.get() == collapsed {
            return;
        }
        self.collapsed.set(collapsed);
        // ⚠️ **两样都要做**：光隐藏还不够——树那一列有 `size_request(330)`，
        // 不隐藏的话 Paned 仍要给它 330px，主区照样被挤。
        self.col.set_visible(!collapsed);
        self.paned.set_position(if collapsed { 0 } else { TREE_WIDTH });
        // 同步顶部开关（程序化改 ⇒ 不再回调，否则重入）
        self.updating.set(true);
        self.btn.set_active(!collapsed);
        self.updating.set(false);
    }
}

pub struct DbUi {
    /// **宿主窗口**（阶段 12：数据库工作台是连接页里的子标签，不再是独立
    /// 窗口）——各种对话框挂在它下面。
    pub win: gtk::Window,
    /// 子标签页的内容（连接页的子标签 notebook 里的一页）
    pub page: gtk::Widget,
    /// 本工作台在子标签里的页 id
    pub tab_id: Cell<u64>,
    /// 当前选中的数据库连接（**连接信息不在工作台里编辑**）
    current: RefCell<Option<DbProfile>>,
    // 「当场输入 / 临时连接」的密码**不在这里**——它挂在 `PanelState.db_pass`
    // 上（会话级的：工作台页签关掉再打开，密码还在，不用再问一遍）。
    /// 上一次导入的结果（冒烟测试读它）
    ///
    /// `Some((true, 说明))` = 成功，`Some((false, 原因))` = 失败。
    last_import: RefCell<Option<(bool, String)>>,
    /// 最近一次打开的**网格详情**内容（阶段 47：单元格 / 行 / 列详情）。
    ///
    /// 详情是独立窗口，测试不好从窗口里翻内容——记一份在这儿给钩子读。
    last_detail: RefCell<Vec<(String, String)>>,
    /// 导入失败的错误框（`AlertDialog`）——**要留着引用**，否则关不掉，
    /// 它会一直浮在界面上挡住东西（冒烟测试要截图时尤其烦）。
    import_err: RefCell<Option<gtk::Dialog>>,
    status: gtk::Label,
    /// SQL 编辑器（冒烟测试要往里写 SQL 验高亮）
    pub sql: gtk::TextView,
    /// SQL 编辑器的**占位提示**（阶段 83 / 审计 P2-11）。
    ///
    /// GTK4 的 `TextView` 没有 `set_placeholder_text` ⇒ 自己拿 Overlay 叠的。
    /// 存一份字段，冒烟能问"它现在显不显示"。
    sql_ph: gtk::Label,
    meta: gtk::Label,
    /// 结果页签
    notebook: gtk::Notebook,
    /// 结果区（没有页签时显示空状态提示）
    stack: gtk::Stack,
    /// 空态那一屏的内容（阶段 51：结构化空态，不再是一句提示文字）
    ///
    /// **为什么要单独一个字段**：空态要随连接状态变（没连接 / 连上了 /
    /// 没选连接），所以得能整体重建。这个 Box 挂在 `stack` 的 "empty" 页里，
    /// 重建时只清它自己的子节点，Stack 的页名不变。
    empty_box: gtk::Box,
    tabs: RefCell<Vec<Rc<ResultTab>>>,
    /// **表文档**（阶段 72e / 审计 P1-7）：打开一张表 = 一个文档页，
    /// 页内四个视图（数据 / 结构 / 属性 / ER）。
    ///
    /// **为什么单独记一份**：对象树右键的「表结构」「对象属性…」「ER 图…」
    /// 都要**切到已有文档的对应视图**，而不是各开一个平级页（审计 P1-7：
    /// "表有五个查看入口……它们的边界对普通用户不清晰"）。
    docs: RefCell<Vec<Rc<TableDoc>>>,
    /// 表结构页签（9.1）
    struct_tabs: RefCell<Vec<Rc<StructTab>>>,
    /// 定义页签（9.3）：函数 / 存储过程 / 触发器的定义
    def_tabs: RefCell<Vec<Rc<DefTab>>>,
    /// ER 图页签（阶段 19：Navicat 的 Database Model）
    er_tabs: RefCell<Vec<Rc<ErTab>>>,
    /// 对象浏览器
    tree: Rc<dbtree::DbTree>,
    /// 对象树的折叠开关（阶段 66 / 审计 P0-2）
    tree_toggle: Rc<TreeToggle>,
    /// 对象属性窗口（13.5：Navicat 的 Details 面板）
    stats_win: RefCell<Option<gtk::Window>>,
    /// 属性窗口里的网格（属性 | 值 两列，第 0 行是表头）
    ///
    /// **为什么用 Grid 而不是 TreeView**：Navicat 的 Details 面板是「属性 | 值」
    /// 两列且带表头。Grid 的列宽由内容算，属性名一列、值一列天然对齐；用
    /// TreeView 的话值列是可选中复制的（更好），但 GTK 的 CellRenderer 还得
    /// 另配一套复制逻辑，收益不大。
    stats_grid: RefCell<Option<gtk::Grid>>,
    /// 对象搜索框（11.1：全库搜表 / 视图 / 字段 / 例程 / 触发器 / 事件）
    search_entry: gtk::Entry,
    /// 搜索按钮（放大镜）
    search_btn: gtk::Button,
    /// 刷新对象树（阶段 51）
    btn_refresh: gtk::Button,
    /// 「表数据导入/导出…」（阶段 72 / 审计 P1-8）：**一个**入口，菜单里两个去向。
    ///
    /// ⚠️ **为什么是菜单按钮而不是普通按钮**：以前这是个「数据传输」对话框，
    /// 导入还要先在对话框里选一次「方向」才进 CSV 向导——审计 P1-7 说那是
    /// "多余的中间步骤"。现在选哪一项就直接进那个向导。
    btn_transfer: gtk::MenuButton,
    /// 对象类型过滤下拉（阶段 51：全部 / 表 / 视图 / 例程 / 触发器 / 事件）
    filter_combo: gtk::ComboBoxText,
    /// 「详细」开关（阶段 51：表节点显示行数 / 大小 / 修改时间）
    detail_check: gtk::CheckButton,
    /// 搜索结果窗口（开着的时候复用，关了就没了）
    search_win: RefCell<Option<gtk::Window>>,
    /// 搜索结果窗口里的网格
    search_grid: Rc<dbgrid::DbGrid>,
    /// 当前搜索命中（与 `search_grid` 的源行号一一对应）
    search_hits: RefCell<Vec<dbmeta::SearchHit>>,
    /// 搜索结果窗口底部的状态文字（窗口建好才有）
    search_status: RefCell<Option<gtk::Label>>,
    // 注：工作台里**不再有**「连接 / 断开 / 新建」按钮（2026-10-27 用户要求）：
    // 子标签栏的「+」负责新建，**关掉页签就自动断开**。
    btn_run: gtk::Button,
    btn_run_sel: gtk::Button,
    btn_cancel: gtk::Button,
    /// 「历史」按钮（11.2：执行历史）
    btn_history: gtk::Button,
    /// 历史窗口（开着就复用）
    hist_win: RefCell<Option<gtk::Window>>,
    /// 历史的分组键（会话 + 数据库连接名；连接前是会话本身）
    ///
    /// **为什么按连接分**：一个 SSH 会话下可以挂多个数据库连接（阶段 10），
    /// 历史混在一起就分不清"这条 SQL 是在哪个库上跑的"——Navicat 的 History
    /// 也是按连接分的。
    hist_key: RefCell<String>,
    /// 历史窗口里的网格
    hist_grid: Rc<dbgrid::DbGrid>,
    /// 当前历史条目（与 `hist_grid` 的源行号一一对应，存的是 SQL 全文）
    hist_entries: RefCell<Vec<dbhist::HistEntry>>,
    connected: Cell<bool>,
    busy: Cell<bool>,
    /// 工作台页面**已经挂上去了**吗（子标签 / 独立窗口）。
    ///
    /// ⚠️ 用户 2026-11-02：「任何时候，必须连上数据库之后才允许显示数据库
    /// 管理页面，不允许在还没连接上数据库时显示数据库管理页面」⇒ 页面在
    /// 连上之前**不挂**（见 `mount`）。
    ///
    /// 这个标志用来判断"状态行用户看不看得见"：没挂 ⇒ 状态行在屏幕上根本
    /// 没有 ⇒ 该说的话得**弹对话框**，否则用户点了"数据库"毫无反应。
    mounted: Cell<bool>,
    session_key: String,
    /// 请求 id（与 worker 的事件对上）
    next_req: Cell<u64>,
    /// 请求 id → 这个请求是干什么的
    pending: RefCell<HashMap<u64, PendingReq>>,
    /// SQL 编辑器高亮器
    sql_hl: Rc<SqlHl>,
    /// 保存作业队列（一条一条发，PG 里一条失败整个事务就废了）
    save_queue: RefCell<VecDeque<String>>,
    save_job: RefCell<Option<SaveJob>>,
    save_id: Cell<u64>,
    /// 「执行定义」的语句队列（9.3；逐条发，不拼多语句）
    def_queue: RefCell<VecDeque<String>>,
    def_job: RefCell<Option<DefJob>>,
    /// 「保存并关闭」的保存链（阶段 63 / 审计 P0-1）。
    ///
    /// **为什么需要链**：数据表的保存走 `ui.save_queue`、表结构的保存走
    /// `tab.sql_queue`，两边都共用 `ui.busy` ⇒ 同一时刻只能存一个。
    /// 统一关闭时可能有好几张表要存 ⇒ 排队一个个来。
    ///
    /// `None` = 当前没有保存链（不是关闭流程，别乱动）。
    save_chain: RefCell<Option<VecDeque<DbSaveTask>>>,
}

/// 统一关闭流程里"要保存的一个东西"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DbSaveTask {
    /// 数据表（网格里的增删改）
    Grid(u64),
    /// 表结构（字段 / 索引 / 外键的改动）
    Struct(u64),
}

/// SQL 编辑器的高亮器。
///
/// **为什么要单独一个 Rc**：buffer 的 `changed` 回调要指回高亮器自己，
/// 而窗口本体挂在 `PanelState.db_ui` 里（不是 Rc），拿不到自引用。
struct SqlHl {
    buffer: gtk::TextBuffer,
    tags: RefCell<Vec<gtk::TextTag>>,
    /// 防抖：敲一个字就重排一次太费
    pending: Cell<bool>,
}

impl SqlHl {
    fn new(buffer: &gtk::TextBuffer) -> Rc<SqlHl> {
        let tags: Vec<gtk::TextTag> = super::syntax::ALL_TOKS
            .iter()
            .map(|t| {
                let tag = super::syntax::make_tok_tag(*t, &crate::theme::syn_colors());
                buffer.tag_table().add(&tag);
                tag
            })
            .collect();
        Rc::new(SqlHl {
            buffer: buffer.clone(),
            tags: RefCell::new(tags),
            pending: Cell::new(false),
        })
    }

    /// 内容变了：防抖后重画高亮。
    fn schedule(self: &Rc<Self>) {
        if self.pending.get() {
            return;
        }
        self.pending.set(true);
        let me = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            me.pending.set(false);
            me.apply();
        });
    }

    /// 按 SQL 方言重画高亮。
    fn apply(&self) {
        let start = self.buffer.start_iter();
        let end = self.buffer.end_iter();
        for tag in self.tags.borrow().iter() {
            self.buffer.remove_tag(tag, &start, &end);
        }
        let text = self.buffer.text(&start, &end, true).to_string();
        let hl = super::syntax::highlight(&text, super::syntax::Lang::Sql);
        if hl.stalls > 0 {
            // 扫描器原地打转被守卫挡下 → 高亮不可信，宁可不上色
            // （守卫保证最坏只是没颜色，不会把窗口冻住）
            eprintln!("{}", trf!("[高亮] SQL 编辑器扫描异常（{v0} 处原地打转），本次不高亮", v0 = hl.stalls));
            return;
        }
        let tags = self.tags.borrow();
        for s in &hl.spans {
            if s.end <= s.start {
                continue;
            }
            let Some(tag) = tags.get(super::syntax::tok_index(s.tok)) else {
                continue;
            };
            let si = self.buffer.iter_at_offset(s.start as i32);
            let ei = self.buffer.iter_at_offset(s.end as i32);
            self.buffer.apply_tag(tag, &si, &ei);
        }
    }

    /// 主题切换后重画颜色（tag 的颜色是写死的，只能重新设一遍）。
    fn refresh_colors(&self) {
        let colors = crate::theme::syn_colors();
        for (i, tag) in self.tags.borrow().iter().enumerate() {
            let tok = super::syntax::ALL_TOKS[i];
            if tok != super::syntax::Tok::Plain {
                tag.set_foreground(Some(colors.of(tok)));
            }
        }
        self.apply();
    }
}

/// 请求的用途（回包时才知道该怎么处理）。
enum PendingReq {
    /// 对象树的元数据 → 填回树里
    Meta(DbMetaReq),
    /// 打开表数据：先取列元信息（类型 + 主键），再查数据
    MetaForTable { tab_id: u64 },
    /// 查询 / 翻页
    Query { tab_id: u64 },
    /// 写语句
    Exec { tab_id: u64 },
    /// 事务
    Tx { op: DbTxOp, tab_id: u64 },
    /// 表结构页签的一块元数据（字段 / 索引 / 外键）
    Struct { tab_id: u64, part: StructPart },
    /// 表结构页签的 DDL 执行
    Ddl { tab_id: u64 },
    /// 表结构页签的**保存修改**（阶段 14：可视化改结构）
    StructSave { tab_id: u64 },
    /// 表结构改动失败后的回滚（阶段 15：PG 的多条语句在事务里）
    StructRollback { tab_id: u64 },
    /// 定义页签的「执行定义」（9.3）
    DefExec { tab_id: u64 },
    /// 定义页签的定义文本（9.3）
    Def { tab_id: u64 },
    /// 整表导出（阶段 52）
    Export,
    /// 对象树上的 DDL（9.6：新建表 / 改名 / 删表）——执行完刷新那一层
    TreeDdl { what: String, schema: String },
    /// 全库对象搜索（11.1）
    Search,
    /// 对象属性（13.5）——要知道是哪个对象的，回包才知道往哪个窗口填
    ObjectStats { schema: String, table: String },
    /// 导入向导（阶段 18）：要目标表的列信息，回包才知道怎么映射
    ImportPrep {
        schema: String,
        table: String,
        /// CSV 原文（读文件那一步拿到的）
        text: String,
        /// true = 不弹映射对话框，按列名自动映射直接导入（冒烟测试用）
        auto: bool,
    },
    /// 导入向导：等导入结果（记住行数，报错时告诉用户处理了多少行）
    ImportRun { rows: usize },
    /// ER 图（阶段 19）：回包要填到哪个 ER 页签
    Er { tab_id: u64 },
    /// 冒烟测试用：执行一条任意 SQL
    Probe,
}

/// 按 id 找定义页签。
fn find_def_tab(ui: &DbUi, tab_id: u64) -> Option<Rc<DefTab>> {
    ui.def_tabs
        .borrow()
        .iter()
        .find(|t| t.id == tab_id)
        .cloned()
}

/// 按 id 找表结构页签。
fn find_struct_tab(ui: &DbUi, tab_id: u64) -> Option<Rc<StructTab>> {
    ui.struct_tabs
        .borrow()
        .iter()
        .find(|t| t.id == tab_id)
        .cloned()
}

/// 表结构页签的三块内容。
#[derive(Clone, Copy, PartialEq, Debug)]
enum StructPart {
    Fields,
    Indexes,
    ForeignKeys,
    /// 触发器（阶段 49；**只读**）
    Triggers,
}

impl StructPart {
    fn label(self) -> &'static str {
        match self {
            StructPart::Fields => tr("字段"),
            StructPart::Indexes => tr("索引"),
            StructPart::ForeignKeys => tr("外键"),
            StructPart::Triggers => tr("触发器"),
        }
    }
}

/// 表结构页签（9.1）：字段 / 索引 / 外键 + DDL。
///
/// 形态对标 Navicat 的表设计器：上面几页是结构信息，最后一页是 DDL 文本
/// （可编辑）。**DDL 是显式执行**的——改完不点「执行 DDL」就绝不会动库。
struct StructTab {
    id: u64,
    st: Weak<PanelState>,
    win: gtk::Window,
    /// 表的定位
    schema: String,
    table: String,
    /// 三块网格
    fields: Rc<dbgrid::DbGrid>,
    indexes: Rc<dbgrid::DbGrid>,
    fks: Rc<dbgrid::DbGrid>,
    /// DDL 文本（可编辑）
    ddl: gtk::TextView,
    /// DDL 高亮器
    ddl_hl: Rc<SqlHl>,
    /// 内层分段控件（阶段 49：字段 / 索引 / 外键 / 触发器 / DDL）
    ///
    /// **为什么不用普通页签**：dbx 的设计器里 `字段 | 索引 | 外键 | 触发器`
    /// 是**分段控件**（一排按钮），不是页签——页签的层级感会让人以为
    /// "换到了另一个文档"，而这几块其实是**同一张表的几个侧面**。
    /// GTK4 里是 `GtkStack` + 一排自己画的 `ToggleButton`
    /// （`GtkStackSwitcher` 的按钮是 homogeneous 的，会摊满整行）。
    inner: gtk::Stack,
    /// 分段控件那排按钮（与 `inner` 配对）
    seg_buttons: Rc<Vec<gtk::ToggleButton>>,
    /// 触发器表（只读，阶段 49）
    triggers: Rc<dbgrid::DbGrid>,
    /// 字段改动的**实时预览**（阶段 50）：只读文本 + 一句「可执行 / 有问题」
    preview: gtk::TextView,
    preview_mark: gtk::Label,
    /// 常用类型下拉（阶段 50）：挑一个类型应用到选中行
    type_combo: gtk::ComboBoxText,
    btn_exec: gtk::Button,
    /// 右下角「应用变更」主按钮（阶段 44）
    btn_apply: gtk::Button,
    status: gtk::Label,
    box_: gtk::Box,
    /// 三块元数据（都到齐了才拼 DDL）
    cols: RefCell<Option<Vec<dbmeta::ColumnDef>>>,
    idx: RefCell<Option<Vec<crate::backend::db::DbIndex>>>,
    fk: RefCell<Option<Vec<dbmeta::ForeignKey>>>,
    /// 待执行的语句队列（一条回包后发下一条）
    ///
    /// **为什么需要队列**：PG 的索引改动是**多条独立语句**（`DROP INDEX` 和
    /// `CREATE INDEX` 各一条，`ALTER TABLE` 管不了索引），一条语句装不下。
    /// PG 的这些语句还会被包在一个事务里（PG 支持事务性 DDL），失败整体回滚。
    sql_queue: RefCell<VecDeque<String>>,
    /// 这个队列是不是包在事务里（失败时要发 ROLLBACK）
    in_tx: Cell<bool>,
}

/// 定义页签（9.3）：函数 / 存储过程 / 触发器的定义。
///
/// 对标 Navicat 的函数编辑器：双击对象树里的函数就开一个页签，里面是
/// **可编辑**的定义文本（带 SQL 高亮），底部两个动作：
/// - 「复制定义」：把定义复制到剪贴板；
/// - 「执行定义」：显式执行当前文本（PG 的 `CREATE OR REPLACE` 幂等，
///   触发器会先 `DROP TRIGGER IF EXISTS`）。
///
/// **绝不自动执行**——改完不点按钮就绝不会动库。
struct DefTab {
    id: u64,
    st: Weak<PanelState>,
    win: gtk::Window,
    /// 页签标题（函数名 / 触发器名）
    title: String,
    /// 对象定位（执行定义时要用来拼 DROP）
    kind: dbmeta::DefKind,
    schema: String,
    table: String,
    name: String,
    /// 定义文本（可编辑）
    text: gtk::TextView,
    /// 高亮器
    hl: Rc<SqlHl>,
    btn_copy: gtk::Button,
    btn_exec: gtk::Button,
    status: gtk::Label,
    box_: gtk::Box,
}

/// 一次「执行定义」的作业：要按顺序发的几条语句。
#[derive(Clone, Copy)]
struct DefJob {
    tab_id: u64,
    /// 总条数
    total: usize,
    /// 已经成功发出去的条数
    sent: usize,
}

/// 编辑模式下要做的行操作（等事务开启成功后执行）。
///
/// **为什么要等事务开好**：插入 / 删除行都是写库动作，必须在事务里做。
/// 事务是异步开的（要等 worker 回包），所以先记下意图，等 BEGIN 成功了
/// 再落到网格上——不然会出现"行已经加上了，但事务没开成"的半截状态。
#[derive(Clone, PartialEq, Debug)]
enum RowAction {
    /// 插入一行
    Insert,
    /// 删除**显示行号**的那几行（阶段 46：多选，一次删掉全部选中的行）
    Delete(Vec<usize>),
}

/// 一次"保存修改"作业。
struct SaveJob {
    tab_id: u64,
    total: usize,
    affected: u64,
}

/// 一个结果页签。
struct ResultTab {
    id: u64,
    /// 页签标题（阶段 72e：挂载的时候要拿它做页签头）
    title: String,
    /// 所属面板（弱引用，避免与 PanelState 形成引用环）
    st: std::rc::Weak<PanelState>,
    /// 所属窗口（导出对话框要挂在它下面）
    win: gtk::Window,
    grid: Rc<dbgrid::DbGrid>,
    filter: gtk::Entry,
    status: gtk::Label,
    btn_next: gtk::Button,
    /// 导出（菜单按钮：CSV / SQL / JSON / XML / HTML，阶段 21）
    ///
    /// **为什么是一个菜单按钮而不是几颗图标**：工具栏已经一排十来颗按钮了，
    /// 再加就挤不下（右侧的「放弃」会被切掉）。Navicat / DBeaver 也是
    /// **一个**导出入口，格式在菜单里选。
    btn_export: gtk::MenuButton,
    btn_edit: gtk::Button,
    /// 插入新行（9.4）
    btn_insert: gtk::Button,
    /// 删除选中行（9.4）
    btn_delete: gtk::Button,
    btn_save: gtk::Button,
    btn_discard: gtk::Button,
    /// 过滤条件编辑器（9.5）：填 Where 条件，重新查库
    btn_filter: gtk::Button,
    /// 这张表对应的对象（可编辑数据）；None = 普通查询结果（只读）
    target: RefCell<Option<TableTarget>>,
    /// 列显示设置的持久化目标（阶段 20）：`(连接标识, 模式, 表名)`。
    ///
    /// **只在表浏览页签上有**：普通查询结果的列每次都不一样
    /// （SELECT 列表换了就是另一批列），存下来没意义。表浏览才是
    /// "这张表我一直只关心这几列"。打开表时填好（那时连接已经定了）。
    col_key: RefCell<Option<(String, String, String)>>,
    /// 列类型与主键（打开表时从元数据来）
    types: RefCell<Vec<ColType>>,
    pk: RefCell<Vec<usize>>,
    /// 列名（打开表时从元数据来，**与 `types` 同序**）
    ///
    /// ⚠️ **单独存一份列名，别拿网格的列标题凑**：网格的列会**按显示设置
    /// 隐藏**（阶段 17），隐藏之后"屏幕上的第几列"跟元数据里的第几列就
    /// 错位了——按位置取类型会把 int 列当成 text 列，条件里的值就转错
    /// （数字列多套一层引号 → 服务器报类型错）。
    col_names: RefCell<Vec<String>>,
    /// 当前过滤条件（9.5；重新查库 / 翻页都带着它）
    filter_conds: RefCell<Vec<dbmeta::FilterCond>>,
    /// 由条件生成的 WHERE 文本（空串 = 不过滤）
    filter_where: RefCell<String>,
    /// 编辑模式中
    editing: Cell<bool>,
    /// 事务已开
    in_tx: Cell<bool>,
    /// 还有下一页
    has_more: Cell<bool>,
    /// 编辑模式下排队的行操作（等事务开启成功后执行）
    pending_row_action: Cell<Option<RowAction>>,
    /// 「只读 / 可编辑」标志（阶段 72c / 审计 P2-10）。
    ///
    /// **为什么要有**：工具栏上「编辑 / 插入行 / 删除行」三颗按钮平时是
    /// **灰的**——普通查询结果本来就不能改。用户看着灰按钮不知道是"坏了"
    /// 还是"本来就这样"，页面上也没一处说明 ⇒ 加个标志讲清楚。
    mode_chip: gtk::Label,
    /// 结果网格的**空结果态**（阶段 83 / 审计 P2-10）。
    ///
    /// 网格区是个 Stack：「grid」是网格本身，「empty」是"没结果"的说明。
    /// 以前零行返回时是一整块空白 ⇒ 用户分不清"查完了没数据"和"还没查"。
    grid_stack: gtk::Stack,
    /// 空结果态的标题（没跑过 / 跑了但没行，两种说法不同）。
    empty_title: gtk::Label,
    /// 空结果态的解释。
    empty_desc: gtk::Label,
    /// 空结果态的主 CTA（只有"可编辑的表"才有：插入一行）。
    ///
    /// **只读的结果没有 CTA**：没有任何可做的动作，硬塞一个按钮反而是噪声。
    empty_cta: gtk::Button,
    /// 面包屑（阶段 43）：页签顶部那排芯片，一眼看清"我在哪"
    crumbs: gtk::Box,
    /// 底栏左半：行数 / 耗时 / 待保存项数（阶段 45，对标 dbx 的状态栏）
    foot_info: gtk::Label,
    /// 底栏右半：页码（第 N 页）
    foot_page: gtk::Label,
    /// 底栏：首页 / 上一页 / 下一页 / 每页行数
    btn_first: gtk::Button,
    btn_prev: gtk::Button,
    btn_size: gtk::ComboBoxText,
    /// 当前页码（1 起）
    page: Cell<usize>,
    /// 每页行数
    page_size: Cell<usize>,
    /// 当前偏移（= (页码-1) * 每页行数）
    offset: Cell<usize>,
    /// 最近一次取回的行数 / 耗时（底栏显示用，阶段 45）
    last_rows: Cell<usize>,
    last_elapsed: Cell<u64>,
    /// 条件条（阶段 48）：可视化 WHERE / ORDER BY 构造器
    cond_bar: Rc<CondBar>,
    /// 排序列（空 = 不排序）
    order_col: RefCell<String>,
    /// 排序方向（true = 升序）
    order_asc: Cell<bool>,
    /// 排序列生成的 ORDER BY 片段（空 = 不排序）
    order_by: RefCell<String>,
    /// 条件条的两个下拉是否已经填过列（一张表只填一次）
    cond_filled: Cell<bool>,
    box_: gtk::Box,
}

/// 表数据页签对应的表。
#[derive(Clone)]
struct TableTarget {
    schema: String,
    table: String,
    is_view: bool,
}

/// 条件条（阶段 48，对标 dbx 的 `WHERE ▾ | ORDER BY ▾` 条）。
///
/// **为什么要有它**：原先过滤条件只能开一个**弹窗**手写（一行一条地拼），
/// 排序更是**根本没有入口**。dbx / Navicat 的做法是**在结果页签上直接摆
/// 一条**：挑列 → 挑运算符 → 填值 → 加条件，改完立刻重查。
///
/// **只在表浏览页签上有**：普通查询结果是用户手写的 SQL，往里拼 WHERE /
/// ORDER BY 是越权（用户自己写 SQL，自己想加什么加什么）。
struct CondBar {
    /// 整条（竖排：上面是控件行，下面是已生效条件的 chip 行）
    root: gtk::Box,
    /// 与上一条条件的连接词（并且 / 或者）
    join: gtk::ComboBoxText,
    /// 条件列
    col: gtk::ComboBoxText,
    /// 运算符
    op: gtk::ComboBoxText,
    /// 值
    val: gtk::Entry,
    /// 「+ 条件」按钮
    btn_add: gtk::Button,
    /// 已生效条件的 chip 行（每颗 chip 带一个 ✕）
    chips: gtk::Box,
    /// 排序列
    order_col: gtk::ComboBoxText,
    /// 排序方向（升序 / 降序）
    btn_dir: gtk::Button,
    /// 「清除」按钮
    btn_clear: gtk::Button,
}

/// 条件 chip 上显示的文字（纯函数，好单测）。
///
/// 形如 `列 = 值`；「IS NULL」这类**不带值**的条件只显示 `列 IS NULL`
/// （拼个空值出来看着像 bug）。
pub fn cond_chip_text(c: &dbmeta::FilterCond) -> String {
    match c.op {
        dbmeta::FilterOp::IsNull | dbmeta::FilterOp::IsNotNull => {
            format!("{} {}", c.column, c.op.label())
        }
        _ => format!("{} {} {}", c.column, c.op.label(), c.value),
    }
}

/// 排序方向按钮上的文字（纯函数，好单测）。
pub fn order_dir_text(asc: bool) -> &'static str {
    if asc {
        tr("↑ 升序")
    } else {
        tr("↓ 降序")
    }
}

/// 建条件条（控件都在这儿造，回调等页签建好再连）。
fn build_cond_bar() -> Rc<CondBar> {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("pp-condbar");

    let lbl = gtk::Label::new(Some(tr("条件")));
    lbl.add_css_class("dim-label");
    row.append(&lbl);

    let join = gtk::ComboBoxText::new();
    // ⚠️ **`append(id, text)` 的顺序别搞反**：第一个参数是 id（程序里认的），
    // 第二个是**显示给用户看的文字**。写成 `append("并且", "AND")` 的话，
    // 界面上显示的是 "AND"，而且 `active_id()` 回来的是"并且"，
    // 下面的 "OR" 判断永远不成立 → 「或者」这个选项形同虚设。
    join.append(Some("AND"), tr("并且"));
    join.append(Some("OR"), tr("或者"));
    join.set_active_id(Some("AND"));
    join.set_tooltip_text(Some(tr("与上一条条件的连接方式（第一条用不上）")));
    // ⚠️ **限宽**：不定的话它会被挤成只剩一个下拉箭头，"并且 / 或者"
    // 两个字都看不见（用户根本不知道这一格是干什么的）。
    join.set_size_request(74, -1);
    row.append(&join);

    let col = gtk::ComboBoxText::new();
    col.set_tooltip_text(Some(tr("要加条件的列")));
    // 列名可能是中文（四个字要 8 列宽），窄了会被截成「…」
    col.set_size_request(108, -1);
    row.append(&col);

    let op = gtk::ComboBoxText::new();
    for o in dbmeta::FilterOp::all() {
        op.append(Some(o.label()), o.label());
    }
    op.set_active_id(Some("="));
    op.set_tooltip_text(Some(tr("比较方式")));
    op.set_size_request(72, -1);
    row.append(&op);

    let val = gtk::Entry::new();
    val.set_placeholder_text(Some(tr("值")));
    // 限宽：不设的话它会把右边的排序控件挤出可视区
    val.set_width_chars(12);
    row.append(&val);

    let btn_add = gtk::Button::with_label(tr("+ 条件"));
    btn_add.set_tooltip_text(Some(tr("把这个条件加进去，并按它重新查这张表")));
    row.append(&btn_add);

    // ---- 排序那一段 ----
    row.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let lbl2 = gtk::Label::new(Some(tr("排序")));
    lbl2.add_css_class("dim-label");
    row.append(&lbl2);

    let order_col = gtk::ComboBoxText::new();
    order_col.set_tooltip_text(Some(tr("按哪一列排序（不选就不排序）")));
    order_col.set_size_request(108, -1);
    row.append(&order_col);

    let btn_dir = gtk::Button::with_label(order_dir_text(true));
    btn_dir.set_tooltip_text(Some(tr("切换排序方向（升序 / 降序）")));
    row.append(&btn_dir);

    let btn_clear = gtk::Button::with_label(tr("清除"));
    btn_clear.set_tooltip_text(Some(tr("清掉全部条件与排序，恢复查整张表")));
    row.append(&btn_clear);

    // 已生效条件的 chip 行（空的时候藏起来，别占一行高度）
    let chips = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    chips.set_visible(false);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.append(&row);
    root.append(&chips);
    // 整条默认藏着：只有**表浏览页签**才有条件条
    root.set_visible(false);

    Rc::new(CondBar {
        root,
        join,
        col,
        op,
        val,
        btn_add,
        chips,
        order_col,
        btn_dir,
        btn_clear,
    })
}

/// 结果页签的**面包屑**分段（阶段 43，对标 dbx 的 `SQLite_du3h / test@main / 3 字段`）。
///
/// - **表浏览**：`连接名` / `表@模式` / `N 字段`
/// - **普通查询结果**：`查询结果` / `N 列`
///
/// 纯函数：拼装逻辑单独拎出来，好单测钉住（面包屑拼错不会报错，
/// 只是"看着不像回事"——那种问题只能靠测试钉）。
pub fn crumb_parts(conn: &str, schema: &str, table: &str, cols: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if table.is_empty() {
        // 普通查询结果：没有"对象"可言
        out.push(tr("查询结果").to_string());
        out.push(trf!("{cols} 列", cols = cols));
        return out;
    }
    if !conn.is_empty() {
        out.push(conn.to_string());
    }
    out.push(if schema.is_empty() {
        table.to_string()
    } else {
        format!("{table}@{schema}")
    });
    out.push(trf!("{cols} 字段", cols = cols));
    out
}

#[derive(Clone)]
struct Form {
    /// 连接名（同一个 SSH 会话下区分多个数据库连接）
    name: gtk::Entry,
    host: gtk::Entry,
    port: gtk::Entry,
    user: gtk::Entry,
    dbname: gtk::Entry,
    pass: gtk::Entry,
}

/// 懒创建工作台（db_ui 挂在 PanelState 上，随标签生命周期存活）。
///
/// ⚠️ **这里只"建"，不"挂"**（用户 2026-11-02：「任何时候，必须连上数据库
/// 之后才允许显示数据库管理页面」）⇒ 页面要等 `on_connected` 成功之后
/// 才由 `mount` 摆到屏幕上。
///
/// **阶段 12**：数据库工作台不再是独立弹窗，而是**连接页里的一个子标签**
/// （对标 XTerminal）——弹窗那点宽度根本放不下对象树 + 结果网格。
pub fn ensure(st: &Rc<PanelState>) {
    if st.db_ui.borrow().is_none() {
        let ui = build(st);
        wire(st, &ui);
        st.db_ui.borrow_mut().replace(ui);
    }
}

/// 把工作台页面**挂上去**（子标签 / 独立窗口）并切过去。
///
/// ⚠️ **只有连上数据库之后才调它**（用户 2026-11-02：「任何时候，必须连上
/// 数据库之后才允许显示数据库管理页面，不允许在还没连接上数据库时显示
/// 数据库管理页面」）。
///
/// **为什么拆成两步**：`ensure` 只**建**工作台（控件、状态、事件），
/// `mount` 才把它**摆到屏幕上**。中间那段（连接中）用户看到的是终端，
/// 不是"一个连不上的空工作台"——以前连不上也会开出一页空白界面，用户
/// 完全不知道发生了什么。
///
/// 这里再判一次 `connected` 是**保险**：万一哪个调用方在没连上的时候喊了
/// 一嗓子，页面也不会挂出去（"任何时候"是硬要求，不是"大部分时候"）。
pub fn mount(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if !ui.connected.get() {
        return;
    }
    match st.sub_tabs() {
        Some(sub) => {
            // 页签可能已经被用户用 × 关掉了——那种情况下它不在 notebook
            // 里了，光 `switch_to` 什么也不会发生（用户看到的是"再点一次
            // 数据库，界面毫无反应"）。所以要先确认页还在，不在就重新挂。
            if !sub.has(ui.tab_id.get()) {
                // 页签带图标：主标签头不放图标了，"这一页是数据库"由
                // 子页签自己的图标说。
                //
                // ⚠️ 关这一页之前**先问**（阶段 63 / P0-1）：表里改了数据、
                // 改了表结构还没提交，直接关就没了。以前这里传的是 `None`
                // ⇒ 按 × 直接把改动丢掉。
                let id_cell = Rc::new(Cell::new(0u64));
                let id_cell2 = Rc::clone(&id_cell);
                let stw = Rc::downgrade(st);
                let subw = Rc::downgrade(&sub);
                let on_close: Box<dyn Fn() -> bool> = Box::new(move || {
                    let Some(st) = stw.upgrade() else { return true };
                    let info = super::closeguard::DirtyInfo {
                        db_edits: pending_edit_count(&st),
                        db_struct_edits: pending_struct_edit_count(&st),
                        ..Default::default()
                    };
                    if info.is_clean() {
                        return true;
                    }
                    let parent = st.topwin();
                    match super::closeguard::ask(parent.as_ref(), &info) {
                        super::closeguard::CloseChoice::Cancel => false,
                        // 不保存 ⇒ 正常关（SubTabs 会来摘这一页）
                        super::closeguard::CloseChoice::Discard => true,
                        super::closeguard::CloseChoice::Save => {
                            let n = start_save_all_pending(&st);
                            if n == 0 {
                                return true;
                            }
                            // ⚠️ 存完再关：现在返回 false（这一拍先不关），
                            // 等保存的回包齐了再摘这一页。
                            let id = id_cell2.get();
                            let subw2 = subw.clone();
                            super::closeguard::arm_save(
                                n,
                                Box::new(move || {
                                    if let Some(sub) = subw2.upgrade() {
                                        sub.remove_page(id);
                                    }
                                }),
                            );
                            false
                        }
                    }
                });
                let tab_id = sub.add_page_icon_kind(
                    Some("drive-harddisk-symbolic"),
                    tr("数据库"),
                    super::subtabs::PageKind::Db,
                    ui.page.clone(),
                    Some(on_close),
                );
                id_cell.set(tab_id);
                ui.tab_id.set(tab_id);
                // 页签头写上连的是哪条
                update_tab_title(st, ui);
                // 关掉这个页签 → **自动断开**这条连接（工作台里不再摆
                // 「断开」按钮）。自己注册自己的钩子：谁的东西谁管。
                //
                // ⚠️ 必须抓**弱引用**：SubTabs 挂在 PanelState 上，抓强引用
                // 就成环了（PanelState → 子标签 → 钩子 → PanelState），
                // 面板永远释放不掉。
                {
                    let stw = Rc::downgrade(st);
                    sub.add_close_hook(move |id| {
                        if id != tab_id {
                            return;
                        }
                        let Some(st) = stw.upgrade() else { return };
                        on_tab_closed(&st, id);
                    });
                }
            }
            sub.switch_to(ui.tab_id.get());
        }
        None => {
            // 没有子标签容器（老路径 / 单测 / 示例）：退回独立窗口。
            //
            // **为什么必须兜底**：这一页若既不在子标签里、也不在窗口里，
            // 就是「存在但谁也看不见」——用户看到的是"点了没反应"，
            // 截图是一片空白，比直接报错还难查。
            if !ui.mounted.get() {
                ui.win.set_title(Some(tr("数据库工作台")));
                ui.win.set_default_size(1200, 760);
                ui.win.set_child(Some(&ui.page.clone()));
                // 独立窗口路径：关窗口 = 关工作台 → **自动断开**这条连接
                // （子标签路径由 SubTabs 的 close_hook 通知，见 on_tab_closed）
                {
                    let st3 = Rc::clone(st);
                    ui.win.connect_close_request(move |_| {
                        {
                            let slot = st3.db_ui.borrow();
                            if let Some(u) = slot.as_ref() {
                                if u.connected.get() {
                                    do_disconnect(&st3, u);
                                }
                            }
                        }
                        st3.db_ui.borrow_mut().take();
                        gtk::glib::Propagation::Proceed
                    });
                }
                ui.win.present();
            }
        }
    }
    ui.mounted.set(true);
}

/// 冒烟 / 离线测试用：**假装已经连上**，把工作台挂上去。
///
/// ⚠️ 真机上页面是 `on_connected` 成功之后才挂的（用户 2026-11-02：
///    「必须连上数据库之后才允许显示数据库管理页面」）。离线测试连不了库，
///    就用这个钩子把页面挂出来（否则测试连界面都看不到）。
pub fn mount_for_test(st: &Rc<PanelState>) {
    set_connected_for_test(st, true);
    mount(st);
}

/// 冒烟测试用：直接设「已连接」标志（**不真的连库**）。
///
/// 用来验证 `mount` 的那道保险：**没连上 ⇒ 挂不上去**。
pub fn set_connected_for_test(st: &Rc<PanelState>, v: bool) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.connected.set(v);
}

fn build(st: &Rc<PanelState>) -> DbUi {
    // **宿主窗口 = 主窗口**（阶段 12：数据库工作台不再是独立窗口了）。
    // 各种对话框（过滤条件、历史、搜索结果、导出）挂在它下面。
    //
    // ⚠️ 没有子标签容器时（老路径 / 单测 / 示例）必须用**自己的窗口**：
    // 那时这个窗口是这一页唯一的容器，借主窗口的话 `set_child` 会把
    // 主窗口的内容整个换掉（整个界面变成数据库工作台）。
    let win = if st.sub_tabs().is_some() {
        st.topwin().unwrap_or_else(gtk::Window::new)
    } else {
        gtk::Window::new()
    };

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_start(10);
    root.set_margin_end(10);
    root.set_margin_top(8);
    root.set_margin_bottom(8);

    /* ---- 状态行：**只有状态文字** ----
     *
     * 阶段 22：工作台里不再摆连接信息（原来那一行「当前连接 [下拉] [+] [连接]
     * [断开]」）。连接在哪、连的是谁，都在**连接那一层**管：双击会话树 /
     * 连接管理器里的连接就**直接连上**，连的是哪条写在**页签头**上
     * （"数据库 · 本地 PG"）。
     *
     * 2026-10-27 用户要求：**连「断开」按钮也不要了**——子标签栏的「+」
     * 已经能新建连接，而**关掉这个页签就自动断开**，不需要再摆一个按钮。
     * 状态行因此只剩一行状态文字。
     */
    let status = gtk::Label::new(Some(tr("未连接")));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_wrap(true);
    // ⚠️ **限宽是防御性的**（审计 P0-2，2026-10-30）：这句提示现在的文案
    // 放得下（实测宽 500px < 主区 626px），但它一旦变长就会按自然宽把
    // 容器撑破 ⇒ 限一限，别等到文案改了才发现被裁。
    // `max_width_chars` 是 Label 版的 max-width（**GTK4 的 CSS 没有
    // max-width 属性**，写了会被整条丢弃）。
    status.set_max_width_chars(30);
    status.set_selectable(true);
    status.set_hexpand(true);
    // 本会话的键（数据库连接是**按会话**分组的）
    let key = dbstore::session_key(&st.opts);
    // 阶段 24 那个「新建数据库连接…」按钮已经撤掉（2026-10-27 用户要求）：
    // **子标签栏的「+」**里就有「新建数据库连接…」，而且它建好之后直接连上，
    // 比工作台里多一个按钮顺手。状态行因此只剩状态文字。
    // 「表数据导入/导出…」（阶段 72 / 审计 P1-7、P1-8）：dbx 那边叫「数据传输」，
    // 我们放在工作台的状态行右边（那一行原来只有状态文字）。
    //
    // **为什么是菜单**：以前这是个「数据传输」对话框，导入还要先选一次
    // 「方向」才进 CSV 向导 ⇒ 中间多了一层（审计 P1-7 的原话："『数据传输』
    // 导入只是给同一个 CSV 向导加了一层方向选择，是多余的中间步骤"）。
    // 现在菜单里选哪一项，就**直接**进那个向导。
    //
    // **为什么不用「数据传输」这个词**（审计 P1-8）：SFTP 的「传输」指文件，
    // DB 的「传输」指表数据——两回事共用一个词，用户分不清。
    let btn_transfer = gtk::MenuButton::new();
    {
        let l = gtk::Label::new(Some(tr("表数据导入/导出…")));
        l.set_xalign(0.0);
        btn_transfer.set_child(Some(&l));
    }
    btn_transfer.set_tooltip_text(Some(
        tr("把整张表导出到本地文件（CSV / SQL / JSON），或把本地 CSV 导入到表里"),
    ));
    {
        // ⚠️ `MenuButton::set_popover()` 自己会给弹层设父级 ⇒ 用
        // `popover_detached()`，**别再手动 `pop.set_parent(&btn)`**。
        let pop = crate::menu::popover_detached();
        let vb = crate::menu::body();
        let b_import = crate::menu::item("document-import-symbolic", tr("导入数据（CSV）…"), None);
        let b_export = crate::menu::item("document-export-symbolic", tr("导出表…"), None);
        vb.append(&b_import);
        vb.append(&b_export);
        pop.set_child(Some(&vb));
        {
            let st2 = Rc::clone(st);
            let pop2 = pop.clone();
            b_import.connect_clicked(move |_| {
                pop2.popdown();
                let parent = {
                    let slot = st2.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    ui.win.clone()
                };
                crate::panel::dbtransfer::open_import(&st2, Some(&parent), None);
            });
        }
        {
            let st2 = Rc::clone(st);
            let pop2 = pop.clone();
            b_export.connect_clicked(move |_| {
                pop2.popdown();
                let parent = {
                    let slot = st2.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    ui.win.clone()
                };
                crate::panel::dbtransfer::open_export(&st2, Some(&parent), None);
            });
        }
        btn_transfer.set_popover(Some(&pop));
    }
    // 「对象树」开关（阶段 66 / 审计 P0-2）：窄屏时把对象树收起来，
    // 主区能宽出一大截（330px）。
    //
    // ⚠️ **必须放在顶部**：树收起来之后，树那一列整个没了 ⇒ 树头上的
    // 「‹」也跟着没了 ⇒ 只剩这里能把树拉回来。
    let btn_tree = gtk::ToggleButton::new();
    btn_tree.set_icon_name("view-list-symbolic");
    btn_tree.set_tooltip_text(Some(tr("对象树：显示 / 隐藏")));
    btn_tree.set_active(true);
    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status_row.append(&status);
    status_row.append(&btn_tree);
    status_row.append(&btn_transfer);
    root.append(&status_row);

    /* ---- 主体：左树 + 右结果/SQL ---- */
    let tree = dbtree::DbTree::new();
    // 事件挂哪儿跟方言有关：PG 的事件是库级的（挂根节点下），MySQL 的挂库下
    // 方言跟着「当前连接」走（连接建的时候类型就定了）
    tree.set_proto(DbProto::Postgres);
    let tree_sw = gtk::ScrolledWindow::new();
    tree_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    tree_sw.set_size_request(TREE_WIDTH, -1);
    // ⚠️ **必须撑满高度**：不撑的话滚动窗口只按**内容高度**占位——树里只有
    // 一行时就只画一行，下面全是空白（用户报「连上数据库后左侧只显示一行」，
    // 看着像数据没加载出来，其实是控件没占满）。撑满后树有多少行就显示多少行，
    // 剩下的空间留给滚动。
    tree_sw.set_vexpand(true);
    tree_sw.set_child(Some(&tree.view));
    tree_sw.add_css_class("frame");

    /* ---- 对象搜索（11.1）---- */
    // 放在对象树正上方：搜到的东西就是树里的东西，顺手就能打开。
    let search_entry = gtk::Entry::new();
    search_entry.set_placeholder_text(Some(tr("搜索对象…")));
    search_entry.set_tooltip_text(Some(
        tr("在整个库里搜表 / 视图 / 字段 / 函数 / 存储过程 / 触发器 / 事件（回车开始）"),
    ));
    search_entry.set_hexpand(true);
    search_entry.set_icon_from_icon_name(
        gtk::EntryIconPosition::Primary,
        Some("system-search-symbolic"),
    );
    let search_btn = icon_button("system-search-symbolic", tr("搜索对象"));
    // 刷新对象树（阶段 51）：库里建了表 / 改了结构，刷新一下就能看到。
    let btn_refresh = icon_button("view-refresh-symbolic", tr("刷新对象树：重新查一遍这个库"));
    // 「收起对象树」（阶段 66）：树就在手边，收起它比去顶部找开关顺手。
    //
    // ⚠️ 收起之后**这一行也跟着没了**（整列都隐藏了）⇒ 把树拉回来只能
    // 靠顶部的开关。
    let btn_tree_hide = icon_button("go-previous-symbolic", tr("收起对象树：给主区腾出宽度"));
    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    search_row.append(&search_entry);
    search_row.append(&search_btn);
    search_row.append(&btn_refresh);
    search_row.append(&btn_tree_hide);

    /* ---- 树工具行（阶段 51）：类型过滤 + 详细模式 ----
     *
     * dbx 的对象树面板上有一排小控件（搜索 / 显示已连接 / 列表·详情切换）。
     * 我们这边对应的是「显示哪类对象」+「表节点要不要带行数等信息」。
     */
    let filter_label = gtk::Label::new(Some(tr("显示")));
    filter_label.add_css_class("dim-label");
    let filter_combo = gtk::ComboBoxText::new();
    for (id, f) in dbtree::TREE_FILTERS {
        filter_combo.append(Some(id), f.label());
    }
    // ⚠️ `append(id, text)` 的顺序：**id 是程序里认的，text 是给用户看的**。
    // 写反了的话 `active_id()` 回来的是显示文字，判断永远不成立
    // （阶段 48 的「并且/或者」就踩过这个坑）。
    filter_combo.set_active_id(Some("all"));
    filter_combo.set_tooltip_text(Some(tr("对象树里显示哪些类型的对象（改了会重新查一遍）")));
    let detail_check = gtk::CheckButton::with_label(tr("详细"));
    detail_check.set_tooltip_text(Some(
        tr("表节点上显示行数 / 占用大小 / 修改时间（行数是数据库的估算值，不是精确行数）"),
    ));
    let tree_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tree_row.append(&filter_label);
    tree_row.append(&filter_combo);
    tree_row.append(&detail_check);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 4);
    left.append(&search_row);
    left.append(&tree_row);
    left.append(&tree_sw);

    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);
    notebook.set_vexpand(true);

    // 空状态：一页签都没有时给的那一屏（阶段 51：**结构化骨架**，不是一句提示）
    //
    // 原来只有一句居中的提示文字 + 一大片空白，用户看着像"功能坏了"。
    // dbx 的空态是「面包屑 + 表单 + 表格骨架」；我们没那么多东西可摆，
    // 但至少摆得像个"开始页"：这个会话下有哪些连接（点一下就连上）、
    // 快速开始的三件事、快捷键提示。内容由 `refresh_empty` 随时重建。
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&notebook, Some("pages"));
    // ⚠️ **三层结构，为的是把卡片真的摆正**（阶段 96 实测）：
    //
    // ① GtkStack 的 allocate_child 会把**halign=CENTER 的那一页**居中
    //    （这一层是真会居中的）；
    // ② 但 GtkBox 摆子控件时**按内容宽算位置**、却按**含 padding 的宽**分配
    //    ⇒ 带 padding 的卡片会偏出去 padding/2（实测 16px）⇒ 看着"没居正"；
    // ③ ⇒ **padding 放到内层**：外层卡片只留底色与边框（边框只差 1px），
    //    内容装在内层（`empty_box`，也就是 `ui.empty_box`）。
    let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    empty_box.add_css_class("pp-empty-inner");
    // ⚠️ **最小宽度**（阶段 97）：品牌页顶上那座"招牌"（图标 + 产品名）
    // 本身不宽，卡片会被内容撑成一条窄条 ⇒ 看着像贴边的小纸条。
    // ⇒ 给个下限（`set_size_request` 量的是**内容盒**，CSS 的 padding
    // 再加上去 ⇒ 卡片实际比这个数宽一点）。
    empty_box.set_size_request(360, -1);
    let empty_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    empty_card.add_css_class("pp-empty");
    empty_card.append(&empty_box);
    let empty_outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    empty_outer.append(&empty_card);
    empty_outer.set_halign(gtk::Align::Center);
    empty_outer.set_valign(gtk::Align::Center);
    stack.add_named(&empty_outer, Some("empty"));
    stack.set_visible_child_name("empty");

    let sql = gtk::TextView::new();
    sql.set_monospace(true);
    let sql_hl = SqlHl::new(&sql.buffer());
    sql.set_wrap_mode(gtk::WrapMode::None);
    sql.set_top_margin(6);
    sql.set_bottom_margin(6);
    sql.set_left_margin(6);
    sql.set_right_margin(6);
    let sql_sw = gtk::ScrolledWindow::new();
    sql_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sql_sw.set_size_request(-1, 160);
    // 同上：**撑满**竖直 Paned 给的剩余高度，别让编辑器缩成 160px 的一条
    sql_sw.set_vexpand(true);
    sql_sw.set_child(Some(&sql));
    sql_sw.add_css_class("frame");

    // ⚠️ **占位提示**（阶段 83 / 审计 P2-11）：GTK4 的 `TextView` **没有**
    // `set_placeholder_text`（那是 `GtkEntry` 才有的）⇒ 自己拿 `Overlay`
    // 叠一个 Label 上去。
    //
    // ⚠️ **Label 不能吃鼠标事件**（`set_can_target(false)`）：不然它盖在
    // 编辑框上面，用户点了却聚焦不进去 ⇒ "编辑器坏了"。
    let sql_ph = gtk::Label::new(Some(
        tr("写一条 SQL，按 Ctrl+Enter 执行；选中一段按 Ctrl+Shift+Enter 只跑那段"),
    ));
    sql_ph.add_css_class("dim-label");
    sql_ph.set_xalign(0.0);
    sql_ph.set_can_target(false);
    // 边缘跟编辑器的内边距对齐（TextView 的 margin 是 6，这里留 8）
    sql_ph.set_margin_start(8);
    sql_ph.set_margin_top(8);
    let sql_overlay = gtk::Overlay::new();
    sql_overlay.set_child(Some(&sql_sw));
    sql_overlay.add_overlay(&sql_ph);
    // 空 ⇒ 显示提示；一有字 ⇒ 藏起来
    {
        let ph = sql_ph.clone();
        sql.buffer().connect_changed(move |b| {
            let empty = b.text(&b.start_iter(), &b.end_iter(), false).trim().is_empty();
            ph.set_visible(empty);
        });
    }

    // ⚠️ **纯图标按钮**（用户 2026-10-28：主界面尽量少用文字）：
    // 这一排原本是「执行 / 执行选中 / 取消 / 历史」四个文字按钮，
    // 文字说明挪进 tooltip。
    let btn_run = icon_button(
        "media-playback-start-symbolic",
        tr("执行：跑编辑器里的全部 SQL (Ctrl+Enter)"),
    );
    btn_run.set_sensitive(false);
    let btn_run_sel = icon_button(
        "edit-select-symbolic",
        tr("执行选中：只跑选中的那段 SQL (Ctrl+Shift+Enter)"),
    );
    btn_run_sel.set_sensitive(false);
    let btn_cancel = icon_button("media-playback-stop-symbolic", tr("取消：中止正在跑的语句"));
    btn_cancel.set_sensitive(false);
    // 执行历史（11.2）：跑过的 SQL 留档，能翻回去重跑
    let btn_history = icon_button(
        "document-open-recent-symbolic",
        tr("历史：看跑过的 SQL（双击一条把它填回编辑器）"),
    );
    let sql_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    sql_row.append(&btn_run);
    sql_row.append(&btn_run_sel);
    sql_row.append(&btn_cancel);
    sql_row.append(&btn_history);
    let meta = gtk::Label::new(Some(tr("Ctrl+Enter 执行全部 · Ctrl+Shift+Enter 只跑选中的那段")));
    meta.add_css_class("dim-label");
    meta.set_xalign(0.0);
    meta.set_hexpand(true);
    meta.set_selectable(true);
    // ⚠️ **必须限宽**（阶段 96 实测）：这行提示的自然宽 ~360px，加上左边
    // 四颗图标按钮 ⇒ 工具行的最小宽 ~510px ⇒ **工作台主区的最小宽被撑到
    // 510px** ⇒ 主区比窗口还宽（窄容器下整块工作台溢出到窗口右边，
    // 里面的内容全被裁掉）。以前空态卡**靠左**，溢出部分正好在右边没被
    // 看见；卡片一旦居中，溢出就露出来了 ⇒ 这里必须先把它收住。
    // ⚠️ GTK4 的 CSS 没有 max-width ⇒ Label 版就是 `set_max_width_chars`。
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.set_max_width_chars(30);
    sql_row.append(&meta);

    let sql_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sql_box.append(&sql_row);
    sql_box.append(&sql_overlay);

    let right = gtk::Paned::new(gtk::Orientation::Vertical);
    right.set_vexpand(true);
    right.set_start_child(Some(&stack));
    right.set_end_child(Some(&sql_box));
    right.set_position(320);
    // ⚠️ **不许把任何一边挤到比它的最小高度还小**（阶段 72e 实测）。
    //
    // 上半区（页签区）的最小高度**随内容变**：结构视图里有 200px 的字段表
    // + 80px 的 DDL 预览（阶段 50 加的），最小高度能顶到 506；下半区的
    // SQL 编辑器最小 196。工作台 704 高 ⇒ 506 + 196 = 702，**放得下**。
    //
    // 但 `GtkPaned` 默认 `shrink-start-child=true` ⇒ 它把上半区按**最小
    // 高度**（506）给，下半区却仍按原来的分隔条位置（320）给（382）
    // ⇒ **两个子控件加起来 888 > 704，溢出重叠** ⇒ 上半区底部被 SQL
    // 编辑器盖住，**更糟的是上半区的页签栏被挤成 0 高**——用户看不到
    // 外层页签，也看不到「数据/结构/属性/ER」，看着像"页签整个不见了"。
    //
    // 改成**不许挤**：Paned 会自己把分隔条挪到合法位置（上半区 506 /
    // 下半区 198），两边都拿到够用的高度。
    right.set_shrink_start_child(false);
    right.set_shrink_end_child(false);

    let main = gtk::Paned::new(gtk::Orientation::Horizontal);
    main.set_vexpand(true);
    main.set_start_child(Some(&left));
    main.set_end_child(Some(&right));
    main.set_position(TREE_WIDTH);
    root.append(&main);

    // ---- 对象树的折叠开关（阶段 66 / 审计 P0-2）----
    let tree_toggle = Rc::new(TreeToggle {
        collapsed: Cell::new(false),
        updating: Cell::new(false),
        btn: btn_tree.clone(),
        col: left.clone(),
        paned: main.clone(),
    });
    {
        // 顶部开关：按下 = 树显示
        let ttw = Rc::downgrade(&tree_toggle);
        btn_tree.connect_toggled(move |b| {
            let Some(tt) = ttw.upgrade() else { return };
            // 程序化同步时不重入（否则会递归）
            if tt.updating.get() {
                return;
            }
            tt.apply(!b.is_active());
        });
    }
    {
        // 树头上的「‹」：收起对象树
        let ttw = Rc::downgrade(&tree_toggle);
        btn_tree_hide.connect_clicked(move |_| {
            let Some(tt) = ttw.upgrade() else { return };
            tt.apply(true);
        });
    }

    // 本会话下的数据库连接（阶段 13：一个会话可挂多个数据库连接）
    let profiles = dbstore::list(&key);
    // 默认选中「上次用的那条」，没有就用第一个
    let last = crate::settings::last_db_connection(&key);
    let current = profiles
        .iter()
        .find(|p| Some(&p.name) == last.as_ref())
        .or_else(|| profiles.first())
        .cloned();
    if let Some(p) = &current {
        tree.set_proto(p.proto);
    } else {
        // 一个会话下还没有数据库连接时，**说清楚下一步该做什么**——
        // 光一个空界面，用户会以为功能坏了。
        // ⚠️ 复查 N3 + §4.1「说明书式提示」：**不再叙述点击路径**。
        // 以前这句是"点子标签栏右端的「+」→…，或者右键会话树里的这条连接 →…"，
        // 一句话里同一个动作两个名字（现已统一成「新建数据库连接…」），而且长句
        // 在 1200 宽下折出「连接…」孤行。
        // ⇒ 句子自然消失：下面那张空态卡上就摆着主 CTA「新建数据库连接…」，
        //   用户看**状态**就够了，**怎么点**由按钮自己说。
        status.set_text(tr("这个会话下还没有数据库连接"));
    }

    DbUi {
        win,
        page: root.clone().upcast(),
        tab_id: Cell::new(0),
        current: RefCell::new(current),
        last_import: RefCell::new(None),
        last_detail: RefCell::new(Vec::new()),
        import_err: RefCell::new(None),
        status,
        sql,
        sql_ph,
        sql_hl,
        meta,
        notebook,
        stack,
        empty_box,
        tabs: RefCell::new(Vec::new()),
        docs: RefCell::new(Vec::new()),
        struct_tabs: RefCell::new(Vec::new()),
        def_tabs: RefCell::new(Vec::new()),
        er_tabs: RefCell::new(Vec::new()),
        tree,
        tree_toggle: tree_toggle.clone(),
        stats_win: RefCell::new(None),
        stats_grid: RefCell::new(None),
        search_entry,
        search_btn,
        btn_refresh,
        btn_transfer,
        filter_combo,
        detail_check,
        search_win: RefCell::new(None),
        search_grid: dbgrid::DbGrid::new(),
        search_hits: RefCell::new(Vec::new()),
        search_status: RefCell::new(None),
        btn_run,
        btn_run_sel,
        btn_cancel,
        btn_history,
        hist_win: RefCell::new(None),
        hist_key: RefCell::new(key.clone()),
        hist_grid: dbgrid::DbGrid::new(),
        hist_entries: RefCell::new(Vec::new()),
        connected: Cell::new(false),
        busy: Cell::new(false),
        // ⚠️ 页面**还没挂**：连上数据库之后才挂（用户 2026-11-02）
        mounted: Cell::new(false),
        session_key: key,
        next_req: Cell::new(1),
        pending: RefCell::new(HashMap::new()),
        save_queue: RefCell::new(VecDeque::new()),
        save_job: RefCell::new(None),
        save_id: Cell::new(0),
        def_queue: RefCell::new(VecDeque::new()),
        def_job: RefCell::new(None),
        save_chain: RefCell::new(None),
    }
}

/* ---------------- 对象树折叠（阶段 66） ---------------- */

/// 对象树现在是不是**收起来了**（阶段 66，冒烟用）。
pub fn tree_collapsed(st: &Rc<PanelState>) -> bool {
    let ui = st.db_ui.borrow();
    ui.as_ref()
        .map(|u| u.tree_toggle.collapsed.get())
        .unwrap_or(false)
}

/// 折叠 / 展开对象树（阶段 66，冒烟用）。
///
/// ⚠️ 借用 `st.db_ui` 的期间**不要**再借它（那是 `RefCell`）——
/// 这里调用的 `apply()` 会改按钮状态 ⇒ 触发 `toggled` 回调 ⇒ 回调里
/// 只碰 `TreeToggle`（不借 `db_ui`）⇒ 不会双重借用。
pub fn set_tree_collapsed(st: &Rc<PanelState>, collapsed: bool) {
    let ui = st.db_ui.borrow();
    if let Some(u) = ui.as_ref() {
        u.tree_toggle.apply(collapsed);
    }
}

/// 对象树那一列**现在可见吗**（阶段 66，冒烟用）。
///
/// ⚠️ 别只看"标志位"：标志位对了但列没隐藏，界面照样被挤——
/// ⇒ 断言要落在**真的可见性**上。
pub fn tree_col_visible(st: &Rc<PanelState>) -> bool {
    let ui = st.db_ui.borrow();
    ui.as_ref()
        .map(|u| u.tree_toggle.col.is_visible())
        .unwrap_or(false)
}

/// 树 + 主区之间的 Paned 位置（阶段 66，冒烟用）。
pub fn tree_paned_pos(st: &Rc<PanelState>) -> i32 {
    let ui = st.db_ui.borrow();
    ui.as_ref().map(|u| u.tree_toggle.paned.position()).unwrap_or(-1)
}

/* ---------------- 空态（阶段 51：结构化骨架；阶段 97：品牌页重做） ---------------- */
/// 重建空态那一屏（阶段 51；**阶段 97 重新设计**）。
///
/// **为什么要重新设计**（用户 2026-11-02）：
/// - 「新建查询」「搜索对象」**点了没有任何作用**，用户不知道它们是干嘛的；
/// - 「这个会话的数据库连接」这一列**没有意义**——能进到这一屏，说明用户
///   刚点了某个数据库连接或者新建了一个，**目的就是打开那个库**；
/// - 「说实话，我也不知道这里应该显示啥，或者显示个漂亮的 logo？图片？艺术字？」
///   ⇒ 我们的工具**不是通用数据库客户端**（不像 dbx / dbeaver），
///   抄它们的首页只会更不像我们自己。
///
/// 现在的样子（**品牌页**）：
/// 1. **应用自己的图标** + 产品名「PuTTYPlus」+ 这一屏是什么（艺术字那一下）；
/// 2. 连上了 ⇒ 只显示**当前连的是哪个库**（一个 chip，**不可点**）；
///    没连上 ⇒ 这一列连接列表**这时才有用**（"我该连哪个库？"）；
/// 3. 三个**看得见效果**的动作：浏览对象 / 写一条查询 / 执行历史。
///
/// ⚠️ **回调一律抓弱引用**：这些按钮挂在 `ui.page` 上，而 `ui.page` 归
/// `PanelState` 管——抓强引用就是 `PanelState → 页 → 按钮 → 回调 →
/// PanelState` 的环，面板永远释放不掉（AGENTS.md 的引用环红线）。
pub fn refresh_empty(st: &Rc<PanelState>, ui: &DbUi) {
    let box_ = &ui.empty_box;
    // 清掉旧内容（重建）
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
    let connected = ui.connected.get();
    let cur = ui.current.borrow().clone();

    // ================= 品牌标识 =================
    //
    // ⚠️ 图标用 `appicon` 里**内嵌的那张 PNG**：不另找素材，也保证
    // 「图标 = 应用」这件事在窗口标题、关于页、这里都是同一张。
    if let Some(mark) = empty_brand_mark() {
        mark.set_halign(gtk::Align::Center);
        box_.append(&mark);
    }
    let title = gtk::Label::new(Some("PuTTYPlus"));
    title.add_css_class("pp-empty-name");
    // ⚠️ **招牌是居中的**：产品名 + 图标居中成一座"牌"，下面才接左对齐的活。
    title.set_xalign(0.5);
    box_.append(&title);
    let what = gtk::Label::new(Some(tr("数据库工作台")));
    what.add_css_class("pp-empty-what");
    what.set_xalign(0.5);
    box_.append(&what);

    // ================= 现在开着什么 =================
    //
    // ⚠️ 用户 2026-11-02：「'这个会话的数据库连接'放在这里也没什么用，
    // 我进入这个页面肯定是我点了某个数据库连接或者新建了数据库连接，
    // 那我的目的就是明确的，就是为了打开那个数据库」
    // ⇒ **那一列连接整个删掉**（连没连上都删）：能进到这一屏，说明用户
    //   刚选好了 ⇒ 一列选择器纯属多余；没连上时那一列也帮不上忙
    //   （他得先有连接才有这一列）。
    //
    // ⚠️ 连上了就只留一个 **chip**，说明"你现在开着哪个库"（**不可点**）。
    if connected {
        // 分隔线：把"招牌"和"手上的活"分开（否则一坨内容看不出层次）
        //
        // ⚠️ **用 `gtk::Separator` 而不是自己拿 Box 画**：空 Box 的背景色在
        // GTK4 里不画出来（实测：`.pp-empty-sep` 的 background-color 完全没
        // 生效 ⇒ 那条线根本看不见）。Separator 自己会画。
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.add_css_class("pp-empty-sep");
        box_.append(&sep);

        if let Some(p) = &cur {
            box_.append(&empty_chip(
                "drive-harddisk-symbolic",
                &p.name,
                &p.summary(),
            ));
        }
    }

    // ================= 动作 =================
    //
    // ⚠️ 用户 2026-11-02：「新建查询和搜索对象点了没有任何作用，
    // 不知道他们的目的是什么。」⇒ 留下的每个动作都要**看得见效果**：
    //   · 浏览对象 → 对象树**摆出来**（它可能正收着！）并拿到焦点；
    //   · 写一条查询 → 底下那个编辑器清空并聚焦（它是**一直可见**的）；
    //   · 执行历史 → 弹出历史窗口。
    //
    // 「搜索对象」**去掉了**：搜索框就在对象树顶上（树一摆出来就看见了），
    // 再摆一个「聚焦那个框」的按钮意义不大——那正是"点了没反应"的来源。
    if connected {
        // ---- 浏览对象：把对象树摆出来 + 聚焦 ----
        {
            let b = empty_row("view-list-symbolic", tr("浏览对象"));
            b.set_tooltip_text(Some(
                tr("展开左侧对象树，按模式找表 / 视图 / 函数；双击表看数据"),
            ));
            let weak = Rc::downgrade(st);
            b.connect_clicked(move |_| {
                let Some(st) = weak.upgrade() else { return };
                let (toggle, view) = {
                    let slot = st.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    (Rc::clone(&ui.tree_toggle), ui.tree.view.clone())
                };
                // ⚠️ **树可能正收着**（阶段 66 的折叠开关）⇒ 先把它摆出来，
                // 否则又是「点了没反应」。
                toggle.apply(false);
                view.grab_focus();
            });
            box_.append(&b);
        }
        // ---- 写一条查询：清空下面的编辑器并聚焦 ----
        {
            let b = empty_row("text-x-generic-symbolic", tr("写一条查询"));
            b.set_tooltip_text(Some(
                tr("清空下面的 SQL 编辑器并聚焦，直接开始写一条查询（Ctrl+Enter 执行）"),
            ));
            let weak = Rc::downgrade(st);
            b.connect_clicked(move |_| {
                let Some(st) = weak.upgrade() else { return };
                // ⚠️ **借用要尽快结束**：`db_ui` 里的控件随时可能回调进来
                // （树发请求、按钮触发…），借着不放就是双重借用 → panic。
                let sql = {
                    let slot = st.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    ui.sql.clone()
                };
                let buf = sql.buffer();
                buf.set_text("");
                sql.grab_focus();
                let slot = st.db_ui.borrow();
                if let Some(ui) = slot.as_ref() {
                    set_status(ui, tr("写好 SQL 后按 Ctrl+Enter 执行"), false);
                }
            });
            box_.append(&b);
        }
        // ---- 执行历史：弹出历史窗口 ----
        {
            let b = empty_row("document-open-recent-symbolic", tr("执行历史"));
            b.set_tooltip_text(Some(tr("看跑过的 SQL，双击一条把它填回编辑器")));
            let weak = Rc::downgrade(st);
            b.connect_clicked(move |_| {
                let Some(st) = weak.upgrade() else { return };
                // ⚠️ **先把按钮拿出来再 emit**：emit 触发的回调里也会借
                // `db_ui`（`btn_history` 的处理器就是 `st.db_ui.borrow()`），
                // 借着不放就是双重借用 → panic。
                let btn = {
                    let slot = st.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    ui.btn_history.clone()
                };
                btn.emit_clicked();
            });
            box_.append(&b);
        }
    }

    // ---- 提示 ----
    //
    // ⚠️ **不再摆快捷键提示**（阶段 96）：工具栏上已经有一模一样的
    // 「Ctrl+Enter 执行全部 · Ctrl+Shift+Enter 只跑选中的那段」
    // （`meta` 那行，见上面 `sql_row`）⇒ 卡片里再摆一遍是**重复**，
    // 而且那行字不换行时自然宽 ~450px ⇒ 把整张卡撑宽（审计 P0-2 的老毛病）。
    // 这里只留**工具栏上没有**的那句。
    let hint = gtk::Label::new(Some(if connected {
        tr("左侧双击表看数据")
    } else {
        tr("先在左侧打开一个数据库连接")
    }));
    hint.add_css_class("pp-empty-hint");
    hint.set_xalign(0.0);
    box_.append(&hint);
}

/// 空态里的**品牌标识**（应用自己的图标）。
///
/// ⚠️ 用 `appicon` 里**内嵌的那张 PNG**（跟窗口标题、关于页同一张）：
/// 不另找素材 ⇒ 不会出现"图标对不上"的情况。
fn empty_brand_mark() -> Option<gtk::Image> {
    let tex = crate::appicon::texture()?;
    let img = gtk::Image::from_paintable(Some(&tex));
    img.set_pixel_size(56);
    Some(img)
}

/// 空态里的一个 **chip**（图标 + 名字 + 淡色摘要；只表示状态，**不能点**）。
///
/// 用在"当前连的是哪个库"上——以前那一列是**可点的连接列表**，可用户
/// 就是点了某个连接才进来的 ⇒ 那一列纯属多余（用户 2026-11-02）。
///
/// ⚠️ **名字和摘要分成两个 Label**（跟下面连接行一个写法）：合成一个
/// 字符串的话，省略号会把**名字**也一起吃掉（"postgres（pg 127.0.0.1:…"）
/// ⇒ 最该看清的那截反而没了。
fn empty_chip(icon: &str, name: &str, summary: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("pp-empty-chip");
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(14);
    row.append(&img);
    let n = gtk::Label::new(Some(name));
    n.add_css_class("pp-empty-chip-label");
    n.set_xalign(0.0);
    n.set_ellipsize(gtk::pango::EllipsizeMode::End);
    n.set_max_width_chars(16);
    row.append(&n);
    let sep = gtk::Label::new(Some("·"));
    sep.add_css_class("pp-empty-chip-sub");
    row.append(&sep);
    let s = gtk::Label::new(Some(summary));
    s.add_css_class("pp-empty-chip-sub");
    s.set_xalign(0.0);
    s.set_ellipsize(gtk::pango::EllipsizeMode::End);
    s.set_max_width_chars(26);
    row.append(&s);
    row
}

/// 空态卡里的一行（图标 + 文字），**整行可点**。
///
/// ⚠️ **不能只用 `gtk::Button::with_label`**：那样按钮里只有一个 Label，
/// 放不了图标；这里自己拼一层 Box（图标 + 左对齐文字）。
/// ⚠️ **文字要 `set_hexpand(true)`**：不设的话它不占满按钮 ⇒ 行末空一块、
/// 跟同列的其它行对不齐（阶段 96 就是为了消灭这种参差）。
fn empty_row(icon: &str, label: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("pp-empty-btn");
    // ⚠️ 普通行用 `flat`（无边框，悬停才高亮）⇒ 一列行看起来像**列表**
    // 而不是一堆按钮；主 CTA 不能加（`.pp-primary` 的实心底会被 flat 盖掉）。
    b.add_css_class("flat");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    row.append(&img);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    row.append(&l);
    b.set_child(Some(&row));
    b
}

/* ---------------- 事件接线 ---------------- */

/// 连接信号。闭包只捕获 `Rc<PanelState>`，点下时再从 `db_ui` 借到控件。
fn wire(st: &Rc<PanelState>, ui: &DbUi) {
    {
        let st2 = Rc::clone(st);
        ui.btn_run.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                do_run(&st2, ui, false);
            }
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.btn_run_sel.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                do_run(&st2, ui, true);
            }
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.btn_cancel.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                do_cancel(&st2, ui);
            }
        });
    }
    // Ctrl+Enter 在 SQL 编辑器里执行
    let key = gtk::EventControllerKey::new();
    {
        let st2 = Rc::clone(st);
        key.connect_key_pressed(move |_, keyval, _, mods| {
            if (keyval == gtk::gdk::Key::Return || keyval == gtk::gdk::Key::ISO_Enter)
                && mods.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            {
                // Ctrl+Shift+Enter = 只跑选中的那段
                let only_sel = mods.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                if let Some(ui) = st2.db_ui.borrow().as_ref() {
                    do_run(&st2, ui, only_sel);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    ui.sql.add_controller(key);

    // SQL 编辑器语法高亮（内容变了就防抖重画）
    {
        let hl = Rc::clone(&ui.sql_hl);
        let hl2 = Rc::clone(&hl);
        ui.sql.buffer().connect_changed(move |_| {
            hl2.schedule();
        });
        hl.apply();
    }

    // 「历史」按钮：看跑过的 SQL（11.2）
    {
        let st2 = Rc::clone(st);
        ui.btn_history.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                open_history_window(&st2, ui);
            }
        });
    }

    // 对象搜索（11.1）：按钮点一下 / 框里按回车，都发一次全库搜索
    {
        let st2 = Rc::clone(st);
        ui.search_btn.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                do_search(&st2, ui);
            }
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.search_entry.connect_activate(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                do_search(&st2, ui);
            }
        });
    }

    /* ---- 树工具行（阶段 51） ----
     *
     * ⚠️ **借用必须先结束再操作**：树的动作会**同步**触发「row-expanded」
     * → 发元数据请求 → 回调里又要借 `db_ui`。拿着 `db_ui` 的借用去调
     * `set_filter` / `reload`，就是双重借用 → panic（AGENTS.md 的老坑）。
     * 所以这里先把 `Rc<DbTree>` 克隆出来，借用一结束再动树。
     */
    {
        let weak = Rc::downgrade(st);
        ui.btn_refresh.connect_clicked(move |_| {
            let Some(st) = weak.upgrade() else { return };
            let tree = {
                let slot = st.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                Rc::clone(&ui.tree)
            };
            tree.reload();
            let slot = st.db_ui.borrow();
            if let Some(ui) = slot.as_ref() {
                set_status(ui, tr("已刷新对象树"), false);
            }
        });
    }
    {
        let weak = Rc::downgrade(st);
        ui.filter_combo.connect_changed(move |c| {
            let Some(st) = weak.upgrade() else { return };
            // ⚠️ `active_id` 认的是**下拉的 id**，不是显示文字
            let id = c.active_id().unwrap_or_default().to_string();
            let f = dbtree::TreeFilter::from_id(&id);
            let tree = {
                let slot = st.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                Rc::clone(&ui.tree)
            };
            tree.set_filter(f);
        });
    }
    {
        let weak = Rc::downgrade(st);
        ui.detail_check.connect_toggled(move |c| {
            let Some(st) = weak.upgrade() else { return };
            let on = c.is_active();
            let tree = {
                let slot = st.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                Rc::clone(&ui.tree)
            };
            tree.set_detail_mode(on);
        });
    }

    // 对象浏览器：展开 → 查元数据；双击表 → 开数据页签
    {
        let st2 = Rc::clone(st);
        ui.tree.set_request_handler(move |req| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                send_meta(&st2, ui, req);
            }
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.tree.set_open_table_handler(move |schema, table, is_view| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                open_table(&st2, ui, schema, table, is_view);
            }
        });
    }
    // 对象树上的 DDL（9.6）：新建表 / 改名 / 删表
    {
        let st2 = Rc::clone(st);
        ui.tree.set_new_table_handler(move |schema| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_create_table_dialog(&st2, ui, schema);
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.tree.set_rename_table_handler(move |schema, table| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_rename_table_dialog(&st2, ui, schema, table);
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.tree.set_drop_table_handler(move |schema, table| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            drop_table_confirm(&st2, ui, schema, table);
        });
    }
    // 右键「对象属性…」（13.5）：Navicat 的 Details 面板
    //
    // 阶段 72e：切到那张表的**表文档**里的「属性」视图，不再弹独立窗口
    // （审计 P1-7："属性作为同一表文档的标签"）。
    {
        let st2 = Rc::clone(st);
        ui.tree.set_object_stats_handler(move |schema, table| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_object_stats(&st2, ui, schema, table);
        });
    }
    // 双击事件：打开定义页签（9.3 的事件对象）
    {
        let st2 = Rc::clone(st);
        ui.tree.set_open_event_handler(move |schema, name| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_event_def(&st2, ui, schema, name);
        });
    }
    // 右键「导入数据（CSV）…」（阶段 18 的 Import Wizard，阶段 72 改成**直接**
    // 进导入向导——以前这里是「先选方向、再进 CSV 向导」的两层）。
    {
        let st2 = Rc::clone(st);
        ui.tree.set_import_csv_handler(move |schema, table| {
            let parent = {
                let slot = st2.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                ui.win.clone()
            };
            crate::panel::dbtransfer::open_import(
                &st2,
                Some(&parent),
                Some((schema.to_string(), table.to_string())),
            );
        });
    }
    // 右键「导出表…」（阶段 52）：带着这张表打开导出向导
    {
        let st2 = Rc::clone(st);
        ui.tree.set_export_table_handler(move |schema, table| {
            let parent = {
                let slot = st2.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                ui.win.clone()
            };
            crate::panel::dbtransfer::open_export(
                &st2,
                Some(&parent),
                Some((schema.to_string(), table.to_string())),
            );
        });
    }
    // 右键「ER 图…」（阶段 19）：表 → 看这张表的关系；模式 → 整个模式
    //
    // 阶段 72e：**表级**的 ER 图切到那张表的表文档里的「ER」视图；**模式级**
    // 的 ER 图（整个模式）不是某张表的侧面 ⇒ 仍是独立页。
    {
        let st2 = Rc::clone(st);
        ui.tree.set_er_handler(move |schema, table| {
            // `table = None` = 整个模式（不是某张表的侧面）⇒ 仍是独立页
            let Some(table) = table else {
                open_er(&st2, schema, None);
                return;
            };
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if !ui.connected.get() {
                set_status(ui, tr("尚未连接数据库"), true);
                return;
            }
            let doc = find_or_create_doc(&st2, ui, schema, table, false);
            focus_doc(Rc::clone(&st2), ui, &doc, DOC_PAGE_ER);
        });
    }
    // 右键「表结构」：打开表结构页签（设计器）
    {
        let st2 = Rc::clone(st);
        ui.tree.set_open_structure_handler(move |schema, table| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_structure(&st2, ui, schema, table);
        });
    }
    // 双击函数 / 存储过程：打开定义页签
    {
        let st2 = Rc::clone(st);
        ui.tree
            .set_open_routine_handler(move |schema, name, args, is_proc| {
                let slot = st2.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                open_routine_def(&st2, ui, schema, name, args, is_proc);
            });
    }
    // 双击触发器：打开定义页签
    {
        let st2 = Rc::clone(st);
        ui.tree.set_open_trigger_handler(move |schema, table, name| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_trigger_def(&st2, ui, schema, table, name);
        });
    }
    {
        let st2 = Rc::clone(st);
        ui.tree.set_copy_handler(move |name| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                ui.win.clipboard().set_text(name);
                set_status(ui, &trf!("已复制：{name}", name = name), false);
            }
        });
    }
    update_buttons(ui);
    // 空态建一次（阶段 51）
    refresh_empty(st, ui);
}

/// 当前连接该**默认展开哪个模式**（阶段 72d / 审计 P2-11）。
///
/// - PG：`public`（标准模式）；
/// - MySQL：**当前库**（连接配置里的库名）。
fn default_schema_of(ui: &DbUi) -> Option<String> {
    let cur = ui.current.borrow().clone()?;
    match cur.proto {
        DbProto::Postgres => Some("public".to_string()),
        DbProto::MySql => {
            if cur.dbname.is_empty() {
                None
            } else {
                Some(cur.dbname.clone())
            }
        }
    }
}

/// 当前连接的方言（没选中连接时按 PG 算——只是兜底，不该走到）。
fn current_proto(ui: &DbUi) -> DbProto {
    ui.current
        .borrow()
        .as_ref()
        .map(|p| p.proto)
        .unwrap_or(DbProto::Postgres)
}

/// 从凭据库取这条连接的密码（没记 / 库锁着 / 键名对不上都返回 None）。
fn stored_password(key: &str, name: &str) -> Option<zeroize::Zeroizing<String>> {
    if !vault::exists() || !vault::is_unlocked() {
        return None;
    }
    vault::get(&dbstore::vault_name(key, name))
        .or_else(|| vault::get(&dbstore::legacy_vault_name(key)))
}

/* ---------------- 连接 ---------------- */

/// 下拉里一条连接的显示文字：`名字`（摘要）。
///
/// **带摘要**：同一个会话下可能有两条都叫「本地库」但地址不同的连接，
/// 光看名字分不清连的是哪个库。
fn conn_label_of(p: &DbProfile) -> String {
    trf!("{v0}（{v1}）", v0 = p.name, v1 = p.summary())
}

/// 设定「当前连接」。
///
/// 换连接 = 换库 = **断开重连**（已经连着的话）。不换的话工作台里的
/// 对象树还指着旧库，而状态栏说"已连接"——用户以为在查新库，其实在看旧库。
fn set_current_conn(st: &Rc<PanelState>, ui: &DbUi, key: &str, name: &str) {
    let Some(p) = dbstore::get(key, name) else {
        return;
    };
    if ui.connected.get() {
        // 已连着 → 先断开（worker 那边会关掉旧连接）
        if let Some(tx) = st.tx.borrow().as_ref() {
            let _ = tx.send(WorkerCmd::DbClose);
        }
        ui.connected.set(false);
        clear_tabs(ui);
        ui.tree.clear();
    }
    *st.db_active.borrow_mut() = None;
    *ui.current.borrow_mut() = Some(p.clone());
    ui.tree.set_proto(p.proto);
    // 记住这次选了哪条：下次打开工作台还选它
    crate::settings::set_last_db_connection(&ui.session_key, &p.name);
    // 页签头写清楚连的是哪条（工作台里不再摆连接信息了）
    update_tab_title(st, ui);
    set_status(ui, &trf!("当前连接：{v0}", v0 = conn_label_of(&p)), false);
    update_buttons(ui);
    // 空态跟着换（阶段 51：副标题写的是"连接 XX"）
    refresh_empty(st, ui);
}

/// 页签标题写上当前连接名：`数据库 · 本地 PG`。
///
/// **为什么放页签头**：阶段 22 起工作台里不再有「当前连接」那一行，
/// 用户得一眼看出"这个工作台连的是哪个库"——页签头是最显眼的位置。
fn update_tab_title(st: &Rc<PanelState>, ui: &DbUi) {
    let name = ui.current.borrow().as_ref().map(|p| p.name.clone());
    let title = match name {
        Some(n) => trf!("数据库 · {n}", n = n),
        None => tr("数据库").to_string(),
    };
    // 没有子标签容器时（老路径 / 单测 / 示例）没有页签头可改，跳过即可
    if let Some(sub) = st.sub_tabs() {
        sub.set_title(ui.tab_id.get(), &title);
    }
}

///
/// **返回值** = 「当场就能发现的错误」（没选连接 / 参数不完整 / worker 没了）。
/// 异步的连接结果走 `on_connected`。返回值是为了让调用方能在**释放
/// `st.db_ui` 借用之后**再弹对话框（弹窗时页面可能还没挂 ⇒ 状态行没人看）。
fn do_connect(st: &Rc<PanelState>, ui: &DbUi) -> Option<String> {
    if ui.busy.get() {
        return None;
    }
    // **连接信息来自「当前连接」**（阶段 13：不在工作台里填表了）。
    // 没有选中连接 → 说清楚下一步该做什么，而不是默默失败。
    let Some(cur) = ui.current.borrow().clone() else {
        let msg = tr("还没有选中数据库连接：先在会话树 / 连接管理器里加一个（右键会话树 →「新建数据库连接…」）");
        set_status(ui, msg, true);
        return Some(msg.to_string());
    };
    // 现读一次配置：连接管理器里可能**改过**地址 / 端口 / 库名，
    // 缓存里的那份是旧的（用户明明改了，却还在拿旧地址去连）。
    // 读不到（比如临时连接没落盘）就用缓存的那份。
    let p = dbstore::get(&ui.session_key, &cur.name).unwrap_or(cur);
    if p.host.trim().is_empty() {
        let msg = tr("这条连接的数据库地址是空的（去连接管理器里改）");
        set_status(ui, msg, true);
        return Some(msg.to_string());
    }
    if p.port == 0 {
        let msg = tr("这条连接的端口无效（去连接管理器里改）");
        set_status(ui, msg, true);
        return Some(msg.to_string());
    }
    if p.user.trim().is_empty() {
        let msg = tr("这条连接的数据库用户是空的（去连接管理器里改）");
        set_status(ui, msg, true);
        return Some(msg.to_string());
    }
    // 密码：先看「当场输入过 / 临时连接」的（不进凭据库），
    // 再看凭据库里记住的（冒烟测试的临时连接走前者 —— 它不进凭据库）
    let password = match st.db_pass.borrow().get(&p.name).cloned() {
        Some(pw) => Some(pw),
        None if p.remember => stored_password(&ui.session_key, &p.name),
        None => None,
    };
    let cfg = DbConfig {
        proto: p.proto,
        host: p.host.clone(),
        port: p.port,
        user: p.user.clone(),
        dbname: p.dbname.clone(),
        password,
    };
    // **历史的分组键在这里定死**：按「会话 + 数据库连接名」分——同一个 SSH
    // 会话下挂了几个库，历史要能分开（Navicat 的 History 就是按连接分的）。
    *ui.hist_key.borrow_mut() = dbhist::hist_key(&ui.session_key, &p.name);
    // 记住这次连的是哪条：下次打开工作台还选它。
    //
    // ⚠️ **只记已存的连接**：冒烟测试塞进来的临时连接不在 databases.ini 里，
    // 记进用户设置就是测试污染了用户配置（"上次用的连接"指向一条并不存在
    // 的连接，下次打开工作台还会去选它）。
    if dbstore::get(&ui.session_key, &p.name).is_some() {
        crate::settings::set_last_db_connection(&ui.session_key, &p.name);
    }

    ui.busy.set(true);
    ui.connected.set(false);
    // 活跃标记（阶段 57）：正在连 → 先灭掉，等连上了再亮
    *st.db_active.borrow_mut() = None;
    clear_tabs(ui);
    ui.tree.clear();
    update_buttons(ui);
    set_status(ui, tr("正在连接数据库…"), false);
    // 空态跟着换（阶段 51）
    refresh_empty(st, ui);
    let tx = st.tx.borrow().clone();
    match tx {
        Some(tx) => {
            if tx.send(WorkerCmd::DbConnect { config: cfg }).is_err() {
                ui.busy.set(false);
                let msg = tr("发送连接命令失败（worker 已退出）");
                set_status(ui, msg, true);
                update_buttons(ui);
                return Some(msg.to_string());
            }
        }
        None => {
            ui.busy.set(false);
            let msg = tr("SSH 连接未就绪");
            set_status(ui, msg, true);
            update_buttons(ui);
            return Some(msg.to_string());
        }
    }
    None
}

/// **页面还没挂**的时候，那句状态提示在屏幕上根本不存在 ⇒ 弹成对话框。
///
/// ⚠️ 用户 2026-11-02：「任何时候，必须连上数据库之后才允许显示数据库管理
/// 页面」⇒ 连不上的时候用户面前**什么都没有**，光靠状态行他永远不知道
/// 出了什么事（以前还会开出一个空工作台，看着像功能坏了）。
///
/// 页面**已经**挂着的时候不弹：状态行看得见就够了（用户嫌啰嗦）。
fn report_unmounted(st: &Rc<PanelState>, msg: &str, is_error: bool) {
    let mounted = st
        .db_ui
        .borrow()
        .as_ref()
        .map(|u| u.mounted.get())
        .unwrap_or(false);
    if mounted {
        return;
    }
    let parent = st.topwin();
    if is_error {
        dialogs::show_error(parent.as_ref(), tr("数据库"), msg);
    } else {
        dialogs::info(parent.as_ref(), tr("数据库"), msg);
    }
}

fn do_disconnect(st: &Rc<PanelState>, ui: &DbUi) {
    if !ui.connected.get() {
        return;
    }
    if let Some(tx) = st.tx.borrow().as_ref() {
        let _ = tx.send(WorkerCmd::DbClose);
    }
}

/* ---------------- 查询 ---------------- */

/// 执行 SQL（`only_sel` = 只跑选中的那段）。
fn do_run(st: &Rc<PanelState>, ui: &DbUi, only_sel: bool) {
    if !ui.connected.get() || ui.busy.get() {
        return;
    }
    let buf = ui.sql.buffer();
    let (sql, ok) = if only_sel {
        let sel = buf.selection_bounds();
        match sel {
            Some((s, e)) if s.offset() < e.offset() => {
                let t = buf.text(&s, &e, false).to_string();
                (t, true)
            }
            _ => {
                set_status(ui, tr("没有选中任何 SQL"), true);
                return;
            }
        }
    } else {
        let t = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
        (t, true)
    };
    let _ = ok;
    if sql.trim().is_empty() {
        set_status(ui, tr("SQL 为空"), true);
        return;
    }
    let tab = new_tab(st, ui, &sql);
    // **记进执行历史**（11.2）：语句发出去了就记（不管结果对不对——用户是
    // 靠历史找回"刚才那条跑错了的 SQL"）。
    dbhist::add(&ui.hist_key.borrow(), &sql);
    ui.busy.set(true);
    update_buttons(ui);
    set_status(ui, tr("执行中…"), false);
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Query { tab_id: tab.id });
    send_cmd(st, ui, WorkerCmd::DbQuery { id, sql, page_size: PAGE_SIZE });
}

/// 对象树里双击表 → 开一个数据页签。
/// 当前连接的标识（列显示设置等按它分区；阶段 20）。
///
/// 没连上 / 没选中连接 = 空串（调用方按"没有归属"处理）。
fn conn_key(ui: &DbUi) -> String {
    let cur = ui.current.borrow();
    match cur.as_ref() {
        Some(p) => dbcols::conn_key(p),
        None => String::new(),
    }
}

fn open_table(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str, is_view: bool) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    // 阶段 72e：打开一张表 = 打开/切到**它的表文档**（数据 / 结构 / 属性 / ER
    // 四个视图在一个页里），不再是"数据页 + 三个平级页"。
    let doc = find_or_create_doc(st, ui, schema, table, is_view);
    focus_doc(Rc::clone(st), ui, &doc, DOC_PAGE_DATA);
}

/// 把一份连接配置填进表单（并顺手取回已保存的密码）。
///
/// 密码存在凭据库里，键是 `db:<会话键>/<连接名>`；老版本（一个会话一份
/// 配置）的键是 `db:<会话键>`——先试新的，再试老的，升级不丢密码。
/// 造一个**图标按钮**（图标 + 悬停提示）。
///
/// **为什么工具栏用图标不用文字**：结果表工具栏一排有七八颗按钮，文字版
/// 宽度撑不下——窗口右侧的按钮会被切掉（截图里「放弃」只剩半颗）。图标 +
/// tooltip 是 Navicat / DBeaver 的做法，省地方也认得出。
///
/// 图标名用 Adwaita 的 symbolic 系列（`gtk::Button::from_icon_name`），
/// 项目里侧边栏、标签页、文件编辑器已经这么用了。
fn icon_button(icon: &str, tip: &str) -> gtk::Button {
    let b = crate::a11y::icon_button(icon);
    b.set_tooltip_text(Some(tip));
    // 压内边距（theme.rs 里的 .pp-icon-btn）：一排 5-8 颗时默认 padding 放不下
    b.add_css_class("pp-icon-btn");
    b
}

/// 新建一个结果页签（标题取 SQL 首行的摘要）。
fn new_tab(st: &Rc<PanelState>, ui: &DbUi, sql: &str) -> Rc<ResultTab> {
    let title = sql_title(sql);
    new_tab_with_title(st, ui, &title)
}

fn sql_title(sql: &str) -> String {
    let first = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(tr("查询"));
    let t: String = first.chars().take(28).collect();
    if first.chars().count() > 28 {
        format!("{t}…")
    } else {
        t
    }
}

fn new_tab_with_title(st: &Rc<PanelState>, ui: &DbUi, title: &str) -> Rc<ResultTab> {
    let tab = build_result_tab(st, ui, title);
    mount_result_tab(ui, &tab);
    tab
}

/// 建一个结果页签（**不挂到任何容器**）。
///
/// **为什么拆出来**（阶段 72e）：表文档里的「数据」视图也是结果页签，但它挂
/// 在**文档页的内层页签**里，不是外层 notebook ⇒ 挂载的事交给调用方。
fn build_result_tab(st: &Rc<PanelState>, ui: &DbUi, title: &str) -> Rc<ResultTab> {
    let grid = dbgrid::DbGrid::new();
    // ⚠️ **多选**（阶段 46）：结果网格要能框选多行——dbx 的「批量修改所选」
    // 「删除所选」都靠它。表结构页签的字段表**保持单选**（一次删一个字段，
    // 删字段要一条一条生成 ALTER）。
    grid.set_multi_select(true);
    let filter = gtk::Entry::new();
    // ⚠️ **名字要和「数据库过滤条件」分清**（阶段 72 / 审计 P1-7、P2-10）：
    // 这个框是**在已经取回的行里**按文字筛（不查库），那个按钮是**生成 WHERE
    // 重新查库**。以前两个都叫"过滤"，用户点错了才发现不是一回事。
    filter.set_placeholder_text(Some(tr("筛选已加载行")));
    filter.set_tooltip_text(Some(tr("在已加载的行里按文字筛选（不区分大小写，不查库）")));
    // ⚠️ **宽度按"显示列"算**：一个汉字占两列，「筛选已加载行」是 6 个汉字
    // ⇒ 12 列才放得下，宽度不够会被截成「筛选已加…」。
    filter.set_width_chars(14);
    // **不许抢剩余空间**：筛选框一旦 hexpand，右侧的按钮就被挤出可视区
    // （截图里「放弃」只剩半颗）。限宽 + 不扩展，按钮才有位置。
    filter.set_max_width_chars(16);
    filter.set_hexpand(false);
    // 搜索图标：筛选框跟旁边一堆图标按钮挨着，加个图标才认得出是筛选
    filter.set_icon_from_icon_name(
        gtk::EntryIconPosition::Primary,
        Some("system-search-symbolic"),
    );

    let status = gtk::Label::new(Some(tr("就绪")));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // 状态文字会很长（「已标记 3 行删除，点保存修改才真正删」），限宽 + 省略号，
    // 完整内容放 tooltip 里——不能让它把工具栏挤没。
    status.set_max_width_chars(18);
    // 文字一变就同步 tooltip：省得每处 set_text 都记得再设一遍
    status.connect_notify_local(Some("label"), |l, _| {
        let t = l.text();
        l.set_tooltip_text(Some(t.as_str()));
    });

    // 工具栏**全用图标按钮**：这一排有下一页 / 导出 / 编辑 / 插入行 /
    // 删除行 / 保存修改 / 放弃，用文字的话右侧会被窗口切掉
    // （截图里「放弃」只剩半颗）。图标 + tooltip 既省地方又看得懂。
    let btn_next = icon_button("go-next-symbolic", tr("下一页"));
    btn_next.set_sensitive(false);
    // 导出：**一个**菜单按钮，格式在菜单里选（阶段 21）
    let btn_export = gtk::MenuButton::new();
    {
        let img = gtk::Image::from_icon_name("document-export-symbolic");
        img.set_pixel_size(16);
        btn_export.set_child(Some(&img));
    }
    crate::a11y::watch_tooltip(&btn_export);
    btn_export.set_tooltip_text(Some(tr("导出当前结果：CSV / SQL / JSON / XML / HTML")));
    btn_export.add_css_class("pp-icon-btn");
    let btn_edit = icon_button("document-edit-symbolic", tr("进入编辑模式（开事务，可改单元格）"));
    btn_edit.set_sensitive(false);
    // 插入 / 删除行（9.4）：都在编辑模式（事务里）下生效
    let btn_insert = icon_button("list-add-symbolic", tr("插入行：在末尾追加一行新记录（没填的列用数据库默认值）"));
    btn_insert.set_sensitive(false);
    let btn_delete = icon_button("list-remove-symbolic", tr("删除行：把选中的行标记为删除（点保存才真正删）"));
    btn_delete.set_sensitive(false);
    let btn_save = icon_button("document-save-symbolic", tr("保存修改：提交事务（任何一条失败就整体回滚）"));
    btn_save.set_sensitive(false);
    // ⚠️ **编辑模式下「保存修改」是主操作**（阶段 44）：dbx 的「提交」也是
    // 实心主按钮。阶段 39 把按钮文字全去掉后，一排图标权重全一样，
    // 用户不知道该点哪个——主操作要自己跳出来。
    btn_save.add_css_class("pp-primary");
    let btn_discard = icon_button("edit-undo-symbolic", tr("放弃修改：回滚事务，网格恢复原样"));
    btn_discard.set_sensitive(false);
    // 数据库过滤条件（9.5，阶段 72 改名）：填 Where 条件后**重新查库**
    // （服务端过滤，不是本地筛行）
    let btn_filter = icon_button(
        "view-filter-symbolic",
        tr("数据库过滤条件：生成 WHERE 条件重新查库（不只筛当前这一页）"),
    );
    btn_filter.set_sensitive(false);
    // 「只读 / 可编辑」标志（阶段 72c）：先按"只读"造，开了表再改成"可编辑"
    let mode_chip = gtk::Label::new(Some(tr("只读")));
    mode_chip.add_css_class("pp-chip");
    mode_chip.set_tooltip_text(Some(tr("这一页是只读的：普通查询结果不能改（表浏览页签才可编辑）")));

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    // ⚠️ **标志放最左边**：它是"这一页能不能改"的唯一说明，埋在工具栏中间
    // 等于没有。
    head.append(&mode_chip);
    head.append(&filter);
    head.append(&status);
    head.append(&btn_filter);
    // ⚠️ **工具栏里没有「列显示设置」按钮**（阶段 72 / 审计 P1-7、P2-10）：
    // 列设置以前有两个入口（这颗按钮 + 列头的「⋮」菜单）⇒ 审计说"列设置
    // 只保留一个入口"。现在只留在列头菜单里（`DbGrid::set_col_setup_hook`）。
    // ⚠️ **「下一页」挪到底栏了**（阶段 45）：dbx 的分页控件在**底部状态栏**
    // 里，不在上面的工具条。一个 widget 不能 append 进两个容器（GTK 会报
    // `gtk_box_append: assertion 'parent == NULL' failed`），所以工具栏
    // 这边必须去掉，只留底栏那一颗。
    head.append(&btn_export);
    head.append(&btn_edit);
    head.append(&btn_insert);
    head.append(&btn_delete);
    head.append(&btn_save);
    head.append(&btn_discard);

    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_vexpand(true);
    sw.set_child(Some(&grid.view));
    sw.add_css_class("frame");

    // ---- 空结果态（阶段 83 / 审计 P2-10）----
    //
    // 以前查询跑完、零行返回时，网格区是一整块空白 ⇒ 用户分不清
    // "查完了但没数据" / "还没查" / "查询挂了"。
    let (empty_box, empty_title, empty_desc, empty_cta) = build_grid_empty_state();
    let grid_stack = gtk::Stack::new();
    grid_stack.add_named(&sw, Some("grid"));
    grid_stack.add_named(&empty_box, Some("empty"));
    grid_stack.set_visible_child_name("empty");
    // ⚠️ **Stack 要撑满**：不撑的话它只按内容高占位，下面的底栏会被顶走。
    grid_stack.set_vexpand(true);

    // 面包屑（阶段 43）：dbx 在标签页下面放一排芯片（`库 / 表@模式 / N 字段`），
    // 我们原先只有页签标题，打开一张表后**看不出自己在哪个库哪张表**。
    let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    crumbs.add_css_class("pp-crumbs");

    // ---- 底栏（阶段 45，对标 dbx 的状态栏）----
    //
    // dbx 底部那一长条信息量很大：`共 N 行 | 耗时 | N 项待保存 | 当前 SQL |
    // SUM/AVG | 每页行数 | 分页控件 | 导出`。我们原先只有页签工具栏里那颗
    // 「下一页」按钮，其余信息全缺——用户看不出"这一页取了多少行""还有没有
    // 下一���"。
    let foot_info = gtk::Label::new(Some(""));
    foot_info.add_css_class("dim-label");
    foot_info.set_xalign(0.0);
    foot_info.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // ⚠️ **底栏左侧不要抢剩余空间**：挤走的正是右边的分页控件（下拉框会被
    // 压成只剩一个箭头，文字全看不见）。改成"文字按内容宽 + 中间一个
    // hexpand 的空隙撑开"。
    foot_info.set_max_width_chars(28);
    // 中间的空隙：把分页控件推到最右
    let foot_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    foot_spacer.set_hexpand(true);

    let btn_first = icon_button("go-top-symbolic", tr("首页：回到第一页"));
    btn_first.set_sensitive(false);
    let btn_prev = icon_button("go-previous-symbolic", tr("上一页"));
    btn_prev.set_sensitive(false);
    let foot_page = gtk::Label::new(Some(tr("第 1 页")));
    foot_page.add_css_class("dim-label");
    // 每页行数：dbx 有这个下拉，改了立刻重查
    let btn_size = gtk::ComboBoxText::new();
    for n in [100usize, 200, 500, 1000] {
        btn_size.append(Some(&trf!("{n} 行/页", n = n)), &n.to_string());
    }
    btn_size.set_active_id(Some("200"));
    btn_size.set_tooltip_text(Some(tr("每页取多少行（改了会重新查）")));
    // 限宽：不设的话它会被旁边的控件挤成只剩一个箭头，文字看不见
    btn_size.set_size_request(96, -1);

    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    foot.append(&foot_info);
    foot.append(&foot_spacer);
    foot.append(&btn_size);
    foot.append(&btn_first);
    foot.append(&btn_prev);
    foot.append(&foot_page);
    foot.append(&btn_next);

    // 条件条（阶段 48）：挑列 / 挑运算符 / 填值 → 加条件；右边挑排序列与方向。
    // 整条默认藏着，只有表浏览页签会把它亮出来。
    let cond_bar = build_cond_bar();

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    // ⚠️ **要撑满**：阶段 72e 之后它多了一层（表文档的内层页签），不撑的话
    // 内容只按自然高度占位，网格被裁掉一截。
    box_.set_vexpand(true);
    box_.append(&crumbs);
    box_.append(&head);
    box_.append(&cond_bar.root);
    box_.append(&grid_stack);
    box_.append(&foot);

    let tab = Rc::new(ResultTab {
        id: next_id(ui),
        title: title.to_string(),
        st: Rc::downgrade(st),
        win: ui.win.clone(),
        grid,
        filter,
        status,
        btn_next,
        btn_export,
        btn_edit,
        btn_insert,
        btn_delete,
        btn_save,
        btn_discard,
        btn_filter,
        target: RefCell::new(None),
        col_key: RefCell::new(None),
        types: RefCell::new(Vec::new()),
        pk: RefCell::new(Vec::new()),
        col_names: RefCell::new(Vec::new()),
        filter_conds: RefCell::new(Vec::new()),
        filter_where: RefCell::new(String::new()),
        editing: Cell::new(false),
        in_tx: Cell::new(false),
        has_more: Cell::new(false),
        pending_row_action: Cell::new(None),
        crumbs,
        foot_info,
        foot_page,
        btn_first,
        btn_prev,
        btn_size,
        page: Cell::new(1),
        page_size: Cell::new(PAGE_SIZE),
        offset: Cell::new(0),
        last_rows: Cell::new(0),
        last_elapsed: Cell::new(0),
        cond_bar: Rc::clone(&cond_bar),
        order_col: RefCell::new(String::new()),
        order_asc: Cell::new(true),
        order_by: RefCell::new(String::new()),
        cond_filled: Cell::new(false),
        mode_chip,
        grid_stack,
        empty_title,
        empty_desc,
        empty_cta,
        box_,
    });

    // 「隐藏此列」（阶段 46）：网格只管"用户点了隐藏"，**落盘由这里做**
    // （列显示设置存在 `columns.ini`，按连接 + 表分区，网格不知道这些）。
    //
    // ⚠️ **必须抓弱引用**：网格归 `ResultTab` 所有，回调若抓 `Rc<ResultTab>`，
    // 就是 `ResultTab → 网格 → 回调 → ResultTab` 的引用环，页签永远释放不掉。
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_hide_col_hook(Rc::new(move |orig| {
            let Some(tab) = weak_tab.upgrade() else { return };
            hide_result_column(&tab, orig);
        }));
    }

    // 「列显示设置…」（阶段 72 / 审计 P1-7、P2-10）：列设置的**唯一入口**。
    //
    // 工具栏上那颗「列显示设置」按钮撤了（两个入口 ⇒ 用户不知道该点哪个），
    // 列设置只留在列头的「⋮」菜单里。
    //
    // ⚠️ **必须抓弱引用**：同上，抓强引用就是引用环。
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_col_setup_hook(Rc::new(move || {
            let Some(tab) = weak_tab.upgrade() else { return };
            open_column_dialog(&tab);
        }));
    }

    // 网格右键菜单（阶段 47）：弹窗口 / 弹对话框这些要 dbui 的上下文，
    // 所以网格把动作抛到这里。
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_row_menu_hook(Rc::new(move |action| {
            let Some(tab) = weak_tab.upgrade() else { return };
            let Some(st) = tab.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            handle_grid_menu(&st, ui, &tab, action);
        }));
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_status_hook(Rc::new(move |msg| {
            let Some(tab) = weak_tab.upgrade() else { return };
            tab.status.set_text(msg);
            let Some(st) = tab.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            set_status(ui, msg, false);
        }));
    }

    // 「有改动」通知 → 底栏的「N 项待保存」实时刷新（阶段 45）
    //
    // ⚠️ **必须抓弱引用**：网格归 `ResultTab` 所有，回调若抓 `Rc<ResultTab>`，
    // 就是 `ResultTab → 网格 → 回调 → ResultTab` 的引用环，页签永远释放不掉。
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_change_hook(Rc::new(move || {
            let Some(tab) = weak_tab.upgrade() else { return };
            let Some(st) = tab.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            update_foot(ui, &tab);
        }));
    }

    // 「数据变了」通知 → 网格 ↔ 空结果态来回切（阶段 83 / 审计 P2-10）
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.grid.set_data_hook(Rc::new(move || {
            let Some(tab) = weak_tab.upgrade() else { return };
            update_grid_empty_state(&tab);
        }));
    }

    // 空结果态的主 CTA：「插入一行」＝ 工具栏那颗按钮的同一个动作
    //
    // ⚠️ **不是说明书**：审计 §4.1 说"凡是需要用文字叙述点击路径的地方，
    // 说明这个动作没有以按钮的形式站在用户面前" ⇒ 这里给**真按钮**，
    // 而且跟工具栏那颗走**同一个函数**（不许两份逻辑）。
    {
        let tab2 = Rc::clone(&tab);
        tab.empty_cta.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_insert_row(&st, ui, &tab2);
        });
    }

    wire_tab(&tab);
    // 初始是"非编辑模式"：保存/放弃先藏着
    set_edit_toolbar(&tab, false);
    // 初始的空结果态**要真算一遍**：不能靠标签的默认文字（那是"还没跑"的那
    // 一句）——表文档的「数据」视图建出来时就带 target，两种说法不一样。
    update_grid_empty_state(&tab);

    ui.tabs.borrow_mut().push(Rc::clone(&tab));
    tab
}

/// 把结果页签挂到**外层** notebook（查询结果走这条路；表数据走表文档）。
///
/// ⚠️ 顺手把「关闭这个结果页签」那颗 × 接上——以前它**建了但没接事件**
/// （点了没反应，界面还不报错）。
fn mount_result_tab(ui: &DbUi, tab: &Rc<ResultTab>) {
    // 页签标题：文字 + 关闭按钮
    let label = gtk::Label::new(Some(&tab.title));
    let close = gtk::Button::new();
    close.add_css_class("flat");
    let img = gtk::Image::from_icon_name("window-close-symbolic");
    img.set_pixel_size(12);
    close.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&close);
    close.set_tooltip_text(Some(tr("关闭这个结果页签")));
    let tab_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_head.append(&label);
    tab_head.append(&close);
    {
        // ⚠️ **抓弱引用**：回调抓 `Rc<ResultTab>` 就是 `ResultTab → 回调 →
        // ResultTab` 的环，页签永远释放不掉。
        let weak = Rc::downgrade(tab);
        close.connect_clicked(move |_| {
            let Some(tab) = weak.upgrade() else { return };
            let Some(st) = tab.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if let Some(pos) = ui.notebook.page_num(&tab.box_) {
                ui.notebook.remove_page(Some(pos));
            }
            ui.tabs.borrow_mut().retain(|t| !Rc::ptr_eq(t, &tab));
            if ui.notebook.n_pages() == 0 {
                ui.stack.set_visible_child_name("empty");
            }
        });
    }
    ui.notebook.append_page(&tab.box_, Some(&tab_head));
    ui.notebook.set_current_page(Some(ui.notebook.n_pages() - 1));
    ui.stack.set_visible_child_name("pages");
}

/// 给页签接上事件。
fn wire_tab(tab: &Rc<ResultTab>) {
    // 筛选
    {
        let grid = Rc::clone(&tab.grid);
        let f = tab.filter.clone();
        f.connect_changed(move |e| {
            grid.set_filter(&e.text());
        });
    }
    // 过滤条件（9.5）
    {
        let tab2 = Rc::clone(tab);
        tab.btn_filter.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_filter_dialog(&st, ui, &tab2);
        });
    }
    // 条件条（阶段 48）：加条件 / 改排序 / 清除
    //
    // ⚠️ **抓弱引用**：条件条挂在页签上，回调若抓 `Rc<ResultTab>`，就是
    // `ResultTab → 条件条 → 回调 → ResultTab` 的环，页签永远释放不掉。
    {
        let weak_tab = Rc::downgrade(tab);
        tab.cond_bar.btn_add.connect_clicked(move |_| {
            let Some(tab) = weak_tab.upgrade() else { return };
            add_cond_from_bar(&tab);
        });
    }
    {
        let weak_tab = Rc::downgrade(tab);
        tab.cond_bar.order_col.connect_changed(move |cb| {
            let Some(tab) = weak_tab.upgrade() else { return };
            let col = cb.active_id().map(|s| s.to_string()).unwrap_or_default();
            // ⚠️ **不要一选就查**：填列的那一瞬间也会触发 `changed`，
            // 那时用户还没挑排序，白查一次。真正生效的是「当前选中项
            // 跟已生效的排序列不一样」的时候。
            if col == *tab.order_col.borrow() {
                return;
            }
            set_order(&tab, &col, tab.order_asc.get());
        });
    }
    {
        let weak_tab = Rc::downgrade(tab);
        tab.cond_bar.btn_dir.connect_clicked(move |_| {
            let Some(tab) = weak_tab.upgrade() else { return };
            let col = tab.order_col.borrow().clone();
            if col.is_empty() {
                return;
            }
            set_order(&tab, &col, !tab.order_asc.get());
        });
    }
    {
        let weak_tab = Rc::downgrade(tab);
        tab.cond_bar.btn_clear.connect_clicked(move |_| {
            let Some(tab) = weak_tab.upgrade() else { return };
            clear_conds(&tab);
        });
    }
    // 下一页
    {
        let tab2 = Rc::clone(tab);
        tab.btn_next.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_next(&st, ui, &tab2);
            }
        });
    }
    // 首页 / 上一页（阶段 45）：跳页 = 重新查（SQL 里带 LIMIT/OFFSET）
    {
        let tab2 = Rc::clone(tab);
        tab.btn_first.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            // ⚠️ 借用要落成**具名变量**：写成 `st.db_ui.borrow().as_ref()` 的话
            // 临时值在语句结束就没了（E0716）。
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            goto_page(&st, ui, &tab2, 1);
        });
    }
    {
        let tab2 = Rc::clone(tab);
        tab.btn_prev.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let p = tab2.page.get().saturating_sub(1).max(1);
            goto_page(&st, ui, &tab2, p);
        });
    }
    // 每页行数（阶段 45）：改了重新查（回到第一页）
    {
        let tab2 = Rc::clone(tab);
        tab.btn_size.connect_changed(move |cb| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let id = cb.active_id().unwrap_or_default();
            let n: usize = id.parse().unwrap_or(PAGE_SIZE);
            if n == tab2.page_size.get() {
                return;
            }
            tab2.page_size.set(n);
            goto_page(&st, ui, &tab2, 1);
        });
    }
    // 导出（阶段 21）：菜单里选格式
    {
        let tab2 = Rc::clone(tab);
        // ⚠️ **别再手动 set_parent**：`MenuButton::set_popover()` 自己就会把
        // popover 挂到按钮上（gtk_popover_set_parent），再设一次会报
        // "gtk_widget_set_parent: assertion 'parent == NULL' failed"。
        // 所以这里用 `menu::popover_detached()`（不挂父的同款弹层）。
        let pop = crate::menu::popover_detached();
        let vb = crate::menu::body();
        // 每种格式一项：CSV / INSERT SQL / JSON / XML / HTML
        for (icon, label, fmt) in [
            ("text-x-generic-symbolic", tr("导出为 CSV…"), ExportFormat::Csv),
            ("document-send-symbolic", tr("导出为 INSERT SQL 脚本…"), ExportFormat::Sql),
            ("format-justify-fill-symbolic", tr("导出为 JSON…"), ExportFormat::Json),
            ("text-html-symbolic", tr("导出为 XML…"), ExportFormat::Xml),
            (
                "x-office-spreadsheet-symbolic",
                tr("导出为 HTML 表格…"),
                ExportFormat::Html,
            ),
        ] {
            let b = crate::menu::item(icon, label, None);
            let tab3 = Rc::clone(&tab2);
            let pop2 = pop.clone();
            b.connect_clicked(move |_| {
                do_export(&tab3, fmt);
                pop2.popdown();
            });
            vb.append(&b);
        }
        pop.set_child(Some(&vb));
        tab2.btn_export.set_popover(Some(&pop));
    }
    // 编辑 / 保存 / 放弃
    {
        let tab2 = Rc::clone(tab);
        tab.btn_edit.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_edit(&st, ui, &tab2);
            }
        });
    }
    {
        let tab2 = Rc::clone(tab);
        tab.btn_save.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_save(&st, ui, &tab2);
            }
        });
    }
    {
        let tab2 = Rc::clone(tab);
        tab.btn_discard.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_discard(&st, ui, &tab2);
            }
        });
    }
    // 插入行 / 删除行（9.4）
    {
        let tab2 = Rc::clone(tab);
        tab.btn_insert.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_insert_row(&st, ui, &tab2);
            }
        });
    }
    {
        let tab2 = Rc::clone(tab);
        tab.btn_delete.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                do_delete_rows(&st, ui, &tab2);
            }
        });
    }
}

/* ---------------- 表文档（阶段 72e / 审计 P1-7） ---------------- */

/// 表文档内层页签的序号。
const DOC_PAGE_DATA: i32 = 0;
const DOC_PAGE_STRUCT: i32 = 1;
const DOC_PAGE_PROPS: i32 = 2;
const DOC_PAGE_ER: i32 = 3;

/// 一个**表文档**（阶段 72e / 审计 P1-7）。
///
/// 审计 P1-7 的原话："表有五个查看入口：查看数据、表结构、查看定义、对象属性、
/// ER 图；它们的边界对普通用户不清晰" ⇒ 现在打开一张表就是**一个**文档页，
/// 页内四个视图：**数据 / 结构 / 属性 / ER**。
///
/// **为什么非数据视图是懒建的**：ER 图要查**整个模式**（几百张表），结构要查
/// 四份元数据 —— 一开表就全查一遍是白花钱，用户还等着。切到那一页时才真建。
///
/// **「定义」（函数 / 存储过程 / 触发器的 DDL）没有并进来**：那些是**另一个
/// 对象**（不是这张表的侧面），它们仍是独立页。
struct TableDoc {
    id: u64,
    st: Weak<PanelState>,
    schema: String,
    table: String,
    is_view: bool,
    /// 外层那一页（notebook 里的）
    page: gtk::Box,
    /// 内层页签（数据 / 结构 / 属性 / ER）
    inner: gtk::Notebook,
    /// 「数据」视图对应的结果页签
    data: Rc<ResultTab>,
    /// 三个懒视图的占位容器（内层页序号 → 容器）
    slots: RefCell<HashMap<i32, gtk::Box>>,
    /// 「属性」视图自己那张网格（**不共享**——共享的那张是给独立属性窗口用的）
    props_grid: RefCell<Option<gtk::Grid>>,
    /// 「结构」视图的结构页签（关文档时要把它从 `ui.struct_tabs` 里摘掉）
    struct_tab: RefCell<Option<Rc<StructTab>>>,
    /// 「ER」视图的 ER 页签（同上）
    er_tab: RefCell<Option<Rc<ErTab>>>,
    /// 已经建出来的视图（内层页序号）
    built: RefCell<HashSet<i32>>,
}

impl TableDoc {
    /// 切到某个视图时**按需**把那个视图建出来（阶段 72e）。
    ///
    /// ⚠️ **要 `ui` 进来，不要自己借 `st.db_ui`**：调用方（对象树的右键
    /// 回调）**已经借着了**，这里再借就是双重借用 panic。
    fn ensure_view(&self, st: &Rc<PanelState>, ui: &DbUi, page: i32) {
        if self.built.borrow().contains(&page) {
            return;
        }
        // ⚠️ **借用要在这里结束**：`slots` 的借用不能跨到下面去（那里要
        // `borrow_mut` 别的 RefCell）。
        let slot = {
            let slots = self.slots.borrow();
            slots.get(&page).cloned()
        };
        let Some(slot) = slot else { return };
        self.built.borrow_mut().insert(page);
        match page {
            DOC_PAGE_STRUCT => {
                let tab = build_struct_tab(st, ui, &self.schema, &self.table);
                *self.struct_tab.borrow_mut() = Some(Rc::clone(&tab));
                // 结构页签里的「字段 / 索引 / 外键 / 触发器 / DDL」分段控件
                // 已经在 `build_struct_tab` 里摆好了。
                slot.append(&tab.box_);
                // 四块元数据一起要；谁先回来先填谁，凑齐了才拼 DDL
                request_struct(st, ui, &tab, StructPart::Fields);
                request_struct(st, ui, &tab, StructPart::Indexes);
                request_struct(st, ui, &tab, StructPart::ForeignKeys);
                request_struct(st, ui, &tab, StructPart::Triggers);
            }
            DOC_PAGE_PROPS => {
                let grid = build_stats_grid();
                let sc = gtk::ScrolledWindow::new();
                sc.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
                sc.set_vexpand(true);
                sc.set_child(Some(&grid));
                *self.props_grid.borrow_mut() = Some(grid);
                slot.append(&sc);
                request_object_stats(st, ui, &self.schema, &self.table);
            }
            DOC_PAGE_ER => {
                let tab = build_er_tab(st, ui, &self.schema, Some(&self.table));
                *self.er_tab.borrow_mut() = Some(Rc::clone(&tab));
                slot.append(&tab.page);
                // 要图的数据（整张图一次查完，见 worker 的 DbEr）
                let id = next_id(ui);
                ui.pending
                    .borrow_mut()
                    .insert(id, PendingReq::Er { tab_id: tab.id });
                send_cmd(
                    st,
                    ui,
                    WorkerCmd::DbEr {
                        id,
                        schema: self.schema.clone(),
                        table: Some(self.table.clone()),
                    },
                );
            }
            _ => {}
        }
    }
}

/// 找这张表的文档（没有就 `None`）。
fn find_doc(ui: &DbUi, schema: &str, table: &str) -> Option<Rc<TableDoc>> {
    ui.docs
        .borrow()
        .iter()
        .find(|d| d.schema == schema && d.table == table)
        .cloned()
}

/// 找这张表的文档；**没有就建一个**（并发起取列元信息的请求）。
fn find_or_create_doc(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    table: &str,
    is_view: bool,
) -> Rc<TableDoc> {
    if let Some(d) = find_doc(ui, schema, table) {
        return d;
    }
    let doc = new_table_doc(st, ui, schema, table, is_view);
    // 先取列元信息（类型 + 主键），拿到后再查数据
    // ⚠️ pending 必须用 send_meta 返回的那个 id，否则回包对不上（见 send_meta 注释）
    let id = send_meta(
        st,
        ui,
        DbMetaReq::Columns {
            schema: schema.to_string(),
            table: table.to_string(),
        },
    );
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::MetaForTable { tab_id: doc.data.id });
    doc
}

/// 新建一个表文档（外层页 + 内层四个视图）。
fn new_table_doc(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    table: &str,
    is_view: bool,
) -> Rc<TableDoc> {
    let title = format!("{}.{}", schema, table);
    let inner = gtk::Notebook::new();
    inner.set_vexpand(true);

    // ---- 「数据」视图 ----
    let data = build_result_tab(st, ui, &title);
    *data.target.borrow_mut() = Some(TableTarget {
        schema: schema.to_string(),
        table: table.to_string(),
        is_view,
    });
    // 列显示设置的持久化目标（阶段 20）：连接 + 模式 + 表。
    let ck = conn_key(ui);
    if !ck.is_empty() {
        *data.col_key.borrow_mut() = Some((ck, schema.to_string(), table.to_string()));
    }
    update_mode_chip(&data);
    inner.append_page(&data.box_, Some(&gtk::Label::new(Some(tr("数据")))));

    // ---- 其余三个视图：先放占位，切过去时才真建 ----
    let mut slots: HashMap<i32, gtk::Box> = HashMap::new();
    for (idx, name) in [
        (DOC_PAGE_STRUCT, tr("结构")),
        (DOC_PAGE_PROPS, tr("属性")),
        (DOC_PAGE_ER, "ER"),
    ] {
        let ph = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner.append_page(&ph, Some(&gtk::Label::new(Some(name))));
        slots.insert(idx, ph);
    }

    // ---- 外层页 ----
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&inner);

    let doc = Rc::new(TableDoc {
        id: next_id(ui),
        st: Rc::downgrade(st),
        schema: schema.to_string(),
        table: table.to_string(),
        is_view,
        page: page.clone(),
        inner: inner.clone(),
        data: Rc::clone(&data),
        slots: RefCell::new(slots),
        props_grid: RefCell::new(None),
        struct_tab: RefCell::new(None),
        er_tab: RefCell::new(None),
        built: RefCell::new(HashSet::new()),
    });

    // ---- 切页 ⇒ 懒建视图 ----
    //
    // ⚠️ **推到下一轮主循环再建**：switch-page 信号是在 GTK 的信号上下文里
    // 发的，这时候上层可能正借着 `db_ui` ⇒ 这里直接建视图就是双重借用
    // panic（AGENTS.md 记过这个坑）。
    {
        let weak = Rc::downgrade(&doc);
        // ⚠️ GTK4 的 `switch-page` 信号有**三个**参数
        // (notebook, page_widget, page_num)——页码是**第三个**。
        inner.connect_switch_page(move |_nb, _page, page_idx| {
            let Some(doc) = weak.upgrade() else { return };
            let weak2 = Rc::downgrade(&doc);
            let page_idx = page_idx as i32;
            glib::idle_add_local_once(move || {
                let Some(doc) = weak2.upgrade() else { return };
                let Some(st) = doc.st.upgrade() else { return };
                let slot = st.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                doc.ensure_view(&st, ui, page_idx);
            });
        });
    }

    // ---- 页签头：表名 + 关闭按钮 ----
    let label = gtk::Label::new(Some(&title));
    let close = gtk::Button::new();
    close.add_css_class("flat");
    let img = gtk::Image::from_icon_name("window-close-symbolic");
    img.set_pixel_size(12);
    close.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&close);
    close.set_tooltip_text(Some(tr("关闭这个表文档")));
    let tab_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_head.append(&label);
    tab_head.append(&close);
    {
        // ⚠️ **抓弱引用**：回调抓 `Rc<TableDoc>` 就是 `TableDoc → 回调 →
        // TableDoc` 的环，文档页永远释放不掉。
        let weak = Rc::downgrade(&doc);
        close.connect_clicked(move |_| {
            let Some(doc) = weak.upgrade() else { return };
            let Some(st) = doc.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if let Some(pos) = ui.notebook.page_num(&doc.page) {
                ui.notebook.remove_page(Some(pos));
            }
            ui.docs.borrow_mut().retain(|d| !Rc::ptr_eq(d, &doc));
            ui.tabs
                .borrow_mut()
                .retain(|t| !Rc::ptr_eq(t, &doc.data));
            // ⚠️ **结构页 / ER 页也得摘掉**：它们的控件已经跟着文档页一起被
            // 销毁了，还留在表里 ⇒ 以后按 id 找它会拿到一个**死控件**。
            if let Some(t) = doc.struct_tab.borrow().clone() {
                ui.struct_tabs.borrow_mut().retain(|x| !Rc::ptr_eq(x, &t));
            }
            if let Some(t) = doc.er_tab.borrow().clone() {
                ui.er_tabs.borrow_mut().retain(|x| !Rc::ptr_eq(x, &t));
            }
            if ui.notebook.n_pages() == 0 {
                ui.stack.set_visible_child_name("empty");
            }
        });
    }
    ui.notebook.append_page(&page, Some(&tab_head));
    ui.notebook.set_current_page(Some(ui.notebook.n_pages() - 1));
    ui.stack.set_visible_child_name("pages");
    ui.docs.borrow_mut().push(Rc::clone(&doc));
    doc
}

/// 把表文档切到某个视图（外层切到这一页 + 内层切到那个视图）。
///
/// **视图是懒建的**：切过去之后下一轮主循环才建（见 `ensure_view`）。
fn focus_doc(st: Rc<PanelState>, ui: &DbUi, doc: &Rc<TableDoc>, page: i32) {
    if let Some(pos) = ui.notebook.page_num(&doc.page) {
        ui.notebook.set_current_page(Some(pos));
    }
    // ⚠️ **已经在那页上就不会再发 switch-page** ⇒ 懒建的视图不会建出来
    // ⇒ 这里补一刀（`ensure_view` 里有"建过了就跳过"的守卫）。
    if doc.inner.current_page() == Some(page as u32) {
        doc.ensure_view(&st, ui, page);
    } else {
        doc.inner.set_current_page(Some(page as u32));
    }
}

/* ---------------- 表结构页签（9.1） ---------------- */

/// 打开表结构：字段 / 索引 / 外键 + DDL。
///
/// 入口是对象树右键「表结构」——双击表还是看数据（和 Navicat 一样：
/// 设计器在右键菜单里）。
///
/// 阶段 72e：不再**另开一个平级页**，而是切到那张表的**表文档**里的
/// 「结构」视图（审计 P1-7："表有五个查看入口……边界对普通用户不清晰"）。
fn open_structure(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let doc = find_or_create_doc(st, ui, schema, table, false);
    focus_doc(Rc::clone(st), ui, &doc, DOC_PAGE_STRUCT);
}

/// 发一块结构元数据的请求。
fn request_struct(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>, part: StructPart) {
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::Struct { tab_id: tab.id, part });
    let req = match part {
        StructPart::Fields => DbMetaReq::Columns {
            schema: tab.schema.clone(),
            table: tab.table.clone(),
        },
        StructPart::Indexes => DbMetaReq::Indexes {
            schema: tab.schema.clone(),
            table: tab.table.clone(),
        },
        StructPart::ForeignKeys => DbMetaReq::ForeignKeys {
            schema: tab.schema.clone(),
            table: tab.table.clone(),
        },
        // ⚠️ **触发器是按模式查的**（`information_schema.triggers` 没有
        // "只看某张表"的过滤条件），回来以后**按表名过滤**。
        StructPart::Triggers => DbMetaReq::Triggers {
            schema: tab.schema.clone(),
        },
    };
    send_cmd(st, ui, WorkerCmd::DbMeta { id, req });
}

/// 新建表结构页签（**挂到外层 notebook**）。
///
/// **为什么拆出 [`build_struct_tab`]**（阶段 72e）：表文档里的「结构」视图
/// 也是结构页签，但它挂在**文档页的内层页签**里 ⇒ 挂载的事交给调用方。
fn new_struct_tab(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    table: &str,
) -> Rc<StructTab> {
    let tab = build_struct_tab(st, ui, schema, table);
    mount_struct_tab(ui, &tab);
    tab
}

/// 把表结构页签挂到**外层** notebook。
fn mount_struct_tab(ui: &DbUi, tab: &Rc<StructTab>) {
    let table = tab.table.clone();
    // 页签标题：表名 + 关闭按钮
    let title = trf!("{table}（结构）", table = table);
    let label = gtk::Label::new(Some(&title));
    let close = gtk::Button::new();
    close.add_css_class("flat");
    let img = gtk::Image::from_icon_name("window-close-symbolic");
    img.set_pixel_size(12);
    close.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&close);
    close.set_tooltip_text(Some(tr("关闭这个页签")));
    let tab_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_head.append(&label);
    tab_head.append(&close);
    ui.notebook.append_page(&tab.box_, Some(&tab_head));
    ui.notebook.set_current_page(Some(ui.notebook.n_pages() - 1));
    ui.stack.set_visible_child_name("pages");

    // 关闭按钮：把这个页签从 notebook 里摘掉
    {
        let tab2 = Rc::clone(&tab);
        close.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if let Some(pos) = ui.notebook.page_num(&tab2.box_) {
                ui.notebook.remove_page(Some(pos));
            }
            ui.struct_tabs
                .borrow_mut()
                .retain(|t| !Rc::ptr_eq(t, &tab2));
            if ui.notebook.n_pages() == 0 {
                ui.stack.set_visible_child_name("empty");
            }
        });
    }
}

/// 建表结构页签（**不挂到任何容器**）。
fn build_struct_tab(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    table: &str,
) -> Rc<StructTab> {
    let fields = dbgrid::DbGrid::new();
    let indexes = dbgrid::DbGrid::new();
    let fks = dbgrid::DbGrid::new();
    // 触发器表（阶段 49）：**只读**——触发器的 DDL 两家语法差太远，
    // 这一阶段只做到"看得见这张表上挂了哪些触发器"。
    let triggers = dbgrid::DbGrid::new();
    triggers.set_sortable(false);
    // **字段表是可编辑的**（阶段 14：可视化改结构），而且**不能排序**——
    // 行在表里的先后是有含义的（生成 ALTER 时按原顺序），用户一排就乱了。
    fields.set_sortable(false);
    fields.set_editable(true);
    // 索引表同理（阶段 15：可视化改索引）
    indexes.set_sortable(false);
    indexes.set_editable(true);
    // 外键表也改成可编辑（阶段 16：可视化改外键）
    fks.set_sortable(false);
    fks.set_editable(true);

    let ddl = gtk::TextView::new();
    ddl.set_monospace(true);
    ddl.set_wrap_mode(gtk::WrapMode::None);
    ddl.set_top_margin(6);
    ddl.set_bottom_margin(6);
    ddl.set_left_margin(6);
    ddl.set_right_margin(6);
    let ddl_hl = SqlHl::new(&ddl.buffer());
    {
        let hl2 = Rc::clone(&ddl_hl);
        ddl.buffer().connect_changed(move |_| {
            hl2.schedule();
        });
        ddl_hl.apply();
    }
    let ddl_sw = gtk::ScrolledWindow::new();
    ddl_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    ddl_sw.set_vexpand(true);
    ddl_sw.set_child(Some(&ddl));
    ddl_sw.add_css_class("frame");

    // 内层导航：**分段控件**（阶段 49，对标 dbx 的 `字段 | 索引 | 外键 | 触发器`）
    //
    // ⚠️ **为什么不用普通页签**：这几块是**同一张表的几个侧面**，不是几个
    // 文档。页签的层级感会让人以为"换到了别的地方"，dbx 用的就是一行
    // 按钮（分段控件）。
    let inner = gtk::Stack::new();
    inner.set_vexpand(true);

    // ---- 字段页的工具栏（加/删字段、保存、放弃）----
    //
    // **为什么工具栏在字段页里、不放底部**：底部已经有「执行 DDL」了，两个
    // 挨着的"执行"按钮用户会点错——「执行 DDL」跑的是 DDL 页里的 SQL，
    // 「保存修改」跑的是**这里字段表**的改动。
    // ⚠️ **纯图标按钮**（用户 2026-10-28：主界面尽量少用文字）：
    // 这一排原本是「+ 加字段 / − 删字段 / 保存修改 / 放弃修改」四个文字按钮。
    let btn_add_field = icon_button("list-add-symbolic", tr("加字段：在字段表末尾加一行"));
    // ⚠️ **主操作要突出**（阶段 44）：dbx 的「新增字段」是实心主按钮，
    // 一眼就知道这是这一页的主要动作。我们所有按钮权重一样，用户不知道
    // 该点哪个。
    btn_add_field.add_css_class("pp-primary");
    let btn_del_field = icon_button("list-remove-symbolic", tr("删字段：删掉字段表里选中的那一行"));
    let btn_save_struct = icon_button("document-save-symbolic", tr("保存修改：把字段表的改动写进库"));
    let btn_discard_struct = icon_button("edit-undo-symbolic", tr("放弃修改：丢掉字段表里没保存的改动"));
    let struct_tip = gtk::Label::new(Some(tr("改完点工具栏的保存图标才会动库")));
    struct_tip.add_css_class("dim-label");
    struct_tip.set_xalign(0.0);
    struct_tip.set_hexpand(true);
    // ---- 类型下拉（阶段 50）----
    //
    // **为什么要有**：类型是自由文本，用户得自己记得每个库的类型名
    // （PG 是 `integer` / `jsonb`，MySQL 是 `int` / `json`）——写错了要等到
    // 点「应用变更」被服务器骂才知道。这里给一份**按方言**的常用类型清单。
    let type_combo = gtk::ComboBoxText::new();
    for t in dbmeta::common_types(current_proto(ui)) {
        type_combo.append(Some(t), t);
    }
    type_combo.set_active_id(Some(dbmeta::common_types(current_proto(ui))[0]));
    type_combo.set_tooltip_text(Some(tr("常用类型：选中一个字段行，再点「应用」就把它的类型设成这个")));
    type_combo.set_size_request(150, -1);
    let btn_type_apply = gtk::Button::with_label(tr("应用"));
    btn_type_apply.set_tooltip_text(Some(tr("把选中的那个字段的类型设成下拉里挑的类型")));
    let field_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    field_bar.append(&btn_add_field);
    field_bar.append(&btn_del_field);
    field_bar.append(&btn_save_struct);
    field_bar.append(&btn_discard_struct);
    field_bar.append(&type_combo);
    field_bar.append(&btn_type_apply);
    field_bar.append(&struct_tip);

    // ---- 字段改动的实时预览（阶段 50）----
    //
    // dbx 的设计器底部一直挂着一块 SQL 预览区，改一个字段就实时刷新，
    // 旁边还有个「✓ 可执行」的标记。我们原先只有「DDL」那一段（手写
    // SQL 用），改字段时看不到"会发出什么 ALTER"——只能改完点保存，
    // 被服务器骂了才知道写错。
    let preview = gtk::TextView::new();
    preview.set_monospace(true);
    preview.set_editable(false);
    preview.set_wrap_mode(gtk::WrapMode::Word);
    preview.set_top_margin(4);
    preview.set_bottom_margin(4);
    preview.set_left_margin(6);
    preview.set_right_margin(6);
    let preview_mark = gtk::Label::new(Some("—"));
    preview_mark.add_css_class("dim-label");
    preview_mark.set_xalign(0.0);
    preview_mark.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let preview_sw = gtk::ScrolledWindow::new();
    preview_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    // ⚠️ **限高**：不限的话它会把上面的字段表挤没（预览只是配角）。
    preview_sw.set_size_request(-1, 80);
    preview_sw.set_child(Some(&preview));
    preview_sw.add_css_class("frame");
    let preview_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let preview_title = gtk::Label::new(Some(tr("字段改动预览")));
    preview_title.add_css_class("dim-label");
    preview_bar.append(&preview_title);
    preview_bar.append(&preview_mark);
    let preview_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    preview_box.append(&preview_bar);
    preview_box.append(&preview_sw);


    // ---- 索引页的工具栏（阶段 15）----
    let btn_add_index = icon_button("list-add-symbolic", tr("加索引：在索引表末尾加一行"));
    let btn_del_index = icon_button("list-remove-symbolic", tr("删索引：删掉索引表里选中的那一行"));
    let index_tip = gtk::Label::new(Some(tr("列名用逗号分开（如 id, name）；改完点保存图标")));
    index_tip.add_css_class("dim-label");
    index_tip.set_xalign(0.0);
    index_tip.set_hexpand(true);
    let index_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    index_bar.append(&btn_add_index);
    index_bar.append(&btn_del_index);
    index_bar.append(&index_tip);

    // ---- 外键页的工具栏（阶段 16）----
    let btn_add_fk = icon_button("list-add-symbolic", tr("加外键：在外键表末尾加一行"));
    let btn_del_fk = icon_button("list-remove-symbolic", tr("删外键：删掉外键表里选中的那一行"));
    let fk_tip = gtk::Label::new(Some(tr("引用表和引用列要跟被引用的表对得上")));
    fk_tip.add_css_class("dim-label");
    fk_tip.set_xalign(0.0);
    fk_tip.set_hexpand(true);
    let fk_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    fk_bar.append(&btn_add_fk);
    fk_bar.append(&btn_del_fk);
    fk_bar.append(&fk_tip);

    for (title, grid) in [(tr("字段"), &fields), (tr("索引"), &indexes), (tr("外键"), &fks)] {
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);
        sw.set_child(Some(&grid.view));
        sw.add_css_class("frame");
        let bar = match title {
            "字段" => Some(&field_bar),
            "索引" => Some(&index_bar),
            "外键" => Some(&fk_bar),
            _ => None,
        };
        match bar {
            // 这一页 = 工具栏 + 表格
            Some(b) => {
                let page = gtk::Box::new(gtk::Orientation::Vertical, 4);
                page.set_vexpand(true);
                page.append(b);
                page.append(&sw);
                // ⚠️ **字段页下面常驻 DDL 预览**（阶段 50，对标 dbx 底部那个
                // 一直显示的 SQL 预览区）：改一个字段就能立刻看到会发出什么
                // ALTER TABLE，以及**这条语句能不能执行**。
                if title == tr("字段") {
                    // ⚠️ **表格要有最小高度**：预览区是固定高度的，它一占
                    // 地方，表格就被挤成只剩一两行（看着像"表里只有一行"）。
                    sw.set_size_request(-1, 200);
                    page.append(&preview_box);
                }
                inner.add_titled(&page, Some(title), title);
            }
            None => {
                inner.add_titled(&sw, Some(title), title);
            }
        }
    }
    // ---- 触发器页（阶段 49）----
    //
    // **只读**：触发器的 DDL 两家语法差得远（PG 的 `CREATE TRIGGER … FOR EACH
    // ROW EXECUTE FUNCTION …` vs MySQL 的 `CREATE TRIGGER … FOR EACH ROW
    // BEGIN … END`），要"可编辑"得另起一套语句生成器（阶段 49 的计划里
    // 写的是"可编辑"，本阶段先做到**看得见**：知道这张表上有哪些触发器、
    // 什么时机、什么事件）。
    let trig_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let trig_tip = gtk::Label::new(Some(tr("这张表上的触发器（只读；要改触发器请到对象树里双击它看定义）")));
    trig_tip.add_css_class("dim-label");
    trig_tip.set_xalign(0.0);
    trig_tip.set_hexpand(true);
    trig_bar.append(&trig_tip);
    {
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);
        sw.set_child(Some(&triggers.view));
        sw.add_css_class("frame");
        let page = gtk::Box::new(gtk::Orientation::Vertical, 4);
        page.set_vexpand(true);
        page.append(&trig_bar);
        page.append(&sw);
        inner.add_titled(&page, Some(tr("触发器")), tr("触发器"));
    }
    inner.add_titled(&ddl_sw, Some("DDL"), "DDL");

    // ---- 分段控件本体（阶段 49）----
    //
    // ⚠️ **为什么不用 `GtkStackSwitcher`**：它内部那排按钮是 **homogeneous**
    // 的（每个按钮平分整行宽度）——实测 5 个按钮摊满 640px，看着像一排
    // 导航链接，不像"分段控件"；而且当前段没有底色高亮（dbx 的分段控件
    // 当前段是有底的）。自己画一排 `ToggleButton` 更可控。
    let seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg.add_css_class("pp-segmented");
    seg.set_halign(gtk::Align::Start);
    let seg_buttons: Rc<Vec<gtk::ToggleButton>> = {
        let mut v: Vec<gtk::ToggleButton> = Vec::new();
        for name in SEGMENT_NAMES {
            // ⚠️ `SEGMENT_NAMES` 是 **const 表**（编译期求值，不能调 `tr()`）
            //    ⇒ 显示的时候在这儿翻（英文界面显示 "Fields" / "Indexes"…）。
            //    ⚠️ 而下面 `stack.set_visible_child_name(&n)` 用的是**中文原名**
            //    —— 那是 stack 的**内部页名**，两边都用中文才对得上。
            let b = gtk::ToggleButton::with_label(crate::i18n::tr_dyn(name).as_ref());
            b.add_css_class("pp-seg-btn");
            if let Some(first) = v.first() {
                // 同组 = 互斥（点一个别的自动弹起来）
                b.set_group(Some(first));
            } else {
                b.set_active(true);
            }
            v.push(b.clone());
            seg.append(&b);
        }
        Rc::new(v)
    };
    // 点哪一段就切到哪一段
    for (i, name) in SEGMENT_NAMES.iter().enumerate() {
        let b = seg_buttons[i].clone();
        let n = name.to_string();
        let stack = inner.clone();
        b.connect_toggled(move |btn| {
            if btn.is_active() {
                stack.set_visible_child_name(&n);
            }
        });
    }
    // 程序切段（测试钩子 / 以后的别的入口）→ 按钮跟着变
    {
        let btns = Rc::clone(&seg_buttons);
        inner.connect_visible_child_name_notify(move |st| {
            let cur = st.visible_child_name().map(|s| s.to_string()).unwrap_or_default();
            for (i, name) in SEGMENT_NAMES.iter().enumerate() {
                let want = *name == cur;
                // ⚠️ **先比再设**：set_active 会发 `toggled` → 又去设
                // visible_child_name → 再 notify → 不比的话会绕成死循环。
                if btns[i].is_active() != want {
                    btns[i].set_active(want);
                }
            }
        });
    }

    let btn_exec = icon_button(
        "media-playback-start-symbolic",
        tr("执行 DDL：在当前连接上执行上面的 DDL（一条语句）。PG 走事务，MySQL 的 DDL 会自动提交。"),
    );
    let status = gtk::Label::new(Some(tr("正在读表结构…")));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_hexpand(true);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    foot.append(&btn_exec);
    foot.append(&status);
    // ⚠️ **「应用变更」主按钮**（阶段 44，对标 dbx 右下角那颗黑色实心按钮）：
    // dbx 把"把结构改动写进库"这个动作放在**右下角常驻**，不藏在工具栏里——
    // 改结构是这一页的主要动作，不该跟"加字段/删字段"挤在一排图标里。
    // 它做的是跟工具栏「保存修改」同一件事（`do_save_structure`）。
    let btn_apply = gtk::Button::with_label(tr("应用变更"));
    btn_apply.add_css_class("pp-primary");
    btn_apply.set_tooltip_text(Some(tr("把字段 / 索引 / 外键的改动写进库")));
    let apply_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    apply_spacer.set_hexpand(true);
    foot.append(&apply_spacer);
    foot.append(&btn_apply);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    box_.set_vexpand(true);
    // 分段控件在**内容上面**（dbx 就是这个顺序：一排按钮，下面是内容）
    box_.append(&seg);
    box_.append(&inner);
    box_.append(&foot);

    let tab = Rc::new(StructTab {
        id: next_id(ui),
        st: Rc::downgrade(st),
        win: ui.win.clone(),
        schema: schema.to_string(),
        table: table.to_string(),
        fields,
        indexes,
        fks,
        triggers,
        ddl,
        ddl_hl,
        preview,
        preview_mark,
        type_combo,
        inner,
        seg_buttons: Rc::clone(&seg_buttons),
        btn_exec,
        btn_apply,
        status,
        box_,
        cols: RefCell::new(None),
        idx: RefCell::new(None),
        fk: RefCell::new(None),
        sql_queue: RefCell::new(VecDeque::new()),
        in_tx: Cell::new(false),
    });

    // 执行 DDL
    {
        let tab2 = Rc::clone(&tab);
        tab.btn_exec.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_exec_ddl(&st, ui, &tab2);
        });
    }
    // 阶段 14：字段表的可视化改结构
    {
        let tab2 = Rc::clone(&tab);
        btn_add_field.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_add_field(ui, &tab2);
        });
    }
    {
        let tab2 = Rc::clone(&tab);
        btn_del_field.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_del_field(ui, &tab2);
        });
    }
    // 类型下拉「应用」：把挑的类型写到**选中行**的类型列（阶段 50）
    {
        let tab2 = Rc::clone(&tab);
        btn_type_apply.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let Some(ty) = tab2.type_combo.active_id().map(|s| s.to_string()) else {
                set_status(ui, tr("先挑一个类型"), true);
                return;
            };
            let sel = tab2.fields.selected_display_rows();
            let Some(&row) = sel.first() else {
                set_status(ui, tr("先选中一个字段行，再点「应用」"), true);
                return;
            };
            // ⚠️ **第 1 列是「类型」**（0=字段 / 1=类型 / 2=可空 / 3=主键 /
            // 4=默认值 / 5=操作）
            tab2.fields.set_edit(row, 1, &ty);
            let msg = trf!("第 {v0} 行的类型改成 {ty}", ty = ty, v0 = row + 1);
            tab2.status.set_text(&msg);
            set_status(ui, &msg, false);
        });
    }
    // 字段表一改 → 实时预览跟着刷新（阶段 50）
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.fields.set_change_hook(Rc::new(move || {
            let Some(tab) = weak_tab.upgrade() else { return };
            let Some(st) = tab.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            refresh_field_preview(ui, &tab);
        }));
    }
    // 字段表 / 索引表 / 外键表：**关掉结果网格那一套右键菜单**（阶段 50）
    //
    // ⚠️ 那套菜单是按结果网格的语义做的（设为 NULL / 克隆为新行 / 转置…），
    // 出现在字段表上根本不对——"把字段名设为 NULL"听着就危险。
    tab.fields.set_row_menu_enabled(false);
    tab.indexes.set_row_menu_enabled(false);
    tab.fks.set_row_menu_enabled(false);
    // 点「操作」那一格 → 弹行内操作菜单（阶段 50）
    {
        let weak_tab = Rc::downgrade(&tab);
        tab.fields.set_cell_click_hook(Rc::new(move |row, col, x, y| {
            let Some(tab) = weak_tab.upgrade() else { return };
            // ⚠️ **第 5 列是「操作」**（0=字段 / 1=类型 / 2=可空 / 3=主键 /
            // 4=默认值 / 5=操作）
            if col != 5 {
                return;
            }
            show_field_ops_menu(&tab, row, x, y);
        }));
    }
    {
        let tab2 = Rc::clone(&tab);
        btn_save_struct.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_save_structure(&st, ui, &tab2);
        });
    }
    {
        // 右下角「应用变更」：跟工具栏的「保存修改」是同一件事
        let tab2 = Rc::clone(&tab);
        tab.btn_apply.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_save_structure(&st, ui, &tab2);
        });
    }
    {
        let tab2 = Rc::clone(&tab);
        btn_discard_struct.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_discard_structure(&st, ui, &tab2);
        });
    }
    // 阶段 15：索引表的可视化改索引
    {
        let tab2 = Rc::clone(&tab);
        btn_add_index.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_add_index(ui, &tab2);
        });
    }
    {
        let tab2 = Rc::clone(&tab);
        btn_del_index.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_del_index(ui, &tab2);
        });
    }
    // 阶段 16：外键表的可视化改外键
    {
        let tab2 = Rc::clone(&tab);
        btn_add_fk.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_add_fk(ui, &tab2);
        });
    }
    {
        let tab2 = Rc::clone(&tab);
        btn_del_fk.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_del_fk(ui, &tab2);
        });
    }

    ui.struct_tabs.borrow_mut().push(Rc::clone(&tab));
    tab
}

/// 表结构的三块内容到齐 → 拼 DDL 填进文本区。
fn refresh_struct_ddl(ui: &DbUi, tab: &Rc<StructTab>) {
    let (Some(cols), Some(idx), Some(fk)) = (
        tab.cols.borrow().clone(),
        tab.idx.borrow().clone(),
        tab.fk.borrow().clone(),
    ) else {
        return;
    };
    let proto = current_proto(ui);
    let ddl = dbmeta::build_ddl(proto, &tab.schema, &tab.table, &cols, &fk);
    tab.ddl.buffer().set_text(&ddl);
    // 索引单独列在「索引」页；DDL 里不重复写（PG 的索引语句各家差异大，
    // 硬拼容易拼错，写错了比不写更糟）
    let _ = idx;
    tab.status.set_text(&trf!("{v0} 个字段、{v1} 个索引、{v2} 个外键", v0 = cols.len(), v1 = idx.len(), v2 = fk.len()));
}

/// 字段表的**行内操作菜单**（阶段 50）：点「操作」那一格弹出来。
///
/// 条目：
/// - **复制这一行**：把这一行的内容复制成一行待插入的新字段；
/// - **删除这一行**：删掉这一行（**只是标记**，点「保存修改」才真删）。
///
/// ⚠️ **没做上移 / 下移**：网格的行序是由 `rebuild_rows` 按数据顺序重建的，
/// 要支持"把新加的列插到指定位置"得给网格加一层**显式行序**（改动面大，
/// 而且结果网格共用同一套代码，风险高）——本阶段先不做。
fn show_field_ops_menu(tab: &Rc<StructTab>, row: usize, x: f64, y: f64) {
    let body = crate::menu::body();
    {
        let tab2 = Rc::clone(tab);
        let b = crate::menu::item("edit-copy-symbolic", tr("复制这一行"), None);
        b.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if tab2.fields.clone_row(row) {
                let msg = trf!("已复制第 {v0} 行（改完点保存修改才生效）", v0 = row + 1);
                tab2.status.set_text(&msg);
                set_status(ui, &msg, false);
            }
        });
        body.append(&b);
    }
    {
        let tab2 = Rc::clone(tab);
        let b = crate::menu::item("list-remove-symbolic", tr("删除这一行"), None);
        b.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if tab2.fields.delete_display_row(row) {
                let msg = trf!("已标记删除第 {v0} 行（点保存修改才真的删）", v0 = row + 1);
                tab2.status.set_text(&msg);
                set_status(ui, &msg, false);
            }
        });
        body.append(&b);
    }
    // 弹在鼠标位置（相对字段表的 TreeView）
    let pop = crate::menu::popover(&tab.fields.view);
    pop.set_child(Some(&body));
    let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
    pop.set_pointing_to(Some(&rect));
    pop.popup();
}

/// 刷新字段改动的**实时预览**（阶段 50）。
///
/// 每次字段表有改动就重算一遍 `ALTER TABLE`，并标出这条语句**能不能执行**：
/// - `✓ 可执行`：语句生成得出来；
/// - `⚠ 有风险`：语句没问题，但会丢数据（删列 / 改类型）——点保存时要确认；
/// - `✗ 不能执行`：字段还没填完（名字空、类型空、重名…），**当场就能看出来**，
///   不用等点保存被服务器骂。
fn refresh_field_preview(ui: &DbUi, tab: &Rc<StructTab>) {
    let Some(cols) = tab.cols.borrow().clone() else {
        return;
    };
    let proto = current_proto(ui);
    let target = collect_field_specs(tab);
    let buf = tab.preview.buffer();
    let (text, mark, err) = match dbmeta::build_alter_table(
        proto,
        &tab.schema,
        &tab.table,
        &cols,
        &target,
    ) {
        Ok(None) => (
            tr("（字段没有改动）").to_string(),
            tr("— 无改动").to_string(),
            false,
        ),
        Ok(Some(plan)) => {
            let mark = if plan.risks.is_empty() {
                tr("✓ 可执行").to_string()
            } else {
                trf!("⚠ 有风险：{v0}", v0 = plan.risks.join(tr("；")))
            };
            (plan.sql.clone(), mark, false)
        }
        Err(e) => (
            trf!("还不能生成语句：{e}", e = e),
            trf!("✗ 不能执行：{e}", e = e),
            true,
        ),
    };
    buf.set_text(&text);
    tab.preview_mark.set_text(&mark);
    tab.preview_mark.remove_css_class("error");
    if err {
        tab.preview_mark.add_css_class("error");
    }
}

/// 执行 DDL 文本区里的语句。
fn do_exec_ddl(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() || ui.busy.get() {
        return;
    }
    let buf = tab.ddl.buffer();
    let sql = buf.text(&buf.start_iter(), &buf.end_iter(), true).trim().to_string();
    if sql.is_empty() {
        set_status(ui, tr("DDL 是空的，没什么可执行"), true);
        return;
    }
    ui.busy.set(true);
    update_buttons(ui);
    set_status(ui, tr("正在执行 DDL…"), false);
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::Ddl { tab_id: tab.id });
    send_cmd(st, ui, WorkerCmd::DbExec { id, sql });
}

/* ---------------- 表结构改结构（阶段 14） ---------------- */

/// 加一个字段（**只是加一行空的**，填完点「保存修改」才动库）。
fn do_add_field(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    tab.fields.insert_row();
    let msg = tr("已加一行，填上字段名和类型，点「保存修改」才生效");
    tab.status.set_text(msg);
    set_status(ui, msg, false);
}

/// 删一个字段（**只是标记**，真正删在点「保存修改」时）。
///
/// 为什么不当场就问"确定删吗"：这里删的还只是网格里的一行，点「放弃修改」
/// 就回来了。等真要执行 ALTER（`DROP COLUMN`）时再确认，那才是不可逆的一刻。
fn do_del_field(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    let Some(row) = tab.fields.selected_display_rows().first().copied() else {
        set_status(ui, tr("先选中要删的字段"), true);
        return;
    };
    if tab.fields.delete_display_row(row) {
        let msg = tr("已标记删除这个字段，点「保存修改」才真的删（列里的数据会一起没了）");
        tab.status.set_text(msg);
        set_status(ui, msg, false);
    }
}

/// 保存字段表 / 索引表的改动。
///
/// **有风险就先问一句**：删列、改类型、加唯一索引这类操作可能丢数据或失败，
/// 确认框里把要做的语句和风险都摆出来，用户看得懂自己在干什么。
fn do_save_structure(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>) {
    save_structure(st, ui, tab, false)
}

/// 真正的保存逻辑。
///
/// `skip_confirm`：测试钩子用（跳过二次确认，免得测试卡在模态框上）。
/// 界面上永远走 `do_save_structure`（要确认）。
fn save_structure(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>, skip_confirm: bool) {
    if !ui.connected.get() || ui.busy.get() {
        return;
    }
    let Some(cols) = tab.cols.borrow().clone() else {
        set_status(ui, tr("表结构还没读出来，稍等一下再保存"), true);
        return;
    };
    let proto = current_proto(ui);
    let mut sqls: Vec<String> = Vec::new();
    let mut risks: Vec<String> = Vec::new();

    // ---- 字段改动 → 一条 ALTER TABLE ----
    let target = collect_field_specs(tab);
    // 新加的字段必须把名字和类型填了——空着的行发过去数据库只会报错
    for t in &target {
        if t.orig_index.is_none() {
            if t.name.trim().is_empty() {
                set_status(ui, tr("新加的字段还没填名字（不要了就点「放弃修改」）"), true);
                return;
            }
            if t.ty.trim().is_empty() {
                set_status(ui, &trf!("新加的字段「{v0}」还没填类型", v0 = t.name), true);
                return;
            }
        }
    }
    match dbmeta::build_alter_table(proto, &tab.schema, &tab.table, &cols, &target) {
        Ok(None) => {}
        Ok(Some(plan)) => {
            sqls.push(plan.sql);
            risks.extend(plan.risks);
        }
        Err(e) => {
            let msg = trf!("改动有问题，未执行：{e}", e = e);
            tab.status.set_text(&msg);
            set_status(ui, &msg, true);
            return;
        }
    }

    // ---- 索引改动 → 若干条（PG 是多条独立语句）----
    let idx_orig = tab.idx.borrow().clone().unwrap_or_default();
    let idx_target = collect_index_specs(tab);
    for t in &idx_target {
        if t.orig_name.is_none() {
            if t.name.trim().is_empty() {
                set_status(ui, tr("新加的索引还没填名字（不要了就点「放弃修改」）"), true);
                return;
            }
            if t.columns.is_empty() {
                set_status(ui, &trf!("新加的索引「{v0}」还没填列", v0 = t.name), true);
                return;
            }
        }
    }
    match dbmeta::build_index_sqls(proto, &tab.schema, &tab.table, &idx_orig, &idx_target) {
        Ok((v, r)) => {
            sqls.extend(v);
            risks.extend(r);
        }
        Err(e) => {
            let msg = trf!("改动有问题，未执行：{e}", e = e);
            tab.status.set_text(&msg);
            set_status(ui, &msg, true);
            return;
        }
    }

    // ---- 外键改动 → 若干条 ----
    let fk_orig = tab.fk.borrow().clone().unwrap_or_default();
    let fk_target = collect_fk_specs(tab);
    for t in &fk_target {
        if t.orig_name.is_none() {
            if t.name.trim().is_empty() {
                set_status(ui, tr("新加的外键还没填约束名（不要了就点「放弃修改」）"), true);
                return;
            }
            if t.column.trim().is_empty()
                || t.ref_table.trim().is_empty()
                || t.ref_column.trim().is_empty()
            {
                set_status(ui, &trf!("新加的外键「{v0}」还没填全（列 / 引用表 / 引用列）", v0 = t.name), true);
                return;
            }
        }
    }
    match dbmeta::build_fk_sqls(proto, &tab.schema, &tab.table, &fk_orig, &fk_target) {
        Ok((v, r)) => {
            sqls.extend(v);
            risks.extend(r);
        }
        Err(e) => {
            let msg = trf!("改动有问题，未执行：{e}", e = e);
            tab.status.set_text(&msg);
            set_status(ui, &msg, true);
            return;
        }
    }

    if sqls.is_empty() {
        tab.status.set_text(tr("没有要保存的改动"));
        set_status(ui, tr("没有要保存的改动"), true);
        return;
    }

    if risks.is_empty() || skip_confirm {
        exec_sql_queue(st, ui, tab, sqls);
        return;
    }
    // 有风险 → 二次确认
    let st2 = Rc::clone(st);
    let tab2 = Rc::clone(tab);
    let msg = trf!("要执行这些改动：\n\n{v0}\n\n⚠️ {v1}\n\n确定要改吗？", v0 = sqls.join(";\n"), v1 = risks.join("\n⚠️ "));
    crate::panel::dialogs::confirm_with(Some(&ui.win), &msg, tr("改表结构"), move |ok| {
        if !ok {
            return;
        }
        let slot = st2.db_ui.borrow();
        let Some(ui2) = slot.as_ref() else { return };
        exec_sql_queue(&st2, ui2, &tab2, sqls);
    });
}

/// 放弃改动：重新读一遍结构，本地改动就没了。
fn do_discard_structure(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>) {
    request_struct(st, ui, tab, StructPart::Fields);
    request_struct(st, ui, tab, StructPart::Indexes);
    request_struct(st, ui, tab, StructPart::ForeignKeys);
    let msg = tr("已放弃本地改动");
    tab.status.set_text(msg);
    set_status(ui, msg, false);
}

/// 执行一批语句（一条一条发，回包到了再发下一条）。
///
/// **PG 包在事务里**：PG 支持事务性 DDL，索引改动又是多条独立语句（`DROP
/// INDEX` / `CREATE INDEX`），不包事务的话中途失败会留下"旧索引删了、新索引
/// 没建"的半截状态。MySQL 的 DDL 不支持事务回滚——它那条 `ALTER TABLE` 本身
/// 是原子的（8.0 的原子 DDL），但多条之间失败仍会留下半截状态，这一点在
/// 状态栏里说明。
fn exec_sql_queue(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>, sqls: Vec<String>) {
    let proto = current_proto(ui);
    let mut q: VecDeque<String> = sqls.into();
    // PG：事务包一层
    let in_tx = proto == DbProto::Postgres;
    if in_tx {
        q.push_front("BEGIN".to_string());
        q.push_back("COMMIT".to_string());
    }
    tab.in_tx.set(in_tx);
    *tab.sql_queue.borrow_mut() = q;
    ui.busy.set(true);
    update_buttons(ui);
    let msg = tr("正在改表结构…");
    tab.status.set_text(msg);
    set_status(ui, msg, false);
    exec_next_sql(st, ui, tab);
}

/// 发队列里的下一条语句。
fn exec_next_sql(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<StructTab>) {
    let Some(sql) = tab.sql_queue.borrow_mut().pop_front() else {
        return;
    };
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::StructSave { tab_id: tab.id });
    send_cmd(st, ui, WorkerCmd::DbExec { id, sql });
}

/// 加一个索引（**只是加一行空的**，填完点「保存修改」才动库）。
fn do_add_index(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    tab.indexes.insert_row();
    let msg = tr("已加一行，填上索引名和列（列名用逗号分开），点「保存修改」才生效");
    tab.status.set_text(msg);
    set_status(ui, msg, false);
}

/// 删一个索引（**只是标记**，真正删在点「保存修改」时）。
fn do_del_index(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    let Some(row) = tab.indexes.selected_display_rows().first().copied() else {
        set_status(ui, tr("先选中要删的索引"), true);
        return;
    };
    if tab.indexes.delete_display_row(row) {
        let msg = tr("已标记删除这个索引，点「保存修改」才真的删（数据不丢，查询会变慢）");
        tab.status.set_text(msg);
        set_status(ui, msg, false);
    }
}

/// 加一个外键（**只是加一行空的**，填完点「保存修改」才动库）。
fn do_add_fk(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    tab.fks.insert_row();
    let msg = tr("已加一行，填上约束名、本表列、引用表、引用列，点「保存修改」才生效");
    tab.status.set_text(msg);
    set_status(ui, msg, false);
}

/// 删一个外键（**只是标记**，真正删在点「保存修改」时）。
fn do_del_fk(ui: &DbUi, tab: &Rc<StructTab>) {
    if !ui.connected.get() {
        return;
    }
    let Some(row) = tab.fks.selected_display_rows().first().copied() else {
        set_status(ui, tr("先选中要删的外键"), true);
        return;
    };
    if tab.fks.delete_display_row(row) {
        let msg = tr("已标记删除这个外键，点「保存修改」才真的删（数据不丢，引用关系没了）");
        tab.status.set_text(msg);
        set_status(ui, msg, false);
    }
}

/// 外键表当前显示的内容 → 设计器的目标外键。
fn collect_fk_specs(tab: &Rc<StructTab>) -> Vec<dbmeta::FkSpec> {
    let orig_list = tab.fk.borrow().clone().unwrap_or_default();
    let vals = tab.fks.visible_values();
    let mut out = Vec::new();
    for (disp, row) in vals.iter().enumerate() {
        let get = |i: usize| row.get(i).cloned().unwrap_or_default().trim().to_string();
        let orig_name = tab
            .fks
            .source_row(disp)
            .and_then(|i| orig_list.get(i).map(|d| d.name.clone()));
        out.push(dbmeta::FkSpec {
            orig_name,
            name: get(0),
            column: get(1),
            ref_table: get(2),
            ref_column: get(3),
        });
    }
    out
}

/// 字段表当前显示的内容 → 设计器的目标列定义。
///
/// **靠显示行号对回原来的第几列**（`source_row`）：用户把列名改了以后就
/// 按名字对不上了，行号不会变。被标记删除的行不在网格里，所以也不会出现。
fn collect_field_specs(tab: &Rc<StructTab>) -> Vec<dbmeta::ColumnSpec> {
    let vals = tab.fields.visible_values();
    let mut out = Vec::new();
    for (disp, row) in vals.iter().enumerate() {
        let get = |i: usize| row.get(i).cloned().unwrap_or_default().trim().to_string();
        let def = get(4);
        out.push(dbmeta::ColumnSpec {
            orig_index: tab.fields.source_row(disp),
            name: get(0),
            ty: get(1),
            // 「可空」列：写「否」才是不可空；认不出来一律当可空（保守）
            nullable: !is_no(&get(2)),
            // 「主键」列：只认明确的「是」
            is_pk: is_yes(&get(3)),
            default_value: if def.is_empty() { None } else { Some(def) },
        });
    }
    out
}

/// 索引表当前显示的内容 → 设计器的目标索引。
///
/// **靠显示行号对回原来的索引**（`source_row` + `tab.idx`）：用户把索引名
/// 改了以后就按名字对不上了，行号不会变。
fn collect_index_specs(tab: &Rc<StructTab>) -> Vec<dbmeta::IndexSpec> {
    let orig_list = tab.idx.borrow().clone().unwrap_or_default();
    let vals = tab.indexes.visible_values();
    let mut out = Vec::new();
    for (disp, row) in vals.iter().enumerate() {
        let get = |i: usize| row.get(i).cloned().unwrap_or_default().trim().to_string();
        let cols = dbmeta::parse_index_columns(&get(1));
        let orig_name = tab
            .indexes
            .source_row(disp)
            .and_then(|i| orig_list.get(i).map(|d| d.name.clone()));
        out.push(dbmeta::IndexSpec {
            orig_name,
            name: get(0),
            unique: is_yes(&get(2)),
            columns: cols,
        });
    }
    out
}

/// 「是 / 否」这两列的解析：认 Y/N/y/n/yes/no/true/false/1/0。
///
/// 认不出来的一律按保守的方向取（可空 → 可空，主键 → 不是主键）——
/// 猜错了就把用户的表改坏了，宁可当没设。
fn is_yes(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "是" | "y" | "yes" | "true" | "1"
    )
}

fn is_no(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "否" | "n" | "no" | "false" | "0"
    )
}

/* ---------------- 定义页签（9.3） ---------------- */

/// 打开函数 / 存储过程的定义页签。
fn open_routine_def(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    name: &str,
    args: &str,
    is_proc: bool,
) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let kind = if is_proc {
        dbmeta::DefKind::Procedure
    } else {
        dbmeta::DefKind::Function
    };
    // 标题带身份参数：PG 里同名重载（`f(int)` / `f(text)`）要能分辨
    let title = if args.is_empty() {
        name.to_string()
    } else {
        format!("{name}({args})")
    };
    let tab = new_def_tab(st, ui, &title, kind, schema, "", name);
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Def { tab_id: tab.id });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::RoutineDef {
                schema: schema.to_string(),
                name: name.to_string(),
                args: args.to_string(),
                is_proc,
            },
        },
    );
}

/// 发起一次全库对象搜索。
///
/// **不是过滤树上的已加载节点**——那种过滤只能筛已经展开过的那几层，
/// 用户真正要的是"这个库里有没有一张叫 xxx 的表"。
fn do_search(st: &Rc<PanelState>, ui: &DbUi) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let kw = ui.search_entry.text().trim().to_string();
    if kw.is_empty() {
        set_status(ui, tr("先写个关键词再搜"), true);
        return;
    }
    set_status(ui, &trf!("正在搜索「{kw}」…", kw = kw), false);
    let id = send_meta(
        st,
        ui,
        DbMetaReq::Search { keyword: kw },
    );
    ui.pending.borrow_mut().insert(id, PendingReq::Search);
}

/// 弹出（或刷新）搜索结果窗口。
///
/// **结果窗口而不是把结果塞进对象树**：树上的节点是"这个库有什么"，
/// 塞进搜索结果会把"库里没有、只是搜到了"的东西混进去，用户分不清。
fn show_search_results(st: &Rc<PanelState>, ui: &DbUi, hits: Vec<dbmeta::SearchHit>) {
    // 窗口开着就复用（换一批结果），没开就新建。
    //
    // ⚠️ **不能一边 borrow() 一边 borrow_mut()**：`match ui.search_win.borrow()`
    // 的临时值会活到整个 match 结束，在 None 分支里再 `borrow_mut()` 就是
    // RefCell 双重借用，直接 panic（第一次搜完窗口就没存下来，第二次搜索的
    // 结果刷不进去）。所以先把 clone 出来，借用就结束了。
    //
    // 窗口要先建好：底部的状态文字是建窗口时才有（`search_status`）。
    let existing = ui.search_win.borrow().clone();
    let win = match existing {
        Some(w) => w,
        None => {
            let w = build_search_window(st, ui);
            *ui.search_win.borrow_mut() = Some(w.clone());
            w
        }
    };

    // 网格的行顺序 = 源行号，命中列表按同样的顺序存一份
    let rows: Vec<Vec<Option<String>>> = hits
        .iter()
        .map(|h| {
            vec![
                Some(h.kind.label().to_string()),
                Some(h.schema.clone()),
                Some(h.name.clone()),
                Some(h.owner.clone()),
                Some(h.extra.clone()),
            ]
        })
        .collect();
    ui.search_grid.set_data(dbgrid::GridData {
        columns: vec![
            tr("类型").into(),
            tr("模式").into(),
            tr("名称").into(),
            tr("所属表").into(),
            tr("说明").into(),
        ],
        types: vec![dbmeta::ColType::Text; 5],
        rows,
        pk: vec![],
    });
    *ui.search_hits.borrow_mut() = hits;
    // 结果窗口底部的状态文字：找到几个、要不要紧（命中数可能被上限截断）
    let n = ui.search_grid.row_count();
    if let Some(lbl) = ui.search_status.borrow().as_ref() {
        let text = if n >= dbmeta::search_limit() {
            trf!("找到 {n} 个对象（已达上限，可能还有更多）", n = n)
        } else {
            trf!("找到 {n} 个对象", n = n)
        };
        lbl.set_text(&text);
    }
    win.present();
}

/// 打开对象属性窗口（Navicat 的 Details 面板）。
///
/// 窗口开着就复用（换对象时重填），关掉了再开就新建一个。
fn open_object_stats(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    // 阶段 72e：切到那张表的**表文档**里的「属性」视图（不再弹独立窗口）。
    let doc = find_or_create_doc(st, ui, schema, table, false);
    focus_doc(Rc::clone(st), ui, &doc, DOC_PAGE_PROPS);
}

/// 发对象属性的请求（独立窗口 / 表文档的「属性」视图共用）。
fn request_object_stats(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str) {
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::ObjectStats { schema: schema.to_string(), table: table.to_string() });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::ObjectStats { schema: schema.to_string(), table: table.to_string() },
        },
    );
}

/// 属性视图（属性 | 值 两列 + 滚动条）。
///
/// **为什么拆出来**（阶段 72e）：表文档里的「属性」视图要的是**内容**，不是
/// 一整个窗口 ⇒ 窗口那层留在 [`build_stats_window`]。
fn build_stats_view(ui: &DbUi) -> gtk::ScrolledWindow {
    let grid = build_stats_grid();
    let sc = gtk::ScrolledWindow::new();
    sc.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sc.set_vexpand(true);
    sc.set_child(Some(&grid));
    *ui.stats_grid.borrow_mut() = Some(grid);
    sc
}

/// 属性网格（属性 | 值 两列，第 0 行是表头）。
///
/// **为什么单独一个函数**（阶段 72e）：表文档里的「属性」视图要**自己**一张
/// 网格（不能跟独立属性窗口共享——一个 widget 不能进两个容器）。
fn build_stats_grid() -> gtk::Grid {
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    grid.set_hexpand(true);
    // 表头（占第 0 行）：Navicat 的 Details 面板是「属性 | 值」两列且带表头。
    // 没有表头的话用户只看到一列名词一列值，分不清哪列是名字、哪列是值。
    // 表头和数据**在同一个 Grid** 里，列宽才天然对齐。
    {
        let h1 = gtk::Label::new(Some(tr("属性")));
        h1.set_xalign(0.0);
        h1.add_css_class("dim-label");
        h1.add_css_class("heading");
        grid.attach(&h1, 0, 0, 1, 1);
        let h2 = gtk::Label::new(Some(tr("值")));
        h2.set_xalign(0.0);
        h2.add_css_class("dim-label");
        h2.add_css_class("heading");
        grid.attach(&h2, 1, 0, 1, 1);
    }
    grid
}

/// 属性窗口（属性 | 值 两列）。
fn build_stats_window(st: &Rc<PanelState>, ui: &DbUi) -> gtk::Window {
    let win = gtk::Window::builder()
        .title(tr("对象属性"))
        .transient_for(&ui.win)
        .default_width(420)
        .default_height(420)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    root.append(&build_stats_view(ui));

    let btn_close = gtk::Button::with_label(tr("关闭"));
    wire_tool_close(&win, &btn_close);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::End);
    row.append(&btn_close);
    root.append(&row);

    win.set_child(Some(&root));
    let _ = st;
    win
}

/// 清掉属性网格里的**数据行**（第 1 行往下）。
///
/// ⚠️ **第 0 行是表头**（「属性 | 值」），不能删——删了用户就分不清哪列是名字、
/// 哪列是值了。数据行是按行号连续摆的，所以从第 1 行往上找空位即可。
fn clear_stats_rows(g: &gtk::Grid) {
    let mut row = 1i32;
    while let Some(c) = g.child_at(0, row) {
        g.remove(&c);
        if let Some(c2) = g.child_at(1, row) {
            g.remove(&c2);
        }
        row += 1;
    }
}

/// 把属性填进窗口（回包到了）。
fn fill_object_stats(ui: &DbUi, stats: &dbmeta::ObjectStats) {
    let Some(g) = ui.stats_grid.borrow().clone() else { return };
    fill_object_stats_into(&g, stats);
}

/// 把属性填进**指定**的网格（阶段 72e：表文档里的「属性」视图有自己的一张）。
fn fill_object_stats_into(g: &gtk::Grid, stats: &dbmeta::ObjectStats) {
    clear_stats_rows(g);
    // 数据从第 1 行开始（第 0 行是表头）
    let mut row = 1i32;
    for (k, v) in stats.rows() {
        let kl = gtk::Label::new(Some(&k));
        kl.set_xalign(0.0);
        kl.add_css_class("dim-label");
        g.attach(&kl, 0, row, 1, 1);
        let vl = gtk::Label::new(Some(&v));
        vl.set_xalign(0.0);
        vl.set_selectable(true);
        vl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        g.attach(&vl, 1, row, 1, 1);
        row += 1;
    }
}

/// 结果网格的**空结果态**构件（阶段 83 / 审计 P2-10）。
///
/// 返回 `(容器, 标题, 解释, 主 CTA 按钮)`。
///
/// ⚠️ **不用 `emptystate::EmptyState`**：那个构件强制"必须有一个主 CTA"，
/// 而这里恰恰分两种：
/// - **只读**（普通查询结果）：没有任何可做的动作 ⇒ 硬塞一个按钮是噪声；
/// - **可编辑**（表浏览）：有真动作（插入一行）⇒ 给一颗**真按钮**，而不是
///   写一句"点工具栏的插入行"（审计 §4.1 说的「说明书式提示」）。
fn build_grid_empty_state() -> (gtk::Box, gtk::Label, gtk::Label, gtk::Button) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
    box_.set_halign(gtk::Align::Center);
    box_.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(Some(tr("还没有结果")));
    title.add_css_class("pp-empty-title");
    box_.append(&title);

    let desc = gtk::Label::new(Some(tr("执行一条 SQL，结果会显示在这里。")));
    desc.add_css_class("pp-empty-sub");
    desc.set_xalign(0.5);
    desc.set_wrap(true);
    // ⚠️ 必须限宽 + 开 wrap：不限宽的话这一行文字的自然宽能把整个容器撑破。
    desc.set_max_width_chars(40);
    box_.append(&desc);

    let cta = gtk::Button::new();
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    h.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    h.append(&gtk::Label::new(Some(tr("插入一行"))));
    cta.set_child(Some(&h));
    cta.add_css_class("pp-empty-btn");
    cta.add_css_class("suggested-action");
    // 默认藏着：只读结果没有可做的动作
    cta.set_visible(false);
    box_.append(&cta);

    (box_, title, desc, cta)
}

/// 空结果态说什么（阶段 83 / 审计 P2-10）。
///
/// 抽成**纯函数**是为了能单测：界面上的分支很容易走错（把"还没查"说成
/// "没有数据"），而这种错**不会报错**，只是用户看着别扭。
///
/// 返回 `(标题, 解释, 有没有主 CTA)`。
fn grid_empty_text(has_columns: bool, has_target: bool) -> (&'static str, &'static str, bool) {
    if !has_columns {
        // 还没跑过查询：新建的查询页签、刚打开的表文档都是这个状态
        (tr("还没有结果"), tr("执行一条 SQL，结果会显示在这里。"), false)
    } else if has_target {
        // 表浏览页签：有真动作 ⇒ 给真按钮
        (tr("这张表还没有数据"), tr("表里一行都没有。"), true)
    } else {
        (tr("没有结果行"), tr("查询执行成功，但没有返回任何行。"), false)
    }
}

/// 网格 ↔ 空结果态来回切（阶段 83 / 审计 P2-10）。
///
/// 三种情形，三种说法：
/// - **有行** ⇒ 显示网格；
/// - **一列都没有** ⇒ 还没跑过查询（新建的查询页签就是这样）；
/// - **有列但零行** ⇒ 查完了，确实没有数据。
fn update_grid_empty_state(tab: &Rc<ResultTab>) {
    if tab.grid.row_count() > 0 {
        tab.grid_stack.set_visible_child_name("grid");
        return;
    }
    tab.grid_stack.set_visible_child_name("empty");
    let (title, desc, cta) = grid_empty_text(
        !tab.grid.columns().is_empty(),
        tab.target.borrow().is_some(),
    );
    tab.empty_title.set_text(title);
    tab.empty_desc.set_text(desc);
    tab.empty_cta.set_visible(cta);
}

/// 列显示设置（11.3：Navicat 的「字段显示设置」）。
///
/// 左边列表是**所有列**（藏起来的列也在里面，勾回来就行），顺序按当前显示
/// 顺序排；右边勾选显不显示，上移 / 下移调顺序。
///
/// **只影响显示**：待保存的改动、导出的 SQL 都仍按**全部列**算——藏一列不该
/// 「只读 / 可编辑」标志跟着目标变（阶段 72c）。
///
/// - **普通查询结果**（没有 target）⇒ 只读；
/// - **表浏览页签**（target 是表不是视图）⇒ 可编辑；
/// - **视图**⇒ 只读（视图本来就不能改）。
fn update_mode_chip(tab: &Rc<ResultTab>) {
    let editable = tab
        .target
        .borrow()
        .as_ref()
        .map(|t| !t.is_view)
        .unwrap_or(false);
    if editable {
        tab.mode_chip.set_text(tr("可编辑"));
        // ⚠️ **只换状态类，别摘基础类**：`.pp-chip` 身上带着底色 / 圆角 /
        // 内边距，摘了就变成"一串白字"（看着跟没标志一样）。
        tab.mode_chip.add_css_class("pp-chip-ok");
        tab.mode_chip
            .set_tooltip_text(Some(tr("这一页是表浏览页签：可以进入编辑模式改数据")));
    } else {
        tab.mode_chip.set_text(tr("只读"));
        tab.mode_chip.remove_css_class("pp-chip-ok");
        tab.mode_chip.set_tooltip_text(Some(
            tr("这一页是只读的：普通查询结果不能改（表浏览页签才可编辑）"),
        ));
    }
    // 目标变了（查询结果 ↔ 表浏览）⇒ 空结果态的说法也跟着变
    update_grid_empty_state(tab);
}

/// 让那一列的数据从 UPDATE 里消失。
fn open_column_dialog(tab: &Rc<ResultTab>) {
    let columns = tab.grid.columns();
    let ncols = columns.len();
    if ncols == 0 {
        return;
    }
    // 状态：`(原始列下标, 是否显示)`，**按当前顺序**（用户排过的顺序）
    //
    // ⚠️ **别把藏起来的列挪到末尾**：那样用户点一次「全显示」，列顺序就
    // 变了（藏过的列跑到最后去）。grid 侧存的是完整顺序，这里直接读它。
    let state: Rc<RefCell<Vec<(usize, bool)>>> =
        Rc::new(RefCell::new(tab.grid.column_layout()));
    let sel: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    // 程序化改勾选状态时也会触发 `toggled`，用这个标志挡掉（否则会自己改自己）
    let updating = Rc::new(Cell::new(false));

    let dlg = gtk::Dialog::with_buttons(
        Some(tr("列显示设置")),
        Some(&tab.win),
        gtk::DialogFlags::MODAL,
        &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("确定"), gtk::ResponseType::Ok),
        ],
    );
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    {
        let st = Rc::clone(&state);
        for (orig, _) in st.borrow().iter() {
            let lbl = gtk::Label::new(Some(&columns[*orig]));
            lbl.set_xalign(0.0);
            list.append(&lbl);
        }
    }
    let list_sw = gtk::ScrolledWindow::new();
    list_sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    list_sw.set_size_request(260, 320);
    list_sw.set_child(Some(&list));

    let chk = gtk::CheckButton::with_label(tr("显示这一列"));
    let btn_up = gtk::Button::with_label(tr("上移"));
    let btn_down = gtk::Button::with_label(tr("下移"));
    let btn_all = gtk::Button::with_label(tr("全显示"));
    let btn_none = gtk::Button::with_label(tr("全隐藏"));

    let side = gtk::Box::new(gtk::Orientation::Vertical, 6);
    side.append(&chk);
    side.append(&btn_up);
    side.append(&btn_down);
    side.append(&btn_all);
    side.append(&btn_none);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    body.append(&list_sw);
    body.append(&side);
    root.append(&body);

    // 重建列表（上下移动、全选全不选之后）
    let rebuild = {
        let list = list.clone();
        let state = Rc::clone(&state);
        let columns = columns.clone();
        move |sel_idx: usize| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            for (orig, _) in state.borrow().iter() {
                let lbl = gtk::Label::new(Some(&columns[*orig]));
                lbl.set_xalign(0.0);
                list.append(&lbl);
            }
            // GTK4 的 `select_row` 收的是**行对象**，不是下标
            if let Some(r) = list.row_at_index(sel_idx as i32) {
                list.select_row(Some(&r));
            }
        }
    };

    // 选中某一行 → 勾选框显示它当前的状态
    {
        let state = Rc::clone(&state);
        let sel = Rc::clone(&sel);
        let updating = Rc::clone(&updating);
        let chk = chk.clone();
        list.connect_row_selected(move |_, row| {
            // GTK4 的回调给的是 `Option<&ListBoxRow>`（取消选择时是 None）
            let Some(row) = row else { return; };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let idx = idx as usize;
            sel.set(idx);
            let on = state.borrow().get(idx).map(|(_, v)| *v).unwrap_or(false);
            updating.set(true);
            chk.set_active(on);
            updating.set(false);
        });
    }

    // 勾选框 → 改状态
    {
        let state = Rc::clone(&state);
        let sel = Rc::clone(&sel);
        let updating = Rc::clone(&updating);
        chk.connect_toggled(move |c| {
            if updating.get() {
                return;
            }
            let idx = sel.get();
            let mut s = state.borrow_mut();
            if let Some(item) = s.get_mut(idx) {
                item.1 = c.is_active();
            }
        });
    }

    // 上移 / 下移
    {
        let state = Rc::clone(&state);
        let sel = Rc::clone(&sel);
        let rebuild = rebuild.clone();
        btn_up.connect_clicked(move |_| {
            let idx = sel.get();
            if idx == 0 {
                return;
            }
            {
                let mut s = state.borrow_mut();
                s.swap(idx - 1, idx);
            }
            sel.set(idx - 1);
            rebuild(idx - 1);
        });
    }
    {
        let state = Rc::clone(&state);
        let sel = Rc::clone(&sel);
        let rebuild = rebuild.clone();
        btn_down.connect_clicked(move |_| {
            let idx = sel.get();
            let len = state.borrow().len();
            if idx + 1 >= len {
                return;
            }
            {
                let mut s = state.borrow_mut();
                s.swap(idx, idx + 1);
            }
            sel.set(idx + 1);
            rebuild(idx + 1);
        });
    }
    // 全显示 / 全隐藏
    {
        let state = Rc::clone(&state);
        let rebuild = rebuild.clone();
        let sel = Rc::clone(&sel);
        btn_all.connect_clicked(move |_| {
            for item in state.borrow_mut().iter_mut() {
                item.1 = true;
            }
            let cur = sel.get();
            rebuild(cur);
        });
    }
    {
        let state = Rc::clone(&state);
        let rebuild = rebuild.clone();
        let sel = Rc::clone(&sel);
        btn_none.connect_clicked(move |_| {
            for item in state.borrow_mut().iter_mut() {
                item.1 = false;
            }
            let cur = sel.get();
            rebuild(cur);
        });
    }

    dlg.content_area().append(&root);
    dlg.present();

    // 确定 → 应用到网格
    let grid = Rc::clone(&tab.grid);
    // 持久化目标（阶段 20）：**表浏览页签才有**（普通查询结果的列每次都
    // 不一样，存了没意义）。这里先取出来，闭包里才好用（闭包要 `'static`）。
    let persist: Option<(String, String, String)> = tab.col_key.borrow().clone();
    dlg.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Ok {
            // **整份布局**都应用（顺序 + 显不显示）——藏起来的列也留着位置，
            // 下次打开设置框时顺序还在。
            let layout: Vec<(usize, bool)> = state.borrow().clone();
            grid.set_column_layout(layout.clone());
            // 存进配置（阶段 20）：表浏览页签下次打开还是这个布局。
            if let Some((conn, schema, table)) = &persist {
                // 按**列名**存（下标会变，名字不会）
                let names = grid.columns();
                let specs: Vec<dbcols::ColSpec> = layout
                    .iter()
                    .filter_map(|(i, visible)| names.get(*i).map(|n| (n.clone(), *visible)))
                    .collect();
                dbcols::save(conn, schema, table, &specs);
            }
        }
        d.close();
    });
}

/// 造搜索结果窗口。
fn build_search_window(st: &Rc<PanelState>, ui: &DbUi) -> gtk::Window {
    let win = gtk::Window::builder()
        .title(tr("对象搜索"))
        .transient_for(&ui.win)
        .default_width(760)
        .default_height(420)
        .build();
    // 位置交给窗口管理器（GTK4 的 `gtk_window_set_position()` 已被移除）。
    // 无 WM 的环境下（如 xvfb）它会落在左上角，真机上有 WM 会居中在父窗口上。
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint = gtk::Label::new(Some(
        tr("双击一行直接打开：表/视图看数据，字段打开它所在的表，函数/过程/触发器/事件看定义。"),
    ));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let sc = gtk::ScrolledWindow::new();
    sc.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sc.set_vexpand(true);
    sc.set_child(Some(&ui.search_grid.view));
    root.append(&sc);

    let status = gtk::Label::new(Some(""));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_hexpand(true);

    let btn_close = gtk::Button::with_label(tr("关闭"));
    wire_tool_close(&win, &btn_close);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&status);
    row.append(&btn_close);
    root.append(&row);

    *ui.search_status.borrow_mut() = Some(status.clone());

    win.set_child(Some(&root));

    // 双击 → 打开对应对象
    {
        let st2 = Rc::clone(st);
        let w = win.clone();
        ui.search_grid
            .view
            .connect_row_activated(move |_, _path, _col| {
                let slot = st2.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                let rows = ui.search_grid.selected_display_rows();
                let Some(disp) = rows.first().copied() else { return };
                // 显示行号 → 源行号（列表可能被排序过）
                let Some(src) = ui.search_grid.source_row(disp) else { return };
                let hit = ui.search_hits.borrow().get(src).cloned();
                let Some(hit) = hit else { return };
                open_search_hit(&st2, ui, &hit);
                drop(slot);
                hide_tool_window(&w);
            });
    }
    win
}

/// 打开一条搜索命中。
fn open_search_hit(st: &Rc<PanelState>, ui: &DbUi, hit: &dbmeta::SearchHit) {
    match hit.kind {
        dbmeta::SearchKind::Table => open_table(st, ui, &hit.schema, &hit.name, false),
        dbmeta::SearchKind::View => open_table(st, ui, &hit.schema, &hit.name, true),
        // 字段：打开它所在的表（视图的字段也归到表里，查询语句一样）
        dbmeta::SearchKind::Column => open_table(st, ui, &hit.schema, &hit.owner, false),
        dbmeta::SearchKind::Function => {
            open_routine_def(st, ui, &hit.schema, &hit.name, &hit.extra, false)
        }
        dbmeta::SearchKind::Procedure => {
            open_routine_def(st, ui, &hit.schema, &hit.name, &hit.extra, true)
        }
        // 触发器：定义要连表一起给（PG 的 `pg_get_triggerdef` 认表）
        dbmeta::SearchKind::Trigger => {
            open_trigger_def(st, ui, &hit.schema, &hit.owner, &hit.name)
        }
        dbmeta::SearchKind::Event => open_event_def(st, ui, &hit.schema, &hit.name),
    }
}

/// 打开执行历史窗口（11.2）。
///
/// **历史按会话分开**（`history.ini` 里一个会话一个组）——Navicat 的 History
/// 也是按连接分的，混在一起没法看。
fn open_history_window(st: &Rc<PanelState>, ui: &DbUi) {
    // 窗口开着就复用（换一批内容），没开就新建。
    // ⚠️ 同搜索结果窗口那个坑：`match x.borrow()` 的临时借用会活到 match
    // 结束，在分支里 `borrow_mut()` 就是双重借用 → panic。先 clone 出来。
    let existing = ui.hist_win.borrow().clone();
    let win = match existing {
        Some(w) => w,
        None => {
            let w = build_history_window(st, ui);
            *ui.hist_win.borrow_mut() = Some(w.clone());
            w
        }
    };
    refresh_history(st, ui);
    win.present();
}

/// 把历史填进历史窗口的网格。
fn refresh_history(st: &Rc<PanelState>, ui: &DbUi) {
    let entries = dbhist::list(&ui.hist_key.borrow());
    let rows: Vec<Vec<Option<String>>> = entries
        .iter()
        .map(|e| {
            vec![
                Some(e.time.clone()),
                Some(sql_one_line(&e.sql)),
            ]
        })
        .collect();
    ui.hist_grid.set_data(dbgrid::GridData {
        columns: vec![tr("时间").into(), "SQL".into()],
        types: vec![ColType::Text; 2],
        rows,
        pk: vec![],
    });
    *ui.hist_entries.borrow_mut() = entries;
    let _ = st;
}

/// SQL 压成一行（给列表看；全文另存一份，填回编辑器时用原文）。
fn sql_one_line(sql: &str) -> String {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 160 {
        let t: String = flat.chars().take(160).collect();
        format!("{t}…")
    } else {
        flat
    }
}

/* ---------------- 工具窗的关闭（阶段 98） ---------------- */

/// 把工具窗**藏起来**（不是 `close()`）。
///
/// ⚠️ **GTK 4.18 的 `gtk_window_close()` 只有第一次有效**：第一次发
/// `close-request` 并把窗口藏起来；之后再 `close()` 就只发信号、
/// **窗口不藏** ⇒ 用户看到的现象是「第一次能关，第二次关不掉」。
/// 最小复现：`examples/gtktest.rs`（一个**空窗口**也这样 ⇒ 是 GTK 自身
/// 的行为，不是我们代码的锅）。
/// ⇒ 一律用 `set_visible(false)` 藏窗口。
pub fn hide_tool_window(w: &gtk::Window) {
    w.set_visible(false);
}

/// 给工具窗装上「关闭」：**按钮**和**窗口自带的 X**都能关。
///
/// ⚠️ X 那个按钮走的是 `close-request` ⇒ 必须自己接住并藏起来
/// （返回 [`gtk::glib::Propagation::Stop`]，别让 GTK 的默认处理再跑一遍
/// —— 它第一次会把窗口"销毁"掉，后面就不听使唤了）。
fn wire_tool_close(win: &gtk::Window, btn_close: &gtk::Button) {
    let w = win.clone();
    btn_close.connect_clicked(move |_| {
        hide_tool_window(&w);
    });
    win.connect_close_request(move |w| {
        hide_tool_window(w);
        gtk::glib::Propagation::Stop
    });
}

/// 造历史窗口。
fn build_history_window(st: &Rc<PanelState>, ui: &DbUi) -> gtk::Window {
    let win = gtk::Window::builder()
        .title(tr("执行历史"))
        .transient_for(&ui.win)
        .default_width(760)
        .default_height(420)
        .build();
    // ⚠️ **模态**（用户 2026-11-02：「我认为它应该是个 modal 的对话框，
    // 它开着，别的东西都点不了」）：它是"看历史、挑一条填回编辑器"的
    // 短任务 ⇒ 开着的时候主窗口不该还能点（更不该能直接关掉程序）。
    win.set_modal(true);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint = gtk::Label::new(Some(
        tr("双击一条（或点「填入编辑器」）把它放回 SQL 编辑器，不自动执行——改完再点「执行」。"),
    ));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let sc = gtk::ScrolledWindow::new();
    sc.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sc.set_vexpand(true);
    sc.set_child(Some(&ui.hist_grid.view));
    root.append(&sc);

    let status = gtk::Label::new(Some(""));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_hexpand(true);

    let btn_fill = gtk::Button::with_label(tr("填入编辑器"));
    let btn_clear = gtk::Button::with_label(tr("清空历史"));
    let btn_close = gtk::Button::with_label(tr("关闭"));
    {
        let st2 = Rc::clone(st);
        btn_fill.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                fill_history_into_editor(&st2, ui);
            }
        });
    }
    {
        let st2 = Rc::clone(st);
        btn_clear.connect_clicked(move |_| {
            if let Some(ui) = st2.db_ui.borrow().as_ref() {
                clear_history(&st2, ui);
            }
        });
    }
    wire_tool_close(&win, &btn_close);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&status);
    row.append(&btn_fill);
    row.append(&btn_clear);
    row.append(&btn_close);
    root.append(&row);

    win.set_child(Some(&root));

    // 双击 → 填回编辑器
    {
        let st2 = Rc::clone(st);
        ui.hist_grid
            .view
            .connect_row_activated(move |_, _path, _col| {
                if let Some(ui) = st2.db_ui.borrow().as_ref() {
                    fill_history_into_editor(&st2, ui);
                }
            });
    }
    win
}

/// 把选中的历史条目填回 SQL 编辑器（**不执行**）。
///
/// **为什么不双击就执行**：项目规矩是"写库/改结构必须显式动作"。历史里
/// 可能有半年前跑过的一条 `DELETE`，双击就执行等于埋雷。填回编辑器，用户
/// 看一眼、确认了再点「执行」。
fn fill_history_into_editor(st: &Rc<PanelState>, ui: &DbUi) {
    let Some(sql) = selected_history_sql(ui) else {
        set_status(ui, tr("先选一条历史"), true);
        return;
    };
    ui.sql.buffer().set_text(&sql);
    set_status(ui, tr("已填入编辑器（没有自动执行）"), false);
    let _ = st;
}

/// 取选中的历史条目的 SQL 全文。
fn selected_history_sql(ui: &DbUi) -> Option<String> {
    let rows = ui.hist_grid.selected_display_rows();
    let disp = rows.first().copied()?;
    // 显示行号 → 源行号（列表能排序）
    let src = ui.hist_grid.source_row(disp)?;
    ui.hist_entries.borrow().get(src).map(|e| e.sql.clone())
}

/// 清空当前会话的历史。
fn clear_history(st: &Rc<PanelState>, ui: &DbUi) {
    dbhist::clear(&ui.hist_key.borrow());
    refresh_history(st, ui);
    set_status(ui, tr("历史已清空"), false);
}

/// 打开事件的定义页签。
///
/// **事件只有 MySQL 有**（PostgreSQL 没有定时事件这个功能），所以 schema
/// 一定是库名——查询语句本身按方言拼（`meta_event_def`）。
fn open_event_def(st: &Rc<PanelState>, ui: &DbUi, schema: &str, name: &str) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let tab = new_def_tab(
        st,
        ui,
        name,
        dbmeta::DefKind::Event,
        schema,
        "",
        name,
    );
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Def { tab_id: tab.id });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::EventDef {
                schema: schema.to_string(),
                name: name.to_string(),
            },
        },
    );
}

/// 打开触发器的定义页签。
fn open_trigger_def(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str, name: &str) {
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let tab = new_def_tab(
        st,
        ui,
        name,
        dbmeta::DefKind::Trigger,
        schema,
        table,
        name,
    );
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Def { tab_id: tab.id });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::TriggerDef {
                schema: schema.to_string(),
                table: table.to_string(),
                name: name.to_string(),
            },
        },
    );
}

/// 新建定义页签。
fn new_def_tab(
    st: &Rc<PanelState>,
    ui: &DbUi,
    title: &str,
    kind: dbmeta::DefKind,
    schema: &str,
    table: &str,
    name: &str,
) -> Rc<DefTab> {
    let text = gtk::TextView::new();
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::None);
    text.set_top_margin(6);
    text.set_bottom_margin(6);
    text.set_left_margin(6);
    text.set_right_margin(6);
    let hl = SqlHl::new(&text.buffer());
    {
        let hl2 = Rc::clone(&hl);
        text.buffer().connect_changed(move |_| {
            hl2.schedule();
        });
    }
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_vexpand(true);
    sw.set_child(Some(&text));
    sw.add_css_class("frame");

    let btn_copy = gtk::Button::with_label(tr("复制定义"));
    btn_copy.set_tooltip_text(Some(tr("把定义文本复制到剪贴板")));
    let btn_exec = gtk::Button::with_label(tr("执行定义"));
    btn_exec.set_tooltip_text(Some(
        tr("在当前连接上执行这段定义。触发器与 MySQL 例程会先 DROP 再重建。"),
    ));
    let status = gtk::Label::new(Some(tr("正在读定义…")));
    status.add_css_class("dim-label");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_hexpand(true);
    let foot = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    foot.append(&btn_copy);
    foot.append(&btn_exec);
    foot.append(&status);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    box_.set_vexpand(true);
    box_.append(&sw);
    box_.append(&foot);

    let tab = Rc::new(DefTab {
        id: next_id(ui),
        st: Rc::downgrade(st),
        win: ui.win.clone(),
        title: title.to_string(),
        kind,
        schema: schema.to_string(),
        table: table.to_string(),
        name: name.to_string(),
        text,
        hl,
        btn_copy,
        btn_exec,
        status,
        box_,
    });

    // 页签标题：对象名 + 关闭按钮
    let label = gtk::Label::new(Some(&trf!("{title}（定义）", title = title)));
    let close = gtk::Button::new();
    close.add_css_class("flat");
    let img = gtk::Image::from_icon_name("window-close-symbolic");
    img.set_pixel_size(12);
    close.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&close);
    close.set_tooltip_text(Some(tr("关闭这个页签")));
    let tab_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_head.append(&label);
    tab_head.append(&close);
    ui.notebook.append_page(&tab.box_, Some(&tab_head));
    ui.notebook.set_current_page(Some(ui.notebook.n_pages() - 1));
    ui.stack.set_visible_child_name("pages");

    // 关闭按钮：把这个页签从 notebook 里摘掉
    {
        let tab2 = Rc::clone(&tab);
        close.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if let Some(pos) = ui.notebook.page_num(&tab2.box_) {
                ui.notebook.remove_page(Some(pos));
            }
            ui.def_tabs
                .borrow_mut()
                .retain(|t| !Rc::ptr_eq(t, &tab2));
            if ui.notebook.n_pages() == 0 {
                ui.stack.set_visible_child_name("empty");
            }
        });
    }
    // 复制定义
    {
        let tab2 = Rc::clone(&tab);
        tab.btn_copy.connect_clicked(move |_| {
            let buf = tab2.text.buffer();
            let s = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
            tab2.win.clipboard().set_text(&s);
            tab2.status.set_text(tr("已复制定义"));
        });
    }
    // 执行定义
    {
        let tab2 = Rc::clone(&tab);
        tab.btn_exec.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            do_exec_def(&st, ui, &tab2);
        });
    }

    ui.def_tabs.borrow_mut().push(Rc::clone(&tab));
    tab
}

/// 执行定义页签里的定义。
///
/// **逐条发，不拼多语句**：函数/存储过程的体里全是 `;`，拼成一条多语句
/// 会被服务端按分号切开（PG 的 dollar-quote、MySQL 的 `BEGIN…END` 都一样）。
/// 所以 `prepare_def_exec` 给出的是"完整语句列表"，这里一条一条发。
fn do_exec_def(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<DefTab>) {
    if !ui.connected.get() || ui.busy.get() {
        return;
    }
    let buf = tab.text.buffer();
    let text = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
    if text.trim().is_empty() {
        tab.status.set_text(tr("定义是空的，没什么可执行"));
        return;
    }
    let proto = current_proto(ui);
    let stmts = dbmeta::prepare_def_exec(
        proto,
        tab.kind,
        &tab.schema,
        &tab.table,
        &tab.name,
        &text,
    );
    if stmts.is_empty() {
        tab.status.set_text(tr("定义是空的，没什么可执行"));
        return;
    }
    let total = stmts.len();
    *ui.def_queue.borrow_mut() = stmts.into();
    *ui.def_job.borrow_mut() = Some(DefJob {
        tab_id: tab.id,
        total,
        sent: 0,
    });
    ui.busy.set(true);
    update_buttons(ui);
    tab.status.set_text(&trf!("正在执行定义（{total} 条语句）…", total = total));
    send_next_def(st, ui);
}

/// 发下一条定义语句。
fn send_next_def(st: &Rc<PanelState>, ui: &DbUi) {
    let next = ui.def_queue.borrow_mut().pop_front();
    let Some(sql) = next else {
        // 队列空了：全部成功
        ui.busy.set(false);
        update_buttons(ui);
        let job = ui.def_job.borrow_mut().take();
        let msg = match job {
            Some(j) => trf!("定义已执行（{v0} 条语句）", v0 = j.total),
            None => tr("定义已执行").to_string(),
        };
        if let Some(j) = job {
            if let Some(tab) = find_def_tab(ui, j.tab_id) {
                tab.status.set_text(&msg);
            }
        }
        set_status(ui, &msg, false);
        return;
    };
    let tab_id = ui.def_job.borrow().as_ref().map(|j| j.tab_id).unwrap_or(0);
    if let Some(job) = ui.def_job.borrow_mut().as_mut() {
        job.sent += 1;
    }
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::DefExec { tab_id });
    send_cmd(st, ui, WorkerCmd::DbExec { id, sql });
}

/// 取下一页。
fn do_next(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() || ui.busy.get() || !tab.has_more.get() {
        return;
    }
    ui.busy.set(true);
    update_buttons(ui);
    set_status(ui, tr("加载下一页…"), false);
    // 页码 / 偏移跟着走（阶段 45）：游标往后翻，偏移同步加一页
    let size = tab.page_size.get().max(1);
    tab.offset.set(tab.offset.get() + size);
    tab.page.set(tab.page.get() + 1);
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::Query { tab_id: tab.id });
    send_cmd(st, ui, WorkerCmd::DbNextPage { id, page_size: size });
}

/// 导出格式（阶段 21）。
#[derive(Clone, Copy, PartialEq, Debug)]
enum ExportFormat {
    /// CSV（带 BOM，Excel 友好）
    Csv,
    /// INSERT 语句脚本
    Sql,
    Json,
    Xml,
    Html,
}

impl ExportFormat {
    /// 格式的名字（菜单项、状态栏都用它）。
    fn label(self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Sql => "INSERT SQL",
            ExportFormat::Json => "JSON",
            ExportFormat::Xml => "XML",
            ExportFormat::Html => "HTML",
        }
    }

    /// 文件扩展名。
    fn ext(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Sql => "sql",
            ExportFormat::Json => "json",
            ExportFormat::Xml => "xml",
            ExportFormat::Html => "html",
        }
    }
}

/// 生成导出内容 + 默认文件名 + 导出完的提示语。
///
/// 返回 None = 没东西可导（没有列 / 没有行）。
fn export_payload(tab: &Rc<ResultTab>, fmt: ExportFormat) -> Option<(String, String, String)> {
    let columns = tab.grid.columns();
    if columns.is_empty() {
        return None;
    }
    // 文件名用表名（普通查询页签没有表名 → 退化成 result）
    let base = tab
        .target
        .borrow()
        .as_ref()
        .map(|t| t.table.clone())
        .unwrap_or_else(|| "result".to_string());
    let file_name = format!("{}.{}", base, fmt.ext());
    let rows = tab.grid.visible_values_opt();
    let (text, done) = match fmt {
        ExportFormat::Csv => (
            tab.grid.to_csv(true),
            trf!("已导出 {v0} 行 CSV", v0 = tab.grid.visible_count()),
        ),
        ExportFormat::Sql => {
            let sql = export_sql_text(tab)?;
            // 行数按**实际导出的**算（SQL 导出走原始行，不含未保存的改动）
            let n = tab.grid.original_rows().len();
            (sql, trf!("已导出 {n} 行 INSERT 语句", n = n))
        }
        ExportFormat::Json => (
            dbexport::to_json(&columns, &rows),
            trf!("已导出 {v0} 行 JSON", v0 = rows.len()),
        ),
        ExportFormat::Xml => (
            dbexport::to_xml(&columns, &rows),
            trf!("已导出 {v0} 行 XML", v0 = rows.len()),
        ),
        ExportFormat::Html => (
            dbexport::to_html(&columns, &rows),
            trf!("已导出 {v0} 行 HTML", v0 = rows.len()),
        ),
    };
    Some((text, file_name, done))
}

/// 导出当前结果为某种格式（弹文件保存框）。
fn do_export(tab: &Rc<ResultTab>, fmt: ExportFormat) {
    let Some((text, file_name, done)) = export_payload(tab, fmt) else {
        tab.status.set_text(tr("没有可导出的数据"));
        return;
    };
    let title = trf!("导出为 {v0}", v0 = fmt.label());
    let mut b = gtk::FileChooserNative::builder()
        .title(&title)
        .action(gtk::FileChooserAction::Save)
        .accept_label(tr("导出"))
        .cancel_label(tr("取消"))
        .modal(true);
    b = b.transient_for(&tab.win);
    let ch = b.build();
    ch.set_current_name(&file_name);
    let tab2 = Rc::clone(tab);
    ch.connect_response(move |c, r| {
        if r == gtk::ResponseType::Accept {
            if let Some(f) = c.file() {
                if let Some(p) = f.path() {
                    if let Err(e) = std::fs::write(&p, text.as_bytes()) {
                        tab2.status.set_text(&trf!("导出失败：{e}", e = e));
                    } else {
                        tab2.status.set_text(&done);
                    }
                }
            }
        }
        c.destroy();
    });
    ch.show();
}

/// 生成 INSERT SQL 脚本文本（导出按钮与冒烟测试共用一份）。
///
/// 表名取页签对应的表；普通查询页签没有表名，退化成 `exported_table`。
fn export_sql_text(tab: &Rc<ResultTab>) -> Option<String> {
    let columns = tab.grid.columns();
    let types = tab.types.borrow().clone();
    let rows = tab.grid.original_rows();
    if columns.is_empty() || rows.is_empty() {
        return None;
    }
    let (schema, table) = match tab.target.borrow().clone() {
        Some(t) => (t.schema, t.table),
        None => (String::new(), "exported_table".to_string()),
    };
    // 方言要从当前连接表单拿（页签自己不知道自己是 PG 还是 MySQL）
    let proto = {
        let Some(st) = tab.st.upgrade() else { return None };
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return None };
        current_proto(ui)
    };
    Some(dbmeta::build_inserts(
        proto, &schema, &table, &columns, &types, &rows,
    ))
}

/// 冒烟测试用：某个结果页签的 INSERT SQL 导出文本。
pub fn export_sql_preview(st: &Rc<PanelState>, idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.tabs.borrow().get(idx).cloned() else {
        return String::new();
    };
    export_sql_text(&tab).unwrap_or_default()
}

/// 导出 INSERT SQL 脚本（Navicat 的 "Export SQL"）。
///
/// 表名取页签对应的表（普通查询页签没有表名，就用 SQL 里的表名猜不出来，
/// 这时退化成 `exported_table`，并在状态里说明）。
fn do_cancel(st: &Rc<PanelState>, ui: &DbUi) {
    if !ui.connected.get() || !ui.busy.get() {
        return;
    }
    ui.btn_cancel.set_sensitive(false);
    set_status(ui, tr("正在取消…"), false);
    if let Some(tx) = st.tx.borrow().as_ref() {
        let _ = tx.send(WorkerCmd::DbCancel);
    }
}

fn send_cmd(st: &Rc<PanelState>, ui: &DbUi, cmd: WorkerCmd) {
    let tx = st.tx.borrow().clone();
    let ok = match tx {
        Some(tx) => tx.send(cmd).is_ok(),
        None => false,
    };
    if !ok {
        ui.busy.set(false);
        set_status(ui, tr("发送命令失败（worker 已退出）"), true);
        update_buttons(ui);
    }
}

/// 发元数据请求；返回这次请求的 id（调用方要按同一个 id 登记 pending）。
///
/// **id 必须一致**：`on_meta` 靠 id 从 pending 里取回"这次请求要干什么"。
/// 之前 `open_table` 自己 next_id() 登记 MetaForTable，而 `send_meta`
/// 又 next_id() 登记 Meta —— worker 回的是后者那个 id，于是表列信息被当成
/// 树节点填充，双击表永远拿不到列（数据页签是空的）。
fn send_meta(st: &Rc<PanelState>, ui: &DbUi, req: DbMetaReq) -> u64 {
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Meta(req.clone()));
    send_cmd(st, ui, WorkerCmd::DbMeta { id, req });
    id
}

/// 申请一个请求 id。
fn next_id(ui: &DbUi) -> u64 {
    let id = ui.next_req.get();
    ui.next_req.set(id + 1);
    id
}

/* ---------------- 导入向导（阶段 18：CSV 导入） ---------------- */

/// 导入入口（对象树右键「导入数据（CSV）…」）。
///
/// **流程**：选文件 → 读文件 → 查目标表的列 → 字段映射对话框 → 执行。
/// 查列那一步是异步的，所以中间挂一个 `PendingReq::ImportPrep` 等回包。
/// 导入：**文件已经选好了**，直接读文件 + 查表结构 → 弹字段映射。
///
/// 阶段 72 改的入口：以前是 [`on_import_csv`] 先弹原生文件选择器、再走这里；
/// 现在导入向导（[`crate::panel::dbtransfer::open_import`]）**一个对话框里**
/// 选表 + 选文件 ⇒ 直接调这个函数。
pub fn on_import_csv_path(st: &Rc<PanelState>, schema: &str, table: &str, path: &PathBuf) {
    {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        if !ui.connected.get() {
            set_status(ui, tr("还没连上数据库，没法导入"), true);
            return;
        }
    }
    let schema = schema.to_string();
    let table = table.to_string();
    let st2 = Rc::clone(st);
    let text = match dbimport::read_text(path) {
        Ok(t) => t,
        Err(e) => {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            set_status(ui, &trf!("导入失败：{e}", e = e), true);
            return;
        }
    };
    // 查目标表的列（类型 + 列名）——映射界面要用
    let slot = st2.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(
        id,
        PendingReq::ImportPrep {
            schema: schema.clone(),
            table: table.clone(),
            text: text.clone(),
            auto: false,
        },
    );
    send_cmd(
        &st2,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::Columns {
                schema: schema.clone(),
                table: table.clone(),
            },
        },
    );
    set_status(ui, tr("正在读取表结构…"), false);
}

/// 列信息到手 → 弹字段映射对话框。
fn show_import_mapping(
    st: &Rc<PanelState>,
    schema: &str,
    table: &str,
    text: &str,
    cols: &[dbmeta::ColumnDef],
    auto: bool,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    // 表列 + 显示用的类型名
    let table_cols: Vec<(String, String)> = cols
        .iter()
        .map(|c| (c.name.clone(), dbmeta::ddl_type(c)))
        .collect();
    if table_cols.is_empty() {
        set_status(ui, &trf!("表 {schema}.{table} 没有列，导不进去", schema = schema, table = table), true);
        return;
    }
    // 预先算好「列下标 → (列名, 类型)」：映射回调是 `'static` 闭包，
    // 拿不到这里的局部借用，只能提前算成 owned 的。
    let meta: Vec<(usize, String, ColType)> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.name.clone(), ColType::from_db_type(&c.data_type)))
        .collect();
    // 自动模式（冒烟测试）：不弹对话框，按列名自动映射直接导入
    if auto {
        let opts = dbimport::ImportOptions::default();
        let parsed = match dbcsv::parse_csv(text, opts.separator) {
            Ok(t) => t,
            Err(e) => {
                set_status(ui, &trf!("导入失败：{e}", e = e), true);
                return;
            }
        };
        let (header, rows) = if opts.first_row_is_header && !parsed.is_empty() {
            (parsed[0].clone(), parsed[1..].to_vec())
        } else {
            let width = parsed.first().map(|r| r.len()).unwrap_or(0);
            ((1..=width).map(|i| trf!("第 {i} 列", i = i)).collect(), parsed.clone())
        };
        let map = dbcsv::auto_map(
            &header,
            &table_cols.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        );
        let m = dbimport::ImportMapping {
            options: opts,
            map,
            csv_header: header,
            rows,
        };
        drop(slot);
        do_import(st, schema, table, &m, &meta);
        return;
    }
    let target = format!("{schema}.{table}");
    let schema = schema.to_string();
    let table = table.to_string();
    let text = text.to_string();
    let st2 = Rc::clone(st);
    dbimport::show_mapping(Some(&ui.win), &target, &text, &table_cols, move |m| {
        do_import(&st2, &schema, &table, &m, &meta);
    });
}

/// 按映射构造批次并发出导入命令。
///
/// `meta` 是「表列下标 → (列名, 类型)」。
fn do_import(
    st: &Rc<PanelState>,
    schema: &str,
    table: &str,
    m: &dbimport::ImportMapping,
    meta: &[(usize, String, ColType)],
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    // 参与导入的表列
    let mut names: Vec<String> = Vec::new();
    let mut types: Vec<ColType> = Vec::new();
    let mut src: Vec<usize> = Vec::new(); // 对应 CSV 的第几列
    for (i, name, ty) in meta {
        // **只导入被映射的列**：没映射的列不写，让数据库用默认值
        // （自增 id、默认时间、默认状态……）。
        let Some(Some(csv_idx)) = m.map.get(*i) else {
            continue;
        };
        names.push(name.clone());
        types.push(*ty);
        src.push(*csv_idx);
    }
    if names.is_empty() {
        set_status(ui, tr("没有映射任何字段，没法导入"), true);
        return;
    }
    // 按目标列对齐每一行
    let mut rows: Vec<Vec<Option<String>>> = Vec::with_capacity(m.rows.len());
    for r in &m.rows {
        let mut row: Vec<Option<String>> = Vec::with_capacity(names.len());
        for &ci in &src {
            let v = r.get(ci).map(|s| s.as_str()).unwrap_or("");
            if v.is_empty() && m.options.empty_as_null {
                row.push(None);
            } else {
                row.push(Some(v.to_string()));
            }
        }
        rows.push(row);
    }
    if rows.is_empty() {
        set_status(ui, tr("文件里没有数据行"), true);
        return;
    }
    let proto = current_proto(ui);
    let batch = crate::backend::db::ImportBatch {
        proto,
        schema: schema.to_string(),
        table: table.to_string(),
        columns: names.clone(),
        types,
        rows: rows.clone(),
    };
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::ImportRun { rows: rows.len() });
    ui.busy.set(true);
    set_status(ui, &trf!("正在导入 {v0} 行到 {schema}.{table}…", schema = schema, table = table, v0 = rows.len()), false);
    send_cmd(st, ui, WorkerCmd::DbImport { id, batch });
}

/// 导入结果。
pub fn on_import_result(
    st: &Rc<PanelState>,
    id: u64,
    result: Result<crate::backend::db::ImportReport, String>,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.busy.set(false);
    let pending = ui.pending.borrow_mut().remove(&id);
    let rows = match pending {
        Some(PendingReq::ImportRun { rows }) => rows,
        _ => 0,
    };
    update_buttons(ui);
    match result {
        Ok(rep) => {
            *ui.last_import.borrow_mut() = Some((true, trf!("{v0} 行", v0 = rep.inserted)));
            set_status(ui, &trf!("导入完成：{v0} 行", v0 = rep.inserted), false);
            dialogs::info(
                Some(&ui.win),
                tr("导入完成"),
                &trf!("已导入 {v0} 行。", v0 = rep.inserted),
            );
            // 导入完刷新对象树（表里的行数变了）。
            // **用 reload() 不用 clear()**：clear() 会把根节点也清掉，
            // 树就再也长不回来——用户看到的是"导入完对象树变空白"。
            ui.tree.reload();
        }
        Err(e) => {
            *ui.last_import.borrow_mut() = Some((false, e.clone()));
            set_status(ui, &trf!("导入失败：{e}", e = e), true);
            // 用普通 Dialog（不是 `AlertDialog`）：要留住引用才能**主动关掉**
            // ——它不关就一直浮在界面上（挡住截图、挡住用户点别的）。
            // 错误文本用可选择的 Label，用户要能选中复制去查资料。
            let dlg = gtk::Dialog::with_buttons(
                Some(tr("导入失败")),
                Some(&ui.win),
                gtk::DialogFlags::MODAL,
                &[(tr("关闭"), gtk::ResponseType::Close)],
            );
            let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
            body.set_margin_start(12);
            body.set_margin_end(12);
            body.set_margin_top(10);
            body.set_margin_bottom(10);
            let lbl = gtk::Label::new(Some(&trf!("导入没有完成，已全部回滚（数据库里一行都没进）。\n\n{e}\n\n本次共处理 {rows} 行。", e = e, rows = rows)));
            lbl.set_wrap(true);
            lbl.set_selectable(true);
            lbl.set_xalign(0.0);
            body.append(&lbl);
            dlg.content_area().append(&body);
            dialogs::style_dialog(&dlg);
            dlg.present();
            *ui.import_err.borrow_mut() = Some(dlg);
        }
    }
}

/* ---------------- ER 图（阶段 19：Database Model） ---------------- */

/// 一个 ER 图页签。
struct ErTab {
    id: u64,
    /// 弱引用：页签 → 面板 → 页签 会成环（面板永远释放不掉）
    st: Weak<PanelState>,
    view: Rc<erview::ErView>,
    /// 宿主页（notebook 里的那一页）
    page: gtk::Box,
    schema: String,
    /// `Some` = 只画这张表；`None` = 整个模式
    table: Option<String>,
}

/// 打开 ER 图。
///
/// **表节点**（`table = Some`）画这张表跟别人的关系；**模式节点**
/// （`table = None`）画整个模式。
pub fn open_er(st: &Rc<PanelState>, schema: &str, table: Option<&str>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if !ui.connected.get() {
        set_status(ui, tr("尚未连接数据库"), true);
        return;
    }
    let tab = build_er_tab(st, ui, schema, table);
    mount_er_tab(ui, &tab);
    // 要图的数据（整张图一次查完，见 worker 的 DbEr）
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Er { tab_id: tab.id });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbEr {
            id,
            schema: schema.to_string(),
            table: table.map(|t| t.to_string()),
        },
    );
}

/// 建 ER 图页签（**不挂到任何容器**）。
///
/// **为什么拆出来**（阶段 72e）：表文档里的「ER」视图也是 ER 页签，但它挂在
/// **文档页的内层页签**里 ⇒ 挂载的事交给调用方。
fn build_er_tab(
    st: &Rc<PanelState>,
    ui: &DbUi,
    schema: &str,
    table: Option<&str>,
) -> Rc<ErTab> {
    let view = erview::ErView::new();
    // 双击图里的表 → 打开它的数据页签（Navicat 的 Model 视图也这样）
    {
        let st2 = Rc::clone(st);
        let schema = schema.to_string();
        view.set_on_open(move |table| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            open_table(&st2, ui, &schema, table, false);
        });
    }
    let tab = Rc::new(ErTab {
        id: next_id(ui),
        st: Rc::downgrade(st),
        view: Rc::clone(&view),
        page: view.root.clone(),
        schema: schema.to_string(),
        table: table.map(|t| t.to_string()),
    });

    ui.er_tabs.borrow_mut().push(Rc::clone(&tab));
    view.set_status(tr("正在读表结构…"));
    tab
}

/// 把 ER 图页签挂到**外层** notebook。
fn mount_er_tab(ui: &DbUi, tab: &Rc<ErTab>) {
    let schema = tab.schema.clone();
    // 页签标题 + 关闭按钮（跟表结构页签一个样式）
    let title = match &tab.table {
        Some(t) => trf!("ER 图：{schema}.{t}", schema = schema, t = t),
        None => trf!("ER 图：{schema}", schema = schema),
    };
    let label = gtk::Label::new(Some(&title));
    let close = gtk::Button::new();
    close.add_css_class("flat");
    let img = gtk::Image::from_icon_name("window-close-symbolic");
    img.set_pixel_size(12);
    close.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&close);
    close.set_tooltip_text(Some(tr("关闭这个页签")));
    let tab_head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab_head.append(&label);
    tab_head.append(&close);
    {
        let tab2 = Rc::clone(tab);
        close.connect_clicked(move |_| {
            let Some(st) = tab2.st.upgrade() else { return };
            let slot = st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            if let Some(pos) = ui.notebook.page_num(&tab2.page) {
                ui.notebook.remove_page(Some(pos));
            }
            ui.er_tabs
                .borrow_mut()
                .retain(|t| !Rc::ptr_eq(t, &tab2));
            if ui.notebook.n_pages() == 0 {
                ui.stack.set_visible_child_name("empty");
            }
        });
    }
    ui.notebook.append_page(&tab.page, Some(&tab_head));
    ui.notebook.set_current_page(Some(ui.notebook.n_pages() - 1));
    ui.stack.set_visible_child_name("pages");
}

/* ---------------- 表数据导入 / 导出（阶段 52，阶段 72 改名） ---------------- */

/// 当前连接的名字（工作台状态行 / 导入导出向导用）。
pub fn current_conn_label(st: &Rc<PanelState>) -> Option<String> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let cur = ui.current.borrow().clone()?;
    Some(conn_label_of(&cur))
}

/// 打开**导出表**向导（测试钩子 / 入口）。
pub fn open_transfer_now(st: &Rc<PanelState>) {
    let parent = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        ui.win.clone()
    };
    crate::panel::dbtransfer::open_export(st, Some(&parent), None);
}

/// 打开**导入数据（CSV）**向导（测试钩子 / 入口）。
pub fn open_import_now(st: &Rc<PanelState>) {
    let parent = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        ui.win.clone()
    };
    crate::panel::dbtransfer::open_import(st, Some(&parent), None);
}

/// 对象树右键菜单上的所有文字（测试钩子）。
pub fn tree_menu_labels(st: &Rc<PanelState>) -> Vec<String> {
    let Some(tree) = tree(st) else { return Vec::new() };
    tree.menu_labels()
}

/// 顶栏「表数据导入/导出…」入口的**菜单项文字**（测试钩子）。
///
/// 阶段 72：这个入口是个**菜单按钮**（导入 / 导出两个去向），不是以前那个
/// 带「方向」下拉的对话框 ⇒ 冒烟要能看见菜单里到底有哪两项。
pub fn transfer_menu_labels(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(pop) = ui.btn_transfer.popover() else {
        return Vec::new();
    };
    let Some(child) = pop.child() else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur = child.first_child();
    while let Some(c) = cur {
        if let Some(b) = c.downcast_ref::<gtk::Button>() {
            if let Some(t) = crate::menu::item_label(b) {
                out.push(t);
            }
        }
        cur = c.next_sibling();
    }
    out
}

/// 顶栏「表数据导入/导出…」入口的**按钮文字**（测试钩子）。
pub fn transfer_button_label(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(child) = ui.btn_transfer.child() else {
        return String::new();
    };
    // `MenuButton` 的 child 就是那颗 Label；万一以后换成 Box 包装，
    // 就往下找一层。
    if let Some(l) = child.downcast_ref::<gtk::Label>() {
        return l.text().to_string();
    }
    let mut cur = child.first_child();
    while let Some(c) = cur {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            return l.text().to_string();
        }
        cur = c.next_sibling();
    }
    String::new()
}

/// 对象树上**已经加载出来**的表 / 视图（`(模式, 名字, 是视图)`）。
pub fn tree_objects(st: &Rc<PanelState>) -> Vec<(String, String, bool)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.tree.objects()
}

/// 开始导出整张表（阶段 52）。
///
/// **导在 worker 侧做**：一次要读的行数可能很大，全塞进 UI 侧会撑爆内存、
/// 还把界面卡住。worker 那边**边查边写**（分页查 + 分页写），UI 只等结果。
pub fn start_export(
    st: &Rc<PanelState>,
    schema: &str,
    table: &str,
    path: &str,
    fmt: crate::backend::dbexport::ExportFmt,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if !ui.connected.get() {
        set_status(ui, tr("还没连上数据库，没法导出"), true);
        return;
    }
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Export);
    send_cmd(
        st,
        ui,
        WorkerCmd::DbExport {
            id,
            schema: schema.to_string(),
            table: table.to_string(),
            path: path.to_string(),
            fmt,
        },
    );
    set_status(
        ui,
        &trf!("正在导出 {schema}.{table} 到 {path}…", schema = schema, table = table, path = path),
        false,
    );
}

/// 导出结果回来了（阶段 52）。
pub fn on_export(
    st: &Rc<PanelState>,
    id: u64,
    result: Result<crate::backend::dbexport::ExportReport, String>,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(PendingReq::Export) = ui.pending.borrow_mut().remove(&id) else {
        return;
    };
    match result {
        Ok(rep) => {
            // ⚠️ **不弹窗**：导完了只在状态栏写一句（AGENTS.md 的规矩：
            // 传完 / 导完不弹窗，桌面没通知守护进程时弹窗等于"每来一次弹一次"）。
            set_status(ui, &trf!("已导出 {v0} 行到 {v1}", v0 = rep.rows, v1 = rep.path), false);
        }
        Err(e) => set_status(ui, &trf!("导出失败：{e}", e = e), true),
    }
}

/// ER 图的数据回来了。
pub fn on_er(
    st: &Rc<PanelState>,
    id: u64,
    result: Result<crate::backend::ermodel::ErGraph, String>,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let pending = ui.pending.borrow_mut().remove(&id);
    let Some(PendingReq::Er { tab_id }) = pending else { return };
    let tab = {
        let tabs = ui.er_tabs.borrow();
        tabs.iter().find(|t| t.id == tab_id).cloned()
    };
    let Some(tab) = tab else { return };
    match result {
        Ok(g) => {
            let n = g.tables.len();
            tab.view.set_graph(g);
            set_status(ui, &trf!("ER 图已就绪（{n} 张表）", n = n), false);
        }
        Err(e) => {
            tab.view.set_status(&trf!("画 ER 图失败：{e}", e = e));
            set_status(ui, &trf!("画 ER 图失败：{e}", e = e), true);
        }
    }
}

fn update_buttons(ui: &DbUi) {
    let idle = !ui.busy.get();
    let conn = ui.connected.get();
    ui.btn_run.set_sensitive(idle && conn);
    ui.btn_run_sel.set_sensitive(idle && conn);
    ui.btn_cancel.set_sensitive(conn && ui.busy.get());
    // ⚠️ **可见 ≠ 可点**：导入导出按钮没连上库的时候是**灰的**——
    // set_visible(true) 不会让它能点（AGENTS.md 的老坑）。
    ui.btn_transfer.set_sensitive(conn);
}

fn set_status(ui: &DbUi, text: &str, error: bool) {
    ui.status.set_text(text);
    if error {
        ui.status.add_css_class("error");
        ui.status.remove_css_class("dim-label");
    } else {
        ui.status.remove_css_class("error");
        ui.status.add_css_class("dim-label");
    }
}

fn is_cancel(err: &str) -> bool {
    let low = err.to_ascii_lowercase();
    low.contains("canceling statement")
        || low.contains("cancelled")
        || low.contains("canceled")
        || err.contains(tr("已取消"))
}

/* ---------------- 数据编辑 ---------------- */

/// 进入编辑模式：开事务 + 允许改单元格。
fn do_edit(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() || tab.editing.get() {
        return;
    }
    if !check_editable(ui, tab) {
        return;
    }
    begin_edit(st, ui, tab);
}

/// 检查这个页签能不能改（是表数据、不是视图）。
///
/// 插入行 / 删除行 / 编辑都走这一关，提示文案统一。
fn check_editable(ui: &DbUi, tab: &Rc<ResultTab>) -> bool {
    let Some(target) = tab.target.borrow().clone() else {
        set_status(ui, tr("这个结果不是表数据，不能编辑"), true);
        return false;
    };
    if target.is_view {
        set_status(ui, tr("视图不能编辑（没有可直接更新的基表）"), true);
        return false;
    }
    true
}

/// 开事务（进入编辑模式）。
fn begin_edit(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    ui.busy.set(true);
    update_buttons(ui);
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(
        id,
        PendingReq::Tx {
            op: DbTxOp::Begin,
            tab_id: tab.id,
        },
    );
    send_cmd(st, ui, WorkerCmd::DbTx { id, op: DbTxOp::Begin });
}

/// 插入一行新记录（9.4）。
///
/// 已经在编辑模式就直接加；不在就先开事务，**事务开成功后**再加行
/// （避免出现"行加上了但事务没开成"的半截状态）。
fn do_insert_row(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() {
        return;
    }
    if !check_editable(ui, tab) {
        return;
    }
    if tab.editing.get() {
        apply_row_action(ui, tab, RowAction::Insert);
        return;
    }
    tab.pending_row_action.set(Some(RowAction::Insert));
    begin_edit(st, ui, tab);
}

/// 删除选中的行（9.4）。
///
/// **只是标记**：真正删库要等点「保存修改」（同一个事务里一起提交）。
///
/// ⚠️ **删掉全部选中的行**（阶段 46：网格改成多选后就是这个语义）——
/// 只删第一行的话，用户框选了 5 行按下删除、只少了一行，还以为程序有 bug。
fn do_delete_rows(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() {
        return;
    }
    if !check_editable(ui, tab) {
        return;
    }
    let rows = tab.grid.selected_display_rows();
    if rows.is_empty() {
        set_status(ui, tr("先选中要删除的行"), true);
        return;
    }
    delete_rows_at(st, ui, tab, rows);
}

/// 标记删除某几行（按显示行号）。
///
/// 已经在编辑模式就直接标记；不在就先开事务，**事务开成功后**再标记
/// （跟插入行一样：避免出现"行标了但事务没开成"的半截状态）。
fn delete_rows_at(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>, rows: Vec<usize>) {
    if tab.editing.get() {
        apply_row_action(ui, tab, RowAction::Delete(rows));
        return;
    }
    tab.pending_row_action.set(Some(RowAction::Delete(rows)));
    begin_edit(st, ui, tab);
}

/// 把行操作落到网格上（事务已经开好了）。
fn apply_row_action(ui: &DbUi, tab: &Rc<ResultTab>, action: RowAction) {
    match action {
        RowAction::Insert => {
            tab.grid.insert_row();
            tab.status.set_text(tr("已插入一行（没填的列用数据库默认值），填完点「保存修改」"));
            set_status(ui, tr("已插入一行，填完点「保存修改」"), false);
        }
        RowAction::Delete(rows) => {
            // ⚠️ **倒着删**：删掉一行会让后面的显示行号往前挪，正着删会漏删。
            let mut rows = rows;
            rows.sort_unstable();
            rows.reverse();
            let mut n = 0usize;
            for r in rows {
                if tab.grid.delete_display_row(r) {
                    n += 1;
                }
            }
            if n == 0 {
                tab.status.set_text(tr("没有选中可删除的行"));
                set_status(ui, tr("先选中要删除的行"), true);
            } else {
                tab.status.set_text(&trf!("已标记 {n} 行删除，点「保存修改」才真正删", n = n));
                set_status(ui, &trf!("已标记 {n} 行删除，点「保存修改」才真正删", n = n), false);
            }
        }
    }
}

/// 保存修改：逐条 INSERT / UPDATE / DELETE，全成功才 COMMIT。
fn do_save(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() || !tab.editing.get() {
        return;
    }
    let Some(target) = tab.target.borrow().clone() else {
        return;
    };
    let proto = current_proto(ui);
    let edits = tab.grid.pending_edits();
    let inserts = tab.grid.pending_inserts();
    let deletes = tab.grid.pending_deletes();
    if edits.is_empty() && inserts.is_empty() && deletes.is_empty() {
        set_status(ui, tr("没有要保存的改动"), true);
        return;
    }
    let columns = tab.grid.columns();
    let types = tab.types.borrow().clone();
    let pk = tab.pk.borrow().clone();
    let rows = tab.grid.original_rows();

    // 拼语句：类型不符、列不存在等问题在这一步就报出来，
    // **一条都不执行**（改了一半比不改更糟）。
    //
    // **顺序：INSERT → UPDATE → DELETE**。先插是有意的：新行要是跟已有行
    // 撞了唯一约束，应该在 INSERT 阶段就失败并整体回滚，而不是先把旧行删掉
    // 再发现插不进去。
    let mut sqls: Vec<String> = Vec::new();
    for (i, vals) in inserts.iter().enumerate() {
        match dbmeta::build_insert(proto, &target.schema, &target.table, &columns, &types, vals) {
            Ok(sql) => sqls.push(sql),
            // 一行都没填 → 跳过（不是错误：用户可能只是开了个空行）
            Err(e) if dbmeta::is_empty_row_err(&e) => {
                let _ = i;
            }
            Err(e) => {
                set_status(ui, &trf!("改动有问题，未提交：{e}", e = e), true);
                return;
            }
        }
    }
    match dbmeta::build_updates(
        proto,
        &target.schema,
        &target.table,
        &columns,
        &types,
        &pk,
        &rows,
        &edits,
    ) {
        Ok(v) => sqls.extend(v),
        Err(e) => {
            set_status(ui, &trf!("改动有问题，未提交：{e}", e = e), true);
            return;
        }
    }
    for src in &deletes {
        let Some(orig) = rows.get(*src) else {
            set_status(ui, &trf!("第 {v0} 行不在结果集里", v0 = src + 1), true);
            return;
        };
        let (pk_cond, fallback) = dbmeta::row_condition(&columns, &pk, orig);
        match dbmeta::build_delete(proto, &target.schema, &target.table, &pk_cond, &fallback) {
            Ok(sql) => sqls.push(sql),
            Err(e) => {
                set_status(ui, &trf!("改动有问题，未提交：{e}", e = e), true);
                return;
            }
        }
    }
    if sqls.is_empty() {
        set_status(ui, tr("没有要保存的改动"), true);
        return;
    }
    *ui.save_queue.borrow_mut() = sqls.into_iter().collect();
    *ui.save_job.borrow_mut() = Some(SaveJob {
        tab_id: tab.id,
        total: ui.save_queue.borrow().len(),
        affected: 0,
    });
    ui.busy.set(true);
    update_buttons(ui);
    set_status(ui, tr("正在保存修改…"), false);
    send_next_save(st, ui);
}

fn do_discard(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    if !ui.connected.get() || !tab.editing.get() {
        return;
    }
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(
        id,
        PendingReq::Tx {
            op: DbTxOp::Rollback,
            tab_id: tab.id,
        },
    );
    send_cmd(st, ui, WorkerCmd::DbTx { id, op: DbTxOp::Rollback });
}

/// 发下一条 UPDATE（队列空了就提交事务）。
fn send_next_save(st: &Rc<PanelState>, ui: &DbUi) {
    let next = ui.save_queue.borrow_mut().pop_front();
    match next {
        Some(sql) => {
            let id = next_id(ui);
            ui.save_id.set(id);
            let tab_id = ui.save_job.borrow().as_ref().map(|j| j.tab_id).unwrap_or(0);
            ui.pending
                .borrow_mut()
                .insert(id, PendingReq::Exec { tab_id });
            send_cmd(st, ui, WorkerCmd::DbExec { id, sql });
        }
        None => {
            // 全部执行完 → 提交
            let tab_id = ui.save_job.borrow().as_ref().map(|j| j.tab_id).unwrap_or(0);
            let id = next_id(ui);
            ui.pending.borrow_mut().insert(
                id,
                PendingReq::Tx {
                    op: DbTxOp::Commit,
                    tab_id,
                },
            );
            send_cmd(st, ui, WorkerCmd::DbTx { id, op: DbTxOp::Commit });
        }
    }
}

/* ---------------- worker 事件 ---------------- */

pub fn on_connected(st: &Rc<PanelState>, result: Result<String, String>) {
    // ⚠️ **借用要尽快结束**：挂页面（`mount`）还要再借一次 `st.db_ui`，
    //    在同一个借用里调 ⇒ RefCell 双重借用 panic。
    let mut fail: Option<String> = None;
    {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        ui.busy.set(false);
        match &result {
            Ok(label) => {
                ui.connected.set(true);
                // 活跃标记（阶段 57）：两棵树上的「已连接」跟着变。
                // 存副本到面板状态，别让别的模块去借 db_ui（那边正被这里借着）。
                *st.db_active.borrow_mut() = ui.current.borrow().as_ref().map(|p| p.name.clone());
                let proto = current_proto(ui);
                // 默认模式自动展开（阶段 72d）：连上就看得见表，不用再点三角
                ui.tree.set_default_schema(default_schema_of(ui));
                ui.tree.reset(label, proto.label());
                // 连接早就建好了（阶段 13：连接信息在连接管理器 / 会话树里建），
                // 这里**不再落配置**——省得"连上了才算数"，用户配错的连接也会
                // 被悄悄存下来。
                let msg = trf!("已连接：{label}", label = label);
                set_status(ui, &msg, false);
            }
            Err(e) => {
                ui.connected.set(false);
                *st.db_active.borrow_mut() = None;
                clear_tabs(ui);
                let msg = trf!("连接失败：{e}", e = e);
                set_status(ui, &msg, true);
                fail = Some(msg);
            }
        }
        update_buttons(ui);
        // 空态跟着换（阶段 51：连上了 / 没连上，写法不一样）
        refresh_empty(st, ui);
        // 数据库这一维变了 ⇒ 状态点 / 两棵树上的标记跟着变（阶段 57/65）
        super::refresh_active_marks();
    }
    // —— 借用放掉了，下面可以安全地挂页面 / 弹窗 ——
    if let Some(msg) = fail {
        // ⚠️ 连不上 ⇒ **不挂页面**（用户 2026-11-02）⇒ 状态行用户看不见
        //    ⇒ 这种时候得弹出来说清楚。
        report_unmounted(st, &msg, true);
    } else {
        // ⚠️ **连上了才把工作台摆出来**（用户 2026-11-02：「任何时候，必须
        // 连上数据库之后才允许显示数据库管理页面」）。
        mount(st);
    }
}

/// 服务器要密码、但这条连接没带密码 → **弹密码框**让用户当场输，再重连。
///
/// **对标 Navicat / DBeaver**：连接时问密码，而不是甩一句"连接失败"。
///
/// **为什么不在连接前就问**：不是所有库都要密码（PG 的 trust 认证、
/// 免密 MySQL）。先问会平白多一次输入，用户还得凭空想密码。所以
/// **先连、缺密码了再问**。
///
/// **实测背景（2026-10）**：PG 驱动在这种情况下抛的是 `Kind::Config`，
/// 原文 "invalid configuration"——用户完全看不懂，只能来问人。
pub fn on_need_password(st: &Rc<PanelState>, detail: String) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.busy.set(false);
    ui.connected.set(false);
    // 活跃标记（阶段 57）：没连上就别亮着
    *st.db_active.borrow_mut() = None;
    let Some(p) = ui.current.borrow().clone() else {
        set_status(ui, &detail, true);
        return;
    };
    let key = ui.session_key.clone();
    let name = p.name.clone();
    let msg = trf!("{detail}\n\n请输入这条连接的数据库密码（{v0}@{v1}:{v2}/{v3}）。", detail = detail, v0 = p.user, v1 = p.host, v2 = p.port, v3 = p.dbname);
    set_status(ui, tr("数据库要求密码：请在上面对话框里输入"), false);
    update_buttons(ui);
    let st2 = Rc::clone(st);
    // ⚠️ 父窗口用**主窗口**：工作台页面这时还没挂（没连上不许显示），
    //    `ui.win` 在独立窗口模式下是个还没映射的窗口 ⇒ 当 transient parent
    //    会让对话框位置跑飞。
    let parent = st.topwin();
    dialogs::ask_password_remember(
        parent.as_ref(),
        tr("数据库需要密码"),
        &msg,
        p.remember,
        // ⚠️ 堡垒机会话（会话没在连接列表里）⇒ 不显示「记住密码」
        //    （用户 2026-11-03：「对于堡垒机使用场景，所有地方都不要出现
        //    『记住密码』」）。
        Some(ui.session_key.as_str()),
        move |pw, keep| {
            let Some(pw) = pw else {
                // 取消：把"下一步该做什么"说清楚，别让用户对着
                // "连接失败"发呆。
                {
                    let slot = st2.db_ui.borrow();
                    let Some(ui) = slot.as_ref() else { return };
                    // ⚠️ 阶段 96，用户 2026-11-01：「这是没必要的，
                    // 没必要这么啰嗦。用户肯定知道自己在干啥」：原来还附了一整段
                    // 「要么现在重连时再输一次，要么去连接管理器里编辑…（勾「记住
                    // 密码」，并先在顶栏「凭据库…」里解锁…）」——用户自己点的取消，
                    // 知道自己要干什么 ⇒ 这段既占地方（顶栏被撑成两行，把下面的
                    // 内容顶下去），又是说教。
                    set_status(ui, tr("没输入密码，连接已取消。"), true);
                    update_buttons(ui);
                }
                // ⚠️ 页面还没挂（没连上不许显示工作台）⇒ 状态行用户看不见
                //    ⇒ 弹一句，别让"点了没反应"。
                //    ⚠️ 必须在**释放借用之后**调（它自己还要借 `st.db_ui`）。
                report_unmounted(&st2, tr("没输入密码，连接已取消。"), false);
                return;
            };
            let pw = zeroize::Zeroizing::new(pw);
            if keep {
                remember_password(&key, &name, pw.as_str());
            }
            let err = {
                let slot = st2.db_ui.borrow();
                let Some(ui) = slot.as_ref() else { return };
                // 这次连接就用刚输的密码（哪怕没能存进凭据库）
                st2.db_pass.borrow_mut().insert(name.clone(), pw);
                do_connect(&st2, ui)
            };
            // 借用放掉了再弹窗
            if let Some(msg) = err {
                report_unmounted(&st2, &msg, true);
            }
        },
    );
}

/// 把刚输入的密码存进凭据库（用户在提示框里勾了「记住密码」时）。
///
/// **存不进去也要说清楚**，但**不能因此不连**——用户已经输了密码，
/// 这次连上才是第一位的（"记住"是加分项，不是前提）。
fn remember_password(key: &str, name: &str, pw: &str) {
    if !crate::vault::exists() {
        dialogs::info(
            None,
            tr("密码没有保存"),
            tr("凭据库还没建，这次的密码没有存住（连接不受影响）。\n\n\
             凭据库是本机存连接密码的地方（用主密码加密）。\
             去顶栏菜单「凭据库…」设个主密码建一个，\
             之后连库时勾上「存进凭据库」就能存住。"),
        );
        return;
    }
    if !crate::vault::is_unlocked() {
        dialogs::info(
            None,
            tr("密码没有保存"),
            tr("凭据库没解锁，这次的密码没有存住（连接不受影响）。\n\n\
             去顶栏菜单「凭据库…」输入主密码解锁，再连一次就能存住。"),
        );
        return;
    }
    if !crate::vault::put(&dbstore::vault_name(key, name), pw) {
        dialogs::info(
            None,
            tr("密码没有保存"),
            tr("写入凭据库失败（凭据库可能已满或被占用）。\n\
             这次的连接不受影响，下次连还要再输一次。"),
        );
        return;
    }
    // 连接配置里的「记住密码」也要跟着打开——
    // 不然下次连接时压根不去凭据库里找密码，勾了也白勾。
    if let Some(mut p) = dbstore::get(key, name) {
        if !p.remember {
            p.remember = true;
            dbstore::save(key, &p);
        }
    }
}

/// 刷新结果页签的面包屑（阶段 43）。
///
/// **什么时候刷**：拿到列信息之后（`on_rows`）。在那之前不知道有几列，
/// 面包屑只有半截。
fn update_crumbs(ui: &DbUi, tab: &Rc<ResultTab>) {
    let conn = ui
        .current
        .borrow()
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let (schema, table) = match tab.target.borrow().as_ref() {
        Some(t) => (t.schema.clone(), t.table.clone()),
        None => (String::new(), String::new()),
    };
    let cols = tab.grid.columns().len();
    let parts = crumb_parts(&conn, &schema, &table, cols);

    // 清掉旧的
    while let Some(child) = tab.crumbs.first_child() {
        tab.crumbs.remove(&child);
    }
    if parts.is_empty() {
        // 没有内容就别占一行高度（空态看着更空）
        tab.crumbs.set_visible(false);
        return;
    }
    tab.crumbs.set_visible(true);
    let n = parts.len();
    for (i, p) in parts.iter().enumerate() {
        let l = gtk::Label::new(Some(p));
        l.add_css_class("pp-crumb");
        // 最后一片是补充信息（"N 字段"），压暗一点
        if i + 1 == n {
            l.add_css_class("pp-crumb-dim");
        }
        tab.crumbs.append(&l);
    }
}

/// 查询结果。
pub fn on_rows(st: &Rc<PanelState>, id: u64, rows: DbRows) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let tab_id = match ui.pending.borrow_mut().remove(&id) {
        Some(PendingReq::Query { tab_id }) => tab_id,
        _ => return,
    };
    ui.busy.set(false);
    let Some(tab) = find_tab(ui, tab_id) else { return };
    tab.has_more.set(!rows.done);

    // 第一页：建列；后续页：追加
    if rows.page <= 1 {
        let mut data = dbgrid::GridData {
            columns: rows.columns.clone(),
            types: tab.types.borrow().clone(),
            rows: Vec::new(),
            pk: tab.pk.borrow().clone(),
        };
        // 列类型（阶段 64）：
        // - **表浏览**：用元数据里的（`information_schema`，跟主键配套）；
        // - **普通查询结果**（元数据对不上）：用**行描述里带回来的真实类型**
        //   （PG 走 `prepare` 问的、MySQL 走线协议的）；取不到就标"未知"
        //   ⇒ 列头显示「—」，**不假装是文本**（假装成文本会让用户按字符串
        //   的规矩改数字列）。
        if data.types.len() != rows.columns.len() {
            data.types = (0..rows.columns.len())
                .map(|i| rows.types.get(i).copied().flatten().unwrap_or(ColType::Other))
                .collect();
            data.pk.clear();
        }
        data.rows = rows.rows.clone();
        tab.grid.set_data(data);
        // 套上用户之前定好的列显示设置（阶段 20）：表浏览页签重开时，
        // 那几列还藏着的、顺序也还在。**按列名对**——表加了列也不会错位。
        {
            let ck = tab.col_key.borrow().clone();
            if let Some((conn, schema, table_name)) = ck {
                let stored = dbcols::load(&conn, &schema, &table_name);
                if !stored.is_empty() {
                    let layout = dbcols::apply(&rows.columns, &stored);
                    tab.grid.set_column_layout(layout);
                }
            }
        }
    } else {
        tab.grid.append_rows(rows.rows.clone());
    }

    tab.btn_next.set_sensitive(!rows.done);
    // 列显示设置对**普通查询结果**同样开放（Navicat 的字段显示设置不限
    // 于表浏览）——SELECT 出一堆宽列时也用得上。
    if !rows.columns.is_empty() {
    }
    let mut msg = trf!("{v0} 行", v0 = rows.rows.len());
    if !rows.done {
        msg.push_str(tr("（还有更多）"));
    }
    if rows.truncated {
        msg.push_str(tr("（已达取数上限）"));
    }
    msg.push_str(&format!(" · {}", rows.elapsed_ms));
    tab.status.set_text(&msg);
    // 底栏（阶段 45）：行数 / 耗时 / 待保存项数 / 页码
    tab.last_rows.set(rows.rows.len());
    tab.last_elapsed.set(rows.elapsed_ms);
    update_foot(ui, &tab);
    // 面包屑（阶段 43）：列齐了才知道写"N 字段"
    update_crumbs(ui, &tab);
    update_buttons(ui);
    set_status(ui, &trf!("查询完成：{msg}", msg = msg), false);
}

pub fn on_error(st: &Rc<PanelState>, err: String) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.busy.set(false);
    ui.save_queue.borrow_mut().clear();
    *ui.save_job.borrow_mut() = None;
    update_buttons(ui);
    if err.contains(tr("MySQL 连接已关闭")) {
        // MySQL 首版取消 = 关会话：把"已断开、请重连"说清楚，别只说已取消
        set_status(ui, &err, true);
    } else if is_cancel(&err) {
        set_status(ui, tr("已取消"), false);
    } else {
        set_status(ui, &trf!("失败：{err}", err = err), true);
    }
}

pub fn on_closed(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.connected.set(false);
    // 活跃标记（阶段 57）：断开就别再亮着
    *st.db_active.borrow_mut() = None;
    ui.busy.set(false);
    clear_tabs(ui);
    update_buttons(ui);
    // 阶段 22：工作台里没有「连接」按钮了，断开后得告诉用户**怎么再连上**
    set_status(ui, tr("已断开（再连：双击会话树 / 连接管理器里的这条连接）"), false);
}

/// 元数据应答。
pub fn on_meta(
    st: &Rc<PanelState>,
    id: u64,
    _req: DbMetaReq,
    result: Result<DbMetaPayload, String>,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let pending = ui.pending.borrow_mut().remove(&id);
    let Some(pending) = pending else { return };
    match pending {
        PendingReq::Meta(r) => match result {
            Ok(payload) => ui.tree.fill(&r, &payload),
            Err(e) => {
                // 占位子节点留在树上会一直显示"载入中…"，改成失败提示并允许重试
                ui.tree.mark_failed(&r, &e);
                set_status(ui, &trf!("读元数据失败：{e}", e = e), true);
            }
        },
        PendingReq::ImportPrep {
            schema,
            table,
            text,
            auto,
        } => match result {
            Ok(DbMetaPayload::Columns(cols)) => {
                show_import_mapping(st, &schema, &table, &text, &cols, auto);
            }
            Ok(_) => set_status(ui, tr("读表结构返回的格式不对"), true),
            Err(e) => set_status(ui, &trf!("读表结构失败：{e}", e = e), true),
        },
        PendingReq::ObjectStats { schema, table } => match result {
            Ok(DbMetaPayload::ObjectStats(Some(stats))) => {
                // ⚠️ **表文档里的「属性」视图有自己的一张网格**（阶段 72e）——
                // 共享的那张是给独立属性窗口用的。
                let doc_grid = find_doc(ui, &schema, &table).and_then(|d| d.props_grid.borrow().clone());
                match doc_grid {
                    Some(g) => fill_object_stats_into(&g, &stats),
                    None => fill_object_stats(ui, &stats),
                }
                set_status(ui, &trf!("已读取对象属性：{schema}.{table}", schema = schema, table = table), false);
            }
            Ok(DbMetaPayload::ObjectStats(None)) => {
                set_status(ui, &trf!("对象不存在：{schema}.{table}", schema = schema, table = table), true);
            }
            Ok(_) => set_status(ui, tr("对象属性格式不对"), true),
            Err(e) => set_status(ui, &trf!("读对象属性失败：{e}", e = e), true),
        },
        PendingReq::Search => match result {
            Ok(DbMetaPayload::SearchHits(hits)) => {
                let n = hits.len();
                set_status(ui, &trf!("搜索完成：找到 {n} 个对象", n = n), false);
                show_search_results(st, ui, hits);
            }
            Ok(_) => set_status(ui, tr("搜索结果格式不对"), true),
            Err(e) => set_status(ui, &trf!("搜索失败：{e}", e = e), true),
        },
        PendingReq::Struct { tab_id, part } => {
            let Some(tab) = find_struct_tab(ui, tab_id) else { return };
            match (part, result) {
                (StructPart::Fields, Ok(DbMetaPayload::Columns(cols))) => {
                    let rows: Vec<Vec<Option<String>>> = cols
                        .iter()
                        .map(|c| {
                            vec![
                                Some(c.name.clone()),
                                Some(dbmeta::ddl_type(c)),
                                Some(if c.nullable { tr("是").into() } else { tr("否").into() }),
                                Some(if c.is_pk { tr("是").into() } else { tr("否").into() }),
                                // **没有默认值就显示空**，别让网格显示成 "NULL"：
                                // 那是 SQL 的 NULL 字面量，保存时会被当成"默认值 =
                                // NULL"，给用户凭空加一条 SET DEFAULT。
                                Some(c.default_value.clone().unwrap_or_default()),
                                // 「操作」列（阶段 50）：点这一格弹行内操作菜单
                                Some("⧉ ✕".to_string()),
                            ]
                        })
                        .collect();
                    tab.fields.set_data(dbgrid::GridData {
                        columns: vec![
                            tr("字段").into(),
                            tr("类型").into(),
                            tr("可空").into(),
                            tr("主键").into(),
                            tr("默认值").into(),
                            // 「操作」列（阶段 50）：行内操作入口（复制 / 删除）
                            tr("操作").into(),
                        ],
                        types: vec![ColType::Text; 6],
                        rows,
                        pk: vec![],
                    });
                    // ⚠️ **「操作」列不可编辑**：双击它弹个文本框纯属添乱。
                    tab.fields.set_column_editable(5, false);
                    *tab.cols.borrow_mut() = Some(cols);
                    refresh_struct_ddl(ui, &tab);
                    // 字段表的数据到了 → 预览也刷新一次
                    refresh_field_preview(ui, &tab);
                }
                (StructPart::Indexes, Ok(DbMetaPayload::Indexes(idx))) => {
                    // **主键索引不显示**：主键由字段页的「主键」列管，两边都能改
                    // 就会打架（字段页改主键要重建约束，索引页又把它当普通索引
                    // 删了）。
                    //
                    // ⚠️ `tab.idx` 存的是**过滤后**的列表（与网格里的行同序）：
                    // 保存时要靠「显示行号 → 源行号 → 原索引名」把改动的索引
                    // 对回原来那条（用户改了名字以后就按名字对不上了）。
                    let shown: Vec<dbmeta::IndexDef> =
                        idx.iter().filter(|i| !i.primary).cloned().collect();
                    let rows: Vec<Vec<Option<String>>> = shown
                        .iter()
                        .map(|i| {
                            vec![
                                Some(i.name.clone()),
                                Some(i.columns.join(", ")),
                                Some(if i.unique { tr("是").into() } else { tr("否").into() }),
                            ]
                        })
                        .collect();
                    tab.indexes.set_data(dbgrid::GridData {
                        columns: vec![tr("索引").into(), tr("列").into(), tr("唯一").into()],
                        types: vec![ColType::Text; 3],
                        rows,
                        pk: vec![],
                    });
                    *tab.idx.borrow_mut() = Some(shown);
                    refresh_struct_ddl(ui, &tab);
                }
                (StructPart::ForeignKeys, Ok(DbMetaPayload::ForeignKeys(fk))) => {
                    let rows: Vec<Vec<Option<String>>> = fk
                        .iter()
                        .map(|f| {
                            vec![
                                Some(f.name.clone()),
                                Some(f.column.clone()),
                                Some(f.ref_table.clone()),
                                Some(f.ref_column.clone()),
                            ]
                        })
                        .collect();
                    tab.fks.set_data(dbgrid::GridData {
                        columns: vec![
                            tr("约束").into(),
                            tr("列").into(),
                            tr("引用表").into(),
                            tr("引用列").into(),
                        ],
                        types: vec![ColType::Text; 4],
                        rows,
                        pk: vec![],
                    });
                    *tab.fk.borrow_mut() = Some(fk);
                    refresh_struct_ddl(ui, &tab);
                }
                (StructPart::Triggers, Ok(DbMetaPayload::Triggers(all))) => {
                    // ⚠️ **按表名过滤**：触发器是按模式查回来的（整个模式的
                    // 触发器都在里头），这一页只显示**这张表上的**那些。
                    let mine: Vec<dbmeta::Trigger> = all
                        .into_iter()
                        .filter(|t| t.table.eq_ignore_ascii_case(&tab.table))
                        .collect();
                    let rows: Vec<Vec<Option<String>>> = mine
                        .iter()
                        .map(|t| {
                            vec![
                                Some(t.name.clone()),
                                Some(t.timing.clone()),
                                Some(t.event.clone()),
                            ]
                        })
                        .collect();
                    tab.triggers.set_data(dbgrid::GridData {
                        columns: vec![tr("触发器").into(), tr("时机").into(), tr("事件").into()],
                        types: vec![ColType::Text; 3],
                        rows,
                        pk: vec![],
                    });
                }
                (_, Err(e)) => {
                    tab.status.set_text(&trf!("读{v0}失败：{e}", e = e, v0 = part.label()));
                    set_status(ui, &trf!("读{v0}失败：{e}", e = e, v0 = part.label()), true);
                }
                _ => {}
            }
        }
        // 定义页签：把定义文本填进去（9.3）
        PendingReq::Def { tab_id } => {
            let Some(tab) = find_def_tab(ui, tab_id) else { return };
            match result {
                Ok(DbMetaPayload::Def(text)) => {
                    tab.text.buffer().set_text(&text);
                    tab.hl.apply();
                    let n = text.chars().count();
                    tab.status.set_text(&trf!("{n} 个字符", n = n));
                }
                Ok(_) => {}
                Err(e) => {
                    tab.status.set_text(&trf!("读定义失败：{e}", e = e));
                    set_status(ui, &trf!("读定义失败：{e}", e = e), true);
                }
            }
        }
        PendingReq::MetaForTable { tab_id } => {
            let Some(tab) = find_tab(ui, tab_id) else { return };
            match result {
                Ok(DbMetaPayload::Columns(cols)) => {
                    // 列类型 + 主键（供编辑时转字面量、定位行）
                    let types: Vec<ColType> = cols
                        .iter()
                        .map(|c| ColType::from_db_type(&c.data_type))

                        .collect();
                    let pk: Vec<usize> = cols
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.is_pk)
                        .map(|(i, _)| i)
                        .collect();
                    *tab.types.borrow_mut() = types;
                    *tab.pk.borrow_mut() = pk;
                    // 列名也存一份（**与 types 同序**）：条件条要靠它列选项，
                    // 条件里的值要靠它找对应类型（阶段 48）
                    *tab.col_names.borrow_mut() =
                        cols.iter().map(|c| c.name.clone()).collect();
                    // 列信息到手了 → 条件条可以用了（它要列清单）
                    fill_cond_bar(&tab);
                    // 列信息到手了 → 过滤条件编辑器可以用了（它要列清单）
                    tab.btn_filter.set_sensitive(true);
                    // 列显示设置也要列清单
                    // 列信息到手了，再查数据
                    let Some(target) = tab.target.borrow().clone() else {
                        return;
                    };
                    // 视图只能看，不能改（插入 / 删除行同样不行）
                    let editable = !target.is_view;
                    // 「只读 / 可编辑」标志跟着变（阶段 72c）
                    update_mode_chip(&tab);
                    tab.btn_edit.set_sensitive(editable);
                    tab.btn_insert.set_sensitive(editable);
                    tab.btn_delete.set_sensitive(editable);
                    query_table(st, ui, &tab, &target);
                }
                Ok(_) => {}
                Err(e) => {
                    tab.status.set_text(&trf!("读表结构失败：{e}", e = e));
                    set_status(ui, &trf!("读表结构失败：{e}", e = e), true);
                }
            }
        }
        _ => {}
    }
}

/// 写语句结果。
pub fn on_exec(st: &Rc<PanelState>, id: u64, result: Result<u64, String>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let pending = ui.pending.borrow_mut().remove(&id);
    // DDL 执行（表结构页签）与「执行定义」（定义页签）都不走保存流程
    match pending {
        Some(PendingReq::Ddl { tab_id }) => {
            ui.busy.set(false);
            update_buttons(ui);
            let ok = result.is_ok();
            let msg = match &result {
                Ok(_) => tr("DDL 已执行").to_string(),
                Err(e) => trf!("DDL 执行失败：{e}", e = e),
            };
            if let Some(tab) = find_struct_tab(ui, tab_id) {
                tab.status.set_text(&msg);
            }
            set_status(ui, &msg, !ok);
            return;
        }
        Some(PendingReq::StructSave { tab_id }) => {
            // 改表结构（阶段 14/15）：语句是一条一条发的（PG 的索引改动有多条），
            // 回包到了才发下一条；全部成功才刷新结构。
            let ok = result.is_ok();
            let err = result.err();
            let Some(tab) = find_struct_tab(ui, tab_id) else {
                ui.busy.set(false);
                update_buttons(ui);
                return;
            };
            if !ok {
                // 失败：剩下的不发了；事务里要回滚
                tab.sql_queue.borrow_mut().clear();
                let msg = trf!("改表结构失败：{v0}", v0 = err.unwrap_or_default());
                if tab.in_tx.get() {
                    tab.in_tx.set(false);
                    // PG 的事务一旦有语句失败就进入 aborted 状态，不显式
                    // ROLLBACK 的话，这个连接上的事务会一直挂着（后面的语句
                    // 全都失败）。
                    let id = next_id(ui);
                    ui.pending
                        .borrow_mut()
                        .insert(id, PendingReq::StructRollback { tab_id });
                    send_cmd(st, ui, WorkerCmd::DbExec { id, sql: "ROLLBACK".into() });
                }
                ui.busy.set(false);
                update_buttons(ui);
                tab.status.set_text(&msg);
                set_status(ui, &msg, true);
                // 存失败 ⇒ 这次关闭作废（改动还在页面上，用户能改）
                *ui.save_chain.borrow_mut() = None;
                super::closeguard::note_save_failed();
                return;
            }
            // 还有下一条 → 接着发
            if !tab.sql_queue.borrow().is_empty() {
                exec_next_sql(st, ui, &tab);
                return;
            }
            // 全部执行完 → 重新读一遍结构（DDL 也要跟着重拼）
            tab.in_tx.set(false);
            ui.busy.set(false);
            update_buttons(ui);
            let msg = tr("表结构已改").to_string();
            tab.status.set_text(&msg);
            set_status(ui, &msg, false);
            request_struct(st, ui, &tab, StructPart::Fields);
            request_struct(st, ui, &tab, StructPart::Indexes);
            request_struct(st, ui, &tab, StructPart::ForeignKeys);
            // 统一关闭流程的保存链：这张表改完了 ⇒ 改下一张（阶段 63）
            chain_step(st, ui);
            return;
        }
        Some(PendingReq::StructRollback { .. }) => {
            // 回滚完了，没什么好说的（错误信息已经报过了）
            ui.busy.set(false);
            update_buttons(ui);
            return;
        }
        Some(PendingReq::TreeDdl { what, schema }) => {
            ui.busy.set(false);
            update_buttons(ui);
            let ok = result.is_ok();
            let msg = match &result {
                Ok(_) => trf!("{what}，对象树已刷新", what = what),
                Err(e) => trf!("{what}失败：{e}", what = what, e = e),
            };
            set_status(ui, &msg, !ok);
            // 成功才刷新（失败刷新没意义）
            if ok {
                ui.tree.reload_schema(&schema);
            }
            return;
        }
        Some(PendingReq::Probe) => {
            ui.busy.set(false);
            update_buttons(ui);
            let ok = result.is_ok();
            let msg = match &result {
                Ok(n) => trf!("执行完成（{n} 行）", n = n),
                Err(e) => trf!("执行失败：{e}", e = e),
            };
            set_status(ui, &msg, !ok);
            return;
        }
        Some(PendingReq::DefExec { tab_id }) => {
            match result {
                Ok(_) => {
                    // 还有下一条就接着发（语句是一条一条发的）
                    if !ui.def_queue.borrow().is_empty() {
                        send_next_def(st, ui);
                        return;
                    }
                    ui.busy.set(false);
                    update_buttons(ui);
                    let job = ui.def_job.borrow_mut().take();
                    let msg = match &job {
                        Some(j) => trf!("定义已执行（{v0} 条语句）", v0 = j.total),
                        None => tr("定义已执行").to_string(),
                    };
                    if let Some(j) = &job {
                        if let Some(tab) = find_def_tab(ui, j.tab_id) {
                            tab.status.set_text(&msg);
                        }
                    }
                    set_status(ui, &msg, false);
                }
                Err(e) => {
                    // 中途失败：剩下的不发了。已经发出去的那几条是生效了的
                    // （MySQL 的 DDL 自动提交，PG 的 DROP 也已经生效），要说清楚。
                    let sent = ui.def_job.borrow().as_ref().map(|j| j.sent).unwrap_or(0);
                    ui.def_queue.borrow_mut().clear();
                    *ui.def_job.borrow_mut() = None;
                    ui.busy.set(false);
                    update_buttons(ui);
                    let msg = trf!("执行定义失败：{e}（已执行的 {sent} 条可能已经生效）", e = e, sent = sent);
                    if let Some(tab) = find_def_tab(ui, tab_id) {
                        tab.status.set_text(&msg);
                    }
                    set_status(ui, &msg, true);
                }
            }
            return;
        }
        _ => {}
    }
    if ui.save_id.get() != id {
        return;
    }
    match result {
        Ok(n) => {
            if let Some(job) = ui.save_job.borrow_mut().as_mut() {
                job.affected += n;
            }
            send_next_save(st, ui);
        }
        Err(e) => {
            // 一条失败 → 整个事务作废（PG 里失败的事务后续语句都报错）
            let tab_id = ui.save_job.borrow().as_ref().map(|j| j.tab_id).unwrap_or(0);
            *ui.save_job.borrow_mut() = None;
            ui.save_queue.borrow_mut().clear();
            ui.busy.set(false);
            let txid = next_id(ui);
            ui.pending.borrow_mut().insert(
                txid,
                PendingReq::Tx {
                    op: DbTxOp::Rollback,
                    tab_id,
                },
            );
            send_cmd(st, ui, WorkerCmd::DbTx { id: txid, op: DbTxOp::Rollback });
            set_status(ui, &trf!("保存失败，已回滚：{e}", e = e), true);
            update_buttons(ui);
        }
    }
}

/// 事务动作结果。
pub fn on_tx(st: &Rc<PanelState>, id: u64, op: DbTxOp, result: Result<(), String>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let tab_id = match ui.pending.borrow_mut().remove(&id) {
        Some(PendingReq::Tx { tab_id, .. }) => tab_id,
        _ => return,
    };
    let Some(tab) = find_tab(ui, tab_id) else { return };
    match op {
        DbTxOp::Begin => match result {
            Ok(()) => {
                ui.busy.set(false);
                update_buttons(ui);
                tab.editing.set(true);
                tab.in_tx.set(true);
                tab.grid.set_editable(true);
                set_edit_toolbar(&tab, true);
                update_foot(ui, &tab);
                // 事务开好了 → 执行之前排队的行操作（插入行 / 删除行）
                if let Some(action) = tab.pending_row_action.take() {
                    apply_row_action(ui, &tab, action);
                } else {
                    tab.status.set_text(tr("编辑中（改动未提交）"));
                    set_status(ui, tr("已进入编辑模式：改完点「保存修改」提交，或「放弃」回滚"), false);
                }
            }
            Err(e) => {
                ui.busy.set(false);
                update_buttons(ui);
                set_status(ui, &trf!("开启事务失败：{e}", e = e), true);
            }
        },
        DbTxOp::Commit => {
            ui.busy.set(false);
            match result {
                Ok(()) => {
                    tab.grid.apply_edits();
                    leave_edit_mode(&tab);
                    // 底栏的「N 项待保存」归零（阶段 45）
                    update_foot(ui, &tab);
                    let affected = ui.save_job.borrow().as_ref().map(|j| j.affected).unwrap_or(0);
                    *ui.save_job.borrow_mut() = None;
                    tab.status.set_text(&trf!("已提交（{affected} 行）", affected = affected));
                    set_status(ui, &trf!("已提交修改（{affected} 行）", affected = affected), false);
                    // 重新查一次表：**插入行的自增 id、没填的列的默认值，只有
                    // 数据库知道**（本地拼不出来）。不刷新的话用户看到的是
                    // 一行 NULL，不是刚插进去的那条记录。
                    refresh_table(st, ui, &tab);
                    // 统一关闭流程的保存链：这张表存完了 ⇒ 存下一张（阶段 63）
                    chain_step(st, ui);
                }
                Err(e) => {
                    leave_edit_mode(&tab);
                    set_status(ui, &trf!("提交失败：{e}", e = e), true);
                    // 存失败 ⇒ 这次关闭作废（数据还在页面上，用户能救）
                    *ui.save_chain.borrow_mut() = None;
                    super::closeguard::note_save_failed();
                }
            }
            update_buttons(ui);
        }
        DbTxOp::Rollback => {
            ui.busy.set(false);
            match result {
                Ok(()) => {
                    tab.grid.discard_edits();
                    leave_edit_mode(&tab);
                    // 底栏的「N 项待保存」归零（阶段 45）
                    update_foot(ui, &tab);
                    tab.status.set_text(tr("已放弃修改"));
                    set_status(ui, tr("已放弃修改（事务已回滚）"), false);
                }
                Err(e) => set_status(ui, &trf!("回滚失败：{e}", e = e), true),
            }
            update_buttons(ui);
        }
    }
}

/// 退出编辑模式（按钮状态复位）。
fn leave_edit_mode(tab: &Rc<ResultTab>) {
    tab.editing.set(false);
    tab.in_tx.set(false);
    tab.grid.set_editable(false);
    let editable = tab
        .target
        .borrow()
        .as_ref()
        .map(|t| !t.is_view)
        .unwrap_or(false);
    tab.btn_edit.set_sensitive(editable);
    tab.btn_insert.set_sensitive(editable);
    tab.btn_delete.set_sensitive(editable);
    set_edit_toolbar(tab, false);
}

/// 重新查一次表的数据（提交后刷新）。
///
/// 走的是"取列元信息 → 再查数据"那条路（和打开表时一样），
/// 所以列类型与主键也一并刷新了。
fn refresh_table(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    let Some(target) = tab.target.borrow().clone() else {
        return;
    };
    let id = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(id, PendingReq::MetaForTable { tab_id: tab.id });
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::Columns {
                schema: target.schema,
                table: target.table,
            },
        },
    );
}

/* ---------------- 过滤条件（9.5） ---------------- */

/// 应用过滤条件：生成 WHERE → 存起来 → 重新查这张表。
///
/// **为什么是服务端过滤**：本地筛选只对「已经取回的行」起作用（一页 200 行），
/// 表达不了 `id > 100 AND name LIKE …`——要按条件过滤就得让数据库查。
/// 条件生成失败（值与列类型不符）在这里就报中文原因，不发半条语句。
fn apply_filter(
    st: &Rc<PanelState>,
    ui: &DbUi,
    tab: &Rc<ResultTab>,
    conds: Vec<dbmeta::FilterCond>,
) -> Result<String, String> {
    let Some(target) = tab.target.borrow().clone() else {
        return Err(tr("这不是表数据页签，用不了数据库过滤条件").to_string());
    };
    let m = col_type_map(tab);
    let where_sql = dbmeta::build_where(current_proto(ui), &conds, &m)?;
    *tab.filter_conds.borrow_mut() = conds;
    *tab.filter_where.borrow_mut() = where_sql.clone();
    // 条件条上的 chip 跟着更新（弹窗里改的条件也要看得见）
    refresh_cond_chips(tab);
    query_table(st, ui, tab, &target);
    Ok(where_sql)
}

/// 按目标（表 + 当前过滤条件）查一次数据。
///
/// **翻页不用改**：下一页走的是 worker 侧同一个游标，WHERE 在初始查询里，
/// 后面几页自动带着。
fn query_table(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>, target: &TableTarget) {
    let where_sql = tab.filter_where.borrow().clone();
    // ⚠️ **ORDER BY 要跟 WHERE 一起拼进 SQL**（阶段 48）：SQL 的语法顺序是
    // `WHERE → ORDER BY → LIMIT/OFFSET`，写反了服务器直接报语法错。
    let order_by = tab.order_by.borrow().clone();
    let sql = dbmeta::build_select_table_page_order(
        current_proto(ui),
        &target.schema,
        &target.table,
        tab.page_size.get(),
        tab.offset.get(),
        &where_sql,
        &order_by,
    );
    ui.busy.set(true);
    update_buttons(ui);
    let qid = next_id(ui);
    ui.pending
        .borrow_mut()
        .insert(qid, PendingReq::Query { tab_id: tab.id });
    send_cmd(st, ui, WorkerCmd::DbQuery { id: qid, sql, page_size: PAGE_SIZE });
}

/* ---------------- 条件条（阶段 48） ---------------- */

/// 列名 → 类型（过滤条件的值要按类型转）。
///
/// ⚠️ **按"列名"配对，不按"第几列"**：列名与类型都取自**元数据**（同序），
/// 拿网格的显示位置去配就错位了——隐藏过几列以后，int 列会被当成 text 列，
/// 条件里的值就多套一层引号（PG 报类型错）。
fn col_type_map(tab: &Rc<ResultTab>) -> HashMap<String, ColType> {
    let mut out: HashMap<String, ColType> = HashMap::new();
    let names = tab.col_names.borrow();
    let types = tab.types.borrow();
    for (i, n) in names.iter().enumerate() {
        out.insert(n.clone(), types.get(i).copied().unwrap_or(ColType::Text));
    }
    if out.is_empty() {
        // 没有元数据（普通查询结果页签）→ 用网格自己的数据
        out = tab.grid.column_type_map();
    }
    out
}

/// 把列清单填进条件条的两个下拉（条件列 + 排序列）。
///
/// **一张表只填一次**：列信息是异步到的（`MetaForTable`），到了才知道
/// 有哪些列可选。重复填会把下拉弄乱，所以用 `cond_filled` 挡一下。
fn fill_cond_bar(tab: &Rc<ResultTab>) {
    if tab.cond_filled.get() {
        return;
    }
    let cols = tab.col_names.borrow().clone();
    if cols.is_empty() {
        return;
    }
    tab.cond_filled.set(true);
    let bar = &tab.cond_bar;
    for c in &cols {
        bar.col.append(Some(c.as_str()), c);
        bar.order_col.append(Some(c.as_str()), c);
    }
    bar.col.set_active_id(Some(cols.first().map(|s| s.as_str()).unwrap_or("")));
    // 排序列**默认不选**：不选 = 不排序（选了会跟用户预期的"原样"不一样）
    bar.order_col.set_active_id(None);
    // 条件条亮出来（只有表浏览页签有）
    bar.root.set_visible(true);
}

/// 刷新条件条上的 chip 行（每颗 chip = 一个已生效条件 + ✕ 移除）。
fn refresh_cond_chips(tab: &Rc<ResultTab>) {
    while let Some(child) = tab.cond_bar.chips.first_child() {
        tab.cond_bar.chips.remove(&child);
    }
    let conds = tab.filter_conds.borrow().clone();
    if conds.is_empty() {
        tab.cond_bar.chips.set_visible(false);
        return;
    }
    tab.cond_bar.chips.set_visible(true);
    for (i, c) in conds.iter().enumerate() {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("pp-cond-chip");
        let text = gtk::Label::new(Some(&cond_chip_text(c)));
        text.set_max_width_chars(28);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        chip.append(&text);
        // 移除按钮：点掉这一颗 chip，条件就没了（立刻重查）
        let del = crate::a11y::icon_button("window-close-symbolic");
        del.add_css_class("flat");
        {
            let img = del.child().and_downcast::<gtk::Image>();
            if let Some(img) = img {
                img.set_pixel_size(12);
            }
        }
        del.set_tooltip_text(Some(tr("去掉这个条件")));
        {
            // ⚠️ **抓弱引用**：chip 挂在页签上，回调若抓 `Rc<ResultTab>`
            // 就是 `ResultTab → chip → 回调 → ResultTab` 的环，页签永远
            // 释放不掉。
            let weak_tab = Rc::downgrade(tab);
            del.connect_clicked(move |_| {
                let Some(tab) = weak_tab.upgrade() else { return };
                remove_cond(&tab, i);
            });
        }
        chip.append(&del);
        tab.cond_bar.chips.append(&chip);
    }
}

/// 条件条上「+ 条件」：把当前三块读成一个条件，加进去并重查。
fn add_cond_from_bar(tab: &Rc<ResultTab>) {
    let Some(st) = tab.st.upgrade() else { return };
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let bar = &tab.cond_bar;
    let Some(column) = bar.col.active_id().map(|s| s.to_string()) else {
        set_status(ui, tr("先选一列再加条件"), true);
        return;
    };
    if column.trim().is_empty() {
        set_status(ui, tr("先选一列再加条件"), true);
        return;
    }
    let Some(op) = bar
        .op
        .active_id()
        .and_then(|s| dbmeta::FilterOp::from_label(s.as_str()))
    else {
        set_status(ui, tr("先选一个运算符"), true);
        return;
    };
    let value = bar.val.text().to_string();
    let join = match bar.join.active_id().map(|s| s.to_string()).unwrap_or_default().as_str() {
        "OR" => dbmeta::FilterJoin::Or,
        _ => dbmeta::FilterJoin::And,
    };
    let mut conds = tab.filter_conds.borrow().clone();
    conds.push(dbmeta::FilterCond {
        join,
        column: column.clone(),
        op,
        value: value.clone(),
    });
    match apply_filter(&st, ui, tab, conds) {
        Ok(_) => {
            bar.val.set_text("");
            refresh_cond_chips(tab);
            set_status(ui, &trf!("已加条件：{v0}", v0 = cond_chip_text(&dbmeta::FilterCond {
                join,
                column: column.clone(),
                op,
                value,
            })), false);
        }
        Err(e) => set_status(ui, &e, true),
    }
}

/// 去掉第 `idx` 个条件（chip 上的 ✕）并重查。
fn remove_cond(tab: &Rc<ResultTab>, idx: usize) {
    let Some(st) = tab.st.upgrade() else { return };
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let mut conds = tab.filter_conds.borrow().clone();
    if idx >= conds.len() {
        return;
    }
    let gone = conds.remove(idx);
    match apply_filter(&st, ui, tab, conds) {
        Ok(_) => {
            refresh_cond_chips(tab);
            set_status(ui, &trf!("已去掉条件：{v0}", v0 = cond_chip_text(&gone)), false);
        }
        Err(e) => set_status(ui, &e, true),
    }
}

/// 改排序（挑了排序列 / 点了方向按钮都走这里）。
fn set_order(tab: &Rc<ResultTab>, column: &str, asc: bool) {
    let Some(st) = tab.st.upgrade() else { return };
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(target) = tab.target.borrow().clone() else {
        set_status(ui, tr("这不是表数据页签，用不了排序"), true);
        return;
    };
    *tab.order_col.borrow_mut() = column.to_string();
    tab.order_asc.set(asc);
    let ob = dbmeta::build_order_by(current_proto(ui), column, asc);
    *tab.order_by.borrow_mut() = ob.clone();
    tab.cond_bar.btn_dir.set_label(order_dir_text(asc));
    // 排序变了 → 回到第一页重查（否则第 3 页的偏移对不上新顺序）
    tab.page.set(1);
    tab.offset.set(0);
    query_table(&st, ui, tab, &target);
    if ob.is_empty() {
        set_status(ui, tr("已去掉排序"), false);
    } else {
        set_status(ui, &trf!("排序：{ob}", ob = ob), false);
    }
}

/// 「清除」：清掉全部条件与排序，恢复查整张表。
fn clear_conds(tab: &Rc<ResultTab>) {
    let Some(st) = tab.st.upgrade() else { return };
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(target) = tab.target.borrow().clone() else {
        set_status(ui, tr("这不是表数据页签"), true);
        return;
    };
    *tab.filter_conds.borrow_mut() = Vec::new();
    *tab.filter_where.borrow_mut() = String::new();
    *tab.order_col.borrow_mut() = String::new();
    *tab.order_by.borrow_mut() = String::new();
    tab.order_asc.set(true);
    tab.cond_bar.order_col.set_active_id(None);
    tab.cond_bar.btn_dir.set_label(order_dir_text(true));
    tab.cond_bar.val.set_text("");
    refresh_cond_chips(tab);
    tab.page.set(1);
    tab.offset.set(0);
    query_table(&st, ui, tab, &target);
    set_status(ui, tr("已清掉全部条件与排序"), false);
}

/// 跳到第 `page` 页（阶段 45）。
///
/// **怎么跳的**：PG 的分页游标是 `NO SCROLL`（**不能回退**），所以回上一页
/// 只能**重新执行查询**——好在 SQL 里带上 `LIMIT n OFFSET m` 就行，PG 与
/// MySQL 通用（`dbmeta::build_select_table_page`）。
///
/// ⚠️ **只有表浏览页签能跳**：普通查询结果是用户手写的 SQL，往后面拼
/// `LIMIT/OFFSET` 可能跟语句里已有的 LIMIT 打架，所以那些页签只能往后翻。
fn goto_page(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>, page: usize) {
    let Some(target) = tab.target.borrow().clone() else {
        set_status(ui, tr("手写查询只能往后翻（要跳页请在表浏览页签里用）"), true);
        return;
    };
    let page = page.max(1);
    let size = tab.page_size.get().max(1);
    tab.offset.set((page - 1) * size);
    tab.page.set(page);
    query_table(st, ui, tab, &target);
}

/// 隐藏结果网格的某一列（阶段 46：列头「⋮」菜单里的「隐藏这一列」）。
///
/// **只影响显示**：待保存的改动、导出的 SQL 仍按**全部列**算。
///
/// 顺带**落盘**（`columns.ini`）——跟「列显示设置」对话框存的是同一份，
/// 所以下次打开这张表还是这个布局。
fn hide_result_column(tab: &Rc<ResultTab>, orig: usize) {
    let mut layout = tab.grid.column_layout();
    let mut changed = false;
    for (i, visible) in layout.iter_mut() {
        if *i == orig {
            *visible = false;
            changed = true;
        }
    }
    if !changed {
        return;
    }
    tab.grid.set_column_layout(layout.clone());
    // 持久化目标：**表浏览页签才有**（普通查询结果的列每次都不一样，存了没意义）
    let persist: Option<(String, String, String)> = tab.col_key.borrow().clone();
    if let Some((conn, schema, table)) = persist {
        // 按**列名**存（下标会变，名字不会）
        let names = tab.grid.columns();
        let specs: Vec<dbcols::ColSpec> = layout
            .iter()
            .filter_map(|(i, visible)| names.get(*i).map(|n| (n.clone(), *visible)))
            .collect();
        dbcols::save(&conn, &schema, &table, &specs);
    }
    let _ = tab;
}

/// 网格右键菜单的动作分派（阶段 47）。
///
/// 网格只管数据；弹窗口 / 弹对话框这些要 dbui 的上下文，所以在这里做。
fn handle_grid_menu(
    st: &Rc<PanelState>,
    ui: &DbUi,
    tab: &Rc<ResultTab>,
    action: dbgrid::GridMenuAction,
) {
    match action {
        dbgrid::GridMenuAction::BatchEdit { col } => open_batch_edit_dialog(st, ui, tab, col),
        dbgrid::GridMenuAction::CellDetail { row, col } => {
            open_grid_detail_window(st, ui, tab, tr("单元格详情"), &detail_cell_rows(tab, row, col))
        }
        dbgrid::GridMenuAction::RowDetail { row } => {
            open_grid_detail_window(st, ui, tab, tr("行详情"), &detail_row_rows(tab, row))
        }
        dbgrid::GridMenuAction::ColDetail { col } => {
            open_grid_detail_window(st, ui, tab, tr("列详情"), &detail_col_rows(tab, col))
        }
        dbgrid::GridMenuAction::Transpose => open_transpose_window(st, ui, tab),
    }
}

/// 「属性 | 值」两列（详情窗口用）。
fn detail_rows(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs
}

/// 单元格详情：列名 / 类型 / 当前值 / 改之前的值 / 是不是主键。
fn detail_cell_rows(tab: &Rc<ResultTab>, row: usize, col: usize) -> Vec<(String, String)> {
    let d = tab.grid.display_data();
    let name = d.columns.get(col).cloned().unwrap_or_default();
    let ty = d
        .types
        .get(col)
        .copied()
        .unwrap_or(dbmeta::ColType::Text)
        .label()
        .to_string();
    let value = tab.grid.cell_text(row, col);
    let orig = d
        .rows
        .get(row)
        .and_then(|r| r.get(col))
        .cloned()
        .flatten()
        .unwrap_or_else(|| "NULL".to_string());
    let is_pk = d.pk.contains(&col);
    detail_rows(vec![
        (tr("列名").to_string(), name),
        (tr("类型").to_string(), ty),
        (tr("当前值").to_string(), value),
        (tr("原始值").to_string(), orig),
        (tr("主键").to_string(), if is_pk { tr("是") } else { tr("否") }.to_string()),
    ])
}

/// 行详情：每一列一行（列名 | 值）。
fn detail_row_rows(tab: &Rc<ResultTab>, row: usize) -> Vec<(String, String)> {
    let d = tab.grid.display_data();
    let mut out = Vec::new();
    for (i, name) in d.columns.iter().enumerate() {
        out.push((name.clone(), tab.grid.cell_text(row, i)));
    }
    out
}

/// 列详情：列名 / 类型 / 主键 / 非空数 / 唯一值数 / 样例。
fn detail_col_rows(tab: &Rc<ResultTab>, col: usize) -> Vec<(String, String)> {
    let d = tab.grid.display_data();
    let name = d.columns.get(col).cloned().unwrap_or_default();
    let ty = d
        .types
        .get(col)
        .copied()
        .unwrap_or(dbmeta::ColType::Text)
        .label()
        .to_string();
    let mut nulls = 0usize;
    let mut uniq: Vec<String> = Vec::new();
    let mut sample = String::new();
    for (i, r) in d.rows.iter().enumerate() {
        let v = r.get(col).cloned().flatten();
        match v {
            None => nulls += 1,
            Some(s) => {
                if sample.is_empty() {
                    sample = s.clone();
                }
                if !uniq.contains(&s) {
                    uniq.push(s);
                }
            }
        }
        let _ = i;
    }
    let is_pk = d.pk.contains(&col);
    detail_rows(vec![
        (tr("列名").to_string(), name),
        (tr("类型").to_string(), ty),
        (tr("主键").to_string(), if is_pk { tr("是") } else { tr("否") }.to_string()),
        (tr("行数").to_string(), d.rows.len().to_string()),
        (tr("NULL 数").to_string(), nulls.to_string()),
        (tr("不同值个数").to_string(), uniq.len().to_string()),
        (tr("样例值").to_string(), sample),
    ])
}

/// 详情窗口（「属性 | 值」两列，独立窗口——跟「对象属性」一个路数）。
fn open_grid_detail_window(
    st: &Rc<PanelState>,
    ui: &DbUi,
    tab: &Rc<ResultTab>,
    title: &str,
    rows: &[(String, String)],
) {
    let _ = st;
    let win = gtk::Window::builder()
        .title(title)
        .transient_for(&ui.win)
        .default_width(420)
        .default_height(360)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    grid.set_hexpand(true);
    {
        let h1 = gtk::Label::new(Some(tr("属性")));
        h1.set_xalign(0.0);
        h1.add_css_class("dim-label");
        h1.add_css_class("heading");
        grid.attach(&h1, 0, 0, 1, 1);
        let h2 = gtk::Label::new(Some(tr("值")));
        h2.set_xalign(0.0);
        h2.add_css_class("dim-label");
        h2.add_css_class("heading");
        grid.attach(&h2, 1, 0, 1, 1);
    }
    *ui.last_detail.borrow_mut() = rows.to_vec();
    for (i, (k, v)) in rows.iter().enumerate() {
        let kl = gtk::Label::new(Some(k));
        kl.set_xalign(0.0);
        kl.add_css_class("dim-label");
        grid.attach(&kl, 0, (i + 1) as i32, 1, 1);
        let vl = gtk::Label::new(Some(v));
        vl.set_xalign(0.0);
        vl.set_selectable(true);
        vl.set_max_width_chars(40);
        vl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        grid.attach(&vl, 1, (i + 1) as i32, 1, 1);
    }
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    // ⚠️ **必须 vexpand**：不撑的话窗口只按内容高度占位，行多了就看不见
    sw.set_vexpand(true);
    sw.set_child(Some(&grid));
    root.append(&sw);
    win.set_child(Some(&root));
    win.present();
    let _ = tab;
}

/// 「转置查看」窗口（dbx 的同名功能）：行列互换，纯本地算。
fn open_transpose_window(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    let d = tab.grid.display_data();
    if d.rows.is_empty() || d.columns.is_empty() {
        return;
    }
    let t = dbgrid::transpose(&d);
    let grid = dbgrid::DbGrid::new();
    grid.set_data(t);
    let win = gtk::Window::builder()
        .title(tr("转置查看"))
        .transient_for(&ui.win)
        .default_width(760)
        .default_height(420)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(10);
    root.set_margin_end(10);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    let hint = gtk::Label::new(Some(tr("转置查看：原表的每一行变成一列，只读，不影响数据。")));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_vexpand(true);
    sw.set_child(Some(&grid.view));
    root.append(&sw);
    win.set_child(Some(&root));
    win.present();
    let _ = st;
}

/// 「批量修改所选…」对话框（阶段 47：多选的价值所在）。
///
/// 一次改**所有选中行**的某一列。
fn open_batch_edit_dialog(
    st: &Rc<PanelState>,
    ui: &DbUi,
    tab: &Rc<ResultTab>,
    col: usize,
) {
    let d = tab.grid.display_data();
    if d.columns.is_empty() {
        return;
    }
    // 选中的行**先快照**：对话框开着的时候网格可能变（用户切页签等）
    let rows = tab.grid.selected_display_rows();
    if rows.is_empty() {
        set_status(ui, tr("先选中要改的行"), true);
        return;
    }
    let dlg = gtk::Dialog::with_buttons(
        Some(tr("批量修改所选")),
        Some(&ui.win),
        gtk::DialogFlags::MODAL,
        &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("应用"), gtk::ResponseType::Ok),
        ],
    );
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    let hint = gtk::Label::new(Some(&trf!("把选中的 {v0} 行一起改掉（改完点「保存修改」才写库）。", v0 = rows.len())));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let col_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let l1 = gtk::Label::new(Some(tr("列")));
    col_box.append(&l1);
    let col_sel = gtk::ComboBoxText::new();
    for name in d.columns.iter() {
        col_sel.append_text(name);
    }
    col_sel.set_active(Some(col as u32));
    col_sel.set_hexpand(true);
    col_box.append(&col_sel);
    root.append(&col_box);

    let val_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let l2 = gtk::Label::new(Some(tr("新值")));
    val_box.append(&l2);
    let val = gtk::Entry::new();
    val.set_placeholder_text(Some(tr("留空 = 空字符串；填 NULL = 设为空值")));
    val.set_hexpand(true);
    val_box.append(&val);
    root.append(&val_box);

    dlg.content_area().append(&root);
    dlg.present();

    let grid = Rc::clone(&tab.grid);
    dlg.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Ok {
            let c = col_sel.active().unwrap_or(0) as usize;
            let text = val.text().to_string();
            let n = grid.set_edit_rows(&rows, c, &text);
            let msg = trf!("已修改 {n} 行，点「保存修改」才写库", n = n);
            grid.set_status(&msg);
        }
        d.close();
    });
    let _ = st;
}

/// 刷新结果页签底栏（阶段 45）：行数 / 耗时 / 待保存项数 / 页码 / 翻页按钮。
///
/// **dbx 的底栏信息量**：`共 N 行 | 耗时 | N 项待保存 | … | 每页 N 行 | 分页`。
/// 我们原先只有工具栏里一颗「下一页」，其余全缺——用户看不出"这一页取了多少
/// 行""还有没有下一页""我改的东西存了没"。
fn update_foot(_ui: &DbUi, tab: &Rc<ResultTab>) {
    let page = tab.page.get();
    let pending = tab.grid.pending_edits().len()
        + tab.grid.pending_inserts().len()
        + tab.grid.pending_deletes().len();
    let mut info = trf!("{v0} 行 · {v1} ms", v0 = tab.last_rows.get(), v1 = tab.last_elapsed.get());
    if pending > 0 {
        info.push_str(&trf!(" · {pending} 项待保存", pending = pending));
    }
    tab.foot_info.set_text(&info);
    tab.foot_page.set_text(&trf!("第 {page} 页", page = page));
    // 首页 / 上一页：只有表浏览页签能跳（手写查询不能重查）
    let can_jump = tab.target.borrow().is_some();
    tab.btn_first.set_sensitive(can_jump && page > 1);
    tab.btn_prev.set_sensitive(can_jump && page > 1);
}

/// 过滤条件编辑器的一行。
struct CondRow {
    /// 与前一条的连接词（第一行用不上）
    join: gtk::ComboBoxText,
    col: gtk::ComboBoxText,
    op: gtk::ComboBoxText,
    val: gtk::Entry,
}

/// 打开过滤条件编辑器。
///
/// 一行一个条件：`[列] [并且/或者] [操作符] [值]`，底部「添加条件 / 应用 /
/// 清除 / 取消」。跟 Navicat 的条件编辑器一个路数，但只做最常用的一层
/// （不做嵌套分组——那玩意儿表达力强，但一行一条已经能覆盖绝大多数筛选）。
fn open_filter_dialog(st: &Rc<PanelState>, ui: &DbUi, tab: &Rc<ResultTab>) {
    let cols: Vec<String> = tab
        .grid
        .view
        .columns()
        .iter()
        .map(|c| c.title().to_string())
        .collect();
    if cols.is_empty() {
        set_status(ui, tr("这张表还没有列信息，过滤条件用不了"), true);
        return;
    }

    let dlg = gtk::Window::builder()
        .title(tr("数据库过滤条件"))
        .transient_for(&tab.win)
        .modal(true)
        .default_width(620)
        .default_height(320)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint = gtk::Label::new(Some(
        tr("条件会拼进 WHERE 重新查库（服务端过滤，不只是筛当前这一页）。\n\
         上面那个「筛选已加载行」框只筛已经取回的行，两者是两回事。"),
    ));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let sc = gtk::ScrolledWindow::new();
    sc.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sc.set_vexpand(true);
    sc.set_child(Some(&rows_box));
    root.append(&sc);

    let err = gtk::Label::new(Some(""));
    err.add_css_class("error");
    err.set_xalign(0.0);
    err.set_ellipsize(gtk::pango::EllipsizeMode::End);
    root.append(&err);

    let rows: Rc<RefCell<Vec<CondRow>>> = Rc::new(RefCell::new(Vec::new()));

    // 加一行条件
    {
        // 闭包自己抓一份 rows；外层还要用它给第一行置灰
        let rows_in = Rc::clone(&rows);
        let cols = cols.clone();
        let rows_box = rows_box.clone();
        let add_row = move |cur: Option<dbmeta::FilterCond>| {
            let join = gtk::ComboBoxText::new();
            join.append(Some("AND"), tr("并且"));
            join.append(Some("OR"), tr("或者"));
            join.set_active_id(Some("AND"));
            join.set_tooltip_text(Some(tr("与上一条条件的连接方式")));
            let col = gtk::ComboBoxText::new();
            for c in &cols {
                col.append(Some(c.as_str()), c);
            }
            let op = gtk::ComboBoxText::new();
            for o in dbmeta::FilterOp::all() {
                op.append(Some(o.label()), o.label());
            }
            op.set_active_id(Some("="));
            let val = gtk::Entry::new();
            val.set_hexpand(true);
            val.set_placeholder_text(Some(tr("值")));

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&join);
            row.append(&col);
            row.append(&op);
            row.append(&val);

            if let Some(c) = cur {
                join.set_active_id(Some(match c.join {
                    dbmeta::FilterJoin::And => "AND",
                    dbmeta::FilterJoin::Or => "OR",
                }));
                col.set_active_id(Some(c.column.as_str()));
                op.set_active_id(Some(c.op.label()));
                val.set_text(&c.value);
            } else {
                col.set_active_id(Some(cols.first().map(|s| s.as_str()).unwrap_or("")));
            }

            rows_box.append(&row);
            rows_in.borrow_mut().push(CondRow {
                join,
                col,
                op,
                val,
            });
        };
        // 已有条件先摆出来（能改也能看）
        for c in tab.filter_conds.borrow().iter() {
            add_row(Some(c.clone()));
        }
        if tab.filter_conds.borrow().is_empty() {
            add_row(None);
        }
        // 第一行的连接词没有意义（前面没条件可连）→ 置灰
        if let Some(first) = rows.borrow().first() {
            first.join.set_sensitive(false);
        }
        // 「+ 添加条件」
        {
            let add = gtk::Button::with_label(tr("+ 添加条件"));
            // 不占满整行宽度
            add.set_halign(gtk::Align::Start);
            add.connect_clicked({
                let add_row = add_row.clone();
                move |_| add_row(None)
            });
            root.append(&add);
        }
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    let btn_apply = gtk::Button::with_label(tr("应用"));
    btn_apply.add_css_class("suggested-action");
    let btn_clear = gtk::Button::with_label(tr("清除条件"));
    let btn_cancel = gtk::Button::with_label(tr("取消"));
    actions.append(&btn_clear);
    actions.append(&btn_cancel);
    actions.append(&btn_apply);
    root.append(&actions);

    dlg.set_child(Some(&root));

    // 收集当前界面上的条件
    let collect = {
        let rows = Rc::clone(&rows);
        move || -> Vec<dbmeta::FilterCond> {
            rows.borrow()
                .iter()
                .filter_map(|r| {
                    let column = r.col.active_id()?.to_string();
                    if column.trim().is_empty() {
                        return None;
                    }
                    let op = r
                        .op
                        .active_id()
                        .and_then(|s| dbmeta::FilterOp::from_label(s.as_str()))?;
                    let join = match r.join.active_id().map(|s| s.to_string()).unwrap_or_default().as_str() {
                        "OR" => dbmeta::FilterJoin::Or,
                        _ => dbmeta::FilterJoin::And,
                    };
                    Some(dbmeta::FilterCond {
                        join,
                        column,
                        op,
                        value: r.val.text().to_string(),
                    })
                })
                .collect()
        }
    };

    // 应用
    {
        let st2 = Rc::clone(st);
        let tab2 = Rc::clone(tab);
        let collect = collect.clone();
        let err = err.clone();
        let dlg2 = dlg.clone();
        btn_apply.connect_clicked(move |_| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let conds = collect();
            match apply_filter(&st2, ui, &tab2, conds) {
                Ok(w) => {
                    err.set_text("");
                    let msg = if w.is_empty() {
                        tr("已清除数据库过滤条件").to_string()
                    } else {
                        trf!("已按条件重查：{w}", w = w)
                    };
                    set_status(ui, &msg, false);
                    tab2.status.set_text(&msg);
                    dlg2.close();
                }
                Err(e) => err.set_text(&e),
            }
        });
    }
    // 清除
    {
        let st2 = Rc::clone(st);
        let tab2 = Rc::clone(tab);
        let dlg2 = dlg.clone();
        btn_clear.connect_clicked(move |_| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            match apply_filter(&st2, ui, &tab2, Vec::new()) {
                Ok(_) => {
                    set_status(ui, tr("已清除数据库过滤条件"), false);
                    dlg2.close();
                }
                Err(e) => err.set_text(&e),
            }
        });
    }
    // 取消
    {
        let dlg = dlg.clone();
        btn_cancel.connect_clicked(move |_| dlg.close());
    }
    dlg.present();
}

/* ---------------- 对象树上的 DDL（9.6：新建表 / 改名 / 删表） ---------------- */

/// 执行一条 DDL（建表 / 改名 / 删表），执行完刷新那个模式节点。
///
/// **走 worker 的写通道**（不是本地拼字符串直接发）：跟其它写操作一样有
/// 状态反馈和错误提示，失败时状态栏直接显示数据库给的原因。
fn exec_tree_ddl(st: &Rc<PanelState>, ui: &DbUi, sql: &str, what: &str, schema: &str) {
    ui.busy.set(true);
    update_buttons(ui);
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(
        id,
        PendingReq::TreeDdl {
            what: what.to_string(),
            schema: schema.to_string(),
        },
    );
    send_cmd(st, ui, WorkerCmd::DbExec { id, sql: sql.to_string() });
}

/// 新建表：填表名 + 列定义（每行「名称 类型」），然后建。
fn open_create_table_dialog(st: &Rc<PanelState>, ui: &DbUi, schema: &str) {
    let dlg = gtk::Window::builder()
        .title(tr("新建表"))
        .transient_for(&ui.win)
        .modal(true)
        .default_width(520)
        .default_height(380)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint_text = trf!("在模式「{schema}」下新建表。", schema = schema);
    let hint = gtk::Label::new(Some(&hint_text));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let name = gtk::Entry::new();
    name.set_placeholder_text(Some(tr("表名")));
    name.set_text("new_table");
    root.append(&name);

    let cols_label = gtk::Label::new(Some(tr("列（每行一个：名称 类型）")));
    cols_label.set_xalign(0.0);
    root.append(&cols_label);
    let cols = gtk::TextView::new();
    let cols_buf = cols.buffer();
    cols_buf.set_text("id integer\nname text");
    let cols_sw = gtk::ScrolledWindow::new();
    cols_sw.set_vexpand(true);
    cols_sw.set_child(Some(&cols));
    root.append(&cols_sw);

    let err = gtk::Label::new(Some(""));
    err.add_css_class("error");
    err.set_xalign(0.0);
    err.set_ellipsize(gtk::pango::EllipsizeMode::End);
    root.append(&err);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    let btn_ok = gtk::Button::with_label(tr("创建"));
    btn_ok.add_css_class("suggested-action");
    let btn_cancel = gtk::Button::with_label(tr("取消"));
    actions.append(&btn_cancel);
    actions.append(&btn_ok);
    root.append(&actions);
    dlg.set_child(Some(&root));

    {
        let st2 = Rc::clone(st);
        let schema = schema.to_string();
        let dlg2 = dlg.clone();
        btn_ok.connect_clicked(move |_| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let tname = name.text().trim().to_string();
            if tname.is_empty() {
                err.set_text(tr("表名不能为空"));
                return;
            }
            if !dbmeta::is_simple_ident(&tname) {
                err.set_text(tr("表名只能用字母、数字、下划线，且不能以数字开头"));
                return;
            }
            let text = cols_buf.text(&cols_buf.start_iter(), &cols_buf.end_iter(), false).to_string();
            let defs = match dbmeta::parse_ddl_columns(&text) {
                Ok(d) => d,
                Err(e) => {
                    err.set_text(&e);
                    return;
                }
            };
            let sql = dbmeta::build_create_table(current_proto(ui), &schema, &tname, &defs);
            exec_tree_ddl(&st2, ui, &sql, &trf!("已新建表 {tname}", tname = tname), &schema);
            dlg2.close();
        });
    }
    {
        let dlg2 = dlg.clone();
        btn_cancel.connect_clicked(move |_| dlg2.close());
    }
    dlg.present();
}

/// 重命名表：填新名字。
fn open_rename_table_dialog(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str) {
    let dlg = gtk::Window::builder()
        .title(tr("重命名表"))
        .transient_for(&ui.win)
        .modal(true)
        .default_width(420)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint_text = trf!("把表「{table}」改成新名字。", table = table);
    let hint = gtk::Label::new(Some(&hint_text));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    root.append(&hint);

    let name = gtk::Entry::new();
    name.set_text(table);
    root.append(&name);

    let err = gtk::Label::new(Some(""));
    err.add_css_class("error");
    err.set_xalign(0.0);
    root.append(&err);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::End);
    let btn_ok = gtk::Button::with_label(tr("改名"));
    btn_ok.add_css_class("suggested-action");
    let btn_cancel = gtk::Button::with_label(tr("取消"));
    actions.append(&btn_cancel);
    actions.append(&btn_ok);
    root.append(&actions);
    dlg.set_child(Some(&root));

    {
        let st2 = Rc::clone(st);
        let schema = schema.to_string();
        let table = table.to_string();
        let dlg2 = dlg.clone();
        btn_ok.connect_clicked(move |_| {
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let new_name = name.text().trim().to_string();
            if new_name.is_empty() {
                err.set_text(tr("新名字不能为空"));
                return;
            }
            if !dbmeta::is_simple_ident(&new_name) {
                err.set_text(tr("表名只能用字母、数字、下划线，且不能以数字开头"));
                return;
            }
            if new_name == table {
                err.set_text(tr("新名字跟原来一样"));
                return;
            }
            let sql = dbmeta::build_rename_table(current_proto(ui), &schema, &table, &new_name);
            exec_tree_ddl(
                &st2,
                ui,
                &sql,
                &trf!("已把 {table} 改名为 {new_name}", table = table, new_name = new_name),
                &schema,
            );
            dlg2.close();
        });
    }
    {
        let dlg2 = dlg.clone();
        btn_cancel.connect_clicked(move |_| dlg2.close());
    }
    dlg.present();
}

/// 删表：**二次确认**（不可逆），确认后才发 DROP。
fn drop_table_confirm(st: &Rc<PanelState>, ui: &DbUi, schema: &str, table: &str) {
    let st2 = Rc::clone(st);
    let schema = schema.to_string();
    let table = table.to_string();
    crate::panel::dialogs::confirm_with(
        Some(&ui.win),
        &trf!("删除表「{table}」？\n表里的数据会一起没了，撤不回来。", table = table),
        tr("删除"),
        move |ok| {
            if !ok {
                return;
            }
            let slot = st2.db_ui.borrow();
            let Some(ui) = slot.as_ref() else { return };
            let sql = dbmeta::build_drop_table(current_proto(ui), &schema, &table);
            exec_tree_ddl(&st2, ui, &sql, &trf!("已删除表 {table}", table = table), &schema);
        },
    );
}

/// 编辑模式切换工具栏：不在编辑模式时显示「编辑 / 插入行 / 删除行」，
/// 在编辑模式时显示「保存修改 / 放弃」。
///
/// **为什么要切换而不是全摆着**：按钮一多工具栏就放不下——窗口右侧会被
/// 截断（截图里「删除行 / 保存修改 / 放弃」全看不见）。而这几颗按钮本来
/// 就是**互斥**的两种状态：没进编辑模式时不能保存，进编辑模式后不需要
/// 再点「编辑」。
fn set_edit_toolbar(tab: &Rc<ResultTab>, editing: bool) {
    let vis = edit_toolbar_visible(editing);
    tab.btn_edit.set_visible(vis[0]);
    tab.btn_insert.set_visible(vis[1]);
    tab.btn_delete.set_visible(vis[2]);
    tab.btn_save.set_visible(vis[3]);
    tab.btn_discard.set_visible(vis[4]);
    // ⚠️ **可见 ≠ 可点**（用户 2026-10-28：「提交和回滚按钮都是禁用的，
    // 改了数据之后也点不了」）。这两个按钮建出来时是 `sensitive(false)`
    // （没进编辑模式时不该能点），进编辑模式**必须把 sensitive 打开**——
    // 否则用户看到的是两个**灰**按钮，点了没反应，界面还不报错。
    let sen = save_discard_sensitive(editing);
    tab.btn_save.set_sensitive(sen[0]);
    tab.btn_discard.set_sensitive(sen[1]);
}

/// 编辑模式工具栏的「可见」状态，顺序：编辑 / 插入行 / 删除行 / 保存修改 / 放弃修改。
///
/// 抽成纯函数是为了能单测钉住这套语义（可见 ≠ 可点 那条坑）。
fn edit_toolbar_visible(editing: bool) -> [bool; 5] {
    [!editing, !editing, !editing, editing, editing]
}

/// 「保存修改 / 放弃修改」两个按钮的「可点」状态。
///
/// ⚠️ **别去动「编辑 / 插入行 / 删除行」的 sensitive**：那三个能不能点取决于
/// "当前目标是不是可编辑的表"（视图表不能编辑），由别处按 `editable` 设，
/// 在这里覆盖掉就会让视图表也能进编辑模式。
fn save_discard_sensitive(editing: bool) -> [bool; 2] {
    [editing, editing]
}

/// 找页签。
fn find_tab(ui: &DbUi, id: u64) -> Option<Rc<ResultTab>> {
    ui.tabs.borrow().iter().find(|t| t.id == id).cloned()
}

/// 清空所有页签。
fn clear_tabs(ui: &DbUi) {
    while ui.notebook.n_pages() > 0 {
        ui.notebook.remove_page(Some(0));
    }
    ui.stack.set_visible_child_name("empty");
    ui.tabs.borrow_mut().clear();
    // ⚠️ **表文档也要清**（阶段 72e）：文档页已经跟着 notebook 一起摘掉了，
    // 但 `ui.docs` 里还留着它们 ⇒ 断线重连后 `find_doc` 会找到**已经死掉的**
    // 文档，用户点表什么反应都没有。
    ui.docs.borrow_mut().clear();
    ui.struct_tabs.borrow_mut().clear();
    ui.def_tabs.borrow_mut().clear();
    ui.pending.borrow_mut().clear();
    ui.save_queue.borrow_mut().clear();
    *ui.save_job.borrow_mut() = None;
    ui.def_queue.borrow_mut().clear();
    *ui.def_job.borrow_mut() = None;
}

/// 主题切换后重画 SQL 高亮的颜色。
pub fn refresh_theme_colors(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.sql_hl.refresh_colors();
}

/// 对象浏览器的 TreeView（冒烟测试要看树里有没有行）。
pub fn tree_view(st: &Rc<PanelState>) -> Option<gtk::TreeView> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    Some(ui.tree.view.clone())
}

/// 对象树的**显示**状态（诊断用）。
///
/// **为什么单列一个钩子**：模型里有节点 ≠ 用户屏幕上看得见。
/// GTK4 里父行一旦折叠，子节点在模型里躺着，屏幕上却一行都不显示——
/// 只看**模型**行数会误判成"数据加载好了"（之前 e2e 就是这么查的，
/// 所以这个显示问题一直没被发现）。
///
/// 这里报四样东西：模型总行数 / 根行子节点数 / **视图里可见的行范围**
/// （`visible_range` 才是"屏幕上真画出来了"的证据）/ 树控件高度。
pub fn tree_state(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return vec![tr("没有数据库工作台").into()];
    };
    let view = &ui.tree.view;
    let Some(model) = view.model() else {
        return vec![tr("树没有 model").into()];
    };
    let Some(root) = model.iter_first() else {
        return vec![tr("模型是空的").into()];
    };
    // 模型里一共多少行（递归）
    fn walk(model: &gtk::TreeModel, iter: &gtk::TreeIter, n: &mut usize) {
        *n += 1;
        if let Some(mut c) = model.iter_children(Some(iter)) {
            loop {
                walk(model, &c, n);
                if !model.iter_next(&mut c) {
                    break;
                }
            }
        }
    }
    let mut total = 0usize;
    walk(&model, &root, &mut total);
    // 根行有几个子节点
    let mut kids = 0usize;
    if let Some(mut c) = model.iter_children(Some(&root)) {
        loop {
            kids += 1;
            if !model.iter_next(&mut c) {
                break;
            }
        }
    }
    let mut out = vec![trf!("模型 {total} 行", total = total), trf!("根行子节点 {kids} 个", kids = kids)];
    // **可见范围**：屏幕上真的画出来的行（模型里有但没画出来的不算）
    match view.visible_range() {
        Some((first, last)) => {
            let f: Vec<String> = first.indices().iter().map(|i| i.to_string()).collect();
            let l: Vec<String> = last.indices().iter().map(|i| i.to_string()).collect();
            out.push(trf!("可见行 {v0}..{v1}", v0 = f.join("."), v1 = l.join(".")));
        }
        None => out.push(tr("可见行：无").into()),
    }
    out.push(trf!("树控件高 {v0}px", v0 = view.allocated_height()));
    out
}

/// 对象树**屏幕上真画出来**的最深一行（返回路径深度；根行 = 0）。
///
/// **为什么要有这个钩子**：2026-10 用户报「连上数据库后左侧只显示一行」，
/// 根因是对象树那个 `ScrolledWindow` 没设 `set_vexpand(true)`——滚动窗口
/// 只按**内容高度**占位，树里当时只有一行，整个控件就缩成一行高，
/// 后面挂上来的子节点全被裁掉了（模型里有，屏幕上没有）。
///
/// 只查**模型**行数是查不出来的：模型里明明有「public」这一行，
/// e2e 之前一直断言"模型 ≥ 2 行"，所以这个 bug 一直没被发现。
/// 这个钩子查的是**视图**（`visible_range`），模型里有 ≠ 屏幕上看得见。
pub fn tree_visible_depth(st: &Rc<PanelState>) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let view = &ui.tree.view;
    let (_, last) = view.visible_range()?;
    Some(last.indices().len().saturating_sub(1))
}

/// 冒烟测试用：设定「当前连接」（**不落盘**——测试不该往用户配置里塞连接）。
///
/// 阶段 13 之后工作台里没有连接表单了，测试要连库就直接给一条临时的连接。
pub fn set_conn_now(
    st: &Rc<PanelState>,
    proto: &str,
    host: &str,
    port: &str,
    user: &str,
    dbname: &str,
    pass: &str,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let p = DbProfile {
        name: format!("{} {}:{}/{}", proto, host, port, dbname),
        proto: DbProto::from_str(proto).unwrap_or(DbProto::Postgres),
        host: host.to_string(),
        port: port.parse().unwrap_or(0),
        user: user.to_string(),
        dbname: dbname.to_string(),
        remember: !pass.is_empty(),
    };
    let conn_name = p.name.clone();
    *ui.current.borrow_mut() = Some(p);
    ui.tree.set_proto(DbProto::from_str(proto).unwrap_or(DbProto::Postgres));
    // 默认模式自动展开（阶段 72d）
    ui.tree.set_default_schema(default_schema_of(ui));
    // 页签头写上连接名（工作台里不再有连接信息那一行）
    update_tab_title(st, ui);
    // 密码：临时连接不落凭据库，直接塞进一个「一次性」的口袋里
    // （按连接名存，见 `PanelState.db_pass` 的注释）
    if !pass.is_empty() {
        st.db_pass
            .borrow_mut()
            .insert(conn_name, zeroize::Zeroizing::new(pass.to_string()));
    }
}

/// 冒烟测试用：等价于点「连接」按钮。
pub fn connect_now(st: &Rc<PanelState>) {
    let err = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        do_connect(st, ui)
    };
    // 借用放掉了再弹窗
    if let Some(msg) = err {
        report_unmounted(st, &msg, true);
    }
}

/// 冒烟测试用：给某个表页签设置过滤条件（走与 UI「应用」**同一条路径**）。
///
/// 条件用 `(列, 操作符, 值)` 表示，操作符是显示文字（`=` / `>` / `LIKE` …）。
pub fn apply_filter_now(
    st: &Rc<PanelState>,
    tab_idx: usize,
    conds: &[(String, String, String)],
) -> Result<String, String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return Err(tr("数据库窗口没开").to_string());
    };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return Err(tr("没有这个结果页签").to_string());
    };
    let list: Vec<dbmeta::FilterCond> = conds
        .iter()
        .filter_map(|(c, o, v)| {
            Some(dbmeta::FilterCond {
                join: dbmeta::FilterJoin::And,
                column: c.clone(),
                op: dbmeta::FilterOp::from_label(o)?,
                value: v.clone(),
            })
        })
        .collect();
    apply_filter(st, ui, &tab, list)
}

/// 冒烟测试用：打开过滤条件编辑器（截图用）。
pub fn open_filter_dialog_now(st: &Rc<PanelState>, tab_idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    open_filter_dialog(st, ui, &tab);
    true
}

/// 冒烟测试用：读某个表页签当前的 WHERE 文本。
pub fn filter_where_now(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let tabs = ui.tabs.borrow();
    let Some(tab) = tabs.get(tab_idx) else {
        return String::new();
    };
    tab.filter_where.borrow().clone()
}

/// 冒烟测试用：读「当前连接」的名字（阶段 13：工作台里只有这一行）。
pub fn current_conn_name(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.current.borrow().as_ref().map(|p| p.name.clone()).unwrap_or_default()
}

/* ---------------- 阶段 51：空态 + 树面板的测试钩子 ---------------- */

/// 对象树本体（阶段 51 的冒烟要往树上灌假数据，验过滤与详细模式）。
pub fn tree(st: &Rc<PanelState>) -> Option<Rc<dbtree::DbTree>> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    Some(Rc::clone(&ui.tree))
}

/// 空态那一屏上的所有文字（阶段 51）。
pub fn empty_state_text(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    collect_widget_text(ui.empty_box.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 空态卡上每颗按钮的文字 + 能不能点（阶段 80 的冒烟用）。
///
/// 审计 P2-1：没连上数据库时，「新建查询」「搜索对象」**不可用却仍可点**
/// ⇒ 用户点了才发现没反应。这个钩子让冒烟能直接查敏感状态。
pub fn empty_state_buttons(st: &Rc<PanelState>) -> Vec<(String, bool)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    collect_buttons(ui.empty_box.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 冒烟 / 截图用：把空态**摆成"已经连上"的样子**（阶段 97）。
///
/// 为什么需要这个钩子：空态有两种长相（**没连上** / **连上了**），
/// 而"连上了"那一版只有真连上库才会出现 ⇒ 截图工具没法离线看它。
/// ⚠️ **只改界面状态**，不真的连库（树是空的），但**版面是真的**。
pub fn empty_state_set_connected(st: &Rc<PanelState>, conn_name: &str) -> bool {
    let key = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return false };
        ui.session_key.clone()
    };
    let Some(p) = dbstore::get(&key, conn_name) else {
        return false;
    };
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.connected.set(true);
    // ⚠️ **借用要立刻结束**：下一行 `refresh_empty` 里还会 `borrow()` 它。
    *ui.current.borrow_mut() = Some(p);
    refresh_empty(st, ui);
    true
}

/// 控件树里**第一个** Label 的文字（自绘按钮取文字用）。
fn first_label_text(w: &gtk::Widget) -> String {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return l.text().to_string();
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        let t = first_label_text(&x);
        if !t.is_empty() {
            return t;
        }
        c = x.next_sibling();
    }
    String::new()
}

fn collect_buttons(w: &gtk::Widget, out: &mut Vec<(String, bool)>) {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        let txt = b.label().map(|s| s.to_string()).unwrap_or_else(|| {
            // 自绘按钮（图标 + 文字）：**递归**去子控件里找 Label
            // ⚠️ 必须递归（阶段 96）：空态的行是「Button → Box → Label」
            // （中间隔了一层 Box）⇒ 只看**直接子控件**就找不到 ⇒ 文字变空串。
            first_label_text(b.upcast_ref::<gtk::Widget>())
        });
        out.push((txt, b.is_sensitive()));
        return;
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        collect_buttons(&x, out);
        c = x.next_sibling();
    }
}

/// 把一个控件树里的文字按顺序收集出来（Label 取文字，Button 取它的标签）。
fn collect_widget_text(w: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push(l.text().to_string());
        return;
    }
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        if let Some(l) = b.child().and_then(|x| x.downcast_ref::<gtk::Label>().cloned()) {
            out.push(l.text().to_string());
            return;
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        collect_widget_text(&c, out);
        child = c.next_sibling();
    }
}

/// 树面板当前的类型过滤（返回下拉的 id）。
pub fn tree_filter_id(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.filter_combo.active_id().unwrap_or_default().to_string()
}

/// 树面板「详细」开关当前的状态。
pub fn tree_detail_mode(st: &Rc<PanelState>) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.detail_check.is_active()
}

/// 冒烟测试用：**走真实控件**改类型过滤（`set_active_id` 会触发
/// `connect_changed` → 树换过滤 → 重建）。
pub fn tree_set_filter_now(st: &Rc<PanelState>, id: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.filter_combo.set_active_id(Some(id));
}

/// 冒烟测试用：**走真实控件**开关「详细」模式。
pub fn tree_set_detail_now(st: &Rc<PanelState>, on: bool) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.detail_check.set_active(on);
}

/// 树上的所有行（`名字`, `说明`）。
pub fn tree_rows(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let Some(tree) = tree(st) else { return Vec::new() };
    tree.rows()
}

/// 冒烟测试用：某个结果页签的面包屑（阶段 43）。
pub fn result_crumbs_now(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut child = tab.crumbs.first_child();
    while let Some(c) = child {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
        child = c.next_sibling();
    }
    out
}

/// 冒烟测试用：结果页签底栏的文字（阶段 45）——`(左半信息, 页码)`。
pub fn result_foot_now(st: &Rc<PanelState>, tab_idx: usize) -> (String, String) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (String::new(), String::new()) };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return (String::new(), String::new());
    };
    (
        tab.foot_info.text().to_string(),
        tab.foot_page.text().to_string(),
    )
}

/// 冒烟测试用：跳到第 N 页（阶段 45）。
pub fn goto_page_now(st: &Rc<PanelState>, tab_idx: usize, page: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    goto_page(st, ui, &tab, page);
}

/// 冒烟测试用：当前会话键（数据库连接在 ini 里是按它分组的）。
pub fn session_key_now(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.session_key.clone()
}

/// 冒烟测试用：数据库现在是连着还是没连。
pub fn is_connected_now(st: &Rc<PanelState>) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.connected.get()
}

/// 冒烟测试用：数据库工作台所在子页签的 id（关掉它 = 自动断开）。
pub fn workbench_tab_id(st: &Rc<PanelState>) -> u64 {
    let slot = st.db_ui.borrow();
    slot.as_ref().map(|ui| ui.tab_id.get()).unwrap_or(0)
}

/// 冒烟测试用：断开数据库连接。
pub fn disconnect_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    do_disconnect(st, ui);
}

/// 冒烟测试用：打开对象属性窗口（13.5）。
pub fn open_object_stats_now(st: &Rc<PanelState>, schema: &str, table: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_object_stats(st, ui, schema, table);
}

/// 冒烟测试用：属性窗口里的内容（`(属性, 值)`）。
pub fn object_stats_rows(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    // ⚠️ **表文档里的「属性」视图有自己的一张网格**（阶段 72e）——先找它，
    // 找不到才看共享的那张（独立属性窗口用的）。
    let doc_grid = {
        let docs = ui.docs.borrow();
        docs.iter().find_map(|d| d.props_grid.borrow().clone())
    };
    let Some(g) = doc_grid.or_else(|| ui.stats_grid.borrow().clone()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // 数据行从第 1 行开始（第 0 行是表头）
    let mut r = 1i32;
    while let Some((k, v)) = grid_row_text(&g, r) {
        out.push((k, v));
        r += 1;
    }
    out
}

/// 读网格第 r 行的两格文字（没有就 None）。
fn grid_row_text(g: &gtk::Grid, r: i32) -> Option<(String, String)> {
    let mut out = Vec::new();
    for c in 0..2 {
        let w = g.child_at(c, r)?;
        let l = w.downcast_ref::<gtk::Label>()?;
        out.push(l.text().to_string());
    }
    Some((out[0].clone(), out[1].clone()))
}

/// 冒烟测试用：状态栏文案（看连接到底成没成）。
pub fn status_text(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.status.text().to_string()
}

/// 冒烟测试用：打开表结构页签（等价于对象树右键「表结构」）。
pub fn open_structure_now(st: &Rc<PanelState>, schema: &str, table: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_structure(st, ui, schema, table);
}

/// 冒烟测试用：表结构页签的摘要。
///
/// 返回 `(表名, DDL 文本, 字段行数, 索引行数, 外键行数)`。
pub fn struct_tabs_info(st: &Rc<PanelState>) -> Vec<(String, String, usize, usize, usize)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.struct_tabs
        .borrow()
        .iter()
        .map(|t| {
            let buf = t.ddl.buffer();
            let ddl = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
            (
                format!("{}.{}", t.schema, t.table),
                ddl,
                t.fields.row_count(),
                t.indexes.row_count(),
                t.fks.row_count(),
            )
        })
        .collect()
}

/// 冒烟测试用：改写表结构页签里的 DDL 文本。
pub fn struct_set_ddl(st: &Rc<PanelState>, idx: usize, text: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(idx).cloned() else {
        return;
    };
    tab.ddl.buffer().set_text(text);
}

/// 冒烟测试用：点「执行 DDL」。
pub fn struct_exec_ddl(st: &Rc<PanelState>, idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(idx).cloned() else {
        return;
    };
    do_exec_ddl(st, ui, &tab);
}

/// 冒烟测试用：切到第几个页签（截图时用）。
pub fn show_tab(st: &Rc<PanelState>, idx: u32) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.notebook.set_current_page(Some(idx));
}

/// 冒烟测试用：当前页签数（结果页签 + 结构页签 + 定义页签都在同一个
/// notebook 里）。
pub fn result_tab_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.notebook.n_pages() as usize
}

/// 冒烟测试用：打开函数 / 存储过程的定义页签。
pub fn open_routine_def_now(
    st: &Rc<PanelState>,
    schema: &str,
    name: &str,
    args: &str,
    is_proc: bool,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_routine_def(st, ui, schema, name, args, is_proc);
}

/// 冒烟测试用：打开触发器的定义页签。
pub fn open_trigger_def_now(st: &Rc<PanelState>, schema: &str, table: &str, name: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_trigger_def(st, ui, schema, table, name);
}

/// 冒烟测试用：定义页签的摘要（标题 + 定义文本）。
pub fn def_tabs_info(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.def_tabs
        .borrow()
        .iter()
        .map(|t| {
            let buf = t.text.buffer();
            let text = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
            (t.title.clone(), text)
        })
        .collect()
}

/// 冒烟测试用：点「执行定义」。
pub fn def_exec_now(st: &Rc<PanelState>, idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.def_tabs.borrow().get(idx).cloned() else {
        return;
    };
    do_exec_def(st, ui, &tab);
}

/// 冒烟测试用：发起一次全库对象搜索。
pub fn search_now(st: &Rc<PanelState>, keyword: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.search_entry.set_text(keyword);
    do_search(st, ui);
}

/// 冒烟测试用：搜索结果（每行 5 列：类型 / 模式 / 名称 / 所属表 / 说明）。
pub fn search_results(st: &Rc<PanelState>) -> Vec<Vec<String>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.search_grid.visible_values()
}

    // （按连接名存，见 `PanelState.db_pass` 的注释）
/// 冒烟测试用：双击搜索结果里的第 `idx` 行（按显示顺序）。
pub fn search_open_now(st: &Rc<PanelState>, idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(src) = ui.search_grid.source_row(idx) else { return };
    let hit = ui.search_hits.borrow().get(src).cloned();
    let Some(hit) = hit else { return };
    open_search_hit(st, ui, &hit);
}

/// 冒烟测试用：打开历史窗口，返回历史条数。
pub fn open_history_now(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    open_history_window(st, ui);
    ui.hist_entries.borrow().len()
}

/// 工作台 SQL 行的四个按钮（执行 / 执行选中 / 取消 / 历史）。
///
/// **给冒烟用**：用户 2026-10-28 要求「主界面尽量少用文字」，按钮条一律
/// **图标 + tooltip**。这条得钉住——不然哪天有人改回「图标 + 文字」，
/// 界面不会报错，只是又变回"一排字挤在一起"。
pub fn sql_row_buttons_now(st: &Rc<PanelState>) -> Vec<gtk::Button> {
    // ⚠️ 借用要落成**具名变量**：写成 `st.db_ui.borrow().as_ref()` 的话临时值
    // 在语句结束就没了（E0716）。
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return Vec::new();
    };
    vec![
        ui.btn_run.clone(),
        ui.btn_run_sel.clone(),
        ui.btn_cancel.clone(),
        ui.btn_history.clone(),
    ]
}

/// 关掉执行历史窗口（没开着就什么都不做）。
///
/// **为什么要有它**：历史窗口是浮在主窗口上的独立窗口，截图/演示时它会
/// 把底下的界面盖住（子标签栏正好在左上角，最容易挡）。关掉它才能看清
/// 真正的布局。
pub fn close_history_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let w = ui.hist_win.borrow().clone();
    if let Some(w) = w {
        hide_tool_window(&w);
    }
}

/// 关掉搜索结果窗口（同 `close_history_now`：浮窗会挡住底下的布局）。
pub fn close_search_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let w = ui.search_win.borrow().clone();
    if let Some(w) = w {
        hide_tool_window(&w);
    }
}

/// 关掉对象属性窗口（同 `close_history_now`：浮窗会挡住底下的布局）。
pub fn close_stats_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let w = ui.stats_win.borrow().clone();
    if let Some(w) = w {
        hide_tool_window(&w);
    }
}

/// 彻底关掉数据库工作台（关连接标签 / 退出程序时调用）。
///
/// ⚠️ **绝对不能碰 `ui.win`**：阶段 12 之后它是**宿主窗口**（主窗口），
/// `destroy()` 会把整个主窗口连同其它标签页一起干掉——关一个连接标签
/// 结果整个程序没了。要关的只有工作台自己那两个浮窗（搜索结果 / 执行历史）。
pub fn close_now(st: &Rc<PanelState>) {
    let ui = st.db_ui.borrow_mut().take();
    let Some(ui) = ui else { return };
    if let Some(w) = ui.search_win.borrow().clone() {
        w.destroy();
    }
    if let Some(w) = ui.hist_win.borrow().clone() {
        w.destroy();
    }
}

/// 冒烟测试用：历史列表（时间 | SQL 摘要）。
pub fn history_info(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.hist_entries
        .borrow()
        .iter()
        .map(|e| (e.time.clone(), sql_one_line(&e.sql)))
        .collect()
}

/// 冒烟测试用：选中第 `idx` 条历史并填进编辑器，返回编辑器里的文本。
pub fn history_fill_now(st: &Rc<PanelState>, idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    if !ui.hist_grid.select_display_row(idx) {
        return String::new();
    }
    fill_history_into_editor(st, ui);
    let buf = ui.sql.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string()
}

/// 冒烟测试用：SQL 编辑器里的文本。
pub fn sql_text(st: &Rc<PanelState>) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let buf = ui.sql.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string()
}

/// 冒烟测试用：往 SQL 编辑器里写字（**不执行**）。
///
/// 阶段 97：空态那个「写一条查询」动作就是"清空编辑器 + 聚焦" ⇒
/// 要验它**真的有效果**，就得先往编辑器里塞点东西。
pub fn sql_set_text(st: &Rc<PanelState>, text: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.sql.buffer().set_text(text);
}

/// 冒烟测试用：「执行历史」窗口开着吗（阶段 97）。
///
/// ⚠️ 别去数 `app.windows()`：那个历史窗口是 `transient_for` 宿主窗口的
/// **独立 `gtk::Window`**，没有 `set_application` ⇒ 不在应用窗口列表里。
pub fn history_window_open(st: &Rc<PanelState>) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.hist_win.borrow().is_some()
}

/// 冒烟测试用：那个「执行历史」窗口本体（阶段 97）。
pub fn history_window(st: &Rc<PanelState>) -> Option<gtk::Window> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    ui.hist_win.borrow().clone()
}

/// 冒烟测试用：打开「执行历史」窗口（阶段 97）。
pub fn open_hist_window_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_history_window(st, ui);
}

/// 连接当前选中的连接（工作台已经开着、已经连着就别重连）。
///
/// 阶段 22：工作台里没有「连接」按钮了——打开工作台就该是能用的状态。
pub fn connect_current(st: &Rc<PanelState>) {
    let err = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        if ui.connected.get() {
            return; // 已经连着了，别打断
        }
        do_connect(st, ui)
    };
    // 借用放掉了再弹窗（弹窗时页面可能还没挂）
    if let Some(msg) = err {
        report_unmounted(st, &msg, true);
    }
}

/// 子标签被关掉时的处理：关掉的是**数据库工作台**这一页时，**自动断开**。
///
/// 2026-10-27 用户要求：工作台里不再摆「断开」按钮——**关掉页签就自动断开**。
/// （新建连接的入口也在子标签栏的「+」里，工作台里不再需要任何连接按钮。）
///
/// 顺带把 `st.db_ui` 清掉：工作台整页都没了，留着这个状态只会让下一次
/// 打开时拿到一个已经销毁的 widget。
pub fn on_tab_closed(st: &Rc<PanelState>, tab_id: u64) {
    {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        // 关掉的不是数据库页签（终端页签 / 文件编辑器页签）→ 不归这里管
        if ui.tab_id.get() != tab_id {
            return;
        }
        if ui.connected.get() {
            do_disconnect(st, ui);
        }
    }
    st.db_ui.borrow_mut().take();
}

/// 切换到某个**已保存**的数据库连接，并**直接连上**。
///
/// **双击会话树 / 连接管理器里的数据库连接时用**。
///
/// 阶段 22：以前这里只"选中"，还得再点一次「连接」——**多此一举**。
/// 用户双击就是想用这条连接，那就直接连。没密码也不要紧：服务端要密码时
/// 会弹框问（`on_need_password`），输完自动重连。
///
/// 已经连着别的库的话先把旧连接断开，免得界面上的状态和实际会话对不上。
pub fn use_profile(st: &Rc<PanelState>, key: &str, name: &str) {
    let Some(p) = dbstore::get(key, name) else {
        return;
    };
    let err = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return };
        // 选中它（工作台里没有下拉了，选中 = 记住"当前连的是哪条"）
        set_current_conn(st, ui, key, name);
        let msg = trf!("正在连接 {v0}…", v0 = p.name);
        set_status(ui, &msg, false);
        // **直接连上**：双击即用，不让用户再点一次
        do_connect(st, ui)
    };
    // 借用放掉了再弹窗（弹窗时页面还没挂 ⇒ 状态行没人看）
    if let Some(msg) = err {
        report_unmounted(st, &msg, true);
    }
}

/// 冒烟测试用：把 SQL 放进编辑器并按「执行」（走**真实**路径，会记进历史）。
pub fn run_sql_now(st: &Rc<PanelState>, sql: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.sql.buffer().set_text(sql);
    do_run(st, ui, false);
}

/// 冒烟测试用：执行一条任意 SQL（结果进状态栏）。
///
/// 用来在真库上准备/清理探针对象（建个函数、触发器之类）。
/// **不走编辑器**，所以不进执行历史（探针语句不该污染历史）。
pub fn exec_sql_now(st: &Rc<PanelState>, sql: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if !ui.connected.get() {
        return;
    }
    ui.busy.set(true);
    update_buttons(ui);
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(id, PendingReq::Probe);
    send_cmd(st, ui, WorkerCmd::DbExec { id, sql: sql.to_string() });
}

/// 冒烟测试用：打开 ER 图（`table = None` = 整个模式）。
pub fn open_er_now(st: &Rc<PanelState>, schema: &str, table: Option<&str>) -> bool {
    {
        let slot = st.db_ui.borrow();
        if slot.as_ref().is_none() {
            return false;
        }
    }
    open_er(st, schema, table);
    true
}

/// 冒烟测试用：最近打开那张 ER 图的数据。
pub fn er_graph_now(st: &Rc<PanelState>) -> Option<crate::backend::ermodel::ErGraph> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let tabs = ui.er_tabs.borrow();
    tabs.last().map(|t| t.view.graph())
}

/// ER 图页签数（冒烟测试用）。
pub fn er_tab_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    match slot.as_ref() {
        Some(ui) => ui.er_tabs.borrow().len(),
        None => 0,
    }
}

/// 冒烟测试用：在对象树里**弹出某张表的右键菜单**（走用户真正点的那条路）。
///
/// 光调 `open_er_now` 测不到右键菜单那一段——菜单项建没建、敏感性对不对、
/// 点了会不会真开图。这条路走的是「右键 → ER 图…」的完整链路。
pub fn tree_er_menu_now(st: &Rc<PanelState>, table: &str) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.tree.popup_menu_for(table)
}

/// 冒烟测试用：打开「新建表」对话框（截图用）。
pub fn open_create_table_dialog_now(st: &Rc<PanelState>, schema: &str) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    open_create_table_dialog(st, ui, schema);
    true
}

/// 冒烟测试用：执行一条对象树上的 DDL（建表 / 改名 / 删表）。
pub fn exec_tree_ddl_now(st: &Rc<PanelState>, schema: &str, sql: &str, what: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    exec_tree_ddl(st, ui, sql, what, schema);
}

/// 冒烟测试用：重新加载对象树（清空子节点后重新懒加载）。
///
/// 为什么需要：树是懒加载的，展开过的节点不会自动重查——刚在库里建的
/// 函数/触发器要刷新一下才看得到。
pub fn refresh_tree(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.tree.reload();
}

/* ---------------- 改结构（阶段 14）的测试钩子 ---------------- */

/// 冒烟测试用：结构页签的数量。
pub fn struct_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.struct_tabs.borrow().len()
}

/// 冒烟测试用：改字段表里的某个格子（等价于用户双击格子改了再回车）。
pub fn struct_set_cell(
    st: &Rc<PanelState>,
    tab_idx: usize,
    row: usize,
    col: usize,
    text: &str,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.fields.set_edit(row, col, text);
}

/// 冒烟测试用：加一个字段（末尾加一行空的）。
pub fn struct_add_field(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    do_add_field(ui, &tab);
}

/// 冒烟测试用：标记删除一个字段（按显示行号）。
pub fn struct_del_field(st: &Rc<PanelState>, tab_idx: usize, row: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.fields.delete_display_row(row);
}

/// 冒烟测试用：保存结构改动（**跳过二次确认**——测试不能卡在模态框上）。
pub fn struct_save_now(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    save_structure(st, ui, &tab, true);
}

/// 冒烟测试用：结构页签里字段表的内容（每行 5 格）。
pub fn struct_field_rows(st: &Rc<PanelState>, tab_idx: usize) -> Vec<Vec<String>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.fields.visible_values()
}

/// 冒烟测试用：结构页签的状态栏文案。
pub fn struct_status(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return String::new();
    };
    tab.status.text().to_string()
}

/// 冒烟测试用：表结构页签右下角的「应用变更」主按钮（阶段 44）。
///
/// ⚠️ **要断言它带 `.pp-primary`**：主操作按钮"不突出"不会报错，只是
/// 界面退化回"一排一样的图标"——这类问题只能靠断言钉住。
pub fn struct_apply_button_now(st: &Rc<PanelState>, tab_idx: usize) -> Option<gtk::Button> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let tab = ui.struct_tabs.borrow().get(tab_idx).cloned()?;
    Some(tab.btn_apply.clone())
}

/// 冒烟测试用：加一个索引。
pub fn struct_add_index(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    do_add_index(ui, &tab);
}

/// 冒烟测试用：标记删除一个索引（按显示行号）。
pub fn struct_del_index(st: &Rc<PanelState>, tab_idx: usize, row: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.indexes.delete_display_row(row);
}

/// 冒烟测试用：改索引表里的某个格子。
pub fn struct_set_index_cell(
    st: &Rc<PanelState>,
    tab_idx: usize,
    row: usize,
    col: usize,
    text: &str,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.indexes.set_edit(row, col, text);
}

/// 表设计器的分段控件有哪几段（阶段 49）。
///
/// **顺序即页名**：`inner`（`gtk::Stack`）里的页名跟这里一一对应，
/// 冒烟 / e2e 按下标切段时也按这个顺序。
const SEGMENT_NAMES: [&str; 5] = ["字段", "索引", "外键", "触发器", "DDL"];

/// 冒烟测试用：结构页签里索引表的内容（每行 3 格：索引 / 列 / 唯一）。
pub fn struct_index_rows(st: &Rc<PanelState>, tab_idx: usize) -> Vec<Vec<String>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.indexes.visible_values()
}

/// 冒烟测试用：切到设计器的某一页（0=字段 / 1=索引 / 2=外键 / 3=触发器 / 4=DDL）。
///
/// ⚠️ **分段控件的页名是字符串**（`gtk::Stack` 的 `add_named`），跟原来
/// 页签的"第几页"不是一回事——顺序变了的话这里要跟着改。
pub fn struct_show_page(st: &Rc<PanelState>, tab_idx: usize, page: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    let name = match page {
        0 => tr("字段"),
        1 => tr("索引"),
        2 => tr("外键"),
        3 => tr("触发器"),
        _ => "DDL",
    };
    tab.inner.set_visible_child_name(name);
}

/// 冒烟测试用：离线造一个表结构页签（**不连库**）。
///
/// 结构页签正常是双击对象树里的表才有的（要连着库）。冒烟里没法连库，
/// 所以这里直接造一个空壳的（字段/索引/外键都是空表，触发器灌两行假的）。
pub fn make_test_struct_tab(st: &Rc<PanelState>, schema: &str, table: &str) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let tab = new_struct_tab(st, ui, schema, table);
    // 灌两行假的字段（好让「实时预览」有东西可生成）
    let cols = vec![
        dbmeta::ColumnDef {
            name: "id".into(),
            data_type: "integer".into(),
            nullable: false,
            is_pk: true,
            ..Default::default()
        },
        dbmeta::ColumnDef {
            name: "name".into(),
            data_type: "text".into(),
            nullable: true,
            ..Default::default()
        },
    ];
    let rows: Vec<Vec<Option<String>>> = cols
        .iter()
        .map(|c| {
            vec![
                Some(c.name.clone()),
                Some(dbmeta::ddl_type(c)),
                Some(if c.nullable { tr("是").into() } else { tr("否").into() }),
                Some(if c.is_pk { tr("是").into() } else { tr("否").into() }),
                Some(c.default_value.clone().unwrap_or_default()),
                Some("⧉ ✕".to_string()),
            ]
        })
        .collect();
    *tab.cols.borrow_mut() = Some(cols);
    tab.fields.set_data(dbgrid::GridData {
        columns: vec![
            tr("字段").into(),
            tr("类型").into(),
            tr("可空").into(),
            tr("主键").into(),
            tr("默认值").into(),
            tr("操作").into(),
        ],
        types: vec![ColType::Text; 6],
        rows,
        pk: vec![],
    });
    tab.fields.set_column_editable(5, false);
    tab.triggers.set_data(dbgrid::GridData {
        columns: vec![tr("触发器").into(), tr("时机").into(), tr("事件").into()],
        types: vec![ColType::Text; 3],
        rows: vec![
            vec![
                Some("trg_audit".into()),
                Some("BEFORE".into()),
                Some("UPDATE".into()),
            ],
            vec![
                Some("trg_notify".into()),
                Some("AFTER".into()),
                Some("INSERT".into()),
            ],
        ],
        pk: vec![],
    });
    let idx = ui.struct_tabs.borrow().len().saturating_sub(1);
    Some(idx)
}


/// 冒烟测试用：字段表的列名（看看「操作」列在不在）。
pub fn struct_field_columns(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.fields.columns()
}

/// 冒烟测试用：类型下拉里的选项。
pub fn struct_type_choices(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    // ⚠️ **从 model 里取**：`ComboBoxText` 的界面结构里只有一个 Label
    // （显示当前选中的那个），要拿**全部**选项得读它的 model。
    // ⚠️ `ComboBoxText::model()` 返回的是 **TreeModel**（不是 ListModel）。
    let mut out = Vec::new();
    if let Some(model) = tab.type_combo.model() {
        if let Some(mut it) = model.iter_children(None) {
            loop {
                let v: String = model.get(&it, 0);
                out.push(v);
                if !model.iter_next(&mut it) {
                    break;
                }
            }
        }
    }
    out
}

/// 冒烟测试用：选中字段表的一行。
pub fn struct_select_field(st: &Rc<PanelState>, tab_idx: usize, row: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    tab.fields.select_display_row(row)
}

/// 冒烟测试用：挑一个类型并「应用」到选中行。
pub fn struct_apply_type(st: &Rc<PanelState>, tab_idx: usize, ty: &str) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    tab.type_combo.set_active_id(Some(ty));
    // 走**真实的按钮**（跟用户点「应用」一条路）：「应用」按钮紧跟在下拉后面
    let btn = tab.type_combo.parent().and_then(|p| {
        let mut kid = p.first_child();
        while let Some(k) = kid {
            if let Some(b) = k.downcast_ref::<gtk::Button>() {
                if b.label().as_deref() == Some(tr("应用")) {
                    return Some(b.clone());
                }
            }
            kid = k.next_sibling();
        }
        None
    });
    let Some(btn) = btn else { return false };
    btn.emit_clicked();
    true
}


/// 冒烟测试用：改字段表里的一个格子（模拟用户双击编辑）。
pub fn struct_set_field_cell(
    st: &Rc<PanelState>,
    tab_idx: usize,
    row: usize,
    col: usize,
    text: &str,
) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    tab.fields.set_edit(row, col, text);
    true
}

/// 冒烟测试用：字段表的右键菜单是不是开着的（阶段 50：应当是关的）。
pub fn struct_row_menu_enabled(st: &Rc<PanelState>, tab_idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    tab.fields.row_menu_is_enabled()
}

/// 冒烟测试用：字段改动的实时预览（SQL 文本）。
pub fn struct_preview_text(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return String::new();
    };
    let buf = tab.preview.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), true)
        .to_string()
}

/// 冒烟测试用：实时预览旁边的标记（「✓ 可执行」/「✗ 不能执行」…）。
pub fn struct_preview_mark(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return String::new();
    };
    tab.preview_mark.text().to_string()
}

/// 冒烟测试用：设计器当前停在哪一段（分段控件的页名）。
pub fn struct_current_page(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return String::new();
    };
    tab.inner
        .visible_child_name()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 冒烟测试用：设计器的分段控件有几个分段（阶段 49）。
pub fn struct_segment_count(st: &Rc<PanelState>, tab_idx: usize) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return 0;
    };
    // StackSwitcher 的分段数 = 它挂的那个 Stack 的子页数
    // ⚠️ `Stack::pages()` 在 gtk4-rs 0.11 里返回的是 `SelectionModel`
    // （没有 `len()`），数子页得自己走兄弟链。
    let mut n = 0usize;
    let mut child = tab.inner.first_child();
    while let Some(c) = child {
        n += 1;
        child = c.next_sibling();
    }
    n
}

/// 冒烟测试用：分段控件的宽度（像素）。
pub fn struct_segment_width(st: &Rc<PanelState>, tab_idx: usize) -> i32 {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return 0;
    };
    tab.seg_buttons
        .first()
        .and_then(|b| b.parent())
        .map(|p| p.width())
        .unwrap_or(0)
}

/// 冒烟测试用：分段控件上那排按钮的文字 + 当前选中的是第几个。
pub fn struct_segment_titles(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.seg_buttons
        .iter()
        .map(|b| {
            // ⚠️ **按钮自己没有 `label` 属性**：文字在它的子控件里，
            // 得往下找一层 Label。
            let mut text = String::new();
            let mut stack: Vec<gtk::Widget> = vec![b.clone().upcast()];
            while let Some(w) = stack.pop() {
                if let Some(l) = w.downcast_ref::<gtk::Label>() {
                    text = l.text().to_string();
                    break;
                }
                let mut kid = w.first_child();
                while let Some(k) = kid {
                    stack.push(k.clone());
                    kid = k.next_sibling();
                }
            }
            text
        })
        .collect()
}

/// 冒烟测试用：分段控件当前选中的是第几个（0 起）。
pub fn struct_segment_active(st: &Rc<PanelState>, tab_idx: usize) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return None;
    };
    tab.seg_buttons.iter().position(|b| b.is_active())
}

/// 冒烟测试用：点分段控件上的第 `idx` 个按钮（走**真实点击**路径）。
pub fn struct_click_segment(st: &Rc<PanelState>, tab_idx: usize, idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    let Some(b) = tab.seg_buttons.get(idx).cloned() else {
        return false;
    };
    b.emit_clicked();
    true
}

/// 冒烟测试用：触发器表的内容（每行 3 格：触发器 / 时机 / 事件）。
pub fn struct_trigger_rows(st: &Rc<PanelState>, tab_idx: usize) -> Vec<Vec<String>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.triggers.visible_values()
}

/// 冒烟测试用：加一个外键。
pub fn struct_add_fk(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    do_add_fk(ui, &tab);
}

/// 冒烟测试用：标记删除一个外键（按显示行号）。
pub fn struct_del_fk(st: &Rc<PanelState>, tab_idx: usize, row: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.fks.delete_display_row(row);
}

/// 冒烟测试用：改外键表里的某个格子。
pub fn struct_set_fk_cell(
    st: &Rc<PanelState>,
    tab_idx: usize,
    row: usize,
    col: usize,
    text: &str,
) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else { return };
    tab.fks.set_edit(row, col, text);
}

/// 冒烟测试用：结构页签里外键表的内容（每行 4 格：约束 / 列 / 引用表 / 引用列）。
pub fn struct_fk_rows(st: &Rc<PanelState>, tab_idx: usize) -> Vec<Vec<String>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.struct_tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    tab.fks.visible_values()
}

/* ---------------- 插入 / 删除行（9.4）的测试钩子 ---------------- */

/// 冒烟测试用：插入一行新记录（等价于点「插入行」）。
pub fn grid_insert_row(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    do_insert_row(st, ui, &tab);
}

/// 冒烟测试用：填某个单元格（等价于用户在格子里输入后回车）。
pub fn grid_set_cell(st: &Rc<PanelState>, tab_idx: usize, row: usize, col: usize, text: &str) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    tab.grid.set_edit(row, col, text);
}

/// 冒烟测试用：按显示行号标记删除（等价于选中那行再点「删除行」）。
pub fn grid_delete_row(st: &Rc<PanelState>, tab_idx: usize, row: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    delete_rows_at(st, ui, &tab, vec![row]);
}

/// 冒烟测试用：点「保存修改」。
pub fn grid_save(st: &Rc<PanelState>, tab_idx: usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    do_save(st, ui, &tab);
}

/* ---------------- 统一关闭流程（阶段 63 / 审计 P0-1） ---------------- */

/// 网格里有没有待保存的改动（新增 / 修改 / 删除）。
fn grid_has_pending(g: &Rc<dbgrid::DbGrid>) -> bool {
    !g.pending_edits().is_empty() || !g.pending_inserts().is_empty() || !g.pending_deletes().is_empty()
}

/// 待保存的**数据**改动：几张表。
pub fn pending_edit_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.tabs
        .borrow()
        .iter()
        .filter(|t| t.target.borrow().is_some() && t.editing.get() && grid_has_pending(&t.grid))
        .count()
}

/// 待保存的**表结构**改动：几张表。
pub fn pending_struct_edit_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.struct_tabs
        .borrow()
        .iter()
        .filter(|t| {
            grid_has_pending(&t.fields) || grid_has_pending(&t.indexes) || grid_has_pending(&t.fks)
        })
        .count()
}

/// 把所有待保存的改动**排队**保存，返回排了几个。
///
/// 排队而不是一股脑发：数据表保存走 `ui.save_queue`、表结构保存走
/// `tab.sql_queue`，两边共用 `ui.busy` ⇒ 同一时刻只能存一个。
pub fn start_save_all_pending(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    if !ui.connected.get() {
        return 0;
    }
    let mut chain: VecDeque<DbSaveTask> = VecDeque::new();
    for t in ui.tabs.borrow().iter() {
        if t.target.borrow().is_some() && t.editing.get() && grid_has_pending(&t.grid) {
            chain.push_back(DbSaveTask::Grid(t.id));
        }
    }
    for t in ui.struct_tabs.borrow().iter() {
        if grid_has_pending(&t.fields) || grid_has_pending(&t.indexes) || grid_has_pending(&t.fks) {
            chain.push_back(DbSaveTask::Struct(t.id));
        }
    }
    if chain.is_empty() {
        return 0;
    }
    let n = chain.len();
    *ui.save_chain.borrow_mut() = Some(chain);
    chain_step(st, ui);
    n
}

/// 保存链的下一步。
///
/// 链走完 ⇒ 报一声"保存完成"，统一关闭流程这才真的关（`SaveBarrier`）。
fn chain_step(st: &Rc<PanelState>, ui: &DbUi) {
    let next = {
        let mut slot = ui.save_chain.borrow_mut();
        match slot.as_mut() {
            // 没有链 ⇒ 不是关闭流程，别乱动
            None => return,
            Some(q) => q.pop_front(),
        }
    };
    match next {
        Some(DbSaveTask::Grid(id)) => {
            let Some(tab) = find_tab(ui, id) else {
                chain_step(st, ui);
                return;
            };
            // ⚠️ 存之前再核一遍条件：`do_save` 条件不满足时会**静默返回**，
            // 链就卡在这儿了（统一关闭永远等不到"保存完成"）。
            if !ui.connected.get()
                || !tab.editing.get()
                || tab.target.borrow().is_none()
                || !grid_has_pending(&tab.grid)
            {
                chain_step(st, ui);
                return;
            }
            do_save(st, ui, &tab);
        }
        Some(DbSaveTask::Struct(id)) => {
            let Some(tab) = find_struct_tab(ui, id) else {
                chain_step(st, ui);
                return;
            };
            if !ui.connected.get()
                || ui.busy.get()
                || !(grid_has_pending(&tab.fields)
                    || grid_has_pending(&tab.indexes)
                    || grid_has_pending(&tab.fks))
            {
                chain_step(st, ui);
                return;
            }
            // ⚠️ `skip_confirm = true`：关闭确认框里用户已经点过"保存并关闭"，
            // 这里再弹一次"确定要改表结构吗"等于拦路。
            save_structure(st, ui, &tab, true);
        }
        None => {
            *ui.save_chain.borrow_mut() = None;
            // 链走完了 ⇒ 报完成。⚠️ 必须**延后**再报：调用点正借着
            // `st.db_ui`，报完成会顺着闸门去关面板（那里要再借一次 `db_ui`）。
            super::closeguard::note_save_done_deferred();
        }
    }
}

/// 冒烟 / e2e 用：结果页签的「编辑 / 保存修改 / 放弃修改」三个**真实按钮**。
///
/// **为什么连按钮都要交出去**（用户 2026-10-28 报「提交和回滚按钮都是
/// 禁用的，改了数据之后也点不了」）：之前的 e2e 全走 `grid_save` 这类
/// **测试钩子直接调内部函数**，**从来不点真实按钮**——所以按钮一直是灰的
/// 也没人发现（钩子绕过了整个按钮层）。
///
/// 返回 `(编辑, 保存修改, 放弃修改)`；e2e 可以 `emit_clicked()` 走真实
/// 路径，并断言 `is_sensitive()`。
pub fn edit_toolbar_buttons(st: &Rc<PanelState>, tab_idx: usize) -> Option<(gtk::Button, gtk::Button, gtk::Button)> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let tab = ui.tabs.borrow().get(tab_idx).cloned()?;
    Some((tab.btn_edit.clone(), tab.btn_save.clone(), tab.btn_discard.clone()))
}

/// 冒烟 / e2e 用：点「编辑」按钮（**走真实按钮**，不是直接调内部函数）。
///
/// 进编辑模式会发 BEGIN，等回包后「保存修改 / 放弃修改」应当变成可点。
pub fn click_edit(st: &Rc<PanelState>, tab_idx: usize) {
    let Some((btn, _, _)) = edit_toolbar_buttons(st, tab_idx) else {
        return;
    };
    btn.emit_clicked();
}

/// 冒烟 / e2e 用：点「放弃修改」按钮（回滚事务，走真实按钮）。
pub fn click_discard(st: &Rc<PanelState>, tab_idx: usize) {
    let Some((_, _, btn)) = edit_toolbar_buttons(st, tab_idx) else {
        return;
    };
    btn.emit_clicked();
}

/// 冒烟测试用：某个结果页签的待保存改动（插入行数、删除行数、改动格数）。
pub fn grid_pending(st: &Rc<PanelState>, tab_idx: usize) -> (usize, usize, usize) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (0, 0, 0) };
    let tab = ui.tabs.borrow().get(tab_idx).cloned();
    let Some(tab) = tab else {
        return (0, 0, 0);
    };
    let edits = tab.grid.pending_edits();
    let cells = edits.values().map(|m| m.len()).sum();
    (
        tab.grid.pending_inserts().len(),
        tab.grid.pending_deletes().len(),
        cells,
    )
}

/// 冒烟测试用：定义页签的状态栏文案。
pub fn def_status(st: &Rc<PanelState>, idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let tab = ui.def_tabs.borrow().get(idx).cloned();
    let Some(tab) = tab else {
        return String::new();
    };
    tab.status.text().to_string()
}

/// 冒烟测试用：直接导入一段 CSV 文本（跳过文件选择与映射对话框）。
///
/// **为什么要这个钩子**：导入向导的前两步（选文件、手工映射）是纯交互，
/// 没法在无头测试里驱动；但真正要紧的是**后面那条链路**——解析 → 映射 →
/// 事务 → 插入 → 回滚，那部分必须对着真库验。
///
/// 映射按**列名自动匹配**（`dbcsv::auto_map`）。
pub fn import_csv_now(st: &Rc<PanelState>, schema: &str, table: &str, csv: &str) -> bool {
    start_import(st, schema, table, csv, true)
}

/// 冒烟测试用：走**完整向导**（弹字段映射对话框），用来截图核对界面。
pub fn import_csv_dialog_now(st: &Rc<PanelState>, schema: &str, table: &str, csv: &str) -> bool {
    start_import(st, schema, table, csv, false)
}

/// 导入的公共入口（查列 → 映射 → 执行）。
fn start_import(
    st: &Rc<PanelState>,
    schema: &str,
    table: &str,
    csv: &str,
    auto: bool,
) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    if !ui.connected.get() {
        return false;
    }
    *ui.last_import.borrow_mut() = None;
    let id = next_id(ui);
    ui.pending.borrow_mut().insert(
        id,
        PendingReq::ImportPrep {
            schema: schema.to_string(),
            table: table.to_string(),
            text: csv.to_string(),
            auto,
        },
    );
    send_cmd(
        st,
        ui,
        WorkerCmd::DbMeta {
            id,
            req: DbMetaReq::Columns {
                schema: schema.to_string(),
                table: table.to_string(),
            },
        },
    );
    true
}

/// 冒烟测试用：关掉导入失败的错误框（免得它挡住后面的截图）。
pub fn close_import_error_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if let Some(d) = ui.import_err.borrow_mut().take() {
        d.close();
    }
}

/// 冒烟测试用：上一次导入的结果（`Some((成功?, 说明))`）。
pub fn last_import(st: &Rc<PanelState>) -> Option<(bool, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    ui.last_import.borrow().clone()
}

/// 冒烟测试用：结果表列头的**显示内容**（阶段 46）。
///
/// 返回每个可见列的 `(列名, 类型小字)`。
///
/// **为什么读列头 widget 而不是模型**：要验的正是"屏幕上那列头长什么样"
/// ——dbx 的列头是「列名 + 下面一行小字类型」，只看模型是查不出来的。
pub fn grid_header_info(st: &Rc<PanelState>, tab_idx: usize) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for c in tab.grid.view.columns() {
        let title = c.title().to_string();
        // 列头 widget = 水平 Box：竖向 Box（列名 + 类型小字）+ ⋮ 按钮
        let mut ty = String::new();
        if let Some(w) = c.widget() {
            if let Some(hb) = w.downcast_ref::<gtk::Box>() {
                let vb = hb
                    .first_child()
                    .and_then(|x| x.downcast_ref::<gtk::Box>().cloned());
                if let Some(vb) = vb {
                    // 竖向 Box 的**第二个** Label 才是类型小字
                    let second = vb.first_child().and_then(|x| x.next_sibling());
                    if let Some(l) = second.and_then(|x| x.downcast_ref::<gtk::Label>().cloned()) {
                        ty = l.text().to_string();
                    }
                }
            }
        }
        out.push((title, ty));
    }
    out
}

/// 冒烟测试用：造一个**结果页签**（不连库、不查数据）。
///
/// **为什么需要**：结果页签的**工具栏**（筛选框 / 数据库过滤条件按钮 /
/// 列设置入口）只有在有页签时才有东西可验，而真页签要连库 ⇒ 这里造一个
/// 空壳页签，专门验界面。
pub fn new_result_tab_now(st: &Rc<PanelState>, title: &str) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    new_tab_with_title(st, ui, title);
    Some(ui.tabs.borrow().len().saturating_sub(1))
}

/// 冒烟测试用：SQL 编辑器的**占位提示**当前显不显示（阶段 83）。
///
/// 空 ⇒ 提示该在；有字 ⇒ 该藏。
pub fn sql_placeholder_visible(st: &Rc<PanelState>) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.sql_ph.is_visible()
}

/// 冒烟测试用：第 `idx` 个结果页签的**空结果态**。
///
/// 返回 `(是不是正显示空结果态, 标题, 解释, 主 CTA 在不在)`。
pub fn result_grid_empty_state(
    st: &Rc<PanelState>,
    idx: usize,
) -> Option<(bool, String, String, bool)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    let tab = ui.tabs.borrow().get(idx).cloned()?;
    let shown = tab.grid_stack.visible_child_name().as_deref() == Some("empty");
    Some((
        shown,
        tab.empty_title.text().to_string(),
        tab.empty_desc.text().to_string(),
        tab.empty_cta.is_visible(),
    ))
}

/// 冒烟测试用：给第 `idx` 个结果页签灌一份**假结果集**（不连库）。
///
/// 列有、行没有 ⇒ 模拟"查询跑完了但一行都没返回"。
pub fn set_result_grid_rows(
    st: &Rc<PanelState>,
    idx: usize,
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(idx).cloned() else {
        return false;
    };
    tab.grid.set_data(dbgrid::GridData {
        columns,
        types: Vec::new(),
        rows,
        pk: Vec::new(),
    });
    true
}

/// 冒烟测试用：表文档的数量（阶段 72e）。
pub fn doc_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.docs.borrow().len()
}

/// 冒烟测试用：第 `idx` 个表文档的内层页签名字（阶段 72e）。
pub fn doc_view_labels(st: &Rc<PanelState>, idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(doc) = ui.docs.borrow().get(idx).cloned() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // 数据页
    out.push(
        doc.inner
            .tab_label_text(&doc.data.box_)
            .map(|s| s.to_string())
            .unwrap_or_default(),
    );
    // 三个懒视图（按页序号排）
    let mut keys: Vec<i32> = doc.slots.borrow().keys().copied().collect();
    keys.sort();
    for k in keys {
        let w = doc.slots.borrow().get(&k).cloned();
        if let Some(w) = w {
            out.push(
                doc.inner
                    .tab_label_text(&w)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            );
        }
    }
    out
}

/// 冒烟测试用：第 `idx` 个表文档现在停在第几个视图（阶段 72e）。
pub fn doc_current_view(st: &Rc<PanelState>, idx: usize) -> i32 {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return -1 };
    let Some(doc) = ui.docs.borrow().get(idx).cloned() else {
        return -1;
    };
    doc.inner.current_page().map(|p| p as i32).unwrap_or(-1)
}

/// 冒烟测试用：每个表文档对应的（模式, 表名）。
pub fn doc_names(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.docs
        .borrow()
        .iter()
        .map(|d| (d.schema.clone(), d.table.clone()))
        .collect()
}

/// 冒烟测试用：外层 notebook 现在停在**哪个表文档**上（不在表文档上就 None）。
///
/// **为什么需要**：阶段 72e 之后「打开一张表」= **切到**它的表文档，
/// 外层页数**不会变** ⇒ 光数页数分不清"切过去了"和"啥也没发生"。
pub fn current_doc_index(st: &Rc<PanelState>) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    let cur = ui.notebook.current_page();
    ui.docs
        .borrow()
        .iter()
        .position(|d| ui.notebook.page_num(&d.page) == cur)
}

/// 冒烟测试用：切到第 `idx` 个表文档的某个视图（阶段 72e）。
pub fn doc_focus_now(st: &Rc<PanelState>, idx: usize, page: i32) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(doc) = ui.docs.borrow().get(idx).cloned() else {
        return false;
    };
    focus_doc(Rc::clone(st), ui, &doc, page);
    true
}

/// 冒烟测试用：工作台右侧竖直分栏**有没有溢出**（阶段 72e）。
///
/// **为什么必须盯这个**（实测踩过的坑）：上半区（页签区）的最小高度**随
/// 内容变**——结构视图里有 200px 的字段表 + 80px 的 DDL 预览（阶段 50 加
/// 的），最小高度能顶到 506；下半区的 SQL 编辑器最小 196。
///
/// `GtkPaned` 默认 `shrink-start-child=true` ⇒ 它把上半区按**最小高度**
/// 给（506），下半区却仍按原来分隔条的位置（320）给（382）⇒ **两个子
/// 控件加起来 888 > 704，溢出重叠** ⇒ 上半区底部被 SQL 编辑器盖住，
/// **更糟的是上半区的页签栏被挤成 0 高**——用户看不到外层页签，也看不到
/// 「数据/结构/属性/ER」，看着像"页签整个不见了"。
///
/// 修法：分栏**不许把任何一边挤到比它的最小高度还小**
/// （`set_shrink_start_child(false)`），Paned 会自己把分隔条挪到合法位置。
///
/// 返回 true = **溢出**（坏）。
pub fn right_paned_overflows(st: &Rc<PanelState>) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(p) = ui.stack.parent().and_then(|w| w.downcast_ref::<gtk::Paned>().cloned())
    else {
        return false;
    };
    let total = p.allocation().height();
    let start = p.start_child().map(|c| c.allocation().height()).unwrap_or(0);
    let end = p.end_child().map(|c| c.allocation().height()).unwrap_or(0);
    // 留 1px 的余量（Paned 自己那条分隔条不算在子控件高度里）
    start + end > total + 1
}

/// 冒烟测试用：外层 notebook 的页数（阶段 72e：表文档占**一个**外层页）。
pub fn outer_page_count(st: &Rc<PanelState>) -> usize {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return 0 };
    ui.notebook.n_pages() as usize
}

/// 冒烟测试用：把工作台标成「已连接」（**不真的连库**）。
///
/// **为什么需要**：`open_table` 一上来就查"连上了没"⇒ 不连库就打不开表页签，
/// 而「可编辑」标志只有表页签才有。
pub fn fake_connected_now(st: &Rc<PanelState>) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.connected.set(true);
}

/// 冒烟测试用：第 `tab_idx` 个结果页签的**只读 / 可编辑**标志。
pub fn result_mode_chip_now(st: &Rc<PanelState>, tab_idx: usize) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return String::new();
    };
    tab.mode_chip.text().to_string()
}

/// 冒烟测试用：第 `tab_idx` 个结果页签的**网格**。
pub fn result_grid_now(st: &Rc<PanelState>, tab_idx: usize) -> Option<Rc<dbgrid::DbGrid>> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    ui.tabs.borrow().get(tab_idx).map(|t| Rc::clone(&t.grid))
}

/// 冒烟测试用：第 `tab_idx` 个结果页签的筛选框文案与过滤按钮提示。
///
/// 返回 `(筛选框 placeholder, 过滤按钮 tooltip)`。
pub fn result_filter_ui_now(st: &Rc<PanelState>, tab_idx: usize) -> (String, String) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (String::new(), String::new()) };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return (String::new(), String::new());
    };
    let ph = tab.filter.placeholder_text().unwrap_or_default().to_string();
    let tip = tab.btn_filter.tooltip_text().unwrap_or_default().to_string();
    (ph, tip)
}

/// 冒烟测试用：第 `tab_idx` 个结果页签的**工具栏按钮提示**（挨个列出来）。
///
/// 用来查"某个功能还有几个入口"——列设置以前有**两个**入口（工具栏按钮 +
/// 列头菜单），审计说只留一个 ⇒ 这个列表里不该再出现「列显示设置」。
pub fn result_toolbar_tips(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack: Vec<gtk::Widget> = vec![tab.box_.clone().upcast()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.clone().downcast_ref::<gtk::Button>() {
            if let Some(t) = b.tooltip_text() {
                out.push(t.to_string());
            }
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            stack.push(x.clone());
            c = x.next_sibling();
        }
    }
    out
}

/// 冒烟测试用：选中若干行（模拟用户 ctrl 多选）。
pub fn grid_select_rows(st: &Rc<PanelState>, tab_idx: usize, rows: &[usize]) {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return;
    };
    let sel = tab.grid.view.selection();
    sel.unselect_all();
    for r in rows {
        sel.select_path(&gtk::TreePath::from_indices(&[*r as i32]));
    }
}

/// 冒烟测试用：点**真实的**「删除行」按钮（阶段 46：多选后要删全部选中行）。
///
/// ⚠️ 走真实按钮（`emit_clicked`）而不是内部函数——按钮灰着、没接上回调，
/// 这条路径照样"通过"（阶段 42 的教训）。
pub fn grid_click_delete(st: &Rc<PanelState>, tab_idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    if !tab.btn_delete.is_sensitive() {
        return false;
    }
    tab.btn_delete.emit_clicked();
    true
}

/// 冒烟 / e2e 用：**离线**造一个结果页签（灌假数据，不连库）。
///
/// 真库跑一次要等网络，慢的冒烟等不起；而「详情窗口 / 转置 / 批量修改」
/// 这些**纯本地**功能用假数据验更稳。
///
/// 返回页签下标。
pub fn make_test_result_tab(
    st: &Rc<PanelState>,
    title: &str,
    columns: &[&str],
    types: &[dbmeta::ColType],
    rows: Vec<Vec<Option<String>>>,
) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    let tab = new_tab_with_title(st, ui, title);
    *tab.types.borrow_mut() = types.to_vec();
    *tab.pk.borrow_mut() = vec![0];
    // 列名也填上（**与 types 同序**）：条件条要靠它列选项，条件里的值要
    // 靠它找对应类型（阶段 48）
    *tab.col_names.borrow_mut() = columns.iter().map(|s| s.to_string()).collect();
    fill_cond_bar(&tab);
    *tab.target.borrow_mut() = Some(TableTarget {
        schema: "public".to_string(),
        table: "demo_users".to_string(),
        is_view: false,
    });
    tab.grid.set_data(dbgrid::GridData {
        columns: columns.iter().map(|s| s.to_string()).collect(),
        types: types.to_vec(),
        rows,
        pk: vec![0],
    });
    let idx = ui.tabs.borrow().len().saturating_sub(1);
    Some(idx)
}

/// 冒烟 / e2e 用：条件条的当前状态。
///
/// 返回 `(WHERE 文本, ORDER BY 文本, 条件 chip 文字, 排序列)`。
pub fn cond_bar_state(st: &Rc<PanelState>, tab_idx: usize) -> (String, String, Vec<String>, String) {
    let tab = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else {
            return (String::new(), String::new(), Vec::new(), String::new());
        };
        let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
            return (String::new(), String::new(), Vec::new(), String::new());
        };
        tab
    };
    let chips = tab
        .filter_conds
        .borrow()
        .iter()
        .map(cond_chip_text)
        .collect::<Vec<_>>();
    (
        tab.filter_where.borrow().clone(),
        tab.order_by.borrow().clone(),
        chips,
        tab.order_col.borrow().clone(),
    )
}

/// 冒烟 / e2e 用：在条件条上填一条条件并点「+ 条件」。
///
/// 走的是**真实的按钮回调**（`btn_add.emit_clicked()`），跟用户点的路径
/// 一样——直接调内部函数的话，按钮灰了、回调断了都测不出来。
pub fn cond_bar_add_now(
    st: &Rc<PanelState>,
    tab_idx: usize,
    column: &str,
    op: &str,
    value: &str,
) -> bool {
    let tab = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return false };
        let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
            return false;
        };
        tab
    };
    tab.cond_bar.col.set_active_id(Some(column));
    tab.cond_bar.op.set_active_id(Some(op));
    tab.cond_bar.val.set_text(value);
    tab.cond_bar.btn_add.emit_clicked();
    true
}

/// 冒烟 / e2e 用：挑排序列（并可切方向）。
pub fn cond_bar_order_now(st: &Rc<PanelState>, tab_idx: usize, column: &str, asc: bool) -> bool {
    let tab = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return false };
        let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
            return false;
        };
        tab
    };
    tab.cond_bar.order_col.set_active_id(Some(column));
    if tab.order_asc.get() != asc {
        tab.cond_bar.btn_dir.emit_clicked();
    }
    true
}

/// 冒烟 / e2e 用：挑条件的连接词（`true` = 或者）。
///
/// ⚠️ **单独一个钩子**：连接词只在**第二条**条件起才有意义，
/// 混在「加条件」里容易忘了先设它再点。
pub fn cond_bar_set_join_now(st: &Rc<PanelState>, tab_idx: usize, or: bool) -> bool {
    let tab = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return false };
        let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
            return false;
        };
        tab
    };
    tab.cond_bar.join.set_active_id(Some(if or { "OR" } else { "AND" }));
    true
}

/// 冒烟 / e2e 用：点条件条上的「清除」。
pub fn cond_bar_clear_now(st: &Rc<PanelState>, tab_idx: usize) -> bool {    let tab = {
        let slot = st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else { return false };
        let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
            return false;
        };
        tab
    };
    tab.cond_bar.btn_clear.emit_clicked();
    true
}

/// 冒烟 / e2e 用：触发结果网格右键菜单里的某个动作（阶段 47）。
///
/// `which`：`cell` / `row` / `col` / `transpose` / `batch`。
///
/// ⚠️ 走的是**真实的菜单回调**（网格 → hook → dbui），不是直接调内部函数。
pub fn grid_menu_action_now(st: &Rc<PanelState>, tab_idx: usize, which: &str) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    let row = tab.grid.selected_display_rows().first().copied().unwrap_or(0);
    let col = 0usize;
    let action = match which {
        "cell" => dbgrid::GridMenuAction::CellDetail { row, col },
        "row" => dbgrid::GridMenuAction::RowDetail { row },
        "col" => dbgrid::GridMenuAction::ColDetail { col },
        "transpose" => dbgrid::GridMenuAction::Transpose,
        "batch" => dbgrid::GridMenuAction::BatchEdit { col },
        _ => return false,
    };
    handle_grid_menu(st, ui, &tab, action);
    true
}

/// 冒烟 / e2e 用：最近一次打开的网格详情内容（阶段 47）。
pub fn grid_detail_now(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.last_detail.borrow().clone()
}

/// 冒烟测试用：给某个结果页签的某一列排序（**原始列下标**，升/降序）。
///
/// **为什么需要**：阶段 64 要在**真库**上验"数字列按数值排序"，
/// 光看列头类型小字不够 ⇒ 得能真的触发一次排序再看行序。
pub fn grid_sort_now(st: &Rc<PanelState>, tab_idx: usize, col: usize, asc: bool) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(tab_idx).cloned() else {
        return false;
    };
    tab.grid.sort_by_column(col, asc);
    true
}

/// 冒烟测试用：结果表当前显示的内容（列名 + 每行的单元格文本）。
///
/// 从 **TreeView 的模型**里读，而不是从 Rust 侧的 GridData 读——要验的正是
/// "视图里到底画出来没有"（列没绑 text 属性时模型里有值、格子里是空的）。
pub fn grid_rows(st: &Rc<PanelState>) -> Vec<(Vec<String>, Vec<Vec<String>>)> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    for tab in ui.tabs.borrow().iter() {
        let model = tab.grid.view.model();
        let Some(model) = model else { continue };
        let mut cols = Vec::new();
        for c in tab.grid.view.columns() {
            cols.push(c.title().to_string());
        }
        let mut rows = Vec::new();
        if let Some(mut i) = model.iter_first() {
            loop {
                let mut cells = Vec::new();
                for c in 0..cols.len() {
                    cells.push(model.get::<String>(&i, c as i32));
                }
                rows.push(cells);
                if !model.iter_next(&mut i) {
                    break;
                }
            }
        }
        out.push((cols, rows));
    }
    out
}

/// 冒烟测试用：第 `tab_idx` 个结果页签当前**显示**的列名（按显示顺序）。
///
/// 读的是 TreeView 的列标题 —— 要验的正是"视图里到底画出来几列"。
pub fn grid_visible_columns(st: &Rc<PanelState>, tab_idx: usize) -> Vec<String> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let tabs = ui.tabs.borrow();
    let Some(tab) = tabs.get(tab_idx) else {
        return Vec::new();
    };
    tab.grid
        .view
        .columns()
        .iter()
        .map(|c| c.title().to_string())
        .collect()
}

/// 冒烟测试用：第 `tab_idx` 个结果页签可见列的**原始下标**（按显示顺序）。
pub fn grid_visible_col_indexes(st: &Rc<PanelState>, tab_idx: usize) -> Vec<usize> {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let tabs = ui.tabs.borrow();
    let Some(tab) = tabs.get(tab_idx) else {
        return Vec::new();
    };
    tab.grid.visible_columns()
}

/// 冒烟测试用：打开一张表（返回新结果页签的下标）。
///
/// 阶段 20 用它验证"列显示设置的持久化"：表浏览页签重开时设置还在。
pub fn open_table_now(st: &Rc<PanelState>, schema: &str, table: &str) -> Option<usize> {
    let slot = st.db_ui.borrow();
    let ui = slot.as_ref()?;
    open_table(st, ui, schema, table, false);
    // 阶段 72e：打开一张表 = 打开/切到它的**表文档**；文档可能已经有了
    // ⇒ 这里返回它的「数据」视图在 `ui.tabs` 里的下标（不是"新建的那个"）。
    let doc = find_doc(ui, schema, table)?;
    let tabs = ui.tabs.borrow();
    tabs.iter().position(|t| Rc::ptr_eq(t, &doc.data))
}

/// 冒烟测试用：某个结果页签按指定格式导出的文本。
///
/// 格式名：`csv` / `sql` / `json` / `xml` / `html`。
pub fn export_preview(st: &Rc<PanelState>, idx: usize, fmt: &str) -> String {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(tab) = ui.tabs.borrow().get(idx).cloned() else {
        return String::new();
    };
    let f = match fmt {
        "csv" => ExportFormat::Csv,
        "sql" => ExportFormat::Sql,
        "json" => ExportFormat::Json,
        "xml" => ExportFormat::Xml,
        "html" => ExportFormat::Html,
        _ => return String::new(),
    };
    export_payload(&tab, f).map(|(t, _, _)| t).unwrap_or_default()
}

/// 冒烟测试用：弹出结果页签的「导出…」菜单（看格式菜单齐不齐）。
pub fn open_export_menu_now(st: &Rc<PanelState>, idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(idx).cloned() else {
        return false;
    };
    let Some(pop) = tab.btn_export.popover() else {
        return false;
    };
    pop.popup();
    true
}

/// 冒烟测试用：关掉结果页签的「导出…」菜单（别在截图里留个浮层）。
pub fn close_export_menu_now(st: &Rc<PanelState>, idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(tab) = ui.tabs.borrow().get(idx).cloned() else {
        return false;
    };
    let Some(pop) = tab.btn_export.popover() else {
        return false;
    };
    pop.popdown();
    true
}

/// 冒烟测试用：打开第 `tab_idx` 个结果页签的「列显示设置」对话框。
///
/// 返回 false = 这一页没有列（对话框不会开）。
pub fn open_column_dialog_now(st: &Rc<PanelState>, tab_idx: usize) -> bool {
    let slot = st.db_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let tabs = ui.tabs.borrow();
    let Some(tab) = tabs.get(tab_idx).cloned() else {
        return false;
    };
    if tab.grid.columns().is_empty() {
        return false;
    }
    open_column_dialog(&tab);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> DbProfile {
        DbProfile {
            name: "本地 PG".into(),
            proto: DbProto::Postgres,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            dbname: "postgres".into(),
            remember: false,
        }
    }

    #[test]
    fn 连接显示带摘要() {
        // 同一个会话下可能有两条都叫「本地库」但地址不同的连接，
        // 光看名字分不清连的是哪个库 → 显示名里必须带地址摘要
        let s = conn_label_of(&profile());
        assert!(s.starts_with("本地 PG（"), "{s}");
        assert!(s.contains("127.0.0.1:5432"), "{s}");
        assert!(s.contains("postgres"), "{s}");
    }

    #[test]
    fn 编辑模式里保存与放弃必须可点() {
        // 用户 2026-10-28：「提交和回滚按钮都是禁用的，改了数据之后也点不了」。
        // 根因：这两个按钮建出来时是 sensitive(false)（没进编辑模式时不该能点），
        // 进编辑模式时**只切了 visible、忘了切 sensitive** → 两个灰按钮。
        assert_eq!(save_discard_sensitive(true), [true, true]);
        // 没进编辑模式时它们是隐藏的，也不该能点
        assert_eq!(save_discard_sensitive(false), [false, false]);
    }

    #[test]
    fn 编辑模式工具栏的可见性() {
        // 非编辑态：显示「编辑 / 插入行 / 删除行」，藏着「保存 / 放弃」
        assert_eq!(edit_toolbar_visible(false), [true, true, true, false, false]);
        // 编辑态：反过来
        assert_eq!(edit_toolbar_visible(true), [false, false, false, true, true]);
    }

    /// 阶段 83 / 审计 P2-10：三种"没结果"要说三种话。
    ///
    /// ⚠️ 说错**不会报错**，只是用户看着别扭（"还没查"说成"没有数据"，
    /// 用户以为自己查了个空库）⇒ 这条断言钉死三个分支。
    #[test]
    fn 空结果态有三种说法() {
        // 还没跑过查询：一列都没有
        let (t, d, cta) = grid_empty_text(false, false);
        assert_eq!(t, "还没有结果");
        assert!(d.contains("执行"), "{d}");
        assert!(!cta, "还没跑查询时不该给主 CTA（没有可做的动作）");

        // 跑了但一行都没有（普通查询结果 ⇒ 只读）
        let (t, d, cta) = grid_empty_text(true, false);
        assert_eq!(t, "没有结果行");
        assert!(d.contains("没有返回任何行"), "{d}");
        assert!(!cta, "只读结果没有可做的动作 ⇒ 不该给 CTA");

        // 表浏览页签：没有行，但能插一行 ⇒ 给真按钮
        let (t, d, cta) = grid_empty_text(true, true);
        assert_eq!(t, "这张表还没有数据");
        assert!(!d.is_empty());
        assert!(cta, "表浏览页签该给「插入一行」按钮");
    }

    #[test]
    fn 面包屑的拼装() {
        // 表浏览：连接 / 表@模式 / N 字段（对标 dbx 的芯片面包屑）
        assert_eq!(
            crumb_parts("本地库", "public", "users", 5),
            vec!["本地库", "users@public", "5 字段"]
        );
        // 没模式时只写表名
        assert_eq!(crumb_parts("本地库", "", "users", 3), vec!["本地库", "users", "3 字段"]);
        // 普通查询结果：没有"对象"可言
        assert_eq!(crumb_parts("本地库", "", "", 2), vec!["查询结果", "2 列"]);
        // 什么都没查到（0 列）也别拼出空的碎片
        assert_eq!(crumb_parts("", "", "", 0), vec!["查询结果", "0 列"]);
    }

    #[test]
    fn 条件chip的文字() {
        // 带值的条件：`列 运算符 值`
        let c = dbmeta::FilterCond::and("id", dbmeta::FilterOp::Eq, "3");
        assert_eq!(cond_chip_text(&c), "id = 3");
        let c = dbmeta::FilterCond::and("name", dbmeta::FilterOp::Like, "a%");
        assert_eq!(cond_chip_text(&c), "name LIKE a%");
        // ⚠️ **IS NULL 这类不带值的条件不能拼个空值出来**（看着像 bug）
        let c = dbmeta::FilterCond::and("email", dbmeta::FilterOp::IsNull, "");
        assert_eq!(cond_chip_text(&c), "email IS NULL");
        let c = dbmeta::FilterCond::and("email", dbmeta::FilterOp::IsNotNull, "");
        assert_eq!(cond_chip_text(&c), "email IS NOT NULL");
    }

    #[test]
    fn 排序方向的文字() {
        assert_eq!(order_dir_text(true), "↑ 升序");
        assert_eq!(order_dir_text(false), "↓ 降序");
    }
}
