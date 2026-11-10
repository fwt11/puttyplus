//! ConPTY 的 UTF-16LE ⇄ UTF-8 管道适配器冒烟（阶段 3）。
//!
//! ## 为什么要有它
//!
//! ⚠️ `src/platform/conpty.rs` 整个文件是 `#![cfg(target_os = "windows")]`
//!    ⇒ 那边的读写循环在 Linux 上**根本不会被编译** ⇒ 改错了只能等
//!    Windows CI 才暴露（一次半小时）。
//!
//! ⇒ 这里用一条**真实的 TCP 管道**（Windows 上也是跨平台的）当 ConPTY 的
//!    替身，跑的是**同一套**代码：
//!    - 输出方向：`utf16::read_decoded` + `Utf16Pipe`（ConPTY 的 `read()` 用的
//!      就是它）；
//!    - 输入方向：`utf16::utf8_to_utf16le`（ConPTY 的 `write()` 用的就是它）。
//!
//! ## 断言
//!
//! 1. **输出方向**：对面写 UTF-16LE ⇒ 这边读回来是**完整的 UTF-8**
//!    （中文 + emoji 都要对）；
//! 2. ⚠️ **半个码元跨读**：读端**每次只读 1 个字节**（模拟管道读的任意
//!    切分）⇒ 中文的两个字节被拆开也要能拼回来；
//! 3. **输入方向**：这边把 UTF-8 编成 UTF-16LE ⇒ 对面收到的**原始字节**
//!    必须是 UTF-16LE（**不是** UTF-8）—— 这一条钉的就是「以前直接透传
//!    UTF-8 ⇒ Windows 上满屏乱码」那个 bug；
//! 4. ⚠️ **编码真的有区别**：把 UTF-8 字节当 UTF-16LE 解，结果**不等于**
//!    原文（说明第 3 条不是碰巧蒙对）。
//!
//! ```text
//! cargo run --example utf16_pipe_smoke
//! ```

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

use puttyplus::platform::utf16::{Utf16Pipe, read_decoded, utf8_to_utf16le};

/// 起一条「假 ConPTY 管道」：对面把收到的东西**原样回显**（像 ConPTY 那样
/// 把子进程的输出送回来）。
///
/// 返回：本地这一头 + 一个句柄（对面一共收到了哪些字节）。
fn fake_pipe() -> (TcpStream, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("监听 127.0.0.1");
    let port = listener.local_addr().expect("本地地址").port();
    let h = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("接受连接");
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match sock.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    all.extend_from_slice(&buf[..n]);
                    // 回显（ConPTY 就是这么把输出送回来的）
                    if sock.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        all
    });
    let sock = TcpStream::connect(("127.0.0.1", port)).expect("连接");
    (sock, h)
}

fn main() {
    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ---------- 1 + 2. 输出方向：UTF-16LE ⇒ UTF-8 ----------
    {
        let (mut sock, peer) = fake_pipe();
        let text = "第一行 中文\r\n第二行 😀\r\n";
        let raw = utf8_to_utf16le(text);
        // ⚠️ **故意分两半写**（模拟管道把一个码元切成两半）。
        let mid = raw.len() / 2;
        sock.write_all(&raw[..mid]).expect("写前半");
        sock.write_all(&raw[mid..]).expect("写后半");
        sock.flush().expect("flush");

        // 读端**每次只读 1 个字节** ⇒ 半个码元必然跨两次读。
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 256];
        let mut got = String::new();
        while got.len() < text.len() {
            let n = read_decoded(
                &mut |b| {
                    // 只给 1 个字节的空间 ⇒ 读端必然切成一小块一小块
                    let m = b.len().min(1);
                    match sock.read(&mut b[..m]) {
                        Ok(k) => Ok(k),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(0),
                        Err(e) => Err(e),
                    }
                },
                &mut buf,
                &mut pipe,
            )
            .expect("读");
            if n == 0 {
                break;
            }
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        check!(
            "输出方向：UTF-16LE ⇒ UTF-8（半个码元跨读也要拼回来）",
            got == text,
            format!("期望 {text:?}，实际 {got:?}")
        );
        check!(
            "读完没有欠着的字节（不留半个字符）",
            !pipe.needs_more(),
            "解码器还欠着东西".to_string()
        );
        drop(sock);
        drop(peer.join());
    }

    // ---------- 3. 输入方向：UTF-8 ⇒ UTF-16LE ----------
    {
        let (mut sock, peer) = fake_pipe();
        let enc = utf8_to_utf16le("中文");
        sock.write_all(&enc).expect("写");
        sock.flush().expect("flush");
        // 对面把收到的东西回显回来 ⇒ 我们读到的是**它眼里的原始字节**。
        let mut back = vec![0u8; enc.len()];
        sock.read_exact(&mut back).expect("读回显");
        check!(
            "输入方向：写出去的是 UTF-16LE（不是 UTF-8）",
            back == vec![0x2Du8, 0x4E, 0x87, 0x65],
            format!("期望 [2d 4e 87 65]，实际 {:x?}", back)
        );
        check!(
            "输入方向：字节数是 UTF-8 的两倍（一个汉字两个字节）",
            back.len() == 4,
            format!("实际 {} 字节", back.len())
        );
        drop(sock);
        drop(peer.join());
    }

    // ---------- 4. 编码真的有区别（不是碰巧蒙对） ----------
    {
        // 把 UTF-8 字节当 UTF-16LE 解 ⇒ 出来的东西**不是**原文。
        let utf8_bytes = "中文".as_bytes().to_vec();
        let mut pipe = Utf16Pipe::new();
        let mut buf = [0u8; 256];
        let n = pipe.feed(&utf8_bytes, &mut buf);
        let got = String::from_utf8_lossy(&buf[..n]).to_string();
        check!(
            "UTF-8 字节不能当 UTF-16LE 用（所以编码这层是必需的）",
            got != "中文",
            format!("竟然解出了原文：{got:?}")
        );
    }

    if fails.is_empty() {
        println!("utf16_pipe smoke ok");
    } else {
        println!("utf16_pipe smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
