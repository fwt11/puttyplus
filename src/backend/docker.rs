//! 远程服务器上的**容器（Docker）运行情况**（阶段 109）。
//!
//! ## 干什么用
//!
//! 用户 2026-11-15：「我们要在软件里面显示**远程服务器上的容器运行情况**」
//! ⇒ 在连接页里开一个「容器」子标签，看这台服务器上跑了哪些容器、镜像是什么、
//! 端口映射到哪、占了多少 CPU/内存。
//!
//! ## 怎么拿数据（**不新增 SSH 连接**）
//!
//! ⚠️ **硬约束：一台服务器只有一条 SSH 连接**（连接中枢 `hub.rs`）⇒ 容器
//! 视图**不能自己开连接**。做法：在**共享连接**上开一条 **exec 通道**
//! （`backend/shell.rs` 的 `ShellChannel::open_exec`，终端/分屏已在用）⇒
//! 每条 docker 命令一条通道（通道很轻，**不是新连接**）。
//!
//! ## 为什么不解析 JSON
//!
//! ⚠️ 仓库里**没有 serde_json**，也不打算为一个页面引入新依赖 ⇒
//! **用 `docker --format` 的 Go 模板**直接把要的字段吐成一行一行，
//! **按分隔符切列**即可。
//!
//! ⚠️ **分隔符用 `|`**：容器名/镜像/状态/端口/compose 项目名里**都不会出现
//! `|`**（docker 的名字只允许 `[a-zA-Z0-9_.-]`，镜像是 `[a-z0-9._/-]`）。
//! 用制表符不安全（状态里可能有空格，用 `\x1f` 又怕经过 shell 时被吃掉）。

use std::collections::HashMap;

use russh::client::Handle;

use crate::backend::conn::HostKeyHandler;
use crate::backend::shell::{ShellChannel, ShellEvent};
use crate::backend::{BackendError, BackendResult};
use crate::i18n::tr;

/// 字段分隔符（见模块注释：为什么不用制表符）。
pub const SEP: char = '|';

/// 容器列表的字段模板（**完整版**）。
///
/// ⚠️ `{{.Label "com.docker.compose.project"}}` 要 **Docker 17.06+**；
/// 老版本会报「template parsing error」⇒ 有个**降级模板**（见
/// [`list_cmd_simple`]）。
const PS_FIELDS: &str = "{{.ID}}|{{.Names}}|{{.Image}}|{{.Status}}|{{.Ports}}|{{.Label \"com.docker.compose.project\"}}";

/// 容器列表的字段模板（**降级版**：不依赖 `.Label`，老 Docker 也能用）。
const PS_FIELDS_SIMPLE: &str = "{{.ID}}|{{.Names}}|{{.Image}}|{{.Status}}|{{.Ports}}";

/// 一个容器。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerInfo {
    /// 容器 ID（前 12 位就够，列表里显示）。
    pub id: String,
    /// 容器名。
    pub name: String,
    /// 镜像。
    pub image: String,
    /// 状态原文（如 `Up 2 hours (healthy)` / `Exited (0) 3 hours ago`）。
    pub status: String,
    /// 状态**归类**（`running` / `exited` / …）——见 [`state_of`]。
    pub state: String,
    /// 端口映射（如 `0.0.0.0:5432->5432/tcp`）。
    pub ports: String,
    /// compose 项目名（不在 compose 里就是空）。
    pub compose: String,
    /// CPU 占用（`2.3%`；没采到就是空）。
    pub cpu: String,
    /// 内存占用（`51MiB / 63GiB`；没采到就是空）。
    pub mem: String,
}

impl ContainerInfo {
    /// 状态的中文说法（界面上的徽章文字）。
    pub fn state_label(&self) -> &'static str {
        state_label(&self.state)
    }

    /// 是不是 compose 项目组里的容器（组标题要显示它的项目名）。
    pub fn compose_group(&self) -> Option<&str> {
        if self.compose.is_empty() {
            None
        } else {
            Some(&self.compose)
        }
    }
}

/// 一次取回来的全部容器 + 探测到的 docker 版本。
#[derive(Clone, Debug, Default)]
pub struct DockerInfo {
    /// 容器列表（按 `docker ps` 的顺序：新建的在前）。
    pub containers: Vec<ContainerInfo>,
    /// 远程 docker 的服务端版本（探测到的）。
    pub version: String,
}

/* ---------------- 命令拼装（纯函数，好测） ---------------- */

/// 探测远程有没有 docker（`command -v`）。
pub fn which_cmd() -> String {
    "command -v docker 2>/dev/null || true".to_string()
}

/// 问 docker 服务端的版本（顺带确认**守护进程在不在**）。
pub fn version_cmd() -> String {
    "docker version --format '{{.Server.Version}}' 2>&1".to_string()
}

/// 容器列表命令（**完整版**）。
pub fn list_cmd() -> String {
    format!("docker ps -a --format '{PS_FIELDS}' 2>&1")
}

/// 容器列表命令（**降级版**：老 Docker 不认 `.Label` 时用）。
pub fn list_cmd_simple() -> String {
    format!("docker ps -a --format '{PS_FIELDS_SIMPLE}' 2>&1")
}

/// 资源占用命令（CPU/内存）。
///
/// ⚠️ `docker stats` **默认只报运行中的容器** ⇒ 已停止的容器没有这一行
/// （界面显示 `—`，跟 dbx 一致）。
pub fn stats_cmd() -> String {
    "docker stats --no-stream --format '{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}' 2>/dev/null"
        .to_string()
}

/* ---------------- 输出解析（纯函数，好测） ---------------- */

/// 解析 `docker ps` 的输出。
///
/// 一行一个容器，字段按 `|` 分隔。⚠️ **降级模板只有 5 个字段** ⇒
/// compose 项目名留空（不是错误）。
pub fn parse_ps(out: &str) -> Vec<ContainerInfo> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split(SEP);
        let id = f.next().unwrap_or("").trim().to_string();
        if id.is_empty() {
            continue;
        }
        let name = f.next().unwrap_or("").trim().to_string();
        let image = f.next().unwrap_or("").trim().to_string();
        let status = f.next().unwrap_or("").trim().to_string();
        let state = state_of(&status).to_string();
        v.push(ContainerInfo {
            id,
            name,
            image,
            status,
            state,
            ports: f.next().unwrap_or("").trim().to_string(),
            compose: f.next().unwrap_or("").trim().to_string(),
            cpu: String::new(),
            mem: String::new(),
        });
    }
    v
}

/// 解析 `docker stats` 的输出 ⇒ 容器名 → (CPU, 内存)。
pub fn parse_stats(out: &str) -> HashMap<String, (String, String)> {
    let mut m = HashMap::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split(SEP);
        let Some(name) = f.next() else { continue };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let cpu = f.next().unwrap_or("").trim().to_string();
        let mem = f.next().unwrap_or("").trim().to_string();
        m.insert(name.to_string(), (cpu, mem));
    }
    m
}

/// 从状态原文归类出状态（`running` / `exited` / …）。
///
/// ⚠️ **不依赖 `{{.State}}` 模板字段**（老 Docker 没有）⇒ 从状态文字推。
pub fn state_of(status: &str) -> &'static str {
    let s = status.trim();
    if s.starts_with("Exited") {
        "exited"
    } else if s.starts_with("Created") {
        "created"
    } else if s.starts_with("Paused") || s.contains("(Paused)") {
        "paused"
    } else if s.starts_with("Restarting") {
        "restarting"
    } else if s.starts_with("Dead") || s.starts_with("Removing") {
        "dead"
    } else if s.starts_with("Up") {
        "running"
    } else {
        "unknown"
    }
}

/// 状态的中文说法（界面徽章）。
pub fn state_label(state: &str) -> &'static str {
    match state {
        "running" => tr("运行中"),
        "exited" => tr("已停止"),
        "created" => tr("已创建"),
        "paused" => tr("已暂停"),
        "restarting" => tr("重启中"),
        "dead" => tr("已失效"),
        _ => tr("状态未知"),
    }
}

/// 把 docker 的报错翻成**中文 + 怎么办**。
///
/// ⚠️ **不自动 sudo**：远程权限不足时擅自 sudo 会要密码、还会留痕迹 ⇒
/// 只把原因说清楚，让用户自己决定。
pub fn classify_failure(raw: &str) -> String {
    let s = raw.to_ascii_lowercase();
    if s.contains("cannot connect to the docker daemon")
        || s.contains("is the docker daemon running")
        || s.contains("error during connect")
    {
        tr("远程的 Docker 服务没有运行（守护进程没起来）").to_string()
    } else if s.contains("permission denied") || s.contains("docker.sock") {
        tr("当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用有权限的账号登录）").to_string()
    } else if s.contains("command not found") || s.contains("no such file or directory") {
        tr("远程没有找到 docker（这台服务器上可能没装 Docker）").to_string()
    } else if s.contains("template parsing error") {
        tr("远程的 Docker 版本太老，容器列表取不到（升级 Docker 或换台服务器）").to_string()
    } else if s.contains("cannot connect to the unix socket") {
        tr("连不上远程的 Docker（服务没起，或当前账号没权限）").to_string()
    } else {
        // 认不出来 ⇒ 把原文给用户（别吞掉信息）
        let t = raw.trim();
        if t.is_empty() {
            tr("远程执行 docker 命令失败").to_string()
        } else {
            t.to_string()
        }
    }
}

/* ---------------- 真跑命令（在共享连接上） ---------------- */

/// 在**共享连接**上跑一条命令，拿到输出与退出码。
///
/// ⚠️ `pty = false`：不要伪终端 ⇒ 输出是原样字节（解析友好），也不会把
/// 换行翻译成 `\r\n`。
pub async fn run_cmd(
    session: &Handle<HostKeyHandler>,
    cmd: &str,
) -> BackendResult<(i32, String)> {
    let ch = ShellChannel::open_exec(session, cmd, 80, 24, false).await?;
    let mut out: Vec<u8> = Vec::new();
    let mut code = 0;
    while let Some(ev) = ch.next_event().await {
        match ev {
            ShellEvent::Data(b) => out.extend_from_slice(&b),
            ShellEvent::Exit(c) => {
                code = c;
                break;
            }
        }
    }
    Ok((code, String::from_utf8_lossy(&out).into_owned()))
}

/// 探路：远程有没有 docker、守护进程在不在、版本多少。
pub async fn probe(session: &Handle<HostKeyHandler>) -> BackendResult<String> {
    let (code, out) = run_cmd(session, &which_cmd()).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    if out.trim().is_empty() {
        return Err(BackendError(
            tr("远程没有找到 docker（这台服务器上可能没装 Docker）").to_string(),
        ));
    }
    let (code, ver) = run_cmd(session, &version_cmd()).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&ver)));
    }
    Ok(ver.trim().to_string())
}

/// 取容器列表（含 CPU/内存）。
///
/// ⚠️ **降级**：完整模板在老 Docker 上会报「template parsing error」⇒
/// 自动改用不带 `.Label` 的模板（compose 分组就没了，其余照常）。
pub async fn list(session: &Handle<HostKeyHandler>) -> BackendResult<Vec<ContainerInfo>> {
    let (code, out) = run_cmd(session, &list_cmd()).await?;
    let (mut list, out) = if code == 0 {
        (parse_ps(&out), out)
    } else if out.to_ascii_lowercase().contains("template parsing error") {
        let (code2, out2) = run_cmd(session, &list_cmd_simple()).await?;
        if code2 != 0 {
            return Err(BackendError(classify_failure(&out2)));
        }
        (parse_ps(&out2), out2)
    } else {
        return Err(BackendError(classify_failure(&out)));
    };

    // CPU/内存：拿不到就算了（不影响列表本身）。
    if let Ok((0, stats)) = run_cmd(session, &stats_cmd()).await {
        let m = parse_stats(&stats);
        for c in list.iter_mut() {
            if let Some((cpu, mem)) = m.get(&c.name) {
                c.cpu = cpu.clone();
                c.mem = mem.clone();
            }
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析容器列表() {
        let out = "abc123def456|web|nginx:latest|Up 2 hours (healthy)|0.0.0.0:80->80/tcp|\n\
                   def789abc012|db|postgres:17|Exited (0) 3 hours ago||proj\n";
        let v = parse_ps(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "abc123def456");
        assert_eq!(v[0].name, "web");
        assert_eq!(v[0].image, "nginx:latest");
        assert_eq!(v[0].state, "running");
        assert_eq!(v[0].ports, "0.0.0.0:80->80/tcp");
        assert_eq!(v[0].compose, "");
        assert_eq!(v[1].state, "exited");
        assert_eq!(v[1].compose, "proj");
    }

    #[test]
    fn 降级模板只有五个字段也认() {
        let out = "abc123def456|web|nginx:latest|Up 2 hours|0.0.0.0:80->80/tcp\n";
        let v = parse_ps(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].ports, "0.0.0.0:80->80/tcp");
        assert_eq!(v[0].compose, "");
    }

    #[test]
    fn 空行与坏行跳过() {
        let out = "\n   \n|坏行|\nabc|web|img|Up 1 hour|\n";
        let v = parse_ps(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "web");
    }

    #[test]
    fn 状态归类() {
        assert_eq!(state_of("Up 2 hours"), "running");
        assert_eq!(state_of("Up 5 minutes (healthy)"), "running");
        assert_eq!(state_of("Up 5 minutes (Paused)"), "paused");
        assert_eq!(state_of("Exited (0) 3 hours ago"), "exited");
        assert_eq!(state_of("Exited (137) 2 seconds ago"), "exited");
        assert_eq!(state_of("Created"), "created");
        assert_eq!(state_of("Restarting (1) 5 seconds ago"), "restarting");
        assert_eq!(state_of("Dead"), "dead");
        assert_eq!(state_of("说不清"), "unknown");
    }

    #[test]
    fn 解析资源占用() {
        let out = "web|2.30%|51.2MiB / 63.1GiB\ndb|0.00%|12MiB / 1GiB\n";
        let m = parse_stats(out);
        assert_eq!(m.get("web").unwrap().0, "2.30%");
        assert_eq!(m.get("web").unwrap().1, "51.2MiB / 63.1GiB");
        assert_eq!(m.get("db").unwrap().0, "0.00%");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn 命令里带完整模板() {
        let c = list_cmd();
        assert!(c.contains("docker ps -a"));
        assert!(c.contains("{{.Names}}"));
        assert!(c.contains("{{.Ports}}"));
        // 降级版不带 .Label
        assert!(!list_cmd_simple().contains(".Label"));
    }

    #[test]
    fn 报错翻成中文() {
        assert!(classify_failure("Cannot connect to the Docker daemon at unix:///var/run/docker.sock")
            .contains("Docker 服务没有运行"));
        assert!(classify_failure("permission denied while trying to connect to the Docker daemon socket")
            .contains("没有访问 Docker 的权限"));
        assert!(classify_failure("docker: command not found").contains("没有找到 docker"));
        assert!(classify_failure("template parsing error").contains("版本太老"));
        // 认不出来 ⇒ 原文给用户（不吞信息）
        assert_eq!(classify_failure("some weird failure"), "some weird failure");
    }
}
