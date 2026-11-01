//! 非协议（非 SSH）连接的终端桥：VTE 的子进程是本程序自己
//! （`--bridge-telnet host port` / `--bridge-serial 设备 波特率`），
//! 桥在 PTY stdio 与 TCP/串口之间逐字节搬运。
//!
//! 好处：终端侧机制（autofeed/搜索/滚动/动态标题）对 telnet/串口
//! 与 ssh 完全一致，无需为 VTE 手写输入输出通路。

use std::io::{Read, Write};
use std::sync::mpsc;

/// 最小 Telnet IAC 处理：剥掉协商序列并把应答写入 reply。
/// 语义：对端 WILL → 我 DONT；对端 DO → 我 WONT（不开任何选项）；
/// WONT/DONT 不应答；IAC IAC = 数据 255；SB..SE 子协商整段丢弃。
/// 足够对付绝大多数行模式设备（交换机/串口服务器）。
fn strip_iac(buf: &[u8], out: &mut Vec<u8>, reply: &mut Vec<u8>) {
    let mut i = 0;
    while i < buf.len() {
        let b = buf[i];
        if b != 255 {
            out.push(b);
            i += 1;
            continue;
        }
        let Some(&next) = buf.get(i + 1) else {
            break; // IAC 截断在包尾，等下一包
        };
        match next {
            255 => {
                out.push(255); // 转义的数据 0xFF
                i += 2;
            }
            251 | 253 => {
                // WILL(251) → 回 DONT(254)；DO(253) → 回 WONT(252)
                if let Some(&opt) = buf.get(i + 2) {
                    reply.push(255);
                    reply.push(if next == 251 { 254 } else { 252 });
                    reply.push(opt);
                    i += 3;
                } else {
                    break;
                }
            }
            252 | 254 => {
                // WONT/DONT：不应答
                i += if buf.get(i + 2).is_some() { 3 } else { 2 };
                if i > buf.len() {
                    i = buf.len();
                }
            }
            250 => {
                // SB(250) ... IAC(255) SE(240)：子协商整段丢弃
                let mut j = i + 2;
                while j < buf.len() {
                    if buf[j] == 240 && buf[j - 1] == 255 {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            _ => {
                // 其余单参数命令（241-249）双字节，丢弃
                i += 2;
            }
        }
    }
}

/// telnet 桥：连接 host:port，stdin ↔ 剥 IAC 后的 socket。
fn run_telnet_bridge(host: &str, port: &str) -> Result<(), String> {
    use std::net::TcpStream;
    let port: u16 = port.parse().map_err(|_| "端口非法".to_string())?;
    let stream =
        TcpStream::connect((host, port)).map_err(|e| format!("连接 {host}:{port} 失败: {e}"))?;
    let mut sock_read = stream.try_clone().map_err(|e| e.to_string())?;
    let sock_w_reply = stream.try_clone().map_err(|e| e.to_string())?;
    let mut sock_w_main = stream;

    // socket → 剥 IAC → stdout；应答经 channel 交给写线程
    let (rtx, rrx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut stdout = std::io::stdout().lock();
        loop {
            match sock_read.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut out = Vec::with_capacity(n);
                    let mut reply = Vec::new();
                    strip_iac(&buf[..n], &mut out, &mut reply);
                    if !out.is_empty() && stdout.write_all(&out).is_err() {
                        break;
                    }
                    let _ = stdout.flush();
                    if !reply.is_empty() && rtx.send(reply).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 应答写线程
    std::thread::spawn(move || {
        let mut w = sock_w_reply;
        while let Ok(r) = rrx.recv() {
            if w.write_all(&r).is_err() {
                break;
            }
            let _ = w.flush();
        }
    });

    // stdin（PTY）→ socket
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 4096];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if sock_w_main.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = sock_w_main.flush();
            }
            Err(_) => break,
        }
    }
    Ok(())
}

/// 串口桥：打开设备（按 -sercfg 参数组），stdin ↔ 串口逐字节搬运。
fn run_serial_bridge(dev: &str, sercfg: &str) -> Result<(), String> {
    use serialport::{DataBits, FlowControl, Parity, StopBits};
    let conf = crate::args::parse_sercfg(sercfg);
    let port = serialport::new(dev, conf.baud)
        .data_bits(match conf.data {
            7 => DataBits::Seven,
            6 => DataBits::Six,
            5 => DataBits::Five,
            _ => DataBits::Eight,
        })
        .parity(match conf.parity {
            'e' => Parity::Even,
            'o' => Parity::Odd,
            _ => Parity::None,
        })
        .stop_bits(match conf.stop {
            2 => StopBits::Two,
            _ => StopBits::One,
        })
        .flow_control(match conf.flow {
            'x' => FlowControl::Software,
            'r' => FlowControl::Hardware,
            _ => FlowControl::None,
        })
        .timeout(std::time::Duration::from_millis(200))
        .open()
        .map_err(|e| format!("打开串口 {dev}@{} 失败: {e}", conf.baud))?;
    let mut rx = port.try_clone().map_err(|e| e.to_string())?;
    let mut tx = port;

    // 串口 → stdout（独立线程，短超时轮询）
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut out = std::io::stdout().lock();
        loop {
            match rx.read(&mut buf) {
                Ok(0) => continue, // 读超时
                Ok(n) => {
                    if out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = out.flush();
                }
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::TimedOut {
                        break;
                    }
                }
            }
        }
    });

    // stdin → 串口
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 4096];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = tx.flush();
            }
            Err(_) => break,
        }
    }
    Ok(())
}

/// 入口：按桥模式运行（参数来自 main 的分发）。返回 Ok(false) 表示不是桥模式。
pub fn run_if_bridge(args: &[String]) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("--bridge-telnet") => {
            if args.len() < 3 {
                return Err("用法: --bridge-telnet <host> <port>".into());
            }
            run_telnet_bridge(&args[1], &args[2])?;
            Ok(true)
        }
        Some("--bridge-serial") => {
            if args.len() < 3 {
                return Err("用法: --bridge-serial <设备> [sercfg]".into());
            }
            let sercfg = args.get(2).map(String::as_str).unwrap_or("115200,8,n,1");
            run_serial_bridge(&args[1], sercfg)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::strip_iac;

    #[test]
    fn iac_stripped_and_replied() {
        let mut out = Vec::new();
        let mut reply = Vec::new();
        // 数据 + WILL ECHO(0) + SB 子协商 + 数据 + DO NAWS(31) + IAC IAC(转义)
        let input = [
            b'a', b'b', 255, 251, 0, 255, 250, 24, 0, 255, 240, b'c', 255, 253, 31, 255, 255,
        ];
        strip_iac(&input, &mut out, &mut reply);
        assert_eq!(out, vec![b'a', b'b', b'c', 255]);
        // WILL → DONT；DO → WONT
        assert_eq!(reply, vec![255, 254, 0, 255, 252, 31]);
    }

    #[test]
    fn plain_data_passthrough() {
        let mut out = Vec::new();
        let mut reply = Vec::new();
        strip_iac(b"hello", &mut out, &mut reply);
        assert_eq!(out, b"hello");
        assert!(reply.is_empty());
    }
}
