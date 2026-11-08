<#
.SYNOPSIS
    Windows 版的**自动**冒烟：能自动验的那部分（不用人盯着）。

.DESCRIPTION
    CI 上只能验「编译 + 单测 + 命令行契约」（Windows runner 没有桌面会话）
    ⇒ GUI 那一半要人看。这份脚本把**能自动化的那几条**做掉：

    1. **exe 能起来不崩**：起进程 → 等几秒 → 还在？→ 关掉。
       （这一步能抓「GTK 的 DLL 缺了」「ConPTY 起不来」这类最基础的问题）
    2. **命令行参数**：抽查几条 `--print-config` 的输出（协议/端口/用户/
       密码只打 <set> 不打明文）。
    3. **契约检查**：跑完整的 73 条（跟 CI 同一套脚本）。

    用法：
        powershell -ExecutionPolicy Bypass -File tools\win_smoke.ps1
        powershell -ExecutionPolicy Bypass -File tools\win_smoke.ps1 -Exe D:\path\puttyplus.exe

    ⚠️ **GUI 那部分（终端出字/中文输入/SFTP 面板）还是要人看**：
       见 docs/跨平台-构建说明.md 的「Windows 真机验证清单」。
#>
param(
    [string]$Exe = "",
    [string]$GtkDir = "C:\gtk"
)

$ErrorActionPreference = "Continue"
Set-Location (Split-Path -Parent $PSScriptRoot)

function Ok([string]$m) { Write-Host "  [OK] $m" -ForegroundColor Green }
function Bad([string]$m) { Write-Host "  [!!] $m" -ForegroundColor Red; $script:failed = $true }
$failed = $false

# ---------------------------------------------------------------- 定位 exe
if (-not $Exe) {
    $Exe = Join-Path (Get-Location) "target\release\puttyplus.exe"
}
if (-not (Test-Path $Exe)) {
    throw "找不到 $Exe ⇒ 先编译（tools\win_build.ps1）"
}
Write-Host "exe：$Exe"

# ⚠️ **GTK 的 DLL 要在 PATH 里**（否则进程一起来就缺 DLL 直接退）
$gtkBin = Join-Path $GtkDir "bin"
if (Test-Path $gtkBin) {
    $env:Path = "$gtkBin;$env:Path"
    Write-Host "PATH 加了：$gtkBin"
} else {
    Write-Warning "没找到 $gtkBin ⇒ exe 可能因为缺 GTK 的 DLL 起不来"
}

# ------------------------------------------------- 1) 起进程，不崩就算过
Write-Host "`n[1] exe 能起来不崩"
$proc = Start-Process -FilePath $Exe -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 5
if ($proc.HasExited) {
    Bad "进程已经退了（退出码 $($proc.ExitCode)）⇒ 多半是缺 DLL 或初始化失败"
} else {
    Ok "起来了，5 秒还在（PID $($proc.Id)）"
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

# ------------------------------------------------- 2) 命令行参数抽查
Write-Host "`n[2] 命令行参数抽查"
$cases = @(
    @{ Arg = @("--print-config", "-ssh", "h");                    Want = "proto=ssh host=h port=22" },
    @{ Arg = @("--print-config", "-P", "2222", "h");               Want = "port=2222" },
    @{ Arg = @("--print-config", "-l", "ops", "h");                Want = "user=ops" },
    @{ Arg = @("--print-config", "-pw", "secret", "h");           Want = "password=<set>" },
    @{ Arg = @("--print-config", "-nosuch", "h");                  Want = "host=h" }
)
foreach ($c in $cases) {
    $out = & $Exe @($c.Arg) 2>&1 | Out-String
    if ($out -match [regex]::Escape($c.Want)) {
        Ok "$($c.Arg -join ' ') ⇒ 含「$($c.Want)」"
    } else {
        Bad "$($c.Arg -join ' ') ⇒ 输出里没有「$($c.Want)」：$($out.Trim())"
    }
    # ⚠️ **密码绝不能出现在输出里**（契约：只打 <set>）
    if ($out -match "secret") {
        Bad "⚠️ 输出里出现了密码明文！"
    }
}

# ------------------------------------------------- 3) 73 条契约
Write-Host "`n[3] 命令行契约（73 条）"
$bash = Get-Command bash -ErrorAction SilentlyContinue
if (-not $bash) {
    Write-Warning "没找到 bash ⇒ 跳过（契约要用 Git Bash）"
} else {
    & bash tools/cli_contract_check.sh --selftest
    if ($LASTEXITCODE -ne 0) { Bad "归一化自测没过" } else { Ok "归一化自测 FAIL=0" }
    & bash tools/cli_contract_check.sh
    if ($LASTEXITCODE -ne 0) { Bad "契约没过" } else { Ok "契约 PASS=73 FAIL=0" }
}

# ---------------------------------------------------------------- 收尾
Write-Host ""
if ($failed) {
    Write-Host "❌ 有没过的地方（看上面）" -ForegroundColor Red
    exit 1
}
Write-Host "✅ 自动那部分全过" -ForegroundColor Green
Write-Host "⚠️ 但**GUI 那一半还没验**（终端出字 / 中文输入 / SFTP 面板）"
Write-Host "   ⇒ 见 docs/跨平台-构建说明.md 的「Windows 真机验证清单」（10 条）"
exit 0
