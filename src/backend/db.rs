//! 数据库通道（7.2，Rust-only）：SSH `direct-tcpip` 通道 → 本地一次性桥接
//! （PG 走 Unix 套接字、MySQL 走回环 TCP）→ 数据库驱动。支持 PostgreSQL 与 MySQL。
//!
//! **为什么还要一个本地桥**：tokio-postgres 0.7 的 `connect_raw` 是
//! 私有模块，公开 API 只接受 TCP/Unix 地址；`mysql_async` 同样没有流注入口。
//! 为了让驱动保持 stock（不 fork、不打补丁），这里在
//! `$TMPDIR/ppdb-<pid>-<n>/` 下建一个**只给本进程用**的 Unix 域套接字
//! （PG 叫 `.s.PGSQL.<port>`），把接入的连接双向泵到 SSH 通道上。于是：
//!
//! - 不占**固定**端口（不是 `ssh -L`，也不与本地服务撞端口）；
//! - 不新建 SSH 连接（通道挂在 SFTP 那条已认证连接上）；
//! - 套接字目录 0700、会话结束即删除，别的用户进不来。
//!
//! **MySQL 是例外：它走回环 TCP（随机端口、只绑 127.0.0.1）**。这不是
//! 偏好问题，是认证方式逼的：`mysql_async` 按**客户端侧**是不是 Unix 套接字
//! 决定要不要把密码明文发（套接字 = "安全连接"），而服务端那边看到的却是
//! 一条 TCP 连接（来自跳板机），只认 TLS 或服务端自己的 Unix 套接字——
//! 两边对"安全"的认知不一致，服务端一律回 `1045 Access denied`。
//! 走回环 TCP 后驱动判定为"不安全连接"，改走 `caching_sha2_password` 的
//! RSA 公钥交换（MySQL 8+ 直连时的标准流程）。详见 `open_mysql` 的注释。
//!
//! **分页**：PostgreSQL 用服务端游标（`DECLARE` / `FETCH FORWARD n`），
//! 只有单条 SELECT/WITH/VALUES/TABLE 才走游标；其它语句一次性执行。MySQL
//! 首版没有服务端游标，先一次取回（上限 2000 行）再做客户端切片——接口与 PG
//! 相同（`query` 取第一页、`next_page` 取下一页），后续可换成 `LIMIT/OFFSET`。
//!
//! **取消**：PostgreSQL 暴露 `CancelToken`，worker 在**另一条 SSH 通道**上
//! 发取消请求（不需要第二条 SSH 连接）；MySQL 首版取消 = 关闭数据库会话。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{StreamExt, TryStreamExt};
use mysql_async::prelude::*;
use mysql_async::{Conn, OptsBuilder, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, UnixListener};
use tokio_postgres::{Client, NoTls, SimpleQueryMessage, SimpleQueryStream};

use super::{BackendError, BackendResult};
use super::dbexport;
use super::dbmeta;
use super::ermodel;

/// 认证 / 建通道的兜底超时：服务器不回应时 UI 不能永远停在"连接中"。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Unix 套接字路径上限（Linux `sun_path` 108 字节，留余量）。
const SOCKET_PATH_MAX: usize = 100;
/// MySQL 首版一次取回的行数上限（分页在客户端切片）。
const MYSQL_BUFFER_LIMIT: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbProto {
    Postgres,
    MySql,
}

impl DbProto {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "pg",
            Self::MySql => "mysql",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pg" | "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" | "mariadb" => Some(Self::MySql),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::MySql => "MySQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::MySql => 3306,
        }
    }
}

/// 数据库连接参数。密码只在内存里（`Zeroizing`），不进日志/argv。
pub struct DbConfig {
    pub proto: DbProto,
    /// 从**跳板机视角**看的主机（`127.0.0.1` = 跳板机回环）
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dbname: String,
    pub password: Option<zeroize::Zeroizing<String>>,
}

/// 数据库**连接**失败的分类。
///
/// **为什么要分类**（2026-10 实测踩出来的）：PG 驱动在"服务器要密码、
/// 但这条连接没带密码"时抛的是 `tokio_postgres::Error`，`Kind::Config`，
/// 原文只有一句 **"invalid configuration"**——用户完全看不懂，也不知道
/// 该干嘛（实际案例：用户存了连接、勾了「记住密码」，但存的时候凭据库
/// 还锁着，密码没存上，一连就撞这一句）。
///
/// UI 拿到这个分类才能**弹密码框**让用户当场补密码（对标 Navicat：
/// 连接时问密码），而不是把一句英文甩过去让人猜。
#[derive(Debug, Clone)]
pub enum DbOpenError {
    /// 服务器要求密码认证，但这条连接没带密码 → UI 应问用户要密码。
    NeedPassword(String),
    /// 其它失败（原文透传）。
    Other(String),
}

impl DbOpenError {
    /// 人话文案（状态栏 / 错误框用）。
    pub fn message(&self) -> String {
        match self {
            DbOpenError::NeedPassword(m) => m.clone(),
            DbOpenError::Other(m) => m.clone(),
        }
    }

    /// 是否属于"该问用户要密码"。
    pub fn needs_password(&self) -> bool {
        matches!(self, DbOpenError::NeedPassword(_))
    }

    /// 把 tokio-postgres 的错误翻成中文分类。
    ///
    /// **PG 的坑**：`Display` 只给 "invalid configuration"，真正的原因在
    /// `source()` 里（"password missing"）。不看 source 就永远只知道那句
    /// 英文。
    ///
    /// ⚠️ **别用 `format!("{e}")`**：PG 的 `Display` 对 `DbError` 只给
    /// **"db error"** 三个字 ⇒ 用户看到的是"db error"，完全不知道是
    /// **密码错了**还是库名不对。**服务器给的原文在 `as_db_error()` 里**
    /// （见 [`pg_msg`]）。
    pub(crate) fn from_pg(e: &tokio_postgres::Error) -> Self {
        let cause = std::error::Error::source(e).map(|s| s.to_string());
        if is_missing_password_cause(cause.as_deref()) {
            DbOpenError::need_password()
        } else {
            DbOpenError::Other(pg_msg(e))
        }
    }

    /// 缺密码时给用户看的那句话（两种方言共用）。
    fn need_password() -> Self {
        DbOpenError::NeedPassword("数据库服务器要求密码认证，但这条连接没有密码。".to_string())
    }

    /// 把 mysql_async 的错误翻成中文分类。
    ///
    /// **MySQL 的坑**：1045（Access denied）有两种——压根没送密码时服务端
    /// 会在消息里写 `(using password: NO)`，密码错了则是 `YES`。
    /// 只有 `NO` 才值得弹密码框；`YES` 是密码真错了，重弹一次也是错的。
    pub(crate) fn from_mysql(e: &mysql_async::Error) -> Self {
        if let mysql_async::Error::Server(se) = e {
            if se.code == 1045 && se.message.contains("using password: NO") {
                return DbOpenError::need_password();
            }
        }
        DbOpenError::Other(format!("{e}"))
    }
}

/// tokio-postgres 里"服务端要密码、我们没给"的 cause 原文。
pub(crate) const PG_MISSING_PASSWORD: &str = "password missing";

/// 这条错误是不是「**密码不对**」（纯函数，便于单测）。
///
/// ⚠️ 为什么要在后端认：认出来才能把提示写在**密码框**那儿 ——
/// 「新建数据库连接」的对话框里，用户刚敲完密码，最需要知道的是
/// "是密码错了"还是"地址/库名不对"（笼统一句"连接失败"等于没说）。
///
/// - PG：`password authentication failed for user "x"`
/// - MySQL：`Error 1045 (28000): Access denied for user 'x'@'y' (using password: YES)`
pub(crate) fn is_password_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    // i18n:keep
    // ⚠️ 认的是**数据库服务器返回的原文**（中文版的 PG 会回「密码」）
    // ⇒ 不是本程序的界面文案，**不能翻译**。
    m.contains("password authentication failed")
        || m.contains("authentication failed")
        || m.contains("access denied")
        || m.contains("incorrect password")
        || m.contains("密码")
    // i18n:end
}

/// PG 驱动的 cause 是不是"缺密码"（纯函数，方便单测）。
///
/// **为什么要单独抽出来**：驱动的 `Error` 造不出来（`Error::config` 是
/// crate 私有的），只能拿 cause 原文来测；而"认 cause"这件事本身正是
/// 最容易写错的地方（认错了就会把普通故障也报成"要密码"）。
pub(crate) fn is_missing_password_cause(cause: Option<&str>) -> bool {
    cause == Some(PG_MISSING_PASSWORD)
}

impl std::fmt::Display for DbOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DbOpenError {}

impl From<DbOpenError> for BackendError {
    fn from(e: DbOpenError) -> Self {
        BackendError(e.message())
    }
}

/// 一批待导入的数据（阶段 18：CSV 导入向导）。
///
/// **行已经按目标列对齐**（列映射是 UI 层做的），`None` = NULL。
#[derive(Debug, Clone)]
pub struct ImportBatch {
    /// 方言（决定转义：MySQL 的 `\` 与 `'` 跟 PG 不一样）
    pub proto: DbProto,
    pub schema: String,
    pub table: String,
    /// 目标列名（导入顺序）
    pub columns: Vec<String>,
    /// 各列类型（决定转义与校验）
    pub types: Vec<dbmeta::ColType>,
    /// 数据行
    pub rows: Vec<Vec<Option<String>>>,
}

/// 导入结果。
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    /// 成功插入的行数
    pub inserted: usize,
}

/// PG 一次发多少条 INSERT（MySQL 恒为 1，见 `DbSession::import` 的注释）。
const IMPORT_CHUNK_PG: usize = 20;

/// 一次 ER 图最多画多少张表（整个模式时）。
///
/// **为什么有上限**：整个模式几百张表的话，图会大到没法看，而且每张表
/// 都要查一次列（一次往返）。与其画一张糊成一片的图，不如让用户先选
/// 一张具体的表看关系。
const ER_MAX_TABLES: usize = 80;

/// 导入前的**本地校验**：类型对不上的值要在动库之前报出来。
///
/// **为什么本地先校验一遍**：不校验的话，脏数据要等插到那一行才炸，
/// 而那时前面几百行已经插进去了（虽然会回滚，但用户等了半天换来一句
/// "第 500 行 age 不是整数"，体验很差）。本地校验是毫秒级的。
///
/// **错误带行号 + 列名**：用户要能直接定位到 CSV 的哪一格有问题。
fn validate_import(batch: &ImportBatch) -> BackendResult<()> {
    for (r, row) in batch.rows.iter().enumerate() {
        if row.len() != batch.columns.len() {
            return Err(BackendError(format!(
                "第 {} 行的列数不对（{} 个 ≠ 表的 {} 个列）",
                r + 1,
                row.len(),
                batch.columns.len()
            )));
        }
        for (c, v) in row.iter().enumerate() {
            let Some(s) = v else { continue };
            let ty = batch.types.get(c).copied().unwrap_or(dbmeta::ColType::Text);
            if let Err(e) = dbmeta::edit_literal(batch.proto, ty, s) {
                return Err(BackendError(format!(
                    "第 {} 行「{}」：{}",
                    r + 1,
                    batch.columns[c],
                    e
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct DbRows {
    /// 列名（DDL/无结果集时为空）
    pub columns: Vec<String>,
    /// **每列的类型**（阶段 64 / 审计 P0-3）。
    ///
    /// `None` = 类型未知（PG 的未知 OID / MySQL 未映射）⇒ UI 显示「—」，
    /// **不猜**：类型猜错会让排序按错的规则走、编辑时按错的格式转字面量。
    ///
    /// 与 `columns` **同序同长**。
    pub types: Vec<Option<dbmeta::ColType>>,
    /// 文本单元格；`None` = SQL NULL
    pub rows: Vec<Vec<Option<String>>>,
    /// 第几页（1 起）
    pub page: u32,
    /// 没有更多页了
    pub done: bool,
    /// 达到本次取数上限后还有剩余行（剩余行已读完丢弃）
    pub truncated: bool,
    /// 命令标签（如 "SELECT 3" / "2 行"）
    pub command: String,
    pub elapsed_ms: u64,
}

/// 一条已认证的数据库会话。
pub struct DbSession {
    inner: Option<Inner>,
    /// PG 的本地套接字目录（要活到会话结束）；MySQL 不用（走回环 TCP）
    _dir: Option<SocketDir>,
}

/// 表 / 视图（对象浏览器的一层）。
#[derive(Debug, Clone, PartialEq)]
pub struct DbTable {
    pub schema: String,
    pub name: String,
    /// 视图（不是表）
    pub is_view: bool,
}

/// 从结果里取"定义文本"。
///
/// PG 的 `pg_get_functiondef` / `pg_get_triggerdef` 返回一列；MySQL 的
/// `SHOW CREATE ...` 返回好几列（名称、sql_mode、定义、字符集…），**定义在
/// 第 3 列**。
fn pick_def_text(proto: DbProto, rows: &[Vec<Option<String>>]) -> String {
    let Some(row) = rows.first() else {
        return String::new();
    };
    match proto {
        DbProto::Postgres => row
            .first()
            .and_then(|c| c.as_deref())
            .unwrap_or_default()
            .to_string(),
        DbProto::MySql => {
            // MySQL 的 `SHOW CREATE …` 返回好几列，**定义列的位置各版本不一样**：
            // MySQL 5.7 是 `Trigger, sql_mode, Created, …, Original_sql`，
            // 8.0 是 `Trigger, sql_mode, Create Trigger, …`，
            // 新版本（26.x）还在中间插了一列 `time_zone`——
            // 按固定下标取会拿到 "Created" / "SYSTEM" 这种垃圾。
            //
            // 认准**内容**而不是位置：只有定义列长这样（CREATE TRIGGER /
            // CREATE EVENT / CREATE FUNCTION / Original_sql）。
            if let Some(d) = row.iter().find_map(|c| {
                let s = c.as_deref()?.trim();
                if s.len() > 6 && s.to_ascii_uppercase().starts_with("CREATE ") {
                    Some(s)
                } else {
                    None
                }
            }) {
                return d.to_string();
            }
            // 兜底：老下标
            row.get(2)
                .and_then(|c| c.as_deref())
                .unwrap_or_default()
                .to_string()
        }
    }
}

/// 列（对象浏览器 + 数据编辑的类型依据）。
///
/// 完整的列定义（默认值 / 长度 / PG 自定义类型）在 `dbmeta::ColumnDef` 里——
/// 结构页签要照着它拼 `CREATE TABLE`，少一项就会把表结构拼错。
pub use super::dbmeta::ColumnDef as DbColumn;

/// 索引（结构页签用；定义在 `dbmeta::IndexDef`）。
pub use super::dbmeta::IndexDef as DbIndex;

enum Inner {
    Pg(Pg),
    My(My),
}

struct Pg {
    client: Client,
    conn_task: tokio::task::JoinHandle<()>,
    bridge: tokio::task::JoinHandle<()>,
    cursor: Option<PgCursor>,
}

struct PgCursor {
    columns: Vec<String>,
    /// 列类型（第一页拿到的，后面几页照抄）
    types: Vec<Option<dbmeta::ColType>>,
    page: u32,
    started: Instant,
}

struct My {
    conn: Conn,
    bridge: tokio::task::JoinHandle<()>,
    buf: Option<MyBuffer>,
}

struct MyBuffer {
    columns: Vec<String>,
    /// 列类型（协议里的列定义）
    types: Vec<Option<dbmeta::ColType>>,
    rows: Vec<Vec<Option<String>>>,
    page: u32,
    started: Instant,
    truncated: bool,
    command: String,
}

impl DbSession {
    /// 在**已打开的 SSH 通道流**上完成数据库握手。
    ///
    /// `stream` 来自 `SftpBackend::open_tcp_stream`；内部经一次性 Unix 套接字
    /// 桥接给驱动（原因见模块注释），调用方仍然只提供一条流。
    pub async fn open<S>(stream: S, cfg: &DbConfig) -> Result<Self, DbOpenError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if cfg.port == 0 {
            return Err(DbOpenError::Other("数据库端口无效".into()));
        }
        let mut dir = None;
        let inner = match cfg.proto {
            DbProto::Postgres => {
                let d = SocketDir::new().map_err(|e| DbOpenError::Other(e.0))?;
                let inner = open_pg(stream, cfg, &d).await?;
                dir = Some(d);
                inner
            }
            // MySQL 走回环 TCP，不需要套接字目录
            DbProto::MySql => open_mysql(stream, cfg).await?,
        };
        Ok(Self {
            inner: Some(inner),
            _dir: dir,
        })
    }

    /// 执行一条（或多条）SQL，取第一页。
    pub async fn query(&mut self, sql: &str, page_size: usize) -> BackendResult<DbRows> {
        let page_size = page_size.max(1);
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => p.query(sql, page_size).await,
            Some(Inner::My(m)) => m.query(sql, page_size).await,
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 取下一页；没有下一页时返回错误（UI 应据 `done` 禁用按钮）。
    pub async fn next_page(&mut self, page_size: usize) -> BackendResult<DbRows> {
        let page_size = page_size.max(1);
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => p.next_page(page_size).await,
            Some(Inner::My(m)) => m.next_page(page_size),
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// PostgreSQL 的取消令牌；MySQL 返回 None（首版取消 = 关闭会话）。
    pub fn cancel_token(&self) -> Option<tokio_postgres::CancelToken> {
        match self.inner.as_ref() {
            Some(Inner::Pg(p)) => Some(p.client.cancel_token()),
            _ => None,
        }
    }

    /// 关闭会话：PG 回滚游标、中止桥与连接任务；MySQL 正常断开。
    pub async fn close(mut self) {
        if let Some(inner) = self.inner.take() {
            inner.close().await;
        }
    }

    /* ---------------- 元数据（对象浏览器） ---------------- */

    /// 列出模式（PG）/ 库（MySQL）。
    pub async fn list_schemas(&mut self) -> BackendResult<Vec<String>> {
        let sql = dbmeta::meta_schemas();
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_schemas(&rows))
    }

    /// 列出某模式下的表与视图。
    pub async fn list_tables(&mut self, schema: &str) -> BackendResult<Vec<DbTable>> {
        let proto = self.proto();
        let sql = dbmeta::meta_tables(proto, schema);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_tables(&rows)
            .into_iter()
            .map(|(name, is_view)| DbTable {
                schema: schema.to_string(),
                name,
                is_view,
            })
            .collect())
    }

    /// 列出某张表的列（含主键标记）。
    pub async fn list_columns(&mut self, schema: &str, table: &str) -> BackendResult<Vec<DbColumn>> {
        let proto = self.proto();
        let sql = dbmeta::meta_columns(proto, schema, table);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_column_defs(&rows))
    }

    /// 函数与存储过程。
    pub async fn list_routines(
        &mut self,
        schema: &str,
    ) -> BackendResult<Vec<dbmeta::Routine>> {
        let proto = self.proto();
        let sql = dbmeta::meta_routines(proto, schema);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_routines(&rows))
    }

    /// 触发器。
    pub async fn list_triggers(&mut self, schema: &str) -> BackendResult<Vec<dbmeta::Trigger>> {
        let proto = self.proto();
        let sql = dbmeta::meta_triggers(proto, schema);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_triggers(&rows))
    }

    /// 例程的定义文本。
    pub async fn routine_def(
        &mut self,
        schema: &str,
        name: &str,
        args: &str,
        is_proc: bool,
    ) -> BackendResult<String> {
        let proto = self.proto();
        let sql = dbmeta::meta_routine_def(proto, schema, name, args, is_proc);
        let rows = self.run_rows(&sql).await?;
        Ok(pick_def_text(proto, &rows))
    }

    /// 触发器的定义文本。
    pub async fn trigger_def(
        &mut self,
        schema: &str,
        table: &str,
        name: &str,
    ) -> BackendResult<String> {
        let proto = self.proto();
        let sql = dbmeta::meta_trigger_def(proto, schema, table, name);
        let rows = self.run_rows(&sql).await?;
        Ok(pick_def_text(proto, &rows))
    }

    /// 事件（定时任务）。**只有 MySQL 有**——PostgreSQL 压根没有这个功能
    /// （它只有 `pg_event_trigger` 那个 DDL 钩子，见
    /// `dbmeta::proto_supports_events`）。所以 PG 直接返回空列表：树上不会
    /// 出现事件节点，**也不会报「读元数据失败」**——硬查 `pg_event` 只会得到
    /// `relation "pg_event" does not exist`。
    pub async fn list_events(&mut self, schema: &str) -> BackendResult<Vec<dbmeta::Event>> {
        let proto = self.proto();
        let Some(sql) = dbmeta::meta_events(proto, schema) else {
            return Ok(Vec::new());
        };
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_events(&rows))
    }

    /// 事件的定义文本。**只有 MySQL 有**（见 `list_events`）。
    pub async fn event_def(&mut self, schema: &str, name: &str) -> BackendResult<String> {
        let proto = self.proto();
        let Some(sql) = dbmeta::meta_event_def(proto, schema, name) else {
            return Ok(String::new());
        };
        let rows = self.run_rows(&sql).await?;
        Ok(pick_def_text(proto, &rows))
    }

    /// 全库搜对象（表 / 视图 / 字段 / 函数 / 存储过程 / 触发器 / 事件）。
    ///
    /// 关键词是**子串匹配**（`LIKE '%关键词%'`），大小写不敏感；通配符
    /// （`%` / `_`）会被转义掉，当普通字符处理。
    pub async fn search_objects(
        &mut self,
        keyword: &str,
    ) -> BackendResult<Vec<dbmeta::SearchHit>> {
        let sql = dbmeta::meta_search_objects(self.proto(), keyword);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_search_objects(&rows))
    }

    /// 对象属性（对标 Navicat 的 Details 面板）。
    ///
    /// 对象不存在（被删了 / 名字错了）时返回 `None`——调用方据此提示
    /// 「对象不存在」，而不是显示一堆「—」的假属性。
    pub async fn object_stats(
        &mut self,
        schema: &str,
        table: &str,
    ) -> BackendResult<Option<dbmeta::ObjectStats>> {
        let sql = dbmeta::meta_object_stats(self.proto(), schema, table);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_object_stats(&rows))
    }

    /// 一次查一个模式下**所有**表 / 视图的统计（阶段 51 的树「详细模式」）。
    ///
    /// **为什么不是逐个表查**：`object_stats` 一张表一条 SQL——一个模式下
    /// 两百张表就是两百次往返，慢得没法用。这里按模式一次查完。
    pub async fn table_stats(&mut self, schema: &str) -> BackendResult<Vec<dbmeta::TableStat>> {
        let sql = dbmeta::meta_table_stats(self.proto(), schema);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_table_stats(&rows))
    }

    /// 外键（表结构页签用）。
    pub async fn list_foreign_keys(
        &mut self,
        schema: &str,
        table: &str,
    ) -> BackendResult<Vec<dbmeta::ForeignKey>> {
        let proto = self.proto();
        let sql = dbmeta::meta_foreign_keys(proto, schema, table);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_foreign_keys(&rows))
    }

    /// 列出某张表的索引。
    pub async fn list_indexes(
        &mut self,
        schema: &str,
        table: &str,
    ) -> BackendResult<Vec<dbmeta::IndexDef>> {
        let proto = self.proto();
        let sql = dbmeta::meta_index_defs(proto, schema, table);
        let rows = self.run_rows(&sql).await?;
        Ok(dbmeta::parse_index_defs(proto, &rows))
    }

    /* ---------------- ER 图（阶段 19） ---------------- */

    /// 取一张 ER 图的数据（表、列、外键关系）。
    ///
    /// **为什么在 backend 里一次查完**：ER 图要问好几次库（模式级外键 +
    /// 每张相关表的列）。放在后端一次做完，UI 侧只等一个结果——中途失败
    /// 也是**整张图失败**，不会出现"图画了一半、边却缺了"的怪样子。
    ///
    /// `focus` 给 `Some(表名)` = 只画这张表跟它的关系；`None` = 整个模式。
    pub async fn er_graph(
        &mut self,
        schema: &str,
        focus: Option<&str>,
    ) -> BackendResult<ermodel::ErGraph> {
        let proto = self.proto();
        // 1) 模式里所有外键（一次问清关系）
        let sql = dbmeta::meta_schema_foreign_keys(proto, schema);
        let rows = self.run_rows(&sql).await?;
        let fks: Vec<ermodel::ErFk> = dbmeta::parse_schema_foreign_keys(&rows)
            .into_iter()
            .map(|f| ermodel::ErFk {
                table: f.table,
                name: f.name,
                column: f.column,
                ref_table: f.ref_table,
                ref_column: f.ref_column,
            })
            .collect();
        // 2) 该查哪几张表的列
        let names: Vec<String> = match focus {
            Some(t) => ermodel::related_tables(t, &fks),
            None => {
                let all = self.list_tables(schema).await?;
                if all.len() > ER_MAX_TABLES {
                    return Err(BackendError(format!(
                        "这个模式里有 {} 张表，一次画不下（上限 {} 张）。\
                         先在对象树里选一张表，看它跟别人的关系。",
                        all.len(),
                        ER_MAX_TABLES
                    )));
                }
                all.into_iter().map(|t| t.name).collect()
            }
        };
        // 3) 每张表的列
        let mut tables: Vec<(String, Vec<dbmeta::ColumnDef>)> = Vec::new();
        for name in &names {
            let cols = self.list_columns(schema, name).await?;
            tables.push((name.clone(), cols));
        }
        // 4) 建图 + 布局
        let mut g = ermodel::build_graph(schema, focus, &tables, &fks);
        ermodel::layout(&mut g);
        Ok(g)
    }

    /* ---------------- 写操作（数据编辑） ---------------- */

    /// 执行写语句（UPDATE / DELETE / INSERT / DDL），返回影响行数。
    ///
    /// **不走分页游标**：写语句没有结果集，游标路径对它没有意义。
    pub async fn exec(&mut self, sql: &str) -> BackendResult<u64> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => p.exec(sql).await,
            Some(Inner::My(m)) => m.exec(sql).await,
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /* ---------------- 导入（阶段 18：CSV 导入向导） ---------------- */

    /// 把一批数据导入一张表：**整个批次一个事务**，任何一行失败就整体回滚。
    ///
    /// **为什么不拆成多个事务**：导入是"要么都进、要么都不进"的语义
    /// （Navicat 的导入向导也是整批一个事务）。拆开的话中途失败会留下
    /// 半截数据，用户得自己清理——那比"整个失败"糟糕得多。
    ///
    /// **PG 一批 20 条、MySQL 一条**：PG 的 `simple_query` 协议本来就支持
    /// 一条消息里带多条语句；**MySQL 不行**——`query_iter` 发的是一条
    /// COM_QUERY，服务端默认不允许多语句（除非客户端开了
    /// `CLIENT_MULTI_STATEMENTS`），所以 MySQL 必须逐条发。
    ///
    /// **出错定位**：PG 一批失败时先**逐行重试**找出具体是哪一行（重试
    /// 反正马上要回滚，不怕重复插）；MySQL 本来就是逐条发的，天然知道行号。
    pub async fn import(&mut self, batch: &ImportBatch) -> BackendResult<ImportReport> {
        // 先本地校验：类型对不上的值要在**动库之前**报出来，
        // 不然插到一半才发现第 500 行是脏的。
        validate_import(batch)?;
        let proto = self.proto();
        let stmts = dbmeta::build_insert_rows(
            proto,
            &batch.schema,
            &batch.table,
            &batch.columns,
            &batch.types,
            &batch.rows,
        );
        if stmts.is_empty() {
            return Err(BackendError("没有可导入的数据行".into()));
        }
        // PG 能合并发；MySQL 一条一条来
        let chunk_size = match self.inner {
            Some(Inner::Pg(_)) => IMPORT_CHUNK_PG,
            _ => 1,
        };
        self.begin().await?;
        let mut done = 0usize;
        for (ci, chunk) in stmts.chunks(chunk_size).enumerate() {
            if let Err(e) = self.exec_chunk(chunk).await {
                // 约束错误翻成中文（原文留着）
                let reason = explain_constraint_error(&e.0).unwrap_or_else(|| e.0.clone());
                // 粗定位失败 → 逐行重试，找出具体是哪一行（仅 PG 合并发送时需要）
                if chunk.len() > 1 {
                    for (i, one) in chunk.iter().enumerate() {
                        if let Err(e2) = self.exec(one).await {
                            let row = ci * chunk_size + i + 1;
                            let _ = self.rollback().await;
                            let why = explain_constraint_error(&e2.0).unwrap_or_else(|| e2.0.clone());
                            return Err(BackendError(format!("第 {row} 行导入失败：{why}")));
                        }
                    }
                }
                let _ = self.rollback().await;
                let row = ci * chunk_size + 1;
                return Err(BackendError(format!("第 {row} 行附近导入失败：{reason}")));
            }
            done += chunk.len();
        }
        self.commit().await?;
        Ok(ImportReport { inserted: done })
    }

    /// 发一批语句。
    ///
    /// PG：拼成一条消息发（`simple_query` 支持多语句）；MySQL：逐条发。
    async fn exec_chunk(&mut self, chunk: &[String]) -> BackendResult<()> {
        match self.inner.as_mut() {
            Some(Inner::Pg(_)) => {
                let sql = chunk.join("\n");
                self.exec(&sql).await.map(|_| ())
            }
            Some(Inner::My(_)) => {
                for one in chunk {
                    self.exec(one).await?;
                }
                Ok(())
            }
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 开启事务。
    pub async fn begin(&mut self) -> BackendResult<()> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => {
                // 先把可能开着的分页游标结掉（它占着一个事务）
                p.abort_cursor().await;
                p.client
                    .batch_execute("BEGIN")
                    .await
                    .map_err(|e| BackendError(format!("开启事务失败: {}", pg_msg(&e))))
            }
            Some(Inner::My(m)) => {
                m.exec("START TRANSACTION").await.map(|_| ())
            }
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 提交事务。
    pub async fn commit(&mut self) -> BackendResult<()> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => p
                .client
                .batch_execute("COMMIT")
                .await
                .map_err(|e| BackendError(format!("提交事务失败: {}", pg_msg(&e)))),
            Some(Inner::My(m)) => m.exec("COMMIT").await.map(|_| ()),
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 回滚事务。
    pub async fn rollback(&mut self) -> BackendResult<()> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => p
                .client
                .batch_execute("ROLLBACK")
                .await
                .map_err(|e| BackendError(format!("回滚事务失败: {}", pg_msg(&e)))),
            Some(Inner::My(m)) => m.exec("ROLLBACK").await.map(|_| ()),
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 当前会话的方言。
    pub fn proto(&self) -> DbProto {
        match self.inner.as_ref() {
            Some(Inner::My(_)) => DbProto::MySql,
            _ => DbProto::Postgres,
        }
    }

    /// 导出整张表到本地文件（阶段 52 的「数据传输」）。
    ///
    /// **为什么分页查而不是一次查完**：一张表可能几百万行，一次查完会把
    /// 内存和网络都撑爆。这里**一页一页查、一页一页写**（`LIMIT n OFFSET m`），
    /// 写完一页就扔。
    ///
    /// ⚠️ **不能用 PG 的服务端游标**：游标要一直开着，而导出期间用户可能
    /// 还要查别的——游标占着连接不释放。`LIMIT/OFFSET` 每页都是独立语句，
    /// 两家方言都支持。
    pub async fn export_table(
        &mut self,
        schema: &str,
        table: &str,
        path: &str,
        fmt: dbexport::ExportFmt,
    ) -> BackendResult<dbexport::ExportReport> {
        const PAGE: usize = 1000;
        let proto = self.proto();
        let mut writer = dbexport::ExportWriter::create(fmt, path, proto, schema, table)
            .map_err(BackendError)?;
        // 列信息一次拿全（表头 + 排序都用它）
        let cols = self.list_columns(schema, table).await?;
        let names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
        // ⚠️ **分页必须有稳定的行序**：`LIMIT n OFFSET m` 是"跳着取"，
        // 数据库按什么顺序吐行不固定的话，第二页可能跟第一页**重叠或漏行**
        // （导出出来就少了几行 / 重复几行）。
        // **有主键就按主键排**（最常见，也最稳）；没有主键的表只能不排序
        //（静态表上数据库的物理顺序通常也是稳定的）。
        let pk: Vec<String> = cols
            .iter()
            .filter(|c| c.is_pk)
            .map(|c| c.name.clone())
            .collect();
        let order = if pk.is_empty() {
            String::new()
        } else {
            format!(
                "ORDER BY {}",
                pk.iter()
                    .map(|c| dbmeta::quote_ident(proto, c))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        // SQL 格式：**先写建表语句**——只导出 INSERT 的话，那个脚本在空库里
        // 跑不了，"搬数据"就搬不成。
        if fmt == dbexport::ExportFmt::Sql {
            let fks = self.list_foreign_keys(schema, table).await.unwrap_or_default();
            let ddl = dbmeta::build_ddl(proto, schema, table, &cols, &fks);
            writer.write_ddl(&ddl).map_err(BackendError)?;
        }
        let mut off = 0usize;
        let mut wrote_header = false;
        loop {
            let sql = dbmeta::build_select_table_page_order(
                proto, schema, table, PAGE, off, "", &order,
            );
            let (page_cols, rows) = self.run_rows_cols(&sql).await?;
            if rows.is_empty() {
                break;
            }
            if !wrote_header {
                writer.write_header(&page_cols).map_err(BackendError)?;
                wrote_header = true;
            }
            for r in &rows {
                writer.write_row(r).map_err(BackendError)?;
            }
            off += rows.len();
            if rows.len() < PAGE {
                break;
            }
        }
        if !wrote_header {
            // 空表也得写表头（不然导出来是 0 字节，Excel 打不开）
            writer.write_header(&names).map_err(BackendError)?;
        }
        let mut rep = writer.finish().map_err(BackendError)?;
        rep.path = path.to_string();
        Ok(rep)
    }

    /// 跑一条只读查询，把全部行取回来（元数据用；不走分页游标）。
    async fn run_rows(&mut self, sql: &str) -> BackendResult<Vec<Vec<Option<String>>>> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => pg_rows(&p.client, sql).await,
            Some(Inner::My(m)) => my_rows(&mut m.conn, sql).await,
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }

    /// 跑一条只读查询，把**列名 + 全部行**取回来（整表导出时用）。
    ///
    /// ⚠️ **为什么不用 `run_rows`**：它只给行不给列名——CSV 的表头、SQL 的
    /// INSERT 列清单都要列名。
    async fn run_rows_cols(
        &mut self,
        sql: &str,
    ) -> BackendResult<(Vec<String>, Vec<Vec<Option<String>>>)> {
        match self.inner.as_mut() {
            Some(Inner::Pg(p)) => pg_rows_cols(&p.client, sql).await,
            Some(Inner::My(m)) => my_rows_cols(&mut m.conn, sql).await,
            None => Err(BackendError("数据库会话已关闭".into())),
        }
    }
}

impl Inner {
    async fn close(self) {
        match self {
            Inner::Pg(mut p) => {
                p.abort_cursor().await;
                p.conn_task.abort();
                p.bridge.abort();
            }
            Inner::My(m) => {
                let _ = m.conn.disconnect().await;
                m.bridge.abort();
            }
        }
    }
}

impl Drop for DbSession {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            match inner {
                Inner::Pg(p) => {
                    p.conn_task.abort();
                    p.bridge.abort();
                }
                Inner::My(m) => {
                    m.bridge.abort();
                }
            }
        }
    }
}

/* ---------------- 连接 ---------------- */

fn spawn_bridge<S>(listener: UnixListener, stream: S) -> tokio::task::JoinHandle<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut stream = stream;
        let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
    })
}

/// 回环 TCP 版的桥（MySQL 用，原因见 `open_mysql`）。
fn spawn_bridge_tcp<S>(listener: TcpListener, stream: S) -> tokio::task::JoinHandle<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut stream = stream;
        let _ = tokio::io::copy_bidirectional(&mut sock, &mut stream).await;
    })
}

fn check_path(path: &std::path::Path) -> BackendResult<()> {
    if path.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(BackendError(format!(
            "本地套接字路径过长（{} 字节）：{}",
            path.as_os_str().len(),
            path.display()
        )));
    }
    Ok(())
}

async fn open_pg<S>(stream: S, cfg: &DbConfig, dir: &SocketDir) -> Result<Inner, DbOpenError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let sock = dir.path.join(format!(".s.PGSQL.{}", cfg.port));
    check_path(&sock).map_err(|e| DbOpenError::Other(e.0))?;
    let listener = UnixListener::bind(&sock)
        .map_err(|e| DbOpenError::Other(format!("创建本地数据库套接字失败: {e}")))?;
    let bridge = spawn_bridge(listener, stream);

    let mut pg = tokio_postgres::Config::new();
    pg.host_path(&dir.path).port(cfg.port).user(&cfg.user);
    if !cfg.dbname.is_empty() {
        pg.dbname(&cfg.dbname);
    }
    if let Some(p) = cfg.password.as_deref() {
        pg.password(p.as_str());
    }
    let (client, connection) = match tokio::time::timeout(CONNECT_TIMEOUT, pg.connect(NoTls)).await
    {
        Err(_) => {
            bridge.abort();
            return Err(DbOpenError::Other(
                "数据库认证超时（服务器未完成握手）".into(),
            ));
        }
        Ok(Err(e)) => {
            bridge.abort();
            // ⚠️ **别把驱动原文直接甩给用户**：PG 的"缺密码"原文是
            // "invalid configuration"，用户看不懂也不知道该干嘛
            // （详见 `DbOpenError` 的注释）。
            return Err(DbOpenError::from_pg(&e));
        }
        Ok(Ok(v)) => v,
    };
    let conn_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(Inner::Pg(Pg {
        client,
        conn_task,
        bridge,
        cursor: None,
    }))
}

/// MySQL 的桥走**回环 TCP**（不是 Unix 套接字）——这不是图省事，是必须：
///
/// `mysql_async` 判断"这条连接安不安全"看的是**客户端侧**的流类型：
/// 客户端连的是 Unix 套接字 → 它认为连接安全 → 密码**明文**发；
/// 而**服务端**那边看到的是一条 TCP 连接（来自跳板机），服务端只认
/// "TLS 或服务端的 Unix 套接字"才算安全，明文密码一律
/// `1045 Access denied`（MySQL 8+ 的 `caching_sha2_password` 就是这么规定的）。
///
/// 两边对"安全"的认知不一致 → 认证必挂。实测：容器里的 MySQL 8/26，
/// 容器内 `mysql -uroot -p...`（走服务端 Unix 套接字）能连，
/// 换成经 SSH 通道的 TCP 就 1045。
///
/// 让驱动走**回环 TCP**（127.0.0.1，端口由系统随机分配）之后，
/// 驱动判定为"不安全连接"，改走 `caching_sha2_password` 的
/// **RSA 公钥交换**——这正是直连 MySQL 8+ 时的标准流程，服务端也认。
///
/// 代价：本地多占一个**回环**端口（随机端口、只绑 127.0.0.1、
/// 会话结束即释放，不与任何服务撞端口）。PostgreSQL 不受影响：
/// 它的 md5/SCRAM 认证不看传输是否安全，仍然走 Unix 套接字。
async fn open_mysql<S>(stream: S, cfg: &DbConfig) -> Result<Inner, DbOpenError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 只绑回环：不对外暴露，也不占固定端口
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| DbOpenError::Other(format!("创建本地数据库桥接端口失败: {e}")))?;
    let local_port = listener
        .local_addr()
        .map_err(|e| DbOpenError::Other(format!("读取本地数据库桥接端口失败: {e}")))?
        .port();
    let bridge = spawn_bridge_tcp(listener, stream);

    let mut opts = OptsBuilder::default()
        .ip_or_hostname("127.0.0.1")
        .tcp_port(local_port)
        .user(Some(cfg.user.clone()));
    if !cfg.dbname.is_empty() {
        opts = opts.db_name(Some(cfg.dbname.clone()));
    }
    if let Some(p) = cfg.password.as_deref() {
        opts = opts.pass(Some(p.to_string()));
    }
    let conn = match tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts)).await {
        Err(_) => {
            bridge.abort();
            return Err(DbOpenError::Other(
                "数据库认证超时（MySQL 未完成握手）".into(),
            ));
        }
        Ok(Err(e)) => {
            bridge.abort();
            return Err(DbOpenError::from_mysql(&e));
        }
        Ok(Ok(c)) => c,
    };
    Ok(Inner::My(My {
        conn,
        bridge,
        buf: None,
    }))
}

/* ---------------- PostgreSQL ---------------- */

impl Pg {
    async fn query(&mut self, sql: &str, page_size: usize) -> BackendResult<DbRows> {
        self.abort_cursor().await;
        let t0 = Instant::now();
        if let Some(sel) = single_select(sql) {
            // ⚠️ **列类型拿不到**（阶段 64 实测）：tokio-postgres 的**简单查询
            // 协议**（`simple_query_raw`）解析出来的 `SimpleColumn` **只有列名**
            // ——行描述里的类型 OID 在解析时被丢掉了 ⇒ 简单查询拿不到类型。
            //
            // 绕法：单独 `prepare` 一次（Parse+Describe+Sync，**不执行、不传
            // 行**）问一句"这条语句返回什么类型的列"。代价是**多一个来回**。
            let types = self.column_types(sel).await;
            if self.start_cursor(sel).await.is_ok() {
                self.cursor = Some(PgCursor {
                    columns: Vec::new(),
                    types,
                    page: 0,
                    started: t0,
                });
                return self.fetch_page(page_size).await;
            }
            return self.one_shot(sql, page_size, t0, types).await;
        }
        // 不是单条 SELECT（DDL / 多语句）⇒ 没有结果列，类型不用问
        self.one_shot(sql, page_size, t0, Vec::new()).await
    }

    /// 问一条语句的列类型（PG）。
    ///
    /// 失败（多语句 / 带占位符 / 语法错）⇒ 返回空 ⇒ UI 显示「—」。
    /// **不猜**：类型猜错会让排序按错的规则走、编辑时按错的格式转字面量。
    async fn column_types(&self, sql: &str) -> Vec<Option<dbmeta::ColType>> {
        match self.client.prepare(sql).await {
            Ok(st) => st
                .columns()
                .iter()
                .map(|c| dbmeta::ColType::from_pg_oid(c.type_().oid()))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    async fn abort_cursor(&mut self) {
        if self.cursor.take().is_some() {
            let _ = self.client.batch_execute("ROLLBACK").await;
        }
    }

    async fn cancel_cursor_after_error(&mut self) {
        if self.cursor.take().is_some() {
            let _ = self.client.batch_execute("ROLLBACK").await;
        }
    }

    async fn start_cursor(&mut self, sql: &str) -> BackendResult<()> {
        self.client
            .batch_execute("BEGIN")
            .await
            .map_err(|e| BackendError(format!("开启分页查询失败: {}", pg_msg(&e))))?;
        let decl = format!("DECLARE _pp_cursor NO SCROLL CURSOR FOR {sql}");
        if let Err(e) = self.client.batch_execute(&decl).await {
            let _ = self.client.batch_execute("ROLLBACK").await;
            return Err(BackendError(format!("声明游标失败: {}", pg_msg(&e))));
        }
        Ok(())
    }

    async fn fetch_page(&mut self, page_size: usize) -> BackendResult<DbRows> {
        let stream = match self
            .client
            .simple_query_raw(&format!("FETCH FORWARD {page_size} FROM _pp_cursor"))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                self.cancel_cursor_after_error().await;
                return Err(BackendError(format!("读取结果失败: {}", pg_msg(&e))));
            }
        };
        let (cols, rows, truncated, _cmd) = match collect_pg_rows(stream, page_size).await {
            Ok(v) => v,
            Err(e) => {
                self.cancel_cursor_after_error().await;
                return Err(e);
            }
        };

        let Some(c) = self.cursor.as_mut() else {
            return Err(BackendError("分页游标已丢失".into()));
        };
        if c.columns.is_empty() {
            c.columns = cols;
            // ⚠️ **别在这里覆盖列类型**：游标的类型是 `prepare` 问来的
            // （简单查询协议给不了），这一路拿到的是空。
        }
        c.page += 1;
        let page = c.page;
        let columns = c.columns.clone();
        let col_types = c.types.clone();
        let started = c.started;
        let done = rows.len() < page_size || truncated;
        if done {
            let _ = self.client.batch_execute("COMMIT").await;
            self.cursor = None;
        }
        Ok(DbRows {
            columns,
            types: col_types,
            rows,
            page,
            done,
            truncated: false,
            command: String::new(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn next_page(&mut self, page_size: usize) -> BackendResult<DbRows> {
        if self.cursor.is_none() {
            return Err(BackendError("没有更多结果".into()));
        }
        self.fetch_page(page_size).await
    }

    async fn one_shot(
        &mut self,
        sql: &str,
        page_size: usize,
        t0: Instant,
        types: Vec<Option<dbmeta::ColType>>,
    ) -> BackendResult<DbRows> {
        let stream = self
            .client
            .simple_query_raw(sql)
            .await
            .map_err(|e| BackendError(format!("查询失败: {}", pg_msg(&e))))?;
        let (columns, rows, truncated, command) = collect_pg_rows(stream, page_size).await?;
        Ok(DbRows {
            columns,
            types,
            rows,
            page: 1,
            done: true,
            truncated,
            command,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }

    /// 写语句：返回影响行数。
    ///
    /// PG 的 `simple_query` 对 UPDATE 回 `CommandComplete(n)`，n 就是
    /// 影响行数（tokio-postgres 已经替我们把 "UPDATE 3" 里的数字取出来了）。
    async fn exec(&mut self, sql: &str) -> BackendResult<u64> {
        let msgs = self
            .client
            .simple_query(sql)
            .await
            .map_err(|e| BackendError(format!("执行失败: {}", pg_msg(&e))))?;
        let mut affected = 0u64;
        for m in msgs {
            if let SimpleQueryMessage::CommandComplete(n) = m {
                affected = n;
            }
        }
        Ok(affected)
    }
}

/// PostgreSQL 错误优先显示服务器给的 message（`Error::to_string` 只有 "db error"）。
fn pg_msg(e: &tokio_postgres::Error) -> String {
    e.as_db_error()
        .map(|d| d.message().to_string())
        .unwrap_or_else(|| e.to_string())
}

/// 把常见的**数据库约束错误**翻成中文。
///
/// **为什么要翻**：PG / MySQL 的报错原文都是英文——用户看到
/// `duplicate key value violates unique constraint "t_pkey"` 只会来问
/// "这是什么意思"。中文只是**解释**，**原文要留着**（后面查资料用得上，
/// 也能看出是哪个约束）。
///
/// 认不出来的返回 `None`（照原样透传，别硬凑）。
pub fn explain_constraint_error(msg: &str) -> Option<String> {
    let m = msg.to_ascii_lowercase();
    let zh = if m.contains("duplicate key") || m.contains("duplicate entry") {
        "违反唯一约束（值重复了）"
    } else if m.contains("not-null constraint") || m.contains("cannot be null") {
        "违反非空约束（这一列不能为空）"
    } else if m.contains("foreign key") {
        "违反外键约束（引用的行不存在，或还有别的行引用着它）"
    } else if m.contains("value too long") || m.contains("data too long") {
        "值太长，超出了列的类型长度"
    } else if m.contains("invalid input syntax") || m.contains("incorrect integer") {
        "值的格式不对（跟列的类型对不上）"
    } else if m.contains("division by zero") {
        "除数为零"
    } else {
        return None;
    };
    Some(format!("{zh}（原文：{msg}）"))
}

/// 单条 SELECT 类语句才走服务端游标；含分号（多语句）时退回一次性执行。
fn single_select(sql: &str) -> Option<&str> {
    let s = sql.trim();
    let s = s.strip_suffix(';').map(str::trim).unwrap_or(s);
    if s.is_empty() || s.contains(';') {
        return None;
    }
    let head = s.split_whitespace().next()?.to_ascii_lowercase();
    matches!(head.as_str(), "select" | "with" | "values" | "table").then_some(s)
}

/// 收 PG 简单查询的结果。
///
/// ⚠️ **这里拿不到列类型**（阶段 64 实测）：tokio-postgres 的**简单查询
/// 协议**解析行描述时只留了**列名**——协议里的类型 OID 被丢掉了
/// （`SimpleColumn` 结构体里压根没有类型字段）。
/// ⇒ 列类型走 `Pg::column_types()`（`prepare` 一次）。
async fn collect_pg_rows(
    stream: SimpleQueryStream,
    limit: usize,
) -> BackendResult<(Vec<String>, Vec<Vec<Option<String>>>, bool, String)> {
    let mut stream = std::pin::pin!(stream);
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut command = String::new();
    while let Some(msg) = stream
        .try_next()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {}", pg_msg(&e))))?
    {
        match msg {
            SimpleQueryMessage::RowDescription(cols) => {
                columns = cols.iter().map(|c| c.name().to_string()).collect();
            }
            SimpleQueryMessage::Row(row) => {
                if rows.len() < limit {
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    rows.push(
                        (0..row.len())
                            .map(|i| row.get(i).map(ToOwned::to_owned))
                            .collect(),
                    );
                } else {
                    truncated = true;
                }
            }
            SimpleQueryMessage::CommandComplete(n) => {
                command = format!("{n} 行");
            }
            _ => {}
        }
    }
    Ok((columns, rows, truncated, command))
}

/* ---------------- MySQL ---------------- */

/// MySQL 的**线协议类型码** → 类型（阶段 64）。
///
/// **认不出来返回 `None`**（= 类型未知），**不猜**。
///
/// ⚠️ MySQL 的 `BOOLEAN` 就是 `TINYINT(1)`，而**线协议里分不出来**
/// （协议不带显示长度）⇒ 这里按整数算。PG 的布尔是真的 `bool`（OID 16），
/// 显示「是/否」没问题。
fn mysql_col_type(ct: mysql_async::consts::ColumnType) -> Option<dbmeta::ColType> {
    use mysql_async::consts::ColumnType as T;
    Some(match ct {
        T::MYSQL_TYPE_TINY
        | T::MYSQL_TYPE_SHORT
        | T::MYSQL_TYPE_LONG
        | T::MYSQL_TYPE_LONGLONG
        | T::MYSQL_TYPE_INT24
        | T::MYSQL_TYPE_YEAR => dbmeta::ColType::Int,
        T::MYSQL_TYPE_FLOAT | T::MYSQL_TYPE_DOUBLE | T::MYSQL_TYPE_DECIMAL
        | T::MYSQL_TYPE_NEWDECIMAL => dbmeta::ColType::Float,
        T::MYSQL_TYPE_VARCHAR
        | T::MYSQL_TYPE_VAR_STRING
        | T::MYSQL_TYPE_STRING
        | T::MYSQL_TYPE_ENUM
        | T::MYSQL_TYPE_SET
        | T::MYSQL_TYPE_JSON
        | T::MYSQL_TYPE_DATE
        | T::MYSQL_TYPE_NEWDATE
        | T::MYSQL_TYPE_DATETIME
        | T::MYSQL_TYPE_DATETIME2
        | T::MYSQL_TYPE_TIMESTAMP
        | T::MYSQL_TYPE_TIMESTAMP2
        | T::MYSQL_TYPE_TIME
        | T::MYSQL_TYPE_TIME2 => dbmeta::ColType::Text,
        // 二进制 / 位串 / 几何：按文本展示会骗人 ⇒ 标"未知"
        T::MYSQL_TYPE_TINY_BLOB
        | T::MYSQL_TYPE_MEDIUM_BLOB
        | T::MYSQL_TYPE_LONG_BLOB
        | T::MYSQL_TYPE_BLOB
        | T::MYSQL_TYPE_BIT
        | T::MYSQL_TYPE_GEOMETRY => dbmeta::ColType::Other,
        _ => return None,
    })
}

impl My {
    async fn query(&mut self, sql: &str, page_size: usize) -> BackendResult<DbRows> {
        self.buf = None;
        let t0 = Instant::now();
        let mut qr = self
            .conn
            .query_iter(sql)
            .await
            .map_err(|e| BackendError(format!("查询失败: {}", e)))?;
        let columns: Vec<String> = qr
            .columns_ref()
            .iter()
            .map(|c| c.name_str().to_string())
            .collect();
        // ⚠️ **协议里就带着列类型**（阶段 64）：`Column::column_type()` 是
        // MySQL 的线协议类型码 ⇒ 不用再猜。
        let types: Vec<Option<dbmeta::ColType>> = qr
            .columns_ref()
            .iter()
            .map(|c| mysql_col_type(c.column_type()))
            .collect();

        let mut all: Vec<Vec<Option<String>>> = Vec::new();
        let mut truncated = false;
        loop {
            match qr.next().await {
                Ok(Some(row)) => {
                    if all.len() < MYSQL_BUFFER_LIMIT {
                        all.push(my_row(&row));
                    } else {
                        truncated = true;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = qr.drop_result().await;
                    return Err(BackendError(format!("读取结果失败: {}", e)));
                }
            }
        }
        let affected = qr.affected_rows();
        qr.drop_result()
            .await
            .map_err(|e| BackendError(format!("读取结果失败: {}", e)))?;

        let command = if columns.is_empty() {
            format!("{affected} 行")
        } else {
            String::new()
        };
        self.buf = Some(MyBuffer {
            columns,
            types,
            rows: all,
            page: 0,
            started: t0,
            truncated,
            command,
        });
        self.next_page(page_size)
    }

    fn next_page(&mut self, page_size: usize) -> BackendResult<DbRows> {
        let Some(b) = self.buf.as_mut() else {
            return Err(BackendError("没有更多结果".into()));
        };
        let start = b.page as usize * page_size;
        let end = (start + page_size).min(b.rows.len());
        let rows = if start < b.rows.len() {
            b.rows[start..end].to_vec()
        } else {
            Vec::new()
        };
        b.page += 1;
        let done = end >= b.rows.len();
        let page = b.page;
        Ok(DbRows {
            columns: b.columns.clone(),
            types: b.types.clone(),
            rows,
            page,
            done,
            truncated: done && b.truncated,
            command: b.command.clone(),
            elapsed_ms: b.started.elapsed().as_millis() as u64,
        })
    }

    /// 写语句：返回影响行数。
    ///
    /// MySQL 的 `query_iter` 对 UPDATE 返回空结果集，`affected_rows()`
    /// 给出改动行数——顺带也用于事务语句（START TRANSACTION 等）。
    async fn exec(&mut self, sql: &str) -> BackendResult<u64> {
        let mut qr = self
            .conn
            .query_iter(sql)
            .await
            .map_err(|e| BackendError(format!("执行失败: {e}")))?;
        while let Some(row) = qr
            .next()
            .await
            .map_err(|e| BackendError(format!("读取结果失败: {e}")))?
        {
            // 写语句没有结果集；真出了行（不该有）也把内容吃掉，别留下未读结果
            let _ = row;
        }
        let affected = qr.affected_rows();
        qr.drop_result()
            .await
            .map_err(|e| BackendError(format!("读取结果失败: {e}")))?;
        Ok(affected)
    }
}

/// 跑一条只读查询并取回全部行 + 列名（PG）。
async fn pg_rows_cols(
    client: &Client,
    sql: &str,
) -> BackendResult<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|e| BackendError(format!("查询失败: {}", pg_msg(&e))))?;
    let mut stream = std::pin::pin!(stream);
    let mut cols: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    while let Some(msg) = stream
        .try_next()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {}", pg_msg(&e))))?
    {
        match msg {
            SimpleQueryMessage::RowDescription(desc) => {
                cols = desc.iter().map(|c| c.name().to_string()).collect();
            }
            SimpleQueryMessage::Row(row) => {
                rows.push((0..row.len()).map(|i| row.get(i).map(ToOwned::to_owned)).collect());
            }
            _ => {}
        }
    }
    Ok((cols, rows))
}

/// 跑一条只读查询并取回全部行 + 列名（MySQL）。
async fn my_rows_cols(
    conn: &mut Conn,
    sql: &str,
) -> BackendResult<(Vec<String>, Vec<Vec<Option<String>>>)> {
    let mut qr = conn
        .query_iter(sql)
        .await
        .map_err(|e| BackendError(format!("查询失败: {e}")))?;
    // ⚠️ `QueryResult::columns()` 返回的是 **Option**——写 `qr.columns().iter()`
    // 迭代出来的是 `&Arc<[Column]>`（Option 的 iter），列名就取不到了。
    let cols: Vec<String> = match qr.columns() {
        Some(cs) => cs.iter().map(|c| c.name_str().to_string()).collect(),
        None => Vec::new(),
    };
    let mut rows = Vec::new();
    while let Some(row) = qr
        .next()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {e}")))?
    {
        rows.push(my_row(&row));
    }
    qr.drop_result()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {e}")))?;
    Ok((cols, rows))
}

/// 跑一条只读查询并取回全部行（PG）。
///
/// 元数据查询不用分页游标——一次拿完，解析在 `dbmeta` 里做。
async fn pg_rows(client: &Client, sql: &str) -> BackendResult<Vec<Vec<Option<String>>>> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|e| BackendError(format!("查询失败: {}", pg_msg(&e))))?;
    let mut stream = std::pin::pin!(stream);
    let mut rows = Vec::new();
    while let Some(msg) = stream
        .try_next()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {}", pg_msg(&e))))?
    {
        if let SimpleQueryMessage::Row(row) = msg {
            rows.push((0..row.len()).map(|i| row.get(i).map(ToOwned::to_owned)).collect());
        }
    }
    Ok(rows)
}

/// 跑一条只读查询并取回全部行（MySQL）。
async fn my_rows(conn: &mut Conn, sql: &str) -> BackendResult<Vec<Vec<Option<String>>>> {
    let mut qr = conn
        .query_iter(sql)
        .await
        .map_err(|e| BackendError(format!("查询失败: {e}")))?;
    let mut rows = Vec::new();
    while let Some(row) = qr
        .next()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {e}")))?
    {
        rows.push(my_row(&row));
    }
    qr.drop_result()
        .await
        .map_err(|e| BackendError(format!("读取结果失败: {e}")))?;
    Ok(rows)
}

fn my_row(row: &mysql_async::Row) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| row.as_ref(i).and_then(my_value_text))
        .collect()
}

fn my_value_text(v: &Value) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Double(d) => Some(d.to_string()),
        Value::Date(y, mo, d, h, mi, s, us) => Some(format!(
            "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{us:06}"
        )),
        Value::Time(neg, d, h, mi, s, us) => Some(format!(
            "{}{d:02} {h:02}:{mi:02}:{s:02}.{us:06}",
            if *neg { "-" } else { "" }
        )),
    }
}

/* ---------------- 套接字目录 ---------------- */

/// 一次性套接字目录：0700，Drop 时整目录删除。
struct SocketDir {
    path: PathBuf,
}

impl SocketDir {
    fn new() -> BackendResult<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("ppdb-{}-{nanos:09}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| BackendError(format!("创建数据库套接字目录失败: {e}")))?;
        Ok(Self { path })
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「是不是密码错了」要认得准 —— 认错了就把提示写错地方
    /// （用户会去改密码，而真正的问题是库名/地址）。
    #[test]
    fn 认得出密码错误() {
        // PG
        assert!(is_password_error(
            "password authentication failed for user \"postgres\""
        ));
        // MySQL
        assert!(is_password_error(
            "Error 1045 (28000): Access denied for user 'root'@'10.0.0.1' (using password: YES)"
        ));
        // 中文原文
        assert!(is_password_error("密码不对"));
        // ⚠️ **别的错不能当成密码错**
        assert!(!is_password_error("database \"demo\" does not exist"));
        assert!(!is_password_error("连接已断开"));
        assert!(!is_password_error("timeout"));
    }

    /// 导入前的本地校验：脏数据要在**动库之前**被拦下，
    /// 而且错误要能直接定位到 CSV 的哪一格。
    /// 数据库的约束报错是英文的，要翻成中文才看得懂；
    /// **原文要留着**（查资料、找是哪个约束）。
    /// MySQL 的线协议类型码 → 类型（阶段 64）。
    ///
    /// MySQL 的 `BOOLEAN` 就是 `TINYINT(1)`，线协议里**分不出来**
    /// （协议不带显示长度）⇒ 按整数算。
    #[test]
    fn mysql_类型码映射() {
        use mysql_async::consts::ColumnType as T;
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_LONG),
            Some(dbmeta::ColType::Int)
        );
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_LONGLONG),
            Some(dbmeta::ColType::Int)
        );
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_DOUBLE),
            Some(dbmeta::ColType::Float)
        );
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_NEWDECIMAL),
            Some(dbmeta::ColType::Float)
        );
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_VARCHAR),
            Some(dbmeta::ColType::Text)
        );
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_DATETIME),
            Some(dbmeta::ColType::Text)
        );
        // 二进制 ⇒ "未知"（不是文本）
        assert_eq!(
            mysql_col_type(T::MYSQL_TYPE_BLOB),
            Some(dbmeta::ColType::Other)
        );
        // 认不出来 ⇒ None（**不猜**）
        assert_eq!(mysql_col_type(T::MYSQL_TYPE_UNKNOWN), None);
        assert_eq!(mysql_col_type(T::MYSQL_TYPE_NULL), None);
    }

    #[test]
    fn 约束错误翻成中文() {
        let dup = explain_constraint_error(
            "执行失败: duplicate key value violates unique constraint \"t_pkey\"",
        )
        .expect("重复键要能翻译");
        assert!(dup.contains("唯一约束"), "{dup}");
        assert!(dup.contains("t_pkey"), "原文要留着：{dup}");

        let notnull = explain_constraint_error(
            "执行失败: null value in column \"name\" violates not-null constraint",
        )
        .expect("非空约束要能翻译");
        assert!(notnull.contains("非空"), "{notnull}");

        let fk = explain_constraint_error("执行失败: insert or update on table \"t\" violates foreign key constraint \"t_fk\"")
            .expect("外键要能翻译");
        assert!(fk.contains("外键"), "{fk}");

        // MySQL 的说法不一样，但一样要能认出来
        let my = explain_constraint_error(
            "执行失败: Server error: `ERROR 1062 (23000): Duplicate entry '1' for key 'PRIMARY'`",
        )
        .expect("MySQL 的重复键要能翻译");
        assert!(my.contains("唯一约束"), "{my}");

        // 认不出来就返回 None（照原样透传，别硬凑）
        assert!(explain_constraint_error("连接断了").is_none());
    }

    #[test]
    fn 导入前本地校验拦下脏数据() {
        let batch = ImportBatch {
            proto: DbProto::Postgres,
            schema: "public".into(),
            table: "t".into(),
            columns: vec!["id".into(), "age".into()],
            types: vec![dbmeta::ColType::Int, dbmeta::ColType::Int],
            rows: vec![
                vec![Some("1".into()), Some("30".into())],
                vec![Some("2".into()), Some("abc".into())],
            ],
        };
        let e = validate_import(&batch).unwrap_err();
        let msg = e.0;
        assert!(msg.contains("第 2 行"), "要带行号：{msg}");
        assert!(msg.contains("age"), "要带列名：{msg}");
        assert!(msg.contains("整数"), "要说清期望：{msg}");
    }

    #[test]
    fn 导入前校验放行正常数据() {
        let batch = ImportBatch {
            proto: DbProto::Postgres,
            schema: "public".into(),
            table: "t".into(),
            columns: vec!["id".into(), "name".into()],
            types: vec![dbmeta::ColType::Int, dbmeta::ColType::Text],
            rows: vec![
                vec![Some("1".into()), Some("张三".into())],
                // NULL 不校验（数据库自己管）
                vec![Some("2".into()), None],
            ],
        };
        assert!(validate_import(&batch).is_ok());
    }

    #[test]
    fn 导入前校验拦下列数不对的行() {
        let batch = ImportBatch {
            proto: DbProto::Postgres,
            schema: "public".into(),
            table: "t".into(),
            columns: vec!["id".into(), "name".into()],
            types: vec![dbmeta::ColType::Int, dbmeta::ColType::Text],
            rows: vec![vec![Some("1".into())]],
        };
        let msg = validate_import(&batch).unwrap_err().0;
        assert!(msg.contains("列数"), "{msg}");
    }

    #[test]
    fn proto_parse_and_defaults() {        assert_eq!(DbProto::from_str("pg"), Some(DbProto::Postgres));
        assert_eq!(DbProto::from_str("mariadb"), Some(DbProto::MySql));
        assert_eq!(DbProto::from_str("oracle"), None);
        assert_eq!(DbProto::Postgres.default_port(), 5432);
        assert_eq!(DbProto::MySql.default_port(), 3306);
        assert_eq!(DbProto::MySql.as_str(), "mysql");
    }

    /// PG 驱动把"服务端要密码、我们没给"报成 `Kind::Config`（原文
    /// "invalid configuration"），真正的原因藏在 cause 里。
    ///
    /// **这条测试钉住的是"认 cause"这件事**——认错了就会把普通故障
    /// （比如认证方式不支持）也报成"要密码"，用户会被弹一个没用的
    /// 密码框。
    #[test]
    fn pg_缺密码靠cause认出来() {
        assert!(is_missing_password_cause(Some(PG_MISSING_PASSWORD)));
        assert!(!is_missing_password_cause(Some("something else")));
        assert!(!is_missing_password_cause(None));
    }

    /// MySQL 的 1045 有两种：压根没送密码（`using password: NO`）与
    /// 密码真错了（`using password: YES`）。只有前者值得弹密码框——
    /// 密码错的时候再弹一次也是错的。
    #[test]
    fn mysql_1045只认没送密码的那种() {
        let no_pw = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1045,
            message: "Access denied for user 'root'@'localhost' (using password: NO)".into(),
            state: "28000".into(),
        });
        assert!(DbOpenError::from_mysql(&no_pw).needs_password());

        let wrong_pw = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1045,
            message: "Access denied for user 'root'@'localhost' (using password: YES)".into(),
            state: "28000".into(),
        });
        assert!(!DbOpenError::from_mysql(&wrong_pw).needs_password());

        let other = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1049,
            message: "Unknown database 'nope'".into(),
            state: "42000".into(),
        });
        assert!(!DbOpenError::from_mysql(&other).needs_password());
    }

    /// 缺密码的提示里要能看出"该做什么"，不能只说"要密码"。
    #[test]
    fn 缺密码的提示说得清怎么办() {
        let e = DbOpenError::need_password();
        assert!(e.needs_password());
        let msg = e.message();
        assert!(msg.contains("密码"), "提示里要说密码：{msg}");
        assert!(!msg.contains("invalid configuration"), "别把驱动英文甩给用户：{msg}");
    }

    #[test]
    fn single_select_detection() {        assert_eq!(single_select("select 1"), Some("select 1"));
        assert_eq!(single_select("  SELECT 1;  "), Some("SELECT 1"));
        assert!(single_select("with x as (select 1) select * from x").is_some());
        assert!(single_select("select 1; select 2").is_none());
        assert!(single_select("update t set x=1").is_none());
        assert!(single_select("").is_none());
    }

    #[test]
    fn mysql_values_render_as_text() {
        assert_eq!(my_value_text(&Value::NULL), None);
        assert_eq!(
            my_value_text(&Value::Bytes(b"hi".to_vec())),
            Some("hi".into())
        );
        assert_eq!(my_value_text(&Value::Int(-3)), Some("-3".into()));
        assert_eq!(my_value_text(&Value::UInt(7)), Some("7".into()));
        assert_eq!(my_value_text(&Value::Double(1.5)), Some("1.5".into()));
        assert_eq!(
            my_value_text(&Value::Date(2026, 10, 20, 12, 34, 56, 0)),
            Some("2026-10-20 12:34:56.000000".into())
        );
        assert_eq!(
            my_value_text(&Value::Time(true, 1, 2, 3, 4, 5)),
            Some("-01 02:03:04.000005".into())
        );
    }
}
