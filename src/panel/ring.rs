//! 图标角上的「转动的圈」（传输忙态）。
//!
//! 干什么用（用户 2026-11-11）：「上传下载文件时传输队列图标可以改为原来
//! 静态的图标上加个转动的圈」⇒ 底下还是原来那个队列图标，右下角叠一个
//! 小转圈（转起来 ⇒ 一眼知道「还在传」）。
//!
//! ⚠️ **为什么不用 `gtk::Spinner`**：`GtkSpinner` 的最小尺寸写死 16×16
//!    ⇒ 盖在一个 16×16 的图标上就是**整个盖住**，不是「加个圈」；而且它
//!    吃图标主题的 spinner 资源 ⇒ 极简主题下可能是个空白。
//!    自己画 ⇒ 大小、位置、颜色都能定，且**任何主题下都在**。
//!
//! ⚠️ **颜色跟主题走**：CSS 里给 `.pp-xfer-ring` 定了 `color: @pp_accent`
//!    ⇒ 画的时候读控件样式上下文的前景色 ⇒ 换主题自动跟着换，
//!    不用在 Rust 里再抄一份配色。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;

/// 一圈正在转的弧。
pub struct Ring {
    /// 画弧的控件（塞进 `Overlay` 当覆盖层）。
    area: gtk::DrawingArea,
    /// 弧的起始角（弧度）。
    angle: Rc<Cell<f64>>,
    /// 旋转定时器（**只在转的时候存在** ⇒ 空闲时一点 CPU 都不占）。
    timer: RefCell<Option<glib::SourceId>>,
    /// 转不转。
    on: Rc<Cell<bool>>,
}

/// 每帧转多少弧度（40ms 一帧 ⇒ 转一圈约 0.46 秒，够快不晃眼）。
const STEP: f64 = 0.62;

/// 弧画多大一圈（弧度）：留 1/4 缺口 ⇒ 看得出在转。
const SWEEP: f64 = 4.71;

impl Ring {
    /// 造一个转圈（`size` = 直径，像素）。
    pub fn new(size: i32) -> Self {
        let angle = Rc::new(Cell::new(0.0f64));
        let on = Rc::new(Cell::new(false));
        let area = gtk::DrawingArea::new();
        area.set_size_request(size, size);
        area.add_css_class("pp-xfer-ring");
        // ⚠️ **默认藏起来**：空闲时不该在图标旁边留个灰圈。
        area.set_visible(false);
        {
            let angle = Rc::clone(&angle);
            let on = Rc::clone(&on);
            area.set_draw_func(move |area, cr, _w, _h| {
                if !on.get() {
                    return;
                }
                draw_arc(area, cr, angle.get());
            });
        }
        Self {
            area,
            angle,
            timer: RefCell::new(None),
            on,
        }
    }

    /// 控件本体（塞进 `Overlay` 当覆盖层）。
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// 开/关转圈（重复调用同一个值是空操作）。
    pub fn set_spinning(&self, on: bool) {
        if self.on.get() == on {
            return;
        }
        self.on.set(on);
        if on {
            self.area.set_visible(true);
            self.start();
            self.area.queue_draw();
        } else {
            self.stop();
            self.area.set_visible(false);
        }
    }

    /// 现在转不转（冒烟/单测用）。
    pub fn spinning(&self) -> bool {
        self.on.get()
    }

    /// 当前弧的起始角（**冒烟用**：确认它真的在转，不只是「设了个标志」）。
    pub fn angle(&self) -> f64 {
        self.angle.get()
    }

    /// 起旋转定时器。
    fn start(&self) {
        if self.timer.borrow().is_some() {
            return;
        }
        let angle = Rc::clone(&self.angle);
        // ⚠️ 定时器闭包只抓**弱引用**：控件没了就停（抓强引用 = 引用环）。
        let weak = self.area.downgrade();
        let id = glib::timeout_add_local(Duration::from_millis(40), move || {
            angle.set((angle.get() + STEP) % std::f64::consts::TAU);
            if let Some(a) = weak.upgrade() {
                a.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(id);
    }

    /// 停旋转定时器。
    fn stop(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // ⚠️ 不停的话定时器会一直转 ⇒ 面板关了还在烧 CPU。
        self.stop();
    }
}

/// 画一段弧（270°，圆头）。
fn draw_arc(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, angle: f64) {
    let w = area.width() as f64;
    let h = area.height() as f64;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let cx = w / 2.0;
    let cy = h / 2.0;
    // 半径留 1.2px 给线宽 ⇒ 弧不会顶出控件边界。
    let r = (w.min(h) / 2.0) - 1.2;
    if r <= 0.5 {
        return;
    }
    // ⚠️ 颜色跟主题走（CSS 的 `color: @pp_accent` ⇒ 样式上下文的前景色）。
    let fg = area.style_context().color();
    cr.set_source_rgba(
        fg.red() as f64,
        fg.green() as f64,
        fg.blue() as f64,
        fg.alpha() as f64,
    );
    cr.set_line_width(2.0);
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    cr.arc(cx, cy, r, angle, angle + SWEEP);
    cr.stroke();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 弧的起止角必须**小于一整圈**（留缺口）⇒ 否则画出来是个圆饼，
    /// 看不出在转。
    #[test]
    fn 弧留了缺口() {
        assert!(SWEEP < std::f64::consts::TAU);
        assert!(SWEEP > 1.0, "缺口留太大就看不出是个圈了");
    }

    /// 每帧步进要够小（转太快会晕）也不能是 0（那就完全不转了）。
    #[test]
    fn 步进角合理() {
        assert!(STEP > 0.0);
        assert!(STEP < 1.0, "一步转太多 ⇒ 看起来是在闪而不是在转");
    }
}
