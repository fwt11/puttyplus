//! **二进制内容的 hex 转储**（纯逻辑，好单测）。
//!
//! 干什么用（用户 2026-11-10）：文本编辑器只能打开文本；对**非文本文件**
//! 要先问用户「要不要以 hex 模式打开」⇒ 同意了就用这里的转储显示原始字节。
//!
//! 布局照抄经典的 `hexdump -C`：
//! ```text
//! 00000000  7f 45 4c 46 02 01 01 00  00 00 00 00 00 00 00 00  |.ELF............|
//! ```
//! ⇒ **偏移**（8 位十六进制）+ **十六进制字节**（两组 8 字节，组间两个空格）
//! + **右侧 ASCII**（可打印的原样，其余用 `.`）。

/// 每行放多少字节。
pub const BYTES_PER_LINE: usize = 16;

/// **最多转储多少原始字节**。
///
/// ⚠️ **必须有上限**：hex 转储的文本量是原始数据的 ~4 倍 ⇒ 8 MB 的二进制
/// 会变成 40 MB 的文本 ⇒ `TextView::set_text` 直接卡死。
/// 64 KiB ⇒ 4096 行、约 330 KB 文本 ⇒ 打开就出，不卡。
pub const MAX_DUMP_BYTES: usize = 64 * 1024;

/// 把字节转成 hex 转储文本（每行以 `\n` 结尾）。
///
/// ⚠️ **只转储前 [`MAX_DUMP_BYTES`] 个字节**（多的不转）——理由见那里。
pub fn dump(data: &[u8]) -> String {
    let data = &data[..data.len().min(MAX_DUMP_BYTES)];
    let mut out = String::with_capacity(data.len() / BYTES_PER_LINE * 80 + 16);
    for (row, chunk) in data.chunks(BYTES_PER_LINE).enumerate() {
        // 偏移
        out.push_str(&format!("{:08x}  ", row * BYTES_PER_LINE));
        // 前 8 字节
        for i in 0..8 {
            match chunk.get(i) {
                Some(b) => out.push_str(&format!("{:02x} ", b)),
                // 缺的位置补三个空格 ⇒ 整行对齐
                None => out.push_str("   "),
            }
        }
        // 组间多一个空格（前面每个字节后面已经带了一个）
        out.push(' ');
        // 后 8 字节
        for i in 8..BYTES_PER_LINE {
            match chunk.get(i) {
                Some(b) => out.push_str(&format!("{:02x} ", b)),
                None => out.push_str("   "),
            }
        }
        // 右侧 ASCII
        out.push_str(" |");
        for &b in chunk {
            out.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
        }
        out.push_str("|\n");
    }
    out
}

/// 这次转储**显示了多少**原始字节（文件可能比上限大）。
pub fn shown_bytes(total: usize) -> usize {
    total.min(MAX_DUMP_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空数据转出空串() {
        assert_eq!(dump(&[]), "");
    }

    #[test]
    fn 布局照抄_hexdump_c() {
        let d = dump(b"Hello");
        // 偏移 + 字节 + ASCII
        assert!(d.starts_with("00000000  48 65 6c 6c 6f"), "实际：{d:?}");
        assert!(d.contains("|Hello"), "实际：{d:?}");
        assert!(d.ends_with("|\n"), "实际：{d:?}");
    }

    #[test]
    fn 不足一行也占满一行的位置() {
        let d = dump(b"AB");
        // ⚠️ **ASCII 竖线固定在第 60 列**（0 起）：缺掉的字节要补三个空格
        //    ⇒ 不然短行的竖线会往前挪，看着就散了。
        assert_eq!(d.find('|'), Some(60), "实际：{d:?}");
        assert!(d.contains("|AB"), "实际：{d:?}");
        // 满行的竖线也在同一列
        let full = dump(&[0u8; 16]);
        assert_eq!(full.find('|'), Some(60), "实际：{full:?}");
    }

    #[test]
    fn 十六进制字节是两位小写() {
        let d = dump(&[0x00, 0x0a, 0xff, 0xAB]);
        assert!(d.contains("00 0a ff ab"), "实际：{d:?}");
    }

    #[test]
    fn 不可打印字符在_ascii_列变成点() {
        let d = dump(&[0x00, b'A', 0x1b, b'B']);
        assert!(d.contains("|.A.B"), "实际：{d:?}");
    }

    #[test]
    fn 偏移按行递增() {
        let data = vec![0u8; 40];
        let d = dump(&data);
        let lines: Vec<&str> = d.lines().collect();
        assert_eq!(lines.len(), 3); // 16 + 16 + 8
        assert!(lines[0].starts_with("00000000  "), "实际：{}", lines[0]);
        assert!(lines[1].starts_with("00000010  "), "实际：{}", lines[1]);
        assert!(lines[2].starts_with("00000020  "), "实际：{}", lines[2]);
    }

    #[test]
    fn 超大文件只转储前一段() {
        let data = vec![0x41u8; MAX_DUMP_BYTES + 1000];
        let d = dump(&data);
        let lines = d.lines().count();
        assert_eq!(lines, MAX_DUMP_BYTES / BYTES_PER_LINE);
        // 显示的字节数也报得对
        assert_eq!(shown_bytes(data.len()), MAX_DUMP_BYTES);
    }
}
