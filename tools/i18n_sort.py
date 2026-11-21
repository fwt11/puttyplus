#!/usr/bin/env python3
"""把 src/i18n_en.rs 的静态表按**解码后**的中文原文重排。

⚠️ **为什么不能按源码里的字面文本排**（阶段 142 踩过）：
Rust 的 `sort_unstable()` 比的是**解码后**的字符串（UTF-8 字节序）；
而源码里 `\r`、`\n` 是**两个字符**（反斜杠 + 字母）⇒
按源码文本排会把 `"\r\nx1b…"` 排到 `"["` 之后 ⇒ **二分查找失效**
⇒ 明明有译文，运行时却显示中文（i18n 的两条测试会红）。

用法：python3 tools/i18n_sort.py
"""

import re
import sys

PATH = "src/i18n_en.rs"

ENTRY = re.compile(r'^\s*\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*\)\s*,?\s*$')


def unescape(s: str) -> str:
    """把 Rust 字符串字面量解码成真正的字符串。"""
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        i += 1
        n = s[i]
        if n == "n":
            out.append("\n")
        elif n == "r":
            out.append("\r")
        elif n == "t":
            out.append("\t")
        elif n == "0":
            out.append("\0")
        elif n == "\\":
            out.append("\\")
        elif n == '"':
            out.append('"')
        elif n == "'":
            out.append("'")
        elif n == "x":
            out.append(chr(int(s[i + 1 : i + 3], 16)))
            i += 2
        elif n == "u":
            # \u{XXXX}
            j = s.index("}", i)
            out.append(chr(int(s[i + 2 : j], 16)))
            i = j
        else:
            out.append(n)
        i += 1
    return "".join(out)


def rust_quote(s: str) -> str:
    """按 **Rust 字符串字面量**的写法加引号（不是 Python 的 repr）。"""
    out = ['"']
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif ch == "\t":
            out.append("\\t")
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            out.append(f"\\u{{{ord(ch):x}}}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def main() -> int:
    lines = open(PATH, encoding="utf-8").read().split("\n")
    start = next(i for i, l in enumerate(lines) if l.startswith("pub static EN:"))
    end = next(i for i, l in enumerate(lines[start:], start) if l.strip() == "];")
    body = lines[start + 1 : end]

    entries = []
    for l in body:
        m = ENTRY.match(l)
        if m:
            entries.append((unescape(m.group(1)), unescape(m.group(2))))
        elif l.strip():
            print(f"看不懂的一行（第 {start + 2 + len(entries)} 行附近）：{l!r}", file=sys.stderr)
            return 1

    keys = [k for k, _ in entries]
    dup = [k for k in set(keys) if keys.count(k) > 1]
    if dup:
        # ⚠️ 重复的 key 会让**二分查找**漏掉一条 ⇒ 保留**先出现的**那条
        #    （表是排好序的 ⇒ 先出现的就是排在前面的那条）。
        print(f"表里有 {len(dup)} 个重复 key，去重（保留先出现的）：{dup[:5]}")
        seen = set()
        uniq = []
        for k, v in entries:
            if k in seen:
                continue
            seen.add(k)
            uniq.append((k, v))
        entries = uniq

    entries.sort(key=lambda kv: kv[0].encode("utf-8"))
    new_body = [f"    ({rust_quote(k)}, {rust_quote(v)})," for k, v in entries]
    out = lines[: start + 1] + new_body + lines[end:]
    text = "\n".join(out)
    text = re.sub(
        r"pub static EN: \[\(&str, &str\); \d+\] = \[",
        "pub static EN: [(&str, &str); %d] = [" % len(entries),
        text,
        count=1,
    )
    open(PATH, "w", encoding="utf-8").write(text)
    print(f"重排完成：{len(entries)} 条")
    return 0


if __name__ == "__main__":
    sys.exit(main())
