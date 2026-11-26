#!/bin/bash
# 构建产物清理：解决「target/ 无限堆积把磁盘写满」。
#
# ⚠️ **为什么需要这个**（2026-11-26 实测）：
#   - `examples/` 下有 **158 个 example**，每个都是**独立可执行文件**
#     （cargo 的 `examples/` 约定）⇒ 每个 ~240MB（带完整调试信息时）。
#   - cargo 每次重编都留一份带哈希的历史副本 ⇒ 实测 `target/debug/examples`
#     里堆到 **167 个不同版本的二进制、合计 39.7G**。
#   - 加上 `target/debug/incremental` 的 1097 个缓存目录（12.8G）
#     ⇒ target/ 共 70G ⇒ 磁盘只剩 9.5G（99%），**再编一次 release 就写满**。
#   ⇒ **治本在别处**（`cargo test --lib` + `Cargo.toml` 的 `debug = 1`），
#     本脚本负责**回收存量 + 兜底**。
#
# ⚠️ **本脚本只清「缓存」不碰「资产」**：
#   - 保留 `target/release/puttyplus`（交付用主二进制）
#   - 保留 `release/`（仓库根的交付目录）
#   - 保留 `target/debug/deps`（单测要重链接，删了要重编全部依赖）
#   - 保留 `target/debug/build`（build script 产物）
#   - **不碰 `.git` / `.qoder`**（worktree 里有未提交改动，不归本脚本管）
#
# 用法：
#   bash tools/clean_build.sh             # 默认：删掉 examples 目录（最省空间）
#   bash tools/clean_build.sh --keep-latest  # 温和：保留每个 example 最新一份
#                                             #   （还能秒跑，但占 ~8G）
#   bash tools/clean_build.sh --dry-run   # 只报要删什么，不真删
#   bash tools/clean_build.sh --all       # 连带 incremental、遗留 cargo-home 全清
#   bash tools/clean_build.sh --worktrees # 连带清 .qoder/worktrees/*/target
#                                          #   （⚠ **只删 target/**，绝不动
#                                          #     worktree 里的 git 内容与
#                                          #     未提交改动 ⇒ 默认不开）
set -u
cd "$(dirname "$0")/.." || exit 2

TARGET_DIR="target"
DRY_RUN=0
FULL=0
KEEP_LATEST=0
WORKTREES=0

for arg in "$@"; do
    case "$arg" in
        --dry-run|-n)  DRY_RUN=1 ;;
        --all)         FULL=1 ;;
        --keep-latest) KEEP_LATEST=1 ;;
        --worktrees)   WORKTREES=1 ;;
        -h|--help)
            sed -n '2,42p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "未知参数：$arg（--help 看用法）" >&2
            exit 2
            ;;
    esac
done

say() { printf '%s\n' "$*"; }

dir_bytes() {  # 目录占用（字节）
    [ -d "$1" ] || { echo 0; return; }
    du -sk "$1" 2>/dev/null | awk '{print $1 * 1024}'
}

human() {
    awk -v b="$1" 'BEGIN {
        if (b >= 1073741824) printf "%.1fG", b/1073741824;
        else if (b >= 1048576)  printf "%.1fM", b/1048576;
        else if (b >= 1024)     printf "%.0fK", b/1024;
        else print b"B";
    }'
}

TOTAL_FREED=0
DRY_PRINTED=0

remove() {  # 统一走这里，方便 --dry-run
    if [ "$DRY_RUN" = 1 ]; then
        DRY_PRINTED=$((DRY_PRINTED + 1))
        [ "$DRY_PRINTED" -le 20 ] && say "    [dry-run] 将删除：$1"
        return 0
    fi
    rm -rf "$1"
}

freed() {  # freed <目录> <之前字节>
    local a
    a=$(dir_bytes "$1")
    TOTAL_FREED=$((TOTAL_FREED + $2 - a))
    say "  ✔ $1：$(human "$2") → $(human "$a")"
}

# ------------------------------------------------------------------
# examples 目录：cargo 在这里的命名规矩
#   foo              ← "便捷副本"（硬链接到下面那个真身）
#   foo-<16位hash>   ← 真身（同一个 inode）
#   foo-<hash>.d     ← dep-info（可随时重新生成）
# 每次重编 ⇒ 新增一份新 hash 的真身 ⇒ 旧的留在那儿没人管。
# ------------------------------------------------------------------
# 判定「当前这一份」：inode 与某个**无哈希名**文件相同的那个。
# 其余独立 inode 的 ⇒ 历史副本 ⇒ 删。
# 所有 `.d` ⇒ 删（重新生成，无所谓）。
clean_examples_keep_latest() {
    local dir="$1"
    [ -d "$dir" ] || return 0
    local before keep ino n
    before=$(dir_bytes "$dir")

    keep=$(
        for f in "$dir"/*; do
            [ -e "$f" ] || continue
            local b
            b=$(basename "$f")
            case "$b" in
                *.d) continue ;;
                *-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) continue ;;
                *) stat -c '%i' "$f" 2>/dev/null ;;
            esac
        done | sort -u
    )
    [ -n "$keep" ] || return 0

    n=0
    for f in "$dir"/*; do
        [ -e "$f" ] || continue
        local b
        b=$(basename "$f")
        case "$b" in
            *.d) n=$((n + 1)); remove "$f"; continue ;;
            *-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
            *) continue ;;   # 无哈希名 ⇒ 当前这一份 ⇒ 留
        esac
        ino=$(stat -c '%i' "$f" 2>/dev/null) || continue
        printf '%s\n' "$keep" | grep -qx "$ino" && continue  # 是当前那一份的硬链接 ⇒ 留
        n=$((n + 1))
        remove "$f"
    done
    say "  ✔ $dir：删掉 $n 份历史副本 / dep-info"
    freed "$dir" "$before"
}

# ------------------------------------------------------------------
# incremental 缓存：只留最近 N 天的，其余删
#    （删了不影响正确性，只是下次编译慢一点）
# ------------------------------------------------------------------
clean_incremental() {
    local dir="$1" keep_days="${2:-1}"
    [ -d "$dir" ] || return 0
    local before n
    before=$(dir_bytes "$dir")
    n=0
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        n=$((n + 1))
        remove "$f"
    done < <(find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime "+$keep_days" 2>/dev/null)
    say "  ✔ $dir：删掉 $n 个超过 ${keep_days} 天的缓存目录"
    freed "$dir" "$before"
}

# ------------------------------------------------------------------
say "构建产物清理（target/ = $TARGET_DIR）"
[ "$DRY_RUN" = 1 ] && say "  （--dry-run：只报告，不删除）"

if [ "$KEEP_LATEST" = 1 ]; then
    say "【温和模式】保留每个 example 的最新一份（还能秒跑）"
    clean_examples_keep_latest "$TARGET_DIR/debug/examples"
    clean_examples_keep_latest "$TARGET_DIR/release/examples"
else
    say "【默认模式】删掉 examples 目录"
    say "  （守门用 cargo check --examples —— 它不产二进制，所以删了不影响 CI）"
    for d in "$TARGET_DIR/debug/examples" "$TARGET_DIR/release/examples"; do
        [ -d "$d" ] || continue
        b=$(dir_bytes "$d"); remove "$d"; freed "$d" "$b"
    done
fi

if [ "$FULL" = 1 ]; then
    say "【彻底模式】连 incremental 与遗留 cargo-home 一起清"
    for d in "$TARGET_DIR/debug/incremental" "$TARGET_DIR/release/incremental"; do
        [ -d "$d" ] || continue
        b=$(dir_bytes "$d"); remove "$d"; freed "$d" "$b"
    done
    # 遗留：2022 年某次把 CARGO_HOME 指到 target/cargo-home 的实验产物
    # （现在 cargo 用 ~/.cargo，这个目录是死的）
    if [ -d "$TARGET_DIR/cargo-home" ]; then
        b=$(dir_bytes "$TARGET_DIR/cargo-home"); remove "$TARGET_DIR/cargo-home"; freed "$TARGET_DIR/cargo-home" "$b"
    fi
else
    clean_incremental "$TARGET_DIR/debug/incremental" 1
fi

# ------------------------------------------------------------------
# 可选：清 agent worktree 里各自的 target/
# ⚠️ **只删 target/**（构建产物）⇒ **绝不动** worktree 的 git 内容、源码、
#    未提交改动。默认**不开** —— 用户明确说过「.qoder 的先不要删」。
# ------------------------------------------------------------------
if [ "$WORKTREES" = 1 ]; then
    say "【worktree 模式】只清 .qoder/worktrees/*/target（源码与未提交改动不动）"
    for d in .qoder/worktrees/*/; do
        [ -d "$d/target" ] || continue
        b=$(dir_bytes "$d/target"); remove "$d/target"; freed "$d/target" "$b"
    done
fi

say ""
say "合计释放：$(human "$TOTAL_FREED")"
say "现在 target/ = $(human "$(dir_bytes "$TARGET_DIR")")"
if [ "$DRY_RUN" != 1 ]; then
    say ""
    say "下一步：cargo check --examples 照跑（它不产二进制）；"
    say "        要跑某个 example 时 cargo 会自动重编那一个。"
fi
exit 0
