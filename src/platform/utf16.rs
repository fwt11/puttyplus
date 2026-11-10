//! ConPTY 的 **UTF-16LE ⇄ UTF-8** 转换（跨平台版 阶段 3）。
//!
//! ## 为什么必须有这一层
//!
//! Windows 的 ConPTY（伪终端）两个方向**都是 UTF-16LE**：
//!
//! - **输出方向**：子进程写的是 UTF-8/ANSI，但 ConPTY 出来的是 **UTF-16LE**
//!   （控制台宿主 API 是 Unicode 的）；
//! - **输入方向**：我们写进管道的字节会被当成 **UTF-16LE** 解释
//!   ⇒ 直接写 UTF-8 的话，中文变成乱码、ASCII 变成「每个字符后面跟个 NUL」。
//!
//! ⚠️ **之前是直接透传原始字节**（`conpty.rs` 的 `read()`/`write()`）⇒
//!    Windows 上走 spawn 路线时屏幕上全是乱码。
//!
//! ## 为什么单独一个文件
//!
//! ⚠️ `conpty.rs` 整个文件是 `#![cfg(target_os = "windows")]` ⇒ 里面的逻辑
//!    **在 Linux 上测不到**。⇒ 把纯逻辑（编解码）挪到这个**不带平台门控**
//!    的文件里 ⇒ Linux 上也能单测（阶段 3 的验收要求）。
//!
//! ## 跨批次的两个坑（都处理了）
//!
//! 1. **一个码元可能横跨两次读**：管道读回来的字节数是任意的 ⇒ 半个 UTF-16
//!    码元（1 字节）要**留着下次**接着拼。
//! 2. **一个代理对（emoji 等）可能横跨两次读**：UTF-16 的代理对是**两个
//!    码元**（高代理 + 低代理）⇒ 末尾那个**孤立的高代理**也要留着下次。

use std::io;

/// UTF-8 字符串 ⇒ **UTF-16LE** 字节（ConPTY 的输入方向）。
///
/// ⚠️ **不是 UTF-16BE**：Windows 是小端 ⇒ 每个码元低字节在前。
pub fn utf8_to_utf16le(s: &str) -> Vec<u8> {
    let mut units: Vec<u16> = Vec::new();
    for ch in s.chars() {
        let mut buf = [0u16; 2];
        units.extend_from_slice(ch.encode_utf16(&mut buf));
    }
    let mut out = Vec::with_capacity(units.len() * 2);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// **UTF-16LE ⇒ UTF-8** 的解码器（ConPTY 的输出方向）。
///
/// ⚠️ **必须是有状态的**：跨批次的半个码元 / 半个代理对要留着下次接着拼
///    （见模块说明的两个坑）。
#[derive(Debug, Default)]
pub struct Utf16LeDecoder {
    /// 攒着的**半个码元**（低字节还没来）。
    half: Option<u8>,
    /// 攒着的**高代理**（代理对的低代理还没来）。
    high: Option<u16>,
}

impl Utf16LeDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 解一批字节 ⇒ UTF-8 文本。
    ///
    /// ⚠️ 半个码元 / 孤立的高代理会被**留着**（下一次 `decode` 接着拼）。
    /// ⚠️ 真正坏掉的字节（不是合法代理对）⇒ `from_utf16_lossy` 换成
    ///    U+FFFD（**不崩、不丢整批**）。
    pub fn decode(&mut self, bytes: &[u8]) -> String {
        let mut units: Vec<u16> = Vec::new();
        // 上次留下的一半先接上
        if let Some(h) = self.high.take() {
            units.push(h);
        }
        let mut buf: Vec<u8> = Vec::with_capacity(bytes.len() + 1);
        if let Some(b) = self.half.take() {
            buf.push(b);
        }
        buf.extend_from_slice(bytes);
        // 字节数是奇数 ⇒ 末尾那个字节是**半个码元** ⇒ 留着下次
        let usable = buf.len() - (buf.len() % 2);
        if usable < buf.len() {
            self.half = Some(buf[usable]);
        }
        for c in buf[..usable].chunks_exact(2) {
            units.push(u16::from_le_bytes([c[0], c[1]]));
        }
        // ⚠️ 末尾若是**孤立的高代理** ⇒ 它的低代理可能在下一次读里
        //    ⇒ 留到下次，别现在就把整个字符变成 U+FFFD。
        if let Some(&last) = units.last() {
            if (0xD800..=0xDBFF).contains(&last) {
                self.high = Some(last);
                units.pop();
            }
        }
        String::from_utf16_lossy(&units)
    }

    /// 还有没有「欠着」的字节/码元（自测用）。
    pub fn has_pending(&self) -> bool {
        self.half.is_some() || self.high.is_some()
    }
}

/// ConPTY 的**输出方向**管道适配器：原始 UTF-16LE 字节 ⇒ UTF-8 字节。
///
/// ⚠️ **为什么要单独一个类型**：解码器有状态（半个码元 / 半个代理对要跨批次
///    留着），而读回来的 UTF-8 可能比调用方的缓冲大 ⇒ 还得留一批「没给完
///    的字节」。两样都归它管。
///
/// ⚠️ **这个类型不带平台门控** ⇒ ConPTY 那套读写逻辑在 **Linux 上也能测**
///    （`conpty.rs` 整个文件是 `#![cfg(target_os = "windows")]`，里面的东西
///    在 Linux 上根本不会被编译）。
#[derive(Debug, Default)]
pub struct Utf16Pipe {
    decoder: Utf16LeDecoder,
    /// 解出来了但**没给出去**的 UTF-8 字节。
    pending: Vec<u8>,
}

impl Utf16Pipe {
    pub fn new() -> Self {
        Self::default()
    }

    /// 把上次没给完的字节先给出去（返回给了多少）。
    pub fn take_pending(&mut self, buf: &mut [u8]) -> usize {
        if self.pending.is_empty() || buf.is_empty() {
            return 0;
        }
        let n = self.pending.len().min(buf.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        n
    }

    /// 喂一批**原始**（UTF-16LE）字节 ⇒ 解成 UTF-8 放进 `buf`。
    ///
    /// 返回放进 `buf` 的字节数。
    ///
    /// ⚠️ 返回 **0** 有两种含义：① 这一批只凑出了半个码元（**要继续读**，
    ///    不能当成 EOF！）；② `buf` 是空的。调用方要区分 ⇒ 用
    ///    [`Utf16Pipe::needs_more`] 判断。
    ///
    /// ⚠️ **新喂进来的字节一个都不能丢**：解出来的文本先**堆进** `pending`，
    ///    再从 `pending` 里往 `buf` 倒 ⇒ `buf` 太小也只会「这次给不完」，
    ///    不会把刚读到的字节弄丢（早期版本在这里直接 return ⇒ 丢数据）。
    pub fn feed(&mut self, raw: &[u8], buf: &mut [u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        let text = self.decoder.decode(raw);
        if !text.is_empty() {
            self.pending.extend_from_slice(text.as_bytes());
        }
        self.take_pending(buf)
    }

    /// 「还要继续读」吗？（这一批只凑出了半个码元 ⇒ 是）。
    ///
    /// ⚠️ **调用方必须区分「0 字节 = 没东西」与「0 字节 = 还要继续读」**
    ///    ⇒ 前者可能是 EOF，后者不是 ⇒ 否则会把半个字符永远卡住。
    pub fn needs_more(&self) -> bool {
        self.decoder.has_pending()
    }
}

/// ConPTY **输出方向**的读循环（纯逻辑 ⇒ **Linux 上也能单测**）。
///
/// - `read_raw`：从管道读**原始**（UTF-16LE）字节的回调（ConPTY 里就是读
///   那个匿名管道）。
/// - `buf`：交给调用方的 **UTF-8** 缓冲。
/// - `pipe`：解码状态（跨批次的半个码元 / 半个代理对）。
///
/// 返回写进 `buf` 的字节数；**0 = EOF**（对端关闭）。
///
/// ⚠️ **「给不出东西」≠ EOF**：这一批只凑出了半个码元 / 半个代理对 ⇒
///    必须**继续读**（否则半个字符永远卡住，屏幕上还少一截）。
///
/// ⚠️ **为什么抽出来**：`conpty.rs` 整个文件是 `#![cfg(target_os="windows")]`
///    ⇒ 那边的读循环在 Linux 上**根本不会被编译** ⇒ 改错了也只能等
///    Windows CI 才暴露。抽到这里 ⇒ Linux 上用假 reader 就能测。
pub fn read_decoded<F>(
    read_raw: &mut F,
    buf: &mut [u8],
    pipe: &mut Utf16Pipe,
) -> io::Result<usize>
where
    F: FnMut(&mut [u8]) -> io::Result<usize>,
{
    if buf.is_empty() {
        return Ok(0);
    }
    // ① 上次解出来没给完的字节 ⇒ 先给出去
    let n = pipe.take_pending(buf);
    if n > 0 {
        return Ok(n);
    }
    let mut raw = [0u8; 8192];
    loop {
        // ② 从管道读原始字节（**UTF-16LE**）
        let n = loop {
            match read_raw(&mut raw) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // 对端关了 ⇒ 当成 EOF（上层按 0 收尾）。
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                Err(e) => return Err(e),
            }
        };
        if n == 0 {
            return Ok(0);
        }
        // ③ 解码成 UTF-8
        let n = pipe.feed(&raw[..n], buf);
        if n > 0 {
            return Ok(n);
        }
        // ⚠️ **给不出东西** ⇒ 这一批只凑出了半个码元 / 半个代理对
        //    ⇒ **继续读**（**不能当成 EOF**！否则半个字符永远卡住，
        //    屏幕上还少一截）。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ASCII：每个字符一个码元，低字节在前。
    #[test]
    fn ascii_编成_utf16le() {
        assert_eq!(utf8_to_utf16le("A"), vec![0x41, 0x00]);
        assert_eq!(utf8_to_utf16le("Hi"), vec![0x48, 0x00, 0x69, 0x00]);
    }

    /// 中文：一个 BMP 字符一个码元（**不是** UTF-8 的三个字节）。
    #[test]
    fn 中文编成_utf16le() {
        // 「中」= U+4E2D ⇒ 小端 2D 4E
        assert_eq!(utf8_to_utf16le("中"), vec![0x2D, 0x4E]);
        // 「中」+「文」
        assert_eq!(utf8_to_utf16le("中文"), vec![0x2D, 0x4E, 0x87, 0x65]);
    }

    /// emoji：代理对（两个码元）。
    #[test]
    fn emoji_编成代理对() {
        // 「😀」= U+1F600 ⇒ 高代理 D83D + 低代理 DE00 ⇒ 小端 3D D8 00 DE
        assert_eq!(utf8_to_utf16le("😀"), vec![0x3D, 0xD8, 0x00, 0xDE]);
    }

    /// 解回来一样（往返）。
    #[test]
    fn 编解码往返() {
        for s in ["", "hello", "中文测试", "a中b😀c", "line1\r\nline2"] {
            let bytes = utf8_to_utf16le(s);
            let mut d = Utf16LeDecoder::new();
            assert_eq!(d.decode(&bytes), s, "往返失败：{s:?}");
            assert!(!d.has_pending());
        }
    }

    /// ⚠️ **半个码元跨批次**：第一次读只拿到低字节（奇数长度）。
    #[test]
    fn 半个码元跨批次也能拼回来() {
        let bytes = utf8_to_utf16le("中");
        let mut d = Utf16LeDecoder::new();
        // 先给 1 个字节（半个码元）
        assert_eq!(d.decode(&bytes[..1]), "");
        assert!(d.has_pending(), "半个码元应当被留着");
        // 再给剩下的那个字节
        assert_eq!(d.decode(&bytes[1..]), "中");
        assert!(!d.has_pending());
    }

    /// ⚠️ **代理对跨批次**：高代理在第一批，低代理在第二批。
    #[test]
    fn 代理对跨批次也能拼回来() {
        let bytes = utf8_to_utf16le("😀");
        let mut d = Utf16LeDecoder::new();
        // 第一批只有高代理（2 字节）
        assert_eq!(d.decode(&bytes[..2]), "", "高代理不该现在就输出");
        assert!(d.has_pending(), "高代理应当被留着");
        // 第二批来了低代理
        assert_eq!(d.decode(&bytes[2..]), "😀");
        assert!(!d.has_pending());
    }

    /// ⚠️ **代理对正好被切成两半**（4 字节 ⇒ 2+2）。
    #[test]
    fn 代理对被切成两半也能拼回来() {
        let bytes = utf8_to_utf16le("😀");
        let mut d = Utf16LeDecoder::new();
        assert_eq!(d.decode(&bytes[..2]), "");
        assert_eq!(d.decode(&bytes[2..]), "😀");
    }

    /// 真坏掉的字节 ⇒ U+FFFD，**不崩、不丢整批**。
    #[test]
    fn 坏字节变成替换字符但不崩() {
        let mut d = Utf16LeDecoder::new();
        // 0xD800 是高代理，后面没有低代理 ⇒ 这一批它被留着
        assert_eq!(d.decode(&[0x00, 0xD8]), "");
        // ⚠️ 接下来这个**不是**低代理（0x0041 = 'A'）⇒ 高代理配不上
        //    ⇒ 整对都不合法 ⇒ U+FFFD（但后面的 'A' 还在，不丢整批）
        let got = d.decode(&[0x41, 0x00]);
        assert!(got.contains('\u{FFFD}'), "应当是替换字符，实际 {got:?}");
        assert!(got.ends_with('A'), "后面的正常字符不该丢，实际 {got:?}");
    }

    /// ⚠️ **孤立的低代理**（0xDC00 单独出现）⇒ U+FFFD。
    #[test]
    fn 孤立低代理变成替换字符() {
        let mut d = Utf16LeDecoder::new();
        let got = d.decode(&[0x00, 0xDC]);
        assert_eq!(got, "\u{FFFD}");
    }

    /// 一次喂很多字节（常见的读循环）⇒ 结果正确。
    #[test]
    fn 一次喂很多字节() {
        let s = "第一行\r\n第二行 中文\r\n";
        let bytes = utf8_to_utf16le(s);
        let mut d = Utf16LeDecoder::new();
        assert_eq!(d.decode(&bytes), s);
    }

    /// 空的输入 ⇒ 空输出，不留东西。
    #[test]
    fn 空输入() {
        let mut d = Utf16LeDecoder::new();
        assert_eq!(d.decode(&[]), "");
        assert!(!d.has_pending());
    }

    /* ---- Utf16Pipe（ConPTY 的输出方向） ---- */

    /// 正常的一批：原始 UTF-16LE ⇒ UTF-8。
    #[test]
    fn 管道_喂一批解成_utf8() {
        let raw = utf8_to_utf16le("hello 中文");
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let n = pipe.feed(&raw, &mut buf);
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "hello 中文");
        assert!(!pipe.needs_more());
    }

    /// ⚠️ **只喂半个码元** ⇒ 这次给不出东西，但**要继续读**。
    #[test]
    fn 管道_半个码元要继续读() {
        let raw = utf8_to_utf16le("中");
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        assert_eq!(pipe.feed(&raw[..1], &mut buf), 0, "半个码元给不出东西");
        assert!(pipe.needs_more(), "不能当成 EOF");
        // 再喂剩下的那个字节 ⇒ 出字了
        let n = pipe.feed(&raw[1..], &mut buf);
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "中");
        assert!(!pipe.needs_more());
    }

    /// ⚠️ **缓冲太小** ⇒ 给不下的字节留在 pending 里，下次接着给。
    #[test]
    fn 管道_缓冲太小剩下的留下次() {
        let raw = utf8_to_utf16le("abcdef");
        let mut pipe = Utf16Pipe::new();
        let mut small = [0u8; 2];
        let n = pipe.feed(&raw, &mut small);
        assert_eq!(n, 2);
        assert_eq!(std::str::from_utf8(&small[..n]).unwrap(), "ab");
        // 剩下的一次给不完 ⇒ 分两次
        let mut buf = [0u8; 64];
        let mut got = String::new();
        got.push_str(std::str::from_utf8(&small[..n]).unwrap());
        loop {
            let n = pipe.feed(&[], &mut buf);
            if n == 0 {
                break;
            }
            got.push_str(std::str::from_utf8(&buf[..n]).unwrap());
        }
        assert_eq!(got, "abcdef");
    }

    /// ⚠️ **代理对跨批次**：高代理一批、低代理一批 ⇒ 中间不能吐半个字符。
    #[test]
    fn 管道_代理对跨批次() {
        let raw = utf8_to_utf16le("😀");
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        assert_eq!(pipe.feed(&raw[..2], &mut buf), 0, "只有高代理 ⇒ 给不出");
        assert!(pipe.needs_more());
        let n = pipe.feed(&raw[2..], &mut buf);
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "😀");
    }

    /// ⚠️ **一批里既有完整字符又有半个代理对** ⇒ 完整的先给，半个留着。
    #[test]
    fn 管道_一半完整一半代理对() {
        let mut raw = utf8_to_utf16le("A");
        // ⚠️ **只取高代理那半**（低代理不在这一批里）⇒ 那个 emoji 现在
        //    还拼不出来 ⇒ 只能先给 "A"，emoji 等下一批。
        raw.extend_from_slice(&utf8_to_utf16le("😀")[..2]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let n = pipe.feed(&raw, &mut buf);
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "A");
        assert!(pipe.needs_more(), "末尾的高代理应当留着");
        // 低代理来了 ⇒ 补上那个 emoji
        let n = pipe.feed(&utf8_to_utf16le("😀")[2..], &mut buf);
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "😀");
    }

    /* ---- read_decoded（ConPTY 的输出方向读循环） ---- */

    /// 假的管道读端：按给定顺序返回每批字节，批次用完 ⇒ EOF（返回 0）。
    struct FakeReader {
        chunks: Vec<Vec<u8>>,
        idx: usize,
        /// 读到最后一批之后要不要报「对端已关」（BrokenPipe）。
        broken: bool,
    }

    impl FakeReader {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                chunks,
                idx: 0,
                broken: false,
            }
        }

        fn broken_pipe(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                chunks,
                idx: 0,
                broken: true,
            }
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.idx >= self.chunks.len() {
                if self.broken {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                return Ok(0); // EOF
            }
            let c = &self.chunks[self.idx];
            self.idx += 1;
            let n = c.len().min(buf.len());
            buf[..n].copy_from_slice(&c[..n]);
            Ok(n)
        }
    }

    /// 正常：一批读完 ⇒ UTF-8。
    #[test]
    fn 读循环_一批读完() {
        let mut r = FakeReader::new(vec![utf8_to_utf16le("hello 中文")]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let n = read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap();
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "hello 中文");
    }

    /// ⚠️ **半个码元跨两次读** ⇒ **不能当成 EOF**，要接着读。
    ///
    /// ⚠️ 这是 `read_decoded` 存在的**全部理由**：ConPTY 的管道读回来的
    ///    字节数是任意的 ⇒ 半个码元横跨两次读是**常态**，不是边角。
    #[test]
    fn 读循环_半个码元要接着读() {
        let raw = utf8_to_utf16le("中");
        let mut r = FakeReader::new(vec![raw[..1].to_vec(), raw[1..].to_vec()]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let n = read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap();
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "中");
        assert!(!pipe.needs_more());
    }

    /// ⚠️ **代理对跨两次读** ⇒ 中间不能吐半个字符。
    #[test]
    fn 读循环_代理对跨批次() {
        let raw = utf8_to_utf16le("😀");
        let mut r = FakeReader::new(vec![raw[..2].to_vec(), raw[2..].to_vec()]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let n = read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap();
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "😀");
    }

    /// **一次读多批** ⇒ 每批都给出去（不攒着）。
    #[test]
    fn 读循环_多批依次给出去() {
        let mut r = FakeReader::new(vec![utf8_to_utf16le("第一行"), utf8_to_utf16le("第二行")]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let mut got = String::new();
        loop {
            let n = read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap();
            if n == 0 {
                break;
            }
            got.push_str(std::str::from_utf8(&buf[..n]).unwrap());
        }
        assert_eq!(got, "第一行第二行");
    }

    /// **EOF** ⇒ 返回 0（上层据此收尾）。
    #[test]
    fn 读循环_到文件尾返回零() {
        let mut r = FakeReader::new(vec![]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        assert_eq!(read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap(), 0);
    }

    /// ⚠️ **对端已关（BrokenPipe）** ⇒ 也当成 EOF（返回 0，不报错）。
    #[test]
    fn 读循环_对端已关当文件尾() {
        let mut r = FakeReader::broken_pipe(vec![]);
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        assert_eq!(read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap(), 0);
    }

    /// ⚠️ **缓冲太小** ⇒ 这次给不完的留在 pending 里，下次接着给。
    #[test]
    fn 读循环_缓冲太小剩下次给() {
        let mut r = FakeReader::new(vec![utf8_to_utf16le("abcdef")]);
        let mut pipe = Utf16Pipe::new();
        let mut small = [0u8; 2];
        let n = read_decoded(&mut |b| r.read(b), &mut small, &mut pipe).unwrap();
        assert_eq!(n, 2);
        assert_eq!(std::str::from_utf8(&small[..n]).unwrap(), "ab");
        // 剩下的 ⇒ 下次读循环一开始就给出来（pending 优先）
        let mut buf = [0u8; 64];
        let n = read_decoded(&mut |b| r.read(b), &mut buf, &mut pipe).unwrap();
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), "cdef");
    }

    /// ⚠️ **别的错误要往上抛**（别一律吞成 EOF ⇒ 那样会静默丢数据）。
    #[test]
    fn 读循环_别的错误往上抛() {
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 64];
        let mut r = |_b: &mut [u8]| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let e = read_decoded(&mut r, &mut buf, &mut pipe).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }
}
