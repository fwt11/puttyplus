# 阶段 C9-二：Windows 开发环境一键安装（把 Windows 变成开发 + 验证工作台）
#
# 干什么用：在这台 Windows 机器上装齐「编译 PuTTYPlus 所需的一切」，
# 装完就能直接 `cargo build` / `cargo test --lib` / `cargo run --example`
# （**有桌面会话 ⇒ GUI 冒烟能真跑**），不必每轮等 CI 编一个 exe 下来手测。
#
# 用法：
#   # 管理员 PowerShell
#   powershell -ExecutionPolicy Bypass -File tools\win_setup.ps1
#   # 先看它要干什么（什么都不装）
#   powershell -ExecutionPolicy Bypass -File tools\win_setup.ps1 -WhatIf
#   # 跳过 VS 生成工具（你自己装）
#   powershell -ExecutionPolicy Bypass -File tools\win_setup.ps1 -SkipVS
#
# ⚠️ **要管理员权限**：VS 生成工具的安装器要提权（UAC 会弹窗）。
# ⚠️ **要联网**：VS 生成工具约 3~5 GB、GTK4 约 300 MB。
# ⚠️ 逐条命令的说明见 `docs/Windows-开发环境搭建.md`。

param(
    # 只打印要做什么，不真的装
    [switch]$WhatIf,
    # 跳过 VS 生成工具的安装（假设你已经装好了）
    [switch]$SkipVS,
    # VS 生成工具的安装路径（**要短**：Windows 的 MAX_PATH 是 260）
    [string]$VsDir = "C:\BuildTools"
)

$ErrorActionPreference = "Stop"
function Say([string]$m) { Write-Host "==> $m" }
function Warn([string]$m) { Write-Host "⚠ $m" -ForegroundColor Yellow }
function Die([string]$m) { throw $m }

$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $repo
Say "仓库目录：$repo"

# ------------------------------------------------------------ 1) VS 生成工具
function Get-VsPath {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { return $null }
    & $vswhere -latest -products * `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
        -property installationPath
}

$vs = Get-VsPath
if ($vs) {
    Say "VS 生成工具已在：$vs"
} elseif ($SkipVS) {
    Say "跳过 VS 生成工具（-SkipVS）"
} else {
    Say "装 VS 生成工具（C++ 生成工具 + Windows SDK，约 3~5 GB，要提权）"
    if ($WhatIf) {
        Write-Host "  [WhatIf] 本该执行："
        Write-Host "    下载 https://aka.ms/vs/17/release/vs_BuildTools.exe"
        Write-Host "    静默安装到 $VsDir（--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended）"
    } else {
        $bt = Join-Path $env:TEMP "vs_BuildTools.exe"
        Invoke-WebRequest -Uri "https://aka.ms/vs/17/release/vs_BuildTools.exe" -OutFile $bt
        & $bt --quiet --wait --norestart --nocache `
              --installPath $VsDir `
              --add Microsoft.VisualStudio.Workload.VCTools `
              --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
              --includeRecommended
        $vs = Get-VsPath
        if (-not $vs) { Die "VS 生成工具装完还是找不到（用 vswhere 查一下）" }
        Say "VS 生成工具装好了：$vs"
    }
}

# ------------------------------------------------------------ 2) Rust
function Get-RustMsvc {
    if (-not (Get-Command rustc -ErrorAction SilentlyContinue)) { return $false }
    $tgt = (& rustup target list --installed 2>$null) -join ", "
    return ($tgt -match "x86_64-pc-windows-msvc")
}

if (Get-RustMsvc) {
    Say "Rust（msvc）已就绪：$((& rustc --version) -replace "`r`n", "")"
} else {
    Say "装 Rust（stable · msvc host）"
    if ($WhatIf) {
        Write-Host "  [WhatIf] 本该执行：下载 rustup-init.exe 并"
        Write-Host "    `"-y --default-host x86_64-pc-windows-msvc --profile minimal`""
    } else {
        $ri = Join-Path $env:TEMP "rustup-init.exe"
        Invoke-WebRequest -Uri "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe" -OutFile $ri
        & $ri -y --default-host x86_64-pc-windows-msvc --profile minimal
        # 装完**当前**这个会话也能用（不然下面调 win_build.ps1 找不到 cargo）
        $cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
        if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
    }
}

# ------------------------------------------------------------ 3) Python
$py = $null
foreach ($cand in @("python", "py", "python3")) {
    if (Get-Command $cand -ErrorAction SilentlyContinue) { $py = $cand; break }
}
if ($py) {
    Say "Python 已就绪：$py"
} else {
    Say "装 Python（装 Zig 用）"
    if ($WhatIf) {
        Write-Host "  [WhatIf] 本该执行：winget install --id Python.Python.3.12 --exact --source winget"
    } else {
        if (Get-Command winget -ErrorAction SilentlyContinue) {
            & winget install --id Python.Python.3.12 --exact --source winget --accept-package-agreements --accept-source-agreements
        } else {
            Die "没有 winget ⇒ 请手动装 Python（https://www.python.org/downloads/windows/），装完重跑本脚本"
        }
        # winget 装的 Python 的 PATH 是新加的 ⇒ 当前会话未必认得
        $env:Path = [Environment]::GetEnvironmentVariable("Path", "Machine") + ";" +
                    [Environment]::GetEnvironmentVariable("Path", "User")
        $py = $null
        foreach ($cand in @("python", "py", "python3")) {
            if (Get-Command $cand -ErrorAction SilentlyContinue) { $py = $cand; break }
        }
        if (-not $py) { Die "Python 还是找不到 ⇒ 装完要**新开一个终端**再重跑本脚本" }
    }
}

if ($WhatIf) {
    Say "WhatIf：到此为止（没装任何东西）"
    Say "真装：去掉 -WhatIf 重跑"
    exit 0
}

# ------------------------------------------------------------ 4) GTK4 / Zig / Ghostty 源码 + 编译 + 契约
Say "准备 GTK4 / Zig / Ghostty 源码，并编译 + 跑 73 条契约"
& (Join-Path $repo "tools\win_build.ps1")
if ($LASTEXITCODE -ne 0) { Die "win_build.ps1 没过（看上面的输出）" }

# ------------------------------------------------------------ 5) 持久化环境变量
Say "把环境变量写成用户级（每个新终端都带着）"
& (Join-Path $repo "tools\win_env.ps1")
if ($LASTEXITCODE -ne 0) { Die "win_env.ps1 没过（看上面的输出）" }

Say "装完了"
Write-Host ""
Write-Host "**新开一个终端**，然后："
Write-Host "  cargo build --release        # 编译"
Write-Host "  cargo test --lib             # 单测"
Write-Host "  cargo run --example <名字>   # GUI 冒烟（Windows 上有桌面会话，能真跑）"
Write-Host "  bash tools/cli_contract_check.sh   # 73 条命令行契约（要 Git Bash）"
Write-Host ""
Write-Host "⚠️ **新终端里 cargo 找不到** ⇒ 跑一次 tools\win_msvc_env.ps1（激活 MSVC）"
Write-Host "   或者确认 %USERPROFILE%\.cargo\bin 在 PATH 里。"
