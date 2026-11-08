# 阶段 C8：Windows 本地开发环境（把 CI 那套搬到开发机上）
#
# 干什么用：**在 Windows 机器上直接开发**（本地 cargo build / test /
# run --example），不必每轮都等 CI 编一个 exe 下来手测。
#
# 干什么不做：它**不装**任何东西（VS 生成工具 / Rust / Python 得自己装，
# 装之前请先得到用户同意）。它只做两件事：
#   1. 检查工具链齐不齐，缺什么说清楚；
#   2. 把 CI 里那套环境变量**持久化**（用户级）⇒ 之后开的每个终端都带着
#      （win_build.ps1 设的变量只在它自己那一个 PowerShell 会话里有效
#      ⇒ 开个新终端就没了 ⇒ 迭代时每开一次终端都要重跑一遍 win_build.ps1，
#      太蠢）。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\win_env.ps1
#   powershell -ExecutionPolicy Bypass -File tools\win_env.ps1 -GtkDir D:\gtk
#   powershell -ExecutionPolicy Bypass -File tools\win_env.ps1 -Check
#
# ⚠️ **装 GTK / Zig / Ghostty 源码**：用 win_build.ps1（它会下载并校验
#    sha256）；本脚本只负责「把它们的位置写进环境变量」。
#    想一步到位（下载 + 编译 + 契约全跑一遍）：
#       powershell -ExecutionPolicy Bypass -File tools\win_build.ps1

param(
    # GTK4 目录（gvsbuild 解压后的那个）
    [string]$GtkDir = "C:\gtk",
    # cargo 的 target 目录（**要短**：Windows 的 MAX_PATH 是 260，
    # ghostty 的源码树很深 ⇒ 默认路径下很容易超限）
    [string]$TargetDir = "",
    # Ghostty 源码目录（留空 = 自己找）
    [string]$GhosttyDir = "",
    # 只检查，不改环境变量
    [switch]$Check
)

$ErrorActionPreference = "Stop"
function Say([string]$m) { Write-Host "==> $m" }
function Warn([string]$m) { Write-Host "⚠ $m" -ForegroundColor Yellow }

if (-not $TargetDir) {
    # 默认放在用户目录下（比仓库路径短）；MAX_PATH 实在不够就改用 D:\t
    $TargetDir = Join-Path $env:USERPROFILE "t"
}

# ------------------------------------------------------- 1. 工具链检查
$missing = @()

# Rust（msvc target）
if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) {
    $missing += "Rust（rustup）：装 stable-x86_64-pc-windows-msvc"
} else {
    $ver = (& rustc --version) -replace "`r`n", ""
    Say "Rust：$ver"
    $tgt = (& rustup target list --installed 2>$null) -join ", "
    if ($tgt -notmatch "x86_64-pc-windows-msvc") {
        $missing += "Rust 的 msvc target：rustup target add x86_64-pc-windows-msvc"
    }
}

# MSVC 链接器（**Rust 的 msvc target 必须要它**）
$cl = Get-Command cl.exe -ErrorAction SilentlyContinue
if ($cl) {
    Say "MSVC：cl.exe 已在 PATH"
} else {
    # 不在 PATH 里**不等于**没装：装了「Visual Studio 生成工具」的话，
    # cl.exe 在 VC\Tools\MSVC\<版本>\bin\Hostx64\x64\ 下，只是当前 shell
    # 没激活 ⇒ 用 vswhere 找一下。
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vc = $null
    if (Test-Path $vswhere) {
        $vc = & $vswhere -latest -products * `
                 -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                 -property installationPath
    }
    if ($vc) {
        Say "MSVC 生成工具：$vc"
        Say "  （cl.exe 不在当前 PATH ⇒ 每个新终端先跑一次："
        Say "     powershell -ExecutionPolicy Bypass -File tools\win_msvc_env.ps1）"
    } else {
        $missing += "Visual Studio 生成工具（C++ 生成工具 + Windows SDK）：Rust 的 msvc target 要链接器"
    }
}

# Python（装 Zig 用）
$py = $null
foreach ($cand in @("python", "py", "python3")) {
    if (Get-Command $cand -ErrorAction SilentlyContinue) { $py = $cand; break }
}
if (-not $py) {
    $missing += "Python（装 Zig 0.17 用：pip install ziglang==0.17.1）"
} else {
    Say "Python：$py"
}

# ------------------------------------------------------- 2. GTK4
$pkgconfig = Join-Path $GtkDir "lib\pkgconfig"
$gtkBin = Join-Path $GtkDir "bin"
if (-not (Test-Path $pkgconfig)) {
    $missing += "GTK4（gvsbuild 2026.10.0）：跑 tools\win_build.ps1 会自动下载到 $GtkDir"
} else {
    Say "GTK4：$GtkDir"
    # system-deps 找的是叫 pkg-config 的可执行文件 ⇒ gvsbuild 里叫 pkgconf.exe
    if (-not (Test-Path (Join-Path $gtkBin "pkg-config.exe"))) {
        if (Test-Path (Join-Path $gtkBin "pkgconf.exe")) {
            Copy-Item (Join-Path $gtkBin "pkgconf.exe") (Join-Path $gtkBin "pkg-config.exe")
            Say "  已把 pkgconf.exe 复制成 pkg-config.exe"
        } else {
            $missing += "GTK4 里的 pkgconf.exe / pkg-config.exe 都不在（包结构变了？）"
        }
    }
}

# ------------------------------------------------------- 3. Zig
$zigExe = $env:ZIG
$zigDir = ""
if ($zigExe -and (Test-Path $zigExe)) {
    $zigDir = Split-Path $zigExe
} else {
    foreach ($cand in @("python", "py", "python3")) {
        $c = Get-Command $cand -ErrorAction SilentlyContinue
        if (-not $c) { continue }
        $dir = & $c -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))" 2>$null
        if ($dir -and (Test-Path (Join-Path $dir "zig.exe"))) {
            $zigExe = Join-Path $dir "zig.exe"
            $zigDir = $dir
            break
        }
    }
}
if (-not $zigExe) {
    $missing += "Zig 0.17（编译 libghostty-vt 用）：pip install ziglang==0.17.1"
} else {
    Say "Zig：$zigExe（$(& $zigExe version)）"
}

# ------------------------------------------------------- 4. Ghostty 源码
# build.rs 自己下载在 Windows 上走 NativeTLS ⇒ 有时失败 ⇒ 用现成的源码树。
$ghostty = $GhosttyDir
if (-not $ghostty) {
    foreach ($base in @("C:\g", (Join-Path $env:TEMP "ghostty-src"))) {
        if (-not (Test-Path $base)) { continue }
        $hit = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
               Where-Object { Test-Path (Join-Path $_.FullName "build.zig") } |
               Select-Object -First 1
        if ($hit) { $ghostty = $hit.FullName; break }
    }
}
if (-not $ghostty) {
    $missing += "Ghostty 源码（build.rs 在 Windows 上自己下载不可靠）：跑 tools\win_build.ps1 会备好"
} else {
    Say "Ghostty 源码：$ghostty"
}

# ------------------------------------------------------- 5. 结论
if ($missing.Count -gt 0) {
    Write-Host ""
    Warn "还缺这些（**装之前请先得到用户同意**）："
    foreach ($m in $missing) { Write-Host "  - $m" }
    Write-Host ""
    Write-Host "GTK4 / Zig / Ghostty 源码这三样不用手动装："
    Write-Host "  powershell -ExecutionPolicy Bypass -File tools\win_build.ps1"
    Write-Host "它会下载、校验 sha256、编译、并跑一遍 73 条命令行契约。"
    Write-Host ""
    if ($Check) { exit 1 }
} else {
    Say "工具链齐了"
}

if ($Check) {
    Write-Host ""
    Say "只检查模式：没改任何环境变量。去掉 -Check 才会正式配置。"
    exit 0
}

# ------------------------------------------------------- 6. 写环境变量
# ⚠️ 写成**用户级** ⇒ 之后开的每个终端都带着。
# ⚠️ INCLUDE / LIB **只能追加，不能覆盖**：MSVC 的那套路径是
#    VsDevCmd.bat 在**进程里**设的（它会把自己的路径**前置**到已有的
#    INCLUDE/LIB 后面）⇒ 用户级只放 GTK 的路径 ⇒ 两边都在。
#    （要是把用户级的 INCLUDE 写成"只有 GTK"，别的工具链路径就没了。）
$envs = @{
    "PKG_CONFIG_PATH"    = $pkgconfig
    "GHOSTTY_SOURCE_DIR" = $ghostty
    "CARGO_TARGET_DIR"   = $TargetDir
    "ZIG"                = $zigExe
}
$gtkInclude = @(
    (Join-Path $GtkDir "include"),
    (Join-Path $GtkDir "include\cairo"),
    (Join-Path $GtkDir "include\glib-2.0"),
    (Join-Path $GtkDir "include\gobject-introspection-1.0"),
    (Join-Path $GtkDir "lib\glib-2.0\include")
)
# 新版 glib 的 introspection 目录名不一样（2.0 / 1.0）⇒ 在就在加
foreach ($extra in @("include\gobject-introspection-2.0", "include\gobject-introspection-2.0\glib")) {
    if (Test-Path (Join-Path $GtkDir $extra)) { $gtkInclude += (Join-Path $GtkDir $extra) }
}
$gtkInclude = ($gtkInclude | Where-Object { $_ } | Select-Object -Unique) -join ";"
$gtkLib = Join-Path $GtkDir "lib"

function Set-UserEnv([string]$name, [string]$value) {
    [Environment]::SetEnvironmentVariable($name, $value, "User")
    Set-Item -Path "Env:$name" -Value $value
    Write-Host "  $name = $value"
}

Say "写用户级环境变量"
foreach ($k in $envs.Keys) {
    $v = $envs[$k]
    if ($v) { Set-UserEnv $k $v }
}
# INCLUDE / LIB：追加（不覆盖）
foreach ($pair in @(@("INCLUDE", $gtkInclude), @("LIB", $gtkLib))) {
    $name = $pair[0]; $add = $pair[1]
    $cur = [Environment]::GetEnvironmentVariable($name, "User")
    if (-not $cur) { $cur = "" }
    # ⚠️ 必须 @() 包一层：$cur 为空时管道过滤后 $parts 是 $null，
    #    $null += "x" 会把数组退化成字符串 ⇒ 后面的 += 全变成**字符串
    #    拼接**，写出来的 INCLUDE 连分号都没有（真机踩过）。
    $parts = @($cur.Split(";") | Where-Object { $_ })
    foreach ($p in @($add.Split(";") | Where-Object { $_ })) {
        if ($parts -notcontains $p) { $parts += $p }
    }
    Set-UserEnv $name ($parts -join ";")
}

# PATH：GTK 的 bin + Zig 目录（用户级追加，不覆盖）
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$add = @()
if ($gtkBin -and ($userPath -notlike "*$gtkBin*")) { $add += $gtkBin }
if ($zigDir -and ($userPath -notlike "*$zigDir*")) { $add += $zigDir }
if ($add.Count -gt 0) {
    $newPath = (($userPath, ($add -join ";")) | Where-Object { $_ }) -join ";"
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    Write-Host "  Path += $($add -join ';')"
}

Say "配好了"
Write-Host ""
Write-Host "⚠️ **当前这个 PowerShell 窗口**已经生效；**新开的终端**也会生效。"
Write-Host ""
Write-Host "下一步（在仓库根目录）："
Write-Host "  cargo build --release          # 编译（第一次会编 libghostty-vt，慢）"
Write-Host "  cargo test --lib               # 638 个单测"
Write-Host "  cargo run --example <名字>     # GUI 冒烟（**Windows 上有桌面会话，能真跑**）"
Write-Host ""
Write-Host "⚠️ **73 条命令行契约要 bash** ⇒ 用 Git for Windows 自带的 Git Bash："
Write-Host "  bash tools/cli_contract_check.sh"
