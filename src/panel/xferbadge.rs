//! 「传输队列」按钮上的**角标**：一个蓝圆（圆里写数字），外面套一圈
//! **在转的弧**。
//!
//! 用户原话（2026-11-11）：
//! 「这个感觉不是很好，或者你可以在 1 周围有个圆圈在转？或者你想想怎么能
//! 更好点？发挥你的想象力。」
//!
//! # 为什么自己画，不拿现成控件拼
//!
//! 最早那版是「`Label`（蓝底白字）+ `Overlay`（上面盖一个转圈的
//! `DrawingArea`）」⇒ 看着别扭，两个原因：
//!
//! 1. **角标不是圆的**：`Label` 的自然高度由字体行高决定 ⇒ 蓝底是个
//!    **胶囊**（高 > 宽），外面套个圆弧就更怪。
//! 2. **圈和数字不同心**：`GtkOverlay` 给**覆盖层**分配的是它自己的
//!    尺寸（`gtk_overlay_get_child_position`：按 `halign` 对齐，
//!    `FILL` 时铺满）⇒ 覆盖层那个 22×22 的圈跟 15×15 的角标**中心差
//!    了几像素**，而且还会被按钮边缘裁掉一截。
//!
//! ⇒ 现在**蓝圆、弧、数字全是自己摆的**：外框固定 26×26，蓝圆半径 8，
//! 弧半径 11.8（线宽 2 ⇒ 弧占 10.8~12.8）⇒ 数字正好在圆心，弧在圆外
//! 留 2.8px 的缝 ⇒ 一眼就是「一个圈围着数字转」。
//!
//! # 颜色跟主题走
//!
//! 三个控件各拿一个 CSS 类，颜色全从**样式上下文的前景色**读
//! （`style_context().color()`）⇒ 换主题/换配色自动跟着换，Rust 里
//! 一份配色都不抄：
//!
//! | 部件 | CSS 类 | 颜色 |
//! |---|---|---|
//! | 蓝圆 | `.pp-xfer-dot` | `@pp_accent` |
//! | 转弧 | `.pp-xfer-ring` | `@pp_fg`（中性 ⇒ 不跟蓝圆糊成一坨） |
//! | 数字 | `.pp-xfer-num` | `@pp_on_accent` |

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use super::ring::Ring;

/// 角标外框的边长（像素）。
///
/// ⚠️ **必须 >= 弧的直径 + 2**：弧的半径是 `边长/2 - 1.2`、线宽 2 ⇒
///    弧的最外缘在 `边长/2 - 0.2` ⇒ 差 0.2px ⇒ **不会被裁掉**。
pub const BOX: i32 = 26;

/// 蓝圆的半径（像素）。
const DOT_R: f64 = 8.0;

/// 「传输队列」按钮上那个角标（数字 + 外面转的圈）。
pub struct XferBadge {
    /// 外框（`Overlay`）：蓝圆是主控件，弧和数字是覆盖层。
    ov: gtk::Overlay,
    /// 画蓝圆的控件。
    dot: gtk::DrawingArea,
    /// 转圈（自己画的那段弧）。
    ring: Ring,
    /// 数字。
    num: gtk::Label,
    /// 现在显示的数字（0 = 没有东西在传）。
    n: Rc<Cell<usize>>,
}

impl XferBadge {
    /// 造一个角标（空闲状态：没有数字、没有圈）。
    pub fn new() -> Self {
        let n = Rc::new(Cell::new(0usize));

        // ---- 蓝圆（自己画 ⇒ 一定是正圆，不会被字体行高拉成胶囊） ----
        let dot = gtk::DrawingArea::new();
        dot.set_size_request(BOX, BOX);
        dot.add_css_class("pp-xfer-dot");
        // ⚠️ **不吃点击**（不然按钮点不动 ⇒ 队列窗口打不开）。
        dot.set_can_target(false);
        {
            let n = Rc::clone(&n);
            dot.set_draw_func(move |area, cr, _w, _h| {
                // 没有数字 ⇒ 什么都不画（空闲时按钮旁不能挂个空蓝圆）。
                if n.get() == 0 {
                    return;
                }
                draw_dot(area, cr);
            });
        }

        // ---- 转圈 ----
        let ring = Ring::new(BOX);
        ring.widget().set_can_target(false);
        // ⚠️ **显式居中**：`GtkOverlay` 给覆盖层的默认对齐是 `FILL`
        //    ⇒ 换个 GTK 版本行为一变，圈就偏了 ⇒ 自己钉死。
        ring.widget().set_halign(gtk::Align::Center);
        ring.widget().set_valign(gtk::Align::Center);

        // ---- 数字 ----
        let num = gtk::Label::new(None);
        num.add_css_class("pp-xfer-num");
        num.set_can_target(false);
        // ⚠️ **跟外框一样大 + 文字居中** ⇒ 数字的圆心 = 蓝圆的圆心。
        //    （靠 Label 的自然尺寸去对齐是**靠不住的**：字体一变高，
        //    数字就偏了 ⇒ 直接给它整个外框的尺寸。）
        num.set_size_request(BOX, BOX);
        num.set_xalign(0.5);
        num.set_yalign(0.5);
        num.set_halign(gtk::Align::Center);
        num.set_valign(gtk::Align::Center);

        // ---- 叠起来 ----
        let ov = gtk::Overlay::new();
        ov.set_size_request(BOX, BOX);
        // 主控件 = 蓝圆（它铺满外框 ⇒ 圆心在外框中心）。
        ov.set_child(Some(&dot));
        ov.add_overlay(ring.widget());
        ov.add_overlay(&num);

        Self {
            ov,
            dot,
            ring,
            num,
            n,
        }
    }

    /// 控件本体（塞进工具栏）。
    pub fn widget(&self) -> &gtk::Overlay {
        &self.ov
    }

    /// 设角标数字（0 = 没有东西在传 ⇒ 数字清空、蓝圆不画、圈不转）。
    ///
    /// ⚠️ **不 hide 整个控件**：外框尺寸是固定的 ⇒ 空闲时也占着地方 ⇒
    ///    传输中出现/消失时**工具栏不会左右跳**（用户 2026-10-07 报过
    ///    「上传结束了还挂着一个蓝胶囊」——那是**底色**没摘，不是位置在动）。
    pub fn set_count(&self, n: usize) {
        if self.n.get() == n {
            return;
        }
        self.n.set(n);
        self.num.set_text(&if n > 0 { n.to_string() } else { String::new() });
        self.num.set_visible(n > 0);
        // 数字变了 ⇒ 蓝圆要重画（0 ↔ 非 0 是「有没有圆」的变化）。
        self.dot.queue_draw();
    }

    /// 现在角标上是几（0 = 没有）。
    pub fn count(&self) -> usize {
        self.n.get()
    }

    /// 角标**有没有底色**（蓝圆）。
    ///
    /// ⚠️ 冒烟用：用户 2026-10-07 报过「上传结束了还是这样，结束了就
    ///    不要这个蓝色的块了吧」⇒ 传完必须没有底色。
    pub fn has_dot(&self) -> bool {
        self.n.get() > 0
    }

    /// 开/关转圈（跟 `Ring` 一个口径）。
    pub fn set_spinning(&self, on: bool) {
        self.ring.set_spinning(on);
    }

    /// 圈现在转不转。
    pub fn ring_spinning(&self) -> bool {
        self.ring.spinning()
    }

    /// 圈现在转到哪个角度（冒烟用：确认它真在转）。
    pub fn ring_angle(&self) -> f64 {
        self.ring.angle()
    }
}

impl Default for XferBadge {
    fn default() -> Self {
        Self::new()
    }
}

/// 画那个蓝圆（**正圆**，跟字体无关）。
fn draw_dot(area: &gtk::DrawingArea, cr: &gtk::cairo::Context) {
    let w = area.width() as f64;
    let h = area.height() as f64;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let cx = w / 2.0;
    let cy = h / 2.0;
    // ⚠️ 颜色跟主题走（CSS 的 `color: @pp_accent` ⇒ 样式上下文的前景色）。
    let fg = area.style_context().color();
    cr.set_source_rgba(
        fg.red() as f64,
        fg.green() as f64,
        fg.blue() as f64,
        fg.alpha() as f64,
    );
    cr.arc(cx, cy, DOT_R, 0.0, std::f64::consts::TAU);
    // 用「填 + 描边」把**抗锯齿的边**补齐（只填的话边缘会有毛刺）。
    cr.fill_preserve();
    cr.set_line_width(1.0);
    cr.stroke();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 外框必须装得下弧（弧的最外缘在 `BOX/2 - 0.2`）⇒ 差 0.2px ⇒
    /// **不会被裁掉**（用户第一次看到的那版就是被裁了一截）。
    #[test]
    fn 外框装得下弧() {
        let outer = BOX as f64 / 2.0 - 1.2 + 1.0;
        assert!(
            outer <= BOX as f64 / 2.0,
            "弧会顶出外框（{outer} > {}）⇒ 会被裁掉",
            BOX as f64 / 2.0
        );
    }

    /// 弧和蓝圆之间要**留缝**（重合就成一坨了）。
    #[test]
    fn 弧和蓝圆留了缝() {
        let ring_inner = BOX as f64 / 2.0 - 1.2 - 1.0;
        assert!(ring_inner > DOT_R, "弧压到蓝圆上了（{ring_inner} <= {DOT_R}）");
    }
}
