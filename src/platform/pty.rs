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
//!
//! ## 「谁读 master」这件事，两个后端两种做法（阶段 C4）
//!
//! - **VTE 后端**：VTE 自己读 master（`set_pty` 之后）⇒ **不要**调
//!   [`PtySession::spawn_reader`]，否则两边抢着读，输出会被撕成两半。
//! - **ghostty 后端**：ghostty 只管解析字节，**不碰 PTY** ⇒ 得由我们
//!   [`PtySession::spawn_reader`] 起线程读，再喂给终端。

use std::io;

/// 一个 PTY 会话（持有 master 端）。
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct PtySession {
    pty: vte4::Pty,
}

/// 一个 PTY 会话。
#[cfg(windows)]
#[derive(Clone)]
pub struct PtySession {
    con: std::sync::Arc<crate::platform::conpty::ConPty>,
}

/// 一个 PTY 会话（其它平台：还没实现）。
#[cfg(not(any(target_os = "linux", windows)))]
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

    /// 写给子进程（用户敲的键）。
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let fd = self.pty.fd().as_raw_fd();
        let mut off = 0usize;
        while off < bytes.len() {
            // SAFETY：fd 由 pty 持有且有效；buf 是本函数的切片。
            let n = unsafe {
                libc::write(
                    fd,
                    bytes[off..].as_ptr() as *const libc::c_void,
                    bytes.len() - off,
                )
            };
            if n > 0 {
                off += n as usize;
                continue;
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        Ok(())
    }

    /// 起一个线程读 master，把读到的字节交给回调。
    ///
    /// ⚠️ **只有 ghostty 后端才调**（VTE 自己读 master，两边抢着读
    /// 会把输出撕成两半）。
    pub fn spawn_reader(&self, on_data: Box<dyn Fn(&[u8]) + Send + 'static>) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let fd = self.pty.fd().as_raw_fd();
        std::thread::Builder::new()
            .name("pp-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    // ⚠️⚠️ **master fd 是非阻塞的**（VTE 建 pty 时就这么设的，
                    // 因为它自己是用事件轮询读的）⇒ 直接 `read` 会立刻返回
                    // **EAGAIN**，一个字节都读不到。
                    // ⇒ 先 `poll` 等有数据（或挂断）再读。
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY：pfd 是栈上的结构；poll 只往 revents 写。
                    let rc = unsafe { libc::poll(&mut pfd, 1, -1) };
                    if rc < 0 {
                        let err = io::Error::last_os_error();
                        if err.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                        continue;
                    }
                    // SAFETY：fd 由 pty 持有（会话还活着），buf 是栈上数组。
                    let n =
                        unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                    if n > 0 {
                        on_data(&buf[..n as usize]);
                        continue;
                    }
                    if n == 0 {
                        // 对端关了 ⇒ 结束
                        break;
                    }
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted
                        || err.kind() == io::ErrorKind::WouldBlock
                    {
                        continue;
                    }
                    // ⚠️ Linux 上**子进程退出后读 master 得到 EIO**（不是 EOF）
                    // —— 这是正常现象，当作结束即可。
                    break;
                }
            })
            .map(|_| ())
    }

    /// 设置 pty 的窗口大小（列 × 行）。
    ///
    /// ⚠️ **远端 ssh 靠它知道屏幕多大**（不发窗口大小的 ssh 会一直按
    /// 80×24 排版）。VTE 后端不用管（VTE 自己设），ghostty 后端必须
    /// 自己在尺寸变化时调。
    pub fn set_size(&self, cols: u16, rows: u16) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let fd = self.pty.fd().as_raw_fd();
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY：`ws` 是栈上的零初始化结构；fd 有效。
        let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl PtySession {
    /// 开一个新的 ConPTY 会话。
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            con: std::sync::Arc::new(crate::platform::conpty::ConPty::new(80, 24)?),
        })
    }

    /// 起子进程（**同步**：ConPTY 的 `CreateProcess` 本身就是同步的）。
    ///
    /// ⚠️ 回调照样是异步风格（`spawn_async` 这个名字是历史包袱）——
    /// 上层（`term.rs`）两种都接得住。
    pub fn spawn_async(
        &self,
        cwd: &str,
        argv: &[&str],
        env: &[&str],
        cb: impl FnOnce(Result<glib::Pid, String>) + 'static,
    ) {
        let res = self
            .con
            .spawn(cwd, argv, env)
            .map(|pid| glib::Pid::from_raw(pid as i32));
        cb(res.map_err(|e| e.to_string()));
    }

    /// 行规程状态（ConPTY 没有本地行规程 ⇒ 恒为 true）。
    pub fn is_raw(&self) -> bool {
        true
    }

    /// 写给子进程（用户敲的键）。
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.con.write(bytes)
    }

    /// 起一个线程读子进程的输出。
    pub fn spawn_reader(&self, on_data: Box<dyn Fn(&[u8]) + Send + 'static>) -> io::Result<()> {
        let con = self.con.clone();
        std::thread::Builder::new()
            .name("pp-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match con.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => on_data(&buf[..n]),
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map(|_| ())
    }

    /// 设置窗口大小。
    pub fn set_size(&self, cols: u16, rows: u16) -> io::Result<()> {
        self.con.resize(cols, rows)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
impl PtySession {
    /// 开一个新的 PTY。
    ///
    /// ⚠️ **非 Linux/Windows 平台的 PTY 还没实现**（macOS 走 Unix PTY）。
    /// 现在直接报错，**不假装能用**。
    pub fn new() -> io::Result<Self> {
        Err(io::Error::other(
            "本平台还没有 PTY 实现（Windows 走 ConPTY，macOS 待做）",
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
        cb(Err("本平台还没有 PTY 实现（Windows 走 ConPTY，macOS 待做）".into()));
    }

    /// 行规程状态（非 Linux 恒为 true：没有本地行规程）。
    pub fn is_raw(&self) -> bool {
        true
    }

    /// 写给子进程。
    pub fn write(&self, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::other("本平台还没有 PTY 实现"))
    }

    /// 读子进程输出。
    pub fn spawn_reader(&self, _on_data: Box<dyn Fn(&[u8]) + Send + 'static>) -> io::Result<()> {
        Err(io::Error::other("本平台还没有 PTY 实现"))
    }

    /// 设置窗口大小。
    pub fn set_size(&self, _cols: u16, _rows: u16) -> io::Result<()> {
        Err(io::Error::other("本平台还没有 PTY 实现"))
    }
}
