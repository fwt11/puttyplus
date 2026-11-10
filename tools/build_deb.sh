#!/usr/bin/env bash
# 把 PuTTYPlus（Rust 主线）打成 deb 包。
#
#   bash tools/build_deb.sh            # 只打包
#   bash tools/build_deb.sh --test     # 先跑单元测试再打包
#   bash tools/build_deb.sh --clean    # 先 cargo clean 再打包（慢）
#
# 产物：../puttyplus_<版本>_<架构>.deb（放在仓库根目录下）。
# 依赖：cargo / pkg-config / build-essential / libgtk-4-dev /
#       libvte-2.91-gtk4-dev / debhelper（dpkg-buildpackage）
set -euo pipefail

cd "$(dirname "$(readlink -f "$0")")/.."   # -> 仓库根目录

# 本机 rustup 代理会被沙箱干扰（报 "unknown proxy name"），直连 toolchain。
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"

run_tests=0
do_clean=0
for arg in "$@"; do
    case "$arg" in
        --test)  run_tests=1 ;;
        --clean) do_clean=1 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "未知参数: $arg（可用: --test / --clean）" >&2
            exit 2
            ;;
    esac
done

say() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }

say "版本: $(grep -m1 '^version' Cargo.toml)"
say "cargo: $(cargo --version 2>/dev/null | tail -1)"

# ⚠️ **Zig 预检**（2026-11-10）：终端引擎 ghostty 现在是**默认特性**
#    ⇒ 编译 libghostty-vt 要 Zig 0.17.x。缺了就早说人话，别让 cargo
#    抛一堆看不懂的 build.rs 错误。
if [ -n "${ZIG:-}" ]; then
    say "zig: $("$ZIG" version 2>&1 | tail -1)"
elif command -v zig >/dev/null 2>&1; then
    say "zig: $(zig version 2>&1 | tail -1)"
else
    echo "!! 缺少 Zig 0.17：ghostty 是默认特性，编译 libghostty-vt 需要它。" >&2
    echo "   装法一：官方 tarball https://ziglang.org/download/0.17.1/" >&2
    echo "   装法二：pip install ziglang==0.17.1（同一个 Zig 二进制）" >&2
    echo "   装好后：export ZIG=/path/to/zig" >&2
    exit 1
fi

if [ "$do_clean" = 1 ]; then
    say "cargo clean"
    cargo clean
fi

if [ "$run_tests" = 1 ]; then
    say "cargo test（全部单元测试）"
    cargo test --locked
fi

say "cargo build --release --locked"
cargo build --release --locked

# dpkg-checkbuilddeps 只认 dpkg 管理的 cargo。本机的 cargo 来自 rustup
# （apt 里的 cargo 是 1.81，编不了 edition 2024），所以 dpkg 视角下
# 「构建依赖未满足」——用 -d 跳过这项校验，其余校验照旧。
extra_args=()
if ! dpkg-query -W -f='${Status}' cargo 2>/dev/null | grep -q "install ok"; then
    echo "注意：系统未安装 dpkg 管理的 cargo（用 rustup 的 cargo 构建），跳过依赖校验" >&2
    extra_args+=(-d)
fi

say "dpkg-buildpackage -us -uc -b ${extra_args[*]:-}"
if [ ${#extra_args[@]} -gt 0 ]; then
    dpkg-buildpackage -us -uc -b "${extra_args[@]}"
else
    dpkg-buildpackage -us -uc -b
fi

deb="$(ls -t ../puttyplus_*.deb 2>/dev/null | head -1)"
if [ -z "$deb" ]; then
    echo "没有找到生成的 .deb" >&2
    exit 1
fi

say "完成: $(cd "$(dirname "$deb")" && pwd)/$(basename "$deb")"
echo "--- 包信息 ---"
dpkg-deb -I "$deb" | sed -n '1,20p'
echo "--- 安装 ---"
echo "  sudo dpkg -i $(basename "$deb")"
