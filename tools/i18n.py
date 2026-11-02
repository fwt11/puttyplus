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

# 中文**与全角标点**。
#
# ⚠️ 光认汉字不够：「，」「、」「：」这些**全角标点**也得跟着翻
#（英文里要写成 ", " / ", " / ": "），不然中英混排看着很别扭。
# ⇒ 把 U+3000-303F（��、。、「」…）与 U+FF00-FFEF（，：（））都算进来。
CJK = re.compile(r"[\u4e00-\u9fff\u3000-\u303f\uff00-\uffef]")
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
        if nxt == "\n":
            # Rust 的**行续接**：反斜杠 + 换行 + 下一行的缩进**整段去掉**
            # （源码里为了排版折行用的，运行时字符串里没有）。
            # ⚠️ 不处理它 ⇒ key 里会多出一截换行和缩进 ⇒ 查表查不到。
            i += 2
            while i < n and raw[i] in " \t":
                i += 1
            continue
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
        _lits, marks = strip_code(src)
        t_ranges = test_ranges(src)
        for start, end, raw in marks:
            if in_ranges(start, end, t_ranges):
                # 测试代码里的中文（断言、夹具）**不是界面文案** ⇒ 不进词条
                continue
            text = unescape(raw)
            if not CJK.search(text):
                continue
            key = text
            entry = catalog.setdefault(key, {"files": [], "lines": []})
            entry["files"].append(path)
            entry["lines"].append(src.count("\n", 0, start) + 1)
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

def test_cutoff(src):
    """`#[cfg(test)]` 之后的内容是测试代码 —— **不翻译**。

    ⚠️ 测试里有大量**中文断言**（`assert!(x.contains("忽略未知参数"))`），
    翻译了源码里的字面量却不翻断言 ⇒ 测试必挂；而断言本来就该用中文
    （默认语言就是中文，测试运行时 `tr()` 返回中文原文）。
    """
    i = src.find("#[cfg(test)]")
    return len(src) if i < 0 else i


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


def find_format_calls(src, limit):
    """找出 `format!(...)` 调用。

    返回 [(call_start, call_end, tmpl_start, tmpl_end, args_text)]。
    """
    out = []
    for mt in re.finditer(r"\bformat!\(", src):
        if mt.start() >= limit:
            break
        i = mt.end()
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
                j += 1  # 跳过闭引号（见上面那句注释）
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

    返回 (新模板, 参数对列表) 或 None（不该动）。
    """
    tmpl = unescape(tmpl_raw)
    if not CJK.search(tmpl):
        return None
    names = set(PLACEHOLDER_RE.findall(tmpl))
    args = split_top_args(args_text)

    named = []
    positional = []
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

    # 模板里出现、但参数里没给的具名占位符 ⇒ 隐式捕获（变量在作用域里）
    for nm in sorted(names - seen_named):
        named.append((nm, nm))

    if len(positional) != tmpl.count("{}"):
        return None  # 对不上 ⇒ 交给人看，不自动改

    counter = {"i": 0}

    def repl(_m):
        i = counter["i"]
        counter["i"] += 1
        return "{v%d}" % i

    new_tmpl = re.sub(r"\{\}", repl, tmpl)
    for i, expr in enumerate(positional):
        named.append(("v%d" % i, expr))
    # 位置参数在前、具名参数在后（Rust 要求位置参数不能跟在具名参数后面）
    named.sort(key=lambda kv: kv[0].startswith("v") and kv[0][1:].isdigit())
    return new_tmpl, named



# ── 词法扫描（tokenizer）──────────────────────────────────────────────
#
# ⚠️ **为什么不用「正则找 format! + 括号计数」**：
# 括号计数一旦遇上**字符串里的括号**或**嵌套的 format!**，就会数错 ⇒
# 取到的「参数区间」会**吞掉半个文件**（阶段：改写 quickcmd.rs 时整个
# 文件的尾部被当成 format! 的参数，源码直接编译不过）。
#
# 现在的做法：先把源码**切成 token**（字符串 / 标识符 / 标点 / 注释…），
# 字符串是**原子**的 ⇒ 数括号时不会数到字符串里的括号 ⇒ 区间永远准。
# 改写时也是**按 token 重组**，不用「按字符偏移替换」⇒ 不会出现
# 「内层替换把外层区间顶歪」的经典事故。

class Tok:
    __slots__ = ("kind", "text", "start", "end")

    def __init__(self, kind, text, start, end):
        self.kind = kind
        self.text = text
        self.start = start
        self.end = end


def tokenize(src):
    """把源码切成 token 列表。

    kind: ws / ident / num / punct / str / raw / char / comment
    - `str` 是普通字符串字面量（**含引号**）
    - `raw` 是原始字符串 `r"…"`，整段当一个 token（主题 CSS 在里面）
    - `char` 是字符字面量（`'x'`）；**生命周期** `'a` 归到 punct
    """
    toks = []
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if c in " \t\r\n":
            j = i
            while j < n and src[j] in " \t\r\n":
                j += 1
            toks.append(Tok("ws", src[i:j], i, j))
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            j = n if j < 0 else j
            toks.append(Tok("comment", src[i:j], i, j))
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2)
            j = n if j < 0 else j + 2
            toks.append(Tok("comment", src[i:j], i, j))
            i = j
            continue
        # 原始字符串 r"…" / r#"…"#
        if c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                k = j + 1
                closed = False
                while k < n:
                    if src[k] == '"':
                        m = k + 1
                        h = 0
                        while m < n and src[m] == "#" and h < hashes:
                            h += 1
                            m += 1
                        if h == hashes:
                            toks.append(Tok("raw", src[i:m], i, m))
                            i = m
                            closed = True
                            break
                    k += 1
                if closed:
                    continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
            toks.append(Tok("str", src[i:j], i, j))
            i = j
            continue
        if c == "'":
            if i + 1 < n and src[i + 1] == "\\":
                j = i + 2
                while j < n and src[j] != "'":
                    j += 1
                j = min(j + 1, n)
                toks.append(Tok("char", src[i:j], i, j))
                i = j
                continue
            if i + 2 < n and src[i + 2] == "'":
                toks.append(Tok("char", src[i:i + 3], i, i + 3))
                i += 3
                continue
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            toks.append(Tok("punct", src[i:j], i, j))
            i = j
            continue
        if c.isalpha() or c == "_":
            j = i
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            toks.append(Tok("ident", src[i:j], i, j))
            i = j
            continue
        if c.isdigit():
            j = i
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            toks.append(Tok("num", src[i:j], i, j))
            i = j
            continue
        toks.append(Tok("punct", c, i, i + 1))
        i += 1
    return toks


def match_paren(toks, open_idx):
    """`toks[open_idx]` 是 `(` ⇒ 返回**配对的 `)`** 的下标（找不到返回 None）。

    字符串 / 原始字符串 / 字符字面量都是**原子 token** ⇒ 里面的括号不计。
    """
    depth = 0
    for j in range(open_idx, len(toks)):
        t = toks[j]
        if t.kind != "punct":
            continue
        if t.text == "(":
            depth += 1
        elif t.text == ")":
            depth -= 1
            if depth == 0:
                return j
    return None


def split_tok_args(toks):
    """按**顶层逗号**切开 token 序列（括号/方括号/花括号里的逗号不算）。"""
    args = []
    cur = []
    depth = 0
    for t in toks:
        if t.kind == "punct":
            if t.text in "([{":
                depth += 1
            elif t.text in ")]}":
                depth -= 1
            elif t.text == "," and depth == 0:
                args.append(cur)
                cur = []
                continue
        cur.append(t)
    if cur:
        args.append(cur)
    # 去掉纯空白
    return [a for a in args if any(t.kind != "ws" for t in a)]


def toks_text(toks):
    return "".join(t.text for t in toks)


def toks_strip(toks):
    a = 0
    b = len(toks)
    while a < b and toks[a].kind == "ws":
        a += 1
    while b > a and toks[b - 1].kind == "ws":
        b -= 1
    return toks[a:b]


# ── format! → trf! ────────────────────────────────────────────────────

PH_RE = re.compile(r"\{([^{}]*)\}")
IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
# ⚠️ 模板里带**格式说明**（`{x:.2}`）或**转义花括号**（`{{`）的，
#    我们的 `trf!` 只做「按名字整体替换」⇒ 处理不了 ⇒ 整个调用原样保留。
BAIL_RE = re.compile(r"\{\{|\}\}|\{[^{}]*:[^{}]*\}")

# Rust 的关键字 —— 不能拿来当占位符名字（`self = self` 语法错误）。
RUST_KEYWORDS = {
    "as", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop",
    "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "async", "await", "abstract", "become", "box", "do",
    "final", "macro", "override", "priv", "typeof", "unsized", "virtual",
    "yield", "try", "union",
}


def parse_ph(text):
    """把模板里的占位符解析成 [(kind, name)]。

    kind: 'pos'（`{}` / `{0}`）或 'named'（`{x}`）
    """
    out = []
    for m in PH_RE.finditer(text):
        inner = m.group(1).strip()
        if inner == "":
            out.append(("pos", None))
        elif inner.isdigit():
            out.append(("pos", int(inner)))
        elif IDENT_RE.match(inner):
            out.append(("named", inner))
        else:
            return None            # 认不出来的占位符 ⇒ 交给人看
    return out


def split_tok_arg_ranges(toks, lo, hi):
    """按**顶层逗号**切分 `toks[lo:hi]`，返回 [(起, 止)]（已去空白）。"""
    ranges = []
    depth = 0
    start = lo
    for j in range(lo, hi):
        t = toks[j]
        if t.kind == "punct":
            if t.text in "([{":
                depth += 1
            elif t.text in ")]}":
                depth -= 1
            elif t.text == "," and depth == 0:
                ranges.append((start, j))
                start = j + 1
                continue
    if start < hi:
        ranges.append((start, hi))
    out = []
    for x, y in ranges:
        while x < y and toks[x].kind == "ws":
            x += 1
        while y > x and toks[y - 1].kind == "ws":
            y -= 1
        if x < y:
            out.append((x, y))
    return out


def expr_key(toks, expr):
    """表达式的「身份」—— 用来判断两个占位符是不是同一个值。"""
    if expr[0] == "ident":
        return expr[1]
    return toks_text(toks[expr[1]:expr[2]])


# **不能翻译**的文件。
#
# ⚠️ 这两个文件的输出是**对外契约**，翻译了就跟 C 版对不上：
# - `src/args.rs`：命令行解析的告警（「忽略未知参数: X」…）是 AGENTS.md
#   里写死的**命令行契约**（堡垒机那边会看到）；
# - `src/backend/selftest.rs`：`--sftp-selftest` 的**全流程输出**要跟
#   C 版**逐字对照**（`tools/compat_check.sh` 第 4 节）。
NO_TRANSLATE_FILES = {"src/args.rs", "src/backend/selftest.rs"}

# 要求**字面量格式串**的宏（`format!` 之外的那些）。
#
# ⚠️ `println!` / `eprintln!` / `panic!` 的格式串**必须是字面量** ⇒
# 不能写成 `println!(tr("…"))`（编译不过：format argument must be a
# string literal）。⇒ 改成 `println!("{}", tr("…"))` / `println!("{}", trf!(…))`。
LITERAL_FMT_MACROS = {"println", "eprintln", "print", "eprint", "panic"}

# `menu::` 下面那些**收菜单文字**的函数：文字在 `menu::row` 里**统一翻译**
# ⇒ 调用点的字面量**不能**再包 `tr()`（会翻两遍），而且
# `sidebar.rs` 那种 `match label { "连接" => … }` 的**数据比较**
# 必须留在中文上。
MENU_LABEL_ARG = {"item": 1, "toggle": 1, "row": 1, "section": 0}


def build_trf(toks, tmpl_idx, end_idx, name_overrides, recurse, protected, stats, macro=None):
    """把一个 `format!("模板", args…)` 变成 `trf!("模板", k = v, …)`。

    `macro` 不为空时（`println!` 之类）改成 `MACRO!("{}", trf!(…))`。

    - 模板里的每个占位符都改成 `{名字}`（名字尽量取自参数本身）；
    - 参数里的**嵌套 format!** 与**中文字符串**会被 `recurse` 递归改写
      （⚠️ 不递归的话，参数里的中文永远翻不了）；
    - 返回新代码文本；处理不了返回 None。
    """
    raw = toks[tmpl_idx].text              # 含引号
    content = raw[1:-1]
    tmpl_text = unescape(content)
    if not CJK.search(tmpl_text):
        return None
    if BAIL_RE.search(content):
        return None
    phs = parse_ph(tmpl_text)
    if phs is None:
        return None

    # ── 参数：拆成「具名」与「位置」两类 ──
    named = {}          # 名字 -> ('ident', 名字) 或 ('range', 起, 止)
    positional = []
    for (x, y) in split_tok_arg_ranges(toks, tmpl_idx + 1, end_idx):
        if (y - x >= 3 and toks[x].kind == "ident" and toks[x + 1].kind == "punct"
                and toks[x + 1].text == "="):
            named[toks[x].text] = ("range", x + 2, y)
        else:
            positional.append(("range", x, y))

    # ── 给每个占位符定一个名字 ──
    assigned = {}       # 名字 -> 表达式身份
    names = [None] * len(phs)
    exprs = {}          # 名字 -> 表达式

    def claim(cand, expr):
        """给表达式占一个名字；撞名了就另起一个。"""
        key = expr_key(toks, expr)
        if cand not in assigned:
            assigned[cand] = key
            exprs[cand] = expr
            return cand
        if assigned[cand] == key:
            exprs.setdefault(cand, expr)
            return cand
        k = 2
        while "%s_%d" % (cand, k) in assigned:
            k += 1
        new = "%s_%d" % (cand, k)
        assigned[new] = key
        exprs[new] = expr
        return new

    # 具名占位符先占名字（它的名字写在模板里，不能改）
    for i, (kind, nm) in enumerate(phs):
        if kind != "named":
            continue
        expr = named.get(nm, ("ident", nm))
        name = nm if nm not in RUST_KEYWORDS else "k_" + nm
        if name in assigned and assigned[name] != expr_key(toks, expr):
            name = claim("k_" + nm, expr)
        else:
            assigned.setdefault(name, expr_key(toks, expr))
            exprs.setdefault(name, expr)
        names[i] = name

    # 位置占位符
    auto = 0
    for i, (kind, nm) in enumerate(phs):
        if kind != "pos":
            continue
        if nm is None:
            idx = auto
            auto += 1
        else:
            idx = nm
        if idx >= len(positional):
            return None            # 占位符比参数多 ⇒ 认不出来
        expr = positional[idx]
        if expr[0] == "range":
            e = toks[expr[1]:expr[2]]
            cand = e[0].text if (len(e) == 1 and e[0].kind == "ident") else "v%d" % idx
        else:
            cand = expr[1]
        if cand in RUST_KEYWORDS:
            cand = "k_" + cand
        names[i] = claim(cand, expr)

    if not exprs:
        # 没有占位符 ⇒ 普通文案
        if macro:
            return '%s!("{}", tr(%s))' % (macro, raw)
        return "tr(%s)" % raw

    # 人工指定的友好名
    ov = name_overrides.get(tmpl_text, {})
    if ov:
        for old, new in ov.items():
            if old in exprs and new not in exprs and new not in RUST_KEYWORDS:
                exprs[new] = exprs.pop(old)
                for i in range(len(names)):
                    if names[i] == old:
                        names[i] = new

    # 模板里的占位符统一改写成 `{名字}`
    out_parts = []
    last = 0
    for m, name in zip(PH_RE.finditer(content), names):
        out_parts.append(content[last:m.start()])
        out_parts.append("{%s}" % name)
        last = m.end()
    out_parts.append(content[last:])
    # ⚠️ `content` 是**源码原文**（`\n` 还是两个字符）⇒ 直接拼进
    # `trf!("…")` 会被 `escape()` **再转义一次** ⇒ 运行时的消息里会
    # 出现字面的 `\n` 而不是换行。
    # ⇒ 这里先还原成**运行时字符串**，最后再由 escape() 转回去。
    new_content = unescape("".join(out_parts))

    arg_src = []
    for k, v in exprs.items():
        if v[0] == "ident":
            text = v[1]
        else:
            text = recurse(toks, v[1], v[2], protected, name_overrides, stats)
        arg_src.append("%s = %s" % (k, text))
    body = 'trf!("%s", %s)' % (escape(new_content), ", ".join(arg_src))
    if macro:
        return '%s!("{}", %s)' % (macro, body)
    return body


# ── apply ────────────────────────────────────────────────────────────

def test_cutoff(src):
    """`#[cfg(test)]` 之后的内容是测试代码 —— **不翻译**。

    ⚠️ 测试里有大量**中文断言**（`assert!(x.contains("忽略未知参数"))`），
    翻译了源码里的字面量却不翻断言 ⇒ 测试必挂；而断言本来就该用中文
    （默认语言就是中文，测试运行时 `tr()` 返回中文原文）。
    """
    i = src.find("#[cfg(test)]")
    return len(src) if i < 0 else i


def const_ranges(src):
    """`const` / `static` **初始化式**的区间（里面的字面量不能包 tr()）。

    ⚠️ `const` / `static` 的初始值必须**编译期可求值** ⇒ 不能在里面调
    `tr()`（它是函数）。这类表（菜单表 `TOP_MENUS`、设置注册表 `SETTINGS`、
    快捷键表 `ALL`…）里的中文是**数据**，要翻的话在**显示处**翻
    （`tr(item.label)`）。

    这里取的是 `=` 之后到**语句末尾的 `;`** 之间的整段（表体和单值都覆盖）。
    """
    ranges = []
    for mt in re.finditer(r"\b(?:const|static)\b(?!\s+fn\b)([^=;]*)=([^;]*);", src, re.S):
        ranges.append((mt.start(2), mt.end(2)))
    return ranges


def test_ranges(src):
    """`#[cfg(test)]` 后面那些**测试代码**的区间（字符下标）。

    ⚠️ **不能"截断到第一个 `#[cfg(test)]`"**：`src/backend/forward.rs`
    里有一个**函数内联**的 `#[cfg(test)]`（第 259 行），后面还有 600 行
    正经代码 ⇒ 一截就全没了。`src/util.rs` 也一样。
    ⇒ 改成**按块配对**：找到 `#[cfg(test)]` 后面那一项
    （`mod xxx {…}` / `fn xxx(…) {…}`），把**它自己**的花括号配平，
    遮住这一块。

    ⚠️ 用**词法 token** 配平（不是裸字符扫描）：字符串里的 `{` / `}`
    不会把计数带偏。

    返回 [(start, end)]，下标与 `strip_code` 的 `marks` 一致。
    """
    toks = tokenize(src)
    ranges = []
    n = len(toks)
    i = 0
    while i < n:
        t = toks[i]
        if not (t.kind == "punct" and t.text == "#"):
            i += 1
            continue
        # 认 `#[cfg(test)]`
        j = i + 1
        if not (j + 6 < n and toks[j].kind == "punct" and toks[j].text == "["):
            i += 1
            continue
        k = j + 1
        if not (toks[k].kind == "ident" and toks[k].text == "cfg"
                and toks[k + 1].kind == "punct" and toks[k + 1].text == "("
                and toks[k + 2].kind == "ident" and toks[k + 2].text == "test"
                and toks[k + 3].kind == "punct" and toks[k + 3].text == ")"
                and toks[k + 4].kind == "punct" and toks[k + 4].text == "]"):
            i += 1
            continue
        # `]` 之后是那一项 ⇒ 找它的 `{`
        m = k + 5
        while m < n and not (toks[m].kind == "punct" and toks[m].text == "{"):
            if toks[m].kind == "punct" and toks[m].text == ";":
                break           # 没有块（如 `#[cfg(test)] use x;`）
            m += 1
        if m >= n or not (toks[m].kind == "punct" and toks[m].text == "{"):
            i += 1
            continue
        depth = 0
        p = m
        while p < n:
            tk = toks[p]
            if tk.kind == "punct" and tk.text == "{":
                depth += 1
            elif tk.kind == "punct" and tk.text == "}":
                depth -= 1
                if depth == 0:
                    ranges.append((t.start, tk.end))
                    i = p
                    break
            p += 1
        i += 1
    return ranges


def keep_regions(src):
    """`// i18n:keep` … `// i18n:end` 之间的代码**不翻译**。

    用途：有些中文**不是界面文案**，而是**被拿来当数据用**的 ——
    远端终端的密码提示、数据库服务器返回的原文、ini 里存的机制名……
    翻了它们，**逻辑就坏**（比如把"远端提示里的密码"翻成英文，
    就再也认不出那个提示了）。
    """
    ranges = []
    i = 0
    while True:
        a = src.find("i18n:keep", i)
        if a < 0:
            break
        b = src.find("i18n:end", a)
        if b < 0:
            b = len(src)
        ranges.append((a, b))
        i = b + 1
    return ranges


def menu_label_ranges(toks):
    """`menu::item/toggle/row/section` 的**标签参数**区间。

    菜单文字在 `menu::row` 里**统一翻译** ⇒ 调用点的字面量**不能**再包
    `tr()`（会翻两遍）；而且 `sidebar.rs` 那种
    `match label { "连接" => … }` 的**数据比较**必须留在中文上。
    """
    ranges = []
    n = len(toks)
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text not in MENU_LABEL_ARG:
            continue
        # 前面得是 `menu::`（`crate::menu::` 也一样）
        if i < 2:
            continue
        if not (toks[i - 1].kind == "punct" and toks[i - 1].text == "::"):
            continue
        if not (toks[i - 2].kind == "ident" and toks[i - 2].text == "menu"):
            continue
        j = i + 1
        if j >= n or not (toks[j].kind == "punct" and toks[j].text == "("):
            continue
        end = match_paren(toks, j)
        if end is None:
            continue
        args = split_tok_arg_ranges(toks, j + 1, end)
        idx = MENU_LABEL_ARG[t.text]
        if idx < len(args):
            x, y = args[idx]
            ranges.append((toks[x].start, toks[y - 1].end))
    return ranges


def i18n_call_ranges(toks):
    """已有的 `tr("…")` / `trf!("…", …)` 里那个**模板字面量**的区间。

    ⚠️ 这些字面量**已经是** i18n 的 key 了 ⇒ 再包一层 `tr()` 会变成
    `trf!(tr("…"), …)`（能跑，但多此一举，而且 key 会取错）。
    """
    ranges = []
    n = len(toks)
    for i, t in enumerate(toks):
        if t.kind != "ident" or t.text not in ("tr", "trf"):
            continue
        j = i + 1
        if j < n and toks[j].kind == "punct" and toks[j].text == "!":
            j += 1
        if j >= n or not (toks[j].kind == "punct" and toks[j].text == "("):
            continue
        end = match_paren(toks, j)
        if end is None:
            continue
        k = j + 1
        while k < end and toks[k].kind == "ws":
            k += 1
        if k < end and toks[k].kind == "str":
            ranges.append((toks[k].start, toks[k].end))
    return ranges


def in_ranges(start, end, ranges):
    for a, b in ranges:
        if start >= a and end <= b:
            return True
    return False


def transform_range(toks, lo, hi, protected, name_overrides, stats):
    """改写 `toks[lo:hi]` 里的中文文案，返回新文本。

    ⚠️ **递归**：参数里的嵌套 `format!` 和中文字符串也要改写 —— 只改
    最外层的话，参数里的中文永远翻不了。
    """
    out = []
    i = lo
    while i < hi:
        t = toks[i]
        # ── format! 调用 ──
        # ── 以及 `println!` / `eprintln!` / `panic!` 这类**要求字面量
        #    格式串**的宏（它们不能直接吃 `tr()` 的结果）──
        is_fmt = (t.kind == "ident" and t.text == "format"
                  and i + 2 < hi
                  and toks[i + 1].kind == "punct" and toks[i + 1].text == "!"
                  and toks[i + 2].kind == "punct" and toks[i + 2].text == "(")
        is_lit_fmt = (t.kind == "ident" and t.text in LITERAL_FMT_MACROS
                      and i + 2 < hi
                      and toks[i + 1].kind == "punct" and toks[i + 1].text == "!"
                      and toks[i + 2].kind == "punct" and toks[i + 2].text == "(")
        if is_fmt or is_lit_fmt:
            mac = t.text if is_lit_fmt else None
            end = match_paren(toks, i + 2)
            if end is not None and end < hi:
                tmpl_idx = i + 3
                while tmpl_idx < end and toks[tmpl_idx].kind == "ws":
                    tmpl_idx += 1
                if tmpl_idx < end and toks[tmpl_idx].kind == "str":
                    content = toks[tmpl_idx].text[1:-1]
                    tmpl_text = unescape(content)
                    in_prot = in_ranges(toks[tmpl_idx].start, toks[tmpl_idx].end,
                                        protected)
                    if CJK.search(tmpl_text) and not in_prot:
                        new = build_trf(toks, tmpl_idx, end, name_overrides,
                                        transform_range, protected, stats,
                                        macro=mac)
                        if new is not None:
                            out.append(new)
                            stats["fmt"] += 1
                            i = end + 1
                            continue
                        # 模板有中文但改不了（带格式说明）⇒ 整个调用原样保留
                        # （`format!` 的模板必须是字面量，包 `tr()` 编译不过）
                        out.append(toks_text(toks[i:end + 1]))
                        stats["kept"] += 1
                        i = end + 1
                        continue
                    # 模板**没有中文** ⇒ 调用照旧，但内部（含参数）照常改写
                    out.append("%s!(" % t.text)
                    out.append(transform_range(toks, tmpl_idx, end, protected,
                                              name_overrides, stats))
                    out.append(")")
                    i = end + 1
                    continue
        # ── 普通中文字面量 ──
        if (t.kind == "str" and CJK.search(unescape(t.text[1:-1]))
                and not in_ranges(t.start, t.end, protected)):
            out.append("tr(%s)" % t.text)
            stats["plain"] += 1
            i += 1
            continue
        out.append(t.text)
        i += 1
    return "".join(out)


def add_imports(out, need_tr, need_trf):
    lines = []
    if need_tr:
        lines.append("use crate::i18n::tr;")
    if need_trf:
        lines.append("use crate::trf;")
    if not lines:
        return out
    if "use crate::i18n::tr;" in out and "use crate::trf;" in out:
        return out
    uses = list(re.finditer(r"^use [^;]*;", out, re.M | re.S))
    if uses:
        at = uses[-1].end()
        return out[:at] + "\n" + "\n".join(lines) + out[at:]
    doc = list(re.finditer(r"^//!.*$", out, re.M))
    at = doc[-1].end() if doc else 0
    return out[:at] + "\n" + "\n".join(lines) + out[at:]


def cmd_apply(args):
    overrides = {}
    if args.overrides and os.path.exists(args.overrides):
        overrides = json.load(open(args.overrides, encoding="utf-8"))
    total = 0
    for path in args.files:
        if path.replace("\\", "/") in NO_TRANSLATE_FILES:
            print(f"{path}: 跳过（命令行契约/对照基准，保持中文）")
            continue
        src = open(path, encoding="utf-8").read()
        toks = tokenize(src)
        # ⚠️ 这些地方**都不能改写**：
        #    - 测试代码：里面的中文是**断言**（该用中文）；
        #    - const 表：里面的是**数据**（编译期求值，不能调 tr()）；
        #    - `// i18n:keep` 区：里面的中文**不是界面文案**（远端提示、
        #      服务器原文、ini 里存的值…）⇒ 翻了逻辑就坏；
        #    - 菜单标签参数：菜单文字在 `menu::row` 里统一翻译。
        protected = (const_ranges(src) + test_ranges(src)
                     + keep_regions(src) + menu_label_ranges(toks)
                     + i18n_call_ranges(toks))
        stats = {"plain": 0, "fmt": 0, "kept": 0}
        out = transform_range(toks, 0, len(toks), protected, overrides, stats)
        if not stats["plain"] and not stats["fmt"]:
            continue
        need_tr = stats["plain"] > 0
        need_trf = stats["fmt"] > 0
        out = add_imports(out, need_tr, need_trf)
        open(path, "w", encoding="utf-8").write(out)
        total += stats["plain"] + stats["fmt"]
        print(f"{path}: tr() {stats['plain']} 处、trf!() {stats['fmt']} 处"
              + (f"（原样保留 {stats['kept']} 个 format!）" if stats["kept"] else ""))
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
    p.add_argument("--overrides", default=None)
    p.set_defaults(func=cmd_apply)

    args = ap.parse_args(argv)
    args.func(args)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
