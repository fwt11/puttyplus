//! **终态文案（完成/失败/…）的淡出节奏**（纯逻辑，好单测）。
//!
//! 用户 2026-11-10：「下载完了，就显示一下完成，然后就慢慢消失吧，不要把
//! 「完成」一直显示在那里」。
//!
//! 干什么用（`pane.rs` 那边）：文件列表里那一行的进度文案，传输结束后
//! **先完整显示一会儿**（让人看清"完成了"）⇒ **再渐隐** ⇒ **最后整条
//! 拿掉**（行回到正常样子）。
//!
//! ⚠️ **只管文件列表**：队列窗口（任务列表）里的「完成」是**记录**（那一行
//!    就是为了留个痕，用户还有「清除已完成」按钮）⇒ 不淡出。

/// 终态文案**完整显示**多久（毫秒）——先让人看清"完成了"，再开始淡。
pub const HOLD_MS: u64 = 1200;

/// 渐隐本身要多久（毫秒）。
pub const FADE_MS: u64 = 1200;

/// 淡出总共要多久（毫秒）。
pub const TOTAL_MS: u64 = HOLD_MS + FADE_MS;

/// 已经过去 `elapsed_ms` 毫秒时，该有的**透明度**（1 = 完全可见）。
///
/// 停留期内恒为 1；之后线性降到 0。
pub fn alpha_of(elapsed_ms: u64) -> f32 {
    if elapsed_ms < HOLD_MS {
        1.0
    } else {
        let t = (elapsed_ms - HOLD_MS) as f32 / FADE_MS as f32;
        (1.0 - t).clamp(0.0, 1.0)
    }
}

/// 已经过去 `elapsed_ms` 毫秒时，淡完了吗（该把文案整条拿掉了）。
pub fn finished(elapsed_ms: u64) -> bool {
    elapsed_ms >= TOTAL_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 停留期内一直看得见() {
        assert_eq!(alpha_of(0), 1.0);
        assert_eq!(alpha_of(HOLD_MS / 2), 1.0);
        assert_eq!(alpha_of(HOLD_MS - 1), 1.0);
    }

    #[test]
    fn 淡出期线性变淡() {
        // 刚开始淡时还是全可见
        assert_eq!(alpha_of(HOLD_MS), 1.0);
        // 中点只剩一半
        let mid = alpha_of(HOLD_MS + FADE_MS / 2);
        assert!(
            (mid - 0.5).abs() < 0.01,
            "淡出中点应该还有一半不透明度，实际 {mid}"
        );
        // 淡完就没了
        assert!(alpha_of(HOLD_MS + FADE_MS) <= 0.001);
        // 超过终点也不该变负
        assert!(alpha_of(TOTAL_MS * 10) >= 0.0);
    }

    #[test]
    fn 淡完才算完() {
        assert!(!finished(0));
        assert!(!finished(HOLD_MS));
        assert!(!finished(TOTAL_MS - 1));
        assert!(finished(TOTAL_MS));
        assert!(finished(TOTAL_MS + 5000));
    }

    #[test]
    fn 整个过程两秒多() {
        assert_eq!(TOTAL_MS, HOLD_MS + FADE_MS);
        assert!(TOTAL_MS >= 2000, "淡出太快，用户来不及看清「完成」");
    }
}
