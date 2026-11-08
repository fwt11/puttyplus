//! 进程内 SSH 的**交互终端通道**（跨平台版 阶段 C7）。
//!
//! ## 为什么要有这个
//!
//! 终端会话历来是「**spawn 一个 ssh 子进程**，把它的输出接到终端上」
//! （Linux 上是 `ssh`，Windows 上是 `ssh` 或 `plink`）。
//!
//! ⚠️ **Windows 上这条路走不通**：
//! - Windows **默认没有 `ssh.exe`**（OpenSSH 是「可选功能」，很多机器
//!   根本没装）⇒ 终端直接报「找不到可用的客户端程序」（用户 2026-11-08
//!   报的截图）；
//! - `plink` 是 PuTTY 的东西，**我们不打算依赖它**（用户明确要求）。
//!
//! ⇒ 改成**自己实现 SSH 客户端**：用 russh（本来就是依赖——SFTP 面板
//!   那条链路就是它）开一条 session channel：
//!   - 要交互 shell ⇒ `request_pty` + `request_shell`；
//!   - 要跑远程命令 ⇒ `request_pty` + `exec`；
//!   - 键盘字节 ⇒ 写进 channel；远程字节 ⇒ 喂给终端内核；
//!   - 窗口尺寸变化 ⇒ `window_change`。
//!
//! ⚠️ **不需要本地 pty**（这是它比「自己编一个 ssh.exe 子进程」好的
//! 地方）：没有本地 pty ⇒ 没有本地回显、没有本地行规程、没有
//! 「等 ssh 切 raw 模式」那一套 ⇒ 键盘字节**直接**进远程，
//! 尺寸也**直接**由界面告诉远程（不用轮询控制台）。
//!
//! ## 认证
//!
//! 复用 [`crate::backend::conn::connect_session`]（与 SFTP 面板**同一段
//! 代码**）⇒ 密码 / 指定私钥 / agent / 默认私钥的顺序与行为两边一致。
//!
//! ## 与 Linux 主线的关系
//!
//! ⚠️ **Linux 主线仍然是 spawn 真 `ssh`**（AGENTS.md 的「主线零回归」
//! 红线：终端里跑的是用户自己的 ssh ⇒ `~/.ssh/config`、agent、
//! ProxyJump、known_hosts 的语义全都在，Windows 版做不到这个）。
//! ⇒ 进程内这条通路**只在 Windows 上默认启用**；Linux 上可以用
//!    `PP_TERM_SSH=inproc` 强制打开（**自测用**，且要配合 ghostty
//!    后端——VTE 没有「无 pty」这条路）。

use std::sync::Arc;

use russh::client::Handle;
use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

use crate::backend::conn::HostKeyHandler;
use crate::backend::{BackendError, BackendResult};

/// 发给远程的终端类型（`TERM`）。
///
/// ⚠️ 用 `xterm-256color`：绝大多数 Linux 发行版与堡垒机都认它
/// （256 色 + 完整的键盘协议）；`xterm` 太老（16 色），`screen`/`tmux`
/// 会让远程少一块功能（鼠标模式）。
pub const TERM_TYPE: &str = "xterm-256color";

/// 通道里出来的事件。
pub enum ShellEvent {
    /// 远程来的字节（喂给终端内核）。
    Data(Vec<u8>),
    /// 远程退出了（带退出码）。
    Exit(i32),
}

/// 进程内 SSH 的交互通道。
///
/// ## 线程模型
///
/// - **写端**（`Arc<ChannelWriteHalf>`）：任何线程都能写 ⇒ 键盘输入
///   走 GTK 主线程没问题（`send` / `resize` 内部把异步调用丢给
///   `rt` 那个 tokio 运行时）。
/// - **读端**（`tokio::sync::Mutex<ChannelReadHalf>`）：**只有一个读者**
///   （后台任务循环 `next_event`）⇒ 用异步锁而不是 `&mut self`，
///   这样读写两端能分给两个不同的线程。
#[derive(Clone)]
pub struct ShellChannel {
    write: Arc<ChannelWriteHalf<Msg>>,
    read: Arc<tokio::sync::Mutex<ChannelReadHalf>>,
    /// 建通道时所在的 tokio 运行时（写操作要在它上面发起）。
    rt: tokio::runtime::Handle,
}

impl ShellChannel {
    /// 开一条**交互 shell**。
    ///
    /// ⚠️ `pty = false` 对应命令行的 `-T`（不要 pty）：远程不分配终端
    /// ⇒ 交互式程序（vim/top）会报错，但脚本/管道能跑。
    pub async fn open_shell(
        session: &Handle<HostKeyHandler>,
        cols: u16,
        rows: u16,
        pty: bool,
    ) -> BackendResult<Self> {
        let chan = session
            .channel_open_session()
            .await
            .map_err(|e| BackendError(format!("打开 SSH 通道失败: {e}")))?;
        if pty {
            Self::setup(&chan, cols, rows).await?;
        }
        chan.request_shell(true)
            .await
            .map_err(|e| BackendError(format!("启动远程 shell 失败: {e}")))?;
        Ok(Self::from_channel(chan))
    }

    /// 开一条**跑远程命令**的通道（命令丢给远程 shell 执行）。
    pub async fn open_exec(
        session: &Handle<HostKeyHandler>,
        cmd: &str,
        cols: u16,
        rows: u16,
        pty: bool,
    ) -> BackendResult<Self> {
        let chan = session
            .channel_open_session()
            .await
            .map_err(|e| BackendError(format!("打开 SSH 通道失败: {e}")))?;
        if pty {
            Self::setup(&chan, cols, rows).await?;
        }
        chan.exec(true, cmd)
            .await
            .map_err(|e| BackendError(format!("执行远程命令失败: {e}")))?;
        Ok(Self::from_channel(chan))
    }

    /// 只开一条**空通道**（不开 pty、不开 shell）。
    ///
    /// 干什么用：命令行 `-N`（不要 shell）——连接建立、转发照跑，
    /// 但**不**起远程 shell（终端里是空白的，这是 `-N` 的语义）。
    pub async fn open_bare(session: &Handle<HostKeyHandler>) -> BackendResult<Self> {
        let chan = session
            .channel_open_session()
            .await
            .map_err(|e| BackendError(format!("打开 SSH 通道失败: {e}")))?;
        Ok(Self::from_channel(chan))
    }

    /// 申请 pty（远程要有个终端才能正确排版）。
    async fn setup(chan: &Channel<Msg>, cols: u16, rows: u16) -> BackendResult<()> {
        chan.request_pty(true, TERM_TYPE, u32::from(cols), u32::from(rows), 0, 0, &[])
            .await
            .map_err(|e| BackendError(format!("申请远程 pty 失败: {e}")))
    }

    /// 把通道拆成读写两半。
    fn from_channel(chan: Channel<Msg>) -> Self {
        let (read, write) = chan.split();
        Self {
            write: Arc::new(write),
            read: Arc::new(tokio::sync::Mutex::new(read)),
            rt: tokio::runtime::Handle::current(),
        }
    }

    /// **写给远程**（用户敲的键）。
    ///
    /// ⚠️ russh 的写是异步的，而调用方在 GTK 主线程 ⇒ 丢给建通道时
    /// 那个 tokio 运行时去做，**不等它**（终端输入不能被网络卡住）。
    pub fn send(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let write = Arc::clone(&self.write);
        let data = bytes.to_vec();
        self.rt.spawn(async move {
            let _ = write.data_bytes(data).await;
        });
    }

    /// **窗口尺寸变了** ⇒ 告诉远程（`window_change`）。
    ///
    /// ⚠️ 不发的话，远程会一直按最初的尺寸排版 ⇒ 画面看着正常其实
    /// 错位（跟 Linux 上「pty 尺寸不同步」是同一个毛病）。
    pub fn resize(&self, cols: u16, rows: u16) {
        let write = Arc::clone(&self.write);
        self.rt.spawn(async move {
            let _ = write
                .window_change(u32::from(cols), u32::from(rows), 0, 0)
                .await;
        });
    }

    /// 读下一个事件（**阻塞到有数据或通道关闭**）。
    ///
    /// ⚠️ 只有一个读者（后台任务循环调它）⇒ 读端用异步锁保护。
    pub async fn next_event(&self) -> Option<ShellEvent> {
        let mut read = self.read.lock().await;
        let mut exit_code: Option<i32> = None;
        loop {
            match read.wait().await? {
                ChannelMsg::Data { data } => return Some(ShellEvent::Data(data.to_vec())),
                // stderr 一般不用（交互 shell 里混着跑才有意义）⇒ 并进 stdout
                ChannelMsg::ExtendedData { data, .. } => {
                    return Some(ShellEvent::Data(data.to_vec()))
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    // ⚠️ 退出码**先记下**、不等它就返回：exit-status 之后
                    // 可能还有数据没发完（真 PuTTY 也是等 EOF 才收尾）。
                    exit_code = Some(exit_status as i32);
                }
                ChannelMsg::Eof | ChannelMsg::Close => {
                    return Some(ShellEvent::Exit(exit_code.unwrap_or(0)))
                }
                _ => {}
            }
        }
    }

    /// 当前通道所在的那个 tokio 运行时（后台读循环要在它上面跑）。
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.rt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 终端类型必须是**服务器普遍认的**那个。
    ///
    /// ⚠️ 这条断言是「防回归」用的：一旦有人改成 `xterm`（16 色）或
    /// `screen`（少功能），远程的颜色/鼠标行为会变 ⇒ 现象是「Windows
    /// 版终端颜色不对」，极难定位。
    #[test]
    fn 终端类型是_xterm_256color() {
        assert_eq!(TERM_TYPE, "xterm-256color");
    }

    /// 事件枚举能安全地跨线程传（后台任务 → GTK 主线程）。
    #[test]
    fn 事件是send的() {
        fn assert_send<T: Send>() {}
        assert_send::<ShellEvent>();
        assert_send::<ShellChannel>();
        fn assert_sync<T: Sync>() {}
        assert_sync::<ShellChannel>();
    }
}
