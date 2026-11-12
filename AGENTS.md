# PuTTYPlus 工作区说明

PuTTY 兼容的 SSH 终端 + SFTP 图形客户端，**双实现并存且功能必须保持一致**：

- **仓库根就是 crate**（`Cargo.toml` / `src/` / `examples/` / `debian/` / `tools/` /
  `docs/`）—— Rust 主线（GTK4 + vte4 + russh + russh-sftp），lib+bin 结构，571 个单测。
  ⚠️ **2026-11-01 起没有 `rust/` 子目录了**：本仓库**直接就是** GitHub 上的发布
  仓库（`git@github.com:fwt11/puttyplus.git`），改完 push 即发布。
- `c/` — 对照基线。C（GTK3 + VTE + libssh），**已冻结**（Rust 的功能改动不再反向
  移植）；只保留作命令行契约的历史依据，**不入库**（本地排除，见末节）。
- `README.md` 用户文档（参数兼容表、堡垒机场景）；`docs/功能说明.md` 功能细节
  （`docs/功能说明-内部.md` 是内部版，含阶段号，**已入库**）。
- ⚠️ **2026-11-11 起：台账全部入库**（`AGENTS.md` / `RESUME.md` /
  `tools/ROADMAP.md` / `tools/FEATURE_PARITY.md` / `tools/DBX_PLAN.md` /
  `docs/UI_AUDIT.md` 与 UI 审查文档 / `docs/功能说明-内部.md` /
  `docs/README-开发版.md`）⇒ **换台机器 clone 下来就能接着干**
  （用户 2026-11-11：「把本地台帐也加入git，方便在其他平台继续开发」）。
  仍是纯本地的只剩：`c/`、`tools/compat_check.sh`、`tools/shot_*.sh`
  与各机器的产物目录。

## 构建 / 测试

⚠️ **2026-11-10 起 `cargo build` 必须带 `ZIG`**：终端引擎 ghostty 变成
**默认特性**（用户拍板：Linux 默认就带 ghostty，用户能在设置里选 libvte
还是 ghostty）⇒ 编译 libghostty-vt 要 **Zig 0.17.x**。本机的 Zig 在
`~/.cache/puttyplus-zig/bin/zig`（**别删**）：

```bash
export ZIG=$HOME/.cache/puttyplus-zig/bin/zig
```

⇒ **每个阶段的第一条命令都要先 export 这个**（新开的 shell 不会自动有）。
⚠️ **Windows 上不能 `--no-default-features`**（那儿没有 VTE，终端后端
只能是 ghostty ⇒ 关掉就编不过）；Linux 上可以（省 Zig）。

```bash
cargo build --release && cargo test                # Rust（release 已带 strip/LTO/ring 配置）
cargo build --release --examples          # ⚠️ **119 个冒烟都得能编**（见下）
(cd c && make)                                      # C 版（已冻结，只作参照）
bash tools/compat_check.sh                    # 两版外部行为对照（81 项，需 C 版二进制）
PP_SSHD='127.0.0.1 2222 <user>' bash tools/compat_check.sh   # 含 selftest 全流程对照
bash tools/cli_contract_check.sh              # 命令行契约对照黄金基准（73 项，⭐ 不需要 C 版）
./target/release/puttyplus --sftp-selftest <host> <port> <user> [pw]   # SFTP 集成自检
bash tools/build_deb.sh                        # 打 deb 包（产物在仓库根目录）
xvfb-run -a cargo run --example db_ui_smoke         # 数据库工作台 GUI 冒烟（不连库）
xvfb-run -a cargo run --example db_conn_smoke       # 数据库连接树冒烟（多连接/树形/老格式）
xvfb-run -a cargo run --example db_ui_e2e -- <ssh_host> <ssh_port> <ssh_user> \
    <db_host> <db_port> <db_user> <dbname> <db_pass> [pg|mysql]   # 真库端到端
```

⚠️ **每完成一个阶段，都要出一次 release 可执行文件**（用户 2026-11-01 明确
要求：他要看效果，却"找不到你构建的可执行文件"）：

```bash
cargo build --release && mkdir -p release && cp target/release/puttyplus release/puttyplus
```

- 产物两处都有：`target/release/puttyplus`（cargo 的原生位置）与
  **仓库根 `release/puttyplus`**（一眼能找到；`.gitignore` 忽略 `/release/`，
  **不会**进 git）；
- ⚠️ **别放成仓库根的 `puttyplus`**：那个名字是 **C 版二进制**的候选路径
  （`compat_check.sh` 从 `./puttyplus` 或 `c/puttyplus` 里探测"C 版基线"）
- ⚠️ **C 版不再维护了**（用户 2026-11-01）⇒ 命令行的对外契约改由
  `tools/cli_contract_check.sh`（对照 **73 条黄金基准**，不依赖 C 版
  二进制）守住；`compat_check.sh` 是**活体对照**，C 版删掉后它会退场。
  ⇒ Rust 产物占了它，compat 就变成"自己跟自己比" ⇒ 必假失败
  （2026-11-01 踩过：差在 SSH_ASKPASS 指向的自身路径）。脚本已加保护
  （内容与 Rust 版相同就跳过），但**别再放那个名字**；
- **构建 ≠ 打包 ≠ 安装**：release 二进制是"给你看效果"的，不写系统目录、
  不碰 dpkg ⇒ 不受"装东西要先请示"的限制；`bash tools/build_deb.sh`
  （deb）与 `sudo dpkg -i`（装进系统）仍然一律先问用户；
- ⚠️ **`cargo test --lib` 与 `cargo build --release` 都不编 example** ⇒ 改了
  **公开签名**（如 `panel::on_row_activated` 多了个 `islink`）而漏改某个
  example ⇒ **本地全绿、CI 才红**。真踩过：`01efb43`（软链双击跟进去）改了
  那个签名却漏了 `examples/selfheal_smoke.rs` ⇒ CI 从第 50 轮起**连红
  10 轮**才被人看见（2026-11-11）。
  ⇒ **每个阶段都要跑 `cargo build --release --examples`**（119 个 example 全
  编一遍）⇒ 签名漂移本地就红。CI 里也已加同名一步（`.github/workflows/ci.yml`）。
- ⚠️ **本地按 CI 那套跑 GUI 冒烟时，别乱改 `HOME`**（2026-11-11 踩过）：
  改了 `HOME` ⇒ cargo 找不到 rustup/cargo 的家 ⇒ **换了 rustc** ⇒ 依赖的
  指纹全变 ⇒ **全量重编**（119 个 example 要 10 分钟，还可能去重新拉依赖）。
  ⇒ 跑之前把 `CARGO_HOME` / `RUSTUP_HOME` **显式指回去**：
  `export CARGO_HOME=/home/fwt/.cargo RUSTUP_HOME=/home/fwt/.rustup`。
  ⚠️ **CI 没这个问题**（那边 `RUSTUP_HOME=/opt/rust/rustup`、
  `CARGO_HOME=/opt/rust/cargo` 是用 `$GITHUB_ENV` 固定到后续步骤的）。
- 冒烟与单测跑的是 **debug**（`cargo test --lib` / `cargo run --example`），
  慢的时候可以并行，**不能拿 debug 代替 release 交付**。

打包：`debian/` 是标准 Debian 打包目录（MIT）。改 `Cargo.toml` 的
GTK/VTE 特性（`v4_18` / `v0_80`）后要同步 `debian/control` 的版本下限，
否则装到 GTK 4.14 的机器上会起不来。本机 cargo 来自 rustup 而非 dpkg，
`dpkg-checkbuilddeps` 会误报缺 cargo，脚本已用 `-d` 跳过。

本地测试 sshd：`$HOME/.cache/puttyplus-test-sshd`（端口 2233，公钥认证，
authorized_keys 含默认密钥）。⚠️ **主机密钥固定放这里，别放 /tmp** —— /tmp
会被清理，host key 一变 ssh 会因 `StrictHostKeyChecking=accept-new` 拒绝
已变更的密钥而秒退。⚠️ 2026-10-27 补了 `Subsystem sftp
/usr/lib/openssh/sftp-server` —— 之前那行**没有**，SFTP 子系统请求全被拒
（sshd 日志里的 "subsystem not found"），远程文件列表根本连不上，
`fileops_smoke` 这类要传文件的冒烟必挂。
仓库根 `.gitignore` 按 basename 忽略 `puttyplus`/`*.o`/`test_follow`，
对 `c/` 下同样生效。

### 跨平台分支（Windows 优先，`feature/cross-platform`）

⚠️ **2026-11-09：跨平台改造已合进 `main`**（快进 41 个提交）⇒ **主线现在
同时是 Linux 版 + Windows 版**。阶段 C0–C14 已完成。
⚠️ **`feature/cross-platform` 与 `fix/cursor-row` 两个分支已删除**（本地 +
远端）—— 内容全在 `main` 里，删了不丢提交 ⇒ **现在只有 `main` 一个分支**。
⇒ **在 Windows 上开发的那个 clone 要改**：切到 `main` 并 `git pull`
（它本地还留着旧分支 ⇒ **再往那个分支 push 会把分支重新建出来**）。

- **Linux 侧零回归**：终端**默认**仍走 **VTE**，一切照旧；⚠️ **但构建要
  Zig**（ghostty 是**默认特性**了 ⇒ 编译 libghostty-vt 要 Zig 0.17）。
  ⇒ 想省掉 Zig：`cargo build --release --no-default-features`（Linux 才行）。
  ⇒ **用户能在「首选项 → 终端」里选引擎**（libvte / ghostty，存
  `[ui] term_backend`；**只对新开的终端生效**；Windows 上那一项不显示）。
  ⇒ 后端优先级：`PP_TERM_BACKEND` 环境变量 > 设置项 > 平台默认。
- **Windows 侧**：终端内核换 **libghostty-vt**（Ghostty 的终端内核）+ 自绘
  控件（cairo + pangocairo）+ **ConPTY** 伪终端；SFTP 面板与命令行契约
  与 Linux 版一致（**73 条黄金基准 PASS=73 FAIL=0**）。
- ⚠️ **Windows 版不能在 Linux 上交叉编译**（`glib-sys` 要 Windows sysroot）
  ⇒ 只能 Windows / CI 上编。

常用命令：

```bash
# Linux 上试 ghostty 后端（不切平台也能跑）
PP_TERM_BACKEND=ghostty xvfb-run -a cargo run --example term_reconnect_smoke

# 在 Linux 上对「不依赖 GTK 的平台层」做 Windows 目标的交叉检查（几秒出结果）
bash tools/win_cross_check.sh
```

⚠️ **路径语义分两套，别混用**（阶段 C5 的核心）：
**远端路径恒为 POSIX**（`/srv/x`）⇒ 用 `crate::util::path_join/path_parent/
path_basename`；**本地路径是平台原生**（Windows 上 `C:\dir\x`）⇒ 用
`crate::platform::localpath::join/parent/basename`。
⇒ 混用会导致 Windows 上「上级」跳到根、重命名预填整条路径、
**上传后远端出现叫 `C:\dir\a.txt` 的文件**。

⚠️ **拼路径别手搓 `+ "/"`**（阶段 C6 踩过）：`~` 展开的**边界分隔符按平台**
（Windows `\`），其余部分原样保留 ⇒ 用 `Path::join`，或把分隔符收进
**可注入的参数**（`config::expand_tilde_for(p, home, windows)`）⇒
**Windows 的行为在 Linux 上就能测**，不用等一轮 Windows CI 才发现写错。
⚠️ **「像不像绝对路径」的形状判定**要与「当前平台的本地路径判定」分成
两个函数：`localpath::is_abs`（按运行平台）vs `localpath::is_abs_shape`
（不分平台：盘符/UNC/前导 `/` 都算绝对）⇒ 否则 Linux 上永远测不到
Windows 的盘符形态（`IdentityFile` 曾因此把 `C:\...` 当相对路径再补一层
`~/.ssh/`）。

✅ **2026-11-10：Windows 上数据库工作台与端口映射都已开放**（不再隐藏入口）：
- DB 工作台的 GTK 层**零平台分支**，后端走 SSH `direct-tcpip` → 本地桥接 →
  纯 Rust 驱动（不依赖外部 psql/mysql）；`backend/db.rs` 早有 `#[cfg(not(unix))]`
  分支（Windows 上 PostgreSQL 改走 127.0.0.1 回环 TCP 桥，与 MySQL 同构）。
  ⚠️ **一处部署侧差别**：Unix 走本地套接字（远端 pg_hba.conf 多半 peer/trust，
  不校验密码）vs Windows 走回环 TCP ⇒ **远端 pg_hba.conf 必须放行 TCP + 密码
  认证**；MySQL 两边都是回环 TCP。
- 端口映射由**程序内引擎**承接（`backend/forward.rs`，三类 `-L`/`-R`/`-D` 都走
  russh 的 `direct-tcpip` 通道）⇒ 判据是「**终端用哪条路连**」
  （`forward::needs_inproc_forwards`），**不是**「系统里有没有 ssh」
  （旧判据会让 Windows 装了 OpenSSH 的机器**静默失效**）。

Windows CI：`.github/workflows/ci-windows.yml`（**独立文件，不动 `ci.yml`**）
⇒ 装 MSVC 工具链 + gvsbuild 的 GTK4 + Zig 0.17 ⇒ 编译 + 单测 + 73 条契约
+ 打包 exe。⚠️ **Windows runner 没有桌面会话 ⇒ GUI 冒烟跑不了**（Linux job
用 xvfb、本地 Windows 机器人工验，清单见 `docs/跨平台-构建说明.md` §10）。
构建细节（GTK4 怎么装、Zig、pangocairo 的坑、ConPTY 的坑、CI 的三个坑）
见 **`docs/跨平台-构建说明.md`**。

⚠️ **Windows 上的 SSH 走哪条路（阶段 3，用户 2026-11-10 拍板）**：
**PATH 里有 `ssh.exe`（或 `plink.exe`）⇒ 就 spawn 它**（终端里跑的是用户
自己的客户端 ⇒ `~/.ssh/config`、agent、ProxyJump、known_hosts 语义**跟
Linux 版一样**）；**一个都没有**（Windows 默认没装 OpenSSH，它是可选功能）
⇒ 兜底走**进程内 russh**（`backend/shell.rs`：申请远程 pty + 开 shell +
键盘字节直接写进去 + 窗口尺寸用 SSH 的 window-change 告诉远程端）⇒
**不装任何外部程序也能跑**（telnet/串口走「自桥接」＝子进程就是本程序自己；
只有 `raw` 协议还要 `nc`）。
⚠️ **判定要认 `.exe`**：`util::find_program_in_path_win` 依次试
`名字.exe`/`.cmd`/`.bat`/原名 ⇒ 以前只找无扩展名的 `ssh` ⇒ **明明装了
OpenSSH 也说「找不到可用的客户端程序」**（用户 2026-11-08 报的正是这个）。
⚠️ **这条规则已被 2026-11-11 的「连接中枢」取代**：终端**默认并入共享
连接**（一个服务器一条 SSH 连接），并**留了开关**切回 spawn ⇒ 见下面
「⭐ 连接中枢」一节。
⚠️ **阶段 B2（2026-11-13）之后 VTE 也能用进程内 SSH**（**假 pty**
接入：自己开一对 pty，master 交给 VTE、slave 留给我们桥接）⇒
Linux 默认后端现在也能并入共享连接；`PP_TERM_SSH=inproc|spawn`
→ 可强制切换（首选项 →「终端」页 →「终端连接方式」也能切）。
⚠️ **ConPTY 双向都是 UTF-16LE** ⇒ 不转码就满屏乱码、中文打不进去 ⇒
转码在 `src/platform/utf16.rs`（**不带平台门控** ⇒ Linux 上能单测：
`read_decoded` + `Utf16Pipe`，24 条单测 + `examples/utf16_pipe_smoke.rs`
用真实 TCP 管道当 ConPTY 替身）。
⇒ **`conpty.rs` 整个文件是 `#![cfg(target_os="windows")]`** ⇒ Linux 上
编译不到 ⇒ 改它要靠 `bash tools/win_cross_check.sh`（阶段 3 起把
`conpty.rs` 也拉进 Windows 目标的交叉编译检查）。

⚠️ **GTK 版本差异会让同一个 bug 两边表现完全不同**（阶段 C7，用户 2026-11-08
在 Windows 上贴了一屏 GLib 警告）：`gtk::Adjustment::new` 的参数不满足
`lower + page_size <= upper` ⇒ **GTK 直接返回 NULL**（不是只警告）⇒ 之后
每次操作都是对着 NULL 做 ⇒ 满屏 `G_IS_OBJECT` / `GTK_IS_ADJUSTMENT` 警告 +
功能整个失效。⚠️ **新 GTK（Windows 上的 4.24）才有这条断言** ⇒ Linux 上
（GTK 4.18）只是「建出一个参数不对的对象」⇒ 症状轻得多 ⇒ 看着不像同一个
bug。⇒ **教训**：跨平台时**别指望 GTK 帮我们拦非法参数** ⇒ 断言要自己写，
而且要写成「值必须是我们给的那一套」（这样「NULL」与「参数不对」两种坏
都能抓到）。

⚠️ **Windows 上不需要装 MSYS2**（用户 2026-11-08 问）：跑 exe 只要 GTK4 的
DLL；编译要 Rust MSVC + VS C++ 生成工具 + gvsbuild 的 GTK4 + Zig；跑 73 条
契约要一个 `bash` ⇒ **Git for Windows 自带的 Git Bash 就够**（CI 就是用它）。
⚠️ 但 Git Bash **本身就是 MSYS 系的** ⇒ 自带「参数路径自动转换」的毛病。

⚠️ **Windows 特有的三个坑（都在测试/环境侧，不在程序）**（阶段 C6 第 12 轮
CI 实测 ⇒ 详见 `docs/跨平台-构建说明.md` §9.1）：
① **Windows 文件系统大小写不敏感** ⇒ `sw_t.txt` 与 `sw_T.txt` **互相覆盖**
⇒ 读到别的用例的基准 ⇒ **基准文件名不能只差大小写**；
② **Git Bash 自动把 `/xxx` 参数转成 Windows 路径**（`/dev/ttyUSB0` →
`C:/Program Files/Git/dev/ttyUSB0`）⇒ 契约脚本在 Windows 下
`export MSYS_NO_PATHCONV=1` + `MSYS2_ARG_CONV_EXCL='*'`；
③ 会话 fixture 叫 `Demo`、ssh 别名叫 `demo` ⇒ **只差大小写** ⇒ Windows 上
`puttyplus demo` 命中了 PuTTY 会话（C 版同样如此 ⇒ **不是程序 bug**）
⇒ 别名用例用**单独的 fixture home**（只有 `.ssh/config`）。
⇒ 三条都进了 `--selftest` ⇒ **少变量 / 基准撞车 ⇒ 自测立刻报错**。

### ⭐ 真库测试**一律用 aly 上的 postgres**（用户 2026-11-02 明确）

⚠️ **不要在本机装数据库**：本机没有 PG 服务（`pg_isready` 没响应、
`postgresql` 服务 inactive），要真库就上 aly。（AGENTS.md 第 9 条是
"往**用户机器**装东西要先请示"；aly 上本来就有 PG 容器，**用它不等于
在用户机器上装东西**。）

**aly 的环境**（实测，别自己猜）：

| 项 | 值 |
|---|---|
| 别名 | `aly`（`~/.ssh/config` 里 → `182.92.244.132`，user `root`） |
| PG 容器 | `some-postgres`，宿主机 `0.0.0.0:5432` |
| 库 / 用户 / 口令 | `postgres` / `postgres` / `mysecretpassword` |
| 已有表 | `public.demo_users`（3 行：id / name / email / age / active / created_at） |

**跑法**（`examples/db_ui_e2e` 的参数顺序）：
```bash
cargo run --example db_ui_e2e -- 182.92.244.132 22 root 127.0.0.1 5432 \
    postgres postgres mysecretpassword pg
```

⚠️ **端口是 5432 = 跳板机自己的回环**（容器发布到宿主机的那个端口）。
**从外网直连 182.92.244.132:5432 不通**（防火墙超时）——但**应用不需要
外网直连**：`backend/conn.rs` 是**通过 SSH 会话开 direct-tcpip 通道**去连库
⇒ 这里的 `127.0.0.1` 指的是**跳板机**，不是开发机。别在本地开隧道再写
隧道的端口（那是跳板机上的端口，写错了就是"连不上"，第一版就踩过）。

⚠️ **写库范围**：只碰 `id >= 900000` 的探针行 + 自建自删的 `pp_probe_*`
对象；用户自己的 3 行（id 1/2/3）不受影响；中途崩会残留探针对象
（e2e 开头会清 `id >= 900000` 的探针行）。

## ⭐ 连接中枢：一个服务器一条 SSH 连接（2026-11-11 起）

用户拍板的方向：
> 「我们默认情况下 sftp, 端口映射, 数据库管理等等都走通道，
> 　我们可以保证对每个服务器的访问只需要一个连接。」

⇒ **同一台服务器（host+port+user）在整个程序里只有一条 SSH 连接**，
各个功能在它上面开自己的**通道**：

| 功能 | 通道类型 |
|---|---|
| SFTP 文件面板 | `session` channel + `subsystem sftp` |
| 交互终端（进程内） | `session` channel + `request_pty` + `request_shell` |
| 端口映射（`-L`/`-R`/`-D`） | `direct-tcpip` / `tcpip-forward` |
| 数据库工作台 | `direct-tcpip` → 远程 5432/3306 |

⚠️ **SFTP 走 `subsystem` 而不是 `direct-tcpip`**：OpenSSH 默认是
`Subsystem sftp internal-sftp` —— 那个进程**只走 SSH 子系统**、**没有 TCP
端口** ⇒ `direct-tcpip` 连不上。通道类型不同，**底层是同一条连接**。

### 代码在哪

- `src/backend/hub.rs`：**连接中枢**。
  - `hub::runtime()`：**全局 multi_thread tokio 运行时**。⚠️ **必须只有
    一个**：russh 的 `Handle` 绑定在创建它的 runtime 上 ⇒ 跨 runtime 用不了
    ⇒「一条连接多方共用」的前提是各方同 runtime。⚠️ **必须 multi_thread**：
    SFTP worker 的命令循环是 `Runtime::block_on`（阻塞线程等命令）⇒
    current_thread 会被独占 ⇒ 多面板没法共用。
  - `hub::ServerKey`：**复用粒度** = `host+port+user`（**归一化过**：主机名
    大小写/空白、端口 0、空用户 ⇒ 同一个服务器不会因为写法不同被当成两台）。
  - `hub::acquire(key, creds)`：**拿（或建）一条共享连接**。⚠️ **不管调用方
    在哪个 runtime 上，连接都建在全局 runtime 上**（否则那条连接会跟着
    调用方的 runtime 一起死，别的使用者莫名断线）。
  - `hub::SharedConn`：会话 + 远程转发登记表 + **进程内转发规则集（按来源
    分组）**。
  - **引用计数**：最后一个使用者放手才断（面板关了但终端还在用 ⇒ 不断）。
- `src/backend/conn.rs`：`SftpBackend` 持有**共享连接**（不再自己建一条）；
  `close()` **只关 SFTP 通道、不断连接**；`connect_with_source()` 指定转发
  规则的来源；`open_sftp_session()` 在既有会话上开 SFTP 通道。
- `src/panel/worker.rs`：面板 worker 改用**全局运行时**（命令循环改成
  「处理完再挂下一条阻塞任务」⇒ 退出时不留挂着的任务）。
- `src/term.rs`：进程内 SSH 走 `hub::acquire` ⇒ 终端与面板共用一条连接。

### 转发规则挂在**连接**上，不挂面板

⚠️ **关掉文件面板不会把端口映射一起停掉**（映射是会话级功能）。
⚠️ **规则按来源分组**（`SharedConn::forwards` 是
`HashMap<来源, Vec<FwdEntry>>`）：同一台服务器可能被**好几个连接条目**引用
（不同名字、不同转发规则）⇒ 共用一条连接后，后连的那个不能把前一个的规则
清掉 ⇒ 各来源只覆盖自己那一份。来源 = 连接条目名（`conn::fwd_source`）。
⚠️ **停规则要等监听端口真释放**（`ActiveForward::wait`）：`abort()` 是异步
取消 ⇒ 端口还占着 ⇒ 紧接着重新 bind 同一个端口必撞 EADDRINUSE。

### 终端走哪条路（开关）

⚠️ **用户 2026-11-11 拍板**：终端**默认并入共享连接**，但**留一个开关**切回
spawn（首选项 →「终端」页 →「终端连接方式」，或环境变量
`PP_TERM_SSH=inproc|spawn`）。
⚠️ **并入的代价**：终端**不走用户自己的 ssh** ⇒ `~/.ssh/config`
（ProxyJump 等）不生效 ⇒ 需要的话切回 spawn。
⚠️ **后端不支持 ⇒ 退回 spawn**（不是报错）。
✅ **阶段 B2（2026-11-13）之后**：VTE 也能用进程内 SSH（**假 pty**
接入）⇒ **Linux 默认就是并入共享连接**（终端 + 文件面板 + 端口映射 +
数据库工作台 = 一台服务器一条 SSH 连接）。
⚠️ **代价**：并入后终端**不走用户自己的 ssh** ⇒ `~/.ssh/config`
（ProxyJump 等）、ssh-agent **都不生效** ⇒ 需要的话切回 spawn
（首选项 →「终端」页 →「终端连接方式」，或 `PP_TERM_SSH=spawn`）。
⚠️ **判定必须与「转发由谁承接」用同一个函数**（`term::use_inproc_ssh()`）⇒
否则会出现「终端没 spawn ssh、转发却发给了 ssh」的空转。
⚠️ **「会话起于何时」（`TermInner::spawn_wall`）两条路都要在
`spawn_term` 开头就记**：面板的**目录跟随轮询**靠它找远端那个 shell
（远端 `ps` 里按 pts + 启动时间匹配）⇒ 不记 ⇒ 轮询**整个不跑**
（`follow_poll_cb` 见到 0 就跳过）⇒ 面板永远跟不上终端的目录。
⚠️ **不能等会话真的起来再记**：进程内 SSH 是**异步**建连的 ⇒ 等它建好
时上层早就把 `spawn_time()` 取走了（拿到 0）⇒ 面板那边的值永远是 0
（阶段 B2 实测踩到；Windows 上一直有这个问题）。

### 怎么验

```bash
cargo run --example hub_share_smoke        # 中枢本体：复用 / 多通道 / 来源分组 / 引用计数
PP_TERM_SSH=inproc PP_TERM_BACKEND=ghostty \
  xvfb-run -a cargo run --example hub_shared_gui_smoke   # 端到端：终端 + 面板 = 1 条连接
xvfb-run -a cargo run --example vte_inproc_smoke        # 阶段 B2：VTE 后端 + 进程内 SSH
```
⚠️ 三个冒烟都要**本机测试 sshd 在 127.0.0.1:2233**（公钥认证）。

## 堡垒机场景：回环目标不查 known_hosts（2026-11-11 记）

**症状**（用户 2026-11-11 报）：在堡垒机上**分屏失败** —— 分屏的那个终端报

```
WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!
Offending ED25519 key in /home/fwt/.ssh/known_hosts:351
  remove with: ssh-keygen -f '/home/fwt/.ssh/known_hosts' -R '[127.0.0.1]:38863'
Host key for [127.0.0.1]:38863 has changed and you have requested strict checking.
Host key verification failed.
```

⇒ 退出码 **255** ⇒ **分屏开不出来**。第一次开终端没事，分屏就报。

### 根因

- 堡垒机在**本地开一个随机端口**，我们连的就是那个端口 ⇒ 目标恒是
  `127.0.0.1:<随机端口>`；
- 堡垒机的隧道**按访问轮换主机密钥**；
- OpenSSH 的 known_hosts 是按 **`[host]:port`** 记的。

⇒ 同一个端口第二次连就是**另一把 key** ⇒ OpenSSH 直接**硬失败**。

⚠️ **为什么「第一次开终端不报」**：那条端口在 known_hosts 里**还没有条目**
⇒ `accept-new` 当成新主机收下（"Permanently added"）⇒ 连上了；分屏时
条目已经在了 ⇒ 对不上 ⇒ 硬失败。

⚠️ **为什么之前用 plink 不报**：PuTTY 的主机密钥缓存**只按主机名**
（**不分端口**）⇒ 随机端口不影响它；而且 plink 是**询问**（y/n）不是
**硬失败** ⇒ 用户答一次 y 就过去了。

### 三条路本来就不一致（这才是真 bug）

| 路 | 之前的做法 | 对不对 |
|---|---|---|
| SFTP / 进程内（russh） | `backend::conn::is_loopback` ⇒ **回环直接放行** | ✅ |
| C 版 | `sftp_backend.c` 里也是**回环跳过** | ✅ |
| **终端 spawn（OpenSSH）** | 一律 `StrictHostKeyChecking=accept-new` | ❌ **漏了** |

⇒ 修法：spawn 那条路对**回环目标**也把 known_hosts 置空
（`term::loopback_host_key_opts`）⇒ 三条路一致。

### 实测（`examples/loopback_hostkey_smoke.rs`）

| 方案 | 结果 |
|---|---|
| 现状（accept-new + 用户 known_hosts） | **硬失败 255** |
| **`UserKnownHostsFile=/dev/null` + `GlobalKnownHostsFile=/dev/null`** | **连上 0**（只多一行 "Permanently added"，无害） |
| `StrictHostKeyChecking=no` | 连上了，**但照样刷那句吓人的 CHANGED** ⇒ ❌ 不是解法 |

⚠️ **两个 `/dev/null` 都要给**：只置 `UserKnownHostsFile` 的话，
`/etc/ssh/ssh_known_hosts`（全局）里若有该主机的条目仍会被查到。

⚠️ **别用 `$HOME` 定位 known_hosts 做测试**：OpenSSH 找 home 目录用的是
`getpwuid()`（passwd 条目），**不看 `$HOME`** ⇒ 设了也没用（踩过一次：
投毒文件根本没被读到，两步都绿 ⇒ 差点以为测对了）。

⚠️ **插 `-o` 必须插在 `--` 之前**：`--` 之后全是**远程命令** ⇒ 插在后面
ssh 根本不当选项看（拿它去跑远端命令 ⇒ 失败原因变成「远端命令出错」，
**测的就不是主机密钥那件事了**）。也踩过一次。

## 上传时界面整体卡死：根因与修法（2026-11-11 记，做任何带进度/动画的活都得读）

**症状**（用户 2026-11-11 深夜报）：堡垒机上上传 1GB 文件（8Mbps）⇒ 界面
**完全卡住**（终端、菜单都点不动），进度停在 2%，很久后突然跳到 80%，然后
才恢复。

### 根因 ①：整表重画（主因）

昨晚的呼吸动画每 60ms 对**整棵文件列表** `view.queue_draw()`，每个进度 tick
（每 64KB）又 `queue_draw()` ⇒ 每个可见行都要新建 Pango 布局 + 量文字宽度。

**实测数字**（`examples/xfer_stall_smoke.rs`）：

| 场景 | 主线程单次 iteration |
|---|---|
| 面板**没真正显示**（GTK 跳过绘制） | 12ms（**假数字**） |
| 面板挂到窗口、真正画出来 | **59ms** |
| 修复后稳态 | ≤16ms |

⇒ **成本几乎全在「整棵列表重画」**，不在网络、不在 SFTP。

⚠️ **测绘制成本必须 `win.present()`**：面板没挂到窗口时 GTK 根本不画 ⇒
测出来的 12ms 是假的（这个坑踩过一次）。

⇒ 堡垒机的远程桌面再乘一个 X 往返 ⇒ GTK 主循环被拖死（**菜单也卡**）⇒
积压的事件一起回放 ⇒「2% 突然跳到 80%」。

### 根因 ②：传输与面板请求挤同一条 SFTP 通道

SSH 的**通道窗口是每通道一份** ⇒ 大文件上传吃光它那条通道的窗口 ⇒ 服务端
连目录列表都回不来 ⇒ 刷新/预检/目录跟随全排在上传后面。

⇒ `backend/conn.rs` 新增 `transfer_backend()`：传输**另开一条 `subsystem
sftp` 通道**（**同一条 SSH 连接** ⇒ 不多认证、不多 TCP ⇒ **守住「一台服务器
一条连接」这条硬约束**）；老服务器开不出来就退回共用那条。

### 修法

- `panel/pane.rs`：`set_progress`/`set_terminal`/`tick_fade` **不再**
  `pane.view.queue_draw()` —— `model.set` 已经让**那一行**变脏 ⇒ GTK 只重画
  那一行；新增 `repaint_xfer_rows()`（只把 `COL_XFER==true` 的行标脏）与
  `queue_draw_full()`（整表重画的**统一入口** + 计数 `Pane::full_redraws`）。
- `panel/mod.rs`：`sync_xfer_anim`（60ms 动画帧）改调 `repaint_xfer_rows`。
- `panel/worker.rs`：**进度事件限流 100ms** —— 数字仍然**每次**写进
  `shared`（不丢进度），只是 UI 通知变稀 ⇒ 与网速解耦（100Mbps 下本来能到
  195 次/秒）。

### 断言怎么写（这轮最值钱的一条）

**别**断言「我们自己调了几次 `queue_draw`」⇒ 改回整表重画照样绿。

**要**断言**与实现无关的外部量**：数**单元格实际绘制次数**
（`panel/namecell.rs` 的 `draw_count()`）⇒ 按「每秒 ≤40 次」卡。

实测：**5727 次 → 39 次**（147 倍）；整表重绘 4 次/上传。

⚠️ **新断言必须先反向验证**：把 `sync_xfer_anim` 临时改回
`p.view.queue_draw()` ⇒ 5727 > 上限 694 ⇒ 退出码 101 ⇒ 断言真抓得住
（随后已恢复修复版）。

### 冒烟

- `examples/xfer_stall_smoke.rs`：限速 TCP 代理 + 真实上传 + 量主线程阻塞
  与定时器抖动（三条断言）。
- `examples/xfer_pulse_smoke.rs`：呼吸动画的像素差 —— ⚠️ **呼吸相位按时间
  算** ⇒ 只截两张图比像素会**随机翻红**（0.00027~0.00154）⇒ 改成
  `max_diff_over_cycle()`：扫完一个 1.6 秒周期取相邻差的最大值。

## 核心规则（改代码前必读）

1. **Rust 主线开发，C 版不再维护**（用户 2026-11-01：「我们现在不需要 c 版本
   了」，但 C 源码**保留**作历史依据与活体对照）。新功能只在仓库根的 Rust 代码里
   做，登记到 `tools/FEATURE_PARITY.md` 的"Rust-only"清单（该文件**已入库**，
   2026-11-11 起）；不再反向移植 C。
   ⚠️ **但 CLI 的外部特性是有契约的**：见下面「命令行参数契约」一节——
   那是**从 C 版源码逐条抄下来的**（C 版 args.c / main.c，2026-11-01 冻结）。
2. **CLI 与 PuTTY 逐字节兼容**（堡垒机无缝替换的生命线）：参数解析、
   `--print-config` 输出、退出码、askpass 协议（`--askpass` + `SSH_ASKPASS`
   指向自身 + `PUTTYPLUS_PW`）、spawn 的 ssh/plink argv 与环境变量。
   ⚠️ **改 `src/args.rs` 之前先跑 `bash tools/cli_contract_check.sh`
   （对照 73 条黄金基准，PASS=73 FAIL=0 才算绿），改完再跑一遍**——基准是
   C 版二进制生成的，对不上就是破坏了契约。
   `tools/compat_check.sh` 是**活体对照**（Rust ↔ C 二进制，81 项：CLI
   矩阵 + 假 ssh/plink 沙箱的 argv/env + selftest 全流程）；C 版删掉后它会
   报「缺 C 版二进制」⇒ **契约检查（cli_contract_check.sh）接手**。
3. **用户可见文案全部中文，两版逐字一致**（对照 c/ 源码里的字符串）。
4. **密码安全**：不进 argv/日志；连接管理器 spawn 用 `PUTTYPLUS_PW_SESSION`
   环境变量传递；Rust 侧用 Zeroizing，释放前抹零。
5. **atoi 语义**（契约要求）：端口等数值原样透传（可负/可溢出，不 clamp）。
   ⚠️ 这是 C 版 `atoi()` 的行为，**不是 bug**，别"顺手修正"。
6. **测试不许写用户真实配置**：数据库连接配置在 `databases.ini`、连接列表在
   `connections.ini`、SQL 执行历史在 `history.ini`、**列显示设置在
   `columns.ini`**，跑示例/冒烟前用
   `PUTTYPLUS_DB_INI` / `PUTTYPLUS_CONN_INI` / `PUTTYPLUS_HIST_INI` /
   **`PUTTYPLUS_COLS_INI`** 指到临时文件（退出时删掉）。否则一次冒烟就在用户
   配置里塞一条假连接（已经塞出过 `@:0` 这种空会话键的组）。
7. **一个功能一个提交**：每实现一个新功能并测试通过后，立即 git 提交（提交
   信息用中文、说明功能与阶段号，风格对齐现有提交）。不要攒多个功能一起提交，
   也不要把测试没通过的代码提交进去。
8. **传输完不弹窗**（用户 2026-10-27 明确要求）：批次结束只发桌面通知，
   **通知发不出去时也不许弹窗**——桌面没有通知守护进程是最常见的情况，
   兜底成模态窗等于"每传完一批弹一次"。改为在状态栏写一句。失败明细在
   通知正文里，**点通知**才展开汇总窗（用户主动点的，不算"传输完弹窗"）。
   ⚠️ `Notify` 是**异步**的，"守护进程拒收"稍后才回主线程——**最容易弹窗
   的正是那一刻**，所以测这条的单测必须**泵主循环**再数顶层窗口。
9. **⚠️ 往用户机器上装任何东西前必须先请示**（用户 2026-10-29 明确要求）：
   apt / dpkg / pip / cargo install / 装 systemd 服务 / 装数据库服务端……
   一律先问，用户同意再装。装完要**卸干净**（`apt-get purge` + 清残留目录、
   用户、组），别留半截。已有工具（如 `postgresql-client`）可以随便用，
   但**不许顺手升级/重装**。
   ⚠️ **「打包」和「装进系统」是两件事**（用户 2026-10-29 补充）：
   - **打包**（`bash tools/build_deb.sh`，产物落在仓库根）→ 以前是
     可以直接做，**但 2026-10-30 用户改了规矩：测试/开发阶段一律
     不打包、不安装**，要交付时他会另行通知 ⇒ **别再主动打包**
     （打包一次要一两分钟，拖慢迭代）；
   - **装进系统**（`sudo dpkg -i` / `apt install` / 任何写到 `/usr/bin`、
     `/usr/share`、dpkg 数据库的操作）→ **必须先问用户**，问完再动。
   ⛔ **2026-10-29 踩过**：交付阶段 54 时打完包直接 `sudo dpkg -i` 装进系统，
   没先问，被用户当场指出。**别拿"上次也是这么干的"当理由**——交付说明里
   写着"记得重新打包安装"，那说的是**打包**，不是**安装**。
   ⚠️ **2026-10-30 再踩一次**：测试期间顺手打了包（用户正要"加快速度"）。
   ⇒ **测试阶段的默认动作是：改代码 → 跑测试/冒烟 → 截图 → 提交，
   不打包、不安装。**
10. **⚠️ 传输/动画期不许整表重画**（用户 2026-11-11 报「上传时界面整体
    卡死」）：进度更新、呼吸动画、任何每帧变化 ⇒ **只重画受影响的那一行/
    那一个控件**，不许 `view.queue_draw()` 把整棵列表重画一遍。
    ⇒ 整表重画 = 每个可见行新建 Pango 布局 + 量文字宽度 = **主线程一帧
    59ms**（实测）⇒ 远程桌面再乘一个 X 往返 ⇒ GTK 主循环被拖死 ⇒
    **菜单也点不动** ⇒ 积压事件一起回放 ⇒「进度 2% 突然跳到 80%」。
    ⚠️ 详见下面「上传时界面整体卡死」那一节。
11. **⚠️ 断言要选「与实现无关」的度量**（同一次事故的教训）：别断言
    「我们自己调了几次 `queue_draw`」⇒ 改回坏行为照样绿；改成断言
    **实际绘制次数**（`panel/namecell.rs` 的 `draw_count()`）这类
    **外部可见**的量。⚠️ **新断言必须先反向验证**：把修复退回去，
    确认断言当场变红。

## 命令行参数契约（C 版基线，2026-11-01 冻结）

⚠️ **C 版不再维护，但它的命令行外部特性是"堡垒机无缝替换 PuTTY"的���命线**
（用户 2026-11-01：「我们现在不需要 c 版本了，但是你需要把 c 版本的命令行
参数写到 AGENTS.md，每次要严格遵守，确保命令行的外部特性不变」）。

⇒ **这份契约就是从 C 版源码逐条抄下来的**（`c/src/args.c` + `c/src/main.c`
+ `c/src/term.c`）。C 版删掉之后，契约靠**下面两样东西**活着：

| 东西 | 位置 | 作用 |
|---|---|---|
| 契约条文 | 本节 | 人读的规范（参数表 + 细则） |
| **黄金基准** | `tools/cli_contract/golden/*.txt`（73 条） | C 版二进制**生成一次**的 `--print-config` 输出（stdout + stderr + 退出码） |
| **检查脚本** | `tools/cli_contract_check.sh` | Rust 版逐条对照黄金基准 ⇒ **PASS=73 FAIL=0** |

```bash
bash tools/cli_contract_check.sh          # 检查 Rust 版（日常跑这个）
bash tools/cli_contract_check.sh --gen    # 用 C 版二进制重新生成基准
                                               #（只在 C 版还在时需要）
```

⚠️ **改 `src/args.rs` 的规矩**：改之前跑一遍（先绿）→ 改 → 再跑一遍
（**必须还是 PASS=73 FAIL=0**）。对不上只有两种可能：① **破坏了契约**
（必须改回来）；② **故意扩了对外行为**（那要先想清楚"堡垒机那边的 PuTTY
调用方会看到什么"，并同步改本节 + 重新生成基准）。

### 参数总表

| 参数 | 带值 | 语义（= C 版行为） | 落到哪 |
|---|---|---|---|
| `-ssh` `-telnet` `-rlogin` `-raw` `-serial` | 否 | **协议开关** | `proto` |
| `-N` | 否 | 不开 shell | `no_shell` |
| `-t` | 否 | 强制分配 pty | `force_pty` |
| `-T` | 否 | 禁用 pty | `no_pty` |
| `-4` / `-6` | 否 | 强制 IPv4 / IPv6 | `ipv4` / `ipv6` |
| `-C` | 否 | 压缩 | `compress` |
| `-v` | 否 | 详细 | `verbose` |
| `-batch` | 否 | 批处理（不交互） | `batch` |
| `-A` `-a` `-1` `-2` `-no-antispoof` `-share` `-noshare` `-beep` `-logappend` `-noagent` `-agent` | 否 | **接受但忽略**（**不告警**） | — |
| `--` | 否 | 选项结束，之后全是位置参数 | `end_of_opts` |
| `-P <port>` | 是 | 端口（**atoi 语义**） | `port` |
| `-l <user>` | 是 | 登录用户 | `user` |
| `-pw <pw>` | 是 | 密码 | `password` |
| `-pwfile <f>` | 是 | 密码文件：**只取第一行**，去掉行尾空白（含 `\r`） | `password` |
| `-i <key>` | 是 | 私钥路径（开头的 `~` / `~/` 展开成 home） | `keyfile` |
| `-m <f>` | 是 | 远程命令 = 文件**全部**内容 | `remote_cmd` |
| `-L <spec>` | 是 | 本地转发 | `forwards` += `L:spec` |
| `-R <spec>` | 是 | 远程转发 | `forwards` += `R:spec` |
| `-D <spec>` | 是 | SOCKS 代理 | `forwards` += `D:spec` |
| `-load <session>` | 是 | 载入 PuTTY 会话（`~/.putty/sessions/<name>`） | `session_name` |
| `-proxycmd <cmd>` | 是 | 代理命令 | `proxycmd` |
| `-hostkey <s>` | 是 | **接受但忽略** | — |
| `-sercfg <s>` | 是 | 串口参数（**C 版忽略**；Rust 版真的用） | `serial_*` |
| `-title <t>` | 是 | 窗口标题（堡垒机 launcher 传「账号@设备IP」） | `title` |
| `-title=<t>` | 连值 | 同 `-title <t>` | `title` |
| `-loghost <s>` | 是 | **接受但忽略** | — |
| `-sessionlog <f>` | 是 | **接受但忽略** | — |
| **位置参数** | — | 第一个 = `[user@]host` 或会话名；**其后全部** = 远程命令 | `host`/`user`/`remote_cmd` |
| **未知参数** | — | 告警「忽略未知参数: X」并跳过，**且不吞掉后面的值** | — |

### 细则（最容易踩的几条）

1. **端口是 atoi 语义**：`atoi()` 原样透传，**不 clamp**。`-P 99999999999`、
   `-P abc`（得 0 ⇒ 走默认端口）、`-P -1` 都照旧透传。
2. **未知参数不吞值**：`puttyplus -nosuch h` ⇒ `host=h`（`h` 不被 `-nosuch`
   吃掉）——这是堡垒机多传 GUI 专用参数时程序不炸的关键。
3. **缺值只告警不退出**：`-P`（后面没值）⇒ 告警「参数 -P 缺少值，已忽略」，
   继续解析后面的参数。
4. **位置参数的顺序**：第一个位置参数是 host（或会话名），**其后所有**位置
   参数拼成远程命令（空格分隔）；`--` 之后的位置参数也一样。
5. **`user@host` 只在 host 还是空时拆**；`-load` 载入的会话里的 `HostName`
   会被**命令行上的 host 覆盖**（会话给的端口/用户保留）。
6. **默认值**：端口为 0 ⇒ ssh=22 / telnet=23 / rlogin=513 / 其它=22；
   用户为空 ⇒ 当前登录用户；密码为空 ⇒ 读环境变量 **`SSHPASS`**（sshpass
   约定），命令行 `-pw` 优先。
7. **`-pwfile` 只取第一行**（行尾 `\r` 也去掉）；**`-m` 取文件全部内容**。
8. **`-i` 的 `~` 展开**：只有开头的 `~` 或 `~/` 展开；`~user/x` **不展开**。
9. **`-title=` 连值**与 `-title x` 等价；`-title=`（等号后为空）⇒ 标题为空串。
10. **解析告警不进 `--print-config`**：告警只显示在 GUI 的「以下 PuTTY 参数
    被忽略或需要留意」对话框里。
11. ⚠️ **`-serial` 是不带值的协议开关**（串口设备路径走**位置参数**）：
    `puttyplus -serial -P 115200 /dev/ttyUSB0`。⚠️ Rust 版曾经让它**吃掉
    后一个参数**⇒ `-P` 被当成设备路径、端口与设备全乱（阶段 95 修掉，
    契约检查的第 2、7 条用例就是钉这个的）。

### `--print-config` 的输出格式（逐字节固定）

```
proto=<proto> host=<host> port=<port> user=<user>
password=<set|-> keyfile=<keyfile|-> session=<session|->
title=<title|->
flags: N=<0|1> t=<0|1> T=<0|1> v4=<0|1> v6=<0|1> C=<0|1> v=<0|1> batch=<0|1>
forwards=<个数> remote_cmd=<set|->
  fwd[0]=<L|R|D>:spec
```

⚠️ **密码只打 `<set>`，绝不打明文**；`remote_cmd` 同理只打 `<set>`。
⚠️ `--print-config` **只在 argv[1] 那个位置才触发**：`puttyplus h --print-config`
不会打印，而是照常开 GUI。

### argv[1] 的特殊模式（C 版就有）

| argv[1] | 行为 | 退出码 |
|---|---|---|
| `--print-config` | 打印上面那几行，**不启动 GUI** | 0 |
| `--sftp-selftest <host> <port> <user> [pw]` | SFTP 自检 | 0/1 |
| `--askpass` | **不启动 GUI**：把 `PUTTYPLUS_PW` 的内容打到 stdout（后面跟一个 `\n`） | 0 |

- `--sftp-selftest` 缺参数 ⇒ stderr 打印「用法: `<argv0>` --sftp-selftest
  <host> <port> <user> [password]」，退出码 **2**。
- **askpass 协议**：ssh 会调 `SSH_ASKPASS=<puttyplus 自身路径> puttyplus` ⇒
  程序发现**自己就是 `SSH_ASKPASS` 指向的那个路径**时**自动进入 askpass
  模式**（不需要显式 `--askpass`）；输出密码后立刻 `_exit(0)`，不做任何清理。
  ⚠️ 判定靠 **`/proc/self/exe` 与 `SSH_ASKPASS` 的 realpath 相同**；显式
  `--askpass` 与环境变量隐式**两条路都要支持**。

### spawn 出去的子进程 argv（终端侧）

- **ssh**：`<ssh> [-p <port>] [-l <user>] [-i <key>] [-L|-R|-D <spec>]…
  [-N][-t][-T][-4][-6][-C][-v] [-o BatchMode=yes] -o
  StrictHostKeyChecking=accept-new -o ServerAliveInterval=30 -o
  ServerAliveCountMax=3 [-o ProxyCommand=<cmd>] -- <host> [remote_cmd]`
  - ⚠️ **回环目标多两条 `-o`**（`UserKnownHostsFile=/dev/null` +
    `GlobalKnownHostsFile=/dev/null`，插在 `StrictHostKeyChecking` 之后）：
    **只在 host 是回环时加**（`backend::conn::is_loopback`）⇒ 普通主机的
    argv **一个字节都不变**。
    ⇒ **为什么**：堡垒机在本地开**随机端口**、隧道**按访问轮换主机密钥**
    ⇒ OpenSSH 按 `[host]:port` 记的 known_hosts **必然对不上** ⇒
    `REMOTE HOST IDENTIFICATION HAS CHANGED` + **硬失败（255）**
    ⇒ 用户报「在堡垒机上分屏失败」。
    ⇒ 与**进程内那条路**（russh，`conn.rs` 的 `is_loopback` ⇒ 回环直接
    放行）和 **C 版**（`sftp_backend.c` 同样跳过）保持一致。
    ⇒ ⚠️ **`StrictHostKeyChecking=no` 不是解法**：实测它照样刷那句吓人的
    警告（只是不失败）⇒ 必须是「压根没有 known_hosts 可查」。
- **plink**：`<plink> -<proto> -P <port> [-l <user>] [-pw <pw>] [-i <key>]
  [-L|-R|-D <spec>]… [-N][-t][-T][-4][-6][-C][-v][-batch] <host> [remote_cmd]`
- ⚠️ **端口是 22 时不加 `-p`**（C 版 `if (o->port != 22)`）——别"顺手改成总是加"。
- ⚠️ ssh 的 `-o` **顺序固定**：`BatchMode=yes`（仅 `-batch` 时）→
  `StrictHostKeyChecking=accept-new` → `ServerAliveInterval=30` →
  `ServerAliveCountMax=3` →（有代理时）`ProxyCommand=…`。
- ⚠️ plink 的密码是**明文进 argv**（plink 就这样）⇒ Rust 版默认走 ssh；
  用 plink 时密码**只**经 `PUTTYPLUS_PW_SESSION` 环境变量传，别自己拼 `-pw`。

### Rust 内部的扩展（**不属于对外契约**）

这几个是 Rust 版自己用的，**别当成 PuTTY 兼容面**：

- `--bridge-telnet <host> <port>` / `--bridge-serial <设备> [sercfg]`：终端桥
  子进程模式（telnet / 串口会话的 VTE 子进程就是本程序自己）。
- `--sftp-selftest` 的**第 7 个参数**（`argv[6]`）= ProxyCommand（Rust 扩展，
  C 版没有）；缺省不影响。
- 连接管理器的「在新窗口打开」会 spawn 自身，密码走 `PUTTYPLUS_PW_SESSION`
  环境变量（**不进 argv**）。

## 环境坑（本机特有）

- `rustc`/`cargo` 的 rustup 代理被沙箱干扰（报 "unknown proxy name"）：命令前加
  `export PATH=$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH`。
- `import`/`pkill`/`head` 等被 ZCode.AppImage 包装：截图用 `/usr/bin/scrot`，
  杀进程用 `kill $(pgrep ...)` 而非 pkill；`make` 输出前缀显示 ZCode.AppImage
  但功能正常，"无需做任何事"常表示前一次调用已构建成功。
- python 脚本做批量文本替换后会**静默失败于结构不匹配**（发生过两次）：
  替换后必须 grep 验证落地。

## gtk4-rs 0.11 已踩过的坑

- ⚠️ **GTK4 的 CSS 选择器匹配的是「节点名」不是控件类型**（2026-11-11 查 GTK
  4.25 源码 `gtk/gtklabel.c` 的 `gtk_widget_class_set_css_name` 得出）：
  ① 按钮里的文字是**一个独立的 `label` 节点** ⇒ 我们主题里那条 blanket 规则
  `label { color: @pp_fg }` 给它一个**显式**颜色 ⇒ 按钮自己的
  `color: @pp_on_accent`（**只是继承**下去）被盖掉 ⇒ 浅色主题下 = 强调蓝底 +
  深色字（用户 2026-11-11 报：首选项里选中的「标准」「中」两个按钮）。
  ⇒ **凡是给容器设了状态色的，容器里的 label 都要单独再定一条**
  （`button:checked label { … }`）。
  ⚠️ `GtkToggleButton` 的节点名就是 `button`（`gtk/gtktogglebutton.c`）⇒
  `button:checked` **匹配得上**，只是颜色没传到里面的文字；
  ② **自己画的 cell renderer，取色要自己算对状态**：文件列表的文件名列是
  我们自己画的（`panel/namecell.rs`）⇒ 颜色由我们取 ⇒ 少算 `:focus` ⇒ 拿
  `:selected` 的深色字画在 GTK 的实心强调底上 = 蓝底黑字（其余列是 GTK 自己
  画的 ⇒ 是白的 ⇒ 只有文件名看不清）。
  ⚠️ `:focus` **只在树视图真有键盘焦点时**才算（跟 GTK 的
  `gtk_cell_renderer_get_state()` 一个口径）⇒ 否则「点别处之后」光标行底色
  退回浅色、字还是白的 ⇒ 白字压浅底 = 看不见。
  ⚠️ **CSS 单测别只 `contains()` 半句话**（注释里也写着那句话 ⇒ 删掉规则
  单测照样绿 = 假阳性）⇒ 要匹配**规则块本身**；
- ⚠️ **GTK 的 `mix()` 权重是「第二个颜色的占比」**：`mix(#ff0000, #0000ff, 0.25)`
  = **75% 红 + 25% 蓝** ⇒ 想要「淡强调底」得写 `mix(accent, bg, 0.86)`。
  ⚠️ 别当成「第一个颜色的权重」（差一次就反了）。实测法（不用截图）：
  CSS 里 `@define-color x mix(...)` + `style_context().lookup_color("x")`
  ⇒ 直接读出算出来的 RGBA；
- **GTK4 的 `TreePath` 没有 `parent()`**（GTK3 有）：找父行得用
  `path.indices()` 拿下标，再 `TreePath::from_indices(&idxs[..len-1])` 重建父路径；
  写 `child.parent()` 会编译到 `MountOperation::parent` 上去（报"方法名不匹配"）；
- **`cargo test --lib` 里的 GTK 用例会静默跳过**：测试跑在 libtest 子线程上，
  `gtk::init()` 在非主线程起不来 → 菜单/CSS/通知那些用例在无头环境下全是
  「跳过：本机没有显示服务」。**GTK 相关的断言必须放进 example 冒烟**
  （冒烟在主线程跑，`gtk::init()` 成功），别指望单测兜底；
- CellRenderer 必须 `column.pack_start()` 否则列空白（不报错）；
- **`pack_start()` 不会绑定 `text` 属性**：还要显式
  `column.add_attribute(&renderer, "text", col)`（列号是 i32），否则单元格
  全空白而列头照常显示（看着像"查出来是空的"）；
- **TreeView 对"没有子节点的行"不画展开箭头，`expand_row()` 也是静默无效**
  （不发 `row-expanded`）；要靠"展开才查"做懒加载，得先给节点挂一个占位
  子节点（如「载入中…」），真数据回来再摘掉；**摘掉占位后必须把父行重新
  展开**（摘占位会让 GTK 认为该行没孩子从而折叠它，而展开一个父行折叠的
  行同样无效）；
- **TreeView 的展开箭头和缩进画在第一列**：越深的节点留给第一列文字的宽度
  越少，第一列只有两三个字的内容会缩成 "…"（`min_width` 在这条路径上不
  顶用，实测把 `min_width` 设成 200 才会变宽）。类型信息要并进第二列；
- ⚠️ **`TreeViewColumn::set_widget()` 只换列头、不动列内容**（2026-10-29 实测，
  探针截图 `/tmp/tvc_probe*.png`）：可以放心用它做「列名 + 类型小字 + ⋮」
  这种自定义列头（dbx 风格），**列数据照常渲染**。
  ⚠️ 但 **`set_title()` 也要留着**：`set_widget` 只影响显示，`title` 属性
  还得有值——导出、过滤条件编辑器、列类型映射都靠 `col.title()` 拿列名，
  少写一句 `set_title` 那里就全是空串。
  ⚠️ 自定义列头 widget 里点子控件**仍会冒泡**到列头按钮（= 排序照样触发），
  得自己记个标志把那次排序吃掉；
- **深遍历 `GtkTreeStore` 不能用 `iter_next`**：在子行上调 `iter_children`
  会让**上层**的 iter 失效，递归两层后顶层 `iter_next` 直接返回 false，
  树只走了一半（测试钩子据此误报"树上没挂出子节点"，差点当成真 bug 去改树）。
  可靠写法是**按路径走**：`TreePath::from_indices(&[i])` + `append_index(i)`
  + `store.iter(&path)`，每步从根重新定位；
- ⚠️ **清空树时别把根节点也抹掉**：`DbTree::clear()` 一开始是
  `store.clear()` + `nodes.clear()`——**根节点也没了**，之后 `reload()`
  找不到根节点就直接 return，**树再也长不回来**。症状是「CSV 导入完
  对象树变空白」——刷新按钮也不管用（没有根节点可刷）。现在根节点的标签
  单独记一份（`root_label`），`reload()` 找不到根节点时按它重建；
  导入完要用 `reload()` 而不是 `clear()`。
- ⚠️ **`MenuButton::set_popover()` 自己就会给 popover 设父级**：别再手动
  `pop.set_parent(&btn)`——会报 `gtk_widget_set_parent: assertion
  'gtk_widget_get_parent (widget) == NULL' failed`（同一个 widget 被设两次
  父级）。这是 `set_parent` 那一类断言的又一个变体。
  → 统一菜单构件 `menu.rs` 里因此分了两个：`popover(parent)`（自己挂父）
  与 **`popover_detached()`（不挂父，给 `MenuButton::set_popover()` 用）**。
- ⚠️ **菜单统一后只有一个构件：`src/menu.rs`**。别再手搓
  `Button::with_label` + `flat` 类——那样**鼠标划过不高亮**（普通按钮没挂
  Motion 控制器；Popover 里 CSS `:hover` 常只更新 PRELIGHT 不刷背景，高亮
  全靠 `menu::attach_hover()` 显式加 `pp-menu-hover` 类）。
  原生 `gio::Menu` 模型（`PopoverMenu` / `PopoverMenuBar`）也不行：没图标、
  hover 样式对不上。改用 `menu::item(icon, label, accel)` +
  `menu::body()` + `menu::popover_detached()`。
- ⚠️ **`MenuButton` 自带的小三角压不掉**（GTK 4.18 里那不是普通 image
  节点，CSS `opacity: 0` 无效）。菜单栏要干净就**用普通 `gtk::Button`**，
  弹层自己 `set_parent(&btn)`，点按钮 `pop.popup()`。
- ⚠️ **CSS 后代选择器会漏进弹层**：弹层是按钮的子控件，
  `button.pp-menubar-item image { … }` 会把弹层里的**菜单图标一起命中**
  ——图标凭空消失（2026-10-28 实测）。给菜单栏写样式时别用后代选择器碰
  `image`。
- ⚠️ **菜单项按钮没有 `label` 属性**：`menu::item` 建的按钮 child 是一层
  Box（图标 + 文字 Label），`b.label()` 返回 `None`。e2e 里按文案找菜单项
  要走后代 Label（`db_ui_e2e.rs::button_text`）。
- ⚠️ **菜单项只建一次，文字/图标要后改**（阶段 58）：右键菜单的项在
  `install_row_menu` 里建好反复用，状态变了（「启用转发」↔「停用转发」）只能
  后改——`menu::set_item_label` / `menu::set_item_icon` 钻进按钮的 child Box 里
  找带 `pp-menu-label` / `pp-menu-icon` 类的那个 Label / Image。
- **回调里别再借 `RefCell`**：`changed` 回调触发时上层往往已经持着借用
  （`change_proto` 改字段 → 回调 → 再借同一个 RefCell = panic）。把要共享的
  状态用 `Rc<Cell<bool>>` / `Rc<RefCell<String>>` 单独拎出来，回调只抓它；
- **RefCell 的借用别跨 `match` / `if let` 撑着**：`match x.borrow().as_ref()`
  里那个临时 `Ref` 会活到整个 match 结束，在分支里再 `borrow_mut()` 就是双重
  借用 → panic。症状很迷惑：第一次操作"看着成功"，**第二次的结果刷不进去**，
  同时 GTK 报 `gtk_box_append: assertion 'gtk_widget_get_parent (child) ==
  NULL'`（东西建完没存下来，第二次又建一个，同一个 widget 被塞进第二个容器）。
  先 `let v = x.borrow().clone();` 把借用结束掉；
- **同一个 widget 别 append 进两个 Box**：GTK4 会报 `gtk_box_append: assertion
  'gtk_widget_get_parent (child) == NULL' failed`（第二个容器抢不走）——不是
  panic，只是那一份没显示出来，很容易漏看；
- **包内容的 `ScrolledWindow` 必须 `set_vexpand(true)`**（2026-10 实测）：
  不撑的话滚动窗口只按**内容高度**占位——树里当时只有一行，整个控件就缩成
  一行高，后面挂上来的子节点**全被裁掉**（模型里有、屏幕上没有，看着像
  "数据没加载出来"）。用户报的「连上数据库后左侧只显示一行」就是这个：
  `dbui.rs` 的 `tree_sw` 漏了 `vexpand`。**凡是"里面放列表/树"的滚动窗口
  都要撑满**（`set_size_request` 只管最小高度，不管"要不要占满剩余空间"）；
- **e2e 查模型 ≠ 查视图**：模型里明明有「public」这一行，屏幕上却一行都
  没有——只断言**模型行数**永远查不出这类问题。查**视图**要用
  `TreeView::visible_range()`（GTK4 没有 `get_visible_range` 的老写法，
  直接 `visible_range()`），断言"可见范围的最深路径 ≥ 1"；
- **背景 `DrawingArea` 必须给尺寸**（2026-10 实测）：它默认**不申请任何
  空间**，放进容器里就是 0×0——画的东西一条都看不见（ER 图的连线全没了，
  表框照常显示，看着特别像"图只有表没有关系"）。要给 `set_size_request`。
- **cairo 的"玩具文字"接口只认 ASCII**：`Context::show_text` 画中文会变成
  一堆问号。**要显示中文就用真控件**（Label / TextView）——字体、主题色、
  省略号全都自动对。所以 ER 图的表框是真实 Label，连线才用画布画。
- **拖动容器里的子控件要用 `move_`，不要 `put`**：`gtk::Fixed::put` 会把已经
  在里面的控件 unparent 再挂回去，**正在进行的拖动手势会被打断**；
  `move_` 只挪位置。
- **`gtk4-rs` 没绑 `n_press`，但信号参数里有**：`GestureClick::connect_pressed`
  的回调签名是 `(gesture, n_press, x, y)`——第二个参数就是按压次数，
  判双击用它（别去找 `n_press()` 方法，找不到）。
- **e2e 截图前要装主题**：自定义 CSS 类（`add_css_class`）只在
  `theme::apply()` 之后才生效。e2e 不装主题的话，截图里那些自定义样式的
  控件全是"没上妆"的样子（ER 图的表框没有边框和表头底色），**截图就白拍了**。
- DropTarget 构造只收单个 GType，多类型挂多个 target；
- **自绘 chip 标志别加 `flat`**：Adwaita 的 `button.flat` 是
  `background-color: transparent`，优先级比自定义 `.pp-chip` 高——加了 chip
  的底色就没了，标志变成"一串白字"（看着跟没标志一样）。换状态配色时也
  **别把基础类摘掉**：`.pp-chip` 身上带着底色、圆角、内边距，只该换
  「状态类」（`pp-chip-ok` / `-warn` / `-err`）。见 `vaultui::badge_button`；
- **Label 在页签里会被压成「…」**：只设 `max_width_chars` 不够——label 的最小
  宽度是 0，notebook 分页签时把它挤到只剩省略号（三个页签全变成「… ×」，
  用户认不出哪个是哪个）。必须给 `width_chars` 一个**最小宽度**；而且宽度要
  按**显示列**算，**一个汉字占两列**（`width_chars(4)` 只放得下两个汉字，
  「数据库」会被截成「数…」）。子标签页签头见 `panel/subtabs.rs::display_width`；
- glib 0.22 已移除 `MainContext::channel`，用 `async_channel` + 主线程
  `spawn_local` 循环；
- ⚠️ **改容器结构前先查"谁依赖某个控件的父容器是什么类型"**：阶段 12 把
  终端改成**子标签**后，终端面板的父容器从 `Paned` 变成了 notebook 的页面
  容器（`Box`），而 `split_index` 的做法是「把目标面板塞进新 `Paned`、再把
  新 Paned 换回原位」——**要求父容器本身是 `Paned`**，拿不到就**静默
  `return`**（不报错、不 panic）。于是「左右分屏」「上下分屏」点了跟没点
  一样，**拖了一年半才被用户报出来**。这类"把控件塞进新容器再换回原位"的
  实现，父容器类型是**隐式契约**，改结构时必须一起改。
  - 现在的做法：终端子页签内容套一层**单子控件的 `gtk::Paned`**
    （`tabs.rs::term_container`）——GTK4 里单子控件的 Paned 会让子控件占满
    全区，外面看不出来。
- ⚠️ **「只剩一块就不动」的守卫会变成永远 no-op**：`close_pane_index` 原本
  「只剩一块面板时不动」（没有另一半可顶上来），本意合理；但上游的分屏一旦
  坏掉，就**永远只有一块** → 这条守卫把「关闭当前终端」也一起废掉了。
  **一个功能坏了，另一个跟着坏、且症状完全不同**时，先怀疑共享前提。
- ⚠️ **容器树要按"拥有者"分组，别按"标签"分组**：阶段 37 之前布局树是
  Tab 级的一棵（`Tab.layout`），**额外终端子页签**（子标签栏「+」新建的）
  的面板不在任何树里 → 在那些页签上点分屏也是没反应。现在改成
  `Tab.layouts: Vec<(u64, PaneLayout)>`（每个终端子页签一棵）。
  ⚠️ 关掉一个子页签时要**把它的树一并摘掉**，并对树里分屏出来的副面板
  `shutdown()`——它们不在 `term_pages` 里，漏了就留下一堆野 ssh 进程。
- ⚠️ **GTK4 的 `GtkPopover` 没有 modal 概念**：那是 GTK3 `GtkMenu` 的东西，
  GTK4 里**没有** `gtk_popover_set_modal`，gtk4-rs 自然也没绑定
  `set_modal`（别照着 GTK3 的经验去写，编译不过）。弹层开着时
  **motion 照常派发到别的按钮上**，`autohide` 默认开着（点别处自动收起）。
  顶栏菜单栏的「悬停切换」（鼠标移到另一个菜单上那个菜单自动打开）就是靠
  给标题按钮挂 **`EventControllerMotion`** 实现的（`menu.rs::MenuBar`）——
  ⚠️ **只接 `clicked` 的话就只能点两下**（用户 2026-10-28 报的就是这个，
  是阶段 36 自绘菜单栏时漏的）。
- ⚠️ **图标按钮的文字要去处有个"三个地方"**（阶段 39）：改成纯图标后，
  ① 按钮自己的 tooltip（必须有，否则用户不知道它是干什么的）；② **界面
  上提到按钮名的说明文字**（工作台空状态原来写「写 SQL 点「执行」」——
  按钮变图标了，提示还指着旧文字就指空了，要改成「点 ▶」）；③ **文档 /
  冒烟里按文案找按钮的代码**（`find_button_by_label` 之类会突然找不到）。
  验证靠 `examples/icon_btn_smoke.rs`：断言按钮的 **child 是 `Image`
  （纯图标）**且**都有非空 tooltip**——少一条界面就悄悄退化回"一排字"，
  还不报错。⚠️ **范围只到按钮条**：弹窗对话框里的「保存修改 / 放弃修改」
  保持文字（表单动作，光看图标分不出来）。
- ⚠️ **调样式要量，别凭感觉**：「宽度太大」这类反馈光看截图很难判断改够
  没有。`examples/ui_shot.rs` 有 `menubar` 模式逐项打印按钮宽度
  （`cargo run --example ui_shot -- menubar`），改前改后各量一次才有数
  （阶段 40：每项 43px→35px，总宽 301→245）。
  ⚠️ **ui_shot 不会自行退出**（结尾 `MainLoop::new(None, false).run()`），
  得用 `timeout 30 cargo run ...` 跑，否则一直挂着。
- ⚠️ **「宽度太大」光调样式是不够的**（阶段 41 的教训）：阶段 40 把
  padding/字号收了一轮，301→245px（-19%），用户直接回「**我看不出变化啊**」，
  卸掉旧包改跑新 build 还是「为什么还不行呢」。**样式微调有天花板**——
  收到"挤成一团"之前顶多 -20%，肉眼分辨不出来。**要让用户一眼看出差别，
  只能动结构**：阶段 41 把顶级菜单从 7 个减到 5 个（编辑并入终端、帮助
  并入工具），总宽 **301 → 160px（-47%）**。
- ⚠️ **菜单表挪到了 lib（`src/topmenus.rs`）**：原来它在 `main.rs` 里，
  `ui_shot` 量宽度只能**手抄一份标题列表**——抄本会随菜单增删漂移
  （阶段 41 菜单从 7 减到 5 时抄本还是 7 项，**量了个寂寞**）。
  现在用 `topmenus::top_menu_titles()` 取**真实标题**，别再手抄。
- ⚠️ **e2e 走测试钩子 ≠ 用户点真实按钮**：用户 2026-10-28 报「数据库的
  提交和回滚按钮都是禁用的，改了数据之后也点不了」——而 e2e 里这段流程走
  的是测试钩子 `dbui::grid_save()`，**直接调内部函数 `do_save()`**，
  **从来不点真实按钮**。钩子绕过了整个按钮层，按钮灰着也照样"通过"。
  ⚠️ **和「e2e 查模型 ≠ 查视图」是同一类毛病**：测的不是用户实际交互的
  那一层，就等于没测。修法：交出**真实按钮**（`dbui::edit_toolbar_buttons()`）
  并断言 `is_sensitive()`。⚠️ **新加的断言要验证过它真抓得住**：临时回退
  修复跑一遍 e2e，确认它**失败**了再还原。
- ⚠️ **可见 ≠ 可点**（同上一条）：`set_visible(true)` 不会让
  `sensitive(false)` 的按钮能点。按钮建出来时 `set_sensitive(false)` 的，
  进了对应状态**必须一起把 sensitive 打开**——否则用户看到的是两个**灰**
  按钮，点了没反应，界面还不报错。
  ⚠️ 但**别乱动别人的 sensitive**：像「编辑 / 插入行 / 删除行」能不能点
  取决于"目标是不是可编辑的表"（视图表不能编辑），由别处按 `editable` 设，
  在工具栏切换函数里覆盖掉会让视图表也能进编辑模式。
- ⚠️ **菜单项与 action 要对账**（`topmenus::missing_actions`）：菜单并来
  并去最容易出的错是"**菜单项还在、action 没了**"——点了没反应、界面
  不报错，排查起来能磨一天。`main.rs` 启动时拿实际注册过的 action 对
  一遍，缺了往 stderr 打一行（`[菜单] 顶级菜单「X」的「Y」引用了未注册
  的 action：Z`）。改菜单时**别把 `act!` 一起删了**——删菜单项不影响
  快捷键，`act!` 是快捷键与菜单的公共底座。
- ⚠️ **交付前记得重新打包**（产物放仓库根，用户自己装或经他同意再装）：
  用户跑的是 `/usr/bin/puttyplus`（dpkg 装的 deb）。改完源码不
  `bash tools/build_deb.sh`，仓库里就没有新包——这正是上一轮
  "看不出变化"的直接原因。⚠️ **打包 ≠ 安装**：`sudo dpkg -i` 属于
  "往用户机器上装东西"，**必须先请示用户**（见核心规则第 9 条）。
- ⚠️ **GtkPopover 的四个脾气（阶段 54 实测，做菜单必读）**：
  1. **默认居中对着锚点**，没有左对齐开关（`set_pointing_to` 只换锚点、
     不改对齐方式）→ 要左对齐只能自己算 `set_offset`（见 `menu.rs` 的
     `base_rel_x` / `POPOVER_TRIM`）。
  2. **`translate_coordinates` 量到的位置含已施加的偏移**（偏移加在表面
     上），拿它当"基准位置"会把菜单又拉回去——**校准时必须减掉偏移**。
  3. **表面在 X11 上是不透明的**：没画到的像素是**黑块**。所以弹层背景
     不能写 `transparent`（圆角外会出现黑角），`box-shadow` 也不能要
     （GTK4 把它画成不透明黑块，整张菜单被 34px 黑框包住）。
  4. **只有真实指针事件才会让弹层 map**：`emit_clicked()` 或直接
     `popup()` 都弹不出来（`is_visible()` 一直是 false）→ 测弹层位置
     必须用 xdotool 挪真指针点。
- Application 必须 `NON_UNIQUE`（C 版无单实例语义，堡垒机连开多窗口）；
- ⚠️ **文字会被容器高度削掉（阶段 60 实测，做任何带文字的控件都得读）**：
  GTK4 的 `GtkLabel` 在**分配高度恰好等于**文字自然高度（ascent+descent）时，
  最后一行字形正好压在绘制边界上（抗锯齿 + 半像素落点）→ **底部那一横被削掉**
  （用户看到的是「左右分屏」的「左」字少一横、顶栏标题下半截被挡）。
  分配高度来自父容器的 `min-height`，**写死的 px 跟不上字号**（中文字体
  fallback 的 ascent/descent 比西文大，用户再调个系统字号就追上来）→
  **高度与字形余量一律用 `em`**（见 `theme.rs` 的 `.pp-menu-item` /
  `button label` 规则）。⚠️ **量余量不能直接看 Label 的 natural**：一旦给
  Label 设了 `min-height`，GTK 报的 natural 就被**抬高成 min-height 那个值**，
  "分配高度 = natural"恒成立 → 得另造一个**同字号、不受 min-height 规则
  影响**的对照 Label 量纯文本高度（见 `examples/menu_fit_smoke.rs`）。
  ⚠️ **别用"文字看着完整"当验收**：开发机的字体余量恰好够、用户机器不够，
  这类问题在本地**复现不了**，只能靠"多字号 + 几何断言"钉死。
- ⚠️ **GTK4 的 CSS 没有 `max-width` / `max-height` 属性**（阶段 62 实测）：
  写了会报 `Theme parser error: No property named "max-width"`，**整条规则
  被丢弃**（连带后面的规则也可能不生效），还会刷
  `gtk_css_node_insert_after: assertion failed`。
  ⇒ **想限制宽度只能靠内容自己收窄**：
  - Label → `set_max_width_chars()`（这就是 Label 版的 max-width）+ `wrap`；
  - 一排按钮 → **竖排**（自然宽 = 最宽的那个按钮），横排的自然宽是几个
    按钮并排，会把父容器一起撑破。
  ⚠️ **`wrap` 单独也不够**：wrap 是按**分配到的宽度**折行的，而父容器
  默认按**自然宽**排版 ⇒ 自然宽就是"不折行时的长度" ⇒ wrap 永远不
  触发。**必须同时给宽度上限**（`max_width_chars`）。
  ⚠️ 顺带记一条 GTK 的死结：`max-width: 100%` 相对的是**父容器的自然
  宽**，而父的自然宽又由子决定 ⇒ 循环 ⇒ 限不住（何况 GTK4 压根没这个
  属性）。
- ⚠️ **GTK4 移除了 `gtk_dialog_run()`**（阶段 63 实测，做任何"同步模态"都要读）：
  gtk4-rs 也没绑它 ⇒ 要等用户选完再往下走，只能自己跑**嵌套主循环**
  （`dialogs::run_modal`：`MainContext::iteration(true)`——`may_block=true` 才会
  阻塞等事件；用 `false` 会**空转烧 CPU**）。
  ⚠️ 嵌套循环里**别的定时器与 worker 回包也会被派发** ⇒ 只在一次性、幂等的
  场景用（关窗确认），别拿它做长流程。
- ⚠️ **「保存后关闭」是异步的，而关闭会杀掉 worker**（阶段 63）：
  保存走 worker 往返，关面板时 `panel::shutdown` 把 `tx` 丢了 ⇒ 不等回包齐了
  就关 = 白存 ⇒ 用"闸门"（登记还差几次回包，齐了再关）。
  ⚠️ **本地文件的保存是同步完成的**（写盘就在 `save()` 里）⇒ 批量保存的"完成
  报告"必须**延后一拍**，否则关闭会插进保存循环中间（面板先被关掉，循环还在
  往已经关掉的编辑器上写）。
  ⚠️ **回包里正借着某个 RefCell 时，完成报告也要延后**（数据库那边借着
  `st.db_ui`，顺着闸门去关面板会再借一次 ⇒ 双重借用 panic）。
- ⚠️ **关窗口时"存完再关"不能顺手 `w.close()`**（阶段 63）：会递归回自己的
  `close-request` 处理器 ⇒ 把「立刻关」与「存完再关」拆成两个回调。
- ⚠️ **`dialogs::style_dialog` 按**文案**认主操作**（阶段 63）：「不保存关闭」
  里也有"保存"两个字 ⇒ 它和「保存并关闭」**一起**被涂成主色（两个蓝按钮）⇒
  危险/主操作**必须自己定角色**，别指望文案匹配。
- russh 0.63 强制 ring 或 aws-lc-rs 二选一后端（体积原因选 ring）；
- russh-sftp 3.x 每请求默认 10s 超时，已放宽到 300s（慢链路大文件必需）。

## 自研扫描器的坑（高亮/解析这类手写游标循环）

- **手写游标循环必须保证每轮推进**：忘了推进 = **原地打转的无限循环**。
  而高亮跑在 **GTK 主线程**上（`panel/fileedit.rs::apply_highlight`），主
  线程一进去就再也回不来——窗口冻结、单核 100%、**零 syscall**，进程既不
  退出也不报错，用户只能杀进程。2026-10-26 办公机现场实锤：`.md` 的代码
  围栏分支原地打转 13 分钟（`panel/syntax.rs::scan_markdown`），伴随症状
  就是"SFTP 通道突然变安静"——那不是远端断流，是客户端自己死了。
- **`find_line_end()` 返回的是 '\n' 本身**，不是"下一行行首"（'\n' 不算进
  span）。拿它当行首，游标就停在 '\n' 上；若 `line_start` 又是 true，下一
  轮 `find_line_end` 返回**同一个下标** → 原地打转。要跳行用
  `next_line_start()`。
- 已有兜底 `panel/syntax.rs::Progress`：每轮检查游标有没有比上一轮前进，
  没前进就**强制走一格**并计数（`Highlight::stalls`）——死循环从此不再可能，
  最坏只是那一格没着色。**新写扫描器一律用 `while pg.step(&mut i, n)`**，
  别写裸 `while i < n`。
- `panel/syntax.rs::tests::扫描器一律不原地打转` 是语料回归测试（覆盖 30
  多种"容易不推进"的输入）。加新语言/新扫描器时把语料补进去。
- 扫描异常时 `apply_highlight` 会**整份放弃高亮**（宁可没颜色，也不贴半吊子
  的着色），并往 stderr 打一行 `[高亮] … 扫描异常`。

## 「用户可见文案」lint 的坑（阶段 93/94 记）

- ⚠ **lint 扫描器原来把「又长又带 `{}` 的界面文案」当成 CSS 放过了**：
  `src/lib.rs` 的 `user_visible_strings()` 有个「这段文字像 CSS 吗」的启发式，
  原来的判据是「长度 > 200 **且** 含 `{` **且** 含 `}`」⇒
  **长 `format!` tooltip（正好有 `{}` 占位符）整个被跳过** ⇒
  「`**`」「远端」「过时的 asciicast 版本」这些 lint **从来没见过**它们。
  用户 2026-11-01 报「录屏的 tooltip 与现在的真实情况显然已经是不符合了」
  ——那句过时的 tooltip 恰恰就是长 `format!`，所以历次 lint 一次都没报出来。
  ⇒ 判据已改成「**有『选择器 + `{`』的行**才算 CSS」（本项目的 CSS 全在
  **原始字符串** `r#"…"#` 里，本来就扫不到）。改完**立刻**扫出一个真 bug：
  `panel/fwdmgr.rs` 的确认框里写着 Markdown 的 `**清空**`（GTK 不渲染
  Markdown ⇒ 用户看到的是**字面的星号**）。
- ⇒ **写界面文案时**：tooltip 一律写成**公开函数**（`term::rec_menu_tooltip`
  那样）⇒ 纯逻辑单测能直接断言它的内容，不必等 GTK 冒烟。
- ⇒ **写 lint 时**：坏词要**拼出来**（`["录到的", "按键"].concat()`），
  否则 lint 会**扫到自己**（源码里的那个字面量自己就命中了）。

## GTK4 里「居中 / 等宽」的几条实测（阶段 96 记，做任何"排版好看"的活都得读）

- ⚠️ **GtkBox 不会居中非 FILL 的子控件**：`halign=CENTER` 在 Box 里**按左边缘
  摆** ⇒ 卡片一直贴着容器左边（宽屏下看着像"贴边的小纸条"）。
  **GtkStack 的 `allocate_child` 才会按 halign 居中那一页** ⇒ 想居中就把
  **整页**设成 CENTER，别指望 Box。
- ⚠️ **GtkBox 摆位时按子控件的「内容宽」算，却按「含 padding 的宽」分配**
  ⇒ 带 padding 的卡片会偏出去 **padding/2**（实测 16px，看着像"没居正"）。
  ⇒ **padding 放到内层**：外层卡片只留底色与边框（边框只差 1px，肉眼无感）。
- ⚠️ **GTK4 的 CSS `min-width` 量的是内容盒**，而控件分配到的宽度是**含
  padding 与边框**的 ⇒ 两者对不上 ⇒ 位置与宽度不一致（同样偏十几像素）。
  ⇒ **别在"要居中"的控件上写 min-width**，宽度交给**内容**决定
  （min == natural == 分配宽 ⇒ 位置与宽度一致）。
- ⚠️ **一列行要"等宽"就别让每行按自己的文字宽度排**：卡片是竖排 Box，子控件
  默认 FILL ⇒ 同一列的行拿到**同样的宽度**；要等宽就把**文字长度也写成一样**
  （「新建连接 / 新建查询 / 搜索对象」都是四字 ⇒ 宽窄一致；「新建数据库连接…」
  就比别的长一截 ⇒ 一列参差的锯齿 ⇒ 这就是"看着丑"的主因）。
- ⇒ 防回归用**几何断言**：卡片中心与容器中心的偏差 ≤ 2px；同一列的行**宽度
  完全相等**。光看截图判断不出偏没偏。

## 数据库方言坑（PG / MySQL 元数据）

- ⚠️ **tokio-postgres 的「简单查询协议」拿不到列类型**（阶段 64 实测）：
  `simple_query_raw` 回来的 `SimpleQueryMessage::RowDescription(Vec<SimpleColumn>)`
  里，**`SimpleColumn` 只有列名**——协议里的**类型 OID 被驱动丢掉了**。
  ⇒ 想拿列类型只能走**扩展协议**：`client.prepare(sql)`（Parse+Describe+Sync，
  **不执行、不传行**）→ `Statement::columns()` → `Column::type_().oid()`。
  ⚠️ 代价是**每条查询多一个来回**；且 `prepare` 失败（多语句 / 带占位符 /
  语法错）时**类型拿不到** ⇒ 只能显示「—」，**别猜**。
  ⚠️ **扩展协议拿不到"文本形态的值"**：`postgres_types` 的 `String` / `&str`
  只接受 TEXT/VARCHAR/BPCHAR/NAME/UNKNOWN ⇒ 整数列取不出字面量 ⇒ **值的
  通路还得留在简单查询协议**（它给的是原始字节）。
  ⇒ 结论：**类型走 `prepare`，值走 `simple_query_raw`**，两条路各管各的。
- ⚠️ **MySQL 的线协议就带类型**（`Column::column_type()`）⇒ 零成本，
  不用绕。⚠️ 但 MySQL 的 `BOOLEAN` 就是 `TINYINT(1)`，**线协议里分不出来**
  （协议不带显示长度）⇒ 按整数算。

- **定时事件只有 MySQL 有**：PostgreSQL 压根没有这个功能——它有的是
  `pg_event_trigger` / `CREATE EVENT TRIGGER`，那是 **DDL 钩子**（DDL 执行前后
  触发一个函数），不是定时任务。查过 PG 9.6/10/12/17/18 的官方系统目录清单，
  里面**只有 `pg_event_trigger`，从来没有 `pg_event`**；`CREATE EVENT` 与
  `pg_get_eventdef` 也从来不是 PG 的语法。硬查 `pg_event` 只会得到
  `relation "pg_event" does not exist`。**教训**：别靠"某版本文档 404"就推断
  "其它版本有"——曾按「PG 18 移除、PG 10~17 才有」写版本门槛，结果在
  PG 12/14/16 上照样炸（那些版本同样没有 `pg_event`）；要下结论就查目录清单。
- **MySQL `SHOW CREATE <对象>` 的列序各版本不同**：26.x 在中间插了
  `time_zone` 一列，"按固定下标取第 2 列当定义"会取错。`db.rs::pick_def_text`
  改成**按内容找「以 CREATE 开头的那一列」**（触发器/例程/事件一起受益）。
- **`information_schema` 的列也有版本差**：`EVENTS.SCHEDULING_TYPE` /
  `EVENT_TYPE` 只有 5.7/8.0 有，26.x 没有——只查各家都有的
  `INTERVAL_VALUE` / `INTERVAL_FIELD` / `STATUS`。
- ⚠️ **PG 的复合外键必须按位置配对列**：直接
  `key_column_usage JOIN constraint_column_usage`（按约束名连）会**叉乘**——
  两列的复合外键会变成 2×2 = 4 行，而且列名还可能配错（a 配 b、b 配 a）。
  正确写法是走 `referential_constraints`（拿到被引用的唯一约束）再用
  `key_column_usage.position_in_unique_constraint` **按位置**对上
  （见 `dbmeta::meta_schema_foreign_keys`）。ER 图上配错列 = 画出错的线。
- **PG 没有 `SHOW CREATE TABLE`**：DDL 自己从 information_schema 拼
  （`dbmeta::build_ddl`），默认值必须带上；外键查询按方言分派
  （`constraint_column_usage` 是 PG 专有表，MySQL 查它报 Unknown table）。

## 状态点与「聚合状态」的坑（阶段 65 记）

- **一个点代表的是"多个维度"**（文件面板 + 终端 + 数据库）⇒ 聚合要**取最严重的**
  （Err > Warn > Ok），而不是各管各的（否则标签、侧边栏、连接管理器三处的点
  会互相矛盾）。⇒ **一处算、处处用**（`active.rs::aggregate` 是唯一出口）。
- ⚠️ **每个维度都要有"没上报"这一态**（**三态**：Unknown / Alive / Dead）：
  **没上报 ≠ 已经结束**。用布尔就必然把"没起过"和"掉了"混成一样。
  没上报时**不参与判定**，tooltip 老实写「未知」——**不谎报绿点**。
- ⚠️ **Err 在两棵树里不画点** ⇒ 把"某一部分功能掉了"映射成 Err，用户看到的
  是"根本没连"，两种故障混成一样 ⇒ 这种半残状态映射成 **Warn（黄点）**。
- ⚠️ **可选功能不参与颜色判定**：数据库工作台没开**不是错误** ⇒ 只进
  tooltip，不进颜色。
- ⚠️ **tooltip 要说清"哪一部分"出的问题**：一句「已连接」在多维度下等于
  没说 ⇒ 「文件面板：已连接 / 终端：已结束 / 数据库：未连接」。
- ⚠️ **别凭"new 里调了 spawn"就断定回调时序**（阶段 65 踩过）：
  `pty.spawn_async` 是**异步**的 ⇒ 回调在主循环里跑，那时钩子早装好了 ⇒
  我以为"启动那次的存活事件会漏掉"、为此加的补报逻辑**其实用不上**。
  ⇒ 写"补报/兜底"之前先确认回调**到底什么时候**跑。
- ⚠️ **回退验证要挑对对象**：回退一个"当前路径上本来就用不到的分支"⇒
  断言照样通过 ⇒ **等于没验证**（阶段 65 第一次回退就踩了：去掉补报逻辑
  后冒烟照样绿，因为那条路径根本走不到）。⇒ 回退必须挑**真会挂**的那处
  （那次是摘掉 `install_alive_hook` ⇒ `Unknown ≠ Alive` ⇒ 挂）。

## GTK4 里「切页 / 显隐」的几条实测（阶段 66 记）

- ⚠️ **`GtkNotebook:current-page` 改了也不发通知**：实测
  `set_current_page()` 之后 `connect_notify_local(Some("current-page"), …)`
  **一次都不跑**。⇒ 要监听"切到哪一页"，用 **`switch-page` 信号**。
- ⚠️ **gtk4-rs 0.11 的 `connect_switch_page` 回调**签名是
  `Fn(&Notebook, &Widget, u32)`——**没有返回值**（GTK 那边返回 false 可以
  挡住切换，这套用不上；写 `return true` 直接编译不过）。
- ⚠️ **`switch-page` 是切换**之前**发的** ⇒ 那一刻 `current_page()` /
  `current_kind()` 还是**旧页**的 ⇒ 要按回调传进来的**页序号**查
  （自己维护 `page_num → 种类` 的映射）。
- ✅ **GTK4 的 `Paned`：把 start child 藏起来，空间会给 end child**——
  实测 1200 宽的连接页，子标签区从 856px 变成 1198px（占满）。
  ⇒ "这一页要独占宽度"用 `set_visible(false)` 就够，**不用**换 Paned 结构
  （换结构会踩"父容器类型隐式契约"那个坑）。
- ⚠️ **但这个"省空间"有副作用：左边界会跳**（阶段 97 的根因）——start child
  一藏，子标签栏的左边界就从 340 变成 0 ⇒ **整排页签头左右乱晃**，
  用户以为界面坏了（2026-11-05 报的截图就是这样）。
  ⇒ 现在的做法是**把文件面板搬进终端子页签**，切到数据库页时**整页替换**
  （文件面板跟着终端页让位），子标签栏全程不动。
  ⇒ 教训：**"隐藏控件"和"换布局"对用户来说不是一回事**——前者会让页面
  边界位移，后者才是真的整页接管。
- ⚠️ **「标志位对了」不等于「界面对了」**：阶段 66 的对象树断言一开始只查
  "折叠标志位" ⇒ 回退"忘了隐藏那一列"时**照样通过** ⇒ 等于没测。
  ⇒ 断言要落在**真的几何**上（控件的 `is_visible()` / Paned 的 `position()`）。
- ⚠️ **折叠掉一列之后，"展开"入口不能也藏在那列里**：树收起来 ⇒ 树头上的
  「‹」跟着没了 ⇒ 只剩顶部的开关能把树拉回来 ⇒ **顶部那个开关是必须的**。

## 数据库工作台的新交互（阶段 84 记，改这块前必读）

- **WHERE 是自由文本，而且是唯一的真相**（`ResultTab.filter_where` 存的就是
  用户写的那段文本）：条件助手只负责**按列类型转义生成一段文本**（「填入」
  **不自动查库**——用户多半还要接着改，按回车才生效）。⚠️ `check_free_where()`
  只拦**引号外**的分号（一条查询只能一条语句；结尾那个句号放过并去掉）。
- **界面上有两个筛选，必须视觉可分**：服务端那个 = 蓝色粗体「WHERE」标签 +
  宽输入框（`.pp-where-label`）；本地那个 = 「筛本页」短名 + 搜索图标（只在已
  取回的这一页里筛，**不查库**）。⚠️ 这俩以前都叫「过滤」⇒ 用户点错了才发现
  不是一回事。
- ⚠️ **`GtkEntry` 的自然宽跟着 placeholder 走** ⇒ WHERE 框的 placeholder 只能
  写「条件…」，例子放 tooltip（一句例子能把这一行的**最小宽度**撑到 300 多
  px ⇒ 窗口一窄右边整排按钮被切掉）。
- **底栏三段式**（对标 dbx）：左 = 行数·耗时·待保存项数 + 页签自己的状态；
  中 = **当前 SQL 常驻**（`foot_sql`，完整内容在 tooltip 里）；右 = 「统计总行
  数」+ 每页行数 + 分页控件。
  ⚠️ **「总行数」是 COUNT(\*) 的真值**，跟对象树「详细模式」里那个**估算值**
  （≈，PG 的 reltuples / MySQL 的 TABLE_ROWS）**不是一回事**，文案要分清。
  ⚠️ **只有表浏览页签能数**：手写查询不知道是哪张表 ⇒ 按钮灰着，不给用户点出
  一个假答案。
- ⚠️ **条件条（WHERE + 助手 + 排序 + 清除）现在和动作按钮挤在同一行** ⇒ 这一
  行的**最小宽度**很敏感（实测 cond_bar 545px + 7 颗动作按钮）⇒ 新加控件前先
  量一遍（`ch.measure()` / `ch.width()`）。
  ⚠️ **别看到「右边少了两颗按钮」就当布局坏了**：编辑模式那两颗（保存 / 放弃）
  是**按设计隐藏**的（`set_edit_toolbar`）⇒ **被切掉的按钮 `measure()` 的 min
  不会变 0，隐藏的才会变 0**（本轮实测确认过）。
- ⚠️ **属性页那块统计 Grid 必须 flex**（`vexpand(true)`）：Grid 是**回包之后**
  才加行的 ⇒ 一开始量高度只有表头那一行 ⇒ 写成 `vexpand(false)`（按内容高）
  ⇒ 11 行属性全被裁掉，页面上只看得见「属性 | 值」两个表头字。
  ⇒ 反过来，下面那段「建表语句」是**固定高度**（240px，长了自己在里面滚）。
- ⚠️ **视图没有 CREATE TABLE**：视图定义要问数据库自己（PG 的
  `pg_get_viewdef` / MySQL 的 `SHOW CREATE VIEW`），我们没这条通路 ⇒ 属性页只
  显示一句说明，**不给一段拼错的 DDL**。

## 窗口与对话框的规矩（阶段 67 记，审计 P2-17/P2-16）

- **三类容器，别再混着来**（新模块 `src/windows.rs`）：
  | 容器 | 形态 | 规矩 |
  |---|---|---|
  | **页面** | 连接页的子标签 | 随会话走（终端 / DB / 编辑器） |
  | **工具窗** | 独立 `gtk::Window` | **非模态**、**单例**、**记尺寸** |
  | **对话框** | `gtk::Dialog` | **模态**、**有父窗口**、短任务 |
- **工具窗一律走 `windows::open_or_focus(kind, build)`**：同类**只开一个**
  （已开着就聚焦），关掉时自动注销 + 记尺寸。
  ⇒ 以前连接管理器 / 私钥 / 凭据库每次都新建一个窗口 ⇒ 用户看见**两个**
  连接管理器，第二个盖着第一个（像程序卡住了）。
- **对话框一律走 `windows::dialog(title, parent, buttons)`**：必定挂上父
  窗口（没给就兜底到主窗口）⇒ 修掉"无父窗口的模态框"（多窗口时它浮在别的
  窗口上面，甚至被主窗口挡住）。
- ⚠️ **父窗口只存一份**：以前 connmgr / about / shortcutsui / replayui
  **各存一份** ⇒ 换个入口弹出来的对话框挂在**别的**窗口下面。现在统一在
  `windows::set_parent` 里。
- ⚠️ **"非模态 GtkDialog" 是被点名要改的混合形态** ⇒ 工具窗一律用
  `gtk::Window`（目录镜像原来就是"非模态 GtkDialog"）。
- ⚠️ **工具窗的按钮闭包只抓弱引用**：抓强引用就是
  窗口 → 按钮 → 闭包 → 窗口 的**环**，窗口永远回收不掉。
- ⚠️ **数窗口要只数"看得见"的**：`gtk_window_list_toplevels()` 里混着 GTK
  内部的**隐藏**窗口（弹层宿主之类）⇒ 把它们算进去，"开没开新窗口"就
  算错了（阶段 67 冒烟在这上面绕了一圈）。
- ⚠️ **测"同类工具只开一个"要走真实入口**（`connmgr::open_manager`），
  别直接调注册表——后者只测"我写的那段逻辑对不对"，测不到"用户点的那个
  入口到底开了几个窗口"。

## 「新建」入口与弹层的规矩（阶段 68 记，审计 P1-2/P1-6）

- **动作表是唯一来源**：`topmenus::NEW_ITEMS`（icon + label + action +
  scope），**动作的实现**只有一份（`newmenu::run_in` / `run_in_session`），
  各入口只是"从表的哪个角度看"（`topmenus::NewEntry`）。
  ⇒ 以前 5 个入口各写各的 ⇒ 从哪个入口进来看到的东西不一样。
- **会话内的入口只给会话内的动作**（`NewScope::Session`）：子标签 `+`
  不给"新建连接 / 导入 PuTTY 会话"（那不属于这个会话）。
- **没有会话时，会话内的动作要置灰**（`newmenu::NewCtx`）：点了没反应
  最让人以为程序卡了。
- ⚠️ **`MenuButton` 的弹层走 `set_popover()`，内容一次性建好** ⇒ 上下文变了
  （有没有会话）菜单**不跟着变**；而且 `MenuButton` 自带的小三角在 GTK 4.18
  里压不掉。⇒ 顶栏/侧栏的 `+` 用**普通 `gtk::Button`** + `menu::popover()`
  （自己挂父），**每次点击按当前上下文重建**菜单。
- ⚠️ **弹层是 `set_parent` 挂上去的，不进容器的 children 链** ⇒ 从控件树里
  **找不到它** ⇒ 冒烟要拿它得走**测试钩子**（`Sidebar::plus_popover()`）。
- ⚠️ **弹层必须在一个真实 toplevel 里**才能 `popup()`：没有 toplevel 时
  GTK-CRITICAL 一串之后**直接段错误**（`gdk_surface_new_popup` 断言）。
- ⚠️ **子标签的 `+` 建库连接必须落在"它自己的"会话里**
  （`newmenu::run_in_session`）：用 `current_panel_state()`（**当前**会话）
  ⇒ 用户切到别的标签页再点，就建到那边去了。
- ⚠️ **欢迎页上顶栏「＋」是隐藏的**（既有行为："同屏只留一个"）——排查
  "顶栏没有 +"时别当成 bug（阶段 68 查了半天才发现是**基线就这样**）。

## 设置项的规矩（阶段 69 记，审计 P1-4）

- **设置项的归属登记在** `settingsui::SETTINGS`（注册表）：一行一个设置项
  （`id` / `label` / `page` / `from`）⇒ **每个设置项有且只有一个归属页**。
  改设置落在哪一页 ⇒ 改注册表 + 改那一页，**两处一起改**。
- ⚠️ **注册表的 `label` 必须与页面上的文字一字不差**：冒烟按文字核对
  "每项只出现在它归属的那一页" ⇒ 页面上的标题改了，注册表也要改
  （阶段 69 就因为「快捷键」↔「快捷键一览」差一个字被冒烟逮到）。
- ⚠️ **别留第二处设置入口**：传输设置原来在队列窗口顶部，现在只在首选项
  中心；队列顶部只留**只读摘要 + 「去设置」链接**。
- ⚠️ **用户可见文字里别留 Markdown 的 `**`**：GTK 标签不渲染，会原样显示
  星号（截图里一眼就看见）。
- ⚠️ GTK4 的 `Notebook::tab_label` 认的是**页控件**不是页序号 ⇒ 得先
  `nth_page()` 拿控件再问标签。
- ⚠️ **页签里的汉字会被压成「…」**：宽度按**显示列**算（汉字两列），
  用 `panel::subtabs::display_width` 换算，别手抄字符个数。

## 阶段 86 记：收口时挖出来的几条"静默无效"

- ⚠️ **`set_tab_reorderable` 必须在 `append_page` **之后****：GTK 要求页**先在
  notebook 里**才谈得上"可重排"。反着写，GTK 报
  `gtk_notebook_set_tab_reorderable: assertion 'list != NULL' failed` 并
  **静默丢弃**这个标记 ⇒ **所有页签都拖不动**，而且界面不报错、用户根本
  不知道有这功能（第三轮复查 N1；这功能从加上去那天起就没生效过）。
  ⚠️ GTK4 **没有**"读回这一页可不可重排"的接口 ⇒ 只能断言**源码顺序**
  （`lint_tests::页签可重排要在挂页之后`）。**静默无效只能靠源码/几何断言钉。**
- ⚠️ **`gtk::is_initialized_main_thread()` 是线程局部的**（gtk4-rs 的 `rt.rs`
  里是 `thread_local!`）⇒ **单测跑在 libtest 的子线程上**，除非这个线程恰好
  是当初调 `gtk::init()` 的那个线程，否则永远是 false ⇒ "GTK 没初始化"是
  **常态**。⇒ 断言里别把"没初始化"当成"坏了"（阶段 86 的主题单测并行跑时
  2/3 概率假红，害得"全量回归全绿"差点卡住）。
- ⚠️ **扫描器的"跳过 CSS"启发式要用长度当闸门**：阶段 81 的文案 lint 靠
  "字符串里同时含 `{` 和 `}` ⇒ 当 CSS 放过" ⇒ 于是 `format!("{name} 的值")`
  这种**带占位符的短字符串**也被一起跳过 ⇒ 界面文案里混进 `**` /「远端」
  **扫不出来**（实测：往 `src/` 丢一个探针文件就能绕过 lint）。
  ⇒ CSS 块是**很长**的一段 ⇒ 判"长且带花括号"才当 CSS。
- ⚠️ **GTK4 的 `TextView` 没有 `set_placeholder_text`**（那是 `GtkEntry` 的）
  ⇒ 要占位提示得自己拿 `gtk::Overlay` 叠一个 Label，文本为空才显示。
  ⚠️ 那个 Label **必须 `set_can_target(false)`**：不然它盖在编辑框上面，
  用户点了却聚焦不进去 ⇒ "编辑器坏了"。
- ⚠️ **新页签的初始文案要真算一遍**，不能靠标签的默认值：表文档的「数据」
  视图建出来时就带 target（"这张表还没有数据"），新建的查询页签没有
  （"还没有结果"）——两个说法不一样，靠默认值就有一半是错的。
- ⚠️ **回退验证没变红 ⇒ 断言没在测那条路径**（阶段 83 踩过）：回退
  `grid_empty_text` 的一个分支，冒烟**照样绿** ⇒ 因为新页签的标题一直是
  标签的默认值，那次调**压根没被调用**。⇒ 回退之后**一定要看输出**，
  别只看它绿不绿。

## 阶段 99 记：断线自愈（失败 ⇒ 重连一次 ⇒ 重试原操作 ⇒ 还失败才报错）

**用户原话**（2026-11-06）：
- 「SFTP 断了显示断开了，但点重连终端时 SFTP 没重连，要手动点重连」；
- 「就是在**异常处理程序中重连一次**，如果还是失败就报错，否则就**继续执行
  原来的操作**」；
- 「**终端和数据库里面也可以这样处理**」。

⇒ 落成三条链路：SFTP / 终端 / 数据库，**同一套「失败 ⇒ 自愈 ⇒ 才报错」的
形状**。

### 分工：判定放 worker，执行放 UI

⚠️ **失败分两类**（这是整套逻辑的地基）：
- 「**连接断了**」⇒ 能自愈（重连一次 + 重试原来的操作）；
- 「**服务器说不行**」（权限不足、重名、SQL 写错）⇒ **只能报错**，重试也是
  同样的错。

⚠️ **判定**用 **worker 共享标志**（`PanelState::conn_dead:
Arc<AtomicBool>`）+ **UI 自己的 `conn_ok`**，**不靠匹配错误文案**——文案会
翻译、会改，匹配它是脆的。

⚠️ **worker 必须在发事件之前就把标志写好**（`worker.rs::send_ev`：先写标志、
再发事件）⇒ UI 收到事件时读到的一定是**那一刻**的真实状态，没有竞态。
反过来（先发事件后写标志）⇒ UI 读到的可能是上一次的旧值 ⇒ 误判 / 漏判。

⚠️ **数据库侧为此新增结构化事件** `PanelEvent::DbLost { id }`（worker.rs），
替换了原先 8 处「数据库连接已断开」的 `DbError` 文案事件——数据库要靠
**事件类型**判「连接断了」，不能靠文案。

### 只重试一次（`retried` 闸门）

⚠️ 用户说的是「重连**一次**」⇒ 必须有闸门：没有它 ⇒ 重连成功、重试又失败 ⇒
又重连 ⇒ **无限循环**（用户看着是"程序在疯狂重连"）。
⇒ `PanelState::retried`：**成功了就清掉**（下一条操作还能自愈），
**又失败了就报给用户并清掉**。

⚠️ **闸门什么时候清，三种情况都得想清楚**（阶段 99 实测踩出来的）：
- **操作成功了** ⇒ 清（**不清的话**：自愈成功一次之后闸门一直挂着 ⇒ 之后
  **每一次**断线都被当成「重试后又失败」⇒ 永远不再重连）；
- **重连本身失败了** ⇒ 也清（那次**没进**「重连成功 ⇒ 重试又失败」的循环 ⇒
  不该占着用户的额度，否则用户下一次操作只会看到一个错、连重连都不发起）；
- **重试又失败了** ⇒ 清（这时才报给用户）。

⚠️ **重发什么要按种类分别记**（`PanelState::retry_ops: HashMap<RetryKind,
RetryOp>`）：失败事件只带"路径"这类信息，**够不着一条完整命令**——「重命名」
要知道改成什么名、「写文件」要知道内容 ⇒ **发命令时把整条命令记下来**，失败
时按种类取出来重发（`RetryOp`：List / Simple / ReadFile / WriteFile）。

⚠️ **但「记住的那条」不一定就是「失败的那条」**（阶段 99 复查时发现的
竞态）：`retry_ops` 记的是**最后一次发出去**的操作，中间插进来一条后台
刷新（终端目录跟随 / 远端目录 mtime 变化触发的**静默列目录**）就会改写它
⇒ 重试的变成别的目录 / 别的文件。
⇒ **列目录**：失败事件**自带路径**（`Listed { path }`）⇒ 直接拿它重发；
**其余种类**（失败事件里没有内容）⇒ **核对路径对不对得上**（`retry_op_matches`
纯函数 + 单测），对不上就**不重试**（重试错的那条 = 替用户干了别的事）。

⚠️ **状态顺序**：`schedule_retry()` 里是**先重连、后写状态**——`on_reconnect()`
自己也会写状态（「正在连接 ……」），写在它后面就把「连接已断开，正在重新
连接…」盖掉了。

⚠️ **数据库只对「查询」做「重连 + 重试原查询」**：翻页 / 元数据 / 写语句
**只重连不重试**（游标已经挪动，重发会翻错页）。

⚠️ **数据库自愈重连必须保住结果页签**（`dbui::clear_tabs(ui, keep_pages)`）：
重连走的是 `do_connect`，它会 `clear_tabs`（**结果页签 + 待办表全清掉**）⇒
断线时失败的那条查询**重发回去也没有地方放结果**：回包按待办表找落点，
找不到就把结果**丢掉**（用户看着就是"重连了，但那条查询没结果"）。
⇒ 自愈重连走 `do_connect_inner(st, ui, keep_pages = true)`（保住页签），
重发时**补登记待办**（`pending[id] = Query { tab_id, sql }`）。
⚠️ 顺带：页签被清掉时 `ui.docs` / `ui.struct_tabs` 要一起清（阶段 72e 的老坑：
**页签被摘了但 `ui.docs` 还留着** ⇒ `find_doc` 找到已经死掉的文档）——
但**保住页签**时文档还活着 ⇒ 不用清。**待办表在任何情况下都要清**（旧连接
上那些「在飞」的请求永远不会有回包）。

### 终端：把用户敲的输入攒起来，等新会话就绪再补发

⚠️ 终端的「原操作」= **用户在会话死后敲进去的输入**。用现成的 `encode_key()`
采集字节（**不要用 VTE 的 `commit` 信号**——有 pty child 时它压根不触发），
存在 `TermInner::pending_input` 里。

⚠️ **补发时机 = 看见提示符之后**（`ReadyCmd::Cmd`），**不是 Probe 阶段**：
Probe 阶段远端还没出提示符 ⇒ 补发进去会被**登录脚本吃掉**（当成登录名）。
⇒ spawn 成功后把 `ready_hook` 合并进现有的那个钩子。

⚠️ **「会话已结束」条 + 终端自带的「重新连接终端」按钮**都走同一个
`reconnect_hook` ⇒ 顺手把 SFTP 也救回来（`panel::reconnect_sftp_if_dead`，
**幂等**：`connecting` 挡着重复发起）。
⇒ 三处终端创建点都要装钩子：`Tabs::open` / `new_term_page` / `rebuild_term`。

### 冒烟

`examples/selfheal_smoke.rs`（10 步，已进 CI 清单）：SFTP 失败⇒自动重连 /
**上次重连失败⇒下一次操作照样发起重连** / 连上⇒重发原操作 / 重试又失败⇒
不再重连 / 数据库 `DbLost`⇒重连 / 连上⇒重发查询（**待办已补登记**）/
**自愈成功⇒闸门清掉⇒下次断线照样自愈** / 重连终端⇒SFTP 也重连。

## 架构边界

Rust 分层（模块间通过消息 enum 往来，禁止手写打包传参）：
`main.rs`（入口/CLI 分支，顺序：askpass → parse → print-config → selftest → GUI）
→ `term.rs`（VTE 终端）/ `panel/`（双栏 UI，`Rc<PanelState>` 仅主线程）→
`panel/worker.rs`（tokio worker 线程独占连接）→ `backend/`（russh 封装）→
`args/config/connmgr/util`。GUI 状态不得跨线程；worker 用
`Arc<Mutex<XferShared>>` 回传传输进度。

**数据库连接不在工作台里（阶段 13）**：工作台顶部**没有连接表单**，只有一行
「当前连接 [下拉] [+] [连接] [断开]」。连接信息在**连接管理器**与**会话树**里
维护（新模块 `panel/dbconn.rs`）：新建时**先选数据库类型**→**再选「在哪个会话
上连」**→ 再填地址/端口/用户/库/密码。数据库连接在两棵树里都**挂在对应的
SSH 连接下面**。**连接是"先建后用"**（保存即落盘，工作台只负责用），不再
"连上了才存"。

⚠️ **会话键必须两边一致**（数据库连接是按会话键分组的）：ssh config 别名
（`aly`）连上之前会被展开成真实主机名——面板里的 opts 是展开后的，条目本身
还是别名。两边算出的会话键不一样的话，连接会"挂在这棵树里、却在那棵树上
找不到"。所以算键统一走 `connmgr::entry_session_key()`（用展开后的主机）；
自定义条目的键是 `s:<名字>`，从**最近项**打开时要把条目的 `session_name`
带上，否则键又对不上了。

**连接页的两层结构（阶段 12，阶段 97 重组、阶段 98 加编辑器页）**：连接页 =
**只有子标签 notebook**（`panel/subtabs.rs`），**文件面板住在「终端子页与
文本编辑页内部」**（那两页 = 横向 Paned：左＝文件面板、右＝终端 / 编辑器）。
**终端是第一个子页**，
数据库工作台（`panel/dbui.rs`）与文件编辑器（`panel/fileedit.rs`）也都是
子页，**不再是独立弹窗**。子标签的关闭回调必须抓**弱引用**（`Weak`），否则
PanelState → 子标签 → 回调 → PanelState 成环，面板永远释放不掉。没有子标签
容器时（老路径 / 单测 / 示例）要**兜底退回独立窗口**，否则那一页「存在但谁也
看不见」；兜底窗口必须自己新建，**不能借主窗口**（`set_child` 会把主窗口内容
整个换掉）。

⚠️ **阶段 97 为什么把文件面板搬进子页签**（用户 2026-11-05 报的现象）：
文件面板原先在**连接页那一层**（子标签区左边）⇒ 切到数据库页时它还占着
340px，而 GTK4 的 `Paned` **把空间给 end child**（`set_visible(false)` 就够）
⇒ 子标签栏左边界从 340 跳到 0 ⇒ **整排页签头左右乱晃**。现在：
- 连接页**没有**外层 Paned，文件面板归**当前那一页**管；
- 切到**终端页 / 文本编辑页**（阶段 98 加的）⇒ 把文件面板**搬进**那个页签
  的 Paned 槽位；
- 切到**数据库页** ⇒ 文件面板**让位**（整页替换），子标签栏不动。
⇒ 机制在 `src/panel/mod.rs` 的 `host_file_panel()` / `track_file_width()`
（阶段 97 起在 `src/tabs.rs`，阶段 98 挪到 `panel/mod.rs`——编辑器页也要用，
`panel/` 是它的正确位置）：**搬家走 Paned 的槽位 API**
（`set_start_child(None)` → GTK 自己 unparent → 再 `set_start_child(Some(..))`
挂新宿主 → 按 `st.file_w` 复位宽度），**不要裸调 `unparent()`**（会误伤别的
终端页签的宿主）。

⚠️ **宿主 Paned 必须有人强引用**（`PanelState::file_host`）：GTK4 里容器被
finalize 时会把它子控件一起 `gtk_widget_destroy` ⇒ **宿主页签一关，文件面板
就跟着没了**（`PanelState` 手里那个 Rust 引用救不回来——widget 已经被标成
「不可再用」）。留一份强引用 ⇒ 宿主不会被 finalize ⇒ 文件面板安全等到
下一次搬家。⚠️ 同理：**「Rust 侧还拿着它」不等于「它还活着」**——
被 destroy 过的 widget 再挂回容器会出问题。

⚠️ **GTK4 里搬 widget 的通用规矩**：同一个 widget 不能同时有两个父级
（`gtk_widget_set_parent: assertion 'gtk_widget_get_parent (widget) == NULL'`
是「已经挂上了」的报错，不是「没挂上」）⇒ 搬之前**先把它从旧槽位摘掉**
（Paned 用 `set_start_child(None)`，Box 用 `remove()`），再挂新父级。

⚠️ **子标签化之后，`db_ui.win` / `file_edit.win` 是「宿主窗口」（主窗口），
不是工作台/编辑器自己的窗口**：`destroy()` 它等于把整个程序关掉（而且
`panel::shutdown()` 是在**关连接标签**时调用的，不只是退出程序）。要关的只有
它们自己那些浮窗（搜索结果 / 执行历史）。

**对象属性详情（阶段 13.5）**：对象树右键「对象属性…」弹一个「属性 | 值」两列
的窗口（对标 Navicat 的 Details 面板）。元数据查询在 `backend/dbmeta.rs`：
PG 走系统目录、MySQL 走 information_schema，两边**列序刻意保持一致**（同样
10 项），取不到的项显示「—」而不是编一个。行数是**估算值**（PG `reltuples` /
MySQL `TABLE_ROWS`），标题写明「行数（估算）」；PG 没有创建时间、也没有「自增」
概念，那些项一律「—」。属性用**独立窗口**（跟搜索结果、执行历史一致），不占
工作台地方。

**表设计器改结构（阶段 14）**：表结构页签的**字段表是可编辑的**（列名/类型/
可空/主键/默认值），工具栏在**字段页里**（不跟底部的「执行 DDL」挨着，免得
用户点错——一个跑 DDL 页的 SQL，一个跑字段表的改动）。改动打包成**一条**
`ALTER TABLE` 发出（PG 与 MySQL 都支持一条语句多动作；PG 的 DDL 在事务里、
MySQL 8.0 是原子 DDL，所以天然全成功或全不动，**不必**再包事务）。SQL 生成在
`backend/dbmeta.rs::build_alter_table`（纯函数，12 项单测钉死方言差异）。

⚠️ **DbGrid 的两个坑（阶段 14 踩的）**：
- **`set_data` 会重建列，新渲染器默认不可编辑** —— `rebuild_columns` 里必须
  重新 `renderer.set_editable(self.editable.get())`，否则换一次结果集编辑功能
  就悄悄没了（结果网格没这个坑是因为它每次都重新进编辑模式）；
- **结构表必须 `set_sortable(false)`** —— 行在表里的先后就是列的先后，生成
  ALTER 按原顺序；用户一排就乱了。

⚠️ **字段表的「默认值」列没有值时要显示空**，不能显示 `NULL`：`NULL` 是 SQL
的 NULL 字面量，保存时会被当成"默认值 = NULL"，凭空给用户加一条 `SET DEFAULT`。

**表设计器改索引（阶段 15）**：索引页的网格也可编辑（索引名 / 列 / 唯一），
改动**打包成若干条语句**（PG 的 `DROP INDEX` / `CREATE INDEX` 是独立语句，
`ALTER TABLE` 管不了索引；MySQL 一条 `ALTER TABLE` 就能删一个建一个）。因此
`StructTab` 里有个**语句队列**（`sql_queue`）：一条回包到了再发下一条。

⚠️ **PG 的多条语句必须包在事务里，且失败要显式 `ROLLBACK`**：PG 支持事务性
DDL，不包事务中途失败会留下"旧索引删了、新索引没建"的半截结构；而 PG 的事务
一旦有语句失败就进入 **aborted** 状态，不显式回滚的话，这个连接上的事务会
一直挂着（后面的语句全都失败）。

⚠️ **MySQL 的 TEXT / BLOB 列建索引必须给前缀长度**（`note(10)`），不给就报
`ERROR 1170 BLOB/TEXT column used in key specification without a key length`
——这是真库上撞出来的（手写 `CREATE INDEX idx ON t (text_col)` 直接失败）。
PG **不支持**前缀索引，填了长度要在生成语句前拦下。

**表设计器改外键（阶段 16）**：外键页的网格也可编辑（约束名 / 本表列 / 引用表 /
引用列）。**只改名**和**改了引用**是两条不同的路径——只改名时 PG 走
`RENAME CONSTRAINT`（一步）、MySQL 删了重建；改了引用一律删了重建。
⚠️ **最容易改坏的地方是"用户只想改个约束名，结果引用列被清空了"**，所以这两
条路径必须分开走。

**结果网格的列显示设置（阶段 17）**：结果页签工具栏多一颗 `view-columns-symbolic`
图标（Navicat 的字段显示设置），表浏览页签与**普通 SQL 查询页签**都开放。
⚠️ **存的是「原始列下标」，不是显示位置** —— 隐藏几列后显示位置跟原始下标就
错位了，编辑单元格时会把改动记到别的列上（改 A 列写进 B 列）。所以 `DbGrid`
存的是 `(原始列下标, 是否显示)` 的**完整顺序**，编辑回调捕获的也是原始下标。
⚠️ **必须存完整顺序，不能只存可见的那些**：只存可见列的话，藏起来的列没有
位置，用户点一次「全显示」列顺序就变了（被藏过的列跑到末尾）。**隐藏只影响
显示**——待保存的 UPDATE 与导出的 INSERT SQL 仍按**全部列**算。

⚠️ **驱动错误别直接甩给用户**（2026-10 实测）：tokio-postgres 在"服务端要密码、
我们没给"时抛的是 `Kind::Config`，`Display` 出来只有一句 **"invalid
configuration"**（真正的原因 "password missing" 藏在 `source()` 里）。用户看到
这句完全不知道该干嘛，只能来问人。**认 cause 而不是认 Display 文本**：PG 看
`source()` 是不是 "password missing"；MySQL 看 1045 且消息含 `using password: NO`
（`YES` 是密码真错了，不该再弹密码框）。识别出来就**弹密码框**让用户当场输、
输完自动重连（对标 Navicat），别甩英文。

⚠️ **当场输入的密码要按连接名分开存**：只有一个"口袋"的话，切到别的连接会把它
带过去——严重的会把 A 的密码发到 B 的服务器上。

⚠️ **别用 `git apply --cached --unidiff-zero` 拆提交**：`-U0` 的 hunk 没有上下文，
git 只能靠行号定位，**后面的 hunk 会插到错的位置**（实测把一段函数插进了
`on_meta` 的表达式中间，还把一行插进了别的函数）。要拆提交就**按行号切片**
删/摘（`python` 里逐行处理，别用大正则跨函数匹配——`.*?` 会在函数里提前收尾，
把中间的代码一起吃掉）。

**CSV 导入向导（阶段 18）**：对象树右键表 →「导入数据（CSV）…」。
⚠️ **MySQL 不能一次发多条语句**：`query_iter` 发的是一条 COM_QUERY，服务端
默认不允许多语句（除非客户端开了 `CLIENT_MULTI_STATEMENTS`）；PG 的
`simple_query` 可以。所以导入是**PG 一批 20 条、MySQL 一条**。
⚠️ **导入要整体回滚**：整批一个事务，任何一行失败就全部回滚（"要么都进、
要么都不进"）；PG 合并发送失败时先粗定位到那一批，再**逐行重试**找具体行
（反正马上要回滚，不怕重复插）。
⚠️ **导入前先本地校验**（`dbmeta::edit_literal`）：整数列里写着 `abc`，要在
**动库之前**报「第 N 行「age」：应为整数」——不然插到那一行才炸。
⚠️ **导入与导出必须用同一套转义**（`dbmeta::build_insert_rows`）：不然"导出再
导入"会把 `\` 和 `'` 转两次，对不上。
⚠️ **CSV 解析必须是纯函数 + 单测**：引号里的逗号、字段里的换行、`""` 转义、
BOM、CRLF 这些，在界面上只表现为"第 3 行多了个逗号"，根本没法查。
⚠️ **约束错误要翻成中文**（`backend::db::explain_constraint_error`）：PG / MySQL
的原文都是英文（`duplicate key value violates unique constraint …`），用户
只会来问"这是什么意思"。**原文要留着**（查资料、找是哪个约束）。

⚠️ **`exec_sql_now` 是执行 DDL 的，`run_sql_now` 才是查结果的**：`exec_sql_now`
走 `DbExec`（只回影响行数，**不开结果页签**），拿 `SELECT count(*)` 的结果要用
`run_sql_now`（走查询路径、会开页签）。搞混了会读到**上一个页签**的数据，
误以为行数不对。

## 仓库布局：主线 = 仓库根 = GitHub 仓库（2026-11-01 起）

⚠️ **本仓库就是公开发布的仓库**（`git@github.com:fwt11/puttyplus.git`，`main`
分支 + `v0.1.0` tag）。crate 在**仓库根**，**没有 `rust/` 子目录** ⇒ `cargo build`
直接在根目录跑，改完 `git push` 即发布（不用再"复制两棵树 + 手工脱敏"）。

**旧开发历史**（100+ 提交，阶段 1–96，还是 `rust/` 布局）在
`../putty-dev-history-2026-11-01.bundle`（`git bundle`，`git clone <bundle>` 可还原）。

### 台账：**已入库**（2026-11-11 起）

⚠️ **2026-11-11 用户拍板：「把本地台帐也加入git，方便在其他平台继续开发」**
⇒ 以前只在本机的过程台账，**现在都跟着仓库走**：

| 文件 | 内容 |
|---|---|
| `AGENTS.md` | 本文件：项目说明书 + 踩坑记录 |
| `RESUME.md` | 工作笔记（接着干先看这里） |
| `docs/功能说明-内部.md`、`docs/README-开发版.md` | 功能说明的内部版（含阶段号 / 审计编号） |
| `docs/UI_AUDIT.md`、`docs/UI_IMPROVE_*.md`、`docs/UI_REVIEW_*.md`、`docs/ui-review-*/` | UI 审查文档与证据截图 |
| `tools/ROADMAP.md`、`tools/FEATURE_PARITY.md`、`tools/DBX_PLAN.md` | 阶段总账 / Rust-only 清单 / 数据库工作台计划 |

⇒ **换台机器（Windows / 另一台 Linux）clone 下来就能接着干**，不必再手工搬
这些文件。

### 仍是纯本地、不入库的东西

| 文件 | 为什么 |
|---|---|
| `c/` | C 版源码（已冻结，只作命令行契约的历史依据） |
| `tools/compat_check.sh`、`tools/shot_*.sh` | 活体对照与截图脚本（C 版在时才有意义） |
| `release/`、`release-github/`、`.shots/`、`.ui-audit/`、`.zcode/`、`.git-dev-old/` | 各机器的产物目录 |

⚠️ 这些的排除规则**分两处**：产物目录写进 **`.gitignore`**（跟着仓库走 ⇒
各机器行为一致），`c/` 与本地脚本留在 **`.git/info/exclude`**（本地）。
⇒ `git add -A` 也不会误提交它们。

### 公开纪律

- **真实主机名 / IP / 密码 / 内网域名一律不进仓库**：示例与单测里用占位值
  （`10.0.0.9` / `host.example.com` / `srv-01` / `db-password` / `/home/user`）。
  ⚠️ 换 IP 时**别把两处不同主机撞成同一个** —— 有两个单测靠"主机不一样"来
  断言：`connmgr::find_fwd_target_ambiguous_aliases`（NoEntry 用例）与
  `known_hosts::wildcard_and_multi_hosts`（通配符用例）。
- **公司名不出现**在 README 与 `docs/` 的公开文档里。
- 阶段号（`（阶段 NN）`）与审计编号目前**留在源码注释里**，公开仓库也带着 ——
  已向用户说明，用户选择保留（那是开发过程的自述，不是机密）。
- `docs/功能说明.md` 是公开版（由内部版 `docs/功能说明-内部.md` 改写而来：
  删内部阶段号 / 审计编号 / 与早期实现的对照）；`docs/命令行契约.md` 同样。

### deb 的 libc 依赖被弱符号撑高（2026-11-01 用户报「装不上」）

**症状**：用户从 Releases 下 deb 装不上 —— `依赖关系问题 … 依赖于 libc6 (>= 2.39)；
然而系统中 libc6 的版本为 2.38-6deepin30`（Deepin 25 就是 glibc 2.38）。

**根因**（不是 bug，是链接期行为）：Rust 的 std 以**弱引用**方式用
`pidfd_spawnp` / `pidfd_getpid`（glibc 2.39 才有的函数）。**构建机 glibc ≥ 2.39**
（Debian 13 / Ubuntu 25.04）时，链接器给这些弱引用打上 `GLIBC_2.39` 版本标签 ⇒
`dpkg-shlibdeps` 把依赖算成 `libc6 (>= 2.39)`。可程序**本来能跑**（弱符号缺失时
std 会走回退路径），只是加载器每次启动打一行
`weak version 'GLIBC_2.39' not found`。

⇒ **在 glibc 2.38 的机器上构建就不会有这个问题**（那里根本没这两个符号，链接器
不打标签，依赖停在 `libc6 (>= 2.34)`）。所以"本地打的包能装、CI 打的包装不上"
是这个原因，不是 CI 环境脏。

**修法**：`tools/strip_weak_glibc_versions.py` 把弱符号的版本索引置 0，并从
`.gnu.version_r` 的 aux 链上摘掉对应的版本需求条目 ⇒ 二进制与"在老 glibc 上编译"
等价（加载器也不再报警告）。`debian/rules` 的 `override_dh_auto_build` 与两套
工作流在编译后各跑一遍；CI/Release 里还加了一步核对 deb 的 `Depends` 不许再出现
`libc6 (>= 2.35)`。

⚠️ 写这个脚本时踩过的坑：
- **ELF 绑定常量是 `STB_LOCAL=0 / STB_GLOBAL=1 / STB_WEAK=2`**（不是 1/2 反过来）——
  搞反了会把弱符号当强符号，脚本就啥也不干；
- **`.gnu.version` 里的版本索引 = 对应 `Elf64_Vernaux.vna_other`**，不是它在 aux
  链里的位置；
- 只清符号的版本索引**不够** —— 加载器还会逐条检查 `.gnu.version_r` 里的版本需求，
  那条 `GLIBC_2.39` 不摘掉，启动时照样打警告。

### GitHub Actions（`.github/workflows/ci.yml`）

在 **`debian:trixie` 容器**里跑：GTK 4.18.6 / VTE 0.80.1 正好是 `debian/control`
声明的依赖下限（Ubuntu runner 只有 GTK 4.14，编译期会被 gtk4-sys 的版本断言
挡下）。步骤 = 装依赖 → 装 stable 工具链 → 缓存 → 打印工具链版本 → 编译 release →
单测（**故意不给 DISPLAY**）→ GUI 冒烟（`xvfb-run`，example 跑在主线程）→
命令行契约回归 → 上传可执行文件 → 打 deb → 上传 deb。

⚠️ 三个踩过的坑：

- **容器 job 里 `HOME` 不是 `/root`**（runner 设成 `/github/home`）⇒ rustup 要装
  到**固定路径**（`/opt/rust`），并把 `CARGO_HOME` / `RUSTUP_HOME` 写进
  `$GITHUB_ENV`；写死 `$HOME/.cargo` 会让 `rustc`/`cargo` 根本不在 PATH，编译
  步直接 command not found。
- **`libudev-dev` 不能少**：`serialport` → `libudev-sys` 要 pkg-config 找
  `libudev.pc`，而它既不是 GTK 的依赖也不是 VTE 的依赖 ⇒ 干净环境默认没有，
  编译期就炸。已补进 `debian/control` 的 Build-Depends（这原本是被本机环境
  掩盖的缺陷：在别人机器上打 deb 也会挂）。
- **Actions 的日志下载要 admin 权限** ⇒ 调试 CI 失败时**别指望 API 拉日志**。
  替代办法：把失败详情做成 **`::error::` 注解**（check-runs API 能读）。
  ⚠️ 注解是**单行**文本：日志里的 ANSI/控制字符会**把整条注解吃掉**，
  整条太长（>1KB）也会被丢弃 ⇒ **gzip + base64 再分段发**（每段一条注解）。
- **`xauth` 不能少**：Debian 的 `xvfb` 包**不依赖** `xauth`，而 `xvfb-run`
  找不到 `xauth` 就直接 exit 3（"xauth command not found"）⇒ GUI 冒烟全挂。
- **基础镜像里一个字体都没有**：GTK 没字体会起不来 ⇒ 装 `fontconfig` +
  `fonts-dejavu-core`（界面是中文的，再加 `fonts-noto-cjk`）。
- **单测那步不要给 DISPLAY**：libtest 把每个测试跑在子线程上，GTK 要求在主
  线程初始化。容器里有 xvfb ⇒ `gtk::init()` 真成功 ⇒ 那几个 GTK 用例就在
  子线程里操作 GTK ⇒ 命中 GTK 的致命断言 ⇒ **整个测试进程 abort**（失败段是
  空的，只有 "test failed"）。不给 DISPLAY 时 `gtk::init()` 返回 Err ⇒ 用例按
  设计跳过 ⇒ 与本机一致。**GUI 的断言要跑 example**（独立进程 ⇒ 主线程）。
- **契约检查依赖 HOME 的路径形状**：脚本里 `-i ~/.ssh/id_rsa` 的 `~` 是
  **eval 所在的那个 shell** 展开的（既不是 HOME 环境变量，也不是程序自己
  展开）⇒ 输出的 keyfile 长成 `$HOME/.ssh/...`；黄金基准是在 `/home/<用户>`
  下生成的 ⇒ HOME 不在这个形状下，73 条里那两条带 `~` 的全 DIFF
  （实测 `HOME=/tmp/xxx` ⇒ PASS=71 FAIL=2）。⇒ CI 里跑契约前
  `export HOME="/home/$(id -un)"`。

### 发版（打 tag → GitHub Actions 出 Release）

⚠️ **2026-11-03 踩过的坑：v0.2.0 那次发布是坏的**——tag 打的是 `v0.2.0`，
可 `Cargo.toml` 与 `debian/changelog` 还停在 `0.1.0` ⇒ 产出的 deb 叫
`puttyplus_0.1.0-1_amd64.deb`（**和 v0.1.0 的同名**），Release 正文也写着
「PuTTYPlus 0.1.0」（正文硬编码在 workflow 里）⇒ 用户装上后「关于」里
显示的还是 0.1.0。⇒ 根因：**版本号有三个来源且互不校验**。

现在的护栏（三道）：

1. **`tools/check_version.py`** —— 版本号校验的**唯一入口**：
   `Cargo.toml` ⟷ `debian/changelog` ⟷ git tag 必须一致。
   ```bash
   python3 tools/check_version.py            # 查 Cargo.toml ⟷ changelog
   python3 tools/check_version.py v0.3.0     # 再跟 tag 比
   python3 tools/check_version.py --print    # 只打印当前版本
   ```
2. **CI（`ci.yml`）**：checkout 之后先查一次（坏得快），打 deb 之后再核对
   **产出的 deb 文件名带的是不是当前版本号**。
3. **Release（`release.yml`）**：发版前再查一次（带 tag）；发布说明
   **不再硬编码** ⇒ 按版本放在 `docs/发布说明/<tag>.md`，`{{VERSION}}`
   会被替换成实际版本号；**文件不存在 ⇒ 直接失败**（宁可不发，也不要发
   一个说错版本的说明）。

⇒ **发版清单**（顺序别乱）：
```bash
# 1) 升版本号：两处一起改
#    Cargo.toml      → [package] version = "0.3.0"
#    debian/changelog → 顶条加 puttyplus (0.3.0-1) unstable; urgency=medium
# 2) 写发布说明：docs/发布说明/v0.3.0.md（正文里用 {{VERSION}} 占位）
# 3) 本地校验
python3 tools/check_version.py v0.3.0
bash tools/cli_contract_check.sh
# 4) 提交 + push
git add Cargo.toml Cargo.lock debian/changelog tools/check_version.py \
        .github/workflows/ docs/发布说明/ && git commit -F /tmp/msg.txt && git push origin main
# 5) 打 tag（push tag 触发 release.yml）
git tag -a v0.3.0 -m "PuTTYPlus 0.3.0：…" && git push origin v0.3.0
```

⚠️ **deb 的包名与包版本来自 `debian/changelog`，不是 `Cargo.toml`** ⇒
只改 `Cargo.toml` 是**不生效**的（v0.2.0 就是这么翻车的）。
⚠️ 程序的「关于」框显示 `CARGO_PKG_VERSION`（= `Cargo.toml` 的 version）
⇒ 两处都得改。
⚠️ 本机**没有 `gh`、没有 GitHub token** ⇒ 只能 `git push` tag 触发 Actions；
**删已发布的 Release / 改 Release 正文**得用户在网页上自己弄。
