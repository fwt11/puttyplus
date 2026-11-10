//! **呼吸相位**：传输中的文件行、队列按钮图标共用一个相位做动态效果。
//!
//! 干什么用（用户 2026-11-10）：「上传/下载的文件背景色可以呼吸变化，
//! 这样更明显些，任务列表等图标也可以有一些动态效果」。
//!
//! **为什么按时间算而不是计数器累加**：一个程序里可能有多个面板（多标签、
//! 多分屏），各自起自己的重绘定时器 ⇒ 计数器会互相打架（一个面板的
//! 定时器把相位推走了，另一个面板看到的相位就乱了）⇒ 相位必须**只由
//! 时间决定**：这样所有面板天然同步，而且**没起定时器时相位也是对的**。
//!
//! **为什么用正弦而不是方波**：方波是"闪"，正弦两头慢中间快 ⇒ 看着像
//! 真的在"呼吸"。

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

/// 一个完整呼吸来回的毫秒数。
pub const CYCLE_MS: i64 = 1600;

/// 现在的呼吸相位（0 = 最暗，1 = 最亮）。
pub fn phase() -> f32 {
    let now = glib::monotonic_time() / 1000; // 毫秒
    let t = (now % CYCLE_MS) as f32 / CYCLE_MS as f32;
    // 正弦：0 → 1 → 0
    0.5 - 0.5 * (t * 2.0 * std::f32::consts::PI).cos()
}

/// 按相位插值出一个透明度（`min` = 最暗，`max` = 最亮）。
pub fn alpha(min: f32, max: f32) -> f32 {
    min + (max - min) * phase()
}

/// 主题的强调色（accent）。
///
/// ⚠️ 查不到就退回 GTK 的经典蓝（各主题下都还看得见）。
pub fn accent(widget: &impl IsA<gtk::Widget>) -> gdk::RGBA {
    widget
        .style_context()
        .lookup_color("accent_bg_color")
        .or_else(|| gdk::RGBA::parse("blue").ok())
        .unwrap_or_else(|| gdk::RGBA::new(0.2, 0.5, 0.9, 1.0))
}
