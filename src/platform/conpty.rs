//! Windows ConPTY —— 伪终端（ssh 子进程要跑在伪终端里，否则没有 TTY、交互式登录会直接失败）。
//!
//! ## 为什么需要 ConPTY
//!
//! Windows 没有 `forkpty`。交互式程序（ssh 的密码提示、`yes/no` 确认、
//! 密码输入）**必须有 TTY**：没有 TTY 时 ssh 会直接报错退出，或者密码提示
//! 根本读不出来。
//!
//! ConPTY（Windows 10 1809 起）是 Windows 官方的伪终端：一个内核对象 +
//! 一对匿名管道。子进程挂在它上面 ⇒ 子进程看到的是「一个真终端」，而我们
//! 这边拿到的只是普通管道的字节流（读写都跟 Linux 上的 pty master 一样）。
//!
//! ## 数据流向
//!
//! ```text
//!   用户按键 ──▶ in_write ──▶ [ConPTY] ──▶ 子进程（ssh）的 stdin
//!                                ▲
//!                                │ 伪终端尺寸 / 屏幕缓冲
//!   子进程的 stdout ──▶ [ConPTY] ──▶ out_write ──▶ out_read ──▶ 我们
//! ```
//!
//! ## Windows API 的坑（都踩过）
//!
//! 1. **`CreatePseudoConsole` 的两个管道句柄方向**：第一个是**读端**
//!    （ConPTY 从这儿读用户输入），第二个是**写端**（ConPTY 往这儿写输出）。
//!    传反了 ⇒ 子进程收不到按键，我们也读不到输出。
//! 2. **传给 ConPTY 的那两个句柄，创建完必须马上在父进程这边关掉**：ConPTY
//!    自己复制了一份；父进程不关 ⇒ 输出管道的写端一直有人握着 ⇒ **读端永远
//!    等不到 EOF**（终端的读循环会挂死）。
//! 3. **`UpdateProcThreadAttribute` 的 `lpValue` 是「指向 HPCON 的指针」**，
//!    不是 HPCON 本身，`cbSize` 传指针大小。传错了 ⇒ 进程能起来，但**没挂在
//!    伪终端上**（没有 TTY，交互式登录直接失败）。
//! 4. **`CreateProcessW` 的命令行是「一整条 UTF-16 字符串」**，不是 argv
//!    数组 ⇒ 得自己按 `CommandLineToArgvW` 的规则给参数加引号（含空格/
//!    引号/反斜杠的必须转义）。
//! 5. **`STARTUPINFOEXW` 的 `cb` 要填 `sizeof(STARTUPINFOEXW)`**（不是
//!    `STARTUPINFOW`）；`lpstartupinfo` 传 `&si.StartupInfo`（结构首成员，
//!    地址相同）。
//! 6. **不要设 `STARTF_USESTDHANDLES`**：`PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`
//!    会自动把子进程的三个标准句柄接到伪终端上；自己再塞一遍反而绕过伪终端。
//! 7. **环境块必须「双 NUL 收尾」**：每条 `KEY=VALUE` 后面一个 NUL，整个块
//!    末尾**再多一个** NUL。
//! 8. **子进程退出后读端不会自动 EOF**：ConPTY 一直握着输出管道的写端 ⇒
//!    必须 `ClosePseudoConsole` 才会让读端拿到 EOF ⇒ 这里起了个看门狗线程
//!    盯着进程句柄，进程一退出就关伪终端。

// ⚠️ **整个文件只在 Windows 上编译**：非 Windows 上这个模块是空的
// （`Cargo.toml` 里 `windows-sys` 也是 `[target.'cfg(windows)'.dependencies]`）。
#![cfg(target_os = "windows")]

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, HANDLE, DUPLICATE_SAME_ACCESS,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTUPINFOEXW, STARTUPINFOW,
};

/// 一个 ConPTY 会话。
///
/// ⚠️ **内部是 `Arc<Mutex<..>>`**：读（阻塞在管道上）与写（UI 线程敲键）
/// 必须能同时发生 ⇒ 句柄用 `Mutex` 保护；但**读的时候不持锁**（持锁读会把
/// 写一起堵住）。
pub struct ConPty {
    inner: Arc<Mutex<Inner>>,
    /// **UTF-16LE ⇒ UTF-8 的管道适配器**（输出方向）。
    ///
    /// ⚠️ ConPTY 的输出**是 UTF-16LE** ⇒ 不解码的话屏幕上全是乱码。
    /// ⚠️ **必须有状态**：半个码元 / 半个代理对会跨两次读（见
    ///    [`crate::platform::utf16`]）⇒ 这套逻辑放在**不带平台门控**的
    ///    模块里，Linux 上也能单测。
    /// ⚠️ **单独一把锁**（不跟 `inner` 共用）：读线程会**阻塞**在管道读上
    ///    ⇒ 共用一把锁的话，用户敲键（写路径）也会被堵住。
    pipe: Mutex<crate::platform::utf16::Utf16Pipe>,
}

/// 会话持有的 Win32 资源。
struct Inner {
    /// 伪终端句柄（`HPCON`；**0 = 已关闭**）。
    hpc: HPCON,
    /// 往伪终端写（用户敲的键）。
    input: Option<Arc<File>>,
    /// 从伪终端读（子进程输出）。
    output: Option<Arc<File>>,
    /// 子进程句柄（**0 = 还没起进程 / 已关闭**）。
    ///
    /// ⚠️ **存成 `usize` 而不是 `HANDLE`**：`HANDLE` 底层是裸指针
    /// （`*mut c_void`），而**裸指针不是 `Send`/`Sync`** ⇒ 那样的话
    /// `ConPty` 就送不到读线程里（终端的读循环必须在别的线程跑）。
    /// 存成 `usize` 就自带 `Send + Sync`，不用写 `unsafe impl`。
    process: usize,
}

impl ConPty {
    /// 创建 ConPTY 与配套管道（列 × 行）。
    pub fn new(cols: u16, rows: u16) -> io::Result<Self> {
        // 输入管道：父进程写 → ConPTY 读
        let (in_read, in_write) = create_pipe()?;
        // 输出管道：ConPTY 写 → 父进程读
        let (out_read, out_write) = match create_pipe() {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: 两个句柄都是刚建出来的、有效；这里还没交给别人。
                unsafe {
                    CloseHandle(in_read);
                    CloseHandle(in_write);
                }
                return Err(e);
            }
        };

        // 创建伪终端（子进程后面要挂上去）。
        let mut hpc: HPCON = 0;
        // SAFETY: `hpc` 是有效出参；`in_read`/`out_write` 是刚建好的有效管道
        // 句柄（方向见文件头的说明：第一个是读端、第二个是写端）。
        let hr = unsafe { CreatePseudoConsole(coord(cols, rows), in_read, out_write, 0, &mut hpc) };
        if hr != 0 {
            // SAFETY: 四个句柄都还有效（还没关过）。
            unsafe {
                CloseHandle(in_read);
                CloseHandle(in_write);
                CloseHandle(out_read);
                CloseHandle(out_write);
            }
            // ⚠️ CreatePseudoConsole 返回的是 **HRESULT**（不是 GetLastError），
            // 所以错误码在 hr 里，不能用 last_error()。
            return Err(io::Error::other(format!(
                "创建 ConPTY 失败（HRESULT 0x{hr:08x}）"
            )));
        }

        // ⚠️ **传进去的那两个句柄，父进程这边必须马上关掉**：ConPTY 自己复制了
        // 一份；父进程这份不关 ⇒ 输出管道的写端一直有人握着 ⇒ 读端**永远等
        // 不到 EOF**（终端的读循环会挂死）。
        // SAFETY: 这两个句柄已被 ConPTY 复制走，父进程这份可以关。
        unsafe {
            CloseHandle(in_read);
            CloseHandle(out_write);
        }

        // 剩下的两个句柄交给 `File` 管：读写用 std 的接口 ⇒ 错误码自动带上
        // `GetLastError()`，也省掉自己写 `ReadFile`/`WriteFile` 的 unsafe。
        // SAFETY: `in_write`/`out_read` 是有效句柄，且**所有权移交**给 File
        // （之后不再手动 CloseHandle）。
        let input = unsafe { File::from_raw_handle(in_write as RawHandle) };
        let output = unsafe { File::from_raw_handle(out_read as RawHandle) };

        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                hpc,
                input: Some(Arc::new(input)),
                output: Some(Arc::new(output)),
                process: 0,
            })),
            pipe: Mutex::new(crate::platform::utf16::Utf16Pipe::new()),
        })
    }

    /// 起子进程（cwd / argv / env），返回进程 id。
    ///
    /// argv 形如 `["ssh", "-p", "22", "user@host"]`；env 是 `"KEY=VALUE"`
    /// 形式的列表（**完整**的环境，不是增量）。
    pub fn spawn(&self, cwd: &str, argv: &[&str], env: &[&str]) -> io::Result<u32> {
        if argv.is_empty() {
            return Err(io::Error::other("argv 为空，没法起子进程"));
        }
        let hpc = {
            let g = self.lock();
            if g.process != 0 {
                return Err(io::Error::other("这个 ConPTY 已经有子进程了"));
            }
            g.hpc
        };
        if hpc == 0 {
            return Err(io::Error::other("ConPTY 已关闭"));
        }

        // Windows 收的是**一整条命令行**，不是 argv 数组 ⇒ 自己拼。
        let mut cmd_line = build_command_line(argv);
        // 环境块：`KEY=VALUE\0` 逐条，末尾**再多一个 NUL**（双 NUL 收尾）。
        let env_block = build_env_block(env);
        // 工作目录（UTF-16，NUL 结尾）。
        let mut cwd_w: Vec<u16> = OsStr::new(cwd).encode_wide().collect();
        cwd_w.push(0);

        // ---- 属性列表：把子进程挂到伪终端上 ----
        let mut attr_size: usize = 0;
        // SAFETY: 第一次调用传 NULL 是官方用法（**只问大小**）；它会返回
        // FALSE + ERROR_INSUFFICIENT_BUFFER，那是预期结果。
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
        }
        if attr_size == 0 {
            return Err(io::Error::other("拿不到进程属性列表的大小"));
        }
        // 属性列表要求 8 字节对齐 ⇒ 用 `u64` 撑（`Vec<u8>` 只有 1 字节对齐）。
        let mut attr_buf = vec![0u64; attr_size.div_ceil(8)];
        let attr_list = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `attr_buf` 至少 `attr_size` 字节且 8 字节对齐（`Vec<u64>`）。
        let ok = unsafe { InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size) };
        if ok == 0 {
            return Err(last_error("初始化进程属性列表"));
        }
        // ⚠️ **`lpValue` 传的是「指向 HPCON 的指针」**，不是 HPCON 本身；
        // 传错 ⇒ 进程能起来，但**没挂在伪终端上**（没有 TTY ⇒ 交互式登录失败）。
        // SAFETY: `hpc` 是本会话的伪终端句柄；内核只读它指向的那 8 字节。
        let ok = unsafe {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                &hpc as *const HPCON as *const c_void,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            // SAFETY: 属性列表已初始化成功，删掉即可。
            unsafe { DeleteProcThreadAttributeList(attr_list) };
            return Err(last_error("设置伪终端属性"));
        }

        // ---- 起进程 ----
        let mut si = STARTUPINFOEXW::default();
        // ⚠️ `cb` 要填 **STARTUPINFOEXW 的大小**（不是 STARTUPINFOW）。
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attr_list;
        // ⚠️ `dwFlags` **保持 0**：不要设 `STARTF_USESTDHANDLES` ——
        // 伪终端属性会自动把子进程的三个标准句柄接上去；自己再塞一遍反而
        // 绕过伪终端。
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: 各指针都指向本函数里活着的内存（`cmd_line`/`env_block`/
        // `cwd_w`/`si`/`pi` 都在整个调用期间有效）；`cmd_line` 是**可写**的
        // NUL 结尾 UTF-16 串（CreateProcessW 可能就地改它）；
        // `&si.StartupInfo` 与 `&si` 同址（STARTUPINFOEXW 的首成员就是
        // STARTUPINFOW）。
        let ok = unsafe {
            CreateProcessW(
                // lpApplicationName 传 NULL ⇒ 从命令行里取程序名（会按 PATH 找）。
                std::ptr::null(),
                cmd_line.as_mut_ptr(),
                std::ptr::null(), // 进程安全属性
                std::ptr::null(), // 线程安全属性
                0,                // bInheritHandles = FALSE（伪终端自己给标准句柄）
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                env_block.as_ptr() as *const c_void,
                cwd_w.as_ptr(),
                &si.StartupInfo as *const STARTUPINFOW,
                &mut pi,
            )
        };
        // 属性列表用完了 ⇒ 删掉（内核在创建时已经把内容复制走）。
        // SAFETY: `attr_list` 是上面初始化成功的那一个。
        unsafe { DeleteProcThreadAttributeList(attr_list) };
        if ok == 0 {
            return Err(last_error("创建子进程"));
        }

        // 线程句柄用不上 ⇒ 立刻关掉（只留进程句柄）。
        // SAFETY: `pi.hThread` 是刚拿到的有效句柄。
        unsafe {
            CloseHandle(pi.hThread);
        }

        let process = pi.hProcess as usize;
        {
            let mut g = self.lock();
            g.process = process;
        }
        // 看门狗：子进程退出 ⇒ 关伪终端 ⇒ 读端才拿得到 EOF。
        self.start_watchdog(process);

        Ok(pi.dwProcessId)
    }

    /// 读子进程的输出（阻塞）。返回 0 表示对端已关闭（EOF）。
    ///
    /// ⚠️ **返回的是 UTF-8**：ConPTY 的管道里是 **UTF-16LE** ⇒ 这里解码成
    ///    UTF-8 再给上层（终端内核只认 UTF-8）。
    /// ⚠️ **两次读之间可能只拿到半个码元 / 半个代理对** ⇒ 解码器有状态
    ///    （见 [`crate::platform::utf16`]）。
    /// ⚠️ **读循环本身在 `utf16.rs` 里**（不带平台门控）⇒ Linux 上也能单测
    ///    （这个文件整个是 `#![cfg(target_os="windows")]` ⇒ 这里的逻辑在
    ///    Linux 上根本不会被编译）。
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let out = {
            let g = self.lock();
            g.output.clone()
        };
        let out = match out {
            Some(f) => f,
            None => return Err(io::Error::other("ConPTY 已关闭")),
        };
        let mut r: &File = &out;
        // ⚠️ **只锁 pipe 那把锁**（不锁 `inner`）⇒ 阻塞读不会堵住敲键。
        let mut pipe = self.pipe.lock().unwrap_or_else(|e| e.into_inner());
        crate::platform::utf16::read_decoded(&mut |b| r.read(b), buf, &mut pipe)
    }

    /// 写给子进程（用户敲的键）。
    ///
    /// ⚠️ **输入方向也是 UTF-16LE**：我们给的是 UTF-8 ⇒ 得先编码，
    ///    否则中文变乱码、ASCII 变成「每个字符后面跟个 NUL」。
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // ⚠️ 终端给的是 UTF-8 字节 ⇒ 先当**文本**解出来再编码成 UTF-16LE。
        //    （按键几乎都是 ASCII ⇒ 这一步不会丢东西；万一有非法字节，
        //    `from_utf8_lossy` 换成 U+FFFD，不至于让整个输入流崩掉。）
        let text = String::from_utf8_lossy(bytes);
        let enc = crate::platform::utf16::utf8_to_utf16le(&text);
        let inp = {
            let g = self.lock();
            g.input.clone()
        };
        let inp = match inp {
            Some(f) => f,
            None => return Err(io::Error::other("ConPTY 已关闭")),
        };
        let mut w: &File = &inp;
        w.write_all(&enc)
    }

    /// 改窗口大小（用户拖了窗口边缘）。
    ///
    /// ⚠️ **远端 ssh 靠它知道屏幕多大**（不发窗口大小的 ssh 会一直按 80×24
    /// 排版）。
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let hpc = {
            let g = self.lock();
            g.hpc
        };
        if hpc == 0 {
            return Err(io::Error::other("ConPTY 已关闭"));
        }
        // SAFETY: `hpc` 是本会话的伪终端句柄（还没关）。
        let hr = unsafe { ResizePseudoConsole(hpc, coord(cols, rows)) };
        if hr != 0 {
            // ResizePseudoConsole 返回 **HRESULT**（不是 GetLastError）⇒ 错误码
            // 在 hr 里。
            return Err(io::Error::other(format!(
                "调整伪终端大小失败（HRESULT 0x{hr:08x}）"
            )));
        }
        Ok(())
    }

    /// 关掉伪终端（子进程还在跑的话也一并结束）。
    ///
    /// ⚠️ **关伪终端才会让读端拿到 EOF**（ConPTY 一直握着输出管道的写端），
    /// 所以子进程正常退出后也得有人调它 —— 这里由看门狗线程负责。
    pub fn close(&self) {
        let (hpc, process) = {
            let mut g = self.lock();
            (
                std::mem::replace(&mut g.hpc, 0),
                std::mem::replace(&mut g.process, 0),
            )
        };
        if hpc != 0 {
            // SAFETY: `hpc` 是本会话的伪终端句柄；换成 0 保证只关一次。
            // 关掉它 ⇒ 输出管道的写端被释放 ⇒ 读端拿到 EOF。
            unsafe {
                ClosePseudoConsole(hpc);
            }
        }
        if process != 0 {
            // 子进程还活着？结束掉（不然会变成野进程一直挂着）。
            // SAFETY: `process` 是本会话起出来的进程句柄，有效；关掉之后
            // 不会再有人用它。
            unsafe {
                TerminateProcess(process as HANDLE, 1);
                // 等它真的死（最多 500ms），免得刚关就退出时它还在写管道。
                WaitForSingleObject(process as HANDLE, 500);
                CloseHandle(process as HANDLE);
            }
        }
    }

    /// 起看门狗线程：子进程一退出就关掉伪终端。
    ///
    /// ⚠️ **为什么必须关**：ConPTY 一直握着输出管道的写端 ⇒ 子进程都退出了，
    /// 读端也**等不到 EOF**（终端的读循环会永远挂着，界面看着像「卡住」）。
    fn start_watchdog(&self, process: usize) {
        // 给看门狗一份**独立的**进程句柄副本：`close()` 会关掉主进程句柄，
        // 而「等进程退出」必须在一个不会被提前关掉的句柄上等。
        let mut dup: HANDLE = std::ptr::null_mut();
        // SAFETY: 源句柄是刚拿到的有效进程句柄；目标进程是当前进程；
        // `DUPLICATE_SAME_ACCESS` 表示按源的访问权复制。
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                process as HANDLE,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            // 复制失败 ⇒ 没有看门狗（子进程退出后读端可能等不到 EOF）。
            // 但会话本身还能用，不算致命错误，不在这里报错。
            return;
        }
        // 裸指针不是 `Send` ⇒ 存成 `usize` 送进线程（在线程里再转回去）。
        let dup_v = dup as usize;
        // 用 `Weak`：看门狗不能把会话**吊着不放**（否则会话永远不析构、
        // 句柄永远不关）。
        let weak: Weak<Mutex<Inner>> = Arc::downgrade(&self.inner);
        let _ = std::thread::Builder::new()
            .name("pp-conpty-watch".into())
            .spawn(move || {
                // SAFETY: `dup_v` 是 DuplicateHandle 出来的有效进程句柄，
                // 本线程独占它（用完即关）。
                unsafe {
                    WaitForSingleObject(dup_v as HANDLE, INFINITE);
                }
                // 给 ConPTY 一点时间把子进程最后的输出冲出去
                // （立刻关的话可能丢掉「Connection closed」这类最后一行）。
                std::thread::sleep(Duration::from_millis(120));
                if let Some(inner) = weak.upgrade() {
                    let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                    if g.hpc != 0 {
                        let hpc = std::mem::replace(&mut g.hpc, 0);
                        // SAFETY: `hpc` 是本会话的伪终端句柄；换成 0 保证只关一次。
                        unsafe {
                            ClosePseudoConsole(hpc);
                        }
                    }
                }
                // SAFETY: `dup_v` 是本线程的副本，用完即关。
                unsafe {
                    CloseHandle(dup_v as HANDLE);
                }
            });
    }

    /// 锁住内部状态。
    ///
    /// ⚠️ **中毒了也当没中毒**：锁中毒只说明某处 panic 过，句柄还得收拾干净，
    /// 不能因为一次 panic 就把会话泄漏掉。
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for ConPty {
    fn drop(&mut self) {
        // 收尾：关伪终端 + 结束子进程（否则 ssh 会变成野进程）。
        self.close();
        // input / output 两个 File 随 Arc 析构自动 CloseHandle。
    }
}

/// 建一对匿名管道（读端、写端）。
fn create_pipe() -> io::Result<(HANDLE, HANDLE)> {
    let mut rd: HANDLE = std::ptr::null_mut();
    let mut wr: HANDLE = std::ptr::null_mut();
    // SAFETY: `rd`/`wr` 是有效出参；SECURITY_ATTRIBUTES 传 NULL ⇒ 句柄
    // **不可继承**（子进程不需要继承它们：ConPTY 那边是内核复制过去的）。
    let ok = unsafe { CreatePipe(&mut rd, &mut wr, std::ptr::null(), 0) };
    if ok == 0 {
        return Err(last_error("创建管道"));
    }
    Ok((rd, wr))
}

/// 列 × 行 ⇒ 伪终端的 `COORD`。
///
/// ⚠️ `COORD` 的字段是 **i16** ⇒ 超大尺寸先夹住（窗口不可能有 32767 列）。
fn coord(cols: u16, rows: u16) -> COORD {
    COORD {
        X: cols.min(32767) as i16,
        Y: rows.min(32767) as i16,
    }
}

/// 把 Win32 的 `GetLastError()` 变成 `io::Error`（**带上原始错误码**）。
fn last_error(what: &str) -> io::Error {
    // SAFETY: `GetLastError` 无参数、不会失败。
    let code = unsafe { GetLastError() } as i32;
    // ⚠️ 用 `from_raw_os_error` 取原始错误码与系统消息（别自己编一句
    // 「失败」—— 那样就把码丢了，排查时只能猜）。
    let raw = io::Error::from_raw_os_error(code);
    io::Error::new(
        raw.kind(),
        format!("{what} 失败：{raw}（Win32 错误码 {code}）"),
    )
}

/// 把 argv 拼成 Windows 的命令行（**一整条 UTF-16 字符串**，NUL 结尾）。
fn build_command_line(argv: &[&str]) -> Vec<u16> {
    let mut s = String::new();
    for (i, a) in argv.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&quote_arg(a));
    }
    let mut wide: Vec<u16> = OsStr::new(&s).encode_wide().collect();
    wide.push(0); // NUL 结尾（CreateProcessW 要求）
    wide
}

/// 按 `CommandLineToArgvW` 的规则给单个参数加引号。
///
/// ⚠️ 不加引号 ⇒ 含空格的参数会被拆成好几个（`ssh -o ProxyCommand=...` 这种
/// 参数就废了）。
fn quote_arg(arg: &str) -> String {
    // 不含空格/制表符/引号 ⇒ 原样（程序名 `ssh` 就是这么传的）。
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut slashes = 0usize; // 连着的反斜杠个数
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                // 引号前的反斜杠要成对转义，再加一个把引号本身转义掉。
                for _ in 0..slashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                slashes = 0;
            }
            _ => {
                for _ in 0..slashes {
                    out.push('\\');
                }
                slashes = 0;
                out.push(c);
            }
        }
    }
    // 结尾的反斜杠（落在引号里）要成对转义。
    for _ in 0..slashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 构造环境块（UTF-16，`KEY=VALUE\0` 逐条，**末尾再多一个 NUL**）。
///
/// ⚠️ 环境块必须**双 NUL 收尾**：每条后面一个 NUL，整个块最后再多一个 ——
/// 少一个 NUL ⇒ 子进程解析环境会跑飞。
/// ⚠️ 环境变量名**大小写不敏感** ⇒ 同名的只留最后一个（后面的覆盖前面的）。
/// ⚠️ 条目按名字排序（Windows 的惯例，也是各种工具的期望）。
fn build_env_block(env: &[&str]) -> Vec<u16> {
    let mut items: Vec<(String, String)> = Vec::new(); // (名字大写, "KEY=VALUE")
    for e in env {
        let Some((k, v)) = e.split_once('=') else {
            continue;
        };
        if k.is_empty() {
            continue;
        }
        let up = k.to_ascii_uppercase();
        // 同名（大小写不敏感）只保留最后一个。
        items.retain(|(u, _)| *u != up);
        items.push((up, format!("{k}={v}")));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut wide: Vec<u16> = Vec::new();
    for (_, kv) in items {
        wide.extend(OsStr::new(&kv).encode_wide());
        wide.push(0);
    }
    // 空环境块也要两个 NUL 才合法。
    if wide.is_empty() {
        wide.push(0);
    }
    wide.push(0); // ⚠️ 末尾额外的 NUL ⇒ 双 NUL 收尾
    wide
}
