//! 转发管理管线（worker 命令 + 事件）真机冒烟。
//! 运行：cargo run --example fwd_rules_smoke
// 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。
//!
//! 不经 GUI，直接驱动 worker 命令通道：Connect → ForwardAdd → 隧道验证 →
//! ForwardSetEnabled 停用/启用 → ForwardRemove，逐步断言 Forwards 快照的
//! key/enabled/active/err 与隧道实际可用性一致。

use std::sync::mpsc;
use std::time::Duration;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::panel::worker::{FwdView, PanelEvent, WorkerCmd};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

async fn next_forwards(
    rx: &async_channel::Receiver<PanelEvent>,
) -> (Vec<FwdView>, Vec<String>) {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("等 Forwards 事件超时")
            .expect("worker 事件通道断开");
        if let PanelEvent::Forwards { views, failed } = ev {
            return (views, failed);
        }
    }
}

/// 连隧道口读 SSH 横幅；timeout/连接失败返回 false。
async fn tunnel_banner(listen: &str) -> bool {
    let Ok(mut s) = TcpStream::connect(listen).await else {
        return false;
    };
    let mut buf = [0u8; 32];
    matches!(
        tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await,
        Ok(Ok(n)) if n > 0 && buf.starts_with(b"SSH-2.0-")
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::var("USER")?;
    let key = format!("/tmp/pp_selftest/{user}_ed25519");
    let (tx, rx_cmd) = mpsc::channel::<WorkerCmd>();
    let (ui_tx, ui_rx) = async_channel::unbounded::<PanelEvent>();
    let worker = puttyplus::panel::worker::spawn_worker(
        "127.0.0.1".into(),
        2222,
        user,
        None,
        Vec::new(),
        // 转发规则的来源（连接中枢按来源分组）
        "smoke".into(),
        rx_cmd,
        ui_tx,
        // 阶段 99：会话生死标志（UI 靠它判断"这次失败能不能自愈"）
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    );

    // 连接（用掉 Connected + 首个 Forwards 快照）
    tx.send(WorkerCmd::Connect {
        password: None,
        keyfile: Some(key),
        keepalive_sec: 30,
        auth_method: AuthMethod::default(),
        auth_order: AuthOrder::default(),
    })?;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(15), ui_rx.recv())
            .await?
            .unwrap();
        match ev {
            PanelEvent::Connected(Ok(_)) => println!("[1] connect ok"),
            PanelEvent::Connected(Err(e)) => panic!("connect failed: {e}"),
            PanelEvent::Forwards { views, .. } => {
                assert!(views.is_empty());
                println!("[2] initial forwards snapshot empty");
                break;
            }
            _ => {}
        }
    }

    // 添加 L:18099 → 127.0.0.1:2222
    tx.send(WorkerCmd::ForwardAdd {
        spec: "L:18099:127.0.0.1:2222".into(),
    })?;
    let (views, failed) = next_forwards(&ui_rx).await;
    assert!(failed.is_empty(), "add failed: {failed:?}");
    assert_eq!(views.len(), 1);
    let v = &views[0];
    assert_eq!(v.key, "L:18099:127.0.0.1:2222");
    assert!(v.enabled && v.active && v.err.is_none(), "{v:?}");
    println!("[3] added: {} ({})", v.text, v.state_text());
    assert!(tunnel_banner("127.0.0.1:18099").await, "隧道不通");
    println!("[4] tunnel works");

    // 停用 → 隧道应断
    tx.send(WorkerCmd::ForwardSetEnabled {
        key: "L:18099:127.0.0.1:2222".into(),
        enabled: false,
    })?;
    let (views, failed) = next_forwards(&ui_rx).await;
    assert!(failed.is_empty(), "disable failed: {failed:?}");
    assert_eq!(views[0].state_text(), "已停用");
    assert!(!tunnel_banner("127.0.0.1:18099").await, "停用后隧道仍通");
    println!("[5] disabled, tunnel closed");

    // 启用 → 隧道恢复
    tx.send(WorkerCmd::ForwardSetEnabled {
        key: "L:18099:127.0.0.1:2222".into(),
        enabled: true,
    })?;
    let (views, failed) = next_forwards(&ui_rx).await;
    assert!(failed.is_empty(), "enable failed: {failed:?}");
    assert_eq!(views[0].state_text(), "活动中");
    assert!(tunnel_banner("127.0.0.1:18099").await, "启用后隧道不通");
    println!("[6] re-enabled, tunnel works");

    // 删除 → 规则消失、隧道断
    tx.send(WorkerCmd::ForwardRemove {
        key: "L:18099:127.0.0.1:2222".into(),
    })?;
    let (views, failed) = next_forwards(&ui_rx).await;
    assert!(failed.is_empty() && views.is_empty(), "remove: {failed:?}");
    assert!(!tunnel_banner("127.0.0.1:18099").await);
    println!("[7] removed, list empty");

    // 重复添加同规格 → 拒重 + 冲突端口占用 → err 可见
    tx.send(WorkerCmd::ForwardAdd {
        spec: "L:18099:127.0.0.1:2222".into(),
    })?;
    next_forwards(&ui_rx).await;
    tx.send(WorkerCmd::ForwardAdd {
        spec: "L:18099:127.0.0.1:2222".into(),
    })?;
    let (views, failed) = next_forwards(&ui_rx).await;
    assert_eq!(views.len(), 1, "重复规则不应入列");
    assert!(failed.iter().any(|f| f.contains("已存在")), "{failed:?}");
    println!("[8] duplicate rejected: {}", failed[0]);
    eprintln!(".. sending remove");
    tx.send(WorkerCmd::ForwardRemove {
        key: "L:18099:127.0.0.1:2222".into(),
    })?;
    eprintln!(".. sent, waiting snapshot");
    let (views, failed) = next_forwards(&ui_rx).await;
    eprintln!(".. remove ack: views={} failed={failed:?}", views.len());

    tx.send(WorkerCmd::Shutdown)?;
    worker.join().unwrap();
    println!("[9] shutdown clean");
    Ok(())
}
