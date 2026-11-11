# PuTTYPlus — 带图形化 SFTP 的 PuTTY 替身

一个约 100KB 的原生 Linux GUI 程序，**命令行参数与 PuTTY 完全兼容**。
把它配置为堡垒机的本地 SSH 客户端后，堡垒机拉起它的同时：

- 右侧自动打开 SSH 终端（VTE 终端组件，即 gnome-terminal 的内核）
- 左侧自动用同一套地址/账号/密码连接 SFTP，出现「远程 + 本地」双栏文件管理器

在 Linux 上获得接近 MobaXterm 的体验，而堡垒机完全无感知。

```
┌─────────────┬──────────────────────────────┐
│ 远程 (SFTP)  │                              │
│  文件列表     │        SSH 终端               │
├─────────────┤                              │
│ 本地         │                              │
│  文件列表     │                              │
└─────────────┴──────────────────────────────┘
```

## 编译

**Rust 重构版（推荐）**：见 [`rust/README.md`](rust/README.md)——GTK4 + russh，
`cargo build --release` 即可，行为与 C 版 1:1 兼容（密码传递、递归深度等
安全细节有改进）。

**打成 deb 分发**：

```bash
bash rust/tools/build_deb.sh     # 产物：puttyplus_<版本>-1_amd64.deb
sudo dpkg -i puttyplus_<版本>-1_amd64.deb
```

带桌面菜单项、中文手册页和图标，依赖 GTK ≥ 4.18 / VTE ≥ 0.80
（Debian 13 / Ubuntu 25.04+ / Deepin 25）。详见
[`rust/README.md`](rust/README.md#打包成-deb)。

C 版（GTK3 + libssh）：

```bash
# Deepin/Debian/Ubuntu
sudo apt install build-essential pkg-config libgtk-3-dev libvte-2.91-dev libssh-dev
cd c              # C 版工程归档于 c/（src/ 同步移动）
make            # 产出 c/puttyplus（约 100KB）
sudo make install   # 可选：安装到 /usr/local/bin
```

运行时只依赖桌面发行版标配的动态库：`libgtk-3-0`、`libvte-2.91-0`、`libssh-4`。
拷贝单个 `puttyplus` 到其他机器即可用；个别精简系统缺库时
`apt install libvte-2.91-0 libssh-4` 即可（无需 dev 包）。

若系统里装有 putty 套件（`plink` 在 PATH 中），终端会优先用 plink
（参数行为与 putty 完全一致）；否则使用系统自带的 OpenSSH `ssh`。

## 堡垒机接入

1. 在堡垒机客户端的本地工具设置里，把 PuTTY 路径指到 `puttyplus`；
2. 若堡垒机按文件名固定查找 `putty`，做个软链：
   `sudo ln -s /usr/local/bin/puttyplus /usr/local/bin/putty`；
3. 正常从堡垒机发起连接即可。

## 与 PuTTY 兼容的命令行

```bash
puttyplus [-ssh] [user@]host [-P 端口] [-l 用户] [-pw 密码] ...
```

| 参数 | 说明 |
| --- | --- |
| `-ssh / -telnet / -raw` | 协议（telnet/raw 走外部命令，SFTP 仅支持 ssh） |
| `[user@]host` | 位置参数；恰好匹配已保存会话名时自动加载该会话 |
| `-P 端口` `-l 用户` | 端口 / 用户名 |
| `-pw 密码` | 密码。终端与 SFTP 都会使用；终端里自动应答密码提示 |
| `-pwfile 文件` | 从文件**第一行**读密码（PuTTY 0.77+ 语义），读完即弃，适配堡垒机临时密码文件 |
| `-load 会话` | 加载 `~/.putty/sessions/` 下的会话（HostName/PortNumber/UserName/PublicKeyFile/PortForwardings） |
| `-i 密钥` | OpenSSH 格式私钥；`.ppk` 需先转换（见 FAQ） |
| `-L/-R/-D` | 端口转发，语法同 putty，透传给 ssh/plink |
| `-N -t -T -4 -6 -C -v -batch` | 透传 |
| `-m 文件` | 远程命令 |
| `-proxycmd 命令` | 透传给 ssh（`ProxyCommand=`） |
| `-hostkey` `-sercfg` 等 | 接受并忽略 |
| 未知参数 | 记录警告后忽略，不会启动失败 |

其他密码来源（命令行未提供时）：环境变量 `SSHPASS`（sshpass -e 约定）。

### 密钥认证（与 OpenSSH 行为一致）

- 未指定 `-i` 时，SFTP 侧自动尝试 `ssh-agent` 及默认私钥
  （`~/.ssh/id_rsa`、`id_ed25519`、`id_ecdsa` 等）——终端侧的 ssh 本来就这么做
- 支持 `~/.ssh/config` 别名：`puttyplus -ssh txy` 会解析其中的
  `HostName` / `Port` / `User` / `IdentityFile`，终端与 SFTP 连到同一目标
- putty 保存的会话（`-load`）优先级高于 `~/.ssh/config`

无参数启动时弹出连接对话框（主机/端口/用户/密码），可当普通 SSH 客户端手动使用。

### 临时会话文件的读取时机

堡垒机常见做法是：生成含认证信息的临时会话/密码文件 → 拉起客户端 →
立刻删除该文件。本程序在 `gtk_init()` **之前**就同步完成全部文件读取，
不存在 GUI 初始化延迟导致文件已被删、读取失败的问题。

## 文件管理

- **SFTP 跟随终端目录（默认开启）**：终端里 `cd` 之后约 2 秒内，左侧远程面板
  自动切换到同一目录（远程面板标题行的「跟随」按钮可关闭）。两条通道互补：
  - **OSC 7**：远程 shell 若配置了向终端上报目录（如 bash 加载 vte.sh），
    即时跟随，零开销
  - **/proc 轮询**：通过 SFTP 连接上的 exec 通道执行 `ps`/`readlink`，找到本
    会话的登录 shell 并读取其工作目录，对默认配置的 bash/zsh 通用；轮询按
    shell 启动时间最接近匹配，且只匹配 pts 上的纯 shell（不会误认堡垒机菜单）
  - 环境变量 `PUTTYPLUS_FOLLOW_MS` 调整轮询间隔（毫秒），设 0 关闭轮询
    （OSC 7 仍生效）；若服务器禁止 exec 通道，连续失败后自动停止轮询并记日志
  - 注意：审计严格的堡垒机会记录每次 exec 命令（约每 2 秒一条）；介意可调大
    间隔或关闭。可用 `cd c && make test_follow && ./test_follow <host> <port> <user> <秒>`
    提前验证目标环境是否支持 exec 通道
- 远程/本地双栏：刷新、上级、主页、新建、改名、删除、隐藏文件切换
- **上传**：选中本地文件点「上传」，或把文件从桌面/本地面板**拖进远程面板**
- **下载**：选中远程文件点「下载」，或把远程文件**拖进本地面板**；双击远程文件即下载
- 目录递归传输；传输队列窗口显示进度/速度，可取消
- 本地删除进回收站（不可用时直接删除）；远程删除为递归删除，有确认框
- 路径栏可直接输入路径回车跳转（远程支持 `~`）

终端：右键菜单复制/粘贴/重连，`Ctrl+Shift+C/V`，中键粘贴。

界面：所有菜单（终端右键、文件面板、对象树、顶栏、连接管理器）统一成
同一套观感——图标 + 左对齐文字 + 快捷键提示，鼠标划过即高亮，不可逆操作
（删除表、关闭终端、退出）标红。

## 安全行为说明

- 主机密钥：与 `ssh -o StrictHostKeyChecking=accept-new` 一致——首见自动记录到
  `~/.ssh/known_hosts`，密钥变化时拒绝连接并提示
- 终端密码自动应答：仅在通过 `-pw`/`-pwfile` 明确提供密码且未用密钥时启用，
  限制 3 次、30 秒内，避免误应答登录之后的普通程序（如 sudo）
- 命令行 `-pw` 会出现在进程列表中（与 putty 相同）；建议堡垒机使用
  `-pwfile` 临时文件方式，本程序读完即不再触碰该文件

## 堡垒机实战兼容性（已实测验证的场景）

针对"基础控件拉起 putty"的网关类堡垒机（本地隧道 `127.0.0.1:动态端口` 架构）：

- **`-title 账号@设备IP`**：正确消费并用作窗口标题（含 `-title=xxx` 形式），
  不再把值泄漏成远端命令（旧版此处有"值被当命令执行→会话秒退"的 bug）
- **`-load 会话名` + 位置参数并存**：与真 PuTTY 一致，命令行 host 覆盖会话里的
  HostName（launcher 常用 `-load 预置会话 user@127.0.0.1 -P 动态端口` 组合）
- **零交互登录**：plink 的密钥确认提示（`(y/n,`）与
  "Access granted. Press Return to begin session." 均自动应答
- **回环目标跳过主机密钥校验**：目标是 127.0.0.1/::1/localhost 时（SFTP 侧）
  不做校验、不写 known_hosts —— 网关按访问轮换密钥、动态端口每次不同，
  逐端口记录只会污染 known_hosts 并在端口复用时误报"密钥已变化"。
  非回环目标维持 `accept-new` + 变更拒绝的严格策略
- **显式密码优先认证**：带 `-pw`/`-pwfile` 时先试密码/键盘交互，不再读取
  或出示本地默认私钥（避免向网关中介暴露本机身份文件；无密码时仍按
  agent/默认密钥顺序尝试）
- **终端通道选择**：PATH 里有 plink 时优先用 plink（与 PuTTY 行为一致）。
  若不想用厂商修改过的 plink，可在 PATH 前面放一个只含系统 ssh 的目录或
  临时改名 plink（puttyplus 找不到 plink 会自动回退到系统 ssh）

部署到堡垒机所在机器：把 `puttyplus` 拷过去替换 `tool.ini` 里指向的
putty 路径即可。若之前装过过滤 `-title` 的包装器，新版已原生支持 `-title`，
可以去掉过滤逻辑（包装器的端口独占/日志模式不受影响，可继续使用）。

## 连接管理器

无参数启动时打开连接管理器（替代旧的简易对话框）；主窗口远程面板标题行的
「连接」按钮也可随时打开，选中的连接在**新窗口**打开，不打断当前会话。

- **数据来源两套**：
  - `~/.ssh/config` 的主机别名（只读展示，标"ssh config"，编辑/删除会提示
    只读；连接时自动按 HostName/Port/User/IdentityFile 展开）
  - 自建连接，存 `~/.config/puttyplus/connections.ini`（权限 0600），支持
    新建/编辑/删除/同名覆盖，**不会写入 ~/.ssh/config**
- 新建/编辑字段：名称、主机、端口、用户名、密码（可选，明文存储，对话框
  中有提示；留空则连接时在终端输入或走密钥认证）
- 双击或选中后「连接」；「手动连接」= 一次性填表不保存
- 条目名称会用作窗口标题（多窗口时便于区分）

## FAQ

**Q: `.ppk` 私钥能用吗？**
OpenSSH/libssh 不认 putty 的 PPK。转换一次即可：
`puttygen key.ppk -O private-openssh -o key.openssh`，之后 `-i key.openssh`。

**Q: `-load` 的会话里配了代理（SOCKS/HTTP）怎么办？**
当前版本会提示并忽略代理设置（终端与 SFTP 都直连目标）。堡垒机本地端口
直连场景通常用不到代理。需要时可在会话外通过 `-proxycmd` 传递。

**Q: SFTP 连接失败但终端正常？**
服务器可能未开启 SFTP 子系统；或堡垒机本地端口只代理了终端会话。
查看远程面板状态行的错误信息。

**Q: 怎么改终端字体？**
`PUTTYPLUS_FONT="Monospace 12" puttyplus ...`；本地面板起始目录用
`PUTTYPLUS_LOCALDIR=/path` 指定（默认 `~/Downloads`）。

**Q: 调试？**
`puttyplus --print-config <putty 参数>` 打印解析结果不启动 GUI；
⚠️ 命令行参数的**完整契约**（参数表、细则、`--print-config` 输出格式、
askpass 协议）见仓库 `AGENTS.md` 的「命令行参数契约」一节 —— 那是照着
C 版源码逐条抄下来并**冻结**的，改参数前先看那一节；
`PUTTYPLUS_AUTOFEED_DEBUG=1 G_MESSAGES_DEBUG=all` 观察密码自动应答的匹配过程。

## 代码结构

```
c/src/args.c         PuTTY 兼容参数解析 + ~/.putty 会话文件加载
c/src/term.c           VTE 终端、ssh/plink 启动、askpass/PTY 密码自动应答
c/src/sftp_backend.c   libssh 封装（连接/认证/列目录/递归传输，全操作互斥）
c/src/sftp_panel.c     双栏 GUI、后台任务队列、传输队列窗口、拖拽
c/src/main.c           入口（askpass 模式 → 参数解析 → GUI 组装）
```
