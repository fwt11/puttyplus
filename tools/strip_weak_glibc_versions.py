#!/usr/bin/env python3
"""把 ELF 里"弱符号（STB_WEAK）"的 glibc 版本要求摘掉。

为什么需要它
------------
Rust 的 std 会**弱引用**若干"新一点的" libc 函数（`copy_file_range` 2.27、
`statx` 2.28、`gettid` 2.30、`pidfd_spawnp` / `pidfd_getpid` 2.39 …）。弱引用的
意思是"glibc 有就用，没有就置空走回退路径"。

链接时若**构建机的 glibc ≥ 某个版本**，链接器会给这些弱符号打上版本标签：

    w  DF *UND*  (GLIBC_2.39)  pidfd_spawnp

标签一旦打上，两个后果：

1. `dpkg-shlibdeps` 看到 `GLIBC_2.39`，把 deb 的依赖写成 `libc6 (>= 2.39)`
   ⇒ **在 glibc 2.38 的机器上 dpkg 直接拒绝安装**（哪怕程序本来能跑）。
2. 在没有该版本的机器上，动态加载器每次启动都打一行
   `weak version 'GLIBC_2.39' not found`。

同一个程序在 glibc 2.38 的机器上构建，就不会带版本标签（因为那里的 libc
根本没有这两个符号）—— 于是 deb 的依赖是 `libc6 (>= 2.34)`，一切正常。

本脚本把 trixie 容器里编出来的二进制**改回"在老 glibc 上构建"的样子**：

- 步骤一：把 `.gnu.version` 里这些弱符号的版本索引置 0（= 不带版本）；
- 步骤二：把 `.gnu.version_r` 里对应的版本需求条目（Vernaux）从链上摘掉。

只动**弱**符号；强符号（STB_GLOBAL）的版本要求一律保留 —— 那是真正的依赖，
不能摘。脚本不改变文件大小，只改写几个 2 字节和几个 4 字节。

用法：
    python3 tools/strip_weak_glibc_versions.py <elf 可执行文件> [...]
"""

import struct
import sys

SHT_DYNSYM = 11
SHT_GNU_VERNEED = 0x6FFFFFFE
SHT_GNU_VERSYM = 0x6FFFFFFF

STB_GLOBAL = 1   # ELF: STB_LOCAL=0, STB_GLOBAL=1, STB_WEAK=2
STB_WEAK = 2
SHN_UNDEF = 0
VER_NDX_GLOBAL = 0


def parse_glibc_version(name):
    """'GLIBC_2.34' -> (2, 34)；不是 GLIBC_x.y 就返回 None。"""
    if not name.startswith("GLIBC_"):
        return None
    parts = name[len("GLIBC_"):].split(".")
    if len(parts) != 2 or not all(p.isdigit() for p in parts):
        return None
    return int(parts[0]), int(parts[1])


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


def collect_verneed(elf):
    """解析 `.gnu.version_r`。

    返回 (版本索引->版本名, [记录])；每条记录 = (记录偏移, 库文件名,
    [(aux 偏移, 版本名, vna_next), …])。

    ⚠️ `.gnu.version` 里符号的版本索引存的是对应 `Elf64_Vernaux.vna_other`
    （gABI：vna_other 就是"这个版本在 .gnu.version 里用的索引"），不是它在
    aux 链里的位置 —— 所以按 vna_other 建表。
    """
    names = {}
    records = []
    for sh in elf.sections:
        if sh["type"] != SHT_GNU_VERNEED:
            continue
        tab = elf.strtab(elf.sections[sh["link"]])
        off = sh["offset"]
        end = sh["offset"] + sh["size"]
        while off < end:
            (_v, vn_cnt, vn_file, vn_aux, vn_next) = struct.unpack_from(
                "<HHIII", elf.data, off)
            auxes = []
            a = off + vn_aux
            for _ in range(vn_cnt):
                (_h, _fl, vna_other, vna_name, vna_next2) = struct.unpack_from(
                    "<IHHII", elf.data, a)
                nm = elf.cstr(tab, vna_name)
                names[vna_other] = nm
                auxes.append((a, nm, vna_next2))
                if vna_next2 == 0:
                    break
                a += vna_next2
            records.append((off, elf.cstr(tab, vn_file), auxes))
            if vn_next == 0:
                break
            off += vn_next
    return names, records


def prune_verneed(elf, records, target_name):
    """把某个版本需求条目从 aux 链上摘掉。返回 True=摘掉了。"""
    for rec_off, _fname, auxes in records:
        for i, (a_off, nm, nxt) in enumerate(auxes):
            if nm != target_name:
                continue
            if i == 0:
                # 第一条：把 vn_aux 指到下一条
                struct.pack_into("<I", elf.data, rec_off + 8, nxt)
            else:
                # 中间/末尾：让前一条不再指向它
                prev_off = auxes[i - 1][0]
                struct.pack_into("<I", elf.data, prev_off + 12, nxt)
            # vn_cnt 减一（Verneed: vn_version(0) vn_cnt(2) vn_file(4) …）
            struct.pack_into("<H", elf.data, rec_off + 2, len(auxes) - 1)
            return True
    return False


def strip(path):
    elf = Elf(path)
    dynsym = elf.find(SHT_DYNSYM)
    versym_sec = elf.find(SHT_GNU_VERSYM)
    if dynsym is None or versym_sec is None:
        print(f"{path}: 没有动态符号/版本表，跳过")
        return 0
    dynstr = elf.strtab(elf.sections[dynsym["link"]])
    vnames, records = collect_verneed(elf)

    n_sym = dynsym["size"] // 24
    versym = list(struct.unpack_from("<%dH" % n_sym, elf.data, versym_sec["offset"]))

    # 先算"硬下限"：强符号用到的最高 glibc 版本
    floor = (0, 0)
    for i in range(n_sym):
        base = dynsym["offset"] + i * 24
        st_name, st_info = struct.unpack_from("<IB", elf.data, base)
        st_shndx = struct.unpack_from("<H", elf.data, base + 6)[0]
        if st_shndx != SHN_UNDEF:
            continue
        if (st_info >> 4) == STB_WEAK:
            continue  # 弱符号不算硬依赖
        v = versym[i]
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
        v = versym[i]
        if v < 2:
            continue
        name = vnames.get(v, "")
        gv = parse_glibc_version(name)
        if not gv or gv <= floor:
            continue  # 不高于硬下限，本来就兼容，保留版本信息
        targets.append((i, elf.cstr(dynstr, st_name), name))

    if not targets:
        print(f"{path}: 无需处理（硬下限 GLIBC_%d.%d）" % (floor[0], floor[1]))
        return 0

    # 步骤一：弱符号的版本索引置 0（= 不带版本）
    for i, _sym, _ver in targets:
        versym[i] = VER_NDX_GLOBAL
    struct.pack_into("<%dH" % n_sym, elf.data, versym_sec["offset"], *versym)

    # 步骤二：把对应的版本需求条目从 aux 链上摘掉（否则加载器仍会报警告）
    removed = []
    for _i, sym, ver in targets:
        if ver not in removed:
            if prune_verneed(elf, records, ver):
                removed.append(ver)

    elf.write_back(path)
    for _i, sym, ver in targets:
        print(f"  {path}: 摘掉弱符号 {sym} 的 {ver} 版本要求")
    for ver in removed:
        print(f"  {path}: 版本需求条目 {ver} 已从 .gnu.version_r 摘掉")
    print(f"  {path}: 硬下限 GLIBC_%d.%d" % (floor[0], floor[1]))
    return len(targets)


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    total = 0
    for p in argv[1:]:
        total += strip(p)
    print(f"共摘掉 {total} 个弱符号的版本要求")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
