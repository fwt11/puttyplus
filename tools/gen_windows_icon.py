#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 Windows 的 assets/puttyplus.ico（阶段 120）。

真源是 src/appicon.rs 里内嵌的 PNG_BASE64（128×128，深色圆角底 + 绿色
提示符 > + 青色传输条）—— 单一来源，不在仓库里再放一份 PNG 资产。

产出 ICO 含 16/24/32/48/64/128 六个尺寸：
  - 16~64 由 128 原图**面积平均**（box filter）缩小 —— 对缩小来说比双线性
    干净（不会把细线抖丢）；透明边缘先按 premultiplied 平均再还原，
    免得圆角外的透明像素把边缘 RGB 拉出黑边；
  - 128 条目**直接用原始 PNG 字节**（不重编码，零损）；
  - 条目载荷全用 PNG 格式（Vista+ 都认，省去 BMP/AND 掩码那套）。

用法：python tools/gen_windows_icon.py
依赖：纯 stdlib（base64/zlib/struct/re），零第三方包。
"""

import base64
import os
import re
import struct
import sys
import zlib

PNG_SIG = b"\x89PNG\r\n\x1a\n"
SIZES = [16, 24, 32, 48, 64]  # 128 用原图字节，不进这个列表


def extract_png_bytes(repo: str) -> bytes:
    """从 src/appicon.rs 的 PNG_BASE64 常量解出 PNG 字节。"""
    src_path = os.path.join(repo, "src", "appicon.rs")
    with open(src_path, encoding="utf-8") as f:
        src = f.read()
    m = re.search(r"const PNG_BASE64:\s*&str\s*=\s*concat!\((.*?)\);", src, re.S)
    if not m:
        raise SystemExit("没在 src/appicon.rs 里找到 PNG_BASE64 常量（格式变了？）")
    parts = re.findall(r'"([^"]*)"', m.group(1))
    if not parts:
        raise SystemExit("PNG_BASE64 里一个字符串段都没有")
    return base64.b64decode("".join(parts))


def decode_png(data: bytes):
    """极简 PNG 解码：8bit、colortype 2(RGB)/6(RGBA)、非隔行。别的直接拒绝。"""
    if data[:8] != PNG_SIG:
        raise SystemExit("不是 PNG（魔数对不上）")
    pos = 8
    ihdr = None
    idat = bytearray()
    while pos + 8 <= len(data):
        length = int.from_bytes(data[pos : pos + 4], "big")
        ctype = data[pos + 4 : pos + 8]
        payload = data[pos + 8 : pos + 8 + length]
        pos += 12 + length  # 4 长度 + 4 类型 + 数据 + 4 CRC
        if ctype == b"IHDR":
            ihdr = payload
        elif ctype == b"IDAT":
            idat += payload
        elif ctype == b"IEND":
            break
    if ihdr is None:
        raise SystemExit("PNG 没有 IHDR")
    w = int.from_bytes(ihdr[0:4], "big")
    h = int.from_bytes(ihdr[4:8], "big")
    bitdepth, colortype, comp, filt, interlace = ihdr[8:13]
    if (bitdepth, colortype) not in ((8, 2), (8, 6)):
        raise SystemExit(f"不支持的 PNG（bitdepth={bitdepth} colortype={colortype}）——"
                         "只认 8bit RGB/RGBA，appicon 的图不该是别的")
    if (comp, filt, interlace) != (0, 0, 0):
        raise SystemExit("不支持的 PNG（压缩/滤波/隔行参数异常）")
    ch = 3 if colortype == 2 else 4
    raw = zlib.decompress(bytes(idat))

    # 反滤波（逐行）
    stride = w * ch
    expected = h * (stride + 1)
    if len(raw) != expected:
        raise SystemExit(f"PNG 像素数据长度不对：期望 {expected}，实际 {len(raw)}")
    rgba = bytearray(w * h * 4)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        ftype = raw[p]
        p += 1
        line = bytearray(raw[p : p + stride])
        p += stride
        if ftype == 1:  # Sub
            for i in range(ch, stride):
                line[i] = (line[i] + line[i - ch]) & 0xFF
        elif ftype == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ftype == 3:  # Average
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:  # Paeth
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                b = prev[i]
                c = prev[i - ch] if i >= ch else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        elif ftype != 0:
            raise SystemExit(f"未知滤波类型 {ftype}（第 {y} 行）")
        prev = line
        for x in range(w):
            si = x * ch
            di = (y * w + x) * 4
            rgba[di] = line[si]
            rgba[di + 1] = line[si + 1]
            rgba[di + 2] = line[si + 2]
            rgba[di + 3] = line[si + 3] if ch == 4 else 255
    return w, h, bytes(rgba)


def resize_box(rgba, w, h, nw, nh):
    """面积平均缩小（premultiplied alpha，免透明边发黑）。"""
    out = bytearray(nw * nh * 4)
    # premultiply
    pm = bytearray(w * h * 4)
    for i in range(0, w * h * 4, 4):
        a = rgba[i + 3]
        pm[i] = (rgba[i] * a + 127) // 255
        pm[i + 1] = (rgba[i + 1] * a + 127) // 255
        pm[i + 2] = (rgba[i + 2] * a + 127) // 255
        pm[i + 3] = a
    sx, sy = w / nw, h / nh
    for dy in range(nh):
        y0, y1 = int(dy * sy), min(int((dy + 1) * sy + 0.999), h)
        for dx in range(nw):
            x0, x1 = int(dx * sx), min(int((dx + 1) * sx + 0.999), w)
            r = g = b = a = 0
            cnt = 0
            for y in range(y0, y1):
                base = y * w * 4
                for x in range(x0, x1):
                    i = base + x * 4
                    r += pm[i]
                    g += pm[i + 1]
                    b += pm[i + 2]
                    a += pm[i + 3]
                    cnt += 1
            r //= cnt
            g //= cnt
            b //= cnt
            a //= cnt
            di = (dy * nw + dx) * 4
            if a > 0:  # unpremultiply
                out[di] = min(255, (r * 255 + a // 2) // a)
                out[di + 1] = min(255, (g * 255 + a // 2) // a)
                out[di + 2] = min(255, (b * 255 + a // 2) // a)
            out[di + 3] = a
    return nw, nh, bytes(out)


def png_chunk(ctype: bytes, payload: bytes) -> bytes:
    crc = zlib.crc32(ctype + payload) & 0xFFFFFFFF
    return len(payload).to_bytes(4, "big") + ctype + payload + crc.to_bytes(4, "big")


def encode_png(w: int, h: int, rgba: bytes) -> bytes:
    stride = w * 4
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # 滤波类型 0（None）
        raw += rgba[y * stride : (y + 1) * stride]
    ihdr = w.to_bytes(4, "big") + h.to_bytes(4, "big") + bytes([8, 6, 0, 0, 0])
    return (
        PNG_SIG
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + png_chunk(b"IEND", b"")
    )


def build_ico(entries) -> bytes:
    """entries: [(size, png_bytes)]。ICO 容器 + PNG 载荷。"""
    header = struct.pack("<HHH", 0, 1, len(entries))
    dir_size = 16 * len(entries)
    offset = 6 + dir_size
    dir_blob = bytearray()
    body = bytearray()
    for size, data in entries:
        b = size if size < 256 else 0
        dir_blob += struct.pack(
            "<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset
        )
        body += data
        offset += len(data)
    return header + bytes(dir_blob) + bytes(body)


def self_check(ico: bytes):
    """重解析一遍：条目数、每个载荷是合法 PNG 且尺寸对得上。"""
    reserved, itype, count = struct.unpack("<HHH", ico[:6])
    assert reserved == 0 and itype == 1, "ICO 头不对"
    sizes = []
    for i in range(count):
        (bw, bh, _, _, planes, bpp, length, off) = struct.unpack(
            "<BBBBHHII", ico[6 + 16 * i : 6 + 16 * (i + 1)]
        )
        w, h = (256 if bw == 0 else bw), (256 if bh == 0 else bh)
        payload = ico[off : off + length]
        assert payload[:8] == PNG_SIG, f"{w}px 条目不是 PNG"
        iw = int.from_bytes(payload[16:20], "big")
        ih = int.from_bytes(payload[20:24], "big")
        assert (iw, ih) == (w, h), f"条目声称 {w}px，实际 PNG 是 {iw}px"
        assert planes == 1 and bpp == 32, "ICONDIRENTRY 的 planes/bpp 不对"
        sizes.append(w)
    return sizes


def main():
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    png = extract_png_bytes(repo)
    w, h, rgba = decode_png(png)
    if (w, h) != (128, 128):
        raise SystemExit(f"appicon 应是 128×128，实际 {w}×{h}")

    # 顺手报一句：debian 的 PNG 跟内嵌的是不是同一张（纯信息，不拦）
    deb = os.path.join(repo, "debian", "icons", "puttyplus.png")
    if os.path.isfile(deb):
        same = open(deb, "rb").read() == png
        print(f"debian/icons/puttyplus.png 与内嵌图{'相同' if same else '不同（以内嵌为准）'}")

    entries = []
    for s in SIZES:
        nw, nh, small = resize_box(rgba, w, h, s, s)
        entries.append((s, encode_png(nw, nh, small)))
    entries.append((128, png))  # 原图字节直接进，零损
    entries.sort(key=lambda e: e[0])

    ico = build_ico(entries)
    out_dir = os.path.join(repo, "assets")
    os.makedirs(out_dir, exist_ok=True)
    out = os.path.join(out_dir, "puttyplus.ico")
    with open(out, "wb") as f:
        f.write(ico)

    sizes = self_check(open(out, "rb").read())
    print(f"写出 {out}（{len(ico)} 字节），条目尺寸：{sizes}")


if __name__ == "__main__":
    sys.exit(main())
