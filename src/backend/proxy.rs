//! ProxyCommand 支持（对应 OpenSSH 的 `-o ProxyCommand`）：
//! 展开 %h/%p/%r 令牌后用 `sh -c` 拉起代理命令，把它的 stdin/stdout
//! 包装成 AsyncRead+AsyncWrite 流喂给 russh 的 `connect_stream`。

use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};
use crate::i18n::tr;
use crate::trf;

/// 展开代理命令中的 OpenSSH 令牌：%h=主机 %p=端口 %r=用户。
fn expand(cmd: &str, host: &str, port: u16, user: &str) -> String {
    cmd.replace("%h", host)
        .replace("%p", &port.to_string())
        .replace("%r", user)
}

pub struct ProxyStream {
    rx: ChildStdout,
    tx: ChildStdin,
    _child: Child,
    stderr: Arc<Mutex<String>>,
}

impl ProxyStream {
    /// 拉起代理进程并返回流。失败时错误信息尽量带上代理进程的 stderr。
    pub async fn spawn(
        cmd: &str,
        host: &str,
        port: u16,
        user: &str,
    ) -> Result<ProxyStream, String> {
        let expanded = expand(cmd, host, port, user);
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&expanded)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| trf!("代理命令启动失败（{expanded}）: {e}", expanded = expanded, e = e))?;

        let mut child = child;
        let rx = child
            .stdout
            .take()
            .ok_or_else(|| tr("代理命令无 stdout").to_string())?;
        let tx = child
            .stdin
            .take()
            .ok_or_else(|| tr("代理命令无 stdin").to_string())?;
        let stderr_pipe = child.stderr.take().ok_or_else(|| tr("代理命令无 stderr").to_string())?;

        // 收集代理进程 stderr，连接失败时给用户看原因（代理工具都往 stderr 写诊断）
        let stderr = Arc::new(Mutex::new(String::new()));
        {
            let sink = Arc::clone(&stderr);
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut err = stderr_pipe;
                let mut buf = Vec::new();
                // 读到 EOF；进程由 kill_on_drop 兜底回收
                let _ = err.read_to_end(&mut buf).await;
                if let Ok(text) = String::from_utf8(buf) {
                    *sink.lock().unwrap() = text;
                }
            });
        }

        Ok(ProxyStream {
            rx,
            tx,
            _child: child,
            stderr,
        })
    }

    /// 代理进程已输出的诊断文本（截断到末尾 500 字符）。
    pub fn stderr_tail(&self) -> String {
        let g = self.stderr.lock().unwrap();
        let s = g.trim();
        if s.chars().count() <= 500 {
            s.to_string()
        } else {
            s.chars().skip(s.chars().count() - 500).collect()
        }
    }
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.rx).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.tx).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.tx).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        // 关闭写端即可让代理进程收到 EOF 而退出（kill_on_drop 兜底）
        Pin::new(&mut self.tx).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_expansion() {
        assert_eq!(
            expand("nc -X connect -x gw:8080 %h %p", "10.1.1.1", 2222, "ops"),
            "nc -X connect -x gw:8080 10.1.1.1 2222"
        );
        assert_eq!(
            expand("ssh -W %h:%p %r@jump", "h", 22, "u"),
            "ssh -W h:22 u@jump"
        );
        // 无令牌原样保留
        assert_eq!(expand("nc host 22", "h", 22, "u"), "nc host 22");
    }
}
