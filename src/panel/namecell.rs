//! 文件名单元格：只让**文件名文字**横向滚动，右侧「大小/权限/修改时间」
//! 保持不动。
//!
//! 为什么不用 TreeView 自带的横向滚动：那滚的是整张表，滚到右边时其他列
//! （甚至文件名的表头）都会跑出视口；而这里希望列表保持很窄、所有列都在，
//! 只把过长的文件名在名称列里左右滑动。
//!
//! 实现：自绘 `CellRenderer`，把文字画在 `x - offset` 位置并裁剪到单元格。
//! `offset` 由面板底部（名称列正下方）的窄滚动条驱动。

use std::cell::{Cell as StdCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct NameCellRenderer {
        pub text: RefCell<String>,
        /// 名称列向右滚动的偏移（像素）
        pub offset: StdCell<f64>,
        /// 传输进度文案（"45%"/"完成"/""）；空串 = 不画进度
        pub progress: RefCell<String>,
        pub progress_pct: StdCell<i32>,
        /// **这一行正在传输**（上传或下载）⇒ 画呼吸背景 + 左侧竖条。
        ///
        /// 干什么用（用户 2026-11-10）：「上传/下载的文件背景色可以呼吸
        /// 变化，这样更明显些」——进度文字在窄面板里经常被挤掉，光靠
        /// 那几个字不够显眼 ⇒ 整行给个会呼吸的底色。
        pub xfer: StdCell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NameCellRenderer {
        const NAME: &'static str = "PpNameCellRenderer";
        type Type = super::NameCellRenderer;
        type ParentType = gtk::CellRenderer;
    }

    impl ObjectImpl for NameCellRenderer {}

    impl CellRendererImpl for NameCellRenderer {
        fn preferred_width<P: IsA<gtk::Widget>>(&self, _widget: &P) -> (i32, i32) {
            // 名称列的宽度由列自己的 min_width/expand 决定；内容再长也不撑列
            (1, 1)
        }

        fn preferred_height<P: IsA<gtk::Widget>>(&self, widget: &P) -> (i32, i32) {
            let layout = widget.create_pango_layout(Some("Ag"));
            let (_, h) = layout.pixel_size();
            (h + 4, h + 4)
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
            let progress = self.progress.borrow().clone();
            let pct = self.progress_pct.get().clamp(0, 100);

            // ⚠️ **传输中的行：背景呼吸**（用户 2026-11-10）。
            //
            // 画在**文字底下**（append_color 先画，后面的文字压在上面），
            // 左侧再补一条实色竖条 ⇒ 眼睛扫一列就能挑出"哪几行在传"。
            if self.xfer.get() {
                let accent = crate::panel::pulse::accent(widget);
                let mut bg = accent;
                bg.set_alpha(crate::panel::pulse::alpha(0.06, 0.26));
                snapshot.append_color(
                    &bg,
                    &graphene::Rect::new(
                        cell_area.x() as f32,
                        cell_area.y() as f32,
                        cell_area.width() as f32,
                        cell_area.height() as f32,
                    ),
                );
                let mut bar = accent;
                bar.set_alpha(0.85);
                snapshot.append_color(
                    &bar,
                    &graphene::Rect::new(
                        cell_area.x() as f32,
                        cell_area.y() as f32,
                        3.0,
                        cell_area.height() as f32,
                    ),
                );
            }

            #[allow(deprecated)]
            let ctx = widget.style_context();
            let mut state = gtk::StateFlags::NORMAL;
            if flags.contains(gtk::CellRendererState::INSENSITIVE) {
                state |= gtk::StateFlags::INSENSITIVE;
            }
            if flags.contains(gtk::CellRendererState::SELECTED) {
                state |= gtk::StateFlags::SELECTED;
            }
            ctx.save();
            ctx.set_state(state);
            let color = ctx.color();
            ctx.restore();

            let mut layout = widget.create_pango_layout(Some(&text));
            let (tw, th) = layout.pixel_size();
            let x = cell_area.x() as f32 - self.offset.get() as f32;
            let y = cell_area.y() as f32 + (cell_area.height() as f32 - th as f32) / 2.0;
            let cell_w = cell_area.width() as f32;

            // 字符足够宽才画进度：极窄面板优先保住文件名
            let label = if progress.is_empty() || cell_w < 96.0 {
                None
            } else {
                let l = widget.create_pango_layout(Some(&progress));
                let (lw, lh) = l.pixel_size();
                Some((l, lw as f32, lh as f32))
            };
            let reserve = label.as_ref().map_or(0.0, |(_, lw, _)| lw + 12.0);

            // ⚠️ 阶段 76 / 审计 P1-15：太宽就画**省略号**。
            // 原来是硬裁 ⇒ 用户以为文件名就这么多（截图里
            // `AndroidStudioProje` 看不出后面还有字）。
            //
            // ⚠️ **滚动时不省略**：名称列下面那条窄滚动条就是给"把长名字
            // 看完"用的（offset > 0 时画原样，滑过去能看全）。
            let avail = (cell_w - reserve).max(1.0);
            if self.offset.get() <= 0.5 && (tw as f32) > avail {
                layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
                layout.set_width((avail as i32) * gtk::pango::SCALE);
                // ⚠️ 不设这个，长名字会**换行**（列只有一行高 ⇒ 文字被切）
                layout.set_single_paragraph_mode(true);
            }

            // 裁剪到单元格：滚动后左边超出的部分不画到图标列上
            snapshot.push_clip(&graphene::Rect::new(
                cell_area.x() as f32,
                cell_area.y() as f32,
                cell_w,
                cell_area.height() as f32,
            ));

            // 文件名只画到进度文案左侧，避免长名字压住进度
            snapshot.push_clip(&graphene::Rect::new(
                cell_area.x() as f32,
                cell_area.y() as f32,
                (cell_w - reserve).max(1.0),
                cell_area.height() as f32,
            ));
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x, y));
            snapshot.append_layout(&layout, &color);
            snapshot.restore();
            snapshot.pop();

            if let Some((pl, lw, lh)) = label {
                // 百分比：靠右、垂直居中
                let px = cell_area.x() as f32 + cell_w - lw - 4.0;
                let py = cell_area.y() as f32 + (cell_area.height() as f32 - lh) / 2.0;
                snapshot.save();
                snapshot.translate(&graphene::Point::new(px, py));
                snapshot.append_layout(&pl, &color);
                snapshot.restore();

                // 底部一条 2px 细进度条：不抢文字视觉，但一眼能看出快慢
                let bar_x = cell_area.x() as f32 + 2.0;
                let bar_y = (cell_area.y() + cell_area.height()) as f32 - 3.0;
                let bar_w = (cell_w - 4.0).max(1.0);
                let mut track = color.clone();
                track.set_alpha(0.18);
                snapshot.append_color(
                    &track,
                    &graphene::Rect::new(bar_x, bar_y, bar_w, 2.0),
                );
                let mut fill = color;
                fill.set_alpha(0.75);
                snapshot.append_color(
                    &fill,
                    &graphene::Rect::new(bar_x, bar_y, bar_w * pct as f32 / 100.0, 2.0),
                );
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct NameCellRenderer(ObjectSubclass<imp::NameCellRenderer>)
        @extends gtk::CellRenderer;
}

impl NameCellRenderer {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_text(&self, text: String) {
        self.imp().text.replace(text);
    }

    pub fn set_offset(&self, offset: f64) {
        self.imp().offset.set(offset.max(0.0));
    }

    pub fn set_progress(&self, text: String, pct: i32) {
        self.imp().progress.replace(text);
        self.imp().progress_pct.set(pct.clamp(0, 100));
    }

    /// 这一行**正在传输**（上传或下载）⇒ 画呼吸背景。
    pub fn set_xfer(&self, on: bool) {
        self.imp().xfer.set(on);
    }
}
