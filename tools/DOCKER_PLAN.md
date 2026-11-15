# 远程容器（Docker）运行情况显示 —— 规划（阶段 109 起）

> **依据**：用户 2026-11-15：「我们要在软件里面显示**远程服务器上的容器运行
> 情况**」；参考 dbx 的 Docker 功能截图（8 张，桌面
> `截图_选择区域_2026111510*.png`；用户说的 `/home/fwt/Desktop/docker` 文件夹
> 当时是空的，图还在桌面上）。
>
> **本文只是规划，还没动手**。每个阶段一个提交，阶段结束跑全量回归
> （`cargo test --lib` + `cargo check --locked --examples` + `cli_contract_check.sh`
> + 涉及的冒烟），然后出一次 release 产物。
>
> **通用验收**：
> - 新逻辑要有**单测**（纯函数：命令拼装 / 输出解析）或**冒烟**（真 GTK 行为）；
> - ⚠️ **新断言要验证过它真抓得住**（临时回退修复跑一遍，确认红）；
> - 界面改动要有**截图**。

---

## 一、dbx 的 Docker 功能拆解（从截图看）

### 1. 容器列表（主视图）

```
顶栏：Docker 页签 + 「AI/MCP 权限·关闭」 + 设置/帮助/终端图标
子标签：容器 | 镜像 | 卷 | 网络
工具条：搜索框 | + 创建容器 | 全部·运行中·已停止 | 上次刷新 10:58:26 | 自动刷新(7秒) | 刷新
表格：  名称 | ID | 镜像 | 端口 | CPU | 内存 | 状态 | 操作
        ├─ ● postgres   34548bd9147c  postgis/postgis:17-3.5  5432→5432  2.3%  51MB/63GB  运行中  ▤↻□✎详情
        └─ ● eng-tut-server  200f9d6b59be  golang:1.25.2-alpine  —  —  —  已停止  ▷🗑✎详情
```

- 按 **compose 项目分组**（`postgres` 组下的容器缩进；组标题旁有「编辑 Compose」）。
- 状态点：绿＝运行中、灰＝已停止；徽章文字「运行中」「已停止」。
- 行操作：**日志 / 重启 / 停止 / 编辑 / 详情**（不同状态显示不同图标）。

### 2. 容器详情（五个子标签）

| 子标签 | 内容 |
|---|---|
| **概览** | 头部卡（图标、名称、镜像、ID、创建时间、运行中徽章、`Up 2 hours (healthy)`、健康检查 healthy）；**运行配置**（重启策略、重启次数、运行用户、工作目录、启动时间、命令）；**网络与端口**（端口映射、网络名 + 容器 IP）；**挂载**（bind / volume → 容器内路径 + 只读/读写）；**环境变量**（13 个，可展开） |
| **监控** | 4 个指标卡（CPU %、内存、网络吞吐 ↓/↑、磁盘 IO 读/写）+ 4 张折线图（CPU%、内存、网络吞吐、磁盘 IO），**每 2 秒采样** |
| **日志** | 搜索框 + 暂停 + 自动跟随最新日志 + 清空；黑底等宽日志流 |
| **文件** | 容器内文件系统浏览（左目录树 + 右预览）+ 上传文件 + 刷新 |
| **终端** | 容器内 shell（`/bin/sh` 可切 sh/bash/ash/zsh）+ 重连 + 清屏 + 搜索输出 |

### 3. 镜像 / 卷 / 网络

- 镜像：`仓库/标签 | ID | 大小 | 创建时间 | 推送·导出·打标签·删除`
- 卷 / 网络：同类列表（名字、驱动、大小、挂载点、容器数等）。

---

## 二、我们的架构约束（**决定了怎么做**，别照抄 dbx）

| 约束 | 含义 |
|---|---|
| **一台服务器一条 SSH 连接**（连接中枢 `backend/hub.rs`） | 容器视图**不能自己开连接**。要在**共享连接**上开 **exec 通道** ⇒ 复用 `hub::acquire()` 拿 `SharedConn` ⇒ `shared.session()` ⇒ `ShellChannel::open_exec(...)`（**已有**，终端/分屏在用） |
| **界面在连接页的子标签区** | 容器视图做成**子标签**（和「数据库」工作台平级）⇒ 不新增主标签（主标签＝一个会话） |
| **不引入新依赖** | 仓库里**没有 serde_json**，也不打算加 ⇒ 用 `docker --format` 的 **Go 模板**直接吐我们要的字段，**不解析 JSON** |
| **用户可见文案全中文** | 状态、错误、提示都中文（Rust-only 功能，不涉及 C 版对齐） |
| **别整表重画** | 项目踩过「整表重画把主线程拖死」的坑（上传卡死事件）⇒ 列表用 `ListStore` **差量更新** |
| **别在用户机器装东西** | 容器命令全在**远端**跑；本机不需要 docker |
| **不自动 sudo** | 远端权限不足时**给中文提示**，不擅自 sudo（会要密码、会留痕迹） |

---

## 三、技术方案

### 3.1 后端：`src/backend/docker.rs`（新模块）

一条命令 = 一条 exec 通道（通道很轻，**不是新连接**）：

```rust
// 走共享连接 ⇒ 一台服务器一条 SSH 连接这条硬约束不受影响
let shared = hub::acquire(ServerKey::new(host, port, user), &creds).await?;
let session = shared.session().clone();
let ch = ShellChannel::open_exec(session.as_ref(), &cmd, 80, 24, false).await?;
// 读 stdout 直到 Exit(退出码)
```

**命令清单**（全部用 Go 模板输出，**不解析 JSON**）：

| 用途 | 命令 |
|---|---|
| 探路 | `command -v docker` → `docker version --format '{{.Server.Version}}'` |
| 列表 | `docker ps -a --format '{{.ID}}\t{{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}\t{{.Label "com.docker.compose.project"}}'` |
| 资源占用 | `docker stats --no-stream --format '{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}'` |
| 详情 | `docker inspect --format '{{.Name}}\t{{.Config.Image}}\t{{.Created}}\t{{.State.Status}}\t{{.State.StartedAt}}\t{{.RestartCount}}\t{{.HostConfig.RestartPolicy.Name}}\t{{.Config.User}}\t{{.Config.WorkingDir}}\t{{.Path}}\t{{.Args}}' <id>` |
| 端口/挂载/环境 | `docker inspect --format '{{range .NetworkSettings.Ports}}…{{end}}'` / `{{range .Mounts}}…{{end}}` / `{{range .Config.Env}}…{{end}}` |
| 日志 | `docker logs -f --tail 200 <id>`（长跑 ⇒ 独立通道 + 流式读） |
| 终端 | `docker exec -it <id> /bin/sh`（**复用现成的终端控件**） |
| 启停 | `docker start / stop / restart <id>`（**写操作 ⇒ 二次确认**） |

⚠️ **分隔符用 `\x1f`（单元分隔符）而不是 `\t`**：镜像名/状态里可能有空格和制表符。

### 3.2 前端：`src/panel/dockerui.rs`（新模块）

- 做成**子标签页**（图标 + 「容器」标题），照 `panel/dbui.rs` 的挂载方式
  （`sub.add_page_icon_kind(...)` + `add_close_hook` 收尾）。
- 列表：`gtk::TreeView` + `ListStore`（照 `panel/dbgrid.rs` 的列/排序写法）。
- 详情：在**子标签区里再开一个子页**（像数据库工作台的「对象树 → 详情」），
  详情页里再分「概览 / 日志 / 终端」几个页签。
- 监控曲线：**自己画**（`gtk::DrawingArea` + cairo）——项目里没有图表库
  （`settingsui.rs` 已有 `rounded_rect` 那种 cairo 写法可参考）。
- 日志/终端：复用 `termview::TermView`（进程内 SSH 的 pty 通道已经有）。

### 3.3 刷新

- 自动刷新（默认 7 秒，可关）+ 手动刷新 + 「上次刷新 hh:mm:ss」。
- 命令在**后台 tokio 任务**里跑（`hub::runtime()`）⇒ 结果回主线程更新表格
  ⇒ **不卡 UI**。
- ⚠️ 表格用 `ListStore` 差量更新（insert/remove/changed），**不整表重画**。

---

## 四、阶段清单（按依赖与性价比排序）

### 阶段 109：探路 + 容器列表（**只读**）⭐ 先做

- **对标**：dbx 的容器列表页。
- **改动点**：
  - `backend/docker.rs`：命令拼装 + 输出解析（**纯函数，好测**）；
  - `panel/dockerui.rs`：「容器」子标签页 + 表格 + 工具条；
  - 错误态：远端没装 docker / 不在 docker 组 / 守护进程没起 ⇒ **中文提示 + 怎么办**。
- **验收**：单测（命令拼装 + 解析：含带空格的镜像名、compose 分组、CPU/内存
  字符串解析）+ 冒烟（**假 docker 脚本**造输出 ⇒ 不依赖真 docker）+ 反向验证。

### 阶段 110：容器详情（概览）

- 运行配置 / 网络与端口 / 挂载 / 环境变量（四个卡片）。
- **启停 / 重启**（写操作 ⇒ 二次确认 + 失败给原因）。

### 阶段 111：日志（流式）

- `docker logs -f` ⇒ 独立通道 + 增量刷新；搜索 / 暂停 / 自动跟随 / 清空。

### 阶段 112：容器内终端

- `docker exec -it` ⇒ 复用 `TermView`（进程内 pty）。
- ⚠️ 容器里可能没有 `/bin/sh` ⇒ 依次试 `/bin/sh` → `/bin/bash` → 报「容器里没有可用的 shell」。

### 阶段 113：监控曲线

- 每 2 秒采样 `docker stats --no-stream` ⇒ 4 个指标卡 + 4 张 cairo 折线图。

### 阶段 114：镜像 / 卷 / 网络

- 三个列表（照 dbx 的镜像页）。

### 阶段 115：容器内文件浏览

- `docker exec` 跑 `ls` / `cat` / `tar`（容器内没有 SFTP ⇒ 只能这样）。

---

## 五、风险与坑

1. **权限**：远端用户不在 docker 组 ⇒ `permission denied … /var/run/docker.sock`
   ⇒ 中文提示「当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用 sudo
   登录）」。⚠️ **不自动 sudo**。
2. **没装 docker**：`command -v docker` 空 ⇒ 「远端没有找到 docker」。
3. **守护进程没起**：`Cannot connect to the Docker daemon` ⇒ 「远端的 Docker
   服务没起来」。
4. **老版本 docker**：Go 模板字段有版本门槛（`{{.Label "x"}}` 要 17.06+，
   `docker stats --format` 要 18.09+）⇒ 先探版本 ⇒ 老版本降级到 tab 格式。
5. **字段里有空格/制表符** ⇒ 分隔符用 `\x1f`。
6. **刷新卡 UI**：⇒ 后台跑命令 + 差量更新。
7. **写操作误触** ⇒ 启停/重启二次确认。
8. **Podman**：`podman ps --format` 的模板和 docker **不完全一样** ⇒ 首期只支持
   docker，podman 放到后面（探测到 podman 时给提示即可）。
9. **客户端是 Windows、服务器是 Windows**：没有 docker ⇒ 提示即可（命令在远端跑，
   客户端平台不影响）。

---

## 六、首期**不做**（YAGNI）

- 「+ 创建容器」：要一整套镜像/端口/卷/环境变量/网络表单 ⇒ 首期不做。
- 镜像推送 / 导出、容器内文件上传下载、Compose 文件编辑。

---

## 七、测试策略

| 层次 | 做法 |
|---|---|
| 单测 | **命令拼装** + **输出解析**（纯函数，最容易出错 ⇒ 覆盖带空格/空字段/多行） |
| 冒烟 | `examples/docker_ui_smoke.rs`：用**假 docker 脚本**（PATH 里放一个 `docker`）造输出 ⇒ 不依赖真 docker，也不污染真环境 |
| 真机 | 用户自己的服务器（**要问用户哪台机器有 docker**；`aly` 上有没有 docker 待确认） |
| 反向验证 | 每个阶段都把修复退一遍，确认断言真会红 |

---

## 八、进度

- 2026-11-15：规划完成，**未动手**。等用户拍板首期范围。
