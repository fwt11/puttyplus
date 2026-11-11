//! 传输调度器冒烟：多批次追加、队列清空只汇总一次、并发数生效。
//!
//! 直接驱动 worker（不启动 GUI），对真实用户态 sshd 跑：
//!   1. 连续提交两批上传（第二批在第一批还没传完时提交）；
//!   2. 只收到 **一次** `XferDrained`，结果包含两批全部文件；
//!   3. `[xfer] parallel` 设为 4 时，观察到的同时在传文件数 > 1；
//!   4. 远端目录里的文件数正确，清理后退出。
//!
//! 运行：
//!   cargo run --example parallel_xfer_smoke -- <host> <port> <user> <keyfile> [远端可写目录]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::panel::worker::{
    PanelEvent, SimpleOp, WorkerCmd, XferConflict, XferKind, XferShared, XferSpec, XferState,
    spawn_worker,
};
use puttyplus::util::path_join;

const N: usize = 32;
const FILE_SIZE: usize = 1024 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pos = std::env::args().skip(1);
    let host = pos.next().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = pos.next().unwrap_or_else(|| "2222".into()).parse()?;
    let user = pos.next().ok_or("需要 user")?;
    let key = pos.next().ok_or("需要 keyfile")?;
    let remote_root = pos.next();

    // 隔离配置：不要动用户真实的 settings.ini
    let cfg = std::env::temp_dir().join(format!("pp_parallel_smoke_{}", std::process::id()));
    let ldir = cfg.join("local");
    std::fs::create_dir_all(&ldir)?;
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &cfg) };
    puttyplus::settings::set_xfer_parallel(4);
    puttyplus::settings::set_xfer_resume(false);
    puttyplus::settings::set_xfer_checksum(false);

    let (cmd_tx, cmd_rx) = channel();
    let (ui_tx, ui_rx) = async_channel::unbounded();
    let worker = spawn_worker(
        host,
        i32::from(port),
        user,
        None,
        Vec::new(),
        // 转发规则的来源（连接中枢按来源分组）
        "smoke".into(),
        cmd_rx,
        ui_tx,
        // 阶段 99：会话生死标志（UI 靠它判断"这次失败能不能自愈"）
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
    );

    cmd_tx.send(WorkerCmd::Connect {
        password: None,
        keyfile: Some(key),
        keepalive_sec: 0,
        auth_method: AuthMethod::default(),
        auth_order: AuthOrder::PasswordFirst,
    })?;
    let home = loop {
        match ui_rx.recv().await? {
            PanelEvent::Connected(Ok(home)) => break home,
            PanelEvent::Connected(Err(e)) => return Err(format!("连接失败: {e}").into()),
            _ => {}
        }
    };
    let remote_dir = remote_root
        .clone()
        .unwrap_or_else(|| path_join(&home, &format!("pp_parallel_smoke_{}", std::process::id())));
    if !remote_dir.is_empty() {
        cmd_tx.send(WorkerCmd::Simple {
            op: SimpleOp::Mkdir,
            path: remote_dir.clone(),
            new_path: None,
        })?;
        loop {
            match ui_rx.recv().await? {
                PanelEvent::SimpleDone { result, .. } => {
                    // 已存在不算错（重复跑冒烟）
                    let _ = result;
                    break;
                }
                _ => {}
            }
        }
    }

    // 准备本地文件与传输项
    let mut specs = Vec::new();
    let mut shares = Vec::new();
    for i in 0..N {
        let name = format!("f{i:02}.bin");
        let path = ldir.join(&name);
        let data = vec![(i as u8).wrapping_mul(31); FILE_SIZE];
        std::fs::write(&path, &data)?;
        let shared = XferShared::new();
        shares.push(shared.clone());
        specs.push(XferSpec {
            id: i as u64 + 1,
            src: path.to_string_lossy().into_owned(),
            dst_dir: remote_dir.clone(),
            upload: true,
            kind: XferKind::Transfer,
            conflict: XferConflict::Overwrite,
            shared,
        });
    }

    // 两批：先交前 8 个，趁它们还在跑时再交后 24 个
    let split = 8;
    let batch_a = specs.split_off(split);
    // 同一目标重复提交：必须排队串行，不能两个任务同时写同一个 .pp-part
    let dup_src = specs[0].src.clone();
    let dup_dst = specs[0].dst_dir.clone();
    specs.push(XferSpec {
        id: 10_000,
        src: dup_src,
        dst_dir: dup_dst,
        upload: true,
        kind: XferKind::Transfer,
        conflict: XferConflict::Overwrite,
        shared: XferShared::new(),
    });
    let total_jobs = specs.len() + batch_a.len();
    cmd_tx.send(WorkerCmd::Transfers { items: batch_a })?;
    tokio::time::sleep(Duration::from_millis(30)).await;
    cmd_tx.send(WorkerCmd::Transfers { items: specs })?;

    let max_active = AtomicUsize::new(0);
    let mut drained: Option<Vec<(String, XferState)>> = None;
    let mut drain_count = 0usize;
    loop {
        tokio::select! {
            ev = ui_rx.recv() => {
                match ev? {
                    PanelEvent::XferDrained { results } => {
                        drain_count += 1;
                        drained = Some(results);
                        break;
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(5)) => {
                let n = shares
                    .iter()
                    .filter(|s| s.lock().unwrap().state == XferState::Run)
                    .count();
                max_active.fetch_max(n, Ordering::Relaxed);
            }
        }
    }

    // 再等一小会儿，确认没有第二个 drain（追加批次不应各自汇总）
    tokio::time::sleep(Duration::from_millis(150)).await;
    while let Ok(ev) = ui_rx.try_recv() {
        if matches!(ev, PanelEvent::XferDrained { .. }) {
            drain_count += 1;
        }
    }

    let results = drained.expect("没有收到 XferDrained");
    assert_eq!(drain_count, 1, "队列清空应只汇总一次");
    assert_eq!(results.len(), total_jobs, "汇总结果没有覆盖两批全部文件");
    assert!(
        results.iter().all(|(_, st)| *st == XferState::Done),
        "有传输没完成: {results:?}"
    );
    let max = max_active.load(Ordering::Relaxed);
    assert!(max > 1, "并发没有生效：观察到的最大并发只有 {max}");
    println!(
        "[1] 两批追加（含一个重复目标）→ 只 drain 一次，{total_jobs} 项完成（最大并发 {max}）"
    );

    // 远端文件数
    cmd_tx.send(WorkerCmd::List {
        id: 1,
        path: remote_dir.clone(),
        silent: false,
    })?;
    let entries = loop {
        match ui_rx.recv().await? {
            PanelEvent::Listed { result, .. } => {
                break result.map_err(|e| format!("列目录失败: {e}"))?;
            }
            _ => {}
        }
    };
    assert_eq!(entries.len(), N, "远端文件数不对: {}", entries.len());
    assert!(
        entries.iter().all(|e| e.size == FILE_SIZE as u64),
        "有文件大小不对（并发写入被截断？）"
    );
    println!("[2] 远端 {} 个文件齐全且大小正确", entries.len());

    cmd_tx.send(WorkerCmd::Simple {
        op: SimpleOp::Remove,
        path: remote_dir.clone(),
        new_path: None,
    })?;
    loop {
        match ui_rx.recv().await? {
            PanelEvent::SimpleDone { result, .. } => {
                result.map_err(|e| format!("清理失败: {e}"))?;
                break;
            }
            _ => {}
        }
    }
    cmd_tx.send(WorkerCmd::Shutdown)?;
    drop(cmd_tx); // 让 worker 的阻塞 recv 返回，否则它退不出去
    let _ = worker.join();
    let _ = std::fs::remove_dir_all(&cfg);
    println!("parallel xfer smoke ok");
    Ok(())
}
