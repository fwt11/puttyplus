//! **连接中枢**：一个服务器一条 SSH 连接，各功能在它上面开自己的通道。
//!
//! ## 为什么要有这个（用户 2026-11-11 定的方向）
//!
//! > 「我们默认情况下 sftp, 端口映射，数据库管理等等都走 direct-tcpip 通道，
//! > 这将是我们与其他项目的重要区别。我们可以保证对每个服务器的访问只需要
//! > 一个连接。」
//!
//! ⇒ **同一台服务器（host+port+user）在整个程序里只有一条 SSH 连接**，
//!   各个功能在它上面开自己的**通道**（channel）：
//!
//! | 功能 | 通道类型 |
//! |---|---|
//! | SFTP 文件面板 | `session` channel + `subsystem sftp` |
//! | 交互终端（进程内） | `session` channel + `request_pty` + `request_shell` |
//! | 端口映射（`-L`/`-R`/`-D`） | `direct-tcpip` / `tcpip-forward` |
//! | 数据库工作台 | `direct-tcpip` → 远程 5432/3306 |
//!
//! ⚠️ **SFTP 走 `subsystem` 而不是 `direct-tcpip`**：OpenSSH 默认是
//!    `Subsystem sftp internal-sftp` —— 那个进程**只走 SSH 子系统**，
//!    **没有 TCP 端口**可以连 ⇒ `direct-tcpip` 根本连不上。通道类型不同，
//!    但**底层是同一条连接**（这才是重点）。
//!
//! ## 两个硬约束（决定了本模块的形状）
//!
//! 1. ⚠️ **russh 的 `Handle` 绑定在创建它的 tokio runtime 上** ⇒ 跨 runtime
//!    用不了 ⇒ 要「一条连接多方共用」，各方**必须同 runtime** ⇒ 本模块
//!    提供**一个全局 runtime**（[`runtime`]），所有 SSH 相关的东西都在它上面。
//! 2. ⚠️ **连接的生命周期由引用计数管**：最后一个使用者放手 ⇒ 连接才真的
//!    断。⇒ 面板关了、但终端还在用同一条连接 ⇒ 连接**不断**（转发照跑）。
//!
//! ## 复用粒度
//!
//! 按 `host + port + user`（[`ServerKey`]）**全局**共享 ⇒ 同一台服务器开
//! 几个标签页都只有一条连接。
//!
//! ⚠️ **认证参数不在 key 里**：密码/私钥只在**第一次建连接**时用得上；
//!    连上之后复用同一份会话 ⇒ **密码只问一次**（这也是「我们的对话框问
//!    一次」那个体验的延续）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use russh::client::Handle;

use crate::args::{AuthMethod, AuthOrder};
use crate::backend::conn::{self, HostKeyHandler};
use crate::backend::forward::{FwdEntry, FwdRule, RfwdTable};
use crate::backend::{BackendError, BackendResult};
use crate::trf;

/// **全局的 SSH 运行时**。
///
/// ⚠️ **为什么必须只有一个**：russh 的 `Handle` 绑定在创建它的 runtime 上
///    ⇒ 跨 runtime 用不了 ⇒ 「一条连接多方共用」的前提是各方同 runtime。
///
/// ⚠️ **必须是 multi_thread**：SFTP worker 那边的命令循环是
///    `Runtime::block_on`（阻塞线程等命令）⇒ current_thread runtime 会被
///    一个 worker 独占 ⇒ 多面板就没法共用了。multi_thread 的 `block_on`
///    可以从多个线程并发调用（tokio 允许）。
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            // 够用即可：每个 SFTP 面板自己占一个线程跑命令循环，任务
            // （传输/保活/转发/终端读循环）在这里面跑。
            .worker_threads(8)
            .enable_all()
            .build()
            .expect("全局 SSH 运行时")
    })
}

/// 一台服务器的标识（**复用连接的唯一依据**）。
///
/// ⚠️ **归一化过**（见 [`ServerKey::new`]）：主机名大小写不敏感、端口 0
///    等价于 22、用户名为空等价于当前登录用户 ⇒ 否则「同一个服务器」会
///    因为写法不同被当成两台 ⇒ 白开两条连接。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ServerKey {
    pub host: String,
    pub port: i32,
    pub user: String,
}

impl ServerKey {
    /// 归一化后的 key。
    ///
    /// - 主机名：**去空白 + 转小写**（DNS 大小写不敏感；IP 不受影响）；
    /// - 端口：**<= 0 ⇒ 22**（ssh 的默认端口；命令行里 `-P 0` 也是它）；
    /// - 用户名：**空 ⇒ 当前登录用户**（`USER` / `USERNAME` 环境变量）。
    pub fn new(host: &str, port: i32, user: &str) -> Self {
        let host = host.trim().to_ascii_lowercase();
        let port = if port <= 0 { 22 } else { port };
        let user = {
            let u = user.trim();
            if u.is_empty() {
                current_user()
            } else {
                u.to_string()
            }
        };
        Self { host, port, user }
    }
}

/// 当前登录用户名（取不到就空串）。
fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 建连接时用的凭据（**只有第一次建连接时用得上**）。
#[derive(Clone, Default)]
pub struct Creds {
    pub password: Option<String>,
    pub keyfile: Option<String>,
    pub proxycmd: Option<String>,
    pub keepalive_sec: u32,
    pub auth_method: AuthMethod,
    pub auth_order: AuthOrder,
}

/// 一条**共享**的 SSH 连接。
///
/// ⚠️ **转发规则挂在连接上，不挂在某个面板上** ⇒ 关掉文件面板**不会**把
///    端口映射一起停掉（映射是会话级功能，不该因为关了个面板就断）。
pub struct SharedConn {
    pub key: ServerKey,
    /// 已认证的 SSH 会话（**共享给所有使用者**）。
    session: Arc<Handle<HostKeyHandler>>,
    /// 远程转发（-R）的入站登记表：与 handler 共享同一张表。
    rfwd: RfwdTable,
    /// 进程内转发规则集，**按来源分组**。
    ///
    /// ⚠️ **为什么按来源分组**：同一台服务器可能被好几个连接条目引用
    ///    （不同名字、不同转发规则）⇒ 如果规则集是「一锅端」，后连的那个
    ///    会把前一个的规则清掉 ⇒ 端口映射凭空消失。⇒ 按来源分组，各来源
    ///    只覆盖自己那一份。
    forwards: tokio::sync::Mutex<HashMap<String, Vec<FwdEntry>>>,
    /// 自弱引用：只用来数「现在有几个使用者」（诊断用）。
    me: Weak<SharedConn>,
}

impl SharedConn {
    /// 已认证的会话（开通道用）。
    pub fn session(&self) -> &Arc<Handle<HostKeyHandler>> {
        &self.session
    }

    /// 远程转发入站登记表。
    pub fn rfwd(&self) -> &RfwdTable {
        &self.rfwd
    }

    /// 这条连接现在有几个使用者（诊断用）。
    pub fn users(&self) -> usize {
        self.me.strong_count()
    }

    /// 会话**断了没有**。
    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }

    /// 装载**某个来源**的转发规则（替换该来源的旧规则）。
    ///
    /// 返回未能启动的 (规则, 原因)。
    pub async fn apply_forward_rules(
        &self,
        source: &str,
        rules: Vec<FwdRule>,
    ) -> Vec<(String, String)> {
        let mut all = self.forwards.lock().await;
        // ⚠️ **规则一模一样就不折腾**：同一个连接条目被打开两个标签页时，
        //    第二个人会把第一个人的转发停掉再重起 ⇒ 正在跑的转发连接被掐断。
        //    规则没变就原样留着。
        if let Some(old) = all.get(source) {
            if same_rules(old, &rules) {
                return Vec::new();
            }
        }
        // ⚠️ **先停掉本来源的旧规则，再起新的**（顺序不能反）：
        //    「同一个监听端口换一个目标」是常见改法 ⇒ 先起新的会撞
        //    EADDRINUSE（端口还被旧监听占着）。
        //    ⚠️ **只停本来源的**：别的来源（别的连接条目）的照跑。
        if let Some(old) = all.remove(source) {
            for mut e in old {
                // ⚠️ **等端口真释放**：`stop()` 只是 abort（异步取消）⇒
                //    不等的话紧接着的 bind 必撞 EADDRINUSE。
                e.stop_wait().await;
            }
        }
        let mut fails = Vec::new();
        let mut fresh = Vec::new();
        for r in rules {
            let mut e = FwdEntry::new(r);
            if e.rule.enabled {
                if let Err(err) = e.start(Arc::clone(&self.session), self.rfwd.clone()).await {
                    fails.push((e.rule.display(), err.0));
                }
            }
            fresh.push(e);
        }
        all.insert(source.to_string(), fresh);
        fails
    }

    /// 运行时新增一条规则（转发管理窗口）。
    pub async fn add_forward_rule(&self, spec: &str) -> BackendResult<()> {
        let rule = crate::backend::forward::parse_rules(spec)
            .into_iter()
            .find(|r| r.enabled)
            .ok_or_else(|| BackendError(trf!("无法识别的转发规格：{spec}", spec = spec)))?;
        let mut all = self.forwards.lock().await;
        let dup = all.values().flatten().any(|e| {
            e.rule.spec == rule.spec && e.rule.kind == rule.kind
        });
        if dup {
            return Err(BackendError(trf!(
                "该转发已存在：{v0}",
                v0 = rule.display()
            )));
        }
        let mut e = FwdEntry::new(rule);
        // 新增的规则建好即启用；起不来要把原因带给用户。
        e.start(Arc::clone(&self.session), self.rfwd.clone()).await?;
        // 手动加的规则归到「手动」这一组（不隶属于任何连接条目的自动规则）。
        all.entry(MANUAL_SOURCE.to_string())
            .or_default()
            .push(e);
        Ok(())
    }

    /// 停用 / 启用一条规则（key = `FwdEntry::key`）。
    pub async fn set_forward_rule_enabled(
        &self,
        key: &str,
        enabled: bool,
    ) -> BackendResult<()> {
        let mut all = self.forwards.lock().await;
        let mut found = false;
        for entries in all.values_mut() {
            let Some(idx) = entries.iter().position(|e| e.key() == key) else {
                continue;
            };
            found = true;
            if enabled {
                entries[idx].rule.enabled = true;
                if let Err(err) = entries[idx]
                    .start(Arc::clone(&self.session), self.rfwd.clone())
                    .await
                {
                    entries[idx].rule.enabled = false;
                    return Err(err);
                }
            } else {
                entries[idx].rule.enabled = false;
                // ⚠️ 等端口真释放（停用后马上重新启用同一个端口会撞
                //    EADDRINUSE）。
                entries[idx].stop_wait().await;
            }
            break;
        }
        if !found {
            return Err(BackendError(trf!(
                "转发规则不存在：{key}",
                key = key
            )));
        }
        Ok(())
    }

    /// 停止并移除一条规则。
    pub async fn remove_forward_rule(&self, key: &str) -> bool {
        let mut all = self.forwards.lock().await;
        for entries in all.values_mut() {
            if let Some(idx) = entries.iter().position(|e| e.key() == key) {
                entries[idx].stop_wait().await;
                entries.remove(idx);
                return true;
            }
        }
        false
    }

    /// 转发规则快照（UI 每次刷新用；**所有来源合并**）。
    pub async fn forward_snapshot(&self) -> Vec<conn::FwdInfo> {
        let all = self.forwards.lock().await;
        let mut out = Vec::new();
        for entries in all.values() {
            for e in entries {
                out.push(conn::FwdInfo {
                    key: e.key(),
                    text: e.rule.display(),
                    enabled: e.rule.enabled,
                    active: e.active().is_some(),
                    listen: e.active().and_then(|a| a.listen),
                    err: e.err().map(|s| s.to_string()),
                });
            }
        }
        out
    }

    /// 停掉全部转发规则（连接真正断开时）。
    pub async fn stop_all_forwards(&self) {
        let mut all = self.forwards.lock().await;
        for entries in all.values_mut() {
            for e in entries.iter_mut() {
                e.stop_wait().await;
            }
        }
        all.clear();
    }
}

/// 两组规则**是否完全一样**（种类 + 规格 + 启停状态）。
fn same_rules(old: &[FwdEntry], new: &[FwdRule]) -> bool {
    if old.len() != new.len() {
        return false;
    }
    old.iter().zip(new.iter()).all(|(e, r)| {
        e.rule.kind == r.kind && e.rule.spec == r.spec && e.rule.enabled == r.enabled
    })
}

/// 手动加的规则归这一组（不属于任何连接条目的自动规则）。
const MANUAL_SOURCE: &str = "\u{0}manual";

impl Drop for SharedConn {
    fn drop(&mut self) {
        // 最后一个使用者放手了 ⇒ 这条连接该收了。
        //
        // ⚠️ **规则表在 Drop 里拿不出来**（tokio 的 Mutex 不能同步取）⇒
        //    用 `mem::take` 把整个表端走，`into_inner` 拿到裸的 HashMap。
        let forwards = std::mem::take(&mut self.forwards).into_inner();
        let session = Arc::clone(&self.session);
        let key = self.key.clone();

        // 从注册表里摘掉（别让注册表攒下一堆死条目）。
        {
            let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = reg.get(&key) {
                let mut cur = slot.current.lock().unwrap_or_else(|e| e.into_inner());
                // ⚠️ 只在「注册表里指的就是我」时才摘（别把后来者摘了）。
                let dead = cur
                    .as_ref()
                    .map(|w| w.upgrade().is_none())
                    .unwrap_or(false);
                if dead {
                    *cur = None;
                }
            }
        }

        // 停转发 + 通知服务器断开：这是异步的 ⇒ 丢给全局运行时。
        // （`FwdEntry::stop` 是同步的；只有 disconnect 需要 await。）
        runtime().spawn(async move {
            for entries in forwards.into_values() {
                for mut e in entries {
                    e.stop();
                }
            }
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "bye", "en")
                .await;
        });
    }
}

/// 注册表里每个 key 的槽位。
struct Slot {
    /// 建连接时的闸门：同一台服务器同时只允许一条连接在建。
    gate: tokio::sync::Mutex<()>,
    /// 当前连接（**弱引用** ⇒ 最后一个使用者放手就自动掉）。
    current: Mutex<Option<Weak<SharedConn>>>,
}

fn registry() -> &'static Mutex<HashMap<ServerKey, Arc<Slot>>> {
    static REG: OnceLock<Mutex<HashMap<ServerKey, Arc<Slot>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn slot_for(key: &ServerKey) -> Arc<Slot> {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.entry(key.clone())
        .or_insert_with(|| {
            Arc::new(Slot {
                gate: tokio::sync::Mutex::new(()),
                current: Mutex::new(None),
            })
        })
        .clone()
}

/// 取当前活着的连接（**死了的不算**）。
fn live(key: &ServerKey) -> Option<Arc<SharedConn>> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let slot = reg.get(key)?;
    let cur = slot.current.lock().unwrap_or_else(|e| e.into_inner());
    let conn = cur.as_ref()?.upgrade()?;
    // ⚠️ **会话已经断了 ⇒ 当作没有**（下次 acquire 会重建一条新的）。
    if conn.is_closed() {
        return None;
    }
    Some(conn)
}

/// **拿到（或建）一条共享连接**。
///
/// ⚠️ **已经有一条活着的连接 ⇒ 直接复用**（不再问密码、不再握手）。
///
/// ⚠️ **不管调用方在哪个 runtime 上，连接都建在全局 runtime 上**：
///    russh 的 `Handle` 绑定在创建它的 runtime 上 ⇒ 如果某个调用方在
///    自己的 runtime 上建了连接，这条连接就会跟着那个 runtime 一起死，
///    而别的使用者（跑在全局 runtime 上）会莫名其妙地断线。
///    ⇒ 这里把建连**搬到全局 runtime**（[`runtime`]）上做。
pub async fn acquire(key: ServerKey, creds: &Creds) -> BackendResult<Arc<SharedConn>> {
    let rt = runtime();
    // 已经在全局 runtime 上 ⇒ 直接干（省一次投递）
    let on_global = tokio::runtime::Handle::try_current()
        .map(|h| h.id() == rt.handle().id())
        .unwrap_or(false);
    if on_global {
        return acquire_inner(key, creds.clone()).await;
    }
    // 不在 ⇒ 丢给全局 runtime 去建，结果拿回来
    let (tx, rx) = tokio::sync::oneshot::channel();
    let creds = creds.clone();
    rt.spawn(async move {
        let _ = tx.send(acquire_inner(key, creds).await);
    });
    match rx.await {
        Ok(r) => r,
        Err(_) => Err(BackendError(crate::i18n::tr("共享连接建立失败").into())),
    }
}

/// 真正建连（**必须跑在全局 runtime 上**）。
async fn acquire_inner(key: ServerKey, creds: Creds) -> BackendResult<Arc<SharedConn>> {
    // 快路径
    if let Some(c) = live(&key) {
        return Ok(c);
    }
    let slot = slot_for(&key);
    // ⚠️ **闸门**：同一台服务器同时只建一条（两个面板同时连 ⇒ 只建一次）。
    let _gate = slot.gate.lock().await;
    // 等闸门期间可能已经有人建好了 ⇒ 再查一次
    if let Some(c) = live(&key) {
        return Ok(c);
    }
    let (session, rfwd) = conn::connect_session(
        &key.host,
        key.port,
        &key.user,
        creds.password.as_deref().filter(|p| !p.is_empty()),
        creds.keyfile.as_deref().filter(|k| !k.is_empty()),
        creds.proxycmd.as_deref().filter(|c| !c.is_empty()),
        creds.keepalive_sec,
        creds.auth_method,
        creds.auth_order,
    )
    .await?;
    let shared = Arc::new_cyclic(|me| SharedConn {
        key: key.clone(),
        session: Arc::new(session),
        rfwd,
        forwards: tokio::sync::Mutex::new(HashMap::new()),
        me: me.clone(),
    });
    *slot.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&shared));
    Ok(shared)
}

/// **当前有几条共享连接**（诊断用）。
pub fn conn_count() -> usize {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.values()
        .filter(|s| {
            s.current
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|w| w.upgrade())
                .is_some()
        })
        .count()
}

/// 列出当前活着的连接（诊断用：`键 → 使用者数`）。
pub fn live_connections() -> Vec<(ServerKey, usize)> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (key, slot) in reg.iter() {
        if let Some(c) = slot
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|w| w.upgrade())
        {
            out.push((key.clone(), c.users()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主机名大小写/空白、端口 0、空用户 ⇒ 归一化到同一个 key。
    #[test]
    fn key会归一化() {
        let a = ServerKey::new("  Example.COM ", 0, "");
        let b = ServerKey::new("example.com", 22, "");
        assert_eq!(a, b, "大小写/空白/端口 0/空用户应当归一化成同一个 key");
        assert_eq!(a.port, 22);
    }

    /// 不同用户 ⇒ 不同 key（不然会拿错人的会话）。
    #[test]
    fn 不同用户是不同key() {
        let a = ServerKey::new("h", 22, "alice");
        let b = ServerKey::new("h", 22, "bob");
        assert_ne!(a, b);
    }

    /// 不同主机 / 不同端口 ⇒ 不同 key。
    #[test]
    fn 不同主机端口是不同key() {
        assert_ne!(
            ServerKey::new("h1", 22, "u"),
            ServerKey::new("h2", 22, "u")
        );
        assert_ne!(
            ServerKey::new("h1", 22, "u"),
            ServerKey::new("h1", 2222, "u")
        );
    }

    /// 注册表是空的（还没人连过）。
    #[test]
    fn 注册表初始是空的() {
        assert_eq!(conn_count(), 0);
        assert!(live_connections().is_empty());
    }
}
