//! 冒烟：**连接中枢**——同一台服务器只开一条 SSH 连接。
//!
//! 这是本项目的核心承诺（用户 2026-11-11）：
//! 「我们默认情况下 sftp, 端口映射，数据库管理等等都走通道，
//! 　我们可以保证对每个服务器的访问只需要一个连接。」
//!
//! 验证四件事：
//! 1. 两次 `acquire` 同一台服务器 ⇒ 拿到**同一条**连接（指针相同）；
//! 2. 同一条连接上能开**多条**通道（两条 shell + 两条 SFTP，各自独立工作）；
//! 3. 转发规则**按来源分组**：后连的不清掉先连的；
//! 4. 放掉一个使用者 ⇒ 连接还在；全放掉 ⇒ 连接归零。
//!
//! 运行前提：本机测试 sshd 在 127.0.0.1:2233（公钥认证）。
//! 端口可用 `PP_HUB_PORT` 覆盖。

use std::sync::Arc;
use std::time::Duration;

use puttyplus::backend::conn;
use puttyplus::backend::forward::FwdRule;
use puttyplus::backend::hub;
use puttyplus::backend::shell::{self, ShellChannel};

const HOST: &str = "127.0.0.1";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ⚠️ **必须用全局运行时**：连接中枢在那儿（russh 的 Handle 绑 runtime）。
    //    这里跑在别的 runtime 上也算数——中枢自己会把建连搬到全局 runtime。
    hub::runtime().block_on(async move { run().await })
}

/// 找一个空闲的本地端口（转发规则要用；解析器不接受 0 端口）。
fn free_port() -> Result<u16, Box<dyn std::error::Error>> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    let p = l.local_addr()?.port();
    drop(l);
    Ok(p)
}

/// 在一条通道上把输出收回来。
async fn drain(chan: &ShellChannel) -> String {
    let mut out = String::new();
    while let Some(ev) = chan.next_event().await {
        match ev {
            shell::ShellEvent::Data(d) => out.push_str(&String::from_utf8_lossy(&d)),
            shell::ShellEvent::Exit(_) => break,
        }
    }
    out
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::var("USER")?;
    let port: i32 = std::env::var("PP_HUB_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2233);
    let home = std::env::var("HOME")?;
    let keyfile = format!("{home}/.ssh/id_rsa");

    // ---- [1] 两次 acquire ⇒ 同一条连接 -------------------------------
    let key = hub::ServerKey::new(HOST, port, &user);
    let creds = hub::Creds {
        keyfile: Some(keyfile),
        ..Default::default()
    };
    let a = hub::acquire(key.clone(), &creds).await?;
    let b = hub::acquire(key.clone(), &creds).await?;
    assert!(Arc::ptr_eq(&a, &b), "同一台服务器必须复用同一条连接");
    assert_eq!(hub::conn_count(), 1, "整个程序应当只有 1 条连接");
    assert_eq!(a.users(), 2, "这条连接应当有 2 个使用者");
    println!("[1] 同一条连接已复用（使用者={}）", a.users());

    // ---- [2] 同一条连接上开多条通道 -----------------------------------
    // 2a. 两条 shell 通道（终端走的就是这种）
    let c1 = ShellChannel::open_exec(a.session().as_ref(), "echo hub-one", 80, 24, false).await?;
    let c2 = ShellChannel::open_exec(a.session().as_ref(), "echo hub-two", 80, 24, false).await?;
    let o1 = drain(&c1).await;
    let o2 = drain(&c2).await;
    assert!(o1.contains("hub-one"), "通道一应当有自己的输出：{o1:?}");
    assert!(o2.contains("hub-two"), "通道二应当有自己的输出：{o2:?}");
    println!("[2a] 同一条连接上两条 shell 通道都可用（{o1:?} / {o2:?}）");

    // 2b. 两条 SFTP 通道（文件面板走的就是这种）
    let s1 = conn::open_sftp_session(a.session().as_ref()).await?;
    let s2 = conn::open_sftp_session(a.session().as_ref()).await?;
    let n1 = s1.read_dir("/").await?.count();
    let n2 = s2.read_dir("/").await?.count();
    assert!(n1 > 0 && n2 > 0, "两条 SFTP 通道都应当能列目录（{n1} / {n2}）");
    println!("[2b] 同一条连接上两条 SFTP 通道都可用（{n1} / {n2} 项）");

    // ---- [3] 转发规则按来源分组 ---------------------------------------
    let p1 = free_port()?;
    let p2 = free_port()?;
    let r1 = vec![FwdRule::new('L', &format!("{p1}:127.0.0.1:22"), true)];
    let r2 = vec![FwdRule::new('L', &format!("{p2}:127.0.0.1:22"), true)];
    let fails = a.apply_forward_rules("来源甲", r1.clone()).await;
    assert!(fails.is_empty(), "来源甲的规则应当启动成功：{fails:?}");
    let fails = a.apply_forward_rules("来源乙", r2.clone()).await;
    assert!(fails.is_empty(), "来源乙的规则应当启动成功：{fails:?}");
    let snap = a.forward_snapshot().await;
    assert_eq!(
        snap.len(),
        2,
        "两个来源的规则都应当在（后连的不该清掉先连的）：{snap:?}"
    );
    // 改来源甲（同一个监听端口换一个目标）⇒ 来源乙的还在
    let r1b = vec![FwdRule::new('L', &format!("{p1}:127.0.0.1:23"), true)];
    let fails = a.apply_forward_rules("来源甲", r1b).await;
    assert!(fails.is_empty(), "改来源甲不该失败：{fails:?}");
    let snap = a.forward_snapshot().await;
    assert_eq!(snap.len(), 2, "改一个来源不该动另一个来源：{snap:?}");
    println!("[3] 转发规则按来源分组 OK");

    // ---- [4] 引用计数：最后一个使用者放手才断 -------------------------
    drop(b);
    assert_eq!(hub::conn_count(), 1, "还有一个使用者 ⇒ 连接不该消失");
    assert_eq!(a.users(), 1);
    assert!(!a.is_closed(), "还有一个使用者 ⇒ 连接不该断");
    drop(a);
    // Drop 里的收尾（停转发 + 断开）是异步的 ⇒ 等一下
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub::conn_count(), 0, "最后一个使用者放手 ⇒ 连接应当归零");
    println!("[4] 引用计数收尾 OK");

    println!("hub_share_smoke: 全部通过");
    Ok(())
}
