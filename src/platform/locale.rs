//! locale 初始化（跨平台版 阶段 C0）。
//!
//! ## 为什么需要
//!
//! 与 C 版一致：`setlocale(LC_ALL, "")` ⇒ `strerror` 等按当前 locale 本地化
//! （中文 locale 下 "Connection refused" 显示成「拒绝连接」）。
//!
//! ⚠️ **Windows 的坑**：Windows 的 C 运行时 locale 默认是**系统 ANSI 代码页**
//! （简体中文机器上是 GBK/936），而 GTK 期望 **UTF-8**。
//! `strerror` 在那里返回的是 **GBK 字节** ⇒ 直接显示就是乱码。
//!
//! ⇒ 跨平台版在非 Unix 上**不做 `setlocale`**：
//! 错误文案改走 [`crate::backend::conn::strerror_local`] 的平台无关分支
//! （Rust 的 `io::Error` 自带 ASCII 描述）。

/// 进程启动时的 locale 初始化（幂等，重复调用无副作用）。
pub fn init() {
    #[cfg(unix)]
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
    #[cfg(not(unix))]
    {
        // Windows：不设 locale（见模块说明），交给系统默认（UTF-8）。
    }
}

/// 当前 locale 下 `errno` 的可读描述。
///
/// Unix：走 `libc::strerror`（与 C 版同源，按 locale 本地化）。
/// Windows：走 Rust 的 `io::Error`（ASCII，不受 ANSI 代码页影响）。
pub fn strerror(errno: i32) -> String {
    #[cfg(unix)]
    {
        // SAFETY：`strerror` 返回的是**静态**字符串，调用方只读不写；
        // 返回值可能为 NULL（未知 errno）⇒ 下面判空。
        unsafe {
            let msg = libc::strerror(errno);
            if !msg.is_null() {
                return std::ffi::CStr::from_ptr(msg).to_string_lossy().into_owned();
            }
        }
        format!("errno {errno}")
    }
    #[cfg(not(unix))]
    {
        format!("errno {errno}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_初始化不炸() {
        init();
        init();
    }

    #[test]
    fn errno_描述非空() {
        let s = strerror(2);
        assert!(!s.is_empty(), "errno 描述不应为空");
    }
}
