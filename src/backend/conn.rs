//! 连接与认证编排（对应 C 版 sftp_backend_connect / auth / kbdint_auth / exec）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::MethodKind;
// ⚠️ **`AgentClient::connect_env` 只在 Unix 上有**（它连的是
//    `$SSH_AUTH_SOCK` 那个 Unix 套接字）⇒ Windows 上这个类型整个没有。
#[cfg(unix)]
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::Algorithm;
use russh::keys::PrivateKeyWithHashAlg;
use russh::client::{AuthResult, Handle};
use russh_sftp::client::SftpSession;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;

use super::forward::RfwdTable;
use super::known_hosts::{self, HostKeyCheck};
use super::{BackendError, BackendResult};
use crate::i18n::tr;
use crate::trf;

/// 单步认证（none/密码/键盘交互/公钥）的兜底超时。认证阶段任何一步
/// 都不能无限等：网关丢弃/不回应认证请求时，UI 至少能在有限时间内
/// 拿到「认证失败」而不是永久「正在连接」。
const AUTH_STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// 认证之后建立 SFTP 通道（channel open / subsystem / INIT）的兜底超时。
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

/// 远程命令的执行结果（直连预检 / scp）。
pub struct ExecOut {
    pub code: Option<u32>,
    pub stdout: String,
    pub stderr: String,
}

/// 与 C 版一致：回环目标（堡垒机本地隧道全是 127.0.0.1:动态端口）跳过
/// 主机密钥校验——网关按访问轮换密钥，逐端口记录只会污染 known_hosts
/// 并在端口复用时误报。
pub fn is_loopback(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host == "localhost" || host.starts_with("127.")
}

/// check_server_key 在 russh 握手任务里执行，把决定带回给 connect 调用方。
#[derive(Clone)]
pub(crate) struct HostKeyHandler {
    host: String,
    port: i32,
    decision: Arc<Mutex<Option<Result<HostKeyCheck, String>>>>,
    /// 远程转发（-R）的入站登记表：服务器把入站连接送回来时，
    /// 据此找「客户端侧要连的目标」。共享表——规则是连上之后才装载的，
    /// handler 早就建好了（见 forward.rs::RfwdTable）。
    rfwd: RfwdTable,
}

/// 直连传输期间临时允许服务器打开 agent 转发通道（计数 > 0 才代理）。
/// 只在 `exec_full(forward_agent=true)` 的那次 exec 生命周期内打开。
static AGENT_FORWARD_ARMED: AtomicUsize = AtomicUsize::new(0);

struct AgentArm(bool);
impl Drop for AgentArm {
    fn drop(&mut self) {
        if self.0 {
            AGENT_FORWARD_ARMED.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl russh::client::Handler for HostKeyHandler {
    type Error = russh::Error;

    /// 服务器请求打开 agent 转发通道：连到本地 `$SSH_AUTH_SOCK` 双向搬运。
    /// 未处于直连传输窗口时一律拒绝。
    ///
    /// ⚠️ **Windows 上这条路整个不存在**：`$SSH_AUTH_SOCK` 是 **Unix 套接字**
    /// （Windows 的 ssh-agent 用的是命名管道）⇒ 直接拒绝。
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if AGENT_FORWARD_ARMED.load(Ordering::Relaxed) == 0 {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            // ⚠️ Windows：没有 Unix 套接字 ⇒ agent 转发没法代理
            let _ = channel;
            reply
                .reject(russh::ChannelOpenFailure::ConnectFailed)
                .await;
            return Ok(());
        }
        #[cfg(unix)]
        {
            let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") else {
                reply
                    .reject(russh::ChannelOpenFailure::ConnectFailed)
                    .await;
                return Ok(());
            };
            reply.accept().await;
            tokio::spawn(async move {
                let Ok(mut local) = tokio::net::UnixStream::connect(std::path::Path::new(&sock)).await
                else {
                    return;
                };
                let mut remote = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut remote, &mut local).await;
            });
            Ok(())
        }
    }

    /// 服务器请求打开「远程转发」的入站通道（-R）：
    /// 服务器那边有人连上了我们请求它监听的端口，它把这条连接送回来。
    ///
    /// ⚠️ **目标在客户端侧**：SSH 协议的 tcpip-forward 请求只带绑定
    /// 地址/端口，不带目标——目标只有客户端知道（见 forward.rs）。
    /// 所以这里按 (地址, 端口) 查登记表，找到后连本机目标并双向泵。
    ///
    /// 查不到（规则已停用/被删）就拒收：宁可拒绝，也不能把入站连接
    /// 接到一个不明目标上。
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let Some((host, port)) = self.rfwd.lookup(connected_address, connected_port) else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        reply.accept().await;
        let mut remote = channel.into_stream();
        tokio::spawn(async move {
            let Ok(mut local) = tokio::net::TcpStream::connect((host.as_str(), port)).await else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
        });
        Ok(())
    }

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = server_public_key else {
            *self.decision.lock().unwrap() =
                Some(Err(tr("服务器出示了证书，暂不支持").into()));
            return Ok(false);
        };
        if is_loopback(&self.host) {
            return Ok(true);
        }
        let kh_path = glib::home_dir().join(".ssh").join("known_hosts");
        match known_hosts::verify_and_learn(&self.host, self.port, key, &kh_path) {
            Ok(_) => Ok(true),
            Err(msg) => {
                *self.decision.lock().unwrap() = Some(Err(msg));
                Ok(false)
            }
        }
    }
}

/// 与 C 版 libssh + strerror 同源的错误文案：网络层 errno 按 locale 本地化
/// （如中文 locale 下 "Connection refused" 显示为 "拒绝连接"），
/// 其余保持 russh 自身的错误描述。
fn strerror_local(e: &russh::Error) -> String {
    if let russh::Error::IO(ioerr) = e {
        if let Some(errno) = ioerr.raw_os_error() {
            // ⚠️ Unix 上走 `libc::strerror`（与 C 版同源，按 locale 本地化）；
            // Windows 上**不能**用它 —— 那里返回的是 ANSI 代码页字节
            // （简体中文机器上是 GBK），塞进 GTK 就是乱码 ⇒ 用 Rust 自带的
            // ASCII 描述。见 `platform::locale::strerror`。
            #[cfg(unix)]
            {
                return crate::platform::locale::strerror(errno);
            }
            #[cfg(not(unix))]
            {
                return ioerr.to_string();
            }
        }
    }
    e.to_string()
}

/// 一个已认证的 SFTP 连接。
///
/// 连接对象放进 `Arc` 供传输调度器与 worker 命令循环共享：
/// `SftpSession` 内部按请求 id 分发，多个文件可以并发读写同一条连接。
/// 并发数由设置 `[xfer] parallel` 控制，不额外新建 SSH/TCP 连接。
pub struct SftpBackend {
    /// Arc 共享：进程内转发任务（forward.rs）需要开新通道，而 russh 0.63
    /// 的 Handle 不可 Clone（channel_open 系列只要求 &self）
    session: Arc<Handle<HostKeyHandler>>,
    sftp: SftpSession,
    /// 进程内转发规则集（含停用项），随连接生命周期启停
    /// （forward.rs，Rust-only；1.4 起规则本体与运行态分离）
    ///
    /// 用异步 Mutex：传输任务只读连接，但转发增删仍需要独占规则集。
    forwards: AsyncMutex<Vec<super::forward::FwdEntry>>,
    /// 远程转发（-R）的入站登记表：与 handler 共享同一张表，
    /// 服务器送回入站连接时据此找客户端侧的目标
    rfwd: RfwdTable,
    /// 连接标识（host:port）。断点续传的 mark 用它做键的一部分，
    /// 免得两台同名主机的半截文件互相续上
    pub id: String,
    /// 远程可用的 SHA-1 工具（checksum.rs）：None = 还没试过，
    /// Some("") = 试过了都没有。Mutex 是因为 exec 要 &self 又要缓存结果
    pub(crate) sha1_tool: Mutex<Option<&'static str>>,
}

/// 转发规则的一行运行态快照（UI 渲染用；规则本体不离开连接）。
#[derive(Debug, Clone, PartialEq)]
pub struct FwdInfo {
    pub key: String,
    pub text: String,
    pub enabled: bool,
    pub active: bool,
    pub listen: Option<std::net::SocketAddr>,
    pub err: Option<String>,
}

/// **只建连 + 认证**，不开 SFTP 子系统（阶段 C7）。
///
/// 干什么用：Windows 上的终端**不再 spawn `ssh.exe`**（Windows 默认没装，
/// `plink` 又不想依赖）⇒ 改成**进程内实现 SSH 客户端**
/// （见 [`crate::backend::shell`]）⇒ 终端要的就是这条「已认证的会话」。
///
/// ⇒ 认证顺序与 SFTP 面板**完全一致**（同一段代码）⇒ 密码 / 私钥 /
/// agent 的行为不会两边不一致。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn connect_session(
    host: &str,
    port: i32,
    user: &str,
    password: Option<&str>,
    keyfile: Option<&str>,
    proxycmd: Option<&str>,
    keepalive_sec: u32,
    auth_method: crate::args::AuthMethod,
    auth_order: crate::args::AuthOrder,
) -> BackendResult<(Handle<HostKeyHandler>, RfwdTable)> {
    // russh 端口是 u16；越界值（堡垒机不会出现）按 0 处理，连接自然失败，
    // 报错信息仍带原始端口值，与 C 版 libssh 行为等价
    let port_u16 = u16::try_from(port).unwrap_or(0);
    // 保活：keepalive_sec 无服务器数据即发 keepalive，连续 3 次无应答断开。
    // 0 = 关闭（不主动发保活，完全依赖 TCP 超时）。
    // inactivity_timeout 保持 None——长时间传输中的空闲期不能被 GC 掉。
    let config = Arc::new(russh::client::Config {
        keepalive_interval: (keepalive_sec > 0).then(|| Duration::from_secs(u64::from(keepalive_sec))),
        ..Default::default()
    });
    let decision: Arc<Mutex<Option<Result<HostKeyCheck, String>>>> =
        Arc::new(Mutex::new(None));
    // 远程转发（-R）的入站登记表：handler 与连接体共享同一张表
    let rfwd = RfwdTable::default();
    let handler = HostKeyHandler {
        host: host.to_string(),
        port,
        decision: decision.clone(),
        rfwd: rfwd.clone(),
    };

    // 有 ProxyCommand 时先拉起代理进程拿流，再走 connect_stream；
    // 否则直连。check_server_key 拒绝时给出具体原因而不是笼统错误。
    let connect_result = if let Some(pc) = proxycmd.filter(|c| !c.is_empty()) {
        match super::proxy::ProxyStream::spawn(pc, host, port_u16, user).await {
            Ok(stream) => {
                let fut = russh::client::connect_stream(config, stream, handler);
                tokio::time::timeout(Duration::from_secs(20), fut).await
            }
            Err(msg) => return Err(BackendError(trf!("连接 {host}:{port} 失败: {msg}", host = host, port = port, msg = msg))),
        }
    } else {
        let fut = russh::client::connect(config, (host, port_u16), handler);
        tokio::time::timeout(Duration::from_secs(15), fut).await
    };
    let mut session = match connect_result {
        Err(_) => {
            return Err(BackendError(trf!("连接 {host}:{port} 失败: 超时", host = host, port = port)));
        }
        Ok(Err(e)) => {
            // check_server_key 拒绝时给出具体原因，而不是 russh 的笼统错误
            if let Some(Err(msg)) = decision.lock().unwrap().take() {
                return Err(BackendError(msg));
            }
            return Err(BackendError(trf!("连接 {host}:{port} 失败: {v0}", host = host, port = port, v0 = strerror_local(&e))));
        }
        Ok(Ok(s)) => s,
    };

    authenticate(&mut session, user, password, keyfile, auth_method, auth_order).await?;

    // ⚠️ **连同 rfwd 一起交出去**：远程转发（-R）的入站登记表是
    //    handler 与连接体**共享**的同一个实例 ⇒ 建连时造好、
    //    两边都拿着同一份。
    Ok((session, rfwd))
}

/// 与 C 版相同的认证顺序：
/// none（探测可用方法）→ 有密码时 password → kbdint → 指定私钥 → agent/默认密钥。
#[allow(clippy::too_many_arguments)]
pub async fn connect(
    host: &str,
    port: i32,
    user: &str,
    password: Option<&str>,
    keyfile: Option<&str>,
    proxycmd: Option<&str>,
    keepalive_sec: u32,
    auth_method: crate::args::AuthMethod,
    auth_order: crate::args::AuthOrder,
) -> BackendResult<SftpBackend> {
    let (mut session, rfwd) = connect_session(
        host,
        port,
        user,
        password,
        keyfile,
        proxycmd,
        keepalive_sec,
        auth_method,
        auth_order,
    )
    .await?;

    // SFTP 子系统。三步都加超时：认证已通过但网关不回应 channel open /
    // subsystem 时，不能把 UI 永久卡在「正在连接」。
    let channel = tokio::time::timeout(SETUP_TIMEOUT, session.channel_open_session())
        .await
        .map_err(|_| BackendError(tr("SFTP 通道初始化失败: 超时（服务器未响应通道请求）").into()))?
        .map_err(|e| BackendError(trf!("SFTP 通道初始化失败: {e}（服务器可能未启用 SFTP）", e = e)))?;
    tokio::time::timeout(SETUP_TIMEOUT, channel.request_subsystem(true, "sftp"))
        .await
        .map_err(|_| BackendError(tr("SFTP 通道初始化失败: 超时（服务器未响应子系统请求）").into()))?
        .map_err(|e| BackendError(trf!("SFTP 通道初始化失败: {e}（服务器可能未启用 SFTP）", e = e)))?;
    // russh-sftp 默认每请求 10s 超时：它一次挂 16 个并发读（预取窗口约 4MB），
    // 慢链路（如 400KB/s 公网小水管）下单请求响应时间会撞线——实测 58MB 文件
    // 首包即 Timeout。libssh（C 版）没有每请求超时；放宽到 300s，
    // 极慢链路也够走完一个窗口，同时保留防死锁兜底。
    let sftp = tokio::time::timeout(
        SETUP_TIMEOUT,
        SftpSession::new_with_config(
            channel.into_stream(),
            russh_sftp::client::Config {
                request_timeout_secs: 300,
                ..Default::default()
            },
        ),
    )
    .await
    .map_err(|_| BackendError(tr("SFTP 通道初始化失败: 超时（服务器未回应 SFTP 协议版本）").into()))?
    .map_err(|e| BackendError(trf!("SFTP 通道初始化失败: {e}（服务器可能未启用 SFTP）", e = e)))?;

    Ok(SftpBackend {
        session: Arc::new(session),
        sftp,
        forwards: AsyncMutex::new(Vec::new()),
        rfwd,
        id: format!("{host}:{port}"),
        sha1_tool: Mutex::new(None),
    })
}

/// 认证链的一种候选凭据（none 探测不算，它是服务端能力协商）。
///
/// ⚠️ 阶段 70 把它拆细了：以前"公钥"是一步（指定私钥 → agent → 默认
/// 密钥，一路试到底），现在按**认证方式**分成四步，才能表达
/// "只准用密码"这类要求（别把本地身份出示给堡垒机网关）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AuthStep {
    Password,
    /// 连接里指定的那一个私钥文件
    KeyFile,
    /// ssh-agent 里的密钥
    Agent,
    /// ~/.ssh 里的默认私钥（OpenSSH 惯例：id_ed25519 / id_ecdsa / id_rsa）
    DefaultKeys,
}

/// 按**认证方式** + **认证优先级**排出的尝试顺序。
///
/// ⚠️ `Auto` 的顺序与阶段 70 之前**逐项相同**（密码 → 指定私钥 →
/// agent → 默认密钥，或反过来），别把默认路径的顺序改了。
fn auth_plan(
    method: crate::args::AuthMethod,
    order: crate::args::AuthOrder,
) -> Vec<AuthStep> {
    use crate::args::{AuthMethod, AuthOrder};
    use AuthStep::*;
    match method {
        AuthMethod::Auto => match order {
            AuthOrder::PasswordFirst => vec![Password, KeyFile, Agent, DefaultKeys],
            AuthOrder::PublicKeyFirst => vec![KeyFile, Agent, DefaultKeys, Password],
        },
        AuthMethod::Password => vec![Password],
        AuthMethod::KeyFile => vec![KeyFile],
        AuthMethod::Agent => vec![Agent],
    }
}

/// 认证链（2.2 起顺序可配）：none 探测 → 密码/kbdint → 公钥。
/// `order` 只调换后两段的先后，"none 探测在最前"不变——那是服务端能力
/// 协商，不是候选凭据。
///
/// 必须**逐段短路**：某一段成功就立刻返回。以前把两段都先 await 完再按
/// `by_password || by_key` 判断，密码明明已经通过，却还会接着走公钥链；
/// 而 SSH 会话一旦认证成功就不再处理 `best_supported_rsa_hash` 之类只
/// 在认证阶段有效的请求，`auth_by_key` 会在那里永远等下去——堡垒机
/// （密码认证通过）的 SFTP 面板因此永久停在「正在连接」。这条链同时
/// 也避免认证成功后再把本地身份交给服务器。
async fn authenticate(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
    password: Option<&str>,
    keyfile: Option<&str>,
    method: crate::args::AuthMethod,
    order: crate::args::AuthOrder,
) -> BackendResult<()> {
    // 认证方式是「只用指定的私钥文件」却没选私钥 ⇒ 当场说清楚，
    // 别让它一路试完再报一句笼统的"认证失败"
    if method == crate::args::AuthMethod::KeyFile
        && keyfile.filter(|k| !k.is_empty()).is_none()
    {
        return Err(BackendError(
            tr("认证方式是「只用指定的私钥文件」，但这条连接没选私钥").into(),
        ));
    }
    let res = tokio::time::timeout(AUTH_STEP_TIMEOUT, session.authenticate_none(user))
        .await
        .map_err(|_| BackendError(tr("认证失败: 服务器未响应认证请求（超时）").into()))?
        .map_err(|e| BackendError(trf!("认证失败: {e}", e = e)))?;
    if res.success() {
        return Ok(());
    }
    let methods = match res {
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
        AuthResult::Success => unreachable!(),
    };
    let has = |k: MethodKind| methods.iter().any(|m| *m == k);

    for step in auth_plan(method, order) {
        let timed = match step {
            AuthStep::Password => {
                tokio::time::timeout(
                    AUTH_STEP_TIMEOUT,
                    auth_by_password(session, user, password, &has),
                )
                .await
            }
            AuthStep::KeyFile => {
                tokio::time::timeout(
                    AUTH_STEP_TIMEOUT,
                    auth_by_keyfile(session, user, password, keyfile, &has),
                )
                .await
            }
            AuthStep::Agent => {
                tokio::time::timeout(AUTH_STEP_TIMEOUT, try_agent(session, user)).await
            }
            AuthStep::DefaultKeys => {
                tokio::time::timeout(AUTH_STEP_TIMEOUT, try_default_keys(session, user)).await
            }
        };
        let ok = timed.map_err(|_| {
            BackendError(trf!("认证失败: {v0}认证超时（服务器未响应）", v0 = match step {
                    AuthStep::Password => tr("密码"),
                    AuthStep::KeyFile => tr("私钥文件"),
                    AuthStep::Agent => "ssh-agent",
                    AuthStep::DefaultKeys => tr("默认私钥"),
                }))
        })??;
        if ok {
            return Ok(());
        }
    }

    Err(BackendError(trf!("认证失败{v0}", v0 = match method {
            crate::args::AuthMethod::Password => tr("（密码认证未通过）"),
            crate::args::AuthMethod::KeyFile => tr("（私钥认证未通过）"),
            crate::args::AuthMethod::Agent => tr("（ssh-agent 认证未通过）"),
            crate::args::AuthMethod::Auto => match (password, keyfile) {
                (Some(_), _) => tr("（密码/密钥均未通过）"),
                (None, Some(_)) => tr("（密钥认证未通过）"),
                (None, None) => tr("（未提供密码或密码错误）"),
            },
        })))
}

/// 键盘交互认证：所有 prompt 一律应答密码（堡垒机网关常见交互）。
/// 密码 / 键盘交互阶段。只在显式给了密码时尝试（-pw/-pwfile）。
async fn auth_by_password(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
    password: Option<&str>,
    has: &impl Fn(MethodKind) -> bool,
) -> BackendResult<bool> {
    let Some(pw) = password.filter(|p| !p.is_empty()) else {
        return Ok(false);
    };
    if has(MethodKind::Password) {
        let r = session
            .authenticate_password(user, pw)
            .await
            .map_err(|e| BackendError(trf!("认证失败: {e}", e = e)))?;
        if r.success() {
            return Ok(true);
        }
    }
    if has(MethodKind::KeyboardInteractive) && kbdint_auth(session, user, Some(pw)).await? {
        return Ok(true);
    }
    Ok(false)
}

/// 公钥阶段：指定私钥 → agent/默认密钥。
/// 用连接里**指定的那一个**私钥文件认证（阶段 70）。
///
/// ⚠️ 只试这一个文件：不再顺带试 agent 与默认密钥——「只用指定的私钥
/// 文件」这个认证方式要的正是"只认它"。
async fn auth_by_keyfile(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
    password: Option<&str>,
    keyfile: Option<&str>,
    has: &impl Fn(MethodKind) -> bool,
) -> BackendResult<bool> {
    if !has(MethodKind::PublicKey) {
        return Ok(false);
    }
    let Some(kf) = keyfile.filter(|k| !k.is_empty()) else {
        return Ok(false);
    };
    // 与 C 版一致：密码同时充当私钥口令
    let key_pair = match russh::keys::load_secret_key(kf, password) {
        Ok(k) => k,
        Err(_) => {
            return Err(BackendError(trf!("无法读取私钥 {kf}。\n若是 PuTTY 的 .ppk 格式，请先转换：\n  puttygen key.ppk -O private-openssh -o key.openssh", kf = kf)));
        }
    };
    let key = key_with_hash(session, key_pair).await;
    let r = session
        .authenticate_publickey(user, key)
        .await
        .map_err(|e| BackendError(trf!("认证失败: {e}", e = e)))?;
    Ok(r.success())
}

async fn kbdint_auth(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
    password: Option<&str>,
) -> BackendResult<bool> {
    use russh::client::KeyboardInteractiveAuthResponse as R;
    let fail = |e: russh::Error| BackendError(trf!("键盘交互认证失败: {e}", e = e));
    let mut resp = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(fail)?;
    loop {
        match resp {
            R::Success => return Ok(true),
            R::Failure { .. } => return Ok(false),
            R::InfoRequest { prompts, .. } => {
                let answers = prompts
                    .iter()
                    .map(|_| password.unwrap_or("").to_string())
                    .collect();
                resp = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(fail)?;
            }
        }
    }
}

/// RSA 密钥需要与服务端协商哈希算法，其余算法哈希参数为 None。
async fn key_with_hash(
    session: &Handle<HostKeyHandler>,
    key_pair: russh::keys::PrivateKey,
) -> PrivateKeyWithHashAlg {
    let hash = if matches!(key_pair.algorithm(), Algorithm::Rsa { .. }) {
        session
            .best_supported_rsa_hash()
            .await
            .ok()
            .and_then(|h| h.flatten())
    } else {
        None
    };
    PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash)
}

/// 与 C 版 ssh_userauth_publickey_auto 对应：先 ssh-agent，再 ~/.ssh 默认密钥。
/// 只用 ssh-agent 里的密钥（阶段 70 的「只用 ssh-agent 里的密钥」）。
///
/// ⚠️ **Windows 上这条路没有**：ssh-agent 在 Windows 上用的是命名管道，
///    `russh` 的 `AgentClient::connect_env`（连 `$SSH_AUTH_SOCK` 那个 Unix
///    套接字）**只在 Unix 上存在** ⇒ Windows 上直接返回 false
///    （⇒ 后面走 ~/.ssh 下的默认密钥）。
#[cfg(not(unix))]
async fn try_agent(
    _session: &mut Handle<HostKeyHandler>,
    _user: &str,
) -> BackendResult<bool> {
    Ok(false)
}

#[cfg(unix)]
async fn try_agent(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
) -> BackendResult<bool> {
    let Ok(mut agent) = AgentClient::connect_env().await else {
        return Ok(false);
    };
    let Ok(identities) = agent.request_identities().await else {
        return Ok(false);
    };
    for ident in &identities {
        if let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = ident {
            let hash = if matches!(key.algorithm(), Algorithm::Rsa { .. }) {
                session
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .and_then(|h| h.flatten())
            } else {
                None
            };
            if let Ok(r) = session
                .authenticate_publickey_with(user, key.clone(), hash, &mut agent)
                .await
            {
                if r.success() {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// 只用 ~/.ssh 里的默认私钥（与 OpenSSH 惯例一致的探测顺序）。
async fn try_default_keys(
    session: &mut Handle<HostKeyHandler>,
    user: &str,
) -> BackendResult<bool> {
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let path = glib::home_dir().join(".ssh").join(name);
        let Ok(key_pair) = russh::keys::load_secret_key(&path, None) else {
            continue;
        };
        let key = key_with_hash(session, key_pair).await;
        if let Ok(r) = session.authenticate_publickey(user, key).await {
            if r.success() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

impl SftpBackend {
    pub fn sftp(&self) -> &SftpSession {
        &self.sftp
    }

    /// 在同一 SSH 连接上另开一条 SFTP 子系统通道。
    /// 跨服务器传输把它交给源 worker：目标侧不需要重新认证。
    pub async fn open_sftp(&self) -> BackendResult<SftpSession> {
        let ch = tokio::time::timeout(SETUP_TIMEOUT, self.session.channel_open_session())
            .await
            .map_err(|_| BackendError(tr("创建 SFTP 通道失败: 超时（服务器未响应通道请求）").into()))?
            .map_err(|e| BackendError(trf!("创建 SFTP 通道失败: {e}", e = e)))?;
        tokio::time::timeout(SETUP_TIMEOUT, ch.request_subsystem(true, "sftp"))
            .await
            .map_err(|_| BackendError(tr("请求 SFTP 子系统失败: 超时（服务器未响应）").into()))?
            .map_err(|e| BackendError(trf!("请求 SFTP 子系统失败: {e}", e = e)))?;
        tokio::time::timeout(SETUP_TIMEOUT, SftpSession::new(ch.into_stream()))
            .await
            .map_err(|_| BackendError(tr("初始化 SFTP 失败: 超时（服务器未回应 SFTP 协议版本）").into()))?
            .map_err(|e| BackendError(trf!("初始化 SFTP 失败: {e}", e = e)))
    }

    /// 在现有连接上开一条到 `host:port` 的 direct-tcpip 通道，返回双向流。
    ///
    /// 数据库客户端（7.2）用它把通道当 TCP 用：不监听本地端口、不新建
    /// SSH 连接。`host` 由**服务器侧**解析，填 `127.0.0.1` 连的是服务器
    /// 自己的回环——这正是堡垒机后面数据库的常见位置。
    ///
    /// 与 `-L` 走同一条服务端开关（`AllowTcpForwarding`）：被禁时
    /// 这里和本地转发会一起失败，错误信息里点明这一点。
    pub async fn open_tcp_stream(
        &self,
        host: &str,
        port: u16,
    ) -> BackendResult<russh::ChannelStream<russh::client::Msg>> {
        let ch = tokio::time::timeout(
            SETUP_TIMEOUT,
            self.session
                .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0),
        )
        .await
        .map_err(|_| BackendError(tr("打开数据库通道超时（服务器未响应通道请求）").into()))?
        .map_err(|e| {
            BackendError(trf!("打开数据库通道失败（服务器可能禁止 TCP 转发）: {e}", e = e))
        })?;
        Ok(ch.into_stream())
    }

    /// 装载会话级转发规则集（1.4：连接启动时调用）。
    /// 先清掉旧规则（会话参数是唯一真源），再逐条启动启用的；
    /// 停用规则保留在列表里但不绑定。返回未能启动的 (规则, 原因)。
    pub async fn apply_forward_rules(
        &self,
        rules: Vec<super::forward::FwdRule>,
    ) -> Vec<(String, String)> {
        let mut forwards = self.forwards.lock().await;
        for e in forwards.iter_mut() {
            e.stop();
        }
        *forwards = rules
            .into_iter()
            .map(super::forward::FwdEntry::new)
            .collect();
        let mut fails = Vec::new();
        let session = Arc::clone(&self.session);
        for e in forwards.iter_mut() {
            if !e.rule.enabled {
                continue;
            }
            if let Err(err) = e.start(session.clone(), self.rfwd.clone()).await {
                fails.push((e.rule.display(), err.0));
            }
        }
        fails
    }

    /// 运行时新增一条规则（转发管理窗口）。规格为内部形
    /// （`L:[监听地址:]端口:主机:端口`），建好即启用。
    pub async fn add_forward_rule(&self, spec: &str) -> BackendResult<()> {
        let rule = super::forward::parse_rules(spec)
            .into_iter()
            .find(|r| r.enabled)
            .ok_or_else(|| BackendError(trf!("无法识别的转发规格：{spec}", spec = spec)))?;
        let mut forwards = self.forwards.lock().await;
        if forwards
            .iter()
            .any(|e| e.rule.spec == rule.spec && e.rule.kind == rule.kind)
        {
            return Err(BackendError(trf!("该转发已存在：{v0}", v0 = rule.display())));
        }
        let mut entry = super::forward::FwdEntry::new(rule);
        entry
            .start(Arc::clone(&self.session), self.rfwd.clone())
            .await?;
        forwards.push(entry);
        Ok(())
    }

    /// 停用 / 启用一条规则（key = `FwdEntry::key`）。
    /// 停用保留规则本体（可再启用）；启用失败保持停用并回错误，
    /// 规则仍留在列表里，用户能看见失败原因再重试。
    pub async fn set_forward_rule_enabled(
        &self,
        key: &str,
        enabled: bool,
    ) -> BackendResult<()> {
        let mut forwards = self.forwards.lock().await;
        let Some(idx) = forwards.iter().position(|e| e.key() == key) else {
            return Err(BackendError(trf!("转发规则不存在：{key}", key = key)));
        };
        if enabled {
            forwards[idx].rule.enabled = true;
            forwards[idx]
                .start(Arc::clone(&self.session), self.rfwd.clone())
                .await
                .inspect_err(|_| {
                    forwards[idx].rule.enabled = false;
                })?;
        } else {
            forwards[idx].rule.enabled = false;
            forwards[idx].stop();
        }
        Ok(())
    }

    /// 停止并移除一条规则。
    pub async fn remove_forward_rule(&self, key: &str) -> bool {
        let mut forwards = self.forwards.lock().await;
        let Some(idx) = forwards.iter().position(|e| e.key() == key) else {
            return false;
        };
        forwards[idx].stop();
        forwards.remove(idx);
        true
    }

    /// 转发规则快照（UI 每次刷新用；规则本体留在连接里）。
    pub async fn forward_snapshot(&self) -> Vec<FwdInfo> {
        let forwards = self.forwards.lock().await;
        forwards
            .iter()
            .map(|e| FwdInfo {
                key: e.key(),
                text: e.rule.display(),
                enabled: e.rule.enabled,
                active: e.active().is_some(),
                listen: e.active().and_then(|a| a.listen),
                err: e.err().map(|s| s.to_string()),
            })
            .collect()
    }

    /// 这条 SSH 会话**断了没有**（服务端关了 / 网络断了 / 空闲超时）。
    ///
    /// ⚠️ **为什么需要它**（2026-11-05）：SFTP 面板和终端是**两条独立的
    /// 连接**——终端那条断了，SFTP 那条可能还活着，也可能一起断了。
    /// 以前没人问过"SFTP 那条还在不在" ⇒ 面板一直显示「已连接」，
    /// 用户点「刷新」才报错（"读取远程目录失败"），还得自己知道要去点
    /// 「重新连接 SFTP」。
    /// 现在 worker 盯着这个标志 ⇒ 断了就**主动上报**，UI 立刻把状态改成
    /// 「已断开」，并在下一次操作（刷新 / 换目录）时**自动重连**。
    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }

    pub async fn close(&self) {
        {
            let mut forwards = self.forwards.lock().await;
            for f in forwards.iter_mut() {
                f.stop();
            }
            forwards.clear();
        }
        let _ = self.sftp.close().await;
        let _ = self
            .session
            .disconnect(russh::Disconnect::ByApplication, "bye", "en")
            .await;
    }

    /// 带退出码/错误输出的执行（直连预检与 scp 用）。
    ///
    /// `forward_agent` 只在这条 exec 通道的生命周期内开启 agent 转发；
    /// `cancel` 由 UI 共享，置位后尽快关闭通道并返回取消。
    pub async fn exec_full(
        &self,
        cmd: &str,
        forward_agent: bool,
        cancel: Arc<AtomicBool>,
        timeout_ms: u64,
    ) -> BackendResult<ExecOut> {
        let mut ch = self
            .session
            .channel_open_session()
            .await
            .map_err(|e| BackendError(trf!("创建通道失败: {e}", e = e)))?;
        let _arm = if forward_agent {
            if let Err(e) = ch.agent_forward(true).await {
                return Err(BackendError(trf!("请求 agent 转发失败: {e}", e = e)));
            }
            AGENT_FORWARD_ARMED.fetch_add(1, Ordering::Relaxed);
            Some(AgentArm(true))
        } else {
            None
        };
        ch.exec(true, cmd)
            .await
            .map_err(|e| BackendError(trf!("执行命令失败: {e}", e = e)))?;

        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let mut code: Option<u32> = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = tokio::time::timeout(Duration::from_secs(2), ch.close()).await;
                return Err(BackendError(tr("已取消").into()));
            }
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                let _ = ch.close().await;
                return Err(BackendError(tr("执行超时").into()));
            }
            // 200ms 一轮：既能收输出，也能及时看到取消
            match tokio::time::timeout(remain.min(Duration::from_millis(200)), ch.wait()).await {
                Err(_) => continue,
                Ok(None) => break,
                Ok(Some(msg)) => match msg {
                    russh::ChannelMsg::Data { data } => {
                        let room = 1_048_576usize.saturating_sub(out.len());
                        out.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    russh::ChannelMsg::ExtendedData { data, ext } if ext == 1 => {
                        let room = 65_536usize.saturating_sub(err.len());
                        err.extend_from_slice(&data[..data.len().min(room)]);
                    }
                    russh::ChannelMsg::ExitStatus { exit_status } => {
                        code = Some(exit_status);
                    }
                    russh::ChannelMsg::ExitSignal { .. } | russh::ChannelMsg::Close => break,
                    _ => {}
                },
            }
        }
        Ok(ExecOut {
            code,
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        })
    }

    /// 执行远程命令并收集 stdout/stderr（对应 C 版 sftp_backend_exec）。
    /// 目录跟随轮询使用；输出超过 max_out 字节时截断。
    pub async fn exec(
        &self,
        cmd: &str,
        timeout_ms: u64,
        max_out: usize,
    ) -> BackendResult<String> {
        let mut ch = self
            .session
            .channel_open_session()
            .await
            .map_err(|e| BackendError(trf!("创建通道失败: {e}", e = e)))?;
        ch.exec(true, cmd)
            .await
            .map_err(|e| BackendError(trf!("执行命令失败: {e}", e = e)))?;

        let mut out: Vec<u8> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        {
            let mut reader = ch.make_reader();
            let mut buf = [0u8; 512];
            loop {
                if out.len() >= max_out {
                    break;
                }
                let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remain.is_zero() {
                    break;
                }
                match tokio::time::timeout(remain, reader.read(&mut buf)).await {
                    Err(_) => break, // 到达 deadline
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => {
                        let take = n.min(max_out - out.len());
                        out.extend_from_slice(&buf[..take]);
                    }
                    Ok(Err(_)) => break,
                }
            }
        }
        // 等对端关闭确认，避免紧接着的下一次 exec 通道 open 失败（C 版同款处理）
        let _ = tokio::time::timeout(Duration::from_secs(3), ch.close()).await;

        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{AuthMethod, AuthOrder};

    /// 认证优先级只决定几段凭据的先后；顺序必须正确。
    /// 真正的「成功后不再试下一段」由 `authenticate` 的循环保证。
    ///
    /// ⚠️ 「自动」的顺序与阶段 70 之前**逐项相同**
    /// （密码 → 指定私钥 → agent → 默认密钥，或反过来）。
    #[test]
    fn auth_plan_respects_order() {
        assert_eq!(
            auth_plan(AuthMethod::Auto, AuthOrder::PasswordFirst),
            [
                AuthStep::Password,
                AuthStep::KeyFile,
                AuthStep::Agent,
                AuthStep::DefaultKeys
            ]
        );
        assert_eq!(
            auth_plan(AuthMethod::Auto, AuthOrder::PublicKeyFirst),
            [
                AuthStep::KeyFile,
                AuthStep::Agent,
                AuthStep::DefaultKeys,
                AuthStep::Password
            ]
        );
    }

    /// 认证方式决定**允许用哪些凭据**（阶段 70）：
    /// 「只用密码」就别把本地身份出示给堡垒机网关，
    /// 「只用私钥文件」就别去翻 ssh-agent。
    #[test]
    fn auth_method_limits_credentials() {
        assert_eq!(
            auth_plan(AuthMethod::Password, AuthOrder::PasswordFirst),
            [AuthStep::Password]
        );
        assert_eq!(
            auth_plan(AuthMethod::KeyFile, AuthOrder::PublicKeyFirst),
            [AuthStep::KeyFile]
        );
        assert_eq!(
            auth_plan(AuthMethod::Agent, AuthOrder::PasswordFirst),
            [AuthStep::Agent]
        );
        // 方式优先于顺序：密钥优先 + 只用密码 ⇒ 还是只有密码
        assert_eq!(
            auth_plan(AuthMethod::Password, AuthOrder::PublicKeyFirst),
            [AuthStep::Password]
        );
    }
}
