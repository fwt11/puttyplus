#!/usr/bin/env python3
"""把 src/i18n_en.rs 的现有译文解析成 JSON，再合并新译文。

用法：python3 tools/i18n_merge.py <新译文 JSON> [输出 JSON]

新译文 JSON 形如 {"中文原文": "English"}；
**特殊键 "__remove__"** 放一个中文原文列表 ⇒ 把这些**已失效**的
译文从表里删掉（文案改了 ⇒ 旧 key 没人用了 ⇒ 留着是垃圾）。
"""
import ast
import json
import re
import sys

EN_RS = "src/i18n_en.rs"
REMOVE_KEY = "__remove__"


def parse_en_rs(path):
    """从生成的 i18n_en.rs 里解析出 {中文: 英文}。

    ⚠️ 用 ast.literal_eval 逐行解析元组，**不要手写正则剥引号**
    （译文里有 \" 和 \\ 等转义）。
    """
    out = {}
    pat = re.compile(r'^\s*\("((?:[^"\\]|\\.)*)",\s*"((?:[^"\\]|\\.)*)"\),\s*$')
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = pat.match(line)
            if not m:
                continue
            zh = ast.literal_eval('"' + m.group(1) + '"')
            en = ast.literal_eval('"' + m.group(2) + '"')
            out[zh] = en
    return out


def main():
    new_path = sys.argv[1]
    out_path = sys.argv[2] if len(sys.argv) > 2 else "/tmp/i18n_en.json"
    table = parse_en_rs(EN_RS)
    print(f"解析到 {len(table)} 条现有译文")
    new = json.load(open(new_path, encoding="utf-8"))
    for zh in new.pop(REMOVE_KEY, []):
        if zh in table:
            del table[zh]
            print(f"  删除：{zh!r}")
        else:
            print(f"  ⚠️ 要删的 key 不在表里：{zh!r}")
    added = 0
    for zh, en in new.items():
        if zh in table:
            if table[zh] != en:
                print(f"  更新：{zh!r}")
                table[zh] = en
            continue
        added += 1
        print(f"  新增：{zh!r} -> {en!r}")
        table[zh] = en
    json.dump(table, open(out_path, "w", encoding="utf-8"),
              ensure_ascii=False, indent=1)
    print(f"合并后 {len(table)} 条（新增 {added} 条）-> {out_path}")


if __name__ == "__main__":
    main()
