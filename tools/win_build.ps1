<#
.SYNOPSIS
    在 Windows 上构建 PuTTYPlus（跨平台版）并跑命令行契约检查。

.DESCRIPTION
    ⚠️ **Windows 版只能在 Windows 上编**（Linux 交叉编译会卡在 glib-sys
    需要的 Windows sysroot 上）—— 这份脚本就是给「自己的 Windows 机器」
    用的。

    它做四件事（跟 CI 里那套一样）：

    1. 检查 Rust 的 msvc 工具链（Windows 版必须用 MSVC 目标）；
    2. 准备 **GTK 4**（Windows 没有系统包 ⇒ 用 gvsbuild 的预编译版；
       已经装过的话跳过）；
    3. 准备 **Zig 0.17**（编译 libghostty-vt 要用；从 PyPI 的 ziglang wheel
       取，已经装过就跳过）；
    4. `cargo build --release`，然后跑 **73 条命令行契约**（Git Bash）。

    产物：`target\release\puttyplus.exe`（外加 GTK 的 DLL 才能跑，
    见下面的「运行」）。

.PARAMETER GtkDir
    GTK 4 的目录。默认 `C:\gtk`（不存在就自动下载解压到这儿）。

.PARAMETER SkipBuild
    只跑契约检查，不重新编译。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\win_build.ps1

.EXAMPLE
    # 已经自己装好 GTK4 了
    powershell -ExecutionPolicy Bypass -File tools\win_build.ps1 -GtkDir D:\gtk
#>
param(
    [string]$GtkDir = "C:\gtk",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

function Say([string]$msg) { Write-Host "==> $msg" }

# ---------------------------------------------------------------- 1) Rust
Say "检查 Rust（MSVC 目标）"
$rustup = Get-Command rustup -ErrorAction SilentlyContinue
if (-not $rustup) {
    throw "没找到 rustup ⇒ 先装 Rust：https://rustup.rs/（默认选 MSVC 工具链）"
}
$installed = (rustup target list --installed) -split "`n" | ForEach-Object { $_.Trim() }
if ($installed -notcontains "x86_64-pc-windows-msvc") {
    Say "装 MSVC 目标"
    rustup target add x86_64-pc-windows-msvc
}

# ---------------------------------------------------------------- 2) GTK4
if (-not (Test-Path (Join-Path $GtkDir "bin"))) {
    Say "下载 GTK 4（gvsbuild 预编译版）"
    # ⚠️ **固定版本 + 校验 sha256**（不再问 API 要「最新」）：
    #    - 不依赖 GitHub API（限流/抖动就不会挂）；
    #    - 每台机器拿到的 GTK 一模一样 ⇒ 结果可复现。
    #    固定版真拿不到时，才退回「最新 release」。
    # ⚠️ 版本要 ≥ 4.18（gtk4-rs 0.11 的 v4_18 特性要求）⇒ 2026.10.0 = GTK 4.24.1。
    $tag = "2026.10.0"
    $name = "GTK4_Gvsbuild_${tag}_x64.zip"
    $url = "https://github.com/wingtk/gvsbuild/releases/download/$tag/$name"
    $sha = "8005b860447e73b10c7f508b1f348e2b6390e01dd61fd5c617b8190a046022fe"
    $zip = Join-Path $env:TEMP "gtk4.zip"

    $got = $false
    for ($i = 1; $i -le 3 -and -not $got; $i++) {
        try {
            Say "下载固定版本 $name（第 $i 次）"
            Invoke-WebRequest -Uri $url -OutFile $zip
            $actual = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
            if ($actual -ne $sha) { throw "sha256 对不上：期望 $sha，实际 $actual" }
            $got = $true
        } catch {
            Write-Warning "第 $i 次失败：$($_.Exception.Message)"
            if ($i -lt 3) { Start-Sleep -Seconds 10 }
        }
    }

    if (-not $got) {
        Say "固定版本拿不到 ⇒ 退回「最新 release」"
        $rel = Invoke-RestMethod -Headers @{ "User-Agent" = "puttyplus" } `
                -Uri "https://api.github.com/repos/wingtk/gvsbuild/releases/latest"
        $asset = $rel.assets |
            Where-Object { $_.name -match "^GTK4_Gvsbuild_.*_x64\.zip$" } |
            Select-Object -First 1
        if (-not $asset) { throw "没找到 GTK4 的 Windows 预编译包（release $($rel.tag_name)）" }
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip
    }

    if (Test-Path $GtkDir) { Remove-Item -Recurse -Force $GtkDir }
    Expand-Archive -Path $zip -DestinationPath $GtkDir -Force
}
$pkgconfig = Join-Path $GtkDir "lib\pkgconfig"
if (-not (Test-Path $pkgconfig)) { throw "找不到 $pkgconfig（gvsbuild 的包结构变了？）" }
$bin = Join-Path $GtkDir "bin"
if (-not (Test-Path (Join-Path $bin "pkg-config.exe"))) {
    if (Test-Path (Join-Path $bin "pkgconf.exe")) {
        Copy-Item (Join-Path $bin "pkgconf.exe") (Join-Path $bin "pkg-config.exe")
    } else {
        throw "GTK 目录里既没有 pkgconf.exe 也没有 pkg-config.exe"
    }
}
$env:Path = "$bin;$env:Path"
$env:PKG_CONFIG_PATH = $pkgconfig
$env:LIB = "$(Join-Path $GtkDir 'lib');$env:LIB"
$env:INCLUDE = @(
    (Join-Path $GtkDir "include"),
    (Join-Path $GtkDir "include\cairo"),
    (Join-Path $GtkDir "include\glib-2.0"),
    (Join-Path $GtkDir "include\gobject-introspection-1.0"),
    (Join-Path $GtkDir "lib\glib-2.0\include")
) -join ";"

# ---------------------------------------------------------------- 3) Zig
if (-not $env:ZIG) {
    Say "装 Zig 0.17（PyPI 的 ziglang wheel）"
    # ⚠️ **python 的命令名在 Windows 上不统一**：`python` 常常没有
    #    （Microsoft Store 的别名 / 只有 `py` / WSL 里的 `python3`）
    #    ⇒ 三种都试一遍，别卡在这儿。
    $py = $null
    foreach ($cand in @("python", "py", "python3")) {
        $cmd = Get-Command $cand -ErrorAction SilentlyContinue
        if ($cmd) { $py = $cand; break }
    }
    if (-not $py) {
        throw "没找到 python（试过 python / py / python3）⇒ 装 Python，或自己装 Zig 0.17.x 并设 ZIG 环境变量"
    }
    Say "用 $py 装 ziglang"
    & $py -m pip install --upgrade pip | Out-Null
    & $py -m pip install "ziglang==0.17.1" | Out-Null
    $zigdir = & $py -c "import ziglang, os; print(os.path.dirname(ziglang.__file__))"
    $env:ZIG = Join-Path $zigdir "zig.exe"
    $env:Path = "$zigdir;$env:Path"
}
if (-not (Test-Path $env:ZIG)) { throw "Zig 不在 $env:ZIG ⇒ 自己装个 Zig 0.17.x 并设 ZIG 环境变量" }

# ------------------------------------------------- 4) Ghostty 源码（可选）
# ⚠️ `ghostty-vt-sys` 的 build.rs 会自己下载 Ghostty 源码（走 ureq，
#    **Windows 上用 NativeTLS** ⇒ 有时失败）。⇒ 这里先自己备好，
#    用 GHOSTTY_SOURCE_DIR 指过去，build.rs 就不联网了。
if (-not $env:GHOSTTY_SOURCE_DIR) {
    Say "准备 Ghostty 源码"
    $commit = "83fb95624b0439ee660c2679b43b38c8b63769cd"
    $sha = "ed57ae9b95581c88f2614e976c1d3f146c02d443a3c26139200eb9de6e645eb4"
    $gdir = Join-Path $env:TEMP "ghostty-src"
    New-Item -ItemType Directory -Force $gdir | Out-Null
    $official = Join-Path $gdir "libghostty-vt-source.tar.gz"
    $codeload = Join-Path $gdir "ghostty-$commit.tar.gz"
    $tarball = $official
    if (-not (Test-Path $official) -and -not (Test-Path $codeload)) {
        $url = "https://tip.files.ghostty.org/$commit/libghostty-vt-source.tar.gz"
        try {
            Invoke-WebRequest -Uri $url -OutFile $official
            $tarball = $official
        } catch {
            Say "官方 tarball 不通 ⇒ 改用 GitHub codeload"
            Invoke-WebRequest `
                -Uri "https://codeload.github.com/ghostty-org/ghostty/tar.gz/$commit" `
                -OutFile $codeload
            $tarball = $codeload
        }
    } elseif (Test-Path $official) {
        $tarball = $official
    } else {
        $tarball = $codeload
    }
    # ⚠️ **官方 tarball 的 sha256 是锁死的**；codeload 那份是整棵源码树，
    #    sha 必然对不上 ⇒ 只在官方那份上校验。
    if ($tarball -eq $official) {
        $actual = (Get-FileHash $tarball -Algorithm SHA256).Hash.ToLower()
        if ($actual -ne $sha) {
            throw "Ghostty 源码 tarball 的 sha256 对不上：期望 $sha，实际 $actual"
        }
    }
    tar -xzf $tarball -C $gdir
    $root = Get-ChildItem $gdir -Directory |
            Where-Object { Test-Path (Join-Path $_.FullName "build.zig") } |
            Select-Object -First 1
    if ($root) { $env:GHOSTTY_SOURCE_DIR = $root.FullName }
}
if ($env:GHOSTTY_SOURCE_DIR) { Say "Ghostty 源码：$($env:GHOSTTY_SOURCE_DIR)" }

# ---------------------------------------------------------------- 编译
if (-not $SkipBuild) {
    Say "编译（release）"
    cargo build --release --locked
}

# ---------------------------------------------------------------- 契约
Say "命令行契约（73 条黄金基准）"
$bash = Get-Command bash -ErrorAction SilentlyContinue
if (-not $bash) {
    throw "没找到 bash ⇒ 契约检查要用 **Git Bash**（Windows 自带 Git 里有）"
}
& bash tools/cli_contract_check.sh --selftest
if ($LASTEXITCODE -ne 0) { throw "归一化自测没过（不是程序的问题，是脚本/环境的问题）" }
& bash tools/cli_contract_check.sh
if ($LASTEXITCODE -ne 0) { throw "命令行契约没过" }

# ---------------------------------------------------------------- 收尾
$exe = Join-Path (Get-Location) "target\release\puttyplus.exe"
Say "产物：$exe"
Write-Host ""
Write-Host "⚠️ **exe 单独拿走跑不起来**：GTK / glib / pango 的 DLL 都在 $bin 里"
Write-Host "   ⇒ 要拷走的话，把它们一起放在 exe 旁边："
Write-Host "       Copy-Item `"$bin\*.dll`" -Destination (Split-Path $exe) -Force"
Write-Host ""
Write-Host "⚠️ **GUI 那一半要人来验**（CI 上没有桌面会话）："
Write-Host "   见 docs/跨平台-构建说明.md 的「Windows 真机验证清单」（10 条）"
