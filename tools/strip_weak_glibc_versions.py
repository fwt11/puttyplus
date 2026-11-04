#!/usr/bin/env python3
"""把 ELF 里"弱符号（STB_WEAK）"的 glibc 版本要求摘掉。

为什么需要它
------------
Rust 的 std 会**弱引用**若干"新一点的" libc 函数（`copy_file_range` 2.27、
`statx` 2.28、`gettid` 2.30、`pidfd_spawnp` / `pidfd_getpid` 2.39 …）。弱引用的
意思是"glibc 有就用，没有就置空走回退路径"。

链接时若**构建机的 glibc >= 某个版本**，链接器会给这些弱符号打上版本标签：

    w  DF *UND*  (GLIBC_2.39)  pidfd_spawnp

标签一旦打上，两个后果：

1. `dpkg-shlibdeps` 看到 `GLIBC_2.39`，把 deb 的依赖写成 `libc6 (>= 2.39)`
   ==> **在 glibc 2.38 的机器上 dpkg 直接拒绝安装**（哪怕程序本来能跑）。
2. 在没有该版本的机器上，动态加载器每次启动都打一行
   `weak version 'GLIBC_2.39' not found`。

同一个程序在 glibc 2.38 的机器上构建，就不会带版本标签（因为那里的 libc
根本没有这两个符号） ==> 于是 deb 的依赖是 `libc6 (>= 2.34)`，一切正常。

本脚本把 trixie 容器里编出来的二进制**改回"在老 glibc 上构建"的样子**：

- 步骤一：把 `.gnu.version` 里这些弱符号的版本索引置 0（= 不带版本）；
- 步骤二：把 `.gnu.version_r` 里对应的版本需求条目**真的从 aux 链上摘掉**
  （改链接指针，**不是只改 vn_cnt**）；
- 步骤三：按链上的真实条数重写 `vn_cnt`；
- 步骤四：自检（链长、符号索引、硬下限三条都要过），不过就报错退出。

⚠️ **dpkg 到底怎么算出这个依赖的**（2026-11 在 ARM 上踩坑记，务必读）：
`dpkg-shlibdeps` 是个 Perl 脚本，它跑 `objdump -p` 并解析 **Version
References** 那一段，把里面的每条版本要求当成一个"未定义符号"注入进去，
再拿**构建机上真实的 libc 导出的版本**去满足它。所以：

- 依赖高低取决于**版本需求表（.gnu.version_r）**，不是符号表；
- 构建机 glibc 只要 >= 2.39（Debian 13 / Ubuntu 25.04 都是），就满足得了
  GLIBC_2.39 ==> 依赖被算成 `libc6 (>= 2.39)`；
  （本机 glibc 2.38 满足不了 ==> 本地怎么试都是 2.34，**本地复现不出来**）
- 而 **objdump 是按 `vn_cnt` 截断打印的** ==> 改版本需求表时，链上少一条、
  `vn_cnt` 没跟着改（或反过来），被吞掉的会是**链尾那一条**。
  链尾在不同架构上不一样（ARM 上是硬下限 GLIBC_2.34，amd64 上可能是
  GLIBC_2.39）==> 同一个 bug 在两个架构上一红一绿，极难看出。

只动**弱**符号；强符号（STB_GLOBAL）的版本要求一律保留 —— 那是真正的依赖，
不能摘。脚本不改变文件大小，只改写几个 2 字节和几个 4 字节。

用法：
    python3 tools/strip_weak_glibc_versions.py <elf 可执行文件> [...]
    python3 tools/strip_weak_glibc_versions.py --check <elf 可执行文件>   # 只体检
    python3 tools/strip_weak_glibc_versions.py --selftest                 # 自测
"""

import struct
import sys

SHT_DYNSYM = 11
SHT_GNU_VERNEED = 0x6FFFFFFE
SHT_GNU_VERDEF = 0x6FFFFFFD
SHT_GNU_VERSYM = 0x6FFFFFFF

STB_GLOBAL = 1   # ELF: STB_LOCAL=0, STB_GLOBAL=1, STB_WEAK=2
STB_WEAK = 2
SHN_UNDEF = 0
VER_NDX_GLOBAL = 0
VER_NDX_LOCAL = 1
VER_NDX_MASK = 0x7fff   # .gnu.version 索引的高位是 hidden 标志，取低 15 位


def parse_glibc_version(name):
    """'GLIBC_2.34' -> (2, 34)；不是 GLIBC_x.y 就返回 None。"""
    if not name.startswith("GLIBC_"):
        return None
    parts = name[len("GLIBC_"):].split(".")
    if len(parts) != 2 or not all(p.isdigit() for p in parts):
        return None
    return int(parts[0]), int(parts[1])


def fmt_ver(v):
    return "GLIBC_%d.%d" % v


class Elf:
    def __init__(self, path):
        with open(path, "rb") as f:
            self.data = bytearray(f.read())
        d = self.data
        if d[:4] != b"\x7fELF":
            raise SystemExit(f"{path}: 不是 ELF 文件")
        if d[4] != 2:
            raise SystemExit(f"{path}: 只支持 ELF64")
        if d[5] != 1:
            raise SystemExit(f"{path}: 只支持小端")
        (self.e_shoff,) = struct.unpack_from("<Q", d, 0x28)
        (self.e_shentsize,) = struct.unpack_from("<H", d, 0x3A)
        (self.e_shnum,) = struct.unpack_from("<H", d, 0x3C)
        (self.e_shstrndx,) = struct.unpack_from("<H", d, 0x3E)
        self.sections = []
        for i in range(self.e_shnum):
            off = self.e_shoff + i * self.e_shentsize
            (name, stype, flags, addr, offset, size, link, info, align,
             entsize) = struct.unpack_from("<IIQQQQIIQQ", d, off)
            self.sections.append(
                dict(name=name, type=stype, offset=offset, size=size,
                     link=link, entsize=entsize))
        shstr = self.sections[self.e_shstrndx]
        self.shstrtab = bytes(d[shstr["offset"]:shstr["offset"] + shstr["size"]])

    def sec_name(self, sh):
        end = self.shstrtab.find(b"\0", sh["name"])
        return self.shstrtab[sh["name"]:end].decode("utf-8", "replace")

    def find(self, stype):
        for sh in self.sections:
            if sh["type"] == stype:
                return sh
        return None

    def strtab(self, sh):
        return bytes(self.data[sh["offset"]:sh["offset"] + sh["size"]])

    def cstr(self, tab, off):
        end = tab.find(b"\0", off)
        return tab[off:end].decode("utf-8", "replace")

    def write_back(self, path):
        with open(path, "wb") as f:
            f.write(self.data)


def verneed_sections(elf):
    """所有 .gnu.version_r 段（一般只有一个）。"""
    return [sh for sh in elf.sections if sh["type"] == SHT_GNU_VERNEED]


def collect_verdef_indices(elf):
    """本二进制**自己定义**的版本索引（.gnu.version_d 里的 vd_ndx）。

    索引空间是版本需求表与版本定义表共用的 ⇒ 符号引用的版本索引既可能
    来自 verneed（别人的库），也可能来自 verdef（自己定义的版本）。
    """
    out = set()
    for sh in elf.sections:
        if sh["type"] != SHT_GNU_VERDEF:
            continue
        sec_end = sh["offset"] + sh["size"]
        off = sh["offset"]
        while off < sec_end:
            (_v, _flags, vd_ndx, _cnt, _hash, _aux, vd_next) = struct.unpack_from(
                "<HHHHIII", elf.data, off)
            out.add(vd_ndx & VER_NDX_MASK)
            if vd_next == 0:
                break
            off += vd_next
    return out


def walk_chain(elf, rec_off, vn_aux, sec_end, tab):
    """按 vna_next 走一遍 aux 链。

    返回 [(文件内偏移, 版本索引, 版本名, vna_next), …]。

    ⚠️ **按 vna_next 走，不按 vn_cnt 走**：vna_next 是"本条到下一条的
    **相对**偏移"（链尾那条是 0）；vn_cnt 只是"有几条"的计数，真实链长
    得自己数（改过之后更要把 vn_cnt 写回去，见 refresh_vn_cnt）。
    """
    out = []
    a = rec_off + vn_aux
    while True:
        if a + 16 > sec_end:
            raise SystemExit("版本需求表的 aux 链越界（文件损坏？）")
        (_h, _fl, other, name, nxt) = struct.unpack_from("<IHHII", elf.data, a)
        out.append((a, other & VER_NDX_MASK, elf.cstr(tab, name), nxt))
        if nxt == 0:
            break
        a += nxt
    return out


def collect_verneed(elf):
    """解析 `.gnu.version_r`。

    返回 (版本索引->版本名, [记录])；每条记录 = (记录偏移, 库文件名, vn_aux,
    [aux 条目])；每个 aux 条目 = (文件内偏移, 版本索引, 版本名, vna_next)。
    """
    names = {}
    records = []
    for sh in verneed_sections(elf):
        tab = elf.strtab(elf.sections[sh["link"]])
        sec_end = sh["offset"] + sh["size"]
        off = sh["offset"]
        while off < sec_end:
            (_v, _vn_cnt, vn_file, vn_aux, vn_next) = struct.unpack_from(
                "<HHIII", elf.data, off)
            auxes = walk_chain(elf, off, vn_aux, sec_end, tab)
            for _a, other, nm, _n in auxes:
                names[other] = nm
            records.append((off, elf.cstr(tab, vn_file), vn_aux, auxes))
            if vn_next == 0:
                break
            off += vn_next
    return names, records


def prune_verneed(elf, records, target_name):
    """把某个版本需求的 aux 条目从链上**真的摘掉**。返回 True=摘掉了。

    ⚠️ **这里最容易写错**（2026-11 在 ARM 上踩过，amd64 上却"碰巧"是对的）：

    - `vna_next` 是「**本条**到下一条的相对偏移」，链尾那条是 0；
    - 摘掉第 i 条时，前一条的 `vna_next` 要改成
      「前一条自己的 vna_next **+** 本条的 vna_next」——
      直接把本条的 vna_next 写进去等于**什么都没改**（两者常常都是 16）；
    - 摘掉的是链尾时，前一条的 `vna_next` 要改成 **0**（链到此为止）；
    - 摘掉的是第一条时，`vn_aux`（相对记录）要改成
      「vn_aux **+** 本条的 vna_next」。

    只改 `vn_cnt` 不改链 ==> objdump 按 vn_cnt 截断打印 ==> **链尾那条被吞掉**
    （ARM 上那恰好是硬下限 GLIBC_2.34 ==> deb 依赖反而变成 libc6 (>= 2.39)）。
    """
    for rec_off, _fname, vn_aux, auxes in records:
        for i, (_a, _other, nm, nxt) in enumerate(auxes):
            if nm != target_name:
                continue
            if i == 0:
                # 第一条：把 vn_aux（相对记录）指到下一条
                new = 0 if nxt == 0 else vn_aux + nxt
                struct.pack_into("<I", elf.data, rec_off + 8, new)
            else:
                # 中间/末尾：让前一条跳过本条
                prev_off, _po, _pn, prev_nxt = auxes[i - 1]
                new = 0 if nxt == 0 else prev_nxt + nxt
                struct.pack_into("<I", elf.data, prev_off + 12, new)
            return True
    return False


def refresh_vn_cnt(elf):
    """按链上的真实条数重写每条记录的 `vn_cnt`。

    ⚠️ objdump（dpkg-shlibdeps 靠它）**按 vn_cnt 截断打印** Version
    References ⇒ vn_cnt 必须等于真实链长，否则链尾那条会被吞掉。
    """
    changed = 0
    for sh in verneed_sections(elf):
        tab = elf.strtab(elf.sections[sh["link"]])
        sec_end = sh["offset"] + sh["size"]
        off = sh["offset"]
        while off < sec_end:
            (_v, vn_cnt, _f, vn_aux, vn_next) = struct.unpack_from(
                "<HHIII", elf.data, off)
            n = len(walk_chain(elf, off, vn_aux, sec_end, tab))
            if vn_cnt != n:
                struct.pack_into("<H", elf.data, off + 2, n)
                changed += 1
            if vn_next == 0:
                break
            off += vn_next
    return changed


def analyze(elf):
    """算出硬下限、要摘的弱符号、版本需求表。"""
    dynsym = elf.find(SHT_DYNSYM)
    versym_sec = elf.find(SHT_GNU_VERSYM)
    if dynsym is None or versym_sec is None:
        return None
    dynstr = elf.strtab(elf.sections[dynsym["link"]])
    vnames, records = collect_verneed(elf)

    n_sym = dynsym["size"] // 24
    versym = list(struct.unpack_from("<%dH" % n_sym, elf.data, versym_sec["offset"]))

    # 硬下限：强符号用到的最高 glibc 版本
    floor = (0, 0)
    for i in range(n_sym):
        base = dynsym["offset"] + i * 24
        _st_name, st_info = struct.unpack_from("<IB", elf.data, base)
        st_shndx = struct.unpack_from("<H", elf.data, base + 6)[0]
        if st_shndx != SHN_UNDEF:
            continue
        if (st_info >> 4) == STB_WEAK:
            continue  # 弱符号不算硬依赖
        v = versym[i] & VER_NDX_MASK
        if v >= 2:
            gv = parse_glibc_version(vnames.get(v, ""))
            if gv and gv > floor:
                floor = gv

    # 找出"版本高于硬下限"的弱符号
    targets = []
    for i in range(n_sym):
        base = dynsym["offset"] + i * 24
        st_name, st_info = struct.unpack_from("<IB", elf.data, base)
        st_shndx = struct.unpack_from("<H", elf.data, base + 6)[0]
        if st_shndx != SHN_UNDEF or (st_info >> 4) != STB_WEAK:
            continue
        v = versym[i] & VER_NDX_MASK
        if v < 2:
            continue
        name = vnames.get(v, "")
        gv = parse_glibc_version(name)
        if not gv or gv <= floor:
            continue  # 不高于硬下限，本来就兼容，保留版本信息
        targets.append((i, elf.cstr(dynstr, st_name), name, v))

    return dict(floor=floor, targets=targets, vnames=vnames, records=records,
                versym=versym, dynsym=dynsym, versym_sec=versym_sec,
                dynstr=dynstr)


def verify(path):
    """体检：版本需求表必须自洽。返回问题列表（空 = 没问题）。"""
    elf = Elf(path)
    problems = []
    info = analyze(elf)
    if info is None:
        return ["没有动态符号/版本表"]
    vnames, records = info["vnames"], info["records"]
    dynsym, versym_sec = info["dynsym"], info["versym_sec"]
    n_sym = dynsym["size"] // 24
    versym = info["versym"]

    # (a) 每条记录的 vn_cnt 必须等于链上的真实条数
    for sh in verneed_sections(elf):
        tab = elf.strtab(elf.sections[sh["link"]])
        sec_end = sh["offset"] + sh["size"]
        off = sh["offset"]
        while off < sec_end:
            (_v, vn_cnt, _f, vn_aux, vn_next) = struct.unpack_from(
                "<HHIII", elf.data, off)
            n = len(walk_chain(elf, off, vn_aux, sec_end, tab))
            if vn_cnt != n:
                problems.append(
                    "vn_cnt=%d 与真实链长 %d 对不上（objdump 会少打印 %d 条）"
                    % (vn_cnt, n, n - vn_cnt if n > vn_cnt else 0))
            if vn_next == 0:
                break
            off += vn_next

    # (b) 未定义符号的版本索引必须都能在链里找到对应条目，
    #     否则动态加载器会报 "version ... not found" 直接起不来。
    #     ⚠️ 索引空间是**版本需求表 + 本二进制自己定义的版本表**共用的，
    #        所以 verdef 里的索引也算"认得"。
    known = set(vnames.keys())
    known |= collect_verdef_indices(elf)
    for i in range(n_sym):
        base = dynsym["offset"] + i * 24
        st_name, st_info = struct.unpack_from("<IB", elf.data, base)
        st_shndx = struct.unpack_from("<H", elf.data, base + 6)[0]
        if st_shndx != SHN_UNDEF:
            continue
        v = versym[i] & VER_NDX_MASK
        if v >= 2 and v not in known:
            problems.append("符号 %s 用的版本索引 %d 在版本需求表里找不到"
                            % (elf.cstr(info["dynstr"], st_name), v))

    # (c) 版本需求表里不该有高于硬下限的 GLIBC 版本
    floor = info["floor"]
    for _rec_off, fname, _vn_aux, auxes in records:
        for _a, _other, nm, _n in auxes:
            gv = parse_glibc_version(nm)
            if gv and gv > floor:
                problems.append("%s 仍要求 %s，高于硬下限 %s"
                                % (fname, nm, fmt_ver(floor)))
    return problems


def needs_table(elf):
    """版本需求表：[(库名, [版本名, …]), …]。"""
    _vnames, records = collect_verneed(elf)
    return [(fname, [a[2] for a in auxes]) for _off, fname, _aux, auxes in records]


def strip(path):
    elf = Elf(path)
    info = analyze(elf)
    if info is None:
        print("%s: 没有动态符号/版本表，跳过" % path)
        return 0
    floor = info["floor"]
    targets = info["targets"]
    if not targets:
        print("%s: 无需处理（硬下限 %s）" % (path, fmt_ver(floor)))
        return 0

    versym = info["versym"]
    versym_sec = info["versym_sec"]
    n_sym = info["dynsym"]["size"] // 24

    # 步骤一：弱符号的版本索引置 0（= 不带版本）
    for i, _sym, _ver, _v in targets:
        versym[i] = VER_NDX_GLOBAL
    struct.pack_into("<%dH" % n_sym, elf.data, versym_sec["offset"], *versym)

    # 步骤二：把对应的版本需求条目从 aux 链上摘掉（否则加载器仍会报警告）
    # ⚠️ **每摘一个都要重新读一遍链**：摘过之后前面的 vna_next 变了，
    #    再拿旧记录表去摘下一条，会写到**已经不在链上的**那条 aux 里
    #    （摘了等于没摘）。
    removed = []
    for _i, sym, ver, _v in targets:
        if ver in removed:
            continue
        _vn, recs = collect_verneed(elf)
        if prune_verneed(elf, recs, ver):
            removed.append(ver)

    # 步骤三：按真实链长重写 vn_cnt
    fixed = refresh_vn_cnt(elf)

    elf.write_back(path)

    for _i, sym, ver, _v in targets:
        print("  %s: 摘掉弱符号 %s 的 %s 版本要求" % (path, sym, ver))
    for ver in removed:
        print("  %s: 版本需求条目 %s 已从 .gnu.version_r 摘掉" % (path, ver))
    if fixed:
        print("  %s: 修正了 %d 条记录的 vn_cnt（按真实链长）" % (path, fixed))
    print("  %s: 硬下限 %s" % (path, fmt_ver(floor)))

    # 步骤四：自检 —— 改坏了就当场报错，别等到 dpkg 那步才发现
    problems = verify(path)
    if problems:
        print("  %s: ❌ 自检不通过：" % path)
        for p in problems:
            print("      - %s" % p)
        sys.exit(1)
    print("  %s: ✅ 自检通过" % path)
    return len(targets)


def check(path):
    problems = verify(path)
    elf = Elf(path)
    info = analyze(elf)
    print("%s:" % path)
    if info is not None:
        print("  硬下限 %s" % fmt_ver(info["floor"]))
    for fname, versions in needs_table(elf):
        print("  %-24s %s" % (fname, " ".join(versions)))
    if problems:
        print("  ❌ 有问题：")
        for p in problems:
            print("      - %s" % p)
        return 1
    print("  ✅ 版本需求表自洽")
    return 0


def build_fake_elf():
    """造一个最小的 ELF64（带一条 .gnu.version_r 链），用来自测。

    链上是 GLIBC_2.39 / GLIBC_2.34 / GLIBC_2.17 三条 —— 故意把
    GLIBC_2.39 放在**链中间**、GLIBC_2.17 放在**链尾**，
    因为这两种位置正是旧实现会摘错的地方。
    """
    dynstr = bytearray(b"\0")
    offs = {}
    for s in (b"libc.so.6", b"GLIBC_2.39", b"GLIBC_2.34", b"GLIBC_2.17"):
        offs[s] = len(dynstr)
        dynstr += s + b"\0"

    verneed = bytearray()
    verneed += struct.pack("<HHIII", 1, 3, offs[b"libc.so.6"], 16, 0)
    verneed += struct.pack("<IHHII", 0, 0, 2, offs[b"GLIBC_2.39"], 16)
    verneed += struct.pack("<IHHII", 0, 0, 3, offs[b"GLIBC_2.34"], 16)
    verneed += struct.pack("<IHHII", 0, 0, 4, offs[b"GLIBC_2.17"], 0)

    shstr = bytearray(b"\0")
    name_offs = {}
    for s in (b".dynstr", b".gnu.version_r", b".shstrtab"):
        name_offs[s] = len(shstr)
        shstr += s + b"\0"

    ehsize = 64
    shentsize = 64
    off_dynstr = ehsize
    off_verneed = off_dynstr + len(dynstr)
    off_shstr = off_verneed + len(verneed)
    off_sh = off_shstr + len(shstr)
    off_sh = (off_sh + 7) & ~7

    hdr = bytearray()
    hdr += b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8
    hdr += struct.pack("<HHIQQQIHHHHHH",
                       2, 62, 1, 0, 0, off_sh, 0, ehsize,
                       0, 0, shentsize, 4, 3)

    def shdr(name, stype, offset, size, link=0):
        return struct.pack("<IIQQQQIIQQ", name, stype, 0, 0, offset, size,
                           link, 0, 1, 0)

    shs = bytearray()
    shs += shdr(0, 0, 0, 0)
    shs += shdr(name_offs[b".dynstr"], 3, off_dynstr, len(dynstr))
    shs += shdr(name_offs[b".gnu.version_r"], SHT_GNU_VERNEED, off_verneed,
                len(verneed), link=1)
    shs += shdr(name_offs[b".shstrtab"], 3, off_shstr, len(shstr))

    data = bytearray()
    data += hdr
    data += dynstr
    data += verneed
    data += shstr
    data += b"\x00" * (off_sh - len(data))
    data += shs
    return bytes(data)


def selftest():
    """自测：摘链的三种位置（链首 / 链中 / 链尾）都不能吃掉别的条目。"""
    import os
    import tempfile

    cases = [
        ("GLIBC_2.39", ["GLIBC_2.34", "GLIBC_2.17"], "摘链中间那条"),
        ("GLIBC_2.17", ["GLIBC_2.39", "GLIBC_2.34"], "摘链尾那条"),
        ("GLIBC_2.34", ["GLIBC_2.39", "GLIBC_2.17"], "摘正中间那条"),
    ]
    fd, path = tempfile.mkstemp(prefix="pp-strip-selftest-")
    os.close(fd)
    failed = 0
    try:
        for target, expect, what in cases:
            with open(path, "wb") as f:
                f.write(build_fake_elf())
            elf = Elf(path)
            _names, records = collect_verneed(elf)
            before = [a[2] for a in records[0][3]]
            if before != ["GLIBC_2.39", "GLIBC_2.34", "GLIBC_2.17"]:
                print("❌ 自测样本构造不对：%s" % before)
                failed += 1
                continue
            if not prune_verneed(elf, records, target):
                print("❌ %s：摘不掉 %s" % (what, target))
                failed += 1
                continue
            refresh_vn_cnt(elf)
            elf.write_back(path)

            elf2 = Elf(path)
            _n2, records2 = collect_verneed(elf2)
            after = [a[2] for a in records2[0][3]]
            vn_cnt = struct.unpack_from("<H", elf2.data, records2[0][0] + 2)[0]
            if after != expect or vn_cnt != len(expect):
                print("❌ %s：摘 %s 后得到 %s（vn_cnt=%d），期望 %s"
                      % (what, target, after, vn_cnt, expect))
                failed += 1
            else:
                print("✅ %s：摘 %s 后剩 %s，vn_cnt=%d"
                      % (what, target, after, vn_cnt))

        # 连着摘两条：第二条必须**重新读链**才能摘对
        # （用旧记录表会写到已经不在链上的那条 aux 里 —— 摘了等于没摘）
        with open(path, "wb") as f:
            f.write(build_fake_elf())
        elf = Elf(path)
        for target in ("GLIBC_2.39", "GLIBC_2.34"):
            _vn, recs = collect_verneed(elf)
            if not prune_verneed(elf, recs, target):
                print("❌ 连摘：摘不掉 %s" % target)
                failed += 1
                break
        else:
            refresh_vn_cnt(elf)
            elf.write_back(path)
            elf2 = Elf(path)
            _n2, records2 = collect_verneed(elf2)
            after = [a[2] for a in records2[0][3]]
            vn_cnt = struct.unpack_from("<H", elf2.data, records2[0][0] + 2)[0]
            if after != ["GLIBC_2.17"] or vn_cnt != 1:
                print("❌ 连摘两条后得到 %s（vn_cnt=%d），期望 ['GLIBC_2.17']"
                      % (after, vn_cnt))
                failed += 1
            else:
                print("✅ 连摘两条后剩 %s，vn_cnt=%d" % (after, vn_cnt))
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
    if failed:
        print("自测失败：%d 项" % failed)
        return 1
    print("自测全部通过")
    return 0


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    args = argv[1:]
    if args[0] == "--selftest":
        return selftest()
    if args[0] == "--check":
        rc = 0
        for p in args[1:]:
            rc |= check(p)
        return rc
    total = 0
    for p in args:
        total += strip(p)
    print("共摘掉 %d 个弱符号的版本要求" % total)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
