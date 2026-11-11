//! 进程内端口转发（Rust-only 功能，C 版无进程内转发，由 spawn 的
//! ssh -L/-R/-D 承担）：三种转发都跑在应用自己那条 SSH 连接上，
//! **不新建 SSH 连接**，也不依赖终端标签页的生死。
//!
//! - 本地（-L）：监听本地 TCP，每个接入连接开一条 direct-tcpip 通道
//!   双向泵数据；目标由**服务器侧**解析。
//! - 远程（-R）：请求服务器在**远端**监听（tcpip-forward），服务器把
//!   入站连接送回来（forwarded-tcpip 通道），客户端再连目标；
//!   目标在**客户端侧**解析（SSH 协议的 tcpip-forward 请求只带绑定
//!   地址/端口，不带目标），与 OpenSSH 的 -R 行为一致。
//! - 动态（-D）：监听本地 SOCKS5 端口，握手拿到目标后同样走
//!   direct-tcpip 通道。
//!
//! 转发任务随 SFTP 连接生命周期启停。

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use russh::client::Handle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::conn::HostKeyHandler;
use super::{BackendError, BackendResult};
use crate::i18n::tr;
use crate::trf;

/// 一条本地转发规则（"-L"）。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalForward {
    /// 监听地址；空表示 127.0.0.1（与 ssh -L 默认一致，只接受本机接入）
    pub listen_host: String,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

impl LocalForward {
    /// 展示用一行：127.0.0.1:8080 → 10.0.0.1:80
    /// 空监听地址按 127.0.0.1 呈现（与实际绑定一致），不需要真去 bind。
    pub fn display(&self) -> String {
        let lh = self.effective_listen_host();
        format!("{lh}:{} → {}:{}",
            self.listen_port, self.target_host, self.target_port
        )
    }

    /// 空监听地址 = 127.0.0.1（与 ssh -L 默认一致，只接受本机接入）
    pub fn effective_listen_host(&self) -> &str {
        if self.listen_host.is_empty() {
            "127.0.0.1"
        } else {
            &self.listen_host
        }
    }
}

/// 一条远程转发规则（"-R"）。
///
/// ⚠️ **方向与 -L 相反**：监听端在**远端**，目标在**客户端侧**解析。
/// 规格形状与 -L 一样（`[绑定地址:]端口:目标主机:目标端口`）。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteForward {
    /// 远端绑定地址；空表示 127.0.0.1（OpenSSH 的 -R 默认只绑回环）
    pub bind_host: String,
    pub bind_port: u16,
    /// 目标（**客户端侧**解析并连接）
    pub target_host: String,
    pub target_port: u16,
}

impl RemoteForward {
    /// 展示用一行：远端 127.0.0.1:2222 → 10.0.0.1:22
    pub fn display(&self) -> String {
        let bh = self.effective_bind_host();
        trf!("远程 {bh}:{v0} → {v1}:{v2}", bh = bh, v0 = self.bind_port, v1 = self.target_host, v2 = self.target_port)
    }

    /// 空绑定地址 = 127.0.0.1（与 OpenSSH 的 -R 默认一致）
    pub fn effective_bind_host(&self) -> &str {
        if self.bind_host.is_empty() {
            "127.0.0.1"
        } else {
            &self.bind_host
        }
    }
}

/// 一条动态转发规则（"-D"）：本地 SOCKS5 代理。
#[derive(Debug, Clone, PartialEq)]
pub struct SocksForward {
    pub listen_host: String,
    pub listen_port: u16,
}

impl SocksForward {
    /// 展示用一行：SOCKS 127.0.0.1:1080
    pub fn display(&self) -> String {
        format!("SOCKS {}:{}", self.effective_listen_host(), self.listen_port)
    }

    /// 空监听地址 = 127.0.0.1（与 ssh -D 默认一致，只接受本机接入）
    pub fn effective_listen_host(&self) -> &str {
        if self.listen_host.is_empty() {
            "127.0.0.1"
        } else {
            &self.listen_host
        }
    }
}

/// 解析单条本地转发规则（不含 "L:" 前缀），两种形状：
/// - CLI 冒号形（putty -L）：`[监听地址:]监听端口:目标主机:目标端口`
/// - 会话文件等号形（PuTTY PortForwardings）：`监听端口=目标主机:目标端口`
pub fn parse_local(spec: &str) -> Result<LocalForward, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(tr("转发规则为空").into());
    }
    let (listen_host, listen_port, dest) = if let Some(eq) = spec.find('=') {
        (
            String::new(),
            parse_port(spec[..eq].trim(), tr("监听"))?,
            spec[eq + 1..].trim().to_string(),
        )
    } else {
        // 从右向左拆两段：目标端口、目标主机，剩下的按 [监听地址:]监听端口
        let (dport, rest) = rsplit_field(spec, tr("目标"))?;
        let (dhost, rest) = rsplit_field(&rest, tr("目标"))?;
        let dest = format!("{dhost}:{dport}");
        match rest.rfind(':') {
            Some(i) => (
                rest[..i].trim().to_string(),
                parse_port(rest[i + 1..].trim(), tr("监听"))?,
                dest,
            ),
            None => (String::new(), parse_port(rest.trim(), tr("监听"))?, dest),
        }
    };
    let (target_host, target_port) = split_dest(&dest)?;
    Ok(LocalForward {
        listen_host,
        listen_port,
        target_host,
        target_port,
    })
}

/// 解析远程转发（"-R"）：规格形状与 -L 相同，语义不同
/// （监听在远端、目标在客户端侧解析）。
pub fn parse_remote(spec: &str) -> Result<RemoteForward, String> {
    let l = parse_local(spec)?;
    Ok(RemoteForward {
        bind_host: l.listen_host,
        bind_port: l.listen_port,
        target_host: l.target_host,
        target_port: l.target_port,
    })
}

/// 解析动态转发（"-D"）：`[监听地址:]端口`（地址可省，默认 127.0.0.1）。
pub fn parse_socks(spec: &str) -> Result<SocksForward, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(tr("SOCKS 端口为空").into());
    }
    let (host, port) = match spec.rfind(':') {
        Some(i) => (
            spec[..i].trim().to_string(),
            spec[i + 1..].trim().to_string(),
        ),
        None => (String::new(), spec.to_string()),
    };
    Ok(SocksForward {
        listen_host: host,
        listen_port: parse_port(&port, "SOCKS")?,
    })
}

/// 拆 "host:port"（从右取端口）。未加方括号的裸 IPv6 地址会拆错——
/// 与 ssh -L 一致要求 IPv6 写作 [addr]:port，这里不支持该写法，直接报错。
fn split_dest(dest: &str) -> Result<(String, u16), String> {
    let (port, host) = rsplit_field(dest, tr("目标"))?;
    if host.is_empty() {
        return Err(tr("目标主机为空").into());
    }
    Ok((host, parse_port(&port, tr("目标"))?))
}

fn rsplit_field(s: &str, what: &str) -> Result<(String, String), String> {
    match s.rfind(':') {
        Some(i) => Ok((s[i + 1..].trim().to_string(), s[..i].to_string())),
        None => Err(trf!("{what}段缺失（应为 端口:主机:端口）: {s}", what = what, s = s)),
    }
}

fn parse_port(s: &str, what: &str) -> Result<u16, String> {
    let v = crate::util::atoi_like(s);
    match u16::try_from(v) {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(trf!("{what}端口无效: {s}", what = what, s = s)),
    }
}

/* ---------------- 远程转发的入站登记表 ---------------- */

/// 一条远程转发（-R）的绑定：服务器在 (bind_host, bind_port) 监听，
/// 入站连接送回来后客户端要连的目标是 (target_host, target_port)。
#[derive(Debug, Clone, PartialEq)]
pub struct RfwdBinding {
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// 远程转发的入站登记表（-R）。
///
/// 服务器把入站连接通过 forwarded-tcpip 通道送回来时，通道里**没有**
/// 目标地址——目标只有客户端知道（SSH 协议的 tcpip-forward 请求只带
/// 绑定地址/端口）。所以客户端侧得自己记一份「哪个绑定对应哪个目标」。
///
/// 用共享表（Arc<Mutex<…>>）而不是塞进 handler：handler 在 connect()
/// 时就建好了，转发规则是之后才装载的；两边共享同一张表最省事。
#[derive(Clone, Default)]
pub struct RfwdTable {
    inner: Arc<Mutex<Vec<RfwdBinding>>>,
}

impl RfwdTable {
    pub fn insert(&self, b: RfwdBinding) {
        let mut v = self.inner.lock().unwrap();
        v.retain(|x| !(x.bind_port == b.bind_port && x.bind_host == b.bind_host));
        v.push(b);
    }

    pub fn remove(&self, bind_host: &str, bind_port: u16) {
        let mut v = self.inner.lock().unwrap();
        v.retain(|x| !(x.bind_port == bind_port && x.bind_host == bind_host));
    }

    /// 按 (地址, 端口) 找目标。
    ///
    /// 地址对不上就按端口兜底：服务器回报的绑定地址可能是 "0.0.0.0"
    /// 或实际网卡地址，跟我们登记的（127.0.0.1）不一样。端口在同一条
    /// 连接内唯一，按端口兜底不会串台。
    pub fn lookup(&self, addr: &str, port: u32) -> Option<(String, u16)> {
        let v = self.inner.lock().unwrap();
        let p = u16::try_from(port).ok()?;
        if let Some(b) = v.iter().find(|b| b.bind_port == p && b.bind_host == addr) {
            return Some((b.target_host.clone(), b.target_port));
        }
        let b = v.iter().find(|b| b.bind_port == p)?;
        Some((b.target_host.clone(), b.target_port))
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/* ---------------- 运行中的转发 ---------------- */

/// 运行中的转发（L/R/D 三种共用一个壳）。
pub struct ActiveForward {
    /// 本地监听地址（L/D）；R 是在**远端**监听，这里是 None
    pub listen: Option<SocketAddr>,
    pub target_host: String,
    pub target_port: u16,
    /// 远端绑定（R）：停用时要通知服务器收回监听
    remote_bind: Option<(String, u16)>,
    /// 远端转发登记表：停用时要把绑定摘掉，否则服务器再送入站连接
    /// 过来，handler 会连到一条已经停掉的规则上
    rfwd: RfwdTable,
    /// 会话句柄（R）：停用时用它发 cancel_tcpip_forward
    session: Option<Arc<Handle<HostKeyHandler>>>,
    /// 本地 accept 循环（L/D）的中止句柄；R 没有本地监听
    abort: Option<tokio::task::AbortHandle>,
    /// accept 循环的任务句柄（L/D）。
    ///
    /// ⚠️ **为什么留着它**：`abort()` 是**异步**取消 ⇒ 监听 socket 还在
    ///    ⇒ 立刻重新 bind 同一个端口会 EADDRINUSE（"同一个监听端口换一
    ///    个目标"这种常见改法必踩）。⇒ 停规则时**等这个任务真的结束**
    ///    （见 [`ActiveForward::wait`]），监听 socket 才会被释放。
    accept_task: Option<tokio::task::JoinHandle<()>>,
    /// 已接入连接的泵任务。停止转发时必须一并 abort：否则删掉规则后
    /// 老连接还能继续跑数据，UI 上却已经"没有这条转发了"。
    /// accept 循环每次接入时按 is_finished 顺手清理，长命转发不积累句柄。
    pumps: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl ActiveForward {
    pub fn stop(&self) {
        if let Some(a) = &self.abort {
            a.abort();
        }
        for p in self.pumps.lock().unwrap().drain(..) {
            p.abort();
        }
        if let (Some((host, port)), Some(session)) = (&self.remote_bind, &self.session) {
            self.rfwd.remove(host, *port);
            let (h, p) = (host.clone(), *port);
            let s = Arc::clone(session);
            // 通知服务器收回远端监听。tokio::spawn 得在运行时里——
            // stop() 有可能从同步上下文调到；没有运行时就跳过
            // （登记表已经摘掉，服务器送来的连接会被拒绝，不会串台）。
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = s.cancel_tcpip_forward(h, u32::from(p)).await;
                });
            }
        }
    }

    /// **等 accept 循环真的结束**（⇒ 监听 socket 释放、端口可再绑）。
    ///
    /// ⚠️ **必须在重新绑定同一个端口之前调**：`stop()` 只是 `abort()`，
    ///    取消是异步的 ⇒ 不等的话端口还占着 ⇒ EADDRINUSE。
    pub async fn wait(&mut self) {
        if let Some(h) = self.accept_task.take() {
            let _ = h.await;
        }
    }
}

/// 绑定本地监听并启动接受循环；返回运行中的转发。
/// session 经 Arc 共享：Handle 0.63 不可 Clone，但 channel_open 只要求 &self。
async fn start_local(
    session: Arc<Handle<HostKeyHandler>>,
    spec: &LocalForward,
) -> BackendResult<ActiveForward> {
    let bind_host: &str = spec.effective_listen_host();
    let listener = TcpListener::bind((bind_host, spec.listen_port))
        .await
        .map_err(|e| BackendError(trf!("监听 {bind_host}:{v0} 失败: {e}", bind_host = bind_host, e = e, v0 = spec.listen_port)))?;
    let listen = listener
        .local_addr()
        .map_err(|e| BackendError(trf!("监听地址获取失败: {e}", e = e)))?;

    let target_host = spec.target_host.clone();
    let target_port = spec.target_port;
    let pumps: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let pumps_in = Arc::clone(&pumps);
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, peer)) => {
                    let h = session.clone();
                    let th = target_host.clone();
                    let kid = tokio::spawn(async move {
                        let _ = pump(h, sock, peer, th, target_port).await;
                    });
                    let mut v = pumps_in.lock().unwrap();
                    v.retain(|k| !k.is_finished());
                    v.push(kid);
                }
                // 监听 socket 损坏（极罕见）：接受循环退出，转发静默失效
                Err(_) => break,
            }
        }
    });
    let abort = task.abort_handle();
    Ok(ActiveForward {
        listen: Some(listen),
        target_host: spec.target_host.clone(),
        target_port,
        remote_bind: None,
        rfwd: RfwdTable::default(),
        session: None,
        abort: Some(abort),
        accept_task: Some(task),
        pumps,
    })
}

/// 单条接入连接：开 direct-tcpip 通道并双向泵数据，任一端断开即结束。
async fn pump(
    session: Arc<Handle<HostKeyHandler>>,
    sock: TcpStream,
    peer: SocketAddr,
    target_host: String,
    target_port: u16,
) -> Result<(), ()> {
    let ch = session
        .channel_open_direct_tcpip(
            &target_host,
            u32::from(target_port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
        .map_err(|_| ())?;
    let mut ssh_stream = ch.into_stream();
    let mut sock = sock;
    let _ = tokio::io::copy_bidirectional(&mut sock, &mut ssh_stream).await;
    Ok(())
}

/// 启动远程转发（-R）：请求服务器在远端监听，登记入站绑定。
///
/// 服务器只在请求端口为 0 时才回实际端口；端口非 0 时回 0 表示成功，
/// 所以这里要把 0 换回自己请求的端口。
async fn start_remote(
    session: Arc<Handle<HostKeyHandler>>,
    spec: &RemoteForward,
    rfwd: RfwdTable,
) -> BackendResult<ActiveForward> {
    let bind_host = spec.effective_bind_host().to_string();
    let ret = session
        .tcpip_forward(bind_host.clone(), u32::from(spec.bind_port))
        .await
        .map_err(|e| {
            BackendError(trf!("远程监听 {bind_host}:{v0} 失败: {e}（服务器可能禁止 TCP 转发）", bind_host = bind_host, e = e, v0 = spec.bind_port))
        })?;
    let bind_port = if ret == 0 {
        spec.bind_port
    } else {
        u16::try_from(ret).unwrap_or(spec.bind_port)
    };
    rfwd.insert(RfwdBinding {
        bind_host: bind_host.clone(),
        bind_port,
        target_host: spec.target_host.clone(),
        target_port: spec.target_port,
    });
    Ok(ActiveForward {
        listen: None,
        target_host: spec.target_host.clone(),
        target_port: spec.target_port,
        remote_bind: Some((bind_host, bind_port)),
        rfwd,
        session: Some(session),
        abort: None,
        accept_task: None,
        pumps: Arc::new(Mutex::new(Vec::new())),
    })
}

/// 启动动态转发（-D）：本地 SOCKS5 代理。
async fn start_socks(
    session: Arc<Handle<HostKeyHandler>>,
    spec: &SocksForward,
) -> BackendResult<ActiveForward> {
    let bind_host = spec.effective_listen_host();
    let listener = TcpListener::bind((bind_host, spec.listen_port))
        .await
        .map_err(|e| BackendError(trf!("监听 {bind_host}:{v0} 失败: {e}", bind_host = bind_host, e = e, v0 = spec.listen_port)))?;
    let listen = listener
        .local_addr()
        .map_err(|e| BackendError(trf!("监听地址获取失败: {e}", e = e)))?;
    let pumps: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let pumps_in = Arc::clone(&pumps);
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((sock, peer)) => {
                    let h = session.clone();
                    let kid = tokio::spawn(async move {
                        let _ = socks_pump(h, sock, peer).await;
                    });
                    let mut v = pumps_in.lock().unwrap();
                    v.retain(|k| !k.is_finished());
                    v.push(kid);
                }
                Err(_) => break,
            }
        }
    });
    let abort = task.abort_handle();
    Ok(ActiveForward {
        listen: Some(listen),
        target_host: String::new(),
        target_port: 0,
        remote_bind: None,
        rfwd: RfwdTable::default(),
        session: None,
        abort: Some(abort),
        accept_task: Some(task),
        pumps,
    })
}

/// 一条 SOCKS5 接入：握手 → 取目标 → direct-tcpip 通道 → 双向泵。
async fn socks_pump(
    session: Arc<Handle<HostKeyHandler>>,
    mut sock: TcpStream,
    peer: SocketAddr,
) -> Result<(), ()> {
    let (host, port) = socks5_handshake(&mut sock).await.map_err(|_| ())?;
    let ch = session
        .channel_open_direct_tcpip(
            &host,
            u32::from(port),
            peer.ip().to_string(),
            u32::from(peer.port()),
        )
        .await
        .map_err(|_| ())?;
    let mut ssh_stream = ch.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut sock, &mut ssh_stream).await;
    Ok(())
}

/// SOCKS5 握手（只做「无认证 + CONNECT」这一种，够 -D 用）。
///
/// 流程：客户端 `05 NMETHODS METHODS…` → 我们回 `05 00` →
/// 客户端 `05 01 00 ATYP ADDR PORT` → 我们回 `05 00 00 01 0.0.0.0 0`
/// （应答里的地址端口是「实际连上哪里」，可以填 0）→ 之后是裸数据。
async fn socks5_handshake(sock: &mut TcpStream) -> Result<(String, u16), String> {
    let mut head = [0u8; 2];
    sock.read_exact(&mut head)
        .await
        .map_err(|e| trf!("SOCKS5 握手失败: {e}", e = e))?;
    if head[0] != 5 {
        return Err(tr("不是 SOCKS5 请求").into());
    }
    let mut methods = vec![0u8; head[1] as usize];
    if !methods.is_empty() {
        sock.read_exact(&mut methods)
            .await
            .map_err(|e| trf!("SOCKS5 握手失败: {e}", e = e))?;
    }
    if !methods.contains(&0) {
        return Err(tr("SOCKS5 客户端要求认证（本程序只提供无认证）").into());
    }
    sock.write_all(&[5, 0])
        .await
        .map_err(|e| trf!("SOCKS5 握手失败: {e}", e = e))?;

    let mut req = [0u8; 4];
    sock.read_exact(&mut req)
        .await
        .map_err(|e| trf!("SOCKS5 请求失败: {e}", e = e))?;
    if req[0] != 5 {
        return Err(tr("SOCKS5 请求版本不对").into());
    }
    if req[1] != 1 {
        return Err(tr("SOCKS5 只支持 CONNECT（绑定/UDP 不支持）").into());
    }
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a)
                .await
                .map_err(|e| trf!("SOCKS5 地址读取失败: {e}", e = e))?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut n = [0u8; 1];
            sock.read_exact(&mut n)
                .await
                .map_err(|e| trf!("SOCKS5 地址读取失败: {e}", e = e))?;
            let mut b = vec![0u8; n[0] as usize];
            sock.read_exact(&mut b)
                .await
                .map_err(|e| trf!("SOCKS5 地址读取失败: {e}", e = e))?;
            String::from_utf8_lossy(&b).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a)
                .await
                .map_err(|e| trf!("SOCKS5 地址读取失败: {e}", e = e))?;
            Ipv6Addr::from(a).to_string()
        }
        _ => return Err(tr("SOCKS5 地址类型不支持").into()),
    };
    let mut p = [0u8; 2];
    sock.read_exact(&mut p)
        .await
        .map_err(|e| trf!("SOCKS5 端口读取失败: {e}", e = e))?;
    let port = u16::from_be_bytes(p);
    if port == 0 {
        return Err(tr("SOCKS5 目标端口无效").into());
    }
    sock.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
        .map_err(|e| trf!("SOCKS5 应答失败: {e}", e = e))?;
    Ok((host, port))
}

/* ---------------- 谁来承接转发 ---------------- */

/// 命令行的转发规则（`-L/-R/-D`）**由谁承接**。
///
/// - `true`  ⇒ 由**程序内的转发引擎**承接（[`FwdRule`] / 本模块的
///   `start_local` / `start_remote` / `start_socks` 全部走 russh 的
///   direct-tcpip 通道 ⇒ 平台无关）。
/// - `false` ⇒ 交给**终端 spawn 的 ssh/plink** 承接（它们用 `-L/-R/-D`
///   的 argv 自己实现）⇒ 进程内就不要再绑同一个端口（会 EADDRINUSE）。
///
/// ⚠️ **判据是「终端用哪条路连」，不是「系统里有没有 ssh」**：
///
///    - 终端走 **spawn**（Linux 主线）：转发进 ssh 的 argv；
///    - 终端走 **inproc**（Windows 默认）：终端**根本没有** ssh 进程 ⇒
///      转发只能由程序内承接。
///
///    ⚠️ 旧判据（「系统里找得到 ssh/plink 就当转发归它管」）在 Windows 上
///    **必错**：Win10/11 把 OpenSSH 做成可选功能，很多人装了 ⇒ 转发被判给
///    一个**根本不会被 spawn** 的 ssh ⇒ 转发静默失效、界面上一点提示都没有。
///
/// ⚠️ `is_ssh = false`（telnet/串口等）⇒ 没有 SSH 通道 ⇒ 转发无处可去 ⇒
///    返回 false（不假装能做）。
pub fn needs_inproc_forwards(is_ssh: bool, term_inproc: bool) -> bool {
    if !is_ssh {
        return false;
    }
    term_inproc
}

/// **这条连接要在进程内承接的转发规则**（终端与文件面板算出**同一份**）。
///
/// ⚠️ **两边必须一致**：终端与文件面板都往**同一条共享连接**上装规则 ⇒
///    算得不一样就会互相把对方的规则换掉（转发反复重启、端口撞车）。
///
/// - `forwards`：命令行的 `-L/-R/-D` ⇒ **只有终端走进程内时**才由程序内
///   承接（终端走 spawn 时它们在 ssh 的 argv 里 ⇒ 进程内再绑同一个端口
///   会 EADDRINUSE）。
/// - `inproc_forwards`：连接配置里的「进程内端口映射」⇒ 始终由程序内承接。
pub fn inproc_rules(
    is_ssh: bool,
    term_inproc: bool,
    forwards: &[String],
    inproc_forwards: &[String],
) -> Vec<FwdRule> {
    if !is_ssh {
        return Vec::new();
    }
    let mut out = Vec::new();
    if needs_inproc_forwards(is_ssh, term_inproc) {
        out.extend(parse_all(forwards));
    }
    out.extend(parse_all(inproc_forwards));
    out
}

/// 逐条解析规则列表（坏条目就地丢弃，不牵连其余规则）。
fn parse_all(list: &[String]) -> Vec<FwdRule> {
    list.iter().flat_map(|f| parse_rules(f)).collect()
}

/* ---------------- 会话级规则：持久化 + 与 PuTTY 互转 ---------------- */

/// 一条会话级转发规则。
///
/// **存储用内部形**（与 `ConnOptions.forwards` 同形）：
/// - `L:[监听地址:]监听端口:目标主机:目标端口`
/// - `R:[绑定地址:]监听端口:目标主机:目标端口`
/// - `D:[监听地址:]端口`（SOCKS 动态转发）
///
/// 停用规则加 `!` 前缀（保留规则但会话启动时不绑定）。
/// 存内部形而不直接存 PuTTY `PortForwardings` 形，是因为内部形多一个
/// 监听地址字段、是它的超集，存进去不丢信息；与 PuTTY 的互转由
/// `to_putty_value` / `from_putty_value` 负责。
#[derive(Debug, Clone, PartialEq)]
pub struct FwdRule {
    /// 'L' 本地 / 'R' 远程 / 'D' 动态(SOCKS)
    pub kind: char,
    /// 去掉 kind 前缀的规格体："9000:127.0.0.1:9000"、"1080"
    pub spec: String,
    /// false = 停用
    pub enabled: bool,
}

impl FwdRule {
    pub fn new(kind: char, spec: &str, enabled: bool) -> Self {
        Self {
            kind,
            spec: spec.to_string(),
            enabled,
        }
    }

    /// 规则描述（不含运行态：启用/停用/失败由 FwdView 呈现）。
    /// L/R 展开成「监听 → 目标」，D 展示 SOCKS 端口；
    /// 解析不了时退回原始规格，用户仍认得出是哪条。
    ///
    /// 带类型前缀：L 与 R 的字段长得一样（都是「地址:端口 → 地址:端口」），
    /// 不加前缀用户在列表里分不清哪个是本地、哪个是远端。
    pub fn display(&self) -> String {
        match self.kind {
            'L' => self
                .to_local()
                .map(|l| trf!("本地 {v0}", v0 = l.display()))
                .unwrap_or_else(|_| self.spec.clone()),
            'R' => self
                .to_remote()
                .map(|r| r.display())
                .unwrap_or_else(|_| self.spec.clone()),
            'D' => self
                .to_socks()
                .map(|s| s.display())
                .unwrap_or_else(|_| format!("SOCKS {}", self.spec)),
            _ => self.spec.clone(),
        }
    }

    /// 类型中文名（UI 徽标用）
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            'L' => tr("本地"),
            'R' => tr("远程"),
            'D' => tr("动态"),
            _ => tr("未知"),
        }
    }

    /// 解析成可进程内实现的本地转发规格（只认 'L'）
    pub fn to_local(&self) -> Result<LocalForward, String> {
        if self.kind != 'L' {
            return Err(trf!("规则类型 {v0} 不是本地转发", v0 = self.kind));
        }
        parse_local(&self.spec)
    }

    /// 解析成可进程内实现的远程转发规格（只认 'R'）
    pub fn to_remote(&self) -> Result<RemoteForward, String> {
        if self.kind != 'R' {
            return Err(trf!("规则类型 {v0} 不是远程转发", v0 = self.kind));
        }
        parse_remote(&self.spec)
    }

    /// 解析成可进程内实现的动态转发规格（只认 'D'）
    pub fn to_socks(&self) -> Result<SocksForward, String> {
        if self.kind != 'D' {
            return Err(trf!("规则类型 {v0} 不是动态转发", v0 = self.kind));
        }
        parse_socks(&self.spec)
    }
}

/// 解析规则列表（内部形，逗号或换行分隔，`!` 前缀 = 停用）。
/// 两种分隔符都收：命令行给逗号串，而 connections.ini 是手写的，
/// 用户按行写规则更自然——分隔符的事在这一处兜住，别散给调用方。
/// 逐条尽力解析：坏条目直接丢弃，不牵连其余规则
/// （一条坏行不该让整个连接的转发全废）。
pub fn parse_rules(value: &str) -> Vec<FwdRule> {
    let mut out = Vec::new();
    for item in value.split([',', '\n', '\r']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (enabled, body) = match item.strip_prefix('!') {
            Some(r) => (false, r.trim()),
            None => (true, item),
        };
        let mut chars = body.chars();
        let Some(kind) = chars.next() else { continue };
        if !matches!(kind, 'L' | 'R' | 'D') {
            continue;
        }
        let rest = &body[kind.len_utf8()..];
        let spec = rest.strip_prefix(':').unwrap_or(rest).trim();
        if spec.is_empty() {
            continue;
        }
        let rule = FwdRule::new(kind, spec, enabled);
        let ok = match kind {
            'L' => rule.to_local().is_ok(),
            'R' => rule.to_remote().is_ok(),
            _ => rule.to_socks().is_ok(),
        };
        if ok {
            out.push(rule);
        }
    }
    out
}

/// 规则列表 → 内部形的存储值（`!` 标停用），写 connections.ini 用。
pub fn to_storage_value(rules: &[FwdRule]) -> String {
    rules
        .iter()
        .map(|r| {
            if r.enabled {
                format!("{}:{}", r.kind, r.spec)
            } else {
                format!("!{}:{}", r.kind, r.spec)
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 单条规则 → PuTTY `PortForwardings` 片段。
/// `None` = PuTTY 表达不了：停用规则、写了显式监听地址
/// （PuTTY 格式只有监听端口，没有地址字段）。
pub fn rule_to_putty(r: &FwdRule) -> Option<String> {
    if !r.enabled {
        return None;
    }
    match r.kind {
        'D' => {
            let s = r.to_socks().ok()?;
            if !s.listen_host.is_empty() {
                return None;
            }
            Some(format!("D{0}=", s.listen_port))
        }
        'L' | 'R' => {
            // 走已验证的解析器拿结构化字段，别自己切字符串
            let l = if r.kind == 'L' {
                r.to_local().ok()?
            } else {
                let rm = r.to_remote().ok()?;
                LocalForward {
                    listen_host: rm.bind_host,
                    listen_port: rm.bind_port,
                    target_host: rm.target_host,
                    target_port: rm.target_port,
                }
            };
            if !l.listen_host.is_empty() {
                return None;
            }
            Some(format!("{}{}={}:{}",
                r.kind, l.listen_port, l.target_host, l.target_port
            ))
        }
        _ => None,
    }
}

/// 规则列表 → PuTTY `PortForwardings` 整串；`skipped` 收集表达不了的规则
/// （导出 PuTTY 会话时应当提示用户，否则会静默丢规则）。
pub fn to_putty_value(rules: &[FwdRule]) -> (String, Vec<String>) {
    let mut parts = Vec::new();
    let mut skipped = Vec::new();
    for r in rules {
        match rule_to_putty(r) {
            Some(s) => parts.push(s),
            None => skipped.push(if r.enabled {
                r.display()
            } else {
                trf!("{v0}（已停用）", v0 = r.display())
            }),
        }
    }
    (parts.join(","), skipped)
}

/// PuTTY `PortForwardings` 整串 → 内部形存储值（导入方向）。
/// PuTTY 没有停用概念，导入的规则一律启用。
pub fn from_putty_value(value: &str) -> String {
    let mut internal = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        let mut chars = item.chars();
        let Some(kind) = chars.next() else { continue };
        if !matches!(kind, 'L' | 'R' | 'D') {
            continue;
        }
        let rest = &item[kind.len_utf8()..];
        match rest.split_once('=') {
            // L9000=host:80 → L:9000:host:80
            Some((listen, dest)) if !dest.trim().is_empty() => {
                internal.push(format!("{kind}:{}:{}", listen.trim(), dest.trim()));
            }
            // D1080= → D:1080
            Some((p, d)) if kind == 'D' && d.trim().is_empty() && !p.trim().is_empty() => {
                internal.push(format!("D:{}", p.trim()));
            }
            _ => {}
        }
    }
    internal.join(",")
}

/// 运行中的一条规则：规则本体 + 运行态。
/// 停用时 active 为 None；绑定失败时 err 记原因、规则仍留在列表里
/// （UI 要能看见失败原因，也能重新启用重试）。
pub struct FwdEntry {
    pub rule: FwdRule,
    active: Option<ActiveForward>,
    err: Option<String>,
}

impl FwdEntry {
    pub fn new(rule: FwdRule) -> Self {
        Self {
            rule,
            active: None,
            err: None,
        }
    }

    pub fn active(&self) -> Option<&ActiveForward> {
        self.active.as_ref()
    }

    pub fn err(&self) -> Option<&str> {
        self.err.as_deref()
    }

    /// 稳定标识（UI 的启停/删除按它匹配）：kind + 规格体。
    /// 不能用绑定后的监听地址——停用的规则没有绑定，得能照样选中。
    pub fn key(&self) -> String {
        format!("{}:{}", self.rule.kind, self.rule.spec)
    }

    /// 会话启动 / 重新启用。已在跑则幂等返回。
    ///
    /// 按 kind 分派到三种进程内实现：
    /// - 'L' 本地监听 → direct-tcpip
    /// - 'R' 远端监听（tcpip-forward）→ 入站连接回来后本机连目标
    /// - 'D' 本地 SOCKS5 → direct-tcpip
    pub async fn start(
        &mut self,
        session: Arc<Handle<HostKeyHandler>>,
        rfwd: RfwdTable,
    ) -> BackendResult<()> {
        if self.active.is_some() {
            return Ok(());
        }
        let res: BackendResult<ActiveForward> = match self.rule.kind {
            'L' => match self.rule.to_local() {
                Ok(s) => start_local(session, &s).await,
                Err(msg) => Err(BackendError(msg)),
            },
            'R' => match self.rule.to_remote() {
                Ok(s) => start_remote(session, &s, rfwd).await,
                Err(msg) => Err(BackendError(msg)),
            },
            'D' => match self.rule.to_socks() {
                Ok(s) => start_socks(session, &s).await,
                Err(msg) => Err(BackendError(msg)),
            },
            _ => Err(BackendError(trf!("未知转发类型：{v0}", v0 = self.rule.kind))),
        };
        match res {
            Ok(a) => {
                self.active = Some(a);
                self.err = None;
                Ok(())
            }
            Err(e) => {
                self.err = Some(e.0.clone());
                Err(e)
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(a) = self.active.take() {
            a.stop();
        }
    }

    /// **停掉这条规则，并等监听端口真正释放**。
    ///
    /// ⚠️ **什么时候必须用它**：要**重新绑定同一个监听端口**的时候
    ///    （改了转发目标 / 换掉一条规则）⇒ `stop()` 只是 `abort()`，
    ///    取消是异步的 ⇒ 不等就会 EADDRINUSE。
    pub async fn stop_wait(&mut self) {
        if let Some(mut a) = self.active.take() {
            a.stop();
            a.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 转发承接看的是终端走哪条路() {
        // ⚠️ **Windows 默认 inproc** ⇒ 转发必须由程序内承接
        //    （哪怕系统里装了 OpenSSH，终端也不 spawn 它）。
        assert!(needs_inproc_forwards(true, true));
        // Linux 主线：终端 spawn ssh ⇒ 转发进 ssh 的 argv ⇒ 程序内别重复绑
        assert!(!needs_inproc_forwards(true, false));
        // 非 SSH 协议（telnet/串口）：没有 SSH 通道 ⇒ 转发无处可去
        assert!(!needs_inproc_forwards(false, true));
        assert!(!needs_inproc_forwards(false, false));
    }

    #[test]
    fn parse_cli_colon_form() {
        let f = parse_local("9000:127.0.0.1:9000").unwrap();
        assert_eq!(
            f,
            LocalForward {
                listen_host: String::new(),
                listen_port: 9000,
                target_host: "127.0.0.1".into(),
                target_port: 9000,
            }
        );
    }

    #[test]
    fn parse_cli_with_listen_addr() {
        let f = parse_local("0.0.0.0:8080:10.0.0.1:80").unwrap();
        assert_eq!(f.listen_host, "0.0.0.0");
        assert_eq!(f.listen_port, 8080);
        assert_eq!(f.target_host, "10.0.0.1");
        assert_eq!(f.target_port, 80);
    }

    #[test]
    fn parse_session_equal_form() {
        // PuTTY PortForwardings 拆出的形状：L:9000=127.0.0.1:9000 → 去前缀后
        let f = parse_local("9000=127.0.0.1:9000").unwrap();
        assert_eq!(f.listen_host, "");
        assert_eq!(f.listen_port, 9000);
        assert_eq!(f.target_host, "127.0.0.1");
        assert_eq!(f.target_port, 9000);
    }

    #[test]
    fn parse_rejects_bad_ports() {
        assert!(parse_local("abc:127.0.0.1:80").is_err());
        assert!(parse_local("70000:127.0.0.1:80").is_err());
        assert!(parse_local("9000:127.0.0.1:-1").is_err());
        assert!(parse_local("9000:127.0.0.1:0").is_err(), "0 端口无效");
        assert!(parse_local("9000=").is_err());
        assert!(parse_local("").is_err());
        assert!(parse_local("9000:80").is_err(), "只给两段不够");
    }

    #[test]
    fn parse_remote_shares_local_shape() {
        let r = parse_remote("2222:host:22").unwrap();
        assert_eq!(r.bind_host, "");
        assert_eq!(r.bind_port, 2222);
        assert_eq!(r.target_host, "host");
        assert_eq!(r.target_port, 22);
        // 远端默认绑回环（OpenSSH 的 -R 默认）
        assert_eq!(r.effective_bind_host(), "127.0.0.1");
        let r2 = parse_remote("0.0.0.0:2222:host:22").unwrap();
        assert_eq!(r2.effective_bind_host(), "0.0.0.0");
        // 目标不能缺
        assert!(parse_remote("2222:").is_err());
    }

    #[test]
    fn parse_socks_forms() {
        let s = parse_socks("1080").unwrap();
        assert_eq!(s.listen_host, "");
        assert_eq!(s.listen_port, 1080);
        assert_eq!(s.effective_listen_host(), "127.0.0.1");
        let s2 = parse_socks("127.0.0.1:1081").unwrap();
        assert_eq!(s2.listen_host, "127.0.0.1");
        assert_eq!(s2.listen_port, 1081);
        assert!(parse_socks("0").is_err());
        assert!(parse_socks("abc").is_err());
        assert!(parse_socks("").is_err());
    }

    #[test]
    fn rules_parse_and_storage_roundtrip() {
        let v = "L:9000:127.0.0.1:9000,!R:2222:host:22,D:1080";
        let rules = parse_rules(v);
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0], FwdRule::new('L', "9000:127.0.0.1:9000", true));
        assert_eq!(rules[1], FwdRule::new('R', "2222:host:22", false));
        assert_eq!(rules[2], FwdRule::new('D', "1080", true));
        // 停用标记往返不丢
        assert_eq!(to_storage_value(&rules), v);
        // 等号形（PuTTY 会话拆出的形状）也认
        assert_eq!(
            parse_rules("L:9000=127.0.0.1:9000")[0],
            FwdRule::new('L', "9000=127.0.0.1:9000", true)
        );
    }

    #[test]
    fn rules_skip_bad_items_only() {
        // 坏 kind / 缺目标 / 端口越界 / 空项 —— 只丢自己，其余照常
        let rules = parse_rules("X:1:2:3,L:9000:host:80,R:2222:,D:0,L:70000:h:1,,D:1080");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0], FwdRule::new('L', "9000:host:80", true));
        assert_eq!(rules[1], FwdRule::new('D', "1080", true));
        assert!(parse_rules("").is_empty());
    }

    #[test]
    fn putty_roundtrip() {
        let rules = parse_rules("L:9000:127.0.0.1:9000,R:2222:host:22,D:1080");
        let (putty, skipped) = to_putty_value(&rules);
        assert_eq!(putty, "L9000=127.0.0.1:9000,R2222=host:22,D1080=");
        assert!(skipped.is_empty());
        // 导出再导入 = 回到内部形
        assert_eq!(
            from_putty_value(&putty),
            "L:9000:127.0.0.1:9000,R:2222:host:22,D:1080"
        );
    }

    #[test]
    fn putty_cannot_express_disabled_or_listen_addr() {
        let rules = parse_rules("!L:9000:host:80,L:0.0.0.0:8080:host:80");
        let (putty, skipped) = to_putty_value(&rules);
        assert_eq!(putty, "", "两条都表达不了");
        assert_eq!(skipped.len(), 2, "都要报给用户，不能静默丢：{skipped:?}");
        assert!(skipped[0].contains("已停用"));
    }

    #[test]
    fn rule_display_ignores_runtime_state() {
        // 规则描述不含运行态：同一条规则启停前后描述一致，
        // 状态由 FwdView 单独呈现（免得两处各判一次）
        assert_eq!(
            parse_rules("L:9000:127.0.0.1:9000")[0].display(),
            parse_rules("!L:9000:127.0.0.1:9000")[0].display()
        );
        // 带类型前缀：本地/远端字段长得一样，不加前缀分不清
        assert_eq!(
            parse_rules("L:9000:127.0.0.1:9000")[0].display(),
            "本地 127.0.0.1:9000 → 127.0.0.1:9000"
        );
        assert_eq!(
            parse_rules("R:2222:host:22")[0].display(),
            "远程 127.0.0.1:2222 → host:22"
        );
        assert_eq!(parse_rules("D:1080")[0].display(), "SOCKS 127.0.0.1:1080");
    }

    #[test]
    fn kind_specific_parsers_reject_other_kinds() {
        let l = FwdRule::new('L', "9000:host:80", true);
        assert!(l.to_local().is_ok());
        assert!(l.to_remote().is_err());
        assert!(l.to_socks().is_err());
        let r = FwdRule::new('R', "2222:host:22", true);
        assert!(r.to_remote().is_ok());
        assert!(r.to_local().is_err());
        let d = FwdRule::new('D', "1080", true);
        assert!(d.to_socks().is_ok());
        assert!(d.to_local().is_err());
    }

    #[test]
    fn rfwd_table_lookup_by_port_fallback() {
        let t = RfwdTable::default();
        t.insert(RfwdBinding {
            bind_host: "127.0.0.1".into(),
            bind_port: 2222,
            target_host: "10.0.0.1".into(),
            target_port: 22,
        });
        assert_eq!(t.len(), 1);
        // 地址对得上
        assert_eq!(
            t.lookup("127.0.0.1", 2222),
            Some(("10.0.0.1".to_string(), 22))
        );
        // 服务器回报 0.0.0.0 → 按端口兜底
        assert_eq!(
            t.lookup("0.0.0.0", 2222),
            Some(("10.0.0.1".to_string(), 22))
        );
        // 端口对不上 → 没有
        assert_eq!(t.lookup("127.0.0.1", 2223), None);
        // 摘掉后就没了
        t.remove("127.0.0.1", 2222);
        assert_eq!(t.len(), 0);
        assert_eq!(t.lookup("127.0.0.1", 2222), None);
    }

    #[test]
    fn putty_cannot_express_socks_with_bind_addr() {
        // PuTTY 的 D 规则只有端口、没有地址字段
        let rules = parse_rules("D:127.0.0.1:1080");
        let (putty, skipped) = to_putty_value(&rules);
        assert_eq!(putty, "");
        assert_eq!(skipped.len(), 1, "得告诉用户这条导不出去：{skipped:?}");
    }
}
