//! 桌面通知能力探测（跨平台版 阶段 C0）。
//!
//! ## 现状
//!
//! 桌面通知走的是 **D-Bus 会话总线**上的 `org.freedesktop.Notifications`
//! （见 `src/notify.rs`）—— 那是 **Unix 桌面的标准接口**。
//!
//! Windows / macOS 没有会话总线 ⇒ 那一套**发不出去**。
//!
//! ## 首版怎么办
//!
//! **Windows 首版不做桌面通知**，直接走已有的**状态栏兜底**
//! （`src/notify.rs` 里「通知发不出去就写状态栏」那条路）。
//!
//! ⚠️ 这**不是**功能缺失的借口，而是有意的取舍：
//! ① 状态栏那句已经把结果说清楚了；
//! ② 「传输完弹窗」是用户明确禁止的行为（AGENTS.md 第 8 条），
//!    桌面通知本来就是那条「不弹窗但能回看」的路；
//! ③ 真要加 Windows 通知（Toast）得引新依赖，那是第二版的事。
//!
//! ⇒ 这里只回答一个是非问题：**这台机器能不能发桌面通知**。

/// 这台机器**可能**支持桌面通知。
///
/// 返回 `false` ⇒ 调用方应当**直接走状态栏兜底**，不要去碰 D-Bus。
///
/// ⚠️ 返回 `true` **不代表一定能发出去**：Unix 上还得看有没有通知守护进程
/// （`src/notify.rs` 里问 D-Bus 那一步会给出真话）。
pub fn desktop_available() -> bool {
    #[cfg(unix)]
    {
        true
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 能力探测不炸() {
        let _ = desktop_available();
    }

    #[test]
    fn 当前平台的能力与编译目标一致() {
        // Linux 上应当支持（真能不能发由 D-Bus 那一步说了算）
        #[cfg(unix)]
        assert!(desktop_available());
        // Windows / macOS 上不走 D-Bus 那条路
        #[cfg(not(unix))]
        assert!(!desktop_available());
    }
}
