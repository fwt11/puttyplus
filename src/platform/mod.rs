//! 平台差异收口层（跨平台版 阶段 C0）。
//!
//! ## 为什么要有这一层
//!
//! 跨平台改造之前，平台相关的东西散在 6 个文件里：
//! `/proc/self/exe`（自身路径）、`libc::setlocale`、`libc::atexit`、
//! `libc::strerror`、termios（pty 行规程）、X11 选区直读。
//! 每换一次平台就要全仓库搜一遍「这个能不能编」——**很容易漏**。
//!
//! ⇒ 约定：**平台差异只允许出现在 `src/platform/` 里**。
//! 业务代码只调 `platform::xxx`，不直接碰 `libc` / `/proc` / X11。
//!
//! ## 当前状态
//!
//! - Linux：全部走原生实现，行为与改造前**逐字节一致**。
//! - Windows / macOS：路径、自身路径、locale 已就绪；
//!   桌面通知走「状态栏兜底」（见 [`notify`]）；PTY 与终端引擎见阶段 C2–C4。

pub mod clipboard;
pub mod exe;
pub mod locale;
pub mod localpath;
pub mod notify;
pub mod paths;
pub mod perm;
pub mod pty;

/// Windows 的 ConPTY（伪终端）。
///
/// ⚠️ 只有 Windows 编译；别的平台上这个模块是空的。
#[cfg(windows)]
pub mod conpty;
