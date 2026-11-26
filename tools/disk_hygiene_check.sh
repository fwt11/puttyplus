#!/bin/bash
# 构建产物卫生检查：防止「example 产物无限堆积」这件事复发。
#
# ⚠️ **为什么需要这个**（2026-11-26 实测）：
#   裸 `cargo test` 会**编译全部 example** ⇒ 本项目有 158 个 example，
#   每个都是独立可执行文件 ⇒ 一轮就是几十 G ⇒ 磁盘被写到 99%
#   （当时只剩 9.5G）。⇒ 守门一律用 `cargo test --lib`
#   （本仓库无 `tests/` 目录、无自定义 `[[test]]` 目标 ⇒ `--lib` 就是全量）。
#
# 本检查做两件事：
#   1. **静态扫**：脚本与 CI 工作流里有没有裸 `cargo test`（不带 --lib/--doc）
#      ⇒ 有就报错（这是堆积的根子）。
#   2. **看现状**：报一下 target/ 各子目录占用 ⇒ 提醒是不是该清了。
#
# 用法：
#   bash tools/disk_hygiene_check.sh          # 检查 + 报告占用
#   bash tools/disk_hygiene_check.sh --quiet  # 只查裸 cargo test，不报占用
set -u
cd "$(dirname "$0")/.." || exit 2

say() { printf '%s\n' "$*"; }

QUIET=0
for arg in "$@"; do
    case "$arg" in
        --quiet|-q) QUIET=1 ;;
        -h|--help)  sed -n '3,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数：$arg" >&2; exit 2 ;;
    esac
done

# ------------------------------------------------------------------
# 1) 静态扫：裸 `cargo test`
# ------------------------------------------------------------------
# 扫的范围：tools/*.sh 与 .github/workflows/*.yml
#   ⇒ **不扫 markdown**（AGENTS.md / ROADMAP 里满是「cargo test --lib」这类
#      说明文字，扫了全是误报）。
# 判定：把行尾注释去掉、把 `cargo   test` 的多空格归一，再看有没有
#      `cargo test` 且**不带** `--lib` / `--doc`。
# ------------------------------------------------------------------
found=""
for f in tools/*.sh .github/workflows/*.yml; do
    [ -f "$f" ] || continue
    # ⚠️ **跳过本脚本自己**：它的源码里写着 `cargo test` 字样（判定模式 + 报错
    #    文案）⇒ 不排除就会自己告自己。
    case "$f" in
        tools/disk_hygiene_check.sh) continue ;;
    esac
    n=0
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n + 1))
        code=$(printf '%s' "$line" | sed 's/#.*$//' | sed 's/cargo[[:space:]][[:space:]]*test/cargo test/g')
        case "$code" in
            *"cargo test"*) ;;
            *) continue ;;
        esac
        case "$code" in
            *--lib*|*--doc*) continue ;;
        esac
        found="$found$f:$n: $line
"
    done < "$f"
done

if [ -n "$found" ]; then
    echo "::error::发现裸 cargo test —— 它会编译全部 example ⇒ target/ 几十 G ⇒ 磁盘写满。"
    echo "::error::本仓库无 tests/ 目录、无自定义 [[test]] 目标 ⇒ 一律用 cargo test --lib。"
    printf '%s' "$found"
    exit 1
fi

say "✔ 没有裸 cargo test（脚本与 CI 工作流都带 --lib）"

# ------------------------------------------------------------------
# 2) 看现状：target/ 占用
# ------------------------------------------------------------------
[ "$QUIET" = 1 ] && exit 0

say ""
say "target/ 占用现状："
if [ -d target ]; then
    du -sh target/* 2>/dev/null | sort -rh | head -8 | while read -r sz dir; do
        say "  $sz  $dir"
    done
    total=$(du -sh target 2>/dev/null | awk '{print $1}')
    say "  合计：$total"
    say ""
    say "（超过 30G 就该跑 bash tools/clean_build.sh）"
else
    say "  （还没有 target/）"
fi
exit 0
