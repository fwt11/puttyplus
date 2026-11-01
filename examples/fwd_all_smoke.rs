//! 进程内三种端口映射（-L / -R / -D）真机冒烟。
//!
//! 运行：`cargo run --example fwd_all_smoke`
//! 运行前提：本机有一个用户态 sshd（默认 127.0.0.1:2233，公钥认证）。
//! 环境变量：PP_FWD_HOST / PP_FWD_PORT / PP_FWD_USER / PP_FWD_KEY
//!
//! 三条链路各走一遍到底，证明它们都跑在应用自己那条 SSH 连接上：
//! 1. 本地（-L）：本机监听 → direct-tcpip → 服务器侧目标 → 读到 SSH 横幅；
//! 2. 动态（-D）：本机 SOCKS5 → 握手 → direct-tcpip → 读到 SSH 横幅；
//! 3. 远程（-R）：**服务器**监听 → forwarded-tcpip 通道送回来 →
//!    **本机**去连目标 → 读到 SSH 横幅。
//!
//! ⚠️ -R 的方向容易搞反：监听端在远端、目标在客户端侧解析
//! （SSH 协议的 tcpip-forward 请求只带绑定地址/端口，不带目标）。

use std::time::Duration;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use puttyplus::backend::forward;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 找一个空闲端口（内核给一个，释放后拿端口号用）。
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// 读 SSH 横幅（隧道另一头是 sshd）。
async fn read_banner(sock: &mut TcpStream) -> Result<String, Box<dyn std::error::Error>> {
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await??;
    let banner = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        banner.starts_with("SSH-2.0-"),
        "隧道里应收到 SSH 横幅，实得: {banner:?}"
    );
    Ok(banner)
}

/// SOCKS5 客户端（只做 CONNECT + 域名地址），用来验 -D。
async fn socks5_connect(
    addr: std::net::SocketAddr,
    target: &str,
    port: u16,
) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mut s = TcpStream::connect(addr).await?;
    // 握手：VER=5, 1 种方法, 无认证
    s.write_all(&[5, 1, 0]).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await?;
    assert_eq!(r[0], 5, "SOCKS 版本不对");
    assert_eq!(r[1], 0, "服务器应选「无认证」");
    // 请求：VER=5, CMD=1(CONNECT), RSV=0, ATYP=3(域名)
    let hb = target.as_bytes();
    let mut req = vec![5, 1, 0, 3, hb.len() as u8];
    req.extend_from_slice(hb);
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;
    let mut resp = [0u8; 10];
    s.read_exact(&mut resp).await?;
    assert_eq!(resp[0], 5, "SOCKS 应答版本不对");
    assert_eq!(resp[1], 0, "CONNECT 应成功（应答第二字节非 0）");
    Ok(s)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = env_or("PP_FWD_HOST", "127.0.0.1");
    let port: i32 = env_or("PP_FWD_PORT", "2233").parse()?;
    let user = env_or("PP_FWD_USER", &std::env::var("USER").unwrap_or_default());
    let key = env_or("PP_FWD_KEY", &format!("{}/.ssh/id_rsa", env_or("HOME", "/root")));

    let backend = conn::connect(
        &host,
        port,
        &user,
        None,
        Some(&key),
        None,
        30,
        AuthMethod::default(),
        AuthOrder::default(),
    )
    .await?;
    println!("[1] connect ok: {user}@{host}:{port}");

    // ---- 本地（-L）----
    let lport = free_port();
    let spec = format!("L:{lport}:127.0.0.1:{port}");
    let rules = forward::parse_rules(&spec);
    assert_eq!(rules.len(), 1, "规则应解析成功：{spec}");
    let fails = backend.apply_forward_rules(rules).await;
    assert!(fails.is_empty(), "规则启动失败：{fails:?}");
    let snap = backend.forward_snapshot().await;
    let listen = snap[0].listen.expect("L 规则应已监听");
    let mut sock = TcpStream::connect(listen).await?;
    let banner = read_banner(&mut sock).await?;
    println!("[2] -L 本地转发通：{} → {}", listen, banner.trim());

    // ---- 动态（-D）：本地 SOCKS5 ----
    let dport = free_port();
    let spec = format!("D:{dport}");
    let rules = forward::parse_rules(&spec);
    assert_eq!(rules.len(), 1, "SOCKS 规则应解析成功：{spec}");
    let fails = backend.apply_forward_rules(rules).await;
    assert!(fails.is_empty(), "SOCKS 规则启动失败：{fails:?}");
    let snap = backend.forward_snapshot().await;
    let dlisten = snap
        .iter()
        .find(|s| s.key.starts_with("D:"))
        .and_then(|s| s.listen)
        .expect("D 规则应已监听");
    let mut sock = socks5_connect(dlisten, "127.0.0.1", port as u16).await?;
    let banner = read_banner(&mut sock).await?;
    println!("[3] -D SOCKS5 通：{} → {}", dlisten, banner.trim());

    // ---- 远程（-R）：服务器监听 ----
    let rport = free_port();
    let spec = format!("R:{rport}:127.0.0.1:{port}");
    let rules = forward::parse_rules(&spec);
    assert_eq!(rules.len(), 1, "远程规则应解析成功：{spec}");
    let fails = backend.apply_forward_rules(rules).await;
    assert!(fails.is_empty(), "远程规则启动失败：{fails:?}");
    let snap = backend.forward_snapshot().await;
    let r = snap.iter().find(|s| s.key.starts_with("R:")).expect("R 规则在列");
    assert!(r.active, "R 规则应在跑：{r:?}");
    // R 的监听在**服务器**上：连服务器的那个端口
    let mut sock = TcpStream::connect(("127.0.0.1", rport)).await?;
    let banner = read_banner(&mut sock).await?;
    println!("[4] -R 远程转发通：服务器 127.0.0.1:{rport} → {}", banner.trim());

    // 停用 R → 服务器应收回监听（再连应失败）
    let key = r.key.clone();
    backend.set_forward_rule_enabled(&key, false).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        TcpStream::connect(("127.0.0.1", rport)).await.is_err(),
        "停用后服务器侧监听应已收回"
    );
    println!("[5] -R 停用后服务器监听已收回");

    backend.close().await;
    println!("[6] close ok");
    Ok(())
}
