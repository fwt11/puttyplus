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
const PS_FIELDS: &str = "{{.ID}}|{{.Names}}|{{.Image}}|{{.Status}}|{{.Ports}}|{{.Label \"com.docker.compose.project\"}}|{{.Label \"com.docker.compose.project.config_files\"}}|{{.Label \"com.docker.compose.project.working_dir\"}}";

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
    /// compose 的**配置文件路径**（标签 `com.docker.compose.project.config_files`）。
    ///
    /// ⚠️ 多个文件时是**逗号分隔**（`-f a.yml -f b.yml` 的项目）。
    pub compose_files: String,
    /// compose 的**工作目录**（标签 `com.docker.compose.project.working_dir`）。
    pub compose_dir: String,
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

/* ---------------- 容器详情（阶段 110） ---------------- */

/// 一个**挂载**（bind / volume）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MountInfo {
    /// 类型（`bind` / `volume` / `tmpfs`）。
    pub kind: String,
    /// 来源（宿主机的路径或卷名）。
    pub source: String,
    /// 容器里的目标路径。
    pub dest: String,
    /// 是否可写（`true` / `false`）。
    pub rw: String,
}

/// 一个**发布端口**。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortInfo {
    /// 容器端口（`5432/tcp`）。
    pub container: String,
    /// 主机端口（没发布就是空）。
    pub host_port: String,
    /// 主机地址（`0.0.0.0` / `127.0.0.1`；没发布就是空）。
    pub host_ip: String,
}

/// 一个**网络**。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkInfo {
    /// 网络名。
    pub name: String,
    /// 容器在这个网络里的 IP。
    pub ip: String,
    /// 网关。
    pub gateway: String,
}

/// 一个容器的**详情**（阶段 110）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerDetail {
    /// 容器名（`docker inspect` 给的是 `/名字` ⇒ 去掉开头的斜杠）。
    pub name: String,
    /// 镜像。
    pub image: String,
    /// 创建时间（ISO8601）。
    pub created: String,
    /// 状态（`running` / `exited` / …）。
    pub status: String,
    /// 健康检查（`healthy` / `unhealthy` / `none`）。
    pub health: String,
    /// 上次启动时间。
    pub started_at: String,
    /// 上次退出时间。
    pub finished_at: String,
    /// 上次退出的退出码。
    pub exit_code: String,
    /// 重启过几次。
    pub restart_count: String,
    /// 重启策略（`always` / `on-failure` / `no`）。
    pub restart_policy: String,
    /// 重启策略的最大重试次数（`on-failure` 才有意义）。
    pub restart_max_retry: String,
    /// 运行用户（空 = root）。
    pub user: String,
    /// 工作目录。
    pub workdir: String,
    /// 启动命令（`[sh -c …]` 这种原样）。
    pub command: String,
    /// 网络模式（`bridge` / `host` / `none` / 自定义网络名）。
    pub network_mode: String,
    /// 环境变量（`KEY=VALUE` 原样）。
    pub env: Vec<String>,
    /// 挂载。
    pub mounts: Vec<MountInfo>,
    /// 发布端口。
    pub ports: Vec<PortInfo>,
    /// 网络。
    pub networks: Vec<NetworkInfo>,
}

impl ContainerDetail {
    /// 状态的中文说法（界面徽章）。
    pub fn state_label(&self) -> &'static str {
        state_label(&self.status)
    }

    /// 是不是在跑（决定「启动 / 停止」哪个按钮可用）。
    pub fn is_running(&self) -> bool {
        self.status == "running"
    }
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

/* ---------------- 阶段 110：容器详情 + 启停 ---------------- */

/// 详情模板（**一条命令出全部字段**）。
///
/// ⚠️ **不用 JSON**：`docker inspect` 默认吐 JSON，而仓库里**没有 serde_json**
///    （也不打算为一个页面加依赖）⇒ 改用 docker 的 **Go 模板**直接吐
///    **带标签的行**：
///
/// ```text
/// F|字段|值      ← 标量字段（镜像、状态、重启策略……）
/// E|KEY=VALUE    ← 一个环境变量一行
/// M|类型|源|目标|读写   ← 一个挂载一行
/// P|容器端口|主机端口|主机地址   ← 一个发布端口一行
/// N|网络名|IP|网关   ← 一个网络一行
/// ```
///
/// ⇒ 按**行首标签**解析 ⇒ 值里有没有 `|` 都不影响（只有 M/P/N 内部按 `|` 切）。
const INSPECT_TEMPLATE: &str = r#"F|Name|{{.Name}}
F|Image|{{.Config.Image}}
F|Created|{{.Created}}
F|Status|{{.State.Status}}
F|Health|{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}
F|StartedAt|{{.State.StartedAt}}
F|FinishedAt|{{.State.FinishedAt}}
F|ExitCode|{{.State.ExitCode}}
F|RestartCount|{{.RestartCount}}
F|RestartPolicy|{{.HostConfig.RestartPolicy.Name}}
F|MaxRetry|{{.HostConfig.RestartPolicy.MaximumRetryCount}}
F|User|{{.Config.User}}
F|WorkDir|{{.Config.WorkingDir}}
F|Command|{{.Config.Cmd}}
F|NetworkMode|{{.HostConfig.NetworkMode}}
{{range .Config.Env}}E|{{.}}
{{end}}{{range .Mounts}}M|{{.Type}}|{{.Source}}|{{.Destination}}|{{.RW}}
{{end}}{{range $p, $bindings := .NetworkSettings.Ports}}{{range $bindings}}P|{{$p}}|{{.HostPort}}|{{.HostIP}}
{{end}}{{end}}{{range $n, $v := .NetworkSettings.Networks}}N|{{$n}}|{{$v.IPAddress}}|{{$v.Gateway}}
{{end}}"#;

/// 容器名**合不合法**（docker 只允许 `[a-zA-Z0-9][a-zA-Z0-9_.-]*`）。
///
/// ⚠️ **为什么必须验**：下面的命令是**拼出来的**（`docker stop <名字>`）⇒
///    名字里带 `;` `$(` 就能注入命令 ⇒ 拼之前先验一遍。
pub fn valid_name(name: &str) -> bool {
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 128
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// 容器详情命令（`docker inspect` + 上面那个模板）。
pub fn inspect_cmd(name: &str) -> String {
    format!("docker inspect --format '{INSPECT_TEMPLATE}' {name} 2>&1")
}

/// 对容器做的**写操作**（阶段 110）。
///
/// ⚠️ **这是写操作** ⇒ 界面必须先**二次确认**（见 `panel::dockerui`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// 启动。
    Start,
    /// 停止。
    Stop,
    /// 重启。
    Restart,
}

impl Action {
    /// 这个动作的命令。
    ///
    /// ⚠️ 名字**必须先过 [`valid_name`]**（见 [`run_action`]）。
    pub fn cmd(&self, name: &str) -> String {
        match self {
            Action::Start => format!("docker start {name} 2>&1"),
            Action::Stop => format!("docker stop {name} 2>&1"),
            Action::Restart => format!("docker restart {name} 2>&1"),
        }
    }

    /// 按钮上的字（动词）。
    pub fn label(&self) -> &'static str {
        match self {
            Action::Start => tr("启动"),
            Action::Stop => tr("停止"),
            Action::Restart => tr("重启"),
        }
    }

    /// 做完之后说的字（过去式）。
    pub fn past_label(&self) -> &'static str {
        match self {
            Action::Start => tr("已启动"),
            Action::Stop => tr("已停止"),
            Action::Restart => tr("已重启"),
        }
    }
}

/* ---------------- compose 项目（阶段 111） ---------------- */

/// 对 **compose 项目**做的写操作。
///
/// ⚠️ **这是写操作** ⇒ 界面必须先**二次确认**（见 `panel::dockerui`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    /// 启动（`docker compose start`）：把**已停止**的容器起起来。
    Start,
    /// 停止（`docker compose stop`）：停掉容器，**容器还留着**。
    Stop,
    /// 销毁（`docker compose down`）：停掉并**删除**容器与网络。
    Down,
    /// 重建（`docker compose up`）：按 compose 文件**重新创建并启动**。
    Up,
}

impl ComposeAction {
    /// `docker compose` 的子命令。
    pub fn subcmd(&self) -> &'static str {
        match self {
            ComposeAction::Start => "start",
            ComposeAction::Stop => "stop",
            ComposeAction::Down => "down",
            ComposeAction::Up => "up",
        }
    }

    /// 按钮上的字（动词）。
    pub fn label(&self) -> &'static str {
        match self {
            ComposeAction::Start => tr("启动"),
            ComposeAction::Stop => tr("停止"),
            ComposeAction::Down => tr("销毁"),
            ComposeAction::Up => tr("重建"),
        }
    }

    /// 做完之后说的字（过去式）。
    pub fn past_label(&self) -> &'static str {
        match self {
            ComposeAction::Start => tr("已启动"),
            ComposeAction::Stop => tr("已停止"),
            ComposeAction::Down => tr("已销毁"),
            ComposeAction::Up => tr("已重建"),
        }
    }

    /// 是不是**危险**动作（销毁会删掉容器与网络 ⇒ 确认框要说清楚）。
    pub fn is_dangerous(&self) -> bool {
        matches!(self, ComposeAction::Down)
    }
}

/// compose 项目的**写操作**命令。
///
/// ## 为什么这么拼
///
/// - **先 `cd` 到工作目录**：compose 会按目录找 `.env`、解析相对路径的卷
///   ⇒ 不 cd 的话行为跟用户自己在那个目录里跑不一样。
/// - **带 `-p <项目名>`**：项目名取自容器标签（`com.docker.compose.project`）
///   ⇒ **不靠目录推导**（用户可能把文件挪过位置）。
/// - **带 `-f <配置文件>`**：`up`/`down` **必须**有 compose 文件（否则 compose
///   报「no configuration file provided」）；文件路径同样取自标签。
///
/// ⚠️ 三个参数都来自**远端容器的标签**（= 远端可控的内容）⇒ 全部
///    `sh_quote` ⇒ 既防命令注入，也防路径里的空格把参数拆开。
pub fn compose_cmd(project: &str, files: &str, dir: &str, action: ComposeAction) -> String {
    let mut cmd = String::new();
    if !dir.is_empty() {
        cmd.push_str(&format!(
            "cd {} && ",
            crate::backend::checksum::sh_quote(dir)
        ));
    }
    cmd.push_str("docker compose");
    if !project.is_empty() {
        cmd.push_str(&format!(" -p {}", crate::backend::checksum::sh_quote(project)));
    }
    for f in files.split(',').map(str::trim).filter(|f| !f.is_empty()) {
        cmd.push_str(&format!(" -f {}", crate::backend::checksum::sh_quote(f)));
    }
    cmd.push(' ');
    cmd.push_str(action.subcmd());
    // ⚠️ `up` 默认**占着前台**（不返回）⇒ 必须加 `-d`（后台）。
    if matches!(action, ComposeAction::Up) {
        cmd.push_str(" -d");
    }
    cmd.push_str(" 2>&1");
    cmd
}

/* ---------------- 阶段 112：容器日志（流式） ---------------- */

/// 日志的**默认回看行数**（`docker logs --tail N`）。
pub const LOG_TAIL: usize = 200;

/// 日志流命令（`docker logs -f`）—— **带看门狗**（纯函数，好测）。
///
/// ## 为什么要有看门狗（**实测踩出来的坑**）
///
/// `docker logs -f` 是**长跑**的 ⇒ 通道断了以后它**不会自己退出**：
/// 实测（ssh 客户端断开后）远端还留着 `docker logs -f` 进程 ⇒
/// 用户每开关一次日志，就漏一个进程在服务器上跑着（容器安静的时候
/// 它能挂到天荒地老 —— 它不写数据 ⇒ 就不会收到 SIGPIPE）。
///
/// ⇒ 用「看门狗」把它包起来：
/// - `docker logs` 放**后台**（`p=$!`）；
/// - 看门狗（`w=$!`）读**通道的 stdin** ⇒ 通道一关，stdin 就是 EOF ⇒
///   看门狗 `kill` 掉 `docker logs`；
/// - `docker logs` 自己先退出（容器被删了……）⇒ `wait $p` 返回 ⇒ 顺手把
///   看门狗收掉 ⇒ 通道正常收尾（客户端能看到「流结束了」）。
///
/// ⚠️ **通道的 stdin 只有关通道时才关**（我们从不发 eof）⇒ 平时 `read`
///    一直阻塞 ⇒ 不会误杀。
/// ⚠️ 整个子 shell 的 **stderr 丢到 /dev/null** ⇒ 杀掉后台任务时 shell 会
///    打一句 "Terminated" ⇒ 那句话会混进日志里（实测）⇒ 丢掉。
///    ⚠️ 但 `docker logs` 自己的 stderr 仍要并进 stdout ⇒ 命令里单独写
///    `2>&1`（那是 **docker 进程**的 fd2，不是 shell 的）。
/// ⚠️ **容器名必须 `sh_quote`**（名字来自远端 ⇒ 远端可控 ⇒ 防注入）。
pub fn logs_cmd(name: &str, tail: usize) -> String {
    format!(
        "( docker logs -f --tail {tail} {n} 2>&1 & p=$!; ( read -r _; kill $p 2>/dev/null ) & w=$!; wait $p; kill $w 2>/dev/null; wait $w 2>/dev/null ) 2>/dev/null",
        n = crate::backend::checksum::sh_quote(name),
    )
}

/// 开一条**日志流**通道（`docker logs -f`）。
///
/// ⚠️ **返回通道**（不是输出）⇒ 调用方能**边收边显示**，也能**主动关掉**
///    （`ShellChannel::close` ⇒ 远端的命令退出 ⇒ 看门狗收尾）。
/// ⚠️ **不 pty** ⇒ 输出是原样字节（换行不被翻译成 `\r\n`）。
pub async fn open_logs(
    session: &Handle<HostKeyHandler>,
    name: &str,
    tail: usize,
) -> BackendResult<ShellChannel> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let ch = ShellChannel::open_exec(session, &logs_cmd(name, tail), 80, 24, false).await?;
    Ok(ch)
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
            compose_files: f.next().unwrap_or("").trim().to_string(),
            compose_dir: f.next().unwrap_or("").trim().to_string(),
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

/// 解析 `docker inspect` 的输出（**带标签的行** ⇒ 按行首标签分派）。
///
/// ⚠️ **认不出的行直接忽略**（docker 的报错文字、模板不支持时的提示……）。
/// ⚠️ `F|Name` 给的是 `/名字` ⇒ 去掉开头的斜杠。
pub fn parse_inspect(out: &str) -> ContainerDetail {
    let mut d = ContainerDetail::default();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = match line.split_once('|') {
            Some(x) => x,
            None => continue,
        };
        match tag {
            "F" => {
                // F|字段|值
                let Some((k, v)) = rest.split_once('|') else { continue };
                let v = v.trim().to_string();
                match k {
                    "Name" => d.name = v.trim_start_matches('/').to_string(),
                    "Image" => d.image = v,
                    "Created" => d.created = v,
                    "Status" => d.status = v,
                    "Health" => d.health = v,
                    "StartedAt" => d.started_at = v,
                    "FinishedAt" => d.finished_at = v,
                    "ExitCode" => d.exit_code = v,
                    "RestartCount" => d.restart_count = v,
                    "RestartPolicy" => d.restart_policy = v,
                    "MaxRetry" => d.restart_max_retry = v,
                    "User" => d.user = v,
                    "WorkDir" => d.workdir = v,
                    "Command" => d.command = v,
                    "NetworkMode" => d.network_mode = v,
                    _ => {}
                }
            }
            "E" => {
                // E|KEY=VALUE（原样；⚠️ 值里可能有 `|`，这里不切）
                if !rest.is_empty() {
                    d.env.push(rest.to_string());
                }
            }
            "M" => {
                // M|类型|源|目标|读写
                let mut f = rest.splitn(4, '|');
                let kind = f.next().unwrap_or("").trim().to_string();
                let source = f.next().unwrap_or("").trim().to_string();
                let dest = f.next().unwrap_or("").trim().to_string();
                let rw = f.next().unwrap_or("").trim().to_string();
                d.mounts.push(MountInfo {
                    kind,
                    source,
                    dest,
                    rw,
                });
            }
            "P" => {
                // P|容器端口|主机端口|主机地址
                let mut f = rest.splitn(3, '|');
                let container = f.next().unwrap_or("").trim().to_string();
                let host_port = f.next().unwrap_or("").trim().to_string();
                let host_ip = f.next().unwrap_or("").trim().to_string();
                d.ports.push(PortInfo {
                    container,
                    host_port,
                    host_ip,
                });
            }
            "N" => {
                // N|网络名|IP|网关
                let mut f = rest.splitn(3, '|');
                let name = f.next().unwrap_or("").trim().to_string();
                let ip = f.next().unwrap_or("").trim().to_string();
                let gateway = f.next().unwrap_or("").trim().to_string();
                d.networks.push(NetworkInfo {
                    name,
                    ip,
                    gateway,
                });
            }
            _ => {}
        }
    }
    d
}

/// 从状态原文归类出状态（`running` / `exited` / …）。
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

/// 取一个容器的**详情**（阶段 110）。
///
/// ⚠️ 走**共享连接**上的 exec 通道（不是新连接）⇒ 一台服务器仍然只有一条
///    SSH 连接。
pub async fn fetch_detail(
    session: &Handle<HostKeyHandler>,
    name: &str,
) -> BackendResult<ContainerDetail> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let (code, out) = run_cmd(session, &inspect_cmd(name)).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    let d = parse_inspect(&out);
    if d.name.is_empty() && d.image.is_empty() {
        return Err(BackendError(
            tr("取不到这个容器的信息（容器可能已经被删掉了）").to_string(),
        ));
    }
    Ok(d)
}

/// 对容器做一次**写操作**（启动 / 停止 / 重启）。
///
/// ⚠️ **这是写操作** ⇒ 调用方（界面）必须先**二次确认**。
/// ⚠️ 失败**给原因**（中文）⇒ 用户知道是权限不够还是容器不存在。
pub async fn run_action(
    session: &Handle<HostKeyHandler>,
    name: &str,
    action: Action,
) -> BackendResult<String> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let (code, out) = run_cmd(session, &action.cmd(name)).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(action.past_label().to_string())
}

/// 对 compose 项目做一次**写操作**（启动 / 停止 / 销毁 / 重建）。
///
/// ⚠️ **这是写操作** ⇒ 调用方（界面）必须先**二次确认**。
/// ⚠️ 失败**给原因**（中文）⇒ 用户知道是权限不够还是 compose 文件找不到了。
pub async fn run_compose_action(
    session: &Handle<HostKeyHandler>,
    project: &str,
    files: &str,
    dir: &str,
    action: ComposeAction,
) -> BackendResult<String> {
    if project.trim().is_empty() {
        return Err(BackendError(tr("compose 项目名不合法").to_string()));
    }
    let cmd = compose_cmd(project, files, dir, action);
    let (code, out) = run_cmd(session, &cmd).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(action.past_label().to_string())
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
    fn compose的配置文件与工作目录也取得到() {
        let out = "abc123def456|db|postgres:17|Up 2 hours||proj|/srv/app/docker-compose.yml|/srv/app\n";
        let v = parse_ps(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].compose, "proj");
        assert_eq!(v[0].compose_files, "/srv/app/docker-compose.yml");
        assert_eq!(v[0].compose_dir, "/srv/app");
    }

    #[test]
    fn compose命令带项目名配置文件和工作目录() {
        let cmd = compose_cmd(
            "proj",
            "/srv/app/docker-compose.yml",
            "/srv/app",
            ComposeAction::Stop,
        );
        assert!(
            cmd.contains("docker compose"),
            "命令里应当有 docker compose：{cmd}"
        );
        assert!(cmd.contains("-p 'proj'"), "应当带项目名：{cmd}");
        assert!(
            cmd.contains("-f '/srv/app/docker-compose.yml'"),
            "应当带配置文件：{cmd}"
        );
        assert!(cmd.starts_with("cd '/srv/app' && "), "应当先 cd：{cmd}");
        assert!(cmd.contains(" stop"), "子命令不对：{cmd}");
        assert!(
            !cmd.contains(" -d"),
            "stop 不该带 -d（只有 up 才带）：{cmd}"
        );
    }

    #[test]
    fn compose重建要后台跑() {
        let cmd = compose_cmd("proj", "/a/compose.yml", "/a", ComposeAction::Up);
        assert!(cmd.contains(" up -d"), "up 必须加 -d（否则占着前台）：{cmd}");
    }

    #[test]
    fn compose命令的参数被引号包住() {
        // ⚠️ 标签是**远端可控**的内容 ⇒ 路径里有空格/引号时不能把参数拆开。
        let cmd = compose_cmd(
            "my proj",
            "/srv/my app/compose.yml",
            "/srv/my app",
            ComposeAction::Down,
        );
        assert!(cmd.contains("-p 'my proj'"), "项目名没包引号：{cmd}");
        assert!(
            cmd.contains("-f '/srv/my app/compose.yml'"),
            "配置文件没包引号：{cmd}"
        );
        assert!(cmd.starts_with("cd '/srv/my app' && "), "目录没包引号：{cmd}");
    }

    #[test]
    fn compose命令防注入() {
        // ⚠️ 标签里塞 shell 元字符 ⇒ 不能被当成命令执行。
        let cmd = compose_cmd(
            "p; rm -rf /",
            "/a/b.yml",
            "",
            ComposeAction::Start,
        );
        assert!(
            cmd.contains("-p 'p; rm -rf /'"),
            "项目名没被引号包住（能被注入）：{cmd}"
        );
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

    /* ---------------- 阶段 110：详情 + 启停 ---------------- */

    /// 解析 `docker inspect` 的输出（**真机模板的形状**）。
    #[test]
    fn 解析容器详情() {
        let out = "F|Name|/web\n\
                   F|Image|nginx:latest\n\
                   F|Created|2026-11-10T11:44:26Z\n\
                   F|Status|running\n\
                   F|Health|healthy\n\
                   F|StartedAt|2026-11-15T00:59:47Z\n\
                   F|FinishedAt|2026-11-14T15:56:52Z\n\
                   F|ExitCode|0\n\
                   F|RestartCount|2\n\
                   F|RestartPolicy|always\n\
                   F|MaxRetry|0\n\
                   F|User|www-data\n\
                   F|WorkDir|/app\n\
                   F|Command|[nginx -g daemon off;]\n\
                   F|NetworkMode|bridge\n\
                   E|PATH=/usr/bin\n\
                   E|DB_URL=postgres://u:p@h:5432/db?a=1|2\n\
                   M|bind|/srv/site|/usr/share/nginx/html|false\n\
                   M|volume|vol1|/data|true\n\
                   P|443/tcp|8443|0.0.0.0\n\
                   N|infra|172.19.0.2|172.19.0.1\n";
        let d = parse_inspect(out);
        // 名字去掉 docker 给的斜杠
        assert_eq!(d.name, "web");
        assert_eq!(d.image, "nginx:latest");
        assert_eq!(d.status, "running");
        assert!(d.is_running());
        assert_eq!(d.health, "healthy");
        assert_eq!(d.restart_count, "2");
        assert_eq!(d.restart_policy, "always");
        assert_eq!(d.user, "www-data");
        assert_eq!(d.workdir, "/app");
        assert_eq!(d.network_mode, "bridge");
        // 环境变量：值里有 `|` 也不能被切坏
        assert_eq!(d.env.len(), 2);
        assert_eq!(d.env[0], "PATH=/usr/bin");
        assert_eq!(d.env[1], "DB_URL=postgres://u:p@h:5432/db?a=1|2");
        // 挂载 / 端口 / 网络
        assert_eq!(d.mounts.len(), 2);
        assert_eq!(d.mounts[0].kind, "bind");
        assert_eq!(d.mounts[0].source, "/srv/site");
        assert_eq!(d.mounts[0].dest, "/usr/share/nginx/html");
        assert_eq!(d.mounts[0].rw, "false");
        assert_eq!(d.ports.len(), 1);
        assert_eq!(d.ports[0].container, "443/tcp");
        assert_eq!(d.ports[0].host_port, "8443");
        assert_eq!(d.networks.len(), 1);
        assert_eq!(d.networks[0].name, "infra");
        assert_eq!(d.networks[0].ip, "172.19.0.2");
    }

    /// 认不出的行**直接忽略**（别把 docker 的报错文字当成字段）。
    #[test]
    fn 详情解析忽略杂行() {
        let out = "Error: No such object: nope\n\
                   F|Name|/web\n\
                   \n\
                   随便一行\n\
                   F|Image|img\n";
        let d = parse_inspect(out);
        assert_eq!(d.name, "web");
        assert_eq!(d.image, "img");
        assert!(d.env.is_empty());
        assert!(d.mounts.is_empty());
    }

    /// 空输出 ⇒ 空详情（界面据此报「取不到信息」）。
    #[test]
    fn 详情解析空输出() {
        let d = parse_inspect("");
        assert_eq!(d.name, "");
        assert_eq!(d.image, "");
        assert!(!d.is_running());
    }

    /// 容器名**必须合法**（命令是拼出来的 ⇒ 别让人拼进 shell 元字符）。
    #[test]
    fn 容器名要合法() {
        assert!(valid_name("web"));
        assert!(valid_name("my-app_1.2"));
        assert!(valid_name("A"));
        assert!(!valid_name(""));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("-dash"));
        assert!(!valid_name("web; rm -rf /"));
        assert!(!valid_name("web$(id)"));
        assert!(!valid_name("web`id`"));
        assert!(!valid_name("web|x"));
        assert!(!valid_name(&"a".repeat(129)));
    }

    /// 详情命令：一条命令出全部字段（不解析 JSON）。
    #[test]
    fn 详情命令用模板不用json() {
        let c = inspect_cmd("web");
        assert!(c.contains("docker inspect"));
        assert!(c.contains("{{.Config.Image}}"));
        assert!(c.contains("com.docker.compose") == false, "详情不需要 compose 标签");
        assert!(!c.contains("--format json"));
        // 名字是拼进去的 ⇒ 命令里就是它（合法性由 valid_name 守着）
        assert!(c.ends_with("web 2>&1"));
    }

    /// 启停命令拼得对。
    #[test]
    fn 启停命令() {
        assert!(Action::Start.cmd("web").contains("docker start web"));
        assert!(Action::Stop.cmd("web").contains("docker stop web"));
        assert!(Action::Restart.cmd("web").contains("docker restart web"));
        assert_eq!(Action::Start.label(), "启动");
        assert_eq!(Action::Stop.past_label(), "已停止");
    }

    /* ---------------- 阶段 112：日志流 ---------------- */

    /// 日志命令：`docker logs -f` + 回看行数。
    #[test]
    fn 日志命令是跟随式的() {
        let c = logs_cmd("web", 200);
        assert!(c.contains("docker logs -f"), "应当是跟随式的：{c}");
        assert!(c.contains("--tail 200"), "应当带回看行数：{c}");
        assert!(c.contains("'web'"), "容器名没包引号：{c}");
        // ⚠️ 容器自己的 stderr 要并进 stdout（docker logs 分两路写）。
        assert!(c.contains("2>&1"), "应当合并 stderr：{c}");
    }

    /// ⚠️ **看门狗必须还在**（否则关掉日志页后远端会漏进程）。
    ///
    /// 这条是**回归钉**：谁把看门狗删了 ⇒ 用户每开关一次日志就在服务器上
    /// 漏一个 `docker logs -f` 进程（实测：断开后它一直挂着）。
    #[test]
    fn 日志命令带看门狗() {
        let c = logs_cmd("web", 200);
        // 看门狗读通道的 stdin ⇒ 通道一关（EOF）就杀掉 docker logs。
        assert!(c.contains("read -r _"), "看门狗没了（不再盯着 stdin）：{c}");
        assert!(c.contains("kill $p"), "看门狗不杀进程了：{c}");
        // docker logs 自己退出时，看门狗也要被收掉。
        assert!(c.contains("wait $p"), "没有等 docker logs 退出：{c}");
        assert!(c.contains("kill $w"), "没回收看门狗：{c}");
        // ⚠️ 杀掉后台任务时 shell 会打 "Terminated" ⇒ 会混进日志 ⇒ 丢掉。
        assert!(c.contains("2>/dev/null"), "shell 的杂音没丢掉：{c}");
    }

    /// 回看行数可以调。
    #[test]
    fn 日志命令的回看行数可调() {
        assert!(logs_cmd("web", 50).contains("--tail 50"));
        assert!(logs_cmd("web", LOG_TAIL).contains("--tail 200"));
    }

    /// ⚠️ **容器名是远端可控的内容** ⇒ 必须引起来（防注入）。
    #[test]
    fn 日志命令防注入() {
        let c = logs_cmd("a; rm -rf /", 200);
        assert!(c.contains("'a; rm -rf /'"), "容器名没被引号包住：{c}");
        // 把引号那一处去掉以后 ⇒ 名字不该再出现在命令里。
        let stripped = c.replace("'a; rm -rf /'", "");
        assert!(
            !stripped.contains("rm -rf"),
            "容器名漏出了引号（能被注入）：{c}"
        );
    }
}
