//! SFTP worker 线程：独占持有连接，处理 UI 提交的命令并回发事件。
//!
//! 对应 C 版 sftp_panel.c 的 job 队列（GAsyncQueue + 三函数指针 Job），
//! 这里换成 `WorkerCmd`/`PanelEvent` 两个 enum + channel——
//! C 版手写的 memcpy 打包（submit_list_full / submit_simple）由类型系统取代。

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::UnboundedSender;

use crate::backend::conn::{self, SftpBackend};
use crate::backend::db::DbSession;
use crate::backend::sftp::ProgressSink;
use crate::backend::SftpEntry;
use crate::platform::localpath;
use crate::util::{path_basename, path_join};
use crate::i18n::tr;
use crate::trf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimpleOp {
    Mkdir,
    Rename,
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum XferState {
    Wait,
    Run,
    Done,
    /// 目标已存在且用户选择跳过
    Skip,
    Err,
    Cancel,
}

/// 目标已存在时的处理策略（批级选择的结果）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum XferConflict {
    Overwrite,
    Skip,
}

/// 队列项要做的事。
///
/// 镜像（6.2）除了上传还要"建目录"和"删除"，三者**按顺序**丢进同一条
/// 队列——进度、取消、批量汇总（4.3）全都现成，不用再造一套。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XferKind {
    /// 上传/下载本地与远端的路径
    Transfer,
    /// 在远端建目录；`src` 放远端绝对路径
    Mkdir,
    /// 删远端路径；`src` 放远端绝对路径
    Remove,
    /// 镜像上传：传完把远端 mtime 对齐本地（否则下次扫描又判为需更新）。
    /// `src` 是本地文件、`dst_dir` 是远端文件。
    MirrorUpload,
    /// 跨服务器传输（由 Relay 命令自己跑，不走 run_transfer 的分发）
    Relay,
}

/// UI 与 worker 共享的传输项状态（worker 写，UI 读）。
pub struct XferShared {
    pub state: XferState,
    pub done: u64,
    pub total: u64,
    pub speed: f64,
    pub cur: String,
    pub msg: Option<String>,
    pub cancel: bool,
    /// 跨服务器传输也用它做取消（exec/流转循环里检查）
    pub cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub started: Option<std::time::Instant>,
}

impl XferShared {
    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(XferShared {
            state: XferState::Wait,
            done: 0,
            total: 0,
            speed: 0.0,
            cur: String::new(),
            msg: None,
            cancel: false,
            cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            started: None,
        }))
    }
}

pub struct XferSpec {
    pub id: u64,
    pub src: String,
    pub dst_dir: String,
    pub upload: bool,
    pub kind: XferKind,
    pub conflict: XferConflict,
    pub shared: Arc<Mutex<XferShared>>,
}

/// 传给传输调度器的一批任务（新批次只是往同一个调度器里追加）。
struct TransferMsg {
    /// 提交时的并发设置；调度器每完成一项也会重新读一次，供运行时调整
    limit: usize,
    jobs: Vec<TransferJob>,
}

/// 调度器能跑的两类任务：普通文件传输与跨服务器传输。
enum TransferJob {
    File(XferSpec),
    /// 镜像的建目录/上传/删除有严格先后顺序，必须整批串行执行，
    /// 不能和其他任务一起并发。
    Serial(Vec<XferSpec>),
    Relay(crate::panel::relay::RelaySpec),
}

/// UI → worker 命令。
pub enum WorkerCmd {
    Connect {
        password: Option<String>,
        keyfile: Option<String>,
        /// 保活间隔（秒），0 = 关闭。每次连接都用当次值——
        /// 会话选项对话框改完点"重连"即生效，不用重启 worker
        keepalive_sec: u32,
        /// 认证优先级（2.2）。同样每次连接都取当次值
        auth_order: crate::args::AuthOrder,
        /// 认证方式（阶段 70）：允许用哪些凭据。同样每次连接都取当次值
        auth_method: crate::args::AuthMethod,
    },
    List {
        id: u64,
        path: String,
        silent: bool,
    },
    /// 轻量轮询：只 stat 目录 mtime，用来判断远端目录是否有增删/改名
    DirStamp { path: String },
    Simple {
        op: SimpleOp,
        path: String,
        new_path: Option<String>,
    },
    /// 终端目录跟随轮询：远程找本会话 shell 的 cwd
    Follow {
        age_sec: i64,
    },
    Transfers {
        items: Vec<XferSpec>,
    },
    /// 内置查看/编辑器：读取远端文件（`id` 由 UI 生成，回包原样带回）
    ReadFile {
        id: u64,
        path: String,
        max: u64,
    },
    /// 问一句这个远端路径（**跟随软链**）是目录还是文件。
    ///
    /// ⚠️ **为什么问**：目录列表里的软链是「软链自己」（`is_dir == false`）
    /// ⇒ 双击它会被当成文件去开编辑器 ⇒ 而服务端 stat（跟随软链）会说
    /// 「这是目录」⇒ 弹一句报错还留一个空编辑器页。⇒ 先问清楚再决定
    /// 是**进目录**还是**开编辑器**。
    ResolvePath { path: String },
    /// 内置编辑器：把修改写回远端（临时文件 + 原子改名）
    WriteFile {
        id: u64,
        path: String,
        data: Vec<u8>,
    },
    /// 上传批次预检：这些远端目标是否已存在（与 batch 下标一一对应）。
    /// `id` 由 UI 生成，回包原样带回，避免并发批次错配。
    CheckExists { id: u64, paths: Vec<String> },
    /// 导出本连接上的一条 SFTP 会话（跨服务器传输交给源 worker 用）
    ExportSftp {
        reply: async_channel::Sender<Result<russh_sftp::client::SftpSession, String>>,
    },
    /// 跨服务器传输：优先源端 scp 直连，失败回退本机中转
    Relay { spec: crate::panel::relay::RelaySpec },
    /// 转发管理 UI：新增一条规则（内部形规格，如 "L:9000:host:80"），建好即启用
    ForwardAdd { spec: String },
    /// 转发管理 UI：停止并移除一条规则（key 见 FwdView::key）
    ForwardRemove { key: String },
    /// 转发管理 UI：启用 / 停用一条规则（停用保留规则本体，可再启用）
    ForwardSetEnabled { key: String, enabled: bool },
    /// 目录镜像（6.2）扫描：本地目录 + 远端目录 + 忽略规则
    MirrorScan {
        local: String,
        remote: String,
        ignore: String,
    },
    /// 数据库通道（7.2）：在现有 SSH 连接上连到跳板机视角的数据库
    DbConnect { config: crate::backend::db::DbConfig },
    /// 数据库通道：**只测一下连不连得上**（不建会话、不动当前连接）。
    ///
    /// ⚠️ **为什么要有**：新建数据库连接的对话框要**当场验密码**——
    /// 密码错了就**留在那个对话框里**、在密码框那儿提示，而不是先打开
    /// 工作台再弹一个「请输入密码」的框（那个框很容易被错过，
    /// 错过之后用户就不知道该怎么办了）。
    ///
    /// ⚠️ 与 [`WorkerCmd::DbConnect`] 分开：后者会**顶掉**当前数据库
    /// 会话（`db.take()`），拿它当"测试"会把正在用的连接掐了。
    DbTest {
        id: u64,
        config: crate::backend::db::DbConfig,
    },
    /// 数据库通道：执行 SQL 并取第一页
    DbQuery { id: u64, sql: String, page_size: usize },
    /// 数据库通道：取下一页（PostgreSQL 游标 / MySQL 缓冲切片）
    DbNextPage { id: u64, page_size: usize },
    /// 数据库通道：取消正在执行的查询
    DbCancel,
    /// 数据库通道：关闭会话（SSH 连接与 SFTP 不受影响）
    DbClose,
    /// 数据库通道：查元数据（对象浏览器懒加载）
    DbMeta { id: u64, req: DbMetaReq },
    /// 数据库通道：执行写语句（数据编辑保存），返回影响行数
    DbExec { id: u64, sql: String },
    /// 数据库通道：事务控制（开始 / 提交 / 回滚）
    DbTx { id: u64, op: DbTxOp },
    /// 数据库通道：导入一批数据（阶段 18：CSV 导入向导）
    ///
    /// **整批在 backend 侧一个事务里执行**——UI 侧不拆批次，
    /// 拆了就不好保证"要么都进、要么都不进"。
    DbImport {
        id: u64,
        batch: crate::backend::db::ImportBatch,
    },
    /// 数据库通道：取 ER 图数据（阶段 19）
    ///
    /// **整张图在 backend 侧一次查完**：ER 图要问好几次库（模式级外键 +
    /// 每张相关表的列），拆成多条命令就会"图先出来、边后补"，也可能
    /// 中途失败留下一张残图。
    DbEr {
        id: u64,
        schema: String,
        /// `Some(表名)` = 只画这张表跟它的关系；`None` = 整个模式
        table: Option<String>,
    },
    /// 数据库通道：导出整张表到本地文件（阶段 52 的「数据传输」）
    ///
    /// **为什么在 worker 侧导出**：一次要读的行数可能很大，全塞进 UI 侧
    /// 的内存会撑爆、还会把界面卡住。在 worker 侧**边查边写**（分页查 +
    /// 分页写），UI 只等一个"导完了 / 出错了"的结果。
    DbExport {
        id: u64,
        schema: String,
        table: String,
        path: String,
        fmt: crate::backend::dbexport::ExportFmt,
    },
    Shutdown,
}

/// 数据库元数据请求（对象浏览器；展开哪个节点就查哪个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbMetaReq {
    Schemas,
    Tables { schema: String },
    Columns { schema: String, table: String },
    Indexes { schema: String, table: String },
    /// 外键（表结构页签）
    ForeignKeys { schema: String, table: String },
    /// 函数与存储过程
    Routines { schema: String },
    /// 触发器
    Triggers { schema: String },
    /// 例程的定义文本
    RoutineDef {
        schema: String,
        name: String,
        args: String,
        is_proc: bool,
    },
    /// 触发器的定义文本
    TriggerDef {
        schema: String,
        table: String,
        name: String,
    },
    /// 事件（PG 是库级的，schema 传空串；MySQL 挂在库下）
    Events { schema: String },
    /// 事件的定义文本
    EventDef { schema: String, name: String },
    /// 全库搜对象（表 / 视图 / 字段 / 函数 / 存储过程 / 触发器 / 事件）
    Search { keyword: String },
    /// 对象属性（行数 / 大小 / 列数 / 索引数 / 外键数 / 注释…）
    ObjectStats { schema: String, table: String },
    /// 一次查一个模式下**所有**表 / 视图的统计（阶段 51 的树「详细模式」）
    TableStats { schema: String },
}

impl DbMetaReq {
    /// 这类请求查的是哪个节点（用于把应答填回树里的对应节点）。
    pub fn kind_index(&self) -> usize {
        match self {
            Self::Schemas => 0,
            Self::Tables { .. } => 1,
            Self::Columns { .. } => 2,
            Self::Indexes { .. } => 3,
            // 外键不占懒加载槽位（表结构页签单独查）
            Self::ForeignKeys { .. } => 4,
            // 例程 / 触发器与表一样挂在模式下，各占一个槽位
            Self::Routines { .. } => 5,
            Self::Triggers { .. } => 6,
            Self::RoutineDef { .. } | Self::TriggerDef { .. } | Self::EventDef { .. } => 7,
            // 事件占一个懒加载槽位（8）：只有 MySQL 有，挂在库下
            Self::Events { .. } => 8,
            // 搜索不属于任何树节点，单独占一个槽位（9）
            Self::Search { .. } => 9,
            // 对象属性不属于任何树节点（弹窗里看），单独占一个槽位（10）
            Self::ObjectStats { .. } => 10,
            // 表统计是"整个模式一次查完"，挂在模式节点上（11）
            Self::TableStats { .. } => 11,
        }
    }
}

/// 数据库元数据应答。
#[derive(Debug, Clone, PartialEq)]
pub enum DbMetaPayload {
    Schemas(Vec<String>),
    Tables(Vec<crate::backend::db::DbTable>),
    Columns(Vec<crate::backend::db::DbColumn>),
    Indexes(Vec<crate::backend::db::DbIndex>),
    /// 外键（表结构页签）
    ForeignKeys(Vec<crate::backend::dbmeta::ForeignKey>),
    /// 函数与存储过程
    Routines(Vec<crate::backend::dbmeta::Routine>),
    /// 触发器
    Triggers(Vec<crate::backend::dbmeta::Trigger>),
    /// 事件
    Events(Vec<crate::backend::dbmeta::Event>),
    /// 搜索命中
    SearchHits(Vec<crate::backend::dbmeta::SearchHit>),
    /// 对象属性（行数 / 大小 / 列数…）；`None` = 对象不存在
    ObjectStats(Option<crate::backend::dbmeta::ObjectStats>),
    /// 一个模式下所有表 / 视图的统计（阶段 51 的树「详细模式」）
    TableStats(Vec<crate::backend::dbmeta::TableStat>),
    /// 定义文本（例程 / 触发器）
    Def(String),
}

/// 事务动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbTxOp {
    Begin,
    Commit,
    Rollback,
}

/// 数据库任务收到的命令（只给 DB 任务，不进 worker 命令循环）。
enum DbCmd {
    Query { id: u64, sql: String, page_size: usize },
    NextPage { id: u64, page_size: usize },
    Meta { id: u64, req: DbMetaReq },
    Exec { id: u64, sql: String },
    Tx { id: u64, op: DbTxOp },
    Import {
        id: u64,
        batch: crate::backend::db::ImportBatch,
    },
    Er {
        id: u64,
        schema: String,
        table: Option<String>,
    },
    Export {
        id: u64,
        schema: String,
        table: String,
        path: String,
        fmt: crate::backend::dbexport::ExportFmt,
    },
}

/// 运行中的数据库会话：命令入口 + 任务句柄。
///
/// 查询在独立任务里**串行**执行——不能 inline 在 worker 命令循环里等，
/// 否则一条慢查询会把 SFTP、保活、端口转发一起卡住。
struct DbHandle {
    tx: tokio::sync::mpsc::UnboundedSender<DbCmd>,
    task: tokio::task::JoinHandle<()>,
    /// PostgreSQL 取消令牌；MySQL 为 None（首版取消 = 关闭会话）
    cancel: Option<tokio_postgres::CancelToken>,
    host: String,
    port: u16,
}

impl DbHandle {
    fn stop(self) {
        self.task.abort();
    }
}

/// 一条转发规则的 UI 视图（事件快照，不做增量——规则集小，全量最省心）。
#[derive(Debug, Clone, PartialEq)]
pub struct FwdView {
    /// 启停/删除用的标识（"L:9000:host:80"），停用时依然有效
    pub key: String,
    /// 规则描述（"127.0.0.1:9000 → host:80"）
    pub text: String,
    /// 已启用（注意：启用 ≠ 在跑，启动失败见 err）
    pub enabled: bool,
    /// 正在监听
    pub active: bool,
    /// 启动失败原因；已停用或正常运行时为 None
    pub err: Option<String>,
}

impl FwdView {
    /// 运行态文案（状态栏 tooltip 与转发管理窗口共用）。
    pub fn state_text(&self) -> String {
        if let Some(e) = &self.err {
            return trf!("启动失败：{e}", e = e);
        }
        if self.active {
            tr("活动中").to_string()
        } else if self.enabled {
            tr("未在运行").to_string()
        } else {
            tr("已停用").to_string()
        }
    }

    /// 一行完整展示：描述（运行态）
    pub fn line_text(&self) -> String {
        trf!("{v0}（{v1}）", v0 = self.text, v1 = self.state_text())
    }
}

/// worker → UI 事件。
pub enum PanelEvent {
    Connected(Result<String, String>),
    /// **连接断了**（服务端关了 / 网络断了 / 空闲超时）。
    ///
    /// ⚠️ **为什么要有**（2026-11-05）：SFTP 面板和终端是**两条独立的
    /// 连接**。终端那条断了，用户点「重新连接终端」能救回来；**SFTP 那条**
    /// 断了却没人知道——面板还显示「已连接」，直到用户点「刷新」才弹一个
    /// 「读取远程目录失败」，而且还得自己想到去点「重新连接 SFTP」。
    /// worker 盯着底层会话（`SftpBackend::is_closed`）⇒ 断了就上报，
    /// UI 立刻改成「已断开」，下一次操作自动重连。
    Disconnected,
    /// 转发规则集快照 + 本次操作的失败说明（Connected 之后到达；
    /// 任何规则变动都重发完整列表）
    Forwards {
        views: Vec<FwdView>,
        /// 弹窗用的失败说明（规则自身状态在 views.err 里，不重复报）
        failed: Vec<String>,
    },
    Listed {
        id: u64,
        path: String,
        silent: bool,
        result: Result<Vec<SftpEntry>, String>,
        /// 目录 mtime（远端自动刷新用；服务器不给就是 0）
        dir_mtime: i64,
    },
    /// DirStamp 的结果
    DirStamp { path: String, mtime: i64 },
    SimpleDone {
        path: String,
        result: Result<(), String>,
    },
    /// 内置查看/编辑器：远端文件内容（或读取失败原因）
    FileRead {
        id: u64,
        path: String,
        result: Result<Vec<u8>, String>,
    },
    /// ResolvePath 的结果：这个路径（**跟随软链**）是目录还是文件。
    PathResolved {
        path: String,
        result: Result<crate::backend::PathKind, String>,
    },
    /// 内置编辑器：远端保存结果
    FileWritten {
        id: u64,
        path: String,
        result: Result<(), String>,
    },
    FollowDone(Option<String>),
    /// 传输进度有更新（去抖后批量刷新）
    XferTick,
    /// 整个队列清空：带上本轮所有结果，UI 只汇总/通知一次。
    /// 追加的新批次也在这个窗口里；只有队列与在跑任务都空了才发。
    XferDrained { results: Vec<(String, XferState)> },
    /// CheckExists 的结果（与请求 paths 下标一一对应）
    CheckExistsDone { id: u64, exists: Vec<bool> },
    /// 跨服务器传输结束（ok/失败原因；进度状态在 shared 里）
    RelayDone {
        id: u64,
        ok: bool,
        msg: String,
    },
    /// 目录镜像扫描结果（本地/远端两份清单）
    MirrorPlan {
        local: String,
        remote: String,
        result: Result<(crate::backend::mirror::Scan, crate::backend::mirror::Scan), String>,
    },
    /// 数据库通道：连接结果（Ok = `user@host:port/db` 展示串）
    DbConnected(Result<String, String>),
    /// 数据库通道：**测连**的结果（`DbTest` 的回包）。
    ///
    /// - `result` = `Ok(())` ⇒ 连得上；
    /// - `Err(msg)` ⇒ 连不上，`msg` 是后端的原文（人话）；
    /// - `needs_password` ⇒ 服务器**要密码**（这条连接没带密码），
    ///   UI 据此把光标放到**密码框**上，而不是笼统地说"连接失败"。
    DbTest {
        id: u64,
        result: Result<(), String>,
        needs_password: bool,
    },
    /// 数据库通道：**服务器要密码，但我们没带**。
    ///
    /// 单独一个变体（而不是塞进 `DbConnected(Err(..))`）：UI 见到它要
    /// **弹密码框**让用户当场输密码再重连（对标 Navicat）。混在普通错误
    /// 里就只能给用户一句"连接失败"，用户得自己猜该干嘛。
    DbNeedPassword(String),
    /// 数据库通道：查询结果（列名 + 文本单元格）
    DbRows { id: u64, rows: crate::backend::db::DbRows },
    /// 数据库通道：连接/查询失败
    DbError(String),
    /// 数据库通道：**连接断了**（阶段 99）。
    ///
    /// ⚠️ **为什么单独一个事件**：数据库的失败有两种——「连接断了」
    /// （**能自愈**：重连一次 + 重试这条操作）与「SQL 写错了 / 权限不足」
    /// （只能报错）。混在一个 `DbError(String)` 里的话，UI 只能靠**匹配
    /// 错误文案**来判断（脆，而且文案会翻译）。
    DbLost { id: u64 },
    /// 数据库通道：已关闭（用户主动断开）
    DbClosed,
    /// 数据库通道：元数据应答（req 原样带回，便于把结果填回树里的对应节点）
    DbMeta {
        id: u64,
        req: DbMetaReq,
        result: Result<DbMetaPayload, String>,
    },
    /// 数据库通道：写语句结果（影响行数）
    DbExec { id: u64, result: Result<u64, String> },
    /// 数据库通道：事务动作结果
    DbTx {
        id: u64,
        op: DbTxOp,
        result: Result<(), String>,
    },
    /// 数据库通道：导入结果（阶段 18）
    DbImport {
        id: u64,
        result: Result<crate::backend::db::ImportReport, String>,
    },
    /// 数据库通道：ER 图数据（阶段 19）
    DbEr {
        id: u64,
        result: Result<crate::backend::ermodel::ErGraph, String>,
    },
    /// 数据库通道：整表导出结果（阶段 52）
    DbExport {
        id: u64,
        result: Result<crate::backend::dbexport::ExportReport, String>,
    },
}

/// 会话是不是已经死了（没连上 / 底层句柄已关）。
fn session_dead(conn: &Option<Arc<SftpBackend>>) -> bool {
    match conn {
        None => true,
        Some(c) => c.is_closed(),
    }
}

/// 把回执发回 UI。
///
/// ⚠️ **必须先写"会话已死"标志再发**（阶段 99）：UI 收到失败事件时会读它，
/// 判断"这次失败能不能自愈"——能自愈的就**重连一次 + 重试原来的操作**，
/// 不能的才报错。晚一步写，UI 读到的就是**上次命令留下的旧值** ⇒ 该自愈
/// 的失败会被当成硬错误弹给用户（用户 2026-11-06 报的现象）。
fn send_ev(
    ui_tx: &async_channel::Sender<PanelEvent>,
    conn: &Option<Arc<SftpBackend>>,
    conn_dead: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ev: PanelEvent,
) {
    conn_dead.store(session_dead(conn), std::sync::atomic::Ordering::SeqCst);
    let _ = ui_tx.try_send(ev);
}

/// 启动 worker 线程。连接对象由该线程独占——UI 侧只发命令。
///
/// 命令循环整体跑在一个 block_on 里（而不是每条命令单独 block_on）：
/// 等待命令期间运行时也持续被驱动——russh 保活、SFTP 响应和
/// 进程内端口转发（forward.rs）都依赖这个驱动，否则 UI 空闲时
/// 转发数据流会整体停摆。
pub fn spawn_worker(
    host: String,
    port: i32,
    user: String,
    proxycmd: Option<String>,
    forwards: Vec<String>,
    inproc_forwards: Vec<String>,
    rx: Receiver<WorkerCmd>,
    ui_tx: async_channel::Sender<PanelEvent>,
    conn_dead: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("sftp-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                // 传输调度器与命令循环共享同一条连接（并发 SFTP 请求）
                let mut conn: Option<Arc<SftpBackend>> = None;
                // **连接代号**：每次重连 +1。盯着连接的那个任务靠它判断
                // 「我盯的那条已经不是当前这条了」⇒ 旧连接的断开**不**上报
                // （否则会把刚连上的新连接误判成断的）。
                let conn_gen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let mut xfer_tx: Option<UnboundedSender<TransferMsg>> = None;
                let mut xfer_task: Option<tokio::task::JoinHandle<()>> = None;
                let mut db: Option<DbHandle> = None;
                // std mpsc 接入异步循环：recv 挂到阻塞线程池上，等待期间
                // 当前线程继续驱动运行时（转发/保活不断流）
                let rx = Arc::new(Mutex::new(rx));
                let mut next_cmd = {
                    let rx = Arc::clone(&rx);
                    tokio::task::spawn_blocking(move || rx.lock().unwrap().recv())
                };

                while let Ok(Ok(cmd)) = next_cmd.await {
                    next_cmd = {
                        let rx = Arc::clone(&rx);
                        tokio::task::spawn_blocking(move || rx.lock().unwrap().recv())
                    };
                    match cmd {
                        WorkerCmd::Shutdown => {
                            if let Some(d) = db.take() {
                                d.stop();
                            }
                            break;
                        }
                        WorkerCmd::Connect {
                            password,
                            keyfile,
                            keepalive_sec,
                            auth_order,
                            auth_method,
                        } => {
                            // 重连会换掉底层 SSH 连接：先停传输调度器（它持有
                            // 旧连接 Arc），再停数据库，最后关旧连接。
                            conn_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            stop_scheduler(&mut xfer_tx, &mut xfer_task);
                            if let Some(d) = db.take() {
                                d.stop();
                            }
                            if let Some(c) = conn.take() {
                                c.close().await;
                            }
                            let pw = password.as_deref().filter(|p| !p.is_empty());
                            let kf = keyfile.as_deref().filter(|k| !k.is_empty());
                            let pc = proxycmd.as_deref().filter(|c| !c.is_empty());
                            match conn::connect(
                                &host,
                                port,
                                &user,
                                pw,
                                kf,
                                pc,
                                keepalive_sec,
                                auth_method,
                                auth_order,
                            )
                            .await
                            {
                                Ok(c) => {
                                    // ⚠️ **连上了 ⇒ 会话没死**（UI 靠这个标志判断
                                    // "这次失败能不能自愈"）。必须在发事件**之前**
                                    // 写：UI 收到事件就可能立刻读它。
                                    conn_dead.store(false, std::sync::atomic::Ordering::SeqCst);
                                    // 连通后取主目录只是「锦上添花」：服务器
                                    // 迟迟不回 realpath 时回落到 / 先让面板可用，
                                    // 不能让 UI 一直停在「正在连接」。
                                    let cwd = match tokio::time::timeout(
                                        std::time::Duration::from_secs(20),
                                        c.cwd(),
                                    )
                                    .await
                                    {
                                        Ok(Ok(d)) => d,
                                        _ => "/".into(),
                                    };
                                    let _ = ui_tx.try_send(PanelEvent::Connected(Ok(cwd)));
                                    // 先装载连接参数自带的规则，再同步完整快照给 UI
                                    // （Forwards 无条件发送：管理窗口/状态栏依赖它重置）
                                    // 两份规则集：连接参数里的（终端不承接时）
                                    // + 连接管理器配的「进程内端口映射」（始终进程内）
                                    let mut rules = rules_of(&forwards);
                                    rules.extend(rules_of(&inproc_forwards));
                                    let failed = c
                                        .apply_forward_rules(rules)
                                        .await
                                        .into_iter()
                                        .map(|(rule, why)| trf!("{rule}：{why}", rule = rule, why = why))
                                        .collect();
                                    let c = Arc::new(c);
                                    push_forwards(Some(c.as_ref()), &ui_tx, failed).await;
                                    let (tx, task) =
                                        spawn_scheduler(Arc::clone(&c), ui_tx.clone());
                                    xfer_tx = Some(tx);
                                    xfer_task = Some(task);
                                    // 盯着这条连接：断了就告诉 UI（2026-11-05）
                                    watch_connection(
                                        Arc::clone(&c),
                                        Arc::clone(&conn_gen),
                                        ui_tx.clone(),
                                    );
                                    conn = Some(c);
                                }
                                Err(e) => {
                                    conn_dead.store(true, std::sync::atomic::Ordering::SeqCst);
                                    let _ = ui_tx.try_send(PanelEvent::Connected(Err(e.0)));
                                }
                            }
                        }
                        WorkerCmd::List { id, path, silent } => {
                            let result = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c.list(&path).await.map_err(|e| e.0),
                            };
                            let dir_mtime = match (&result, &conn) {
                                (Ok(_), Some(c)) => c
                                    .sftp()
                                    .metadata(&path)
                                    .await
                                    .ok()
                                    .and_then(|m| m.mtime)
                                    .map(|t| t as i64)
                                    .unwrap_or(0),
                                _ => 0,
                            };
                            send_ev(&ui_tx, &conn, &conn_dead, PanelEvent::Listed {
                                id,
                                path,
                                silent,
                                result,
                                dir_mtime,
                            });
                        }
                        WorkerCmd::DirStamp { path } => {
                            let mtime = match &conn {
                                Some(c) => c
                                    .sftp()
                                    .metadata(&path)
                                    .await
                                    .ok()
                                    .and_then(|m| m.mtime)
                                    .map(|t| t as i64)
                                    .unwrap_or(0),
                                None => 0,
                            };
                            let _ = ui_tx.try_send(PanelEvent::DirStamp { path, mtime });
                        }
                        WorkerCmd::Simple { op, path, new_path } => {
                            let result = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => {
                                    let r = match op {
                                        SimpleOp::Mkdir => c.mkdir(&path).await,
                                        SimpleOp::Rename => {
                                            c.rename(&path, new_path.as_deref().unwrap_or_default()).await
                                        }
                                        SimpleOp::Remove => c.remove_rec(&path).await,
                                    };
                                    r.map_err(|e| e.0)
                                }
                            };
                            send_ev(
                                &ui_tx,
                                &conn,
                                &conn_dead,
                                PanelEvent::SimpleDone { path, result },
                            );
                        }
                        WorkerCmd::ResolvePath { path } => {
                            let result = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c.path_kind(&path).await.map_err(|e| e.0),
                            };
                            send_ev(
                                &ui_tx,
                                &conn,
                                &conn_dead,
                                PanelEvent::PathResolved { path, result },
                            );
                        }
                        WorkerCmd::ReadFile { id, path, max } => {
                            let result = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c
                                    .read_file_limited(&path, max)
                                    .await
                                    .map_err(|e| e.0),
                            };
                            send_ev(
                                &ui_tx,
                                &conn,
                                &conn_dead,
                                PanelEvent::FileRead {
                                    id,
                                    path,
                                    result,
                                },
                            );
                        }
                        WorkerCmd::WriteFile { id, path, data } => {
                            let result = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c
                                    .write_file_atomic(&path, &data)
                                    .await
                                    .map_err(|e| e.0),
                            };
                            send_ev(
                                &ui_tx,
                                &conn,
                                &conn_dead,
                                PanelEvent::FileWritten {
                                    id,
                                    path,
                                    result,
                                },
                            );
                        }
                        WorkerCmd::Follow { age_sec } => {
                            let path = match &conn {
                                None => None,
                                Some(c) => {
                                    let cmd = follow_cmd(age_sec);
                                    match c.exec(&cmd, 4000, 1024).await {
                                        Ok(out) => {
                                            let first = out
                                                .lines()
                                                .find(|l| !l.trim().is_empty())
                                                .unwrap_or("")
                                                .trim();
                                            if first.starts_with('/') {
                                                Some(first.to_string())
                                            } else {
                                                None
                                            }
                                        }
                                        Err(_) => None,
                                    }
                                }
                            };
                            let _ = ui_tx.try_send(PanelEvent::FollowDone(path));
                        }
                        WorkerCmd::Transfers { items } => {
                            // 镜像批次（建目录→上传→删除）有顺序依赖，
                            // 必须整批串行；普通上传/下载才能并发。
                            let jobs = if items
                                .iter()
                                .all(|it| it.kind == XferKind::Transfer)
                            {
                                items
                                    .into_iter()
                                    .map(TransferJob::File)
                                    .collect::<Vec<_>>()
                            } else {
                                vec![TransferJob::Serial(items)]
                            };
                            let msg = TransferMsg {
                                limit: crate::settings::xfer_parallel(),
                                jobs,
                            };
                            match &xfer_tx {
                                Some(tx) => {
                                    if let Err(err) = tx.send(msg) {
                                        fail_jobs(err.0.jobs, tr("传输调度器已停止"), &ui_tx);
                                    }
                                }
                                None => {
                                    fail_jobs(msg.jobs, tr("连接已断开"), &ui_tx);
                                }
                            }
                        }
                        WorkerCmd::ExportSftp { reply } => {
                            let r = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c.open_sftp().await.map_err(|e| e.0),
                            };
                            let _ = reply.try_send(r);
                        }
                        WorkerCmd::Relay { spec } => {
                            let msg = TransferMsg {
                                limit: crate::settings::xfer_parallel(),
                                jobs: vec![TransferJob::Relay(spec)],
                            };
                            match &xfer_tx {
                                Some(tx) => {
                                    if let Err(err) = tx.send(msg) {
                                        fail_jobs(err.0.jobs, tr("传输调度器已停止"), &ui_tx);
                                    }
                                }
                                None => {
                                    fail_jobs(msg.jobs, tr("连接已断开"), &ui_tx);
                                }
                            }
                        }
                        WorkerCmd::CheckExists { id, paths } => {
                            let exists = match &conn {
                                None => vec![false; paths.len()],
                                Some(c) => {
                                    let mut v = Vec::with_capacity(paths.len());
                                    for p in &paths {
                                        v.push(c.sftp().try_exists(p).await.unwrap_or(false));
                                    }
                                    v
                                }
                            };
                            let _ = ui_tx.try_send(PanelEvent::CheckExistsDone { id, exists });
                        }
                        WorkerCmd::ForwardAdd { spec } => {
                            let mut failed = Vec::new();
                            match conn.as_ref() {
                                None => failed.push(trf!("{spec}: 未连接", spec = spec)),
                                Some(c) => {
                                    if let Err(e) = c.add_forward_rule(&spec).await {
                                        failed.push(format!("{spec}: {}", e.0));
                                    }
                                }
                            }
                            push_forwards(conn.as_deref(), &ui_tx, failed).await;
                        }
                        WorkerCmd::ForwardRemove { key } => {
                            let mut failed = Vec::new();
                            match conn.as_ref() {
                                None => failed.push(tr("未连接").into()),
                                Some(c) => {
                                    if !c.remove_forward_rule(&key).await {
                                        failed.push(trf!("转发规则已不存在：{key}", key = key));
                                    }
                                }
                            }
                            push_forwards(conn.as_deref(), &ui_tx, failed).await;
                        }
                        WorkerCmd::MirrorScan { local, remote, ignore } => {
                            let ig = crate::backend::mirror::Ignore::parse(&ignore);
                            // 本地扫描也在 worker 线程上跑：几万个文件的
                            // read_dir 不该把 UI 卡住
                            let lres = crate::backend::mirror::walk_local(
                                std::path::Path::new(&local),
                                &ig,
                            );
                            let rres = match &conn {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => c.walk_dir(&remote, &ig).await,
                            };
                            let result = match (lres, rres) {
                                (Ok(l), Ok(r)) => Ok((l, r)),
                                (Err(e), _) | (_, Err(e)) => Err(e),
                            };
                            let _ = ui_tx.try_send(PanelEvent::MirrorPlan {
                                local,
                                remote,
                                result,
                            });
                        }
                        WorkerCmd::ForwardSetEnabled { key, enabled } => {
                            let mut failed = Vec::new();
                            match conn.as_ref() {
                                None => failed.push(tr("未连接").into()),
                                Some(c) => {
                                    if let Err(e) = c.set_forward_rule_enabled(&key, enabled).await
                                    {
                                        failed.push(format!("{key}: {}", e.0));
                                    }
                                }
                            }
                            push_forwards(conn.as_deref(), &ui_tx, failed).await;
                        }
                        WorkerCmd::DbTest { id, config } => {
                            // ⚠️ **只测，不建会话**：当前数据库连接不动。
                            //    （`DbConnect` 会 `db.take()` 顶掉正在用的连接，
                            //    拿它当"测试"会把用户的活连接掐了。）
                            let mut needs_password = false;
                            let result = match conn.as_ref() {
                                None => Err(tr("SSH 连接已断开").to_string()),
                                Some(c) => {
                                    match c.open_tcp_stream(&config.host, config.port).await {
                                        Err(e) => Err(e.0),
                                        Ok(stream) => match DbSession::open(stream, &config).await
                                        {
                                            Ok(_) => Ok(()),
                                            Err(e) => {
                                                needs_password = e.needs_password();
                                                Err(e.message())
                                            }
                                        },
                                    }
                                }
                            };
                            let _ = ui_tx.try_send(PanelEvent::DbTest {
                                id,
                                result,
                                needs_password,
                            });
                        }
                        WorkerCmd::DbConnect { config } => {
                            if let Some(d) = db.take() {
                                d.stop();
                            }
                            let label = format!("{}@{}:{}/{}",
                                config.user, config.host, config.port, config.dbname
                            );
                            // 连接失败分两种回包：普通失败 → DbConnected(Err)，
                            // "服务器要密码" → DbNeedPassword（UI 弹密码框）。
                            let mut need_password: Option<String> = None;
                            let result = match conn.as_ref() {
                                None => Err(tr("连接已断开").into()),
                                Some(c) => {
                                    match c.open_tcp_stream(&config.host, config.port).await {
                                        Err(e) => Err(e.0),
                                        Ok(stream) => match DbSession::open(stream, &config).await
                                        {
                                            Err(e) => {
                                                if e.needs_password() {
                                                    need_password = Some(e.message());
                                                    Err(tr("需要密码").to_string())
                                                } else {
                                                    Err(e.message())
                                                }
                                            }
                                            Ok(mut sess) => {
                                                let cancel = sess.cancel_token();
                                                let (tx, mut rx) =
                                                    tokio::sync::mpsc::unbounded_channel::<DbCmd>();
                                                let ui = ui_tx.clone();
                                                let task = tokio::spawn(async move {
                                                    while let Some(cmd) = rx.recv().await {
                                                        let ev = match cmd {
                                                            DbCmd::Query { id, sql, page_size } => {
                                                                match sess.query(&sql, page_size).await {
                                                                    Ok(rows) => PanelEvent::DbRows { id, rows },
                                                                    Err(e) => PanelEvent::DbError(e.0),
                                                                }
                                                            }
                                                            DbCmd::NextPage { id, page_size } => {
                                                                match sess.next_page(page_size).await {
                                                                    Ok(rows) => PanelEvent::DbRows { id, rows },
                                                                    Err(e) => PanelEvent::DbError(e.0),
                                                                }
                                                            }
                                                            DbCmd::Meta { id, req } => {
                                                                let r = match req.clone() {
                                                                    DbMetaReq::Schemas => sess
                                                                        .list_schemas()
                                                                        .await
                                                                        .map(DbMetaPayload::Schemas),
                                                                    DbMetaReq::Tables { schema } => sess
                                                                        .list_tables(&schema)
                                                                        .await
                                                                        .map(DbMetaPayload::Tables),
                                                                    DbMetaReq::Columns { schema, table } => sess
                                                                        .list_columns(&schema, &table)
                                                                        .await
                                                                        .map(DbMetaPayload::Columns),
                                                                    DbMetaReq::Indexes { schema, table } => sess
                                                                        .list_indexes(&schema, &table)
                                                                        .await
                                                                        .map(DbMetaPayload::Indexes),
                                                                    DbMetaReq::ForeignKeys { schema, table } => sess
                                                                        .list_foreign_keys(&schema, &table)
                                                                        .await
                                                                        .map(DbMetaPayload::ForeignKeys),
                                                                    DbMetaReq::Routines { schema } => sess
                                                                        .list_routines(&schema)
                                                                        .await
                                                                        .map(DbMetaPayload::Routines),
                                                                    DbMetaReq::Triggers { schema } => sess
                                                                        .list_triggers(&schema)
                                                                        .await
                                                                        .map(DbMetaPayload::Triggers),
                                                                    DbMetaReq::RoutineDef { schema, name, args, is_proc } => sess
                                                                        .routine_def(&schema, &name, &args, is_proc)
                                                                        .await
                                                                        .map(|d| DbMetaPayload::Def(d)),
                                                                    DbMetaReq::TriggerDef { schema, table, name } => sess
                                                                        .trigger_def(&schema, &table, &name)
                                                                        .await
                                                                        .map(|d| DbMetaPayload::Def(d)),
                                                                    DbMetaReq::Events { schema } => sess
                                                                        .list_events(&schema)
                                                                        .await
                                                                        .map(DbMetaPayload::Events),
                                                                    DbMetaReq::EventDef { schema, name } => sess
                                                                        .event_def(&schema, &name)
                                                                        .await
                                                                        .map(|d| DbMetaPayload::Def(d)),
                                                                    DbMetaReq::Search { keyword } => sess
                                                                        .search_objects(&keyword)
                                                                        .await
                                                                        .map(DbMetaPayload::SearchHits),
                                                                    DbMetaReq::ObjectStats { schema, table } => sess
                                                                        .object_stats(&schema, &table)
                                                                        .await
                                                                        .map(DbMetaPayload::ObjectStats),
                                                                    DbMetaReq::TableStats { schema } => sess
                                                                        .table_stats(&schema)
                                                                        .await
                                                                        .map(DbMetaPayload::TableStats),
                                                                };
                                                                let result = r.map_err(|e| e.0);
                                                                PanelEvent::DbMeta { id, req, result }
                                                            }
                                                            DbCmd::Exec { id, sql } => {
                                                                let result = sess.exec(&sql).await.map_err(|e| e.0);
                                                                PanelEvent::DbExec { id, result }
                                                            }
                                                            DbCmd::Tx { id, op } => {
                                                                let result = match op {
                                                                    DbTxOp::Begin => sess.begin().await,
                                                                    DbTxOp::Commit => sess.commit().await,
                                                                    DbTxOp::Rollback => sess.rollback().await,
                                                                }
                                                                .map_err(|e| e.0);
                                                                PanelEvent::DbTx { id, op, result }
                                                            }
                                                            DbCmd::Import { id, batch } => {
                                                                // **整批在 backend 侧一个事务里跑**（失败整体回滚）
                                                                let result = sess.import(&batch).await.map_err(|e| e.0);
                                                                PanelEvent::DbImport { id, result }
                                                            }
                                                            DbCmd::Er { id, schema, table } => {
                                                                // **整张图一次查完**（失败就是整张图失败，不留半张）
                                                                let result = sess.er_graph(&schema, table.as_deref()).await.map_err(|e| e.0);
                                                                PanelEvent::DbEr { id, result }
                                                            }
                                                            DbCmd::Export { id, schema, table, path, fmt } => {
                                                                // 整表导出：**在 worker 侧边查边写**（分页查 + 分页写）
                                                                let result = sess
                                                                    .export_table(&schema, &table, &path, fmt)
                                                                    .await
                                                                    .map_err(|e| e.0);
                                                                PanelEvent::DbExport { id, result }
                                                            }
                                                        };
                                                        let _ = ui.send(ev).await;
                                                    }
                                                    sess.close().await;
                                                });
                                                db = Some(DbHandle {
                                                    tx,
                                                    task,
                                                    cancel,
                                                    host: config.host.clone(),
                                                    port: config.port,
                                                });
                                                Ok(label)
                                            }
                                        },
                                    }
                                }
                            };
                            // "要密码"单独发：UI 要靠它弹密码框。
                            // ⚠️ **别把两种混着发**——DbConnected(Err) 会先
                            // 把状态栏刷成"连接失败：需要密码"，紧接着又弹框，
                            // 用户会以为连失败了两次。
                            match need_password {
                                Some(detail) => {
                                    let _ = ui_tx.try_send(PanelEvent::DbNeedPassword(detail));
                                }
                                None => {
                                    let _ = ui_tx.try_send(PanelEvent::DbConnected(result));
                                }
                            }
                        }
                        WorkerCmd::DbQuery { id, sql, page_size } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Query { id, sql, page_size }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbNextPage { id, page_size } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::NextPage { id, page_size }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbCancel => {
                            let cancel = db.as_ref().and_then(|d| d.cancel.clone());
                            match cancel {
                                None => {
                                    // MySQL 首版：关闭会话即取消
                                    if let Some(d) = db.take() {
                                        d.stop();
                                    }
                                    let _ = ui_tx.try_send(PanelEvent::DbClosed);
                                    let _ = ui_tx.try_send(PanelEvent::DbError(
                                        tr("已取消（MySQL 连接已关闭，请重新连接）").into(),
                                    ));
                                }
                                Some(tok) => {
                                    let (host, port) = match db.as_ref() {
                                        Some(d) => (d.host.clone(), d.port),
                                        None => (String::new(), 0),
                                    };
                                    match conn.as_ref() {
                                        None => {
                                            let _ = ui_tx.try_send(PanelEvent::DbError(
                                                tr("取消失败：SSH 连接已断开").into(),
                                            ));
                                        }
                                        Some(c) => {
                                            match c.open_tcp_stream(&host, port).await {
                                                Ok(stream) => {
                                                    tokio::spawn(async move {
                                                        let _ =
                                                            tok.cancel_query_raw(
                                                                stream,
                                                                tokio_postgres::NoTls,
                                                            )
                                                            .await;
                                                    });
                                                }
                                                Err(e) => {
                                                    let _ = ui_tx.try_send(PanelEvent::DbError(
                                                        trf!("取消失败：{v0}", v0 = e.0),
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        WorkerCmd::DbClose => {
                            if let Some(d) = db.take() {
                                d.stop();
                            }
                            let _ = ui_tx.try_send(PanelEvent::DbClosed);
                        }
                        WorkerCmd::DbMeta { id, req } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Meta { id, req }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbExec { id, sql } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Exec { id, sql }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbTx { id, op } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Tx { id, op }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbImport { id, batch } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Import { id, batch }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbExport { id, schema, table, path, fmt } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Export { id, schema, table, path, fmt }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                        WorkerCmd::DbEr { id, schema, table } => match db.as_ref() {
                            None => {
                                let _ = ui_tx
                                    .try_send(PanelEvent::DbError(tr("尚未连接数据库").into()));
                            }
                            Some(d) => {
                                if d.tx.send(DbCmd::Er { id, schema, table }).is_err() {
                                    db = None;
                                    // ⚠️ **连接断了**（阶段 99）：发 `DbLost`
                                    // （结构化事件，不靠匹配文案）⇒ UI 知道
                                    // 「这次失败能自愈」：重连一次 + 重试这条操作。
                                    let _ = ui_tx.try_send(PanelEvent::DbLost { id });
                                }
                            }
                        },
                    }
                }
                stop_scheduler(&mut xfer_tx, &mut xfer_task);
                if let Some(d) = db.take() {
                    d.stop();
                }
                if let Some(c) = conn.take() {
                    c.close().await;
                }
            });
            // ⚠️ **tokio 的运行时 drop 会等所有阻塞任务结束** ⇒ 命令循环里那条
            // 「等下一条命令」的 `spawn_blocking(recv)` 永远等不到（`Shutdown`
            // 之后不会再有命令了）⇒ **worker 线程退不出来**（`join()` 一直挂
            // 着）。症状：转发规则的冒烟跑完 8 步断言**全过**，却卡在收尾
            // （打不出「[9] shutdown clean」），只能被 timeout 杀掉。
            // ⇒ 给一个宽限期强制收尾（把那条阻塞任务中止掉）。
            rt.shutdown_timeout(std::time::Duration::from_millis(200));
        })
        .expect("spawn sftp-worker")
}

/// 连接参数里的转发规则（内部形字符串列表）→ 规则集。
/// 逐条走 parse_rules：坏条目就地丢弃，不牵连其余规则。
fn rules_of(forwards: &[String]) -> Vec<crate::backend::forward::FwdRule> {
    forwards
        .iter()
        .flat_map(|f| crate::backend::forward::parse_rules(f))
        .collect()
}

/// 盯着一条连接：**它断了就上报** [`PanelEvent::Disconnected`]。
///
/// ⚠️ **只报「当前这条」的断开**：连接被换掉（重连）时代号会变 ⇒ 这个任务
/// 直接退出，不把旧连接的断开报上去（否则刚连上的新连接会被误判成断的）。
fn watch_connection(
    conn: Arc<SftpBackend>,
    conn_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ui_tx: async_channel::Sender<PanelEvent>,
) {
    let my_gen = conn_gen.load(std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            // 连接已经被换掉了 ⇒ 我盯的这条没意义了，退出
            if conn_gen.load(std::sync::atomic::Ordering::SeqCst) != my_gen {
                return;
            }
            if conn.is_closed() {
                let _ = ui_tx.try_send(PanelEvent::Disconnected);
                return;
            }
        }
    });
}

/// 当前连接上的转发规则视图（含停用项）。
async fn forward_views(conn: &SftpBackend) -> Vec<FwdView> {
    conn.forward_snapshot()
        .await
        .into_iter()
        .map(|e| FwdView {
            key: e.key,
            text: e.text,
            enabled: e.enabled,
            active: e.active,
            err: e.err,
        })
        .collect()
}

/// 回发规则快照；`failed` 是本次操作的用户可见失败说明（弹窗）。
async fn push_forwards(
    conn: Option<&SftpBackend>,
    ui_tx: &async_channel::Sender<PanelEvent>,
    failed: Vec<String>,
) {
    let views = match conn {
        Some(c) => forward_views(c).await,
        None => Vec::new(),
    };
    let _ = ui_tx.try_send(PanelEvent::Forwards { views, failed });
}

/// 同一个目标（上传看远端路径、下载看本地路径）不能同时写：
/// 两批都选中同一文件时，并发写同一个 `.pp-part` 会互相截断。
/// 普通文件传输返回去重键；镜像/删除/跨服务器返回 None（各自单独处理）。
fn job_key(job: &TransferJob) -> Option<String> {
    match job {
        TransferJob::File(it) if it.kind == XferKind::Transfer => Some(format!("{}|{}",
            if it.upload { "up" } else { "down" },
            // ⚠️ 目标路径按方向拼：上传 = 远端目录 + 本地文件名；
            //    下载 = 本地目录 + 远端文件名。
            if it.upload {
                path_join(&it.dst_dir, &localpath::basename(&it.src))
            } else {
                localpath::join(&it.dst_dir, &path_basename(&it.src))
            }
        )),
        _ => None,
    }
}

/// 传输调度器：一个连接一条队列，最多 `limit` 个文件同时跑。
///
/// 新批次不另起队列，直接追加进 `queue`；只有 `queue` 与 `active` 都空了
/// 才发一次 `XferDrained`（整个队列统一汇总）。
fn spawn_scheduler(
    conn: Arc<SftpBackend>,
    ui_tx: async_channel::Sender<PanelEvent>,
) -> (UnboundedSender<TransferMsg>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TransferMsg>();
    let task = tokio::spawn(async move {
        let mut queue: VecDeque<TransferJob> = VecDeque::new();
        let mut active: FuturesUnordered<
            BoxFuture<'static, (Option<String>, Vec<(String, XferState)>)>,
        > = FuturesUnordered::new();
        // 正在写的目标键：同一目标的任务留在队列里等前一个完成
        let mut active_keys: HashSet<String> = HashSet::new();
        let mut results: Vec<(String, XferState)> = Vec::new();
        let mut limit = crate::settings::xfer_parallel();
        let mut rx_open = true;

        loop {
            if rx_open {
                loop {
                    match rx.try_recv() {
                        Ok(msg) => {
                            limit = msg.limit.clamp(1, crate::settings::XFER_PARALLEL_MAX);
                            queue.extend(msg.jobs);
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            rx_open = false;
                            break;
                        }
                    }
                }
            }
            while active.len() < limit {
                // 找队首第一个目标没被占用的任务：被占用的继续等，
                // 后面不冲突的任务可以先走
                let Some(pos) = queue.iter().position(|job| {
                    job_key(job).map_or(true, |k| !active_keys.contains(&k))
                }) else {
                    break;
                };
                let Some(job) = queue.remove(pos) else { break };
                let key = job_key(&job);
                if let Some(k) = &key {
                    active_keys.insert(k.clone());
                }
                active.push(spawn_job(Arc::clone(&conn), ui_tx.clone(), job, key));
            }

            if active.is_empty() && queue.is_empty() {
                if !results.is_empty() {
                    let _ = ui_tx.try_send(PanelEvent::XferDrained {
                        results: std::mem::take(&mut results),
                    });
                }
                if !rx_open {
                    break;
                }
                // 空闲：等下一批；收到后回到循环顶部继续装填
                match rx.recv().await {
                    Some(msg) => {
                        limit = msg.limit.clamp(1, crate::settings::XFER_PARALLEL_MAX);
                        queue.extend(msg.jobs);
                    }
                    None => rx_open = false,
                }
                continue;
            }

            if rx_open {
                tokio::select! {
                    maybe = active.next() => {
                        if let Some((key, rows)) = maybe {
                            if let Some(k) = key {
                                active_keys.remove(&k);
                            }
                            results.extend(rows);
                        }
                        // 运行时改并发数：下一个空位立刻按新值装填
                        limit = crate::settings::xfer_parallel();
                    }
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                limit = msg.limit.clamp(1, crate::settings::XFER_PARALLEL_MAX);
                                queue.extend(msg.jobs);
                            }
                            None => rx_open = false,
                        }
                    }
                }
            } else if let Some((key, rows)) = active.next().await {
                if let Some(k) = key {
                    active_keys.remove(&k);
                }
                results.extend(rows);
            }
        }
    });
    (tx, task)
}

fn spawn_job(
    conn: Arc<SftpBackend>,
    ui_tx: async_channel::Sender<PanelEvent>,
    job: TransferJob,
    key: Option<String>,
) -> BoxFuture<'static, (Option<String>, Vec<(String, XferState)>)> {
    Box::pin(async move {
        match job {
            TransferJob::File(it) => {
                let name = localpath::basename(&it.src);
                let shared = it.shared.clone();
                run_transfer(Some(conn.as_ref()), it, &ui_tx).await;
                let state = shared.lock().unwrap().state;
                (key, vec![(name, state)])
            }
            TransferJob::Serial(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    let name = localpath::basename(&it.src);
                    let shared = it.shared.clone();
                    run_transfer(Some(conn.as_ref()), it, &ui_tx).await;
                    out.push((name, shared.lock().unwrap().state));
                }
                (None, out)
            }
            TransferJob::Relay(spec) => {
                let id = spec.id;
                let shared = spec.shared.clone();
                let cancel = spec.cancel.clone();
                let name = relay_name(&spec);
                if let Ok(mut g) = shared.lock() {
                    g.state = XferState::Run;
                    g.started = Some(std::time::Instant::now());
                }
                let res = crate::panel::relay::run(Some(conn.as_ref()), spec, &ui_tx).await;
                let (ok, msg) = match &res {
                    Ok(()) => (true, tr("完成").to_string()),
                    Err(e) => (false, e.clone()),
                };
                if let Ok(mut g) = shared.lock() {
                    g.state = if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        XferState::Cancel
                    } else if ok {
                        XferState::Done
                    } else {
                        XferState::Err
                    };
                    g.msg = Some(msg.clone());
                }
                let _ = ui_tx.try_send(PanelEvent::RelayDone { id, ok, msg });
                let state = shared.lock().unwrap().state;
                (None, vec![(name, state)])
            }
        }
    })
}

fn relay_name(spec: &crate::panel::relay::RelaySpec) -> String {
    let first = spec.paths.first().map(String::as_str).unwrap_or("");
    let base = crate::util::path_basename(first);
    if spec.paths.len() > 1 {
        trf!("{base} 等 {v0} 项", base = base, v0 = spec.paths.len())
    } else {
        base.to_string()
    }
}

/// 调度器不可用（未连接/已停止）时把任务标成失败，并补一次 drain 事件，
/// 否则 UI 会永远停在“等待”。
fn fail_jobs(jobs: Vec<TransferJob>, why: &str, ui_tx: &async_channel::Sender<PanelEvent>) {
    let mut results = Vec::new();
    for job in jobs {
        match job {
            TransferJob::File(it) => {
                let name = localpath::basename(&it.src);
                if let Ok(mut g) = it.shared.lock() {
                    g.state = XferState::Err;
                    g.msg = Some(why.to_string());
                }
                results.push((name, XferState::Err));
            }
            TransferJob::Serial(items) => {
                for it in items {
                    let name = localpath::basename(&it.src);
                    if let Ok(mut g) = it.shared.lock() {
                        g.state = XferState::Err;
                        g.msg = Some(why.to_string());
                    }
                    results.push((name, XferState::Err));
                }
            }
            TransferJob::Relay(spec) => {
                let id = spec.id;
                let name = relay_name(&spec);
                if let Ok(mut g) = spec.shared.lock() {
                    g.state = XferState::Err;
                    g.msg = Some(why.to_string());
                }
                let _ = ui_tx.try_send(PanelEvent::RelayDone {
                    id,
                    ok: false,
                    msg: why.to_string(),
                });
                results.push((name, XferState::Err));
            }
        }
    }
    if !results.is_empty() {
        let _ = ui_tx.try_send(PanelEvent::XferDrained { results });
    }
}

fn stop_scheduler(
    tx: &mut Option<UnboundedSender<TransferMsg>>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    tx.take();
    if let Some(t) = task.take() {
        t.abort();
    }
}

async fn run_transfer(
    conn: Option<&SftpBackend>,
    it: XferSpec,
    ui_tx: &async_channel::Sender<PanelEvent>,
) {
    let Some(c) = conn else {
        let mut g = it.shared.lock().unwrap();
        g.state = XferState::Err;
        g.msg = Some(tr("连接已断开").into());
        return;
    };
    {
        let mut g = it.shared.lock().unwrap();
        if g.cancel {
            g.state = XferState::Cancel;
            return;
        }
        g.state = XferState::Run;
        g.started = Some(std::time::Instant::now());
    }

    let shared = it.shared.clone();
    let ui = ui_tx.clone();
    let sink = ProgressSink::new(move |done, total, cur| {
        let cancel = {
            let mut g = shared.lock().unwrap();
            g.done = done;
            g.total = total;
            if !cur.is_empty() {
                g.cur = cur.to_string();
            }
            if let Some(t0) = g.started {
                let el = t0.elapsed().as_secs_f64();
                if el > 0.3 {
                    g.speed = done as f64 / el;
                }
            }
            g.cancel
        };
        let _ = ui.try_send(PanelEvent::XferTick);
        cancel
    });

    let conflict = it.conflict;
    let note = crate::backend::sftp::XferNote::default();
    let kind = it.kind;
    let res: crate::backend::BackendResult<bool> = match kind {
        XferKind::Mkdir => c.mkdir(&it.src).await.map(|_| true),
        XferKind::Remove => c.remove_rec(&it.src).await.map(|_| true),
        XferKind::MirrorUpload => {
            c.mirror_upload(&it.src, &it.dst_dir, &sink, &note).await
        }
        XferKind::Relay => Ok(true),
        XferKind::Transfer => {
            if it.upload {
                c.upload_rec(&it.src, &it.dst_dir, &sink, conflict, &note)
                    .await
            } else {
                c.download_rec(&it.src, &it.dst_dir, &sink, conflict, &note)
                    .await
            }
        }
    };

    let mut g = it.shared.lock().unwrap();
    match res {
        Ok(false) => g.state = XferState::Skip,
        Ok(_) => {
            g.state = XferState::Done;
            // 附注（已校验 / 未校验的原因）显示在状态列
            g.msg = note.get();
        }
        Err(e) => {
            if g.cancel {
                g.state = XferState::Cancel;
            } else {
                g.state = XferState::Err;
                g.msg = Some(if e.0.is_empty() { tr("失败").into() } else { e.0 });
            }
        }
    }
    let _ = ui_tx.try_send(PanelEvent::XferTick);
}

/// 在远程找"本会话的交互 shell"并输出其 cwd：
/// 条件 = 当前用户 + pts 终端 + 纯 shell 名（-bash/zsh/...，排除堡垒机菜单程序）
///       + 启动时间与我们的 ssh 最接近（与 C 版逐字符一致）
fn follow_cmd(age_sec: i64) -> String {
    format!("P=$(ps -eo pid=,tty=,etimes=,args= 2>/dev/null | awk -v t0={age_sec} \
         '$2 ~ /^pts\\// && $4 ~ /^-?(ba|z|k|da|fi|c|tc)?sh$/ \
         {{d=$3-t0; if(d<0)d=-d; if(m==\"\"||d<m){{p=$1;m=d}}}} \
         END{{if(p!=\"\")print p}}'); \
         [ -n \"$P\" ] && readlink /proc/$P/cwd 2>/dev/null"
    )
}
