//! 服务器到服务器传输：优先在源服务器上执行 scp（A→B 直连），
//! 失败则退回本机中转（读 A、写 B，不依赖服务器上的 scp/ssh）。
//!
//! 两种方式都只由源标签的 worker 执行；目标侧通过一条**导出的 SFTP
//! 会话**（`ExportSftp`）访问，避免再做一次认证。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::worker::{PanelEvent, XferShared};
use crate::backend::conn::SftpBackend;
use crate::util::{path_basename, path_join};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMode {
    Auto,
    DirectOnly,
    RelayOnly,
}

/// 一次跨服务器传输的全部参数（由 UI 构造，交给源 worker）。
pub struct RelaySpec {
    pub id: u64,
    /// 源端绝对路径（文件或目录）
    pub paths: Vec<String>,
    /// 目标目录（绝对路径）
    pub dst_dir: String,
    /// 目标侧导出的 SFTP 会话
    pub dst_sftp: SftpSession,
    pub dst_user: String,
    pub dst_host: String,
    pub dst_port: i32,
    pub dst_label: String,
    pub mode: RelayMode,
    pub allow_agent: bool,
    /// 目标同名文件：true = 覆盖，false = 跳过
    pub overwrite: bool,
    pub shared: Arc<Mutex<XferShared>>,
    pub cancel: Arc<AtomicBool>,
    /// 每次传输唯一（临时目录/半成品文件名用），避免并发撞名
    pub tag: String,
}

impl RelaySpec {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
            || self
                .shared
                .lock()
                .map(|g| g.cancel)
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
struct Entry {
    /// 相对目标目录的路径
    rel: String,
    is_dir: bool,
    size: u64,
}

fn fail(msg: impl Into<String>) -> String {
    msg.into()
}

fn update(shared: &Arc<Mutex<XferShared>>, done: u64, total: u64, cur: &str) {
    if let Ok(mut g) = shared.lock() {
        g.done = done;
        g.total = total;
        g.cur = cur.to_string();
        if let Some(start) = g.started {
            let secs = start.elapsed().as_secs_f64().max(0.001);
            g.speed = done as f64 / secs;
        }
    }
}

fn set_msg(shared: &Arc<Mutex<XferShared>>, msg: &str) {
    if let Ok(mut g) = shared.lock() {
        g.msg = Some(msg.to_string());
    }
}

/// 建目录（等价 mkdir -p，已存在则忽略）。
async fn ensure_dir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err(fail(format!("目标目录必须是绝对路径：{path}")));
    }
    let mut cur = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        cur.push('/');
        cur.push_str(seg);
        if sftp.metadata(&cur).await.map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        match sftp.create_dir(&cur).await {
            Ok(()) => {}
            Err(_) => {
                // 并发/已存在：再看一次
                if !sftp.metadata(&cur).await.map(|m| m.is_dir()).unwrap_or(false) {
                    return Err(fail(format!("创建目录失败：{cur}")));
                }
            }
        }
    }
    Ok(())
}

fn remove_rec<'a>(
    sftp: &'a SftpSession,
    path: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let is_dir = sftp
            .metadata(&path)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if !is_dir {
            return sftp.remove_file(&path).await.map_err(|e| e.to_string());
        }
        let entries: Vec<String> = match sftp.read_dir(&path).await {
            Ok(rd) => rd.map(|e| e.file_name()).collect(),
            Err(_) => Vec::new(),
        };
        for name in entries {
            remove_rec(sftp, path_join(&path, &name)).await?;
        }
        sftp.remove_dir(&path).await.map_err(|e| e.to_string())
    })
}

/// 递归扫描源端，产出（相对路径、是否目录、大小）。
async fn scan(sftp: &SftpSession, paths: &[String]) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for top in paths {
        let base = path_basename(top).to_string();
        let meta = sftp.metadata(top).await.map_err(|e| e.to_string())?;
        if meta.is_dir() {
            out.push(Entry {
                rel: base.clone(),
                is_dir: true,
                size: 0,
            });
            let mut stack = vec![(top.clone(), base.clone())];
            while let Some((dir, rel)) = stack.pop() {
                let rd = sftp.read_dir(&dir).await.map_err(|e| e.to_string())?;
                for e in rd {
                    let name = e.file_name();
                    let child_rel = path_join(&rel, &name);
                    let child = path_join(&dir, &name);
                    let m = e.metadata();
                    if m.is_dir() {
                        out.push(Entry {
                            rel: child_rel.clone(),
                            is_dir: true,
                            size: 0,
                        });
                        stack.push((child, child_rel));
                    } else {
                        out.push(Entry {
                            rel: child_rel,
                            is_dir: false,
                            size: m.size.unwrap_or(0),
                        });
                    }
                }
            }
        } else {
            out.push(Entry {
                rel: base,
                is_dir: false,
                size: meta.size.unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// 目标端临时目录当前占用的字节数（直连进度用）。
async fn tree_size(sftp: &SftpSession, path: &str) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = sftp.read_dir(path).await else {
        return 0;
    };
    let mut stack: Vec<String> = rd.map(|e| path_join(path, &e.file_name())).collect();
    while let Some(p) = stack.pop() {
        match sftp.metadata(&p).await {
            Ok(m) if m.is_dir() => {
                if let Ok(rd) = sftp.read_dir(&p).await {
                    stack.extend(rd.map(|e| path_join(&p, &e.file_name())));
                }
            }
            Ok(m) => total = total.saturating_add(m.size.unwrap_or(0)),
            Err(_) => {}
        }
    }
    total
}

/// 单个路径是否适合直接交给源端 shell/scp。
fn quote_shell(s: &str) -> Result<String, String> {
    if s.contains(['\n', '\r', '\'']) || s.bytes().any(|b| b == 0) {
        return Err("路径包含换行或单引号，改用本机中转".into());
    }
    if s.chars().any(|c| matches!(c, '*' | '?' | '[' | ']')) {
        return Err("路径包含通配符，改用本机中转".into());
    }
    Ok(format!("'{s}'"))
}

fn scp_target(user: &str, host: &str, path: &str) -> Result<String, String> {
    if user.contains([' ', '\t']) || host.contains([' ', '\t']) {
        return Err("用户名/主机名包含空白".into());
    }
    Ok(format!("{user}@{host}:{path}"))
}

pub async fn run(
    conn: Option<&SftpBackend>,
    spec: RelaySpec,
    ui_tx: &async_channel::Sender<PanelEvent>,
) -> Result<(), String> {
    let conn = conn.ok_or_else(|| "源连接已断开".to_string())?;
    if spec.dst_dir.trim().is_empty() {
        return Err("目标目录为空".into());
    }
    ensure_dir(&spec.dst_sftp, &spec.dst_dir).await?;

    let entries = scan(conn.sftp(), &spec.paths).await?;
    let total: u64 = entries.iter().map(|e| e.size).sum();
    update(&spec.shared, 0, total, "");

    if spec.mode != RelayMode::RelayOnly {
        match direct(conn, &spec, total, ui_tx).await {
            Ok(()) => return Ok(()),
            Err(why) => {
                if spec.is_cancelled() {
                    return Err("已取消".into());
                }
                if spec.mode == RelayMode::DirectOnly {
                    return Err(why);
                }
                relay(conn.sftp(), &spec, &entries, total, Some(&why), ui_tx).await?
            }
        }
        return Ok(());
    }
    relay(conn.sftp(), &spec, &entries, total, None, ui_tx).await
}

async fn direct(
    conn: &SftpBackend,
    spec: &RelaySpec,
    total: u64,
    ui_tx: &async_channel::Sender<PanelEvent>,
) -> Result<(), String> {
    if spec.dst_port <= 0 {
        return Err("目标端口无效".into());
    }
    // 提前校验目标串，别让带空白的 user/host 进 shell
    let tmp_name = format!(".puttyplus-relay-{}", spec.tag);
    let tmp = path_join(&spec.dst_dir, &tmp_name);
    let _ = scp_target(&spec.dst_user, &spec.dst_host, &format!("{tmp}/"))?;

    // 预检（此时还没碰目标目录）：A 上有 scp/ssh，且能以 BatchMode 登录 B
    let pre = format!(
        "command -v scp >/dev/null 2>&1 && command -v ssh >/dev/null 2>&1 && \
         ssh -o BatchMode=yes -o ConnectTimeout=5 -p {port} {user}@{host} true",
        port = spec.dst_port,
        user = spec.dst_user,
        host = spec.dst_host,
    );
    let pre_out = conn
        .exec_full(&pre, spec.allow_agent, spec.cancel.clone(), 8_000)
        .await
        .map_err(|e| e.0)?;
    if pre_out.code != Some(0) {
        let why = pre_out.stderr.trim();
        return Err(if why.is_empty() {
            format!("预检失败（退出码 {:?}）", pre_out.code)
        } else {
            format!("预检失败：{why}")
        });
    }

    // 构造 scp：源端文件/目录 → B 的临时目录
    let mut cmd = String::from("scp -r -p -o BatchMode=yes -o ConnectTimeout=10 ");
    cmd.push_str(&format!("-P {} ", spec.dst_port));
    for p in &spec.paths {
        cmd.push_str(&quote_shell(p)?);
        cmd.push(' ');
    }
    let target = scp_target(&spec.dst_user, &spec.dst_host, &format!("{tmp}/"))?;
    cmd.push_str(&quote_shell(&target)?);

    // 目标临时目录：直连失败时清掉它即可安全回退
    ensure_dir(&spec.dst_sftp, &tmp).await?;
    set_msg(
        &spec.shared,
        &format!("直连中（源服务器 → {}）", spec.dst_label),
    );

    let fut = conn.exec_full(&cmd, spec.allow_agent, spec.cancel.clone(), 24 * 3600 * 1000);
    tokio::pin!(fut);
    let mut last_poll = Instant::now();
    let out = loop {
        tokio::select! {
            r = &mut fut => break r,
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                if spec.is_cancelled() {
                    set_msg(&spec.shared, "已取消");
                }
                if last_poll.elapsed() >= std::time::Duration::from_millis(800) {
                    last_poll = Instant::now();
                    let used = tree_size(&spec.dst_sftp, &tmp).await;
                    update(&spec.shared, used.min(total), total, "");
                    let _ = ui_tx.try_send(PanelEvent::XferTick);
                }
            }
        }
    };

    let out = match out {
        Ok(o) => o,
        Err(e) => {
            let _ = remove_rec(&spec.dst_sftp, tmp.clone()).await;
            return Err(e.0);
        }
    };
    if out.code != Some(0) {
        let why = out.stderr.trim().to_string();
        let _ = remove_rec(&spec.dst_sftp, tmp.clone()).await;
        return Err(if why.is_empty() {
            format!("scp 失败（退出码 {:?}）", out.code)
        } else {
            format!("scp 失败：{why}")
        });
    }

    // 从临时目录搬到目标目录（同名按策略处理）
    let rd = spec
        .dst_sftp
        .read_dir(&tmp)
        .await
        .map_err(|e| format!("读取临时目录失败：{e}"))?;
    let names: Vec<String> = rd.map(|e| e.file_name()).collect();
    let mut touched = 0u64;
    for name in names {
        let from = path_join(&tmp, &name);
        let to = path_join(&spec.dst_dir, &name);
        if spec.dst_sftp.metadata(&to).await.is_ok() {
            if !spec.overwrite {
                set_msg(&spec.shared, &format!("已跳过（目标已存在）：{name}"));
                continue;
            }
            remove_rec(&spec.dst_sftp, to.clone()).await?;
        }
        if let Err(e) = spec.dst_sftp.rename(&from, &to).await {
            let _ = remove_rec(&spec.dst_sftp, tmp.clone()).await;
            return Err(format!("移动到目标目录失败（{name}）：{e}"));
        }
        touched += 1;
    }
    let _ = remove_rec(&spec.dst_sftp, tmp.clone()).await;
    update(&spec.shared, total, total, "");
    set_msg(&spec.shared, &format!("直连完成（{} 项）", touched));
    Ok(())
}

async fn relay(
    src: &SftpSession,
    spec: &RelaySpec,
    entries: &[Entry],
    total: u64,
    fallback_reason: Option<&str>,
    ui_tx: &async_channel::Sender<PanelEvent>,
) -> Result<(), String> {
    let msg = match fallback_reason {
        Some(why) => format!(
            "本机中转中（源 → 本机 → {}；直连失败：{why}）",
            spec.dst_label
        ),
        None => format!("本机中转中（源 → 本机 → {}）", spec.dst_label),
    };
    set_msg(&spec.shared, &msg);
    let mut done = 0u64;
    let mut last_tick = Instant::now();
    let mut buf = vec![0u8; 256 * 1024];

    for e in entries {
        if spec.is_cancelled() {
            return Err("已取消".into());
        }
        let dst = path_join(&spec.dst_dir, &e.rel);
        if e.is_dir {
            ensure_dir(&spec.dst_sftp, &dst).await?;
            continue;
        }
        if spec.dst_sftp.metadata(&dst).await.is_ok() {
            if !spec.overwrite {
                set_msg(&spec.shared, &format!("已跳过（目标已存在）：{}", e.rel));
                continue;
            }
            remove_rec(&spec.dst_sftp, dst.clone()).await?;
        }
        if let Some(parent) = dst.rsplit_once('/').map(|(p, _)| p) {
            if !parent.is_empty() {
                ensure_dir(&spec.dst_sftp, parent).await?;
            }
        }
        let part = format!("{dst}.puttyplus-{}.part", spec.tag);
        let mut sf = src
            .open(&source_path(&spec.paths, &e.rel))
            .await
            .map_err(|err| format!("打开源文件失败（{}）：{err}", e.rel))?;
        let mut df = spec
            .dst_sftp
            .create(&part)
            .await
            .map_err(|err| format!("创建目标文件失败（{}）：{err}", e.rel))?;
        loop {
            if spec.is_cancelled() {
                let _ = spec.dst_sftp.remove_file(&part).await;
                return Err("已取消".into());
            }
            let n = match sf.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    let _ = spec.dst_sftp.remove_file(&part).await;
                    return Err(format!("读取源文件失败（{}）：{err}", e.rel));
                }
            };
            if let Err(err) = df.write_all(&buf[..n]).await {
                let _ = spec.dst_sftp.remove_file(&part).await;
                return Err(format!("写入目标文件失败（{}）：{err}", e.rel));
            }
            done = done.saturating_add(n as u64);
            update(&spec.shared, done, total, &e.rel);
            if last_tick.elapsed() >= std::time::Duration::from_millis(250) {
                last_tick = Instant::now();
                let _ = ui_tx.try_send(PanelEvent::XferTick);
            }
        }
        let _ = df.flush().await;
        drop(df);
        drop(sf);
        spec.dst_sftp
            .rename(&part, &dst)
            .await
            .map_err(|err| format!("重命名目标文件失败（{}）：{err}", e.rel))?;
    }
    update(&spec.shared, total, total, "");
    set_msg(&spec.shared, "本机中转完成");
    Ok(())
}

/// 由相对路径拼回源端绝对路径：entries 的 rel 已带顶层 basename。
fn source_path(roots: &[String], rel: &str) -> String {
    for root in roots {
        let base = path_basename(root);
        if rel == base {
            return root.clone();
        }
        if let Some(rest) = rel.strip_prefix(&format!("{base}/")) {
            return path_join(root, rest);
        }
    }
    rel.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 安全路径走单引号() {
        assert_eq!(quote_shell("/tmp/a b.txt").unwrap(), "'/tmp/a b.txt'");
        assert_eq!(quote_shell("/tmp/$x`y`").unwrap(), "'/tmp/$x`y`'");
    }

    #[test]
    fn 危险路径拒绝直连() {
        assert!(quote_shell("/tmp/a'b").is_err());
        assert!(quote_shell("/tmp/a*b").is_err());
        assert!(quote_shell("/tmp/a?b").is_err());
        assert!(quote_shell("/tmp/a\nb").is_err());
    }

    #[test]
    fn 目标串校验() {
        assert_eq!(
            scp_target("u", "h", "/tmp/").unwrap(),
            "u@h:/tmp/"
        );
        assert!(scp_target("u u", "h", "/tmp/").is_err());
    }

    #[test]
    fn 相对路径还原到源根() {
        let roots = vec!["/home/u/dir".to_string(), "/home/u/a.txt".to_string()];
        assert_eq!(
            source_path(&roots, "dir/sub/x"),
            "/home/u/dir/sub/x"
        );
        assert_eq!(source_path(&roots, "a.txt"), "/home/u/a.txt");
    }
}
