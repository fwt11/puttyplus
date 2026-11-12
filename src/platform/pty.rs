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

/// 子进程标识（**平台原生**）。
///
/// ⚠️ **为什么不能直接用 `glib::Pid`**：GLib 的 `GPid` 在 **Unix 上是
/// `c_int`**，在 **Windows 上是 `*mut c_void`**（GLib 在 Windows 上把 pid
/// 当指针存）⇒ 同一个 `glib::Pid` 在两个平台上**内部类型不一样**
/// ⇒ 任何 `glib::Pid(整数)` 的写法在 Windows 上都编不过（E0308）。
///
/// ⇒ Linux 上就是 `glib::Pid`（VTE 的 `spawn_async` 直接给这个）；
///    别的平台自己用 `u32` 表示（ghostty 后端不看它 ⇒ 值无所谓）。
#[cfg(target_os = "linux")]
pub type ChildPid = glib::Pid;

/// 子进程标识（**平台原生**）—— 非 Linux 平台见上面的说明。
#[cfg(not(target_os = "linux"))]
pub type ChildPid = u32;

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
        // ⚠️ **程序名要在开 pty 之前设上**：VTE 0.80 的 `SpawnContext` 里
        // `m_systemd_scope` **默认 true** ⇒ 每 spawn 一个子进程都会走
        // 「挪进 systemd user scope」那条路 ⇒ 那儿要 `g_get_prgname()`
        // ⇒ 没设过就是 NULL ⇒ `g_utf8_make_valid(NULL)` ⇒
        // **每开一个终端都喷一条 GLib-CRITICAL**。
        //
        // ⇒ 放在这里而不是只放在 `main` 里：**所有**开 pty 的路径
        // （主程序 + 各个冒烟示例）都从这里过 ⇒ 一处设，处处生效。
        // 详见 `crate::util::init_prgname`。
        crate::util::init_prgname();
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
        cb: impl FnOnce(Result<ChildPid, String>) + 'static,
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

/* ---------------- 假 pty（阶段 B2） ---------------- */

/// **假 pty**（阶段 B2）：VTE 后端接**进程内 SSH**（russh）用。
///
/// ## 为什么需要它
///
/// VTE 的键盘输入**必须**经由一个 pty（`vte_terminal_feed_child` 把字节
/// 写进 pty 的 **master**）⇒ 进程内 SSH 那条路**没有子进程**、也就没有
/// pty ⇒ 键盘字节无处可去。
///
/// ⚠️ 这就是「**VTE 用不了进程内 SSH**」的根本原因（阶段 C7 留下的坑）：
///    ghostty 后端不需要 pty（它自己管字节），VTE 需要。
///
/// ⇒ 自己开**一对** fd（`posix_openpt` + `grantpt` + `unlockpt` +
/// `ptsname_r` + `open`）：
///
/// - **master 交给 VTE**（`vte4::Pty::foreign_sync`）⇒ 它往里写键盘
///   字节、从里面读显示内容；
/// - **slave 留给我们** ⇒ 从里面读出来的就是「用户敲的键」⇒ 转给 SSH。
///
/// ## 方向（别搞反）
///
/// - **键盘**：VTE 写 **master** ⇒ 行规程的**输入**处理 ⇒ 我们从
///   **slave** 读出来；
/// - **显示**：远程来的字节**直接**喂给 VTE（`vte_terminal_feed`），
///   **不**走 slave ⇒ 也就不会有「OPOST 把 `\n` 变成 `\r\n`」的二次
///   转换（真 ssh 也是这么干的：它把本地 tty 切成 raw）。
///
/// ## 行规程
///
/// slave 侧必须切成 **raw**（`cfmakeraw`），否则三个坑：
///
/// - `ICANON` 不关 ⇒ 敲一个键要等回车才送到远程；
/// - `ECHO` 不关 ⇒ 本地回显一遍、远程再回显一遍 = **双回显**；
/// - `ISIG` 不关 ⇒ Ctrl-C 变成本地信号（本地没有前台进程组，信号
///   没人收 ⇒ 远程根本收不到中断）。
///
/// ⚠️ **只能建在 GTK 主线程**（`vte4::Pty::foreign_sync` 内部有
///    `assert_initialized_main_thread!`）⇒ 别在单测里建（单测跑在
///    工作线程上 ⇒ 会 panic）⇒ 用 `examples/vte_inproc_smoke.rs` 验。
#[cfg(target_os = "linux")]
pub struct FakePty {
    /// VTE 接管的那一半（master）。
    vte: vte4::Pty,
    /// 我们这一半（slave）+ 停止标志（读线程用）。
    inner: std::sync::Arc<FakeInner>,
}

/// 假 pty 的**可共享部分**（读线程拿一份 `Arc` ⇒ 主体被丢了也不影响它）。
#[cfg(target_os = "linux")]
struct FakeInner {
    /// slave 端（读键盘字节）。
    slave: std::os::fd::OwnedFd,
    /// 让读线程收工（单独套一层 `Arc` ⇒ 别的线程也能拿一份）。
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(target_os = "linux")]
impl FakePty {
    /// 开一对 pty，把 **master 交给 VTE**。
    pub fn new() -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        let (master, slave) = open_pty_pair()?;
        set_raw_termios(slave.as_raw_fd())?;
        // ⚠️ **master 交给 VTE 之后就不能再自己关它**（VTE finalize 时关）。
        let vte = vte4::Pty::foreign_sync(master, None::<&gtk::gio::Cancellable>)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            vte,
            inner: std::sync::Arc::new(FakeInner {
                slave,
                stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }),
        })
    }

    /// VTE 那边要用的那个 `vte::Pty`（**master**）。
    pub fn vte_pty(&self) -> &vte4::Pty {
        &self.vte
    }

    /// 起一个线程读 **slave**（= VTE 写进来的键盘字节）⇒ 回调。
    ///
    /// ⚠️ 回调在**后台线程**里跑 ⇒ 别碰 GTK 的东西（要动就发事件回
    ///    主线程）。
    pub fn spawn_input_reader(
        &self,
        on_data: Box<dyn Fn(&[u8]) + Send + 'static>,
    ) -> io::Result<()> {
        let inner = std::sync::Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name("pp-fake-pty-in".into())
            .spawn(move || {
                use std::os::fd::AsRawFd;
                let fd = inner.slave.as_raw_fd();
                let mut buf = [0u8; 4096];
                loop {
                    if inner.stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY：pfd 是栈上的结构；poll 只往 revents 写。
                    let rc = unsafe { libc::poll(&mut pfd, 1, 200) };
                    if rc < 0 {
                        let err = io::Error::last_os_error();
                        if err.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if rc == 0 {
                        // 超时 ⇒ 回去看 stop 标志（别在 poll 里死等）
                        continue;
                    }
                    if pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                        continue;
                    }
                    // SAFETY：fd 由 `inner` 里的 OwnedFd 持有（线程活着它就
                    // 活着）；buf 是栈上数组。
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
                    // ⚠️ **master 关了 ⇒ 读 slave 得到 EIO**（不是 EOF）
                    // —— 正常现象，当作结束即可。
                    break;
                }
            })
            .map(|_| ())
    }

    /// 让读线程收工（会话结束时调）。
    ///
    /// ⚠️ **不是立刻停**（读线程最多 200ms 才醒一次）⇒ 只是别再往下读。
    pub fn stop(&self) {
        self.inner
            .stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 停止标志的**副本**（给别的定时器用：会话结束就别再轮询了）。
    pub fn stop_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.inner.stop)
    }
}

/// 开一对 pty（**master** + **slave**）。
///
/// ⚠️ **不用 `openpty`**：它在 **libutil** 里（老 glibc 要 `-lutil`，
///    Rust 的 `libc` crate 不会自动加这个链接参数）⇒ 用 POSIX 那套
///    （`posix_openpt` + `grantpt` + `unlockpt` + `ptsname_r` + `open`），
///    全在 libc 本身里。
#[cfg(target_os = "linux")]
fn open_pty_pair() -> io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY：只是打开 /dev/ptmx，返回的 fd 由我们自己接管。
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：master 是上面拿到的有效 fd。
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY：fd 有效。
    if unsafe { libc::grantpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：同上。
    if unsafe { libc::unlockpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0 as libc::c_char; 256];
    // SAFETY：name 是栈上数组；`ptsname_r` 按 buflen 写，不会越界。
    let rc = unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY：ptsname_r 写的是 NUL 结尾的路径。
    let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    // SAFETY：path 是有效的路径字符串（带 NUL）。
    let slave = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：slave 是上面拿到的有效 fd。
    Ok((master, unsafe { OwnedFd::from_raw_fd(slave) }))
}

/// 把 fd 的行规程切成 **raw**（`cfmakeraw`）。
///
/// ⚠️ 切成 raw 之后：`ICANON`/`ECHO`/`ISIG`/`OPOST` 全关 ⇒
///    「敲键即发、不回显、Ctrl-C 当字节送远程、远程字节原样显示」。
#[cfg(target_os = "linux")]
fn set_raw_termios(fd: std::os::fd::RawFd) -> io::Result<()> {
    let mut tio: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY：tio 是本函数栈上的零初始化结构；fd 有效；`tcgetattr` 只往里写。
    if unsafe { libc::tcgetattr(fd, &mut tio) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：tio 是刚取回来的有效 termios。
    unsafe { libc::cfmakeraw(&mut tio) };
    // SAFETY：同上。
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &tio) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
        cb: impl FnOnce(Result<ChildPid, String>) + 'static,
    ) {
        // ⚠️ **别用 `glib::Pid`**：GLib 的 `GPid` 在 Windows 上是
        //    `*mut c_void`（**不是整数**）⇒ `glib::Pid(pid as i32)` 报
        //    E0308（Windows CI 抓到的）⇒ 用自己的 `ChildPid`（= u32）。
        //    Windows 上这个 pid 没人用（ghostty 后端不 watch_child）。
        let res = self
            .con
            .spawn(cwd, argv, env)
            .map(|pid| pid as ChildPid);
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
        cb: impl FnOnce(Result<ChildPid, String>) + 'static,
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
