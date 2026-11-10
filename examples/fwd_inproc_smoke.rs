//! **进程内转发承接命令行 `-L`** 的端到端冒烟。
//! 运行：PP_TERM_SSH=inproc cargo run --example fwd_inproc_smoke
//!
//! 用户 2026-11-10：「端口映射和数据库连接是我们这个项目的特色，Windows
//! 版本也要实现」。
//!
//! ## 为什么要这么测
//!
//! Windows 上**终端默认走进程内 SSH**（`term::use_inproc_ssh()`）⇒ 命令行的
//! `-L/-R/-D` **不能**交给 ssh 的 argv（Windows 上压根没有 ssh 进程）⇒ 必须
//! 由程序内的转发引擎（`backend::forward`）承接。
//!
//! ⚠️ 这条路在 **Linux 上也能测**：把 `PP_TERM_SSH=inproc` 设上 ⇒ 终端那条
//! 决策就走 inproc ⇒ 转发承接方与 Windows 完全一致 ⇒ 不用等 Windows CI。
//!
//! 冒烟做三件事：
//!   1. 起一个**本地回显服务**（转发链路的终点）
//!   2. 用带 `-L` 的会话连本机测试 sshd ⇒ 等转发端口起来
//!   3. 连**本地转发端口** ⇒ 数据要能原样回来 ⇒ 证明 direct-tcpip 通道
//!      真的通了（不是"端口在监听但不通"）
//!
//! 需要本机测试 sshd（127.0.0.1:2233）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{PanelState, SftpPanel};

fn pump(n: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..n {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(40);
    }
    f()
}

/// 本地测试 sshd 的连接参数。
fn test_opts() -> ConnOptions {
    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    });
    let key = std::env::var("PP_SSH_KEY")
        .unwrap_or_else(|_| format!("{}/.ssh/id_rsa", std::env::var("HOME").unwrap_or_default()));
    ConnOptions {
        proto: "ssh".into(),
        host,
        port,
        user,
        keyfile: Some(key),
        ..Default::default()
    }
}

/// 起一个**回显服务**（转发链路的终点）：收到什么就原样吐回来。
fn spawn_echo_server(port: u16) -> std::thread::JoinHandle<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("绑定回显服务端口");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    })
}

/// 连本地转发端口，把一行字发过去，看它是不是原样回来。
fn echo_through(port: u16, payload: &str) -> Result<String, String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("连不上：{e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    s.write_all(payload.as_bytes()).map_err(|e| format!("写失败：{e}"))?;
    s.flush().map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1024];
    let n = s.read(&mut buf).map_err(|e| format!("读失败：{e}"))?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    // ⚠️ **必须**走 inproc：这条决策决定转发由谁承接（与 Windows 同一条路）
    if !puttyplus::term::use_inproc_ssh() {
        eprintln!(
            "⚠️ 这个冒烟必须配 PP_TERM_SSH=inproc 跑（当前是 spawn 路线 ⇒ \
             转发归 ssh 的 argv，不走进程内这条）"
        );
        std::process::exit(2);
    }

    const ECHO_PORT: u16 = 19099;
    const FWD_PORT: u16 = 18080;
    let _echo = spawn_echo_server(ECHO_PORT);

    let mut opts = test_opts();
    // ⚠️ **命令行 `-L` 的内部形**（与 `ConnOptions.forwards` 同形）：
    // `L:监听端口:目标主机:目标端口`
    opts.forwards = vec![format!("L:{FWD_PORT}:127.0.0.1:{ECHO_PORT}")];

    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);
    pump(20);

    let connected = wait_until(Duration::from_secs(30), || st.conn_ok.get());
    assert!(
        connected,
        "SFTP 连接没建立（本地 sshd 在 {}:{} 吗？）",
        opts.host, opts.port
    );
    println!("[1] 已连上 {}@{}:{}", opts.user, opts.host, opts.port);

    // ---- 等转发端口真的起来 ----
    let up = wait_until(Duration::from_secs(20), || {
        TcpStream::connect(("127.0.0.1", FWD_PORT)).is_ok()
    });
    assert!(
        up,
        "转发端口 {FWD_PORT} 没起来 ⇒ 进程内转发没有承接命令行的 -L"
    );
    println!("[2] 转发端口 {FWD_PORT} 已监听（进程内承接了命令行的 -L）");

    // ---- 数据要能真的往返（direct-tcpip 通道通了）----
    let payload = "PP_FWD_ALIVE_9527";
    let got = echo_through(FWD_PORT, payload);
    let got = got.expect("通过转发端口读写失败");
    assert_eq!(
        got, payload,
        "通过转发端口回来的数据不对（实际 {got:?}）⇒ 通道没真的通"
    );
    println!("[3] 数据经转发端口原样往返：{got}");

    println!("\n✔ 进程内转发承接命令行的 -L：端口起来了、数据真的通");
}
