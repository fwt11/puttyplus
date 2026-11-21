<#
.SYNOPSIS
    本机出 PuTTYPlus 的 Windows 安装包（Inno Setup 6，阶段 121）。

.DESCRIPTION
    与 ci-windows.yml 的「打包安装包」步骤同一套逻辑：

    1. staging `release-windows\`：bin\（exe + GTK DLL）+ share\icons
       （Adwaita/hicolor 图标主题）——⚠️ **层级不能变**：GTK 按「自己 DLL
       所在目录叫 bin ⇒ 上一级是前缀」找图标主题 ⇒ share\ 必须在 bin\ 旁
       （全平铺就是 2026-11-20 用户报的「GitHub 下载版图标全没有」）；
    2. 找 Inno Setup 6 的 ISCC.exe（env:ISCC → 标准路径 → PATH）；
       ⚠️ **找不到就明说，不自动安装**（用户拍板：本机不装 Inno Setup，
       安装包只在 CI 出；要本机出就自己装一个或设 ISCC 指向 ISCC.exe）；
    3. iscc /DAppVersion=<Cargo.toml 的版本> tools\windows-installer.iss。

    产物：release-windows\puttyplus-windows-x86_64-setup.exe

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\win_build.ps1      # 先编 exe
    powershell -ExecutionPolicy Bypass -File tools\win_installer.ps1  # 再打安装包
#>
param(
    [string]$GtkDir = "C:\gtk",
    [string]$ExePath = ""
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

function Say([string]$msg) { Write-Host "==> $msg" }

# ------------------------------------------------------- 1) 找 ISCC
$iscc = $null
if ($env:ISCC -and (Test-Path $env:ISCC)) { $iscc = $env:ISCC }
foreach ($p in @(
    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe",
    "C:\Program Files\Inno Setup 6\ISCC.exe"
)) {
    if (-not $iscc -and (Test-Path $p)) { $iscc = $p }
}
if (-not $iscc) {
    $cmd = Get-Command iscc -ErrorAction SilentlyContinue
    if ($cmd) { $iscc = $cmd.Source }
}
if (-not $iscc) {
    throw "没找到 Inno Setup 6（ISCC.exe）。`n本机拍板不装它 ⇒ 安装包只在 CI 出（push 后看 Windows job 的 pkg-windows artifact）。`n要在本机出：装 Inno Setup 6，或把 env:ISCC 指向 ISCC.exe。"
}

# ------------------------------------------------------- 2) 版本号
$m = Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1
if (-not $m) { throw "Cargo.toml 里没有 version = " }
$ver = $m.Matches[0].Groups[1].Value
Say "版本 $ver"

# ------------------------------------------------------- 3) staging
if (-not $ExePath) { $ExePath = "target\release\puttyplus.exe" }
if (-not (Test-Path $ExePath)) { throw "没有 $ExePath ⇒ 先跑 tools\win_build.ps1" }
$bin = Join-Path $GtkDir "bin"
if (-not (Test-Path (Join-Path $bin "gtk-4-1.dll"))) {
    throw "GTK 不在 $GtkDir ⇒ 先跑 tools\win_build.ps1"
}
$icons = Join-Path $GtkDir "share\icons"
if (-not (Test-Path $icons)) {
    throw "图标主题不在 $icons（gvsbuild 的包里应该有 Adwaita + hicolor）"
}
$stage = "release-windows"
# ⚠️ **ISCC 解析 Source:/OutputDir 是相对脚本所在目录（tools\），不是相对
#    当前目录** ⇒ 传给 /DStageDir 的必须是绝对路径。
$stage_abs = Join-Path (Get-Location) $stage
New-Item -ItemType Directory -Force "$stage\bin", "$stage\share" | Out-Null
Copy-Item $ExePath "$stage\bin\" -Force
Copy-Item "$bin\*.dll" "$stage\bin\" -Force
# 图标主题整棵重拷（避免上次残留的旧主题混进来）
if (Test-Path "$stage\share\icons") { Remove-Item "$stage\share\icons" -Recurse -Force }
Copy-Item $icons "$stage\share\icons" -Recurse -Force
Say "staging：$stage\bin（exe + DLL）+ $stage\share\icons"

# ------------------------------------------------------- 4) 打安装包
Say "ISCC：$iscc"
# ⚠️ 简体中文语言文件在 Inno 6.x 的 Languages\Unofficial\ 下（非官方维护
#    批次；写 Languages\ 根路径 ISCC 必挂，2026-11-21 CI 实测）。新版可能
#    移回根目录 ⇒ 依次找，都找不到退回英文 Default.isl（不挂，只降级）。
$innoDir = Split-Path $iscc -Parent
$lang = @("Languages\Unofficial\ChineseSimplified.isl",
          "Languages\ChineseSimplified.isl") |
  ForEach-Object { Join-Path $innoDir $_ } |
  Where-Object { Test-Path $_ } |
  Select-Object -First 1
if (-not $lang) {
    Write-Warning "没找到 ChineseSimplified.isl ⇒ 安装器界面退回英文"
    $lang = Join-Path $innoDir "Default.isl"
}
Say "语言文件：$lang"
$out = & $iscc "/DAppVersion=$ver" "/DLangFile=$lang" "/DStageDir=$stage_abs" "tools\windows-installer.iss" 2>&1
$code = $LASTEXITCODE
$out | ForEach-Object { Write-Host "$_" }
if ($code -ne 0) {
    Write-Host "---- ISCC 失败输出的最后 25 行 ----"
    $out | Select-Object -Last 25 | ForEach-Object { Write-Host "  $_" }
    throw "ISCC 失败（退出码 $code）"
}
if ($LASTEXITCODE -ne 0) { throw "ISCC 失败（退出码 $LASTEXITCODE）" }
$out = Join-Path $stage "puttyplus-windows-x86_64-setup.exe"
if (-not (Test-Path $out)) { throw "ISCC 成功但找不到 $out" }
Say "产物：$out"
Get-Item $out | Format-List Name, Length, LastWriteTime
