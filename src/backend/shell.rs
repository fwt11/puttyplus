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
//! ⚠️ **Linux 主线（2026-11-13，阶段 B2 之后）默认也走这条**：VTE
//! 后端用**假 pty**（`platform::pty::FakePty`）也能把键盘字节送进
//! 通道 ⇒ 终端与文件面板、端口映射、数据库工作台**共用同一条连接**。
//! ⇒ **代价**：并入之后终端**不走用户自己的 ssh** ⇒ `~/.ssh/config`、
//!    agent、ProxyJump 的语义**都不在** ⇒ 需要的话切回 spawn
//!    （首选项 →「终端」页 →「终端连接方式」，或 `PP_TERM_SSH=spawn`）。

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

/// 收到 EOF 之后，**最多再等多久**去拿 `exit-status`。
///
/// ⚠️ **为什么要这个上限**：exit-status 排在 EOF 后面（见 `next_event`
///   的说明）⇒ 得等它；但万一服务器只发 EOF 不发 close ⇒ 不能把调用方
///   挂死 ⇒ 2 秒足够（同一台机器上毫秒级就到）。
const EXIT_AFTER_EOF: std::time::Duration = std::time::Duration::from_secs(2);

/// 通道里出来的事件。
pub enum ShellEvent {
    /// 远程来的字节（喂给终端内核）。
    Data(Vec<u8>),
    /// 远程退出了（带退出码）。
    Exit(i32),
}

/// 通道消息 ⇒ 事件的翻译结果。
enum Step {
    /// 这条消息自己不带事件 ⇒ 继续等。
    Wait,
    /// 有数据要往外送。
    Data(Vec<u8>),
    /// 通道收尾了（带退出码）。
    Done(i32),
}

/// 通道消息的**状态机**（纯逻辑 ⇒ 好测）。
///
/// ⚠️ **为什么要抽成纯函数**：退出码那个坑**极隐蔽**（exit-status 排在
///   EOF 后面 ⇒ 早退一步就永远拿不到退出码）⇒ 抽出来才能用单测钉住
///   （真服务器上的验证在 `examples/docker_ui_smoke.rs` 的 A 组）。
#[derive(Default)]
struct ExitTracker {
    /// 退出码（`exit-status` 到达时记下）。
    code: Option<i32>,
    /// 见过 EOF 了（⇒ 之后只等一小会儿，别把调用方挂死）。
    eof_seen: bool,
}

impl ExitTracker {
    /// 处理一条通道消息。
    fn on_msg(&mut self, msg: &ChannelMsg) -> Step {
        match msg {
            ChannelMsg::Data { data } => Step::Data(data.to_vec()),
            // stderr 一般不用（交互 shell 里混着跑才有意义）⇒ 并进 stdout
            ChannelMsg::ExtendedData { data, .. } => Step::Data(data.to_vec()),
            ChannelMsg::ExitStatus { exit_status } => {
                // ⚠️ 退出码**先记下**、不等它就返回：exit-status 之后
                // 可能还有数据没发完（真 PuTTY 也是等 EOF 才收尾）。
                self.code = Some(*exit_status as i32);
                Step::Wait
            }
            ChannelMsg::Eof => {
                // ⚠️ **不是结束**：exit-status 排在 EOF 后面（见 `next_event`）。
                self.eof_seen = true;
                Step::Wait
            }
            ChannelMsg::Close => Step::Done(self.code()),
            _ => Step::Wait,
        }
    }

    /// 手上的退出码（没拿到就是 0）。
    fn code(&self) -> i32 {
        self.code.unwrap_or(0)
    }
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
    /// 通道**已经收尾**了（退出码已经报过一次）⇒ 之后的 `next_event`
    /// 一律返回 `None`。
    ///
    /// ⚠️ **为什么需要它**：`next_event` 现在会**先报一次退出码**再收尾
    ///   （见那里的说明）⇒ 同一个通道只能报一次 ⇒ 第二次开始要返回
    ///   `None`，否则调用方的 `while let Some(ev) = …` 会**死循环**
    ///   （每次都拿到一个 `Exit`）。
    done: Arc<std::sync::atomic::AtomicBool>,
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
            .map_err(|e| BackendError(crate::trf!("打开 SSH 通道失败: {e}", e = e)))?;
        if pty {
            Self::setup(&chan, cols, rows).await?;
        }
        chan.request_shell(true)
            .await
            .map_err(|e| BackendError(crate::trf!("启动远程 shell 失败: {e}", e = e)))?;
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
            .map_err(|e| BackendError(crate::trf!("打开 SSH 通道失败: {e}", e = e)))?;
        if pty {
            Self::setup(&chan, cols, rows).await?;
        }
        chan.exec(true, cmd)
            .await
            .map_err(|e| BackendError(crate::trf!("执行远程命令失败: {e}", e = e)))?;
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
            .map_err(|e| BackendError(crate::trf!("打开 SSH 通道失败: {e}", e = e)))?;
        Ok(Self::from_channel(chan))
    }

    /// 申请 pty（远程要有个终端才能正确排版）。
    async fn setup(chan: &Channel<Msg>, cols: u16, rows: u16) -> BackendResult<()> {
        chan.request_pty(true, TERM_TYPE, u32::from(cols), u32::from(rows), 0, 0, &[])
            .await
            .map_err(|e| BackendError(crate::trf!("申请远程 pty 失败: {e}", e = e)))
    }

    /// 把通道拆成读写两半。
    fn from_channel(chan: Channel<Msg>) -> Self {
        let (read, write) = chan.split();
        Self {
            write: Arc::new(write),
            read: Arc::new(tokio::sync::Mutex::new(read)),
            rt: tokio::runtime::Handle::current(),
            done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    /// **主动关掉通道**（停流式输出时用）。
    ///
    /// ⚠️ **为什么必须有**：russh 的通道**析构时不会**发 close ⇒ 只把
    ///    通道丢掉的话，远端的命令（`docker logs -f`）会**继续挂着**
    ///    （它还在往一个没人读的通道里写）⇒ 白白占着远端的进程。
    ///    显式 close ⇒ 远端收到 EOF/SIGPIPE ⇒ 命令自己退出 ✓。
    ///
    /// ⚠️ 异步的（要发 SSH 消息）⇒ 这里**不等**：丢给建通道时那个
    ///    tokio 运行时去做（与 `send` / `resize` 同一个套路）。
    pub fn close(&self) {
        let write = Arc::clone(&self.write);
        self.rt.spawn(async move {
            let _ = write.close().await;
        });
    }

    /// 读下一个事件（**阻塞到有数据或通道关闭**）。
    ///
    /// ⚠️ 只有一个读者（后台任务循环调它）⇒ 读端用异步锁保护。
    ///
    /// ## ⚠️ 退出码：**收到 EOF 不算结束**（2026-11-16 实测踩出来的坑）
    ///
    /// OpenSSH 发包的顺序是 **EOF(96) → exit-status(98) → close(97)**
    /// （用 `ssh -vvv` 抓的包序）⇒ **exit-status 排在 EOF 后面**。
    ///
    /// ⇒ 以前这里「一收到 EOF 就返回退出码」⇒ **退出码永远是 0**
    /// （exit-status 还没送到就被丢掉了）⇒ 凡是靠退出码判断成败的地方
    /// 全都失灵：
    /// - `docker.rs` 的 `run_cmd` ⇒ `code != 0` 从来不成立；
    /// - 阶段 113 的 `find_shell` ⇒ 「挨个试哪个 shell 能用」**根本没试**
    ///   （探测永远「成功」）⇒ 容器里没有 `/bin/sh` 也会说「有」；
    /// - 终端退出时显示的状态码。
    ///
    /// ⇒ 现在：**EOF 只是「不再有数据了」的信号**，**继续等**
    /// `exit-status` / `close`；等到了才带真实退出码收尾。
    ///
    /// ⚠️ **兜底**：万一服务器只发 EOF 不发 close（不常见，但存在）⇒
    ///   EOF 之后**最多再等 2 秒** ⇒ 免得调用方永远挂着。
    ///
    /// ⚠️ **退出码只报一次**：报完就把 `done` 置上 ⇒ 之后再调返回 `None`
    ///   （否则调用方的 `while let Some(ev) = …` 会死循环）。
    pub async fn next_event(&self) -> Option<ShellEvent> {
        use std::sync::atomic::Ordering;
        let mut read = self.read.lock().await;
        if self.done.load(Ordering::SeqCst) {
            return None;
        }
        let mut tracker = ExitTracker::default();
        loop {
            // ⚠️ EOF 之后只再等一会儿（等 exit-status）⇒ 别把调用方挂死。
            let msg = if tracker.eof_seen {
                match tokio::time::timeout(EXIT_AFTER_EOF, read.wait()).await {
                    Ok(Some(m)) => m,
                    // 超时了（服务器没给退出码）⇒ 就按手上的信息收尾。
                    Ok(None) | Err(_) => {
                        self.done.store(true, Ordering::SeqCst);
                        return Some(ShellEvent::Exit(tracker.code()));
                    }
                }
            } else {
                match read.wait().await {
                    Some(m) => m,
                    // 通道没了（服务器关了它 / 整条 SSH 断了）⇒ 收尾。
                    None => {
                        self.done.store(true, Ordering::SeqCst);
                        return Some(ShellEvent::Exit(tracker.code()));
                    }
                }
            };
            match tracker.on_msg(&msg) {
                Step::Wait => continue,
                Step::Data(b) => return Some(ShellEvent::Data(b)),
                Step::Done(code) => {
                    self.done.store(true, Ordering::SeqCst);
                    return Some(ShellEvent::Exit(code));
                }
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

    /// ⚠️ **回归钉**：退出码排在 **EOF 后面**（服务端发包顺序实测：
    ///   EOF → exit-status → close）⇒ **收到 EOF 不能收尾**。
    ///
    /// ⚠️ **这条为什么值钱**：早退一步（EOF 就返回）⇒ 退出码**永远是 0**
    ///   ⇒ `run_cmd` 的 `code != 0` 从来不成立 ⇒ 「挨个试哪个 shell 能用」
    ///   根本没试 ⇒ 而且**看不出来**（探测永远「成功」）。
    #[test]
    fn 退出码排在eof后面也得拿到() {
        let mut t = ExitTracker::default();
        // EOF：**不是结束** ⇒ 继续等。
        assert!(matches!(t.on_msg(&ChannelMsg::Eof), Step::Wait));
        // exit-status：记下来，但**还不收尾**（后面可能还有数据）。
        assert!(matches!(
            t.on_msg(&ChannelMsg::ExitStatus { exit_status: 7 }),
            Step::Wait
        ));
        // close：这时候才收尾，退出码是 7。
        match t.on_msg(&ChannelMsg::Close) {
            Step::Done(code) => assert_eq!(code, 7, "退出码没带对"),
            _ => panic!("close 应当收尾"),
        }
    }

    /// 退出码排在 **close 后面**的服务器（少见）⇒ 收尾时按 0 算。
    #[test]
    fn 没有退出码就按零收尾() {
        let mut t = ExitTracker::default();
        match t.on_msg(&ChannelMsg::Close) {
            Step::Done(code) => assert_eq!(code, 0),
            _ => panic!("close 应当收尾"),
        }
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
