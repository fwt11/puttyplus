# 阶段 C8：在当前 PowerShell 会话里激活 MSVC（cl.exe / link.exe 进 PATH）
#
# 干什么用：Rust 的 `x86_64-pc-windows-msvc` target 要链接器（link.exe），
# 而它住在 Visual Studio 的 VC 目录里，**只有激活过「开发者命令提示符」
# 的会话才在 PATH 上** ⇒ 普通 PowerShell 里直接 `cargo build` 会报
# "linker `link.exe` not found"。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\win_msvc_env.ps1
#   # 或者在当前会话里 dot-source（环境变量留在当前窗口）：
#   . .\tools\win_msvc_env.ps1
#
# ⚠️ **VS 的许可条款**：Visual Studio 生成工具在**非商业用途**下免费。
#    商用要买许可。这台机器上的用途由用户自己负责确认。

$ErrorActionPreference = "Stop"
function Say([string]$m) { Write-Host "==> $m" }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) {
    throw "找不到 vswhere.exe（${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\）⇒ 没装 Visual Studio 生成工具？"
}

$vc = & $vswhere -latest -products * `
         -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
         -property installationPath
if (-not $vc) {
    throw "找到了 Visual Studio，但**没有 C++ 生成工具**（组件 Microsoft.VisualStudio.Component.VC.Tools.x86.x64）"
}
Say "Visual Studio：$vc"

$devcmd = Join-Path $vc "Common7\Tools\VsDevCmd.bat"
if (-not (Test-Path $devcmd)) {
    throw "找不到 $devcmd"
}

# ⚠️ **怎么把 VsDevCmd.bat 设的环境变量搬进当前 PowerShell**：
#    让 cmd 跑一遍「VsDevCmd.bat && set」，把它的输出逐行读回来，
#    再设成当前进程的環境变量（只影响当前会话，不写用户级）。
$lines = cmd /c "`"$devcmd`" -arch=x64 -host_arch=x64 -no_logo >nul && set"
$count = 0
foreach ($line in $lines) {
    if ($line -match "^([^=]+)=(.*)$") {
        [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        $count++
    }
}
Say "导入 $count 个环境变量"

if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    throw "导入后 PATH 里仍然没有 link.exe ⇒ VS 安装可能不完整"
}
Say "link.exe：$((Get-Command link.exe).Source)"
Say "现在可以 cargo build 了"
