#!/bin/bash
# 在 Linux 上对「**不依赖 GTK** 的平台层」做 Windows 目标交叉编译检查。
#
# ## 为什么要有它
#
# Windows 版**不能在 Linux 上交叉编译**（`glib-sys` 要 Windows sysroot 与
# pkg-config 的 cross 配置）⇒ 平时改了平台层代码，Windows 上编不过要等
# CI（一次半小时）才发现。
#
# ⇒ 把**纯 std 的那部分**（本地路径语义、POSIX 权限的 Windows 折算）
# 单独放进一个隔离 crate，用 `cargo check --target x86_64-pc-windows-msvc`
# 过一遍 ⇒ 本地几秒就能发现 Windows 专属的编译错误。
#
# ## 覆盖什么
#
# - `src/platform/localpath.rs`（本地路径：判定/父目录/拼接/取名）
# - `src/platform/perm.rs`（POSIX 权限 → Windows 折算）
#
# ## 不覆盖什么
#
# 依赖 GTK/GIO 的那部分（clipboard / notify / exe / paths / ghostty_view…）
# ⇒ 那些只能靠 CI（`windows-latest`，见 `.github/workflows/ci-windows.yml`）。
#
# 用法：
#   bash tools/win_cross_check.sh
set -u
cd "$(dirname "$0")/.." || exit 2

TARGET="x86_64-pc-windows-msvc"
REPO="$(pwd)"

# ---- 检查目标有没有装（**不自动装**：往机器上装东西要先问用户）----
if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  echo "没装 $TARGET 这个编译目标 ⇒ 先跑："
  echo "    rustup target add $TARGET"
  echo "（装目标也算往机器上装东西 ⇒ 请确认后再执行）"
  exit 2
fi

# ---- 临时隔离 crate ----
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/src"
cat > "$TMP/Cargo.toml" <<EOF
[package]
name = "pp_wincheck"
version = "0.0.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[workspace]
EOF

cat > "$TMP/src/lib.rs" <<EOF
//! 隔离交叉检查用：只放**不依赖 GTK** 的平台层文件。
#![allow(dead_code)]
#![allow(rustdoc::broken_intra_doc_links)]

#[path = "$REPO/src/platform/localpath.rs"]
pub mod localpath;

#[path = "$REPO/src/platform/perm.rs"]
pub mod perm;
EOF

echo "== 交叉检查：$TARGET =="
( cd "$TMP" && cargo check --quiet --target "$TARGET" ) || {
  echo "❌ Windows 目标下有编译错误（见上面）"
  exit 1
}
echo "✅ Windows 目标：零错误"
