//! 状态药丸单元格（美化·地基 2/5）：**圆角胶囊底 + 状态色圆点 + 正文色文字**。
//!
//! 以前容器表的状态列是「● 运行中」——Pango markup 的小圆点，状态信息
//! 挤在一行普通文字里，一屏 30 行时扫不出"哪些在跑"。药丸给状态一个
//! **形状**（胶囊底 + 色点），扫一列就是一眼的事。
//!
//! 为什么自绘渲染器而不是 Pango markup：markup 的 `background` 属性画的
//! 是**直角矩形**（没有圆角、紧贴文字）⇒ 药丸的"胶囊感"出不来；而
//! TreeView 列又放不进真正的 widget。所以走 `CellRenderer` 自绘
//! （跟 [`crate::panel::namecell`] 同一套路）。
//!
//! 颜色全部**跟主题走**（项目硬规矩，UI 审查 D1 的教训）：
//! - 底色 [`crate::theme::pill_bg_hex`]（状态色往面板底混 0.84）；
//! - 圆点 [`crate::theme::state_hex`]；
//! - 文字取**样式上下文的正文色**——选中行要跟着选择底变色（见
//!   [`cell_text_state`]，`:focus` 那个坑的教训同样适用）。

use std::cell::{Cell as StdCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// 药丸的状态档位。
///
/// 比 [`crate::theme::State`] 多一个 `Dim`（灰）：「已停止 / 已创建」
/// **不是错误**，只是没在跑（跟 `state_markup` 的口径一致）；
/// compose 父行也没有自己的状态 ⇒ 同样灰。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PillState {
    /// 运行中（绿）。
    Ok,
    /// 已暂停 / 重启中（黄）。
    Warn,
    /// 已失效（红）。
    Err,
    /// 中性：已停止 / 已创建 / compose 父行（灰）。
    ///
    /// **默认档**：未知状态宁可低调，不许装绿。
    #[default]
    Dim,
}

impl PillState {
    /// 一档状态的（底色，圆点色），**全走主题令牌**。
    ///
    /// ⚠️ 文字色**不在这里定**：文字要跟选择态走（选中行底色会变），
    /// 得从样式上下文现取（见 `snapshot`）。
    fn colors(self) -> (String, String) {
        use crate::theme::{mix_hex, panel_hex, state_hex, State};
        match self {
            PillState::Ok => (
                crate::theme::pill_bg_hex(State::Ok),
                state_hex(State::Ok).to_string(),
            ),
            PillState::Warn => (
                crate::theme::pill_bg_hex(State::Warn),
                state_hex(State::Warn).to_string(),
            ),
            PillState::Err => (
                crate::theme::pill_bg_hex(State::Err),
                state_hex(State::Err).to_string(),
            ),
            PillState::Dim => (
                mix_hex(&crate::theme::dim_hex(), &panel_hex(), 0.84),
                crate::theme::dim_hex(),
            ),
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PillCellRenderer {
        pub text: RefCell<String>,
        /// 状态档位（`PillState` as u8；StdCell 不是字段类型限制，是够用）。
        pub state: StdCell<u8>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PillCellRenderer {
        const NAME: &'static str = "PpPillCellRenderer";
        type Type = super::PillCellRenderer;
        type ParentType = gtk::CellRenderer;
    }

    impl ObjectImpl for PillCellRenderer {}

    impl CellRendererImpl for PillCellRenderer {
        fn preferred_width<P: IsA<gtk::Widget>>(&self, _widget: &P) -> (i32, i32) {
            // 列宽由列自己的 min_width 决定（跟 namecell 同款：内容不撑列）
            (1, 1)
        }

        fn preferred_height<P: IsA<gtk::Widget>>(&self, widget: &P) -> (i32, i32) {
            // 胶囊高 = 文字高 + 上下各 3px；再给行留 2px 余量
            let layout = widget.create_pango_layout(Some("Ag"));
            let (_, h) = layout.pixel_size();
            let pill_h = h + 6;
            (pill_h + 2, pill_h + 2)
        }

        fn snapshot<P: IsA<gtk::Widget>>(
            &self,
            snapshot: &gtk::Snapshot,
            widget: &P,
            _background_area: &gdk::Rectangle,
            cell_area: &gdk::Rectangle,
            flags: gtk::CellRendererState,
        ) {
            let text = self.text.borrow().clone();
            if text.is_empty() {
                return;
            }
            let state = PillState::from_u8(self.state.get());

            let (bg_hex, dot_hex) = state.colors();
            // 底色不认得就整个不画（宁可没形状，不画错色）
            let (Some(bg), Some(dot)) = (
                gdk::RGBA::parse(&bg_hex).ok(),
                gdk::RGBA::parse(&dot_hex).ok(),
            ) else {
                return;
            };

            let mut layout = widget.create_pango_layout(Some(&text));
            layout.set_single_paragraph_mode(true);
            let (mut tw, mut th) = layout.pixel_size();

            // ⚠️ 太宽就省略（列宽固定 ⇒ 长状态不挤邻居列）
            let avail = (cell_area.width() as f32 - PILL_PAD_X * 2.0 - DOT - DOT_GAP).max(1.0);
            if tw as f32 > avail {
                layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
                layout.set_width((avail as i32) * gtk::pango::SCALE);
                let (_, h2) = layout.pixel_size();
                tw = tw.min(avail as i32);
                th = h2;
            }

            // 文字色跟选择态走（namecell 的 cell_text_state：`:focus` 必须算）
            #[allow(deprecated)]
            let ctx = widget.style_context();
            let st = crate::panel::namecell::cell_text_state(flags, widget.is_focus());
            ctx.save();
            ctx.set_state(st);
            let text_color = ctx.color();
            ctx.restore();

            // 胶囊：垂直居中，水平贴列起点
            let pill_h = th as f32 + PILL_PAD_Y * 2.0;
            let pill_w = PILL_PAD_X * 2.0 + DOT + DOT_GAP + tw as f32;
            let px = cell_area.x() as f32;
            let py = cell_area.y() as f32 + (cell_area.height() as f32 - pill_h) / 2.0;
            let bounds = graphene::Rect::new(px, py, pill_w, pill_h);
            let radius = pill_h / 2.0;

            // 胶囊底：圆角裁剪 + 填色（GTK4 Snapshot 没有"画圆角矩形"的直接
            // 接口，push_rounded_clip + append_color 就是标准做法）
            let rr = gsk::RoundedRect::new(
                bounds,
                graphene::Size::new(radius, radius),
                graphene::Size::new(radius, radius),
                graphene::Size::new(radius, radius),
                graphene::Size::new(radius, radius),
            );
            snapshot.push_rounded_clip(&rr);
            snapshot.append_color(&bg, &bounds);

            // 圆点：小方块 + 全角圆角裁剪 = 圆
            let d = DOT - 1.0;
            let dot_bounds = graphene::Rect::new(
                px + PILL_PAD_X,
                py + (pill_h - d) / 2.0,
                d,
                d,
            );
            let drr = gsk::RoundedRect::new(
                dot_bounds,
                graphene::Size::new(d / 2.0, d / 2.0),
                graphene::Size::new(d / 2.0, d / 2.0),
                graphene::Size::new(d / 2.0, d / 2.0),
                graphene::Size::new(d / 2.0, d / 2.0),
            );
            snapshot.push_rounded_clip(&drr);
            snapshot.append_color(&dot, &dot_bounds);
            snapshot.pop();

            // 文字：圆点右侧，垂直居中
            let ly = py + (pill_h - th as f32) / 2.0;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                px + PILL_PAD_X + DOT + DOT_GAP,
                ly,
            ));
            snapshot.append_layout(&layout, &text_color);
            snapshot.restore();

            snapshot.pop(); // 胶囊裁剪
        }
    }
}

glib::wrapper! {
    pub struct PillCellRenderer(ObjectSubclass<imp::PillCellRenderer>)
        @extends gtk::CellRenderer;
}

/// 胶囊左右内边距。
const PILL_PAD_X: f32 = 8.0;
/// 胶囊上下内边距。
const PILL_PAD_Y: f32 = 3.0;
/// 圆点直径。
const DOT: f32 = 8.0;
/// 圆点与文字的间距。
const DOT_GAP: f32 = 6.0;

impl PillCellRenderer {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_text(&self, text: String) {
        self.imp().text.replace(text);
    }

    pub fn set_state(&self, state: PillState) {
        self.imp().state.set(state as u8);
    }

    /// 冒烟用：当前生效的档位。
    pub fn state(&self) -> PillState {
        PillState::from_u8(self.imp().state.get())
    }
}

impl PillState {
    /// u8 往返（StdCell 存档用）。越界一律回退 Dim（渲染不 panic）。
    fn from_u8(v: u8) -> Self {
        match v {
            0 => PillState::Ok,
            1 => PillState::Warn,
            2 => PillState::Err,
            _ => PillState::Dim,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 档位的 u8 往返必须无损；越界回退 Dim（不能 panic）。
    ///
    /// ⚠️ **别在这里 `PillCellRenderer::new()`**：libtest 跑在子线程，
    ///    GTK 对象只能在主线程建（AGENTS.md 的老坑）⇒ 渲染器本身
    ///    归冒烟管，单测只管纯逻辑。
    #[test]
    fn 档位往返无损() {
        for (s, v) in [
            (PillState::Ok, 0u8),
            (PillState::Warn, 1),
            (PillState::Err, 2),
            (PillState::Dim, 3),
        ] {
            assert_eq!(PillState::from_u8(v), s, "档位 {s:?} 的 u8 往返丢了");
        }
        assert_eq!(PillState::from_u8(99), PillState::Dim, "越界该回退 Dim");
    }

    /// 三档 + 灰档的颜色都必须是**合法颜色**（cairo/gdk 要 RGB）。
    #[test]
    fn 档位颜色都合法() {
        for s in [PillState::Ok, PillState::Warn, PillState::Err, PillState::Dim] {
            let (bg, dot) = s.colors();
            assert!(gdk::RGBA::parse(&bg).is_ok(), "{s:?} 底色 {bg} 不合法");
            assert!(gdk::RGBA::parse(&dot).is_ok(), "{s:?} 圆点 {dot} 不合法");
        }
    }

    /// 默认档是灰（Dim）：未知状态宁可低调，不许装绿。
    #[test]
    fn 默认档是灰() {
        assert_eq!(PillState::default(), PillState::Dim);
    }
}
