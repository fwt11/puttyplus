//! PTY（伪终端）会话（跨平台版 阶段 C0 起，Windows 分支见阶段 C4）。
//!
//! ## 为什么要有这一层
//!
//! 终端会话是「**跑一个 ssh 子进程，把它的输出接到终端上**」：
//! - Linux：`forkpty`（VTE 自带的 `vte::Pty`）；
//! - Windows：**ConPTY**（阶段 C4）——Windows 上**没有** forkpty，
//!   跑交互式子进程必须走 ConPTY。
//!
//! 这两者的 API 差得远 ⇒ 统一收口在这里，应用层（`term.rs`）只管
//! 「起会话、读输出、写输入、问它是不是 raw 了」。
//!
//! ## Linux 上的两个坑（都保留原有行为）
//!
//! 1. **spawn 之前要把 pty 交给终端控件**（`set_pty`）——VTE 自己读 master。
//! 2. **pty 的 master 与 slave 共用同一份 termios**：ssh 认证通过后把
//!    slave 切到 raw，master 的 `c_lflag` 同步变了 ⇒ 拿 master fd 查就行。

use std::io;

/// 一个 PTY 会话（持有 master 端）。
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct PtySession {
    pty: vte4::Pty,
}

/// 一个 PTY 会话。
#[cfg(not(target_os = "linux"))]
#[derive(Clone)]
pub struct PtySession {
    _priv: (),
}

#[cfg(target_os = "linux")]
impl PtySession {
    /// 开一个新的 PTY。
    pub fn new() -> io::Result<Self> {
        let pty = vte4::Pty::new_sync(vte4::PtyFlags::empty(), None::<&gtk::gio::Cancellable>)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self { pty })
    }

    /// VTE 后端要用的那个 `vte::Pty`（**只有 VTE 后端能调**）。
    #[cfg(target_os = "linux")]
    pub fn vte_pty(&self) -> &vte4::Pty {
        &self.pty
    }

    /// 起子进程（异步）。回调拿到子进程 pid。
    pub fn spawn_async(
        &self,
        cwd: &str,
        argv: &[&str],
        env: &[&str],
        cb: impl FnOnce(Result<glib::Pid, String>) + 'static,
    ) {
        self.pty.spawn_async(
            Some(cwd),
            argv,
            env,
            gtk::glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            None::<&gtk::gio::Cancellable>,
            move |res| cb(res.map_err(|e| e.to_string())),
        );
    }

    /// 本地行规程是不是已经切到 **raw 模式**（ICANON / ECHO 关掉）。
    ///
    /// ssh **认证通过之后**才切；那之前往里写字节会被**本地回显**
    /// （屏幕上凭空多一行命令）并**缓冲**，等切了 raw 才转发给远端 ——
    /// 那会儿远端可能还停在登录流程里，命令就被吃掉了。
    pub fn is_raw(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut tio: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY：`tio` 是本函数栈上的零初始化结构；fd 由 pty 持有且有效；
            // `tcgetattr` 只往里写，不读未初始化内存。
            let rc = unsafe { libc::tcgetattr(self.pty.fd().as_raw_fd(), &mut tio) };
            if rc != 0 {
                // 取不到 ⇒ 当作还没切 raw（宁可多等，也别写出本地回显的噪声）
                return false;
            }
            tio.c_lflag & libc::ICANON == 0 && tio.c_lflag & libc::ECHO == 0
        }
        #[cfg(not(unix))]
        {
            // ConPTY 没有本地行规程 ⇒ 不存在「本地回显 + 缓冲」
            true
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl PtySession {
    /// 开一个新的 PTY。
    ///
    /// ⚠️ **非 Linux 平台的 PTY 还没实现**（阶段 C4：Windows 走 ConPTY，
    /// macOS 走 Unix PTY）。现在直接报错，**不假装能用**。
    pub fn new() -> io::Result<Self> {
        Err(io::Error::other(
            "本平台还没有 PTY 实现（阶段 C4 落地 ConPTY）",
        ))
    }

    /// 起子进程（异步）。
    pub fn spawn_async(
        &self,
        _cwd: &str,
        _argv: &[&str],
        _env: &[&str],
        cb: impl FnOnce(Result<glib::Pid, String>) + 'static,
    ) {
        cb(Err("本平台还没有 PTY 实现（阶段 C4 落地 ConPTY）".into()));
    }

    /// 行规程状态（非 Linux 恒为 true：没有本地行规程）。
    pub fn is_raw(&self) -> bool {
        true
    }
}
