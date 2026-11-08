# Windows 开发环境搭建（把 Windows 变成开发 + 验证工作台）

> 面向「在 Windows 上直接开发 PuTTYPlus」这件事。
> 目标：在这台机器上能 `cargo build` / `cargo test --lib` /
> `cargo run --example`（**有桌面会话 ⇒ GUI 冒烟能真跑**），
> 而不必每轮都等 CI 编一个 exe 下来手测。
>
> ⚠️ **装东西之前请自己确认**：下面会往机器上装 VS 生成工具（约 3~5 GB）、
> Rust、Python。这三样是**编译 Windows 版必需**的。
> GTK4 与 Zig 由脚本自动下载（约 350 MB，放在 `C:\gtk` 与 Python 包目录）。
>
> 配套脚本（都在 `tools/`）：
> - `tools/win_setup.ps1` —— **一条命令装完全套**（带进度输出；`-WhatIf`
>   只打印不执行）。
> - `tools/win_build.ps1` —— 下载 GTK4 / Zig / Ghostty 源码 + 编译 + 跑 73 条契约。
> - `tools/win_env.ps1` —— 把环境变量**持久化**（用户级），让每个新终端都带着。
> - `tools/win_msvc_env.ps1` —— 在当前会话里激活 MSVC（`link.exe` 进 PATH）。

---

## 1. 要装的东西

| 东西 | 为什么 | 大小 | 谁来装 |
|---|---|---|---|
| **Visual Studio 生成工具**（C++ 生成工具 + Windows SDK） | Rust 的 `x86_64-pc-windows-msvc` target 要链接器 `link.exe`，它只在这个包里 | ~3~5 GB | **你**（要管理员权限） |
| **Rust（msvc host）** | 编译本体 | ~500 MB | 脚本（rustup-init） |
| **Python** | 装 Zig 用（PyPI 的 ziglang wheel） | ~100 MB | 脚本（winget） |
| **GTK 4（gvsbuild 2026.10.0）** | Windows 没有系统 GTK 包；这个包是 MSVC 编译的，跟 Rust 的 msvc target 对得上 | ~300 MB | 脚本（`win_build.ps1`） |
| **Zig 0.17.1** | 编译 `libghostty-vt`（Ghostty 的 C 代码是 Zig 写的） | ~50 MB | 脚本（pip ziglang） |
| **Ghostty 源码** | `ghostty-vt-sys` 的 build.rs 在 Windows 上自己下载不可靠（走 NativeTLS）⇒ 我们自己备好并校验 sha256 | ~20 MB | 脚本（`win_build.ps1`） |

⚠️ **不需要 MSYS2**：跑 exe 只要 GTK 的 DLL；编译要 Rust MSVC + VS 生成工具 +
GTK4 + Zig。**只有跑那 73 条命令行契约才需要 bash** ⇒ 用 Git for Windows 自带的
Git Bash 就够（CI 就是用它）。

---

## 2. 一步一命令

### 第 1 步：VS 生成工具（**管理员** PowerShell，一次约 10~30 分钟）

```powershell
# 下载安装器（官方引导程序，约 4 MB）
$bt = "$env:TEMP\vs_BuildTools.exe"
Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" -OutFile $bt

# 静默安装：C++ 生成工具 + Windows SDK（--includeRecommended 会带上 SDK）
& $bt --quiet --wait --norestart --nocache `
      --installPath "C:\BuildTools" `
      --add Microsoft.VisualStudio.Workload.VCTools `
      --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
      --includeRecommended
```

> `--quiet`  ⇒ 不弹 UI；`--wait` ⇒ 等它装完；`--installPath` ⇒ 装在
> `C:\BuildTools`（默认在 `C:\Program Files\...`，**路径太长容易撞
> MAX_PATH**，所以指定一个短的）。
> 装完**不用重启**（除非提示要）。

验一下：

```powershell
& "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe" `
   -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
   -property installationPath
```

⇒ 打出 `C:\BuildTools` 就算好了。

### 第 2 步：Rust（msvc host）

```powershell
$ri = "$env:TEMP\rustup-init.exe"
Invoke-WebRequest -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" -OutFile $ri
& $ri -y --default-host x86_64-pc-windows-msvc --profile minimal
```

> rustup 会把 `%USERPROFILE%\.cargo\bin` 加进**用户级 PATH** ⇒ **新开的
> 终端**里 `cargo` 就能用了（当前这个窗口要等一下或手动加）。

### 第 3 步：Python

```powershell
winget install --id Python.Python.3.12 --exact --source winget
```

> 装完**新开一个终端**（winget 装的 Python 的 PATH 是新加的）。

### 第 4 步：GTK4 + Zig + Ghostty 源码 + 编译 + 契约

```powershell
cd <仓库目录>            # 也就是 clone 下来的 puttyplus
powershell -ExecutionPolicy Bypass -File tools\win_build.ps1
```

它会：下载并校验 GTK4（固定 2026.10.0 + sha256）⇒ 下载 Zig（pip）⇒
下载并校验 Ghostty 源码 ⇒ `cargo build --release` ⇒ 跑 **73 条命令行契约**。

⚠️ 第一次编译 `libghostty-vt` 要跑 Zig，**比较慢**（十几分钟）⇒ 别以为
卡死了。

### 第 5 步：把环境变量持久化（**每个新终端都带着**）

```powershell
powershell -ExecutionPolicy Bypass -File tools\win_env.ps1 -Check   # 先只检查
powershell -ExecutionPolicy Bypass -File tools\win_env.ps1          # 正式配
```

它把 `PKG_CONFIG_PATH` / `LIB` / `INCLUDE` / `ZIG` / `GHOSTTY_SOURCE_DIR` /
`CARGO_TARGET_DIR` / `PATH` 写成**用户级** ⇒ 以后开任何终端直接敲
`cargo build` 就行。

⚠️ `INCLUDE` / `LIB` 是**追加**不是覆盖（MSVC 那套路径由
`VsDevCmd.bat` 在进程里前置上去）。

⚠️ `CARGO_TARGET_DIR` 默认 `%USERPROFILE%\t`（**要短**：Windows 的
MAX_PATH 是 260，Ghostty 的源码树很深 ⇒ 默认路径下很容易超限）。

---

## 3. 一条命令装完全套

不想一步步来，就跑这个（**管理员** PowerShell）：

```powershell
cd <仓库目录>
powershell -ExecutionPolicy Bypass -File tools\win_setup.ps1
```

它按上面的顺序做，**每一步都打印在做什么**；想先看它要干什么：

```powershell
powershell -ExecutionPolicy Bypass -File tools\win_setup.ps1 -WhatIf
```

---

## 4. 验证（装完之后)

```powershell
# ① 编译
cargo build --release

# ② 单测（642 个）
cargo test --lib

# ③ 命令行契约（73 条黄金基准）—— 要 bash
bash tools/cli_contract_check.sh
```

⚠️ **单开一个终端**再跑（环境变量是持久化到用户级的，新终端才带）。

⚠️ `cargo test --lib` 在 Windows 上**只跑单测**：example 里有直接用 vte4
的（Windows 上编不过）⇒ `cargo test`（不带 `--lib`）会把 example 也编一遍
⇒ 会失败。用 `cargo test --lib`。

---

## 5. 日常怎么用

| 想干什么 | 命令 |
|---|---|
| 编译 | `cargo build --release`（产物 `target\release\puttyplus.exe`） |
| 跑单测 | `cargo test --lib` |
| 跑一个 GUI 冒烟 | `cargo run --example ghostty_view_smoke --features ghostty` |
| 跑那 73 条契约 | `bash tools/cli_contract_check.sh` |
| 跑全部 GUI 冒烟 | `bash tools/win_smoke.ps1` |
| 出一条能拷走的包 | `powershell -ExecutionPolicy Bypass -File tools\win_build.ps1` |

⚠️ **exe 单独拷走跑不起来**：GTK / glib / pango 的 DLL 都在 `C:\gtk\bin` 里
⇒ 要拷就连 DLL 一起拷（`win_build.ps1` 最后会提示命令）。

⚠️ **终端后端**：Windows 上默认走 **ghostty 自绘**（不依赖 VTE）。
`PP_TERM_BACKEND=ghostty` 可以显式指定；`PP_TERM_SSH=inproc|spawn` 切换
SSH 传输（默认 `inproc` = 进程内 russh，不 spawn 任何外部程序）。

⚠️ **Linux 主线仍走 VTE + spawn 用户自己的 ssh** ⇒ 两边的差异只有这四处，
其余全是同一份代码（`src/` 里 85000+ 行，平台相关的 `cfg` 只有 96 行）。

---

## 6. 出问题怎么办

| 症状 | 原因 / 怎么办 |
|---|---|
| `linker 'link.exe' not found` | 当前 PowerShell 没激活 MSVC ⇒ 跑 `tools\win_msvc_env.ps1`（或**新开终端**） |
| `The system cannot find the file specified`（pkg-config） | `PKG_CONFIG_PATH` 没设 ⇒ 跑 `tools\win_env.ps1` |
| 编译报路径太长 | `CARGO_TARGET_DIR` 指到短路径（`D:\t` 或 `%USERPROFILE%\t`） |
| `zig` 找不到 | `ZIG` 没设 ⇒ `tools\win_env.ps1`；或 `pip install ziglang==0.17.1` |
| Ghostty 源码下载失败 | `win_build.ps1` 会自动退回 GitHub codeload（那条路不校验内容） |
| 契约脚本报 `$'\r': command not found` | 仓库的 `.gitattributes` 已钉 LF；若是旧 checkout ⇒ `git rm --cached -r . && git reset --hard` |
| 契约脚本报权限/换行怪事 | Git Bash 的 MSYS 路径转换 ⇒ 脚本里已 `export MSYS_NO_PATHCONV=1`，确认 Git for Windows 是最新版 |
