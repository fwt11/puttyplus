#!/usr/bin/env python3
"""版本号一致性检查（发版链路的护栏）。

**为什么要有这个脚本**：v0.2.0 那次发版出了三处版本号打架——

| 地方 | 本该写 | 实际写的 |
|---|---|---|
| `Cargo.toml` 的 `[package] version` | 0.2.0 | 0.1.0（从 v0.1.0 起就没动过） |
| `debian/changelog` 顶条（**deb 包版本来自这里**） | 0.2.0-1 | 0.1.0-1 |
| `release.yml` 里的发布说明正文 | 0.2.0 | 硬编码的 0.1.0 |

后果：打上 `v0.2.0` 的 tag，产出的 deb 却叫 `puttyplus_0.1.0-1_amd64.deb`，
Release 说明也写着「PuTTYPlus 0.1.0」——用户装上后「关于」里显示的
还是 0.1.0，且两个 Release 的 deb **同名**。

⇒ 根因是**版本号有三个来源且互不校验**。这个脚本把它们钉在一起：

  1. `Cargo.toml` 的 `[package] version`（程序「关于」框显示的就是它）
  2. `debian/changelog` 顶条的版本（**deb 包名与包版本来自这里**）
  3. 发版时的 git tag（`vX.Y.Z`）

用法：
    python3 tools/check_version.py           # 查 1 ⟷ 2
    python3 tools/check_version.py v0.3.0    # 再跟 tag 比（带不带 v 前缀都行）
    python3 tools/check_version.py --print   # 只打印 Cargo.toml 的版本号

退出码：0 = 一致；1 = 不一致（并打印差在哪）。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def cargo_version() -> str:
    """`Cargo.toml` 的 `[package] version`。

    只认 `[package]` 段里的键——依赖项写成 `russh = { version = "0.63" }`，
    全文正则扫会把那些版本号也捞进来。
    """
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = ""
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line
            continue
        if section != "[package]":
            continue
        m = re.match(r'version\s*=\s*"([^"]+)"', line)
        if m:
            return m.group(1)
    raise SystemExit("Cargo.toml 里找不到 [package] version")


def changelog_version() -> str:
    """`debian/changelog` 顶条的版本（去掉 debian revision）。

    条目形如 `puttyplus (0.3.0-1) unstable; urgency=medium`：
    `0.3.0` 是上游版本，`1` 是 debian revision（打包序号）——
    **deb 的包版本（以及包名）用的是整串 `0.3.0-1`**。
    """
    text = (ROOT / "debian" / "changelog").read_text(encoding="utf-8")
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        m = re.match(r"^\S+\s+\(([^)]+)\)", line)
        if not m:
            raise SystemExit(f"debian/changelog 顶条看不懂：{line!r}")
        full = m.group(1)
        # 去掉 epoch（1:0.3.0-1）与 debian revision（0.3.0-1 → 0.3.0）
        ver = full.split(":")[-1]
        return ver.rsplit("-", 1)[0] if "-" in ver else ver
    raise SystemExit("debian/changelog 是空的")


def normalize(tag: str) -> str:
    """tag（`v0.3.0`）与版本号（`0.3.0`）归一成同一种写法。"""
    return tag.strip().lstrip("vV")


def main(argv: list[str]) -> int:
    args = [a for a in argv[1:] if a != "--print"]
    if "--print" in argv[1:]:
        print(cargo_version())
        return 0

    cargo = cargo_version()
    changelog = changelog_version()
    tag = normalize(args[0]) if args else None

    problems: list[str] = []
    if cargo != changelog:
        problems.append(f"Cargo.toml 的版本是 {cargo}，debian/changelog 的是 {changelog}")
    if tag is not None and tag != cargo:
        problems.append(f"tag 是 {args[0]}（即 {tag}），Cargo.toml 的是 {cargo}")

    if problems:
        print("版本号对不上：", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        print(
            "\n⇒ 发版前把三个地方改成同一个版本号：\n"
            "  1. Cargo.toml 的 [package] version\n"
            "  2. debian/changelog 顶条加一条新版本\n"
            "  3. git tag 用 v<同一个版本号>",
            file=sys.stderr,
        )
        return 1

    where = "Cargo.toml ⟷ debian/changelog"
    if tag is not None:
        where += " ⟷ tag"
    print(f"版本号一致（{where}）：{cargo}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
