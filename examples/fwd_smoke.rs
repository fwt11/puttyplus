//! 进程内本地转发（-L）真机冒烟。
//! 运行：cargo run --example fwd_smoke
// 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。
//!
//! 做法：连上 sshd 后通过会话规则入口启动一条 127.0.0.1:0 → 127.0.0.1:2222
//! 的进程内转发，再从隧道口 TCP 连入——应读到 sshd 发来的 SSH 版本横幅，
//! 证明 本地监听 → direct-tcpip 通道 → 目标服务 全链路贯通。
//! 随后验证「停用保留规则、再启用」这条 1.4 的新语义。

use std::time::Duration;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::var("USER").unwrap();
    let key = format!("/tmp/pp_selftest/{user}_ed25519");
    let backend =
        conn::connect(
            "127.0.0.1",
            2222,
            &user,
            None,
            Some(&key),
            None,
            30,
            AuthMethod::default(),
            AuthOrder::default(),
        )
            .await?;
    println!("[1] connect ok");

    // 与 GUI 路径同一入口：会话规则字符串 → apply_forward_rules
    // 先探一个空闲端口（解析器拒绝 0 端口，不允许"内核分配"式写法）
    let probe = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = probe.local_addr()?.port();
    drop(probe);
    let spec = format!("L:{port}:127.0.0.1:2222");
    let rules = forward::parse_rules(&spec);
    assert_eq!(rules.len(), 1);
    let fails = backend.apply_forward_rules(rules).await;
    assert!(fails.is_empty(), "规则启动失败：{fails:?}");
    let snap = backend.forward_snapshot().await;
    let key = snap[0].key.clone();
    let listen = snap[0].listen.expect("规则应已监听");
    println!("[2] forward listening: {listen}  (key={key})");

    // 模拟外部客户端：连隧道口，等 sshd 横幅
    let banner = read_banner(listen).await?;
    println!("[3] banner through tunnel: {}", banner.trim());

    // 空闲 2 秒再读一次连接是否仍存活（转发任务不依赖 block_on 驱动）
    tokio::time::sleep(Duration::from_secs(2)).await;
    println!("[4] idle 2s ok");

    // 停用：规则保留（列表里还在），但不再监听
    backend.set_forward_rule_enabled(&key, false).await?;
    let snap = backend.forward_snapshot().await;
    assert!(!snap[0].enabled && !snap[0].active, "停用后应保留规则且不监听");
    // 停用后端口应已释放：原地址连不上
    assert!(
        TcpStream::connect(listen).await.is_err(),
        "停用后监听端口应已释放"
    );
    println!("[5] disabled: rule kept, port released");

    // 再启用：重新绑定同一端口，隧道恢复
    backend.set_forward_rule_enabled(&key, true).await?;
    let snap = backend.forward_snapshot().await;
    assert_eq!(snap[0].listen, Some(listen));
    let banner2 = read_banner(listen).await?;
    println!("[6] re-enabled, banner: {}", banner2.trim());

    backend.close().await;
    println!("[7] close ok");
    Ok(())
}

use puttyplus::backend::forward;

async fn read_banner(addr: std::net::SocketAddr) -> Result<String, Box<dyn std::error::Error>> {
    let mut sock = TcpStream::connect(addr).await?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await??;
    let banner = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(banner.starts_with("SSH-2.0-"), "隧道里应收到 SSH 横幅，实得: {banner:?}");
    Ok(banner)
}
