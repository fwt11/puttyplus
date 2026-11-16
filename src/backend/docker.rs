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
use crate::trf;

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

/* ---------------- 阶段 113：容器内终端 ---------------- */

/// 容器里可能有的 shell（**按顺序挨个试**）。
///
/// ⚠️ **为什么要挨个试**：容器镜像千差万别 —— alpine 只有 `/bin/sh`
///    （busybox），debian 有 `/bin/sh` ＋ `/bin/bash`，有些镜像**一个都没有**
///    （distroless）⇒ 挨个试，全不行才报错。
///
/// ⚠️ **顺序**：先 `/bin/sh`（几乎所有镜像都有，且是 POSIX 的）⇒ 命中率最高。
pub const SHELL_CANDIDATES: [&str; 4] = ["/bin/sh", "/bin/bash", "/bin/ash", "/bin/zsh"];

/// 探测命令：这个 shell 在容器里**能不能跑**（**不分配 pty** ⇒ 不需要交互）。
///
/// ⚠️ 用 `exit 0` 而不是 `true`：命令 `true` 得是容器里的**二进制**，
///    而 distroless 那种镜像里未必有 ⇒ 用 shell 内建的 `exit` 最保险。
pub fn shell_probe_cmd(name: &str, shell: &str) -> String {
    format!(
        "docker exec {n} {sh} -c 'exit 0' 2>&1",
        n = crate::backend::checksum::sh_quote(name),
        sh = crate::backend::checksum::sh_quote(shell)
    )
}

/// 容器内终端的命令（`docker exec -it`）。
///
/// ⚠️ **`-t`（在容器里分配 pty）**：容器里的 shell 需要终端才是交互式的
///    （提示符、行规程都靠它）⇒ 我们这条 SSH 通道**也**申请了 pty
///    （`ShellChannel::open_exec(..., pty=true)`）⇒ 两边都是 tty ⇒ docker
///    不会报「the input device is not a TTY」。
pub fn exec_shell_cmd(name: &str, shell: &str) -> String {
    format!(
        "docker exec -it {n} {sh}",
        n = crate::backend::checksum::sh_quote(name),
        sh = crate::backend::checksum::sh_quote(shell)
    )
}

/// 挨个试 ⇒ 容器里到底哪个 shell 能用（都没有 ⇒ 报错说清楚）。
///
/// ⚠️ **失败要给「怎么办」**：容器**没在运行**时也说「没有可用的 shell」是
///    误导 ⇒ 单独认出来。
pub async fn find_shell(session: &Handle<HostKeyHandler>, name: &str) -> BackendResult<String> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let mut last = String::new();
    for sh in SHELL_CANDIDATES {
        let (code, out) = run_cmd(session, &shell_probe_cmd(name, sh)).await?;
        if code == 0 {
            return Ok(sh.to_string());
        }
        last = out;
    }
    // ⚠️ **先分类**（容器没运行 / 没权限 ⇒ 说那件事，别说「没有 shell」）。
    let low = last.to_ascii_lowercase();
    if low.contains("is not running") || low.contains("container is not running") {
        return Err(BackendError(
            tr("这个容器没有在运行（先启动它才能进终端）").to_string(),
        ));
    }
    if low.contains("permission denied") || low.contains("docker.sock") {
        return Err(BackendError(
            tr("当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用有权限的账号登录）")
                .to_string(),
        ));
    }
    Err(BackendError(trf!(
        "这个容器里没有可用的 shell（试过 {list}）",
        list = SHELL_CANDIDATES.join("、")
    )))
}

/// 探**用户点名**的某个 shell 在容器里能不能用（**不挨个试**）。
///
/// ⚠️ **为什么要有这个**（阶段 119）：界面上可以让用户**点名**要哪个
///    shell（比如 `/bin/bash`）⇒ 点名的那个**不能用**时得说清楚
///    （「这个容器里没有 /bin/bash」），**不能悄悄换一个**。
///    （挨个试的 [`find_shell`] 是「自动」模式用的。）
pub async fn verify_shell(
    session: &Handle<HostKeyHandler>,
    name: &str,
    shell: &str,
) -> BackendResult<String> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let (code, out) = run_cmd(session, &shell_probe_cmd(name, shell)).await?;
    if code == 0 {
        return Ok(shell.to_string());
    }
    Err(BackendError(shell_probe_error_msg(shell, &out)))
}

/// 探 shell 失败时**分类**成一句中文提示（**纯函数** ⇒ 能单测）。
///
/// ⚠️ **为什么要分类**：容器**没在运行** / **账号没权限** ⇒ 说「没有这个
///   shell」是**误导** ⇒ 用户会照着错误提示去改 shell 名（越改越不对）。
/// ⚠️ **「没有这个 shell」那条要给出「怎么办」**：把**能换的**列出来。
pub fn shell_probe_error_msg(shell: &str, out: &str) -> String {
    let low = out.to_ascii_lowercase();
    if low.contains("is not running") || low.contains("container is not running") {
        return tr("这个容器没有在运行（先启动它才能进终端）").to_string();
    }
    if low.contains("permission denied") || low.contains("docker.sock") {
        return tr("当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用有权限的账号登录）")
            .to_string();
    }
    trf!(
        "这个容器里没有 {sh}（可以换一个：{list}）",
        sh = shell,
        list = SHELL_CANDIDATES.join("、")
    )
}

/// 开一条**容器内终端**通道（`docker exec -it`）。
///
/// ⚠️ **要 pty**（`pty = true`）：容器里的 shell 得有个终端才是交互式的。
pub async fn open_exec_shell(
    session: &Handle<HostKeyHandler>,
    name: &str,
    shell: &str,
    cols: u16,
    rows: u16,
) -> BackendResult<ShellChannel> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let ch = ShellChannel::open_exec(session, &exec_shell_cmd(name, shell), cols, rows, true).await?;
    Ok(ch)
}

/* ---------------- 阶段 114：监控曲线（docker stats 流） ---------------- */

/// 监控采样的命令：`docker stats` **持续模式**，**只盯一个容器**。
///
/// ⚠️ **为什么用持续模式而不是 `--no-stream`**：
/// - `--no-stream` **单次就要 2 秒**（实测：docker 内部要采两次 cgroup
///   差值）⇒ 每 2 秒跑一次 = 远端**几乎连续**在跑 docker stats；
/// - 持续模式是**一条通道一个进程** ⇒ 远端开销固定，而且**关通道就能
///   收掉**（看门狗，跟阶段 112 的日志流一个套路）。
///
/// ⚠️ **为什么只盯一个容器**：`docker stats`（不加名字）会统计**所有**
///   容器 ⇒ 每 500ms 刷一次 ⇒ 容器多的时候远端开销明显 ⇒ 只盯选中的
///   那个，便宜得多。
///
/// ⚠️ **看门狗**：通道断了远端的 `docker stats` **不会自己退出**（实测：
///   阶段 112 的 `docker logs -f` 同样问题）⇒ 这里套同一层看门狗
///   （读通道的 stdin ⇒ 通道一关就是 EOF ⇒ 杀掉它）。
///
/// ⚠️ **持续模式会带 ANSI 转义**（docker 的「表格重绘」：`\x1b[H` /
///   `\x1b[K` / `\x1b[J`）⇒ 客户端要**剥掉**（见 [`strip_ansi`]）。
pub fn stats_stream_cmd(name: &str) -> String {
    format!(
        "( docker stats --format '{{{{.CPUPerc}}}}|{{{{.MemPerc}}}}|{{{{.NetIO}}}}|{{{{.BlockIO}}}}' {n} 2>&1 & p=$!; ( read -r _; kill $p 2>/dev/null ) & w=$!; wait $p; kill $w 2>/dev/null; wait $w 2>/dev/null ) 2>/dev/null",
        n = crate::backend::checksum::sh_quote(name),
    )
}

/// 开一条**监控采样**通道（`docker stats` 持续模式）。
///
/// ⚠️ **返回通道**（不是输出）⇒ 调用方能**边收边画曲线**，也能**主动关掉**
///    （`ShellChannel::close` ⇒ 看门狗收掉远端的 docker stats）。
/// ⚠️ **不 pty** ⇒ 输出是原样字节（不掺 `\r\n` 翻译）。
pub async fn open_stats(
    session: &Handle<HostKeyHandler>,
    name: &str,
) -> BackendResult<ShellChannel> {
    if !valid_name(name) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let ch = ShellChannel::open_exec(session, &stats_stream_cmd(name), 80, 24, false).await?;
    Ok(ch)
}

/// 一次采样（**纯数据**，好测）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatsSample {
    /// CPU 占用（%）。
    pub cpu: f64,
    /// 内存占用（%）。
    pub mem: f64,
    /// 网络**接收**（**累计**字节 ⇒ 差分才是速率）。
    pub net_rx: u64,
    /// 网络**发送**（累计字节）。
    pub net_tx: u64,
    /// 磁盘**读**（累计字节）。
    pub blk_read: u64,
    /// 磁盘**写**（累计字节）。
    pub blk_write: u64,
}

/// 剥掉 ANSI 转义序列（**纯函数**）。
///
/// ⚠️ **为什么需要**：`docker stats` 的持续模式是「**表格重绘**」⇒ 每行
///   都带 `\x1b[H`（光标到行首）/ `\x1b[K`（清行）/ `\x1b[J`（清屏）。
///   不剥的话，解析出来的全是转义垃圾。
///
/// ⚠️ **规则**（跟终端的 CSI 一样）：`ESC [` 开头 ⇒ 一直到**第一个
///   0x40~0x7E 的字节**为止是转义序列；`ESC` 后面不是 `[` ⇒ 两字节转义
///   （吃掉后一个字节）。
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if it.peek() == Some(&'[') {
            it.next(); // 吃掉 '['
            // CSI 的最后一个字节在 0x40~0x7E。
            for c2 in it.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c2) {
                    break;
                }
            }
        } else {
            it.next(); // 两字节转义：吃掉后一个字节
        }
    }
    out
}

/// 解析 docker 的大小（`"6.14kB"` ⇒ 6140）⇒ **字节数**。
///
/// ⚠️ **单位是 SI 还是二进制**：docker 用 `units.HumanSize` ⇒ **SI**
///   （kB=1000、MB=10⁶）；但有些版本会吐 `KiB/MiB` ⇒ **两种都认**
///   （二进制那套按 1024 算）。
///
/// ⚠️ **认不出来的单位** ⇒ 当成字节（宁可低估，也别丢数据）。
pub fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // ⚠️ **不能按「非数字」split**：`"6.14kB"` 会把 `k` 当分隔符吃掉
    //   ⇒ 单位变成 "B" ⇒ 6.14kB 变成 6 字节（**少了一千倍**）。
    //   ⇒ 改成**按位置切**：数字段结束的地方就是单位的开始。
    let num_end = s.find(|c: char| !(c.is_ascii_digit() || c == '.'));
    let (num_s, unit) = match num_end {
        Some(i) => (&s[..i], s[i..].trim().to_ascii_lowercase()),
        None => (s, String::new()),
    };
    let num: f64 = num_s.parse().ok()?;
    let mul: f64 = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    Some((num * mul).round() as u64)
}

/// `"6.14kB / 126B"` ⇒ `(接收, 发送)`（**纯函数**）。
fn split_io(s: &str) -> Option<(u64, u64)> {
    let mut it = s.split('/');
    let a = parse_size_bytes(it.next()?)?;
    let b = parse_size_bytes(it.next()?)?;
    Some((a, b))
}

/// 解析 `docker stats` 的一行 ⇒ 一次采样（**纯函数**）。
///
/// ⚠️ **输入**：`"2.08%|0.08%|6.14kB / 126B|40.4MB / 242kB"`
///   （也可能带 ANSI ⇒ 先剥）。
/// ⚠️ **认不出来就返回 `None`**（那一行是表头/空行/别的 ⇒ 丢掉）。
pub fn parse_stats_line(line: &str) -> Option<StatsSample> {
    let s = strip_ansi(line);
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let f: Vec<&str> = s.split('|').collect();
    if f.len() < 4 {
        return None;
    }
    let pct = |x: &str| -> f64 {
        x.trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
    };
    let cpu = pct(f[0]);
    let mem = pct(f[1]);
    let (net_rx, net_tx) = split_io(f[2])?;
    let (blk_read, blk_write) = split_io(f[3])?;
    Some(StatsSample {
        cpu,
        mem,
        net_rx,
        net_tx,
        blk_read,
        blk_write,
    })
}

/// 两次采样之间的**速率**（字节/秒）。
///
/// ⚠️ **为什么差分**：`docker stats` 的 NetIO/BlockIO 是**从容器启动起
///   的累计值** ⇒ 直接画曲线会是一条单调上升的直线（看不出「现在有多
///   忙」）⇒ 差分成速率才直观。
///
/// ⚠️ **累计值变小** ⇒ 容器**重启过**（计数归零）⇒ 那一次的差分**丢掉**
///   （否则会画出一根巨大的尖峰）。
pub fn rate_between(prev: &StatsSample, cur: &StatsSample, dt_secs: f64) -> Option<(f64, f64, f64, f64)> {
    if dt_secs <= 0.0 {
        return None;
    }
    let d = |a: u64, b: u64| -> Option<f64> {
        if b < a {
            None
        } else {
            Some((b - a) as f64 / dt_secs)
        }
    };
    Some((
        d(prev.net_rx, cur.net_rx)?,
        d(prev.net_tx, cur.net_tx)?,
        d(prev.blk_read, cur.blk_read)?,
        d(prev.blk_write, cur.blk_write)?,
    ))
}

/// 曲线上的**一个点**（瞬时值 ＋ 速率）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StatPoint {
    /// CPU（%）。
    pub cpu: f64,
    /// 内存（%）。
    pub mem: f64,
    /// 网络接收（字节/秒）。
    pub net_rx: f64,
    /// 网络发送（字节/秒）。
    pub net_tx: f64,
    /// 磁盘读（字节/秒）。
    pub blk_read: f64,
    /// 磁盘写（字节/秒）。
    pub blk_write: f64,
}

/// 采样节流器（**每 N 秒出一个点**）。
///
/// ⚠️ **为什么需要它**：`docker stats` 的持续模式**每 500ms 刷一次**
///   ⇒ 直接用会得到一堆挤在一起的点（曲线是锯齿、消息也多）⇒ 我们
///   **每 `interval` 秒出一个点**。
///
/// ⚠️ **速率怎么算**：拿**上一个原始样本**差分（不是上一个「出点」）
///   ⇒ 速率是「这一段时间的**平均**」⇒ 中间的尖峰不会被漏掉。
#[derive(Default)]
pub struct StatsSampler {
    /// 上一次**出点**的时刻（相对秒，调用方给）。
    last_at: Option<f64>,
    /// 上一个原始样本 ＋ 它的时刻（差分用）。
    last_raw: Option<(f64, StatsSample)>,
}

impl StatsSampler {
    /// 喂一个原始样本 ⇒ **够一个周期**就返回一个点（否则 `None`）。
    ///
    /// ⚠️ `now` 是**相对时间**（秒）⇒ 纯逻辑，好测。
    /// ⚠️ **第一次**没有上一个累计值 ⇒ 差分算不出来 ⇒ 速率按 0 算
    ///   （CPU/内存照常有值）。
    pub fn push(&mut self, raw: &StatsSample, now: f64, interval: f64) -> Option<StatPoint> {
        let due = self.last_at.map(|t| now - t >= interval).unwrap_or(true);
        let prev = self.last_raw;
        self.last_raw = Some((now, *raw));
        if !due {
            return None;
        }
        self.last_at = Some(now);
        let mut pt = StatPoint {
            cpu: raw.cpu,
            mem: raw.mem,
            ..Default::default()
        };
        if let Some((t0, p0)) = prev {
            if let Some((rx, tx, br, bw)) = rate_between(&p0, raw, now - t0) {
                pt.net_rx = rx;
                pt.net_tx = tx;
                pt.blk_read = br;
                pt.blk_write = bw;
            }
        }
        Some(pt)
    }
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
    } else if s.contains("no such container") {
        tr("取不到这个容器的信息（容器可能已经被删掉了）").to_string()
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
    // ⚠️ **先看输出、再看退出码**：`command -v docker` 找不到时**输出是空的**
    //   （退出码 1）⇒ 说「这台机器上没装 Docker」比说「执行失败」准。
    if out.trim().is_empty() {
        return Err(BackendError(
            tr("远程没有找到 docker（这台服务器上可能没装 Docker）").to_string(),
        ));
    }
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
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

/* ---------------- 阶段 115：镜像 / 卷 / 网络（只读列表） ---------------- */

/// 镜像列表命令。
///
/// ⚠️ **只列本地镜像**（`docker images` 不带 `-a` 之外的参数就是本地）⇒
///   跟 dbx 的镜像页一致（用户要的是「这台机器上有哪些镜像」）。
/// ⚠️ `{{.Repository}}` 与 `{{.Tag}}` 分开取 ⇒ 界面能分别显示「仓库」与
///   「标签」两列（悬空镜像是 `<none>` / `<none>`）。
pub fn images_cmd() -> String {
    "docker images --format '{{.Repository}}|{{.Tag}}|{{.ID}}|{{.Size}}|{{.CreatedAt}}' 2>&1"
        .to_string()
}

/// 卷列表命令（**本地卷**）。
pub fn volumes_cmd() -> String {
    "docker volume ls --format '{{.Name}}|{{.Driver}}' 2>&1".to_string()
}

/// 网络列表命令。
///
/// ⚠️ **会把默认网络也列出来**（`bridge` / `host` / `none`）⇒ 这是
///   docker 的事实 ⇒ 照实显示（用户要看到全貌）。
pub fn networks_cmd() -> String {
    "docker network ls --format '{{.Name}}|{{.Driver}}|{{.Scope}}' 2>&1".to_string()
}

/// 一个镜像（阶段 115）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageInfo {
    /// 仓库名（如 `postgis/postgis`；悬空镜像是 `<none>`）。
    pub repository: String,
    /// 标签（如 `17-3.5`；悬空镜像是 `<none>`）。
    pub tag: String,
    /// 镜像 ID（前 12 位）。
    pub id: String,
    /// 大小（docker 的原文，如 `1.23GB`）。
    pub size: String,
    /// 创建时间（docker 的原文，如 `3 days ago`）。
    pub created: String,
}

/// 一个卷（阶段 115）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 卷名。
    pub name: String,
    /// 驱动（如 `local`）。
    pub driver: String,
}

/// 一个网络（阶段 115）。
///
/// ⚠️ **别跟容器详情里的 `NetworkInfo` 混**：那个是这个容器挂上去的
///   网络（IP / 网关）；这个是 docker 的**网络对象**。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetInfo {
    /// 网络名。
    pub name: String,
    /// 驱动（如 `bridge` / `overlay`）。
    pub driver: String,
    /// 范围（`local` / `swarm`）。
    pub scope: String,
}

/// 解析 `docker images` 的输出 ⇒ 镜像列表（**纯函数**）。
///
/// ⚠️ **行数不够就丢掉**：docker 出错时也会往 stdout 吐东西 ⇒ 别把
///   错误信息当成一行镜像。
pub fn parse_images(out: &str) -> Vec<ImageInfo> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(SEP).collect();
        if f.len() < 5 {
            continue;
        }
        v.push(ImageInfo {
            repository: f[0].trim().to_string(),
            tag: f[1].trim().to_string(),
            id: f[2].trim().to_string(),
            size: f[3].trim().to_string(),
            created: f[4].trim().to_string(),
        });
    }
    v
}

/// 解析 `docker volume ls` 的输出 ⇒ 卷列表（**纯函数**）。
pub fn parse_volumes(out: &str) -> Vec<VolumeInfo> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(SEP).collect();
        if f.len() < 2 {
            continue;
        }
        v.push(VolumeInfo {
            name: f[0].trim().to_string(),
            driver: f[1].trim().to_string(),
        });
    }
    v
}

/// 解析 `docker network ls` 的输出 ⇒ 网络列表（**纯函数**）。
pub fn parse_networks(out: &str) -> Vec<NetInfo> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(SEP).collect();
        if f.len() < 3 {
            continue;
        }
        v.push(NetInfo {
            name: f[0].trim().to_string(),
            driver: f[1].trim().to_string(),
            scope: f[2].trim().to_string(),
        });
    }
    v
}

/// 取**镜像列表**（阶段 115）。
///
/// ⚠️ 走**共享连接**上的 exec 通道（不是新连接）⇒ 一台服务器仍然只有
///   一条 SSH 连接。
pub async fn list_images(session: &Handle<HostKeyHandler>) -> BackendResult<Vec<ImageInfo>> {
    let (code, out) = run_cmd(session, &images_cmd()).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(parse_images(&out))
}

/// 取**卷列表**（阶段 115）。
pub async fn list_volumes(session: &Handle<HostKeyHandler>) -> BackendResult<Vec<VolumeInfo>> {
    let (code, out) = run_cmd(session, &volumes_cmd()).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(parse_volumes(&out))
}

/// 取**网络列表**（阶段 115）。
pub async fn list_networks(session: &Handle<HostKeyHandler>) -> BackendResult<Vec<NetInfo>> {
    let (code, out) = run_cmd(session, &networks_cmd()).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(parse_networks(&out))
}

/* ---------------- 阶段 116：容器内文件浏览 + 上传 + 下载 ---------------- */

/// 列目录的命令（`docker exec` 跑 `ls -lA`）。
///
/// ⚠️ **为什么是 `ls -lA`**：容器里**没有 SFTP** ⇒ 只能 `docker exec`。
///   - `-l`（长格式）⇒ 拿得到**大小**和**类型**（权限串第一个字符）；
///   - `-A`（**不是** `-a`）⇒ 列出**隐藏文件**，但**不含** `.` 与 `..`；
///   - ⚠️ **时间字段不解析**（各 locale 的格式不一样：`Nov 15 19:33` /
///     `2024-11-15 19:33:06` / `Nov 15 2024` 都出现过）⇒ 界面不显示时间。
/// ⚠️ **不套 `sh -c`**：`docker exec` 直接把参数交给容器里的 `ls`
///   ⇒ **没有嵌套引号的问题**（路径里的引号只需要 `sh_quote` 一次）。
pub fn list_dir_cmd(container: &str, path: &str) -> String {
    format!(
        "docker exec {c} ls -lA -- {p} 2>&1",
        c = crate::backend::checksum::sh_quote(container),
        p = crate::backend::checksum::sh_quote(path),
    )
}

/// 读文件内容的命令（`docker exec` 跑 `cat`）。
///
/// ⚠️ **大小限制在客户端做**（先看目录列表里的大小，太大就不读）⇒
///   远端不会白吐几百兆。
pub fn read_file_cmd(container: &str, path: &str) -> String {
    format!(
        "docker exec {c} cat {p} 2>&1",
        c = crate::backend::checksum::sh_quote(container),
        p = crate::backend::checksum::sh_quote(path),
    )
}

/// **上传**的命令（本地 → 容器）：**`docker cp` 的归档流模式**。
///
/// ⚠️ **`docker cp - 容器:目录`** ⇒ **从 stdin 读 tar 归档** ⇒ 落到那个
///   目录里。
/// ⚠️ **目标必须是目录**（docker 的规定，2026-11-16 实测）：给文件路径会报
///   「destination ... must be a directory」⇒ **文件名由 tar 归档里的路径
///   决定** ⇒ 上传一个文件 ⇒ tar 里有 `名字` ⇒ 落到 `<目录>/<名字>`。
/// ⚠️ **不套 pty** ⇒ 字节流是原样的（pty 会把 `\n` 翻成 `\r\n` ⇒ 二进制就废了）。
pub fn cp_upload_cmd(container: &str, dir: &str) -> String {
    // ⚠️ **整段引号包住**（`容器:路径` 是**一个**参数 ⇒ 一个词）⇒ 名字或
    //   路径里有空格/分号都不会被拆开、更不会被执行。
    let target = format!("{container}:{}/", dir.trim_end_matches('/'));
    format!(
        "docker cp - {t}",
        t = crate::backend::checksum::sh_quote(&target),
    )
}

/// **下载**的命令（容器 → 本地）：**`docker cp` 的归档流模式**。
///
/// ⚠️ **`docker cp 容器:路径 -`** ⇒ **往 stdout 写 tar 归档** ⇒ 本地解开
///   就得到 `<本地目录>/<名字>`（归档的根就是 basename ⇒ 不用 strip）。
/// ⚠️ **stderr 必须隔离**（`2>/dev/null`）⇒ 报错信息不会混进归档流里
///   （混进去 ⇒ 本地 tar 解不开 ⇒ 下载白费）。
pub fn cp_download_cmd(container: &str, path: &str) -> String {
    // ⚠️ **整段引号包住**（同 `cp_upload_cmd`）。
    let target = format!("{container}:{path}");
    format!(
        "docker cp {t} - 2>/dev/null",
        t = crate::backend::checksum::sh_quote(&target),
    )
}

/// 容器里一个文件的**类型**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileKind {
    /// 目录（`-d`）。
    Dir,
    /// 普通文件（`-`）。
    File,
    /// 符号链接（`-l`）。
    Link,
    /// 其它（字符设备 / 块设备 / 管道 / socket）。
    Other,
    /// **不是目录条目**（解析失败 ⇒ 多半是错误信息）。
    #[default]
    Unknown,
}

impl FileKind {
    /// 界面上的**中文名**。
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Dir => "目录",
            FileKind::File => "文件",
            FileKind::Link => "链接",
            FileKind::Other => "其它",
            FileKind::Unknown => "未知",
        }
    }
}

/// 容器里的一个文件（阶段 116）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainerFile {
    /// **名字**（不含目录）。
    pub name: String,
    /// 大小（字节；目录是 0）。
    pub size: u64,
    /// 类型。
    pub kind: FileKind,
    /// 权限串（如 `drwxr-xr-x`）。
    pub perms: String,
}

impl ContainerFile {
    /// 这个条目的**远端绝对路径**（拼在目录路径后面）。
    pub fn join(&self, dir: &str) -> String {
        if dir.ends_with('/') {
            format!("{dir}{}", self.name)
        } else {
            format!("{dir}/{}", self.name)
        }
    }
}

/// 解析 `ls -lA` 的输出 ⇒ 文件列表（**纯函数**）。
///
/// ⚠️ **一行怎么读**（`ls -l` 的字段）：
/// ```text
/// -rw-r--r-- 1 root root  1234 Nov 15 19:33 my file.txt
/// drwxr-xr-x 2 root root  4096 Nov 15 19:33 subdir
/// ```
/// - 第 1 个字段 = **权限串**（第 1 个字符 ⇒ 类型：`d` 目录 / `-` 文件 /
///   `l` 链接）；
/// - 第 5 个字段 = **大小**；
/// - 第 9 个字段起 = **名字**（日期是 3 段 ⇒ 名字从第 9 段开始 ⇒
///   **名字里有空格也不怕**）。
/// ⚠️ **符号链接**：`name -> target` ⇒ 只取 `->` 之前的名字。
/// ⚠️ **认不出来就丢掉**（docker 的错误信息混在 stdout 里 ⇒ 不能当数据）。
pub fn parse_ls(out: &str) -> Vec<ContainerFile> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        // ⚠️ **至少 9 段**（权限 + 链接数 + 属主 + 属组 + 大小 + 日期 3 段 + 名字）
        if f.len() < 9 {
            continue;
        }
        let perms = f[0];
        // ⚠️ **权限串的合法字符**：`-dlcbps` 开头
        let kind = match perms.chars().next() {
            Some('d') => FileKind::Dir,
            Some('-') => FileKind::File,
            Some('l') => FileKind::Link,
            Some(c) if "cbps".contains(c) => FileKind::Other,
            _ => continue,
        };
        let size: u64 = match f[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // ⚠️ **名字**：第 9 段起全部（含空格）⇒ 再去掉 ` -> 目标`。
        let name_all = f[8..].join(" ");
        let name = match name_all.find(" -> ") {
            Some(i) => name_all[..i].to_string(),
            None => name_all,
        };
        let name = name.trim_matches('"').to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        v.push(ContainerFile {
            name,
            size,
            kind,
            perms: perms.to_string(),
        });
    }
    v
}

/// 列容器里的一个目录（阶段 116）。
///
/// ⚠️ 走**共享连接**上的 exec 通道（不是新连接）⇒ 一台服务器仍然只有
///   一条 SSH 连接。
pub async fn list_dir(
    session: &Handle<HostKeyHandler>,
    container: &str,
    path: &str,
) -> BackendResult<Vec<ContainerFile>> {
    if !valid_name(container) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let (code, out) = run_cmd(session, &list_dir_cmd(container, path)).await?;
    let files = parse_ls(&out);
    if code != 0 || files.is_empty() {
        // ⚠️ **一行都没解析出来** ⇒ 多半是错误信息（容器没跑 / 没权限 /
        //   容器里没有 ls……）⇒ 翻成中文。
        return Err(BackendError(classify_failure(&out)));
    }
    Ok(files)
}

/// 读容器里的一个文件（阶段 116）。
///
/// ⚠️ `max_bytes`：**超了就不读**（免得远端白吐几百兆）⇒ 返回错误说明
///   文件太大。
pub async fn read_file(
    session: &Handle<HostKeyHandler>,
    container: &str,
    path: &str,
    max_bytes: u64,
) -> BackendResult<String> {
    if !valid_name(container) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let (code, out) = run_cmd(session, &read_file_cmd(container, path)).await?;
    if code != 0 {
        return Err(BackendError(classify_failure(&out)));
    }
    if out.len() as u64 > max_bytes {
        return Err(BackendError(
            trf!(
                "文件太大（{n} 字节），没读进来（上限 {m} 字节）",
                n = out.len(),
                m = max_bytes
            ),
        ));
    }
    Ok(out)
}

/* ---------------- 阶段 116：上传 / 下载（流式） ---------------- */

/// 进度回调的类型（**跨线程** ⇒ 要 `Send + Sync`）。
pub type XferProgress = std::sync::Arc<dyn Fn(u64) + Send + Sync>;

/// **多久没有进展就判定卡住**（秒）。
///
/// ⚠️ **为什么要有看门狗**：远端命令要是**不退出**（比如 `cat >` 一直没
///   收到 EOF）⇒ 等待方会**永远挂着** ⇒ 界面上一直显示「正在上传…」⇒
///   而且**再也传不了别的**（那个「正在传」的标志一直不清）⇒ 必须能
///   自己发现「卡住了」并报出来。
pub const XFER_STALL_SECS: u64 = 120;

/// 现在的毫秒数（判「多久没进展」用）。
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 把远端（容器）的一个流**写进本地文件**（**私有辅助**）。
///
/// ⚠️ **退出码非 0 ⇒ 失败**（`docker exec` 的退出码 = 里面命令的退出码）。
/// ⚠️ **stderr 已经隔离**（命令里写了 `2>/dev/null`）⇒ 不会把错误信息
///   混进数据流里。
async fn stream_to_file(
    session: &Handle<HostKeyHandler>,
    cmd: &str,
    dest: &std::path::Path,
    progress: &XferProgress,
) -> BackendResult<u64> {
    use std::io::Write;
    let ch = ShellChannel::open_exec(session, cmd, 80, 24, false).await?;
    let mut f = std::fs::File::create(dest)
        .map_err(|e| BackendError(trf!("写本地文件失败：{e}", e = e)))?;
    let mut total: u64 = 0;
    // ⚠️ **看门狗**：多久没进展就判「卡住」（见 `XFER_STALL_SECS`）。
    let last = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(now_millis()));
    let last2 = std::sync::Arc::clone(&last);
    let user = std::sync::Arc::clone(progress);
    let wrapped: XferProgress = std::sync::Arc::new(move |n: u64| {
        last2.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
        user(n);
    });
    loop {
        let ev = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ch.next_event(),
        )
        .await
        {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => {
                let idle = now_millis() - last.load(std::sync::atomic::Ordering::Relaxed);
                if idle > XFER_STALL_SECS * 1000 {
                    return Err(BackendError(tr("传输卡住了（长时间没有进展）").to_string()));
                }
                continue;
            }
        };
        match ev {
            ShellEvent::Data(b) => {
                f.write_all(&b)
                    .map_err(|e| BackendError(trf!("写本地文件失败：{e}", e = e)))?;
                total += b.len() as u64;
                wrapped(total);
            }
            ShellEvent::Exit(code) => {
                if code != 0 {
                    return Err(BackendError(
                        trf!("下载失败（远程命令退出码 {c}）", c = code),
                    ));
                }
                break;
            }
        }
    }
    Ok(total)
}

/// **下载**：把容器里的文件/目录下载到**本地目录** `local_dir`。
///
/// ⚠️ **走 `docker cp` 的归档流**：远端 `docker cp <容器>:<路径> -` ⇒
///   stdout 就是一个 tar 归档 ⇒ 落本地临时文件 ⇒ 本地 `tar -xf` 解开。
/// ⚠️ **归档的根就是 basename** ⇒ 解到 `local_dir` ⇒ 得到
///   `local_dir/<名字>`（**文件与目录都一样** ⇒ 不用 strip-components）。
/// ⚠️ **返回传输的字节数**（进度用）。
pub async fn download_to(
    session: &Handle<HostKeyHandler>,
    container: &str,
    remote_path: &str,
    local_dir: &std::path::Path,
    progress: &XferProgress,
) -> BackendResult<u64> {
    if !valid_name(container) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let name = crate::util::path_basename(remote_path).to_string();
    if name.is_empty() {
        return Err(BackendError(tr("这个条目没有名字").to_string()));
    }
    // ⚠️ **归档先落本地临时文件**（流式解包要自己写 tar 解析器 ⇒ 不值当）。
    let tmp = local_dir.join(format!(".{name}.pp-cp.tar"));
    let total = match stream_to_file(
        session,
        &cp_download_cmd(container, remote_path),
        &tmp,
        progress,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            // ⚠️ **失败也要清掉临时文件**（不然本地攒一堆垃圾）。
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let out = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tmp)
        .arg("-C")
        .arg(local_dir)
        .output()
        .map_err(|e| BackendError(trf!("本机没有 tar 命令，解不开：{e}", e = e)))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        return Err(BackendError(trf!(
            "解开下载的内容失败：{e}",
            e = String::from_utf8_lossy(&out.stderr).trim().to_string()
        )));
    }
    Ok(total)
}

/// **上传**：把**本地**的文件/目录传到容器的 `remote_dir` 目录下。
///
/// ⚠️ **走 `docker cp` 的归档流**：本地 `tar -cf - -C <父> <名字>` ⇒ 字节流
///   直接喂给远端 `docker cp - 容器:<目录>/` 的 stdin ⇒ Docker 自己解包。
/// ⚠️ **本地不落临时文件**（直接把 tar 的 stdout 喂进通道）⇒ 大目录也不会
///   把本地磁盘撑爆。
/// ⚠️ **进度**：按**已写出去的字节数**报（归档字节数）。
pub async fn upload_from(
    session: &Handle<HostKeyHandler>,
    container: &str,
    local_src: &std::path::Path,
    remote_dir: &str,
    progress: &XferProgress,
) -> BackendResult<()> {
    if !valid_name(container) {
        return Err(BackendError(tr("容器名不合法").to_string()));
    }
    let name = local_src
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(BackendError(tr("这个本地条目没有名字").to_string()));
    }
    // ⚠️ **本地打成 tar 归档**：`docker cp` 的 stdin 模式**只认 tar**
    //   （文件与目录都行 ⇒ tar 那一层不用分情况）。
    let parent = local_src.parent().unwrap_or(std::path::Path::new("."));
    let mut child = std::process::Command::new("tar")
        .arg("-cf")
        .arg("-")
        .arg("-C")
        .arg(parent)
        .arg(&name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| BackendError(trf!("本机没有 tar 命令，打不了包：{e}", e = e)))?;
    let reader = child
        .stdout
        .take()
        .ok_or_else(|| BackendError(tr("本地打包失败：拿不到 tar 的输出").to_string()))?;

    let ch = ShellChannel::open_exec(
        session,
        &cp_upload_cmd(container, remote_dir),
        80,
        24,
        false,
    )
    .await?;
    // ⚠️ **看门狗**：多久没进展就判「卡住」（见 `XFER_STALL_SECS`）。
    let last = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(now_millis()));
    let last2 = std::sync::Arc::clone(&last);
    let user = std::sync::Arc::clone(progress);
    let wrapped: XferProgress = std::sync::Arc::new(move |n: u64| {
        last2.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
        user(n);
    });
    ch.send_reader_and_eof(reader, 32 * 1024, wrapped);

    // ⚠️ **等退出码**（EOF 不算结束 ⇒ 见 `next_event` 的说明）。
    let mut code = 0;
    let mut stalled = false;
    loop {
        let ev = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ch.next_event(),
        )
        .await
        {
            Ok(Some(ev)) => ev,
            Ok(None) => break,
            Err(_) => {
                let idle = now_millis() - last.load(std::sync::atomic::Ordering::Relaxed);
                if idle > XFER_STALL_SECS * 1000 {
                    stalled = true;
                    break;
                }
                continue;
            }
        };
        if let ShellEvent::Exit(c) = ev {
            code = c;
            break;
        }
    }
    // ⚠️ **tar 自己也要正常退出**（否则归档是截断的 ⇒ docker 那边会报错）。
    let tar_ok = matches!(child.wait(), Ok(st) if st.success());
    if stalled {
        return Err(BackendError(tr("传输卡住了（长时间没有进展）").to_string()));
    }
    if !tar_ok {
        return Err(BackendError(tr("本地打包失败（tar 没能正常结束）").to_string()));
    }
    if code != 0 {
        return Err(BackendError(
            trf!("上传失败（远程命令退出码 {c}）", c = code),
        ));
    }
    Ok(())
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

    /// 容器已经被删掉时，错误要说清楚（别把英文原文甩给用户）。
    ///
    /// ⚠️ **这条为什么现在才有用**：退出码修好之前（2026-11-16 之前）
    ///   `run_cmd` 的 `code` 恒为 0 ⇒ **根本走不到 `classify_failure`**
    ///   ⇒ 这条分支是死代码。
    #[test]
    fn 容器不在了的错误能认出来() {
        let s = classify_failure("Error response from daemon: No such container: web");
        assert!(
            s.contains("容器可能已经被删掉了"),
            "认错了，得到的是 {s}"
        );
    }

    /// 守护进程没起时，错误要说清楚（这条分支同样只有退出码修好后才有意义）。
    #[test]
    fn 守护进程没起的错误能认出来() {
        let s = classify_failure(
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.",
        );
        assert!(s.contains("守护进程"), "认错了，得到的是 {s}");
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

    /* ---------------- 阶段 113：容器内终端 ---------------- */

    /// 探测命令：**不**分配 pty（探路不该要终端）。
    #[test]
    fn 探测命令不分配pty() {
        let c = shell_probe_cmd("web", "/bin/sh");
        assert!(c.contains("docker exec"), "应当是 docker exec：{c}");
        assert!(!c.contains("-it"), "探测不该分配 pty：{c}");
        assert!(
            c.contains("'web'") && c.contains("'/bin/sh'"),
            "参数没包引号：{c}"
        );
        // ⚠️ 用 shell 内建的 `exit`（不要 `true` 那种二进制）。
        assert!(c.contains("exit 0"), "探测命令不对：{c}");
    }

    /// 终端命令：**要** `-it`（容器里的 shell 得有个终端才是交互式的）。
    #[test]
    fn 终端命令带it() {
        let c = exec_shell_cmd("web", "/bin/sh");
        assert!(c.contains("docker exec -it"), "应当带 -it：{c}");
        assert!(
            c.contains("'web'") && c.contains("'/bin/sh'"),
            "参数没包引号：{c}"
        );
    }

    /// ⚠️ **容器名是远端可控的内容** ⇒ 必须引起来（防注入）。
    #[test]
    fn 终端命令防注入() {
        let c = exec_shell_cmd("a; rm -rf /", "/bin/sh");
        assert!(c.contains("'a; rm -rf /'"), "容器名没被引号包住：{c}");
        let c2 = shell_probe_cmd("a; rm -rf /", "/bin/sh");
        assert!(c2.contains("'a; rm -rf /'"), "容器名没被引号包住：{c2}");
    }

    /// 挨个试的顺序：先 `/bin/sh`（几乎所有镜像都有 ⇒ 命中率最高）。
    #[test]
    fn shell候选按命中率排序() {
        assert_eq!(SHELL_CANDIDATES[0], "/bin/sh");
        for s in ["/bin/bash", "/bin/ash", "/bin/zsh"] {
            assert!(SHELL_CANDIDATES.contains(&s), "候选里少了 {s}");
        }
    }

    /* ---------------- 阶段 119：点名要某个 shell ---------------- */

    /// 点名的 shell **没有** ⇒ 报错要**点名说是哪个**、还要给出**能换的**。
    #[test]
    fn 点名的shell没有时报错说清楚() {
        let m = shell_probe_error_msg("/bin/zsh", "OCI runtime exec failed: exec: \"/bin/zsh\": not found");
        assert!(m.contains("/bin/zsh"), "报错里要点名是哪个 shell：{m}");
        assert!(
            m.contains("/bin/sh") && m.contains("/bin/bash"),
            "报错要列出能换的：{m}"
        );
    }

    /// ⚠️ **容器没在运行** ⇒ 别说「没有这个 shell」（用户会照着改 shell 名）。
    #[test]
    fn 容器没运行时说的是那件事() {
        let m = shell_probe_error_msg("/bin/bash", "Error response from daemon: Container abc is not running");
        assert!(
            m.contains("没有在运行"),
            "应当说容器没在运行：{m}"
        );
        assert!(!m.contains("/bin/bash"), "别扯到 shell 名上：{m}");
    }

    /// ⚠️ **账号没权限** ⇒ 同样单独说（这是**账号**的问题，不是 shell 的）。
    #[test]
    fn 没权限时说的是那件事() {
        let m = shell_probe_error_msg("/bin/bash", "permission denied while trying to connect to the Docker daemon socket");
        assert!(
            m.contains("权限"),
            "应当说权限问题：{m}"
        );
        assert!(!m.contains("/bin/bash"), "别扯到 shell 名上：{m}");
    }

    /* ---------------- 阶段 114：监控曲线 ---------------- */

    /// 采样命令：**只盯一个容器** ＋ **有看门狗**。
    #[test]
    fn 采样命令只盯一个容器() {
        let c = stats_stream_cmd("web");
        assert!(c.contains("docker stats"), "应当是 docker stats：{c}");
        assert!(c.contains("'web'"), "容器名没被引号包住：{c}");
        // ⚠️ **看门狗必须在位**（阶段 112 的教训：通道断了远端会留进程）。
        assert!(c.contains("read -r _"), "看门狗不在：{c}");
        assert!(c.contains("kill $p"), "看门狗没杀：{c}");
    }

    /// ⚠️ **容器名是远端可控的内容** ⇒ 必须引起来（防注入）。
    #[test]
    fn 采样命令防注入() {
        // ⚠️ 这里故意用一个「能拼出危险命令」的名字 ⇒ 只能作为**数据**。
        let evil = format!("a; {} {}", "rm", "-rf /");
        let c = stats_stream_cmd(&evil);
        assert!(
            c.contains(&format!("'{evil}'")),
            "容器名没被引号包住：{c}"
        );
    }

    /// docker stats 的持续模式会带 **ANSI 转义** ⇒ 要剥干净。
    #[test]
    fn ansi转义能剥干净() {
        let raw = "\u{1b}[H2.08%|0.08%|6.14kB / 126B|40.4MB / 242kB\u{1b}[K";
        assert_eq!(
            strip_ansi(raw),
            "2.08%|0.08%|6.14kB / 126B|40.4MB / 242kB"
        );
        // 光标移动 + 清行 + 清屏都要能剥掉。
        assert_eq!(strip_ansi("\u{1b}[J\u{1b}[K"), "");
        // 普通的文本不能被误伤。
        assert_eq!(strip_ansi("hello"), "hello");
    }

    /// 一行采样 ⇒ 四个指标。
    #[test]
    fn 采样行能解析() {
        let s = parse_stats_line("2.08%|0.08%|6.14kB / 126B|40.4MB / 242kB").expect("应当解析得出");
        assert!((s.cpu - 2.08).abs() < 0.001, "CPU 不对：{}", s.cpu);
        assert!((s.mem - 0.08).abs() < 0.001, "内存不对：{}", s.mem);
        assert_eq!(s.net_rx, 6140, "网络接收不对：{}", s.net_rx);
        assert_eq!(s.net_tx, 126, "网络发送不对：{}", s.net_tx);
        assert_eq!(s.blk_read, 40_400_000, "磁盘读不对：{}", s.blk_read);
        assert_eq!(s.blk_write, 242_000, "磁盘写不对：{}", s.blk_write);
    }

    /// 表头 / 空行 / 半截行 ⇒ **丢掉**（不能画成 0）。
    #[test]
    fn 认不出来的行丢掉() {
        assert!(parse_stats_line("").is_none(), "空行不该解析出来");
        assert!(parse_stats_line("   ").is_none(), "空白行不该解析出来");
        assert!(parse_stats_line("NAME|CPU").is_none(), "半截行不该解析出来");
        assert!(parse_stats_line("\u{1b}[K").is_none(), "只有转义符不该解析出来");
    }

    /// 大小单位：**SI**（1000）与**二进制**（1024）都要认。
    #[test]
    fn 大小单位两种都认() {
        assert_eq!(parse_size_bytes("0B"), Some(0));
        assert_eq!(parse_size_bytes("126B"), Some(126));
        assert_eq!(parse_size_bytes("6.14kB"), Some(6140));
        assert_eq!(parse_size_bytes("40.4MB"), Some(40_400_000));
        assert_eq!(parse_size_bytes("1.5GB"), Some(1_500_000_000));
        assert_eq!(parse_size_bytes("1KiB"), Some(1024));
        assert_eq!(parse_size_bytes("2MiB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size_bytes(""), None);
    }

    /// 累计值 ⇒ **差分**成速率（字节/秒）。
    #[test]
    fn 累计值差分成速率() {
        let a = StatsSample {
            net_rx: 1000,
            net_tx: 2000,
            blk_read: 3000,
            blk_write: 4000,
            ..Default::default()
        };
        let b = StatsSample {
            net_rx: 3000,
            net_tx: 2000,
            blk_read: 3000,
            blk_write: 6000,
            ..Default::default()
        };
        let r = rate_between(&a, &b, 2.0).expect("应当算得出速率");
        assert!((r.0 - 1000.0).abs() < 0.001, "网络接收速率不对：{}", r.0);
        assert!((r.1 - 0.0).abs() < 0.001, "网络发送速率不对：{}", r.1);
        assert!((r.2 - 0.0).abs() < 0.001, "磁盘读速率不对：{}", r.2);
        assert!((r.3 - 1000.0).abs() < 0.001, "磁盘写速率不对：{}", r.3);
    }

    /// ⚠️ **累计值变小** ⇒ 容器重启过（计数归零）⇒ 这一次**丢掉**
    ///   （否则会画出一根巨大的尖峰）。
    #[test]
    fn 容器重启后不画尖峰() {
        let a = StatsSample {
            net_rx: 5000,
            ..Default::default()
        };
        let b = StatsSample {
            net_rx: 10,
            ..Default::default()
        };
        assert!(rate_between(&a, &b, 2.0).is_none(), "不该算出速率");
    }

    /// 节流器：**每 N 秒出一个点**（docker stats 每 500ms 刷一次）。
    #[test]
    fn 采样每N秒出一个点() {
        let mut s = StatsSampler::default();
        let raw = StatsSample {
            cpu: 1.0,
            mem: 2.0,
            net_rx: 1000,
            ..Default::default()
        };
        // 第一次：出点（速率还没有 ⇒ 0）。
        let p = s.push(&raw, 0.0, 2.0).expect("第一次应当出点");
        assert!((p.cpu - 1.0).abs() < 0.001, "CPU 不对：{}", p.cpu);
        assert!((p.net_rx - 0.0).abs() < 0.001, "第一次不该有速率：{}", p.net_rx);
        // 1 秒时：**不出点**（还没到周期）。
        assert!(s.push(&raw, 1.0, 2.0).is_none(), "1 秒时不该出点");
        // 2 秒时：出点。
        //
        // ⚠️ **速率是按「上一个原始样本」差分的**（不是上一个「出点」）
        //   ⇒ 上一个原始样本在 t=1.0（net_rx=1000）⇒ 这一秒里多了 2000
        //   ⇒ 速率 = 2000 字节/秒。
        let raw2 = StatsSample {
            cpu: 3.0,
            mem: 4.0,
            net_rx: 3000,
            ..Default::default()
        };
        let p2 = s.push(&raw2, 2.0, 2.0).expect("2 秒时应当出点");
        assert!((p2.cpu - 3.0).abs() < 0.001, "CPU 不对：{}", p2.cpu);
        assert!((p2.net_rx - 2000.0).abs() < 0.001, "速率不对：{}", p2.net_rx);
    }

    /* ---------------- 阶段 115：镜像 / 卷 / 网络 ---------------- */

    #[test]
    fn 镜像命令列出五个字段() {
        let c = super::images_cmd();
        assert!(c.contains("docker images"), "应当是 docker images：{c}");
        assert!(c.contains("{{.Repository}}"), "应当带仓库：{c}");
        assert!(c.contains("{{.Tag}}"), "应当带标签：{c}");
        assert!(c.contains("{{.Size}}"), "应当带大小：{c}");
        assert!(c.contains("{{.CreatedAt}}"), "应当带创建时间：{c}");
    }

    #[test]
    fn 卷命令和网络命令对() {
        let v = super::volumes_cmd();
        assert!(v.contains("docker volume ls"), "应当是 docker volume ls：{v}");
        assert!(v.contains("{{.Name}}"), "应当带名字：{v}");
        let n = super::networks_cmd();
        assert!(n.contains("docker network ls"), "应当是 docker network ls：{n}");
        assert!(n.contains("{{.Driver}}"), "应当带驱动：{n}");
        assert!(n.contains("{{.Scope}}"), "应当带范围：{n}");
    }

    #[test]
    fn 镜像行能解析() {
        let out = "postgis/postgis|17-3.5|34548bd0e2f1|1.23GB|3 days ago\n";
        let v = super::parse_images(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].repository, "postgis/postgis");
        assert_eq!(v[0].tag, "17-3.5");
        assert_eq!(v[0].id, "34548bd0e2f1");
        assert_eq!(v[0].size, "1.23GB");
        assert_eq!(v[0].created, "3 days ago");
    }

    #[test]
    fn 悬空镜像也认() {
        // docker 对没有仓库/标签的镜像给 `<none>`
        let out = "<none>|<none>|abc123def456|88.8MB|2 weeks ago\n";
        let v = super::parse_images(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].repository, "<none>");
        assert_eq!(v[0].tag, "<none>");
    }

    #[test]
    fn 卷行和网络行能解析() {
        let v = super::parse_volumes("myproj_pgdata|local\nother|local\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "myproj_pgdata");
        assert_eq!(v[0].driver, "local");
        let n = super::parse_networks("bridge|bridge|local\nmyproj_default|bridge|local\n");
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].name, "bridge");
        assert_eq!(n[0].driver, "bridge");
        assert_eq!(n[0].scope, "local");
        assert_eq!(n[1].name, "myproj_default");
    }

    #[test]
    fn 坏行丢掉() {
        // docker 报错时也会往 stdout 吐东西 ⇒ 字段不够的行不能当成数据
        let v = super::parse_images("Cannot connect to the Docker daemon\n");
        assert!(v.is_empty(), "错误信息不该被当成镜像：{v:?}");
        let vols = super::parse_volumes("bad\n");
        assert!(vols.is_empty(), "字段不够的行不该被当成卷：{vols:?}");
        let nets = super::parse_networks("a|b\n");
        assert!(nets.is_empty(), "字段不够的行不该被当成网络：{nets:?}");
    }


    /* ---------------- 阶段 116：容器内文件浏览 ---------------- */

    #[test]
    fn 列目录命令走docker_exec() {
        let c = super::list_dir_cmd("web", "/etc");
        assert!(c.contains("docker exec"), "应当走 docker exec：{c}");
        assert!(c.contains("ls -lA"), "应当是 ls -lA：{c}");
        assert!(c.contains("'/etc'"), "路径要引号包住：{c}");
    }

    #[test]
    fn 容器名与路径都防注入() {
        // ⚠️ 名字带引号/分号 ⇒ 必须被引号包住 ⇒ 不能拼成命令
        let c = super::list_dir_cmd("a'; touch /tmp/pwned; echo '", "/tmp/x");
        let expect = "'a'\\''; touch /tmp/pwned; echo '\\''";
        assert!(c.contains(expect), "容器名没转义：{c}");
        assert!(c.contains("'/tmp/x'"), "路径没转义：{c}");
    }

    #[test]
    fn ls输出能解析() {
        let out = "total 8\n\
-rw-r--r-- 1 root root  1234 Nov 15 19:33 config.yaml\n\
drwxr-xr-x 2 root root  4096 Nov 15 19:30 conf.d\n";
        let v = super::parse_ls(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "config.yaml");
        assert_eq!(v[0].size, 1234);
        assert_eq!(v[0].kind, super::FileKind::File);
        assert_eq!(v[0].perms, "-rw-r--r--");
        assert_eq!(v[1].name, "conf.d");
        assert_eq!(v[1].kind, super::FileKind::Dir);
    }

    #[test]
    fn 文件名里有空格也不怕() {
        let out = "-rw-r--r-- 1 root root   42 Nov 15 19:33 my notes.txt\n";
        let v = super::parse_ls(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "my notes.txt");
    }

    #[test]
    fn 符号链接只取名字() {
        let out = "lrwxrwxrwx 1 root root    7 Nov 15 19:33 link.txt -> /etc/real.txt\n";
        let v = super::parse_ls(out);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "link.txt");
        assert_eq!(v[0].kind, super::FileKind::Link);
    }

    #[test]
    fn 错误信息不当文件() {
        // docker 的错误信息混在 stdout 里 ⇒ 不能当数据
        let v = super::parse_ls("Error: No such container: web\n");
        assert!(v.is_empty(), "错误信息不该被当成文件：{v:?}");
        let v2 = super::parse_ls("total 0\n");
        assert!(v2.is_empty(), "表头不该被当成文件：{v2:?}");
    }

    #[test]
    fn 文件类型有中文名() {
        assert_eq!(super::FileKind::Dir.label(), "目录");
        assert_eq!(super::FileKind::File.label(), "文件");
        assert_eq!(super::FileKind::Link.label(), "链接");
    }

    #[test]
    fn 路径拼接不丢分隔符() {
        let f = super::ContainerFile {
            name: "a.txt".into(),
            ..Default::default()
        };
        assert_eq!(f.join("/etc"), "/etc/a.txt");
        assert_eq!(f.join("/etc/"), "/etc/a.txt");
    }

    #[test]
    fn 上传走cp归档流目标是目录() {
        // ⚠️ **`docker cp -` 的目标必须是目录**（文件名由 tar 决定）。
        let u = super::cp_upload_cmd("web", "/tmp/dir");
        assert!(u.starts_with("docker cp - "), "上传应当走 docker cp：{u}");
        assert!(
            u.contains("'web:/tmp/dir/'"),
            "目标应当是「容器:目录/」（整段一个词）：{u}"
        );
    }

    #[test]
    fn 下载走cp归档流且隔离stderr() {
        // ⚠️ **stderr 必须隔离**（`2>/dev/null`）⇒ 报错不会混进归档流
        //   （混进去 ⇒ 本地 tar 解不开 ⇒ 下载白费）。
        let d = super::cp_download_cmd("web", "/etc/conf.d");
        assert!(d.contains("docker cp "), "下载应当走 docker cp：{d}");
        assert!(
            d.contains("'web:/etc/conf.d'"),
            "路径没写对（应当整段一个词）：{d}"
        );
        assert!(d.contains(" - "), "应当输出到 stdout：{d}");
        assert!(d.contains("2>/dev/null"), "stderr 必须隔离：{d}");
    }

    #[test]
    fn 下载上传命令防注入() {
        // ⚠️ **容器名与路径都来自界面**（路径栏是用户能改的）⇒ 必须转义。
        let d = super::cp_download_cmd("a'; touch /tmp/pwned; echo '", "/tmp/x");
        assert!(
            d.contains("'a'\\''; touch /tmp/pwned; echo '\\'':/tmp/x'"),
            "容器名没转义（应当整段一个词）：{d}"
        );
        let u = super::cp_upload_cmd("web", "/tmp/a b.txt");
        assert!(
            u.contains("'web:/tmp/a b.txt/'"),
            "路径没转义（应当整段一个词）：{u}"
        );
    }


}
