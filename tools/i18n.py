#!/usr/bin/env python3
"""英语支持的辅助工具（提取 / 翻译 / 生成 Rust 表 / 改写调用点）。

子命令：
    extract   扫 src/ 里的**中文字符串字面量**，产出清单 JSON
    translate 调本机配好的 LLM 批量翻译（带断点续跑）
    gen       从译文 JSON 生成 src/i18n_en.rs（排序好的静态数组）
    apply     把清单里的字面量改写成 tr("…") 调用

⚠️ **为什么扫描逻辑要跟 `src/lib.rs` 的 lint 一致**：
那边判「什么算界面文案」用的是同一套规则（跳注释 / 原始字符串 /
字符字面量 / CSS 块）。两边不一致 ⇒ lint 扫得到而这里扫不到（或反过来），
翻译表就会漏条目。所以这里的扫描器是照着 `scan_file` 写的。

用法：
    python3 tools/i18n.py extract  --out /tmp/i18n_catalog.json
    python3 tools/i18n.py translate --catalog /tmp/i18n_catalog.json \
                                    --out /tmp/i18n_en.json
    python3 tools/i18n.py gen      --translations /tmp/i18n_en.json \
                                    --out src/i18n_en.rs
    python3 tools/i18n.py apply    --catalog /tmp/i18n_catalog.json \
                                    --files src/topmenus.rs src/connmgr.rs
"""

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

CJK = re.compile(r"[一-鿿]")
PLACEHOLDER = re.compile(r"\{[^{}]*\}")

# ── 扫描器（照抄 src/lib.rs::scan_file 的规则）──────────────────────────

def strip_code(src):
    """返回 (代码片段列表, 字符串字面量列表[(行号, 原文)])。

    字符串字面量的原文保留**转义**（`\\n` 等），改写时按原文回填。
    """
    chars = list(src)
    i = 0
    n = len(chars)
    line = 1
    in_block = False
    lits = []          # (line, raw_text)
    marks = []         # (start_index, end_index, raw_text) —— 供 apply 精确回填
    while i < n:
        c = chars[i]
        if c == "\n":
            line += 1
            i += 1
            continue
        if in_block:
            if c == "*" and i + 1 < n and chars[i + 1] == "/":
                in_block = False
                i += 2
                continue
            i += 1
            continue
        if c == "/" and i + 1 < n and chars[i + 1] == "/":
            while i < n and chars[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and chars[i + 1] == "*":
            in_block = True
            i += 2
            continue
        if c == "'" and i + 2 < n and chars[i + 1] == '"' and chars[i + 2] == "'":
            i += 3
            continue
        if c == "r":  # 原始字符串 r"…" / r#"…"#
            j = i + 1
            hashes = 0
            while j < n and chars[j] == "#":
                hashes += 1
                j += 1
            if j < n and chars[j] == '"':
                k = j + 1
                ok = False
                while k < n:
                    if chars[k] == '"':
                        m = k + 1
                        h = 0
                        while m < n and chars[m] == "#" and h < hashes:
                            h += 1
                            m += 1
                        if h == hashes:
                            i = m
                            ok = True
                            break
                    k += 1
                if ok:
                    continue
        if c == '"':
            start = i
            start_line = line
            buf = []
            i += 1
            while i < n:
                d = chars[i]
                if d == "\\":
                    if i + 1 < n:
                        if chars[i + 1] == "\n":
                            line += 1
                        buf.append(chars[i])
                        buf.append(chars[i + 1])
                        i += 2
                        continue
                    buf.append(d)
                    i += 1
                    continue
                if d == '"':
                    i += 1
                    break
                if d == "\n":
                    line += 1
                buf.append(d)
                i += 1
            raw = "".join(buf)
            if not (len(raw) > 200 and looks_like_css(raw)):
                lits.append((start_line, raw))
                marks.append((start, i, raw))
            continue
        i += 1
    return lits, marks


def looks_like_css(s):
    """与 lint 的判据一致：有「选择器 + {」的行才算 CSS。"""
    for line in s.splitlines():
        t = line.strip()
        if not t or "{" not in t:
            continue
        head = t.split("{", 1)[0].strip()
        if head and all(ord(ch) < 128 for ch in head):
            return True
    return False


_ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\",
            '"': '"', "'": "'"}


_ESCAPES = {"n": "\n", "r": "\r", "t": "\t", "0": "\0", "\\": "\\",
            '"': '"', "'": "'"}


def unescape(raw):
    r"""把 Rust 字符串字面量原文（带转义）还原成实际内容。

    ⚠️ 不能用 `unicode_escape`：它按 latin-1 解释字节，中文会被拆成乱码。
    这里手工处理 Rust 的几种转义（含 \u{...} 形式）。
    """
    out = []
    i = 0
    n = len(raw)
    while i < n:
        c = raw[i]
        if c != "\\" or i + 1 >= n:
            out.append(c)
            i += 1
            continue
        nxt = raw[i + 1]
        if nxt == "u" and i + 2 < n and raw[i + 2] == "{":
            end = raw.find("}", i + 3)
            if end > 0:
                try:
                    out.append(chr(int(raw[i + 3:end], 16)))
                    i = end + 1
                    continue
                except ValueError:
                    pass
        if nxt in _ESCAPES:
            out.append(_ESCAPES[nxt])
            i += 2
            continue
        out.append(nxt)
        i += 2
    return "".join(out)


def escape(s):
    """把内容转成合法的 Rust 字符串字面量内容（不含引号）。"""
    out = []
    for c in s:
        if c == "\\":
            out.append("\\\\")
        elif c == '"':
            out.append('\\"')
        elif c == "\n":
            out.append("\\n")
        elif c == "\r":
            out.append("\\r")
        elif c == "\t":
            out.append("\\t")
        else:
            out.append(c)
    return "".join(out)


# ── extract ────────────────────────────────────────────────────────────

def cmd_extract(args):
    catalog = {}
    files = []
    for root, _dirs, names in os.walk("src"):
        for nm in sorted(names):
            if nm.endswith(".rs"):
                files.append(os.path.join(root, nm))
    files.sort()
    for path in files:
        src = open(path, encoding="utf-8").read()
        lits, _marks = strip_code(src)
        for line, raw in lits:
            text = unescape(raw)
            if not CJK.search(text):
                continue
            key = text
            entry = catalog.setdefault(key, {"files": [], "lines": []})
            entry["files"].append(path)
            entry["lines"].append(line)
    out = {
        "count": len(catalog),
        "entries": [
            {"zh": k, "files": sorted(set(v["files"])), "lines": v["lines"]}
            for k, v in sorted(catalog.items())
        ],
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
    print(f"提取到 {len(catalog)} 条含中文的字符串字面量 -> {args.out}")


# ── translate ──────────────────────────────────────────────────────────

def load_llm_config():
    path = os.path.expanduser("~/.minimax/config.yaml")
    base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    if not os.path.exists(path):
        raise SystemExit(f"找不到 {path}")
    text = open(path, encoding="utf-8").read()
    api = re.search(r"apiKey:\s*(\S+)", text)
    url = re.search(r"baseURL:\s*(\S+)", text)
    if not api or not url:
        raise SystemExit("config.yaml 里没有 apiKey / baseURL")
    return api.group(1).strip('"'), url.group(1).strip('"')


def call_llm(api_key, base_url, system, prompt, timeout=180):
    payload = {
        "model": "MiniMax-M3.1-Flash",
        "max_tokens": 16000,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": prompt},
        ],
    }
    req = urllib.request.Request(
        base_url.rstrip("/") + "/messages",
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = json.loads(resp.read().decode("utf-8"))
    parts = []
    for blk in data.get("content", []):
        if isinstance(blk, dict) and blk.get("type") == "text":
            parts.append(blk.get("text", ""))
    return "".join(parts)


SYSTEM = """你是 Linux 桌面软件（PuTTY 兼容的 SSH 终端 + SFTP + 数据库工作台）的本地化专家。
把给定的中文界面文案翻译成英文。要求：
1. 只输出 JSON 对象，键是原文（一个字都不能改），值是英文译文。
2. 风格：简洁、专业、像 GNOME/GTK 应用（Shortcuts, Cancel, Save, Connect…）。
3. 占位符必须**原样保留**（如 {host}、{0}、{}），不能增删改名。
4. 按钮词表统一：确定=OK，取消=Cancel，保存=Save，关闭=Close，删除=Delete，
   复制=Copy，粘贴=Paste，新建=New，编辑=Edit，重命名=Rename，刷新=Refresh，
   连接=Connect，断开=Disconnect，导入=Import，导出=Export，设置=Settings，
   打开=Open，文件=File，目录=Directory，终端=Terminal。
5. 术语一致：远程=Remote，本地=Local，数据库=Database，会话=Session，
   主机=Host，用户=User，密码=Password，私钥=Private Key，凭据库=Credential Vault，
   端口转发=Port Forwarding，传输队列=Transfer Queue。
6. 不要翻译 SQL、命令、路径、环境变量、占位符里的标识符。
7. 译文不要带引号以外的解释，不要 Markdown。"""


def cmd_translate(args):
    cat = json.load(open(args.catalog, encoding="utf-8"))
    entries = cat["entries"]
    if args.only_files:
        want = set(args.only_files)
        entries = [e for e in entries
                   if set(e["files"]) & want]
    todo = [e["zh"] for e in entries]
    done = {}
    if os.path.exists(args.out):
        done = json.load(open(args.out, encoding="utf-8"))
    todo = [t for t in todo if t not in done]
    print(f"待翻译 {len(todo)} 条（已完成 {len(done)}）")
    if not todo:
        return
    api_key, base_url = load_llm_config()
    batch = args.batch
    for idx in range(0, len(todo), batch):
        chunk = todo[idx:idx + batch]
        prompt = "翻译下面这些中文界面文案，返回 JSON 对象：\n" + \
            json.dumps(chunk, ensure_ascii=False)
        last_err = None
        for attempt in range(3):
            try:
                out = call_llm(api_key, base_url, SYSTEM, prompt)
                got = parse_json_object(out)
                ok = {k: v for k, v in got.items() if k in chunk and isinstance(v, str)}
                missing = [k for k in chunk if k not in ok]
                bad = [k for k, v in ok.items()
                       if sorted(PLACEHOLDER.findall(k)) != sorted(PLACEHOLDER.findall(v))]
                for k in missing + bad:
                    ok.pop(k, None)
                done.update(ok)
                print(f"[{idx + len(chunk)}/{len(todo)}] 本批 {len(ok)}/{len(chunk)} 条"
                      + (f"，失败 {len(missing) + len(bad)} 条" if missing or bad else ""))
                break
            except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, OSError) as e:
                last_err = e
                print(f"  第 {attempt + 1} 次失败：{e}；重试…")
                time.sleep(3 + attempt * 3)
        else:
            print(f"⚠️ 这批放弃：{last_err}")
        json.dump(done, open(args.out, "w", encoding="utf-8"),
                  ensure_ascii=False, indent=1)
    print(f"完成：{len(done)} 条 -> {args.out}")


def parse_json_object(text):
    text = text.strip()
    if text.startswith("```"):
        text = re.sub(r"^```[a-zA-Z]*\n", "", text)
        text = re.sub(r"\n```$", "", text)
    start = text.find("{")
    end = text.rfind("}")
    if start < 0 or end < 0:
        raise ValueError("返回里没有 JSON 对象")
    return json.loads(text[start:end + 1])


# ── gen（生成 Rust 表）────────────────────────────────────────────────

def cmd_gen(args):
    tr = json.load(open(args.translations, encoding="utf-8"))
    lines = []
    lines.append("//! 英语翻译表（**自动生成，别手改**）。")
    lines.append("//!")
    lines.append("//! 由 `tools/i18n.py gen` 从译文 JSON 生成；键是**中文原文**，")
    lines.append("//! 值是英文译文。查表用二分（键已排序）。")
    lines.append("//!")
    lines.append("//! 重新生成：")
    lines.append("//! ```bash")
    lines.append("//! python3 tools/i18n.py extract --out /tmp/i18n_catalog.json")
    lines.append("//! python3 tools/i18n.py translate --catalog /tmp/i18n_catalog.json \\")
    lines.append("//!         --out /tmp/i18n_en.json")
    lines.append("//! python3 tools/i18n.py gen --translations /tmp/i18n_en.json \\")
    lines.append("//!         --out src/i18n_en.rs")
    lines.append("//! ```")
    lines.append("")
    lines.append("/// （中文原文, 英文译文）—— 按中文原文排序，供二分查找。")
    lines.append(f"pub static EN: [(&str, &str); {len(tr)}] = [")
    for k in sorted(tr):
        v = tr[k]
        lines.append(f'    ({escape(k)}, {escape(v)}),')
    lines.append("];")
    with open(args.out, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"生成 {len(tr)} 条译文 -> {args.out}")


# ── apply（把字面量改写成 tr("…") / trf!("…")）────────────────────────

def const_table_ranges(src):
    """`const` / `static` 的**表体**区间（里面的字面量不能包 tr()）。

    ⚠️ `const` / `static` 的初始值必须**编译期可求值** ⇒ 不能在里面调
    `tr()`（它是函数）。这类表（菜单表 `TOP_MENUS`、设置注册表 `SETTINGS`、
    快捷键表 `ALL`…）里的中文是**数据**，要翻的话在**显示处**翻
    （`tr(item.label)`）。
    """
    ranges = []
    for mt in re.finditer(r"\b(const|static)\s+[A-Z_0-9]+\s*:[^=]*=\s*&?\[", src):
        i = mt.end()
        depth = 1
        j = i
        while j < len(src) and depth:
            if src[j] == "[":
                depth += 1
            elif src[j] == "]":
                depth -= 1
            j += 1
        ranges.append((i, j))
    return ranges


def test_cutoff(src):
    """`#[cfg(test)]` 之后的内容是测试代码 —— **不翻译**。

    ⚠️ 测试里有大量**中文断言**（`assert!(x.contains("忽略未知参数"))`），
    翻译了源码里的字面量却不翻断言 ⇒ 测试必挂；而断言本来就该用中文
    （默认语言就是中文，测试运行时 `tr()` 返回中文原文）。
    """
    i = src.find("#[cfg(test)]")
    return len(src) if i < 0 else i


def find_format_calls(src, limit):
    """找出 `format!(...)` 调用。

    返回 [(call_start, call_end, tmpl_start, tmpl_end, args_text)]。
    """
    out = []
    for mt in re.finditer(r"\bformat!\(", src):
        if mt.start() >= limit:
            break
        i = mt.end()
        # 跳过空白
        while i < len(src) and src[i].isspace():
            i += 1
        if i >= len(src) or src[i] != '"':
            continue  # format!(变量, …) —— 模板不是字面量，跳过
        t_start = i
        i += 1
        while i < len(src):
            if src[i] == "\\":
                i += 2
                continue
            if src[i] == '"':
                break
            i += 1
        t_end = i + 1
        # 找到配对的右括号
        depth = 1
        j = t_end
        while j < len(src) and depth:
            c = src[j]
            if c == '"':
                j += 1
                while j < len(src) and src[j] != '"':
                    if src[j] == "\\":
                        j += 1
                    j += 1
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
            j += 1
        args_text = src[t_end:j - 1]
        out.append((mt.start(), j, t_start, t_end, args_text))
    return out


def split_top_args(text):
    """按顶层逗号切分参数（忽略括号/方括号/花括号/字符串里的逗号）。"""
    args = []
    depth = 0
    buf = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == '"':
            buf.append(c)
            i += 1
            while i < n:
                buf.append(text[i])
                if text[i] == "\\":
                    i += 1
                    if i < n:
                        buf.append(text[i])
                    i += 1
                    continue
                if text[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == "," and depth == 0:
            args.append("".join(buf).strip())
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    tail = "".join(buf).strip()
    if tail:
        args.append(tail)
    return [a for a in args if a]


NAMED_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$", re.S)
IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
PLACEHOLDER_RE = re.compile(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")


def transform_format(tmpl_raw, args_text):
    """把 `format!("模板", args…)` 变成 `trf!("模板", k = v, …)`。

    - 模板里的 `{}` 位置占位符改成 `{v0}` `{v1}`…（**位置参数按序对应**）；
    - 模板里的 `{name}` 具名占位符原样保留；
    - 隐式捕获（`format!("{x}", x)`）改写成显式 `x = x`；
    - 位置参数改写成 `vN = 表达式`。

    返回 (新模板, 新参数列表) 或 None（不该动）。
    """
    tmpl = unescape(tmpl_raw)
    if not CJK.search(tmpl):
        return None
    names = set(PLACEHOLDER_RE.findall(tmpl))
    args = split_top_args(args_text)

    named = []       # (占位符名, 表达式)
    positional = []  # 表达式（位置）
    implicit = []    # 模板里有、但参数里没给的具名占位符（隐式捕获）
    seen_named = set()
    for a in args:
        m = NAMED_RE.match(a)
        if m:
            key, expr = m.group(1), m.group(2).strip()
            named.append((key, expr))
            seen_named.add(key)
            continue
        if IDENT_RE.match(a) and a in names:
            named.append((a, a))       # 隐式捕获 → 显式
            seen_named.add(a)
            continue
        positional.append(a)

    # 模板里出现、但没人给的具名占位符 ⇒ 隐式捕获（变量在作用域里）
    for nm in sorted(names - seen_named):
        named.append((nm, nm))

    if len(positional) != tmpl.count("{}"):
        # 对不上（不该发生）⇒ 放弃这一条，交给人看
        return None

    # `{}` → `{vN}`
    counter = {"i": 0}

    def repl(_m):
        i = counter["i"]
        counter["i"] += 1
        return "{v%d}" % i

    new_tmpl = re.sub(r"\{\}", repl, tmpl)
    for i, expr in enumerate(positional):
        named.append(("v%d" % i, expr))
    # 具名在前、位置在后（Rust 要求位置参数在具名参数之前 ⇒ 反过来才合法）
    named.sort(key=lambda kv: kv[0].startswith("v") and kv[0][1:].isdigit())
    return new_tmpl, named


def cmd_apply(args):
    total = 0
    for path in args.files:
        src = open(path, encoding="utf-8").read()
        limit = test_cutoff(src)
        calls = find_format_calls(src, limit)
        edits = []       # (start, end, 新文本)
        consumed = []    # 已经被 format! 改写吃掉区间

        for (cs, ce, ts, te, args_text) in calls:
            tmpl_raw = src[ts + 1:te - 1]
            res = transform_format(tmpl_raw, args_text)
            if res is None:
                continue
            new_tmpl, named = res
            arg_src = ", ".join("%s = %s" % (k, v) for k, v in named)
            new_text = 'trf!("%s", %s)' % (escape(new_tmpl), arg_src)
            edits.append((cs, ce, new_text))
            consumed.append((ts, te))

        # 剩下的中文字面量 → tr("…")（跳过 const/static 表体）
        protected = const_table_ranges(src)
        lits, marks = strip_code(src[:limit])
        # marks 的下标是相对 src[:limit] 的 ⇒ 补回偏移
        off = 0
        for (start, end, raw) in marks:
            text = unescape(raw)
            if not CJK.search(text):
                continue
            if any(start >= a and end <= b for a, b in consumed):
                continue
            if any(start >= a and end <= b for a, b in protected):
                continue  # const/static 表体：显示处再翻
            before = src[:start]
            if before.rstrip().endswith("tr(") or before.rstrip().endswith("trf!("):
                continue
            edits.append((start, end, 'tr("%s")' % raw))

        if not edits:
            continue
        out = src
        for start, end, text in sorted(edits, key=lambda e: -e[0]):
            out = out[:start] + text + out[end:]
        # 补 import（`use` 语句块之后）
        if "use crate::i18n::tr;" not in out:
            uses = list(re.finditer(r"^use [^;]*;", out, re.M | re.S))
            if uses:
                at = uses[-1].end()
            else:
                # 没有 use 语句的纯数据模块 ⇒ 插到模块文档（//!）之后
                doc = list(re.finditer(r"^//!.*$", out, re.M))
                at = doc[-1].end() if doc else 0
            out = out[:at] + "\nuse crate::i18n::tr;\nuse crate::trf;" + out[at:]
        open(path, "w", encoding="utf-8").write(out)
        total += len(edits)
        print(f"{path}: 改写 {len(edits)} 处")
    print(f"合计 {total} 处")


def main(argv):
    ap = argparse.ArgumentParser(description="英语支持辅助工具")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("extract")
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_extract)

    p = sub.add_parser("translate")
    p.add_argument("--catalog", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--batch", type=int, default=40)
    p.add_argument("--only-files", nargs="*", default=None)
    p.set_defaults(func=cmd_translate)

    p = sub.add_parser("gen")
    p.add_argument("--translations", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(func=cmd_gen)

    p = sub.add_parser("apply")
    p.add_argument("--catalog", required=True)
    p.add_argument("--files", nargs="+", required=True)
    p.set_defaults(func=cmd_apply)

    args = ap.parse_args(argv)
    args.func(args)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
