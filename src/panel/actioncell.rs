//! 「操作」列的单元格渲染器：一格里画**一排图标按钮**。
//!
//! ## 为什么自己画
//!
//! GTK4 **没有 `GtkCellRendererWidget`**（GTK3 才有）⇒ TreeView 的单元格里
//! **放不下真的 widget** ⇒ 「每行一排按钮」只能自己画：
//! - `CellRenderer` 子类（跟 [`crate::panel::pillcell`]、[`crate::panel::namecell`]
//!   同一套路）⇒ 用 `Snapshot` 把图标一个个画进去；
//! - **点哪儿算哪个** ⇒ 渲染器只管画；命中判定在
//!   [`ActionCellRenderer::hit_index`]（**纯函数** ⇒ 好测），
//!   由调用方拿 `TreeView::path_at_pos` 给的**格内坐标**去问。
//!
//! ## 画的是图标，不是按钮
//!
//! 真按钮（`gtk::Button`）有 hover、焦点、tooltip、键盘可达性；这里画的是
//! **图标** ⇒ 命中判定、tooltip、禁用态都得自己做：
//! - **禁用**：画的时候压一层透明度（`Snapshot::push_opacity`）⇒ 一眼看出点不了；
//! - **tooltip**：调用方接 `Widget::query-tooltip` ⇒ 按命中的图标给不同的说明。
//!
//! ## 阶段 157：图标**要看得见**（用户 2026-11-23：「整个界面很不美」）
//!
//! - ⚠️ **符号图标自带固定颜色**（Adwaita 是 **#bebebe**）⇒ 直接画出来是
//!   **浅灰**，在白底上对比度约 1.6:1 ⇒ **几乎看不见**。
//! - ⚠️ 试过「`GtkImage` ＋ `GtkWidgetPaintable`」⇒ **行不通**：
//!   `GtkImage` 只有在**已映射**（在窗口里、看得见）时，`WidgetPaintable`
//!   才画得出它（实测：不在层级里 ⇒ 画出来是空白）。
//!   ⇒ 不想为了几个图标去搞「藏起来的宿主控件」。
//! - ✅ **最终方案：GSK 色彩矩阵染色**——
//!   1. 从图标主题取**符号图标**（`IconTheme::lookup_icon`）；
//!   2. 画进一个 `gtk::Snapshot`，同时压一张**颜色矩阵**
//!      （`Snapshot::push_color_matrix`）⇒ 把图标那层固定浅灰
//!      **乘**成当前的前景色（深色主题就是浅色图标）；
//!   3. `Snapshot::to_node()` ⇒ 得到一个可复用的 `gtk::gsk::RenderNode`
//!      ⇒ 单元格里 `append_node` 画它。
//!   ⇒ **不需要任何隐藏控件**，颜色跟主题走（浅/深色都对）。
//!
//! - ⚠️ **常态就有一层极淡的按钮底盘**（悬停时加深）⇒ 一眼看出「这一格
//!   可以点」，而不是一排浮在白底上的"糊点"。

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, glib};

/// 图标的边长（像素）。
pub const ICON_SIZE: i32 = 16;
/// 图标之间的间距（像素）。
///
/// ⚠️ **阶段 157**：6 → 8（**留白更透气**）。以前一排图标挤在一起
///   ⇒ 看着像一坨"糊点"，不像一排按钮。
pub const ICON_GAP: i32 = 8;
/// 一个图标**占位**的宽度（图标 ＋ 间距）。
pub const SLOT_W: i32 = ICON_SIZE + ICON_GAP;
/// 按钮底盘的圆角（像素）。
const BASE_RADIUS: f32 = 8.0;
/// 按钮底盘比图标高出来的那一截（上下各一半）。
const BASE_PAD: f32 = 4.0;

/// 「操作」列里的一颗图标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    /// 图标名（**Adwaita 真有的** ⇒ 别瞎写，否则显示成空白方块）。
    pub icon: String,
    /// 能不能点（不能 ⇒ 画暗的、tooltip 说为什么）。
    pub enabled: bool,
}

impl Icon {
    pub fn new(icon: &str, enabled: bool) -> Self {
        Icon {
            icon: icon.to_string(),
            enabled,
        }
    }
}

/// **前景色 ⇒ 染色矩阵**（把符号图标那层固定浅灰乘成这个颜色）。
///
/// ⚠️ **矩阵是列优先**（`graphene::Matrix::from_float` 的顺序是
///   「一列一列」）⇒ 语义是 SVG 的 `feColorMatrix`：
///   `R' = r·R + g·G + b·B + a·A`、`G' = …`、`B' = …`、`A' = A`。
/// ⇒ 只想「把 RGB 乘上前景色」⇒ 对角线上放前景色的 r/g/b 即可。
///
/// ⚠️ **结果比前景色略深**（符号图标本身是 #bebebe ≈ 0.745 ⇒ 乘完是
///   `前景色 × 0.745`）⇒ 浅色主题里图标比文字稍重一点，**正好更醒目**。
pub fn tint_matrix(color: &gdk::RGBA) -> graphene::Matrix {
    graphene::Matrix::from_float([
        color.red(), 0.0, 0.0, 0.0, // R 行
        0.0, color.green(), 0.0, 0.0, // G 行
        0.0, 0.0, color.blue(), 0.0, // B 行
        0.0, 0.0, 0.0, 1.0, // A 行（不变）
    ])
}

/// **自绘图标**（名字以 `pp-` 开头）⇒ 自己画一条路径。
///
/// ⚠️ **为什么要自绘**（阶段 157）：Adwaita 只有**实心**的播放 / 停止图标
///   （▶ / ■）⇒ 混在一排**线性**图标里，**视觉重量完全不对**
///   （实测截图：实心黑方块在一排细线图标里特别扎眼）。
///   ⇒ 自己画**描边**版本 ⇒ 一整排图标是**同一套语言**。
///
/// ⚠️ **描边线宽**：2.0px（跟 Adwaita 符号图标的笔画粗细接近）。
///
/// ⚠️ **阶段 164：只有「启动 / 停止」自绘**（用户 2026-11-24：「这几个图标
///   实在不够精致」）——**自绘的是因为主题里那两个是实心的**（▶ / ■）⇒ 混在
///   一排线性图标里特别扎眼（阶段 157 的老结论）；而「重启 / 重建」**用主题
///   自己的**符号图标（`view-refresh-symbolic` / `edit-copy-symbolic`）⇒
///   它们本来就是按 16px 设计的 ⇒ 比自己画的清楚（实测：自己画的圆弧箭头
///   在 16px 下**看不出箭头**，自己画的叠方框**糊成一团**）。
fn custom_node(name: &str, color: &gdk::RGBA) -> Option<gtk::gsk::RenderNode> {
    let s = ICON_SIZE as f32;
    let mut pb = gtk::gsk::PathBuilder::new();
    match name {
        // ▶ 播放 / 启动：等腰三角形（描边）
        "pp-play" => {
            pb.move_to(s * 0.28, s * 0.17);
            pb.line_to(s * 0.80, s * 0.50);
            pb.line_to(s * 0.28, s * 0.83);
            pb.close();
        }
        // ■ 停止：方框（描边）
        "pp-stop" => {
            let m = s * 0.22;
            pb.move_to(m, m);
            pb.line_to(s - m, m);
            pb.line_to(s - m, s - m);
            pb.line_to(m, s - m);
            pb.close();
        }
        _ => return None,
    }
    let path = pb.to_path();
    let mut snap = gtk::Snapshot::new();
    // ⚠️ **线宽 2.0 + 圆角接头** ⇒ 跟 Adwaita 符号图标的笔画粗细一致
    //   （1.6 太细 ⇒ 跟旁边那排图标不是一个重量级）。
    let mut stroke = gtk::gsk::Stroke::new(2.0);
    stroke.set_line_cap(gtk::gsk::LineCap::Round);
    stroke.set_line_join(gtk::gsk::LineJoin::Round);
    snap.append_stroke(&path, &stroke, color);
    snap.to_node()
}

/// **图标组在格子里的左边缘**（**左对齐** ⇒ 紧跟在上一列后面）。
///
/// ⚠️ **阶段 158：曾经是右对齐**（贴着格子的右边）——那是因为「操作」列
///   **吸收了表格的富余宽度**（一个 480px 宽的列里只有 4 颗 16px 的图标
///   贴在最右边 ⇒ 中间一大片空白 ⇒ 用户 2026-11-24：「操作列太宽了，按钮们
///   太靠右了，没有一点留白」）。
/// ⚠️ **阶段 164 改成左对齐**：
/// - 「操作」列**不再吸收富余**（固定窄宽）⇒ 按钮跟着列走，**离窗口右缘
///   有一截留白**（列宽里图标右边那一截就是留白）；
/// - 列头也跟着左对齐 ⇒ **「操作」两个字正好压在第一颗图标上面**
///   （右对齐的话标题会跑到图标右边，对不上）。
/// - ⚠️ **命中判定必须跟绘制用同一个算术** ⇒ 画和判都调这个函数
///   （以前一个在渲染器里、一个在 `dockerui` 里各写一份 ⇒ 迟早漂移）。
pub fn group_offset(_cell_w: i32, _n: usize) -> i32 {
    0
}

/// **格内坐标 ⇒ 命中第几颗图标**（**纯函数** ⇒ 画与判共用一份算术）。
///
/// ⚠️ **`n` 必须是「这一行」有几颗** ⇒ 不能用渲染器里存着的那个
///   （渲染器里存的是**最后绘制的那一行**的图标 ⇒ 拿它算会错位）。
///
/// ⚠️ 图标组**左对齐**（阶段 164）⇒ 先减去组在格内的左边缘。
pub fn hit_index_at(cell_x: i32, cell_w: i32, n: usize) -> Option<usize> {
    if n == 0 || cell_x < 0 {
        return None;
    }
    let off = group_offset(cell_w, n);
    if cell_x < off {
        // 点在图标组**左边**的空白处 ⇒ 不算命中。
        return None;
    }
    let idx = (cell_x - off) / SLOT_W;
    if idx < 0 || idx as usize >= n {
        None
    } else {
        Some(idx as usize)
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ActionCellRenderer {
        /// 当前这一行的图标（**每次画之前由 `cell_data_func` 换掉**）。
        pub icons: RefCell<Vec<Icon>>,
        /// 图标名 ⇒ **画它用的节点**（`gtk::gsk::RenderNode`，已染色）。
        ///
        /// ⚠️ **为什么要缓存**（阶段 157）：每帧新建 `Snapshot` + 查图标主题
        ///   ⇒ 表格重画很频繁 ⇒ 缓存下来复用。
        /// ⚠️ **颜色变了要重画**（浅色/深色主题切换）⇒ 连颜色一起存。
        pub nodes: RefCell<HashMap<String, (gtk::gsk::RenderNode, gdk::RGBA, i32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ActionCellRenderer {
        const NAME: &'static str = "PpActionCellRenderer";
        type Type = super::ActionCellRenderer;
        type ParentType = gtk::CellRenderer;
    }

    impl ObjectImpl for ActionCellRenderer {}

    impl ActionCellRenderer {
        /// 图标名 ＋ 颜色 ⇒ **画它用的节点**（缓存；见外层同名方法的说明）。
        ///
        /// ⚠️ 查不到图标（名字写错 / 主题里没有）⇒ 返回 None ⇒ **那一格
        ///   留空**（而不是画个空白方块吓人）。
        pub(super) fn node_for(
            &self,
            name: &str,
            color: &gdk::RGBA,
            scale: i32,
        ) -> Option<gtk::gsk::RenderNode> {
            // ⚠️ **自绘图标**（`pp-` 开头）不走图标主题 ⇒ 单独一条路。
            if name.starts_with("pp-") {
                return custom_node(name, color);
            }
            {
                let cache = self.nodes.borrow();
                if let Some((node, c, s)) = cache.get(name) {
                    if c == color && *s == scale {
                        return Some(node.clone());
                    }
                }
            }
            let disp = gdk::Display::default()?;
            let theme = gtk::IconTheme::for_display(&disp);
            let paintable = theme.lookup_icon(
                name,
                &[],
                ICON_SIZE,
                scale,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            // ⚠️ **染色就在这里做**：把快照里的那层固定浅灰乘成前景色。
            let mut snap = gtk::Snapshot::new();
            snap.push_color_matrix(&tint_matrix(color), &graphene::Vec4::new(0.0, 0.0, 0.0, 0.0));
            paintable.snapshot(&mut snap, ICON_SIZE as f64, ICON_SIZE as f64);
            // ⚠️ **必须把状态弹回去**（GTK4 的 `gtk_snapshot_to_node` 见到
            //   没弹干净的状态会警告「Too many gtk_snapshot_push() calls」）。
            //   颜色矩阵在**画图标那一刻**就已经生效了 ⇒ 弹掉不影响结果。
            snap.pop();
            let node = snap.to_node()?;
            self.nodes.borrow_mut().insert(
                name.to_string(),
                (node.clone(), color.clone(), scale),
            );
            Some(node)
        }
    }

    impl CellRendererImpl for ActionCellRenderer {
        /// 列宽**由列自己定**（`min_width`）⇒ 这里不撑列
        /// （内容是图标 ⇒ 跟文字列不一样，撑出来会很难看）。
        fn preferred_width<P: IsA<gtk::Widget>>(&self, _widget: &P) -> (i32, i32) {
            (1, 1)
        }

        fn preferred_height<P: IsA<gtk::Widget>>(&self, _widget: &P) -> (i32, i32) {
            (ICON_SIZE + 8, ICON_SIZE + 8)
        }

        fn snapshot<P: IsA<gtk::Widget>>(
            &self,
            snapshot: &gtk::Snapshot,
            widget: &P,
            _background_area: &gdk::Rectangle,
            cell_area: &gdk::Rectangle,
            flags: gtk::CellRendererState,
        ) {
            let icons = self.icons.borrow();
            if icons.is_empty() {
                return;
            }
            // ⚠️ **颜色要跟「选中 / 聚焦」状态走**（跟 `namecell` 一个口径）：
            //   聚焦的选中行是**实底 accent** ⇒ 图标得是 on_accent（白），
            //   否则就是「蓝底黑字」那种看不见（用户 2026-11-11 踩过）。
            #[allow(deprecated)]
            let ctx = widget.style_context();
            let state = crate::panel::namecell::cell_text_state(flags, widget.is_focus());
            ctx.save();
            ctx.set_state(state);
            let fg = ctx.color();
            ctx.restore();
            let scale = widget.scale_factor();

            // ⚠️ **悬停（PRELIT）⇒ 整格加一条淡底**（阶段 157）。
            //
            // 以前这一格是**纯白底 + 一排灰图标** ⇒ 看着像"糊点"，不像能点。
            // ⇒ 现在常态就有一层极淡的**按钮底盘**，悬停时加深 ⇒ 一眼看出
            //   「这一格可以点」。
            //
            // ⚠️ **底盘只盖图标那一段**（不是整个单元格）⇒ 最后一颗图标右边
            //   不会拖一条空荡荡的灰条。
            let hovered = flags.contains(gtk::CellRendererState::PRELIT);
            let group_w = (icons.len() as i32 * SLOT_W - ICON_GAP).max(ICON_SIZE);
            // ⚠️ **左对齐**（阶段 164）⇒ 图标组贴着格子的左边。
            let x0 = cell_area.x() as f32 + group_offset(cell_area.width(), icons.len()) as f32;
            let y0 = cell_area.y() as f32 + (cell_area.height() as f32 - ICON_SIZE as f32) / 2.0;
            {
                let mut bg = fg.clone();
                bg.set_alpha(if hovered { 0.16 } else { 0.08 });
                let rect = graphene::Rect::new(
                    x0,
                    y0 - BASE_PAD,
                    group_w as f32,
                    (ICON_SIZE as f32 + BASE_PAD * 2.0),
                );
                let rr = gtk::gsk::RoundedRect::from_rect(rect, BASE_RADIUS);
                snapshot.push_rounded_clip(&rr);
                snapshot.append_color(&bg, &rect);
                snapshot.pop();
            }

            for (i, ic) in icons.iter().enumerate() {
                // ⚠️ **图标名必须是 Adwaita 真有的**（AGENTS.md 的老坑）：
                //    写错 ⇒ 显示成空白方块 ⇒ 用户以为功能坏了。
                let Some(node) = self.node_for(&ic.icon, &fg, scale) else {
                    continue;
                };
                let x = x0 + i as f32 * SLOT_W as f32;
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, y0));
                // 禁用 ⇒ 压一层透明度（一眼看出点不了）。
                if !ic.enabled {
                    snapshot.push_opacity(0.4);
                }
                snapshot.append_node(&node);
                if !ic.enabled {
                    snapshot.pop();
                }
                snapshot.restore();
            }
        }
    }
}

glib::wrapper! {
    pub struct ActionCellRenderer(ObjectSubclass<imp::ActionCellRenderer>)
        @extends gtk::CellRenderer;
}

impl ActionCellRenderer {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// 换掉**当前这一行**的图标（`cell_data_func` 每画一行调一次）。
    pub fn set_icons(&self, icons: Vec<Icon>) {
        self.imp().icons.replace(icons);
    }

    /// 当前这一行的图标（测试钩子）。
    pub fn icons(&self) -> Vec<Icon> {
        self.imp().icons.borrow().clone()
    }

    /// **格内坐标** ⇒ 命中第几颗图标。
    ///
    /// ⚠ 坐标是**格内**的（`TreeView::path_at_pos` 给的 `cell_x`）
    ///    ⇒ 不用管列在窗口里的位置。
    /// ⚠ **图标组是左对齐的**（阶段 164）⇒ 得先减去它在格内的左边缘
    ///    （`group_offset`）⇒ 跟绘制用的是同一套算术。
    /// ⚠ 超出这一行的图标数 ⇒ None（点到空白处 ⇒ 不当作点了哪颗）。
    pub fn hit_index(&self, cell_x: i32, cell_w: i32) -> Option<usize> {
        let n = self.imp().icons.borrow().len();
        hit_index_at(cell_x, cell_w, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 命中判定：**纯逻辑**（不碰 GTK ⇒ 单测里能跑）。
    ///
    /// ⚠️ 渲染器本身归冒烟管（GTK 对象只能在主线程建 ⇒ AGENTS.md 的老坑）。
    #[test]
    fn 命中判定是纯算术() {
        // 一个图标占 SLOT_W；从格子的左边缘开始排。
        assert_eq!(0 / SLOT_W, 0);
        assert_eq!((SLOT_W - 1) / SLOT_W, 0, "第一个占位还没走完");
        assert_eq!(SLOT_W / SLOT_W, 1, "第二个占位开始了");
        assert_eq!((3 * SLOT_W) / SLOT_W, 3);
        // 负坐标 / 越界 ⇒ 不算命中
        assert_eq!((-1i32).div_euclid(SLOT_W), -1);
    }

    /// **阶段 164：图标组左对齐** ⇒ 左边缘恒为 0（紧跟上一列）。
    ///
    /// ⚠️ **阶段 158 曾经是右对齐**（贴着格子右边）⇒ 那是「操作」列吸收了
    ///   表格富余宽度造成的 ⇒ 用户 2026-11-24 报「操作列太宽、按钮太靠右」。
    ///   ⇒ 现在「操作」列固定窄宽 ＋ 图标左对齐 ⇒ 窗口右缘留出一截空白。
    #[test]
    fn 图标组左对齐() {
        // 5 颗图标 ⇒ 组宽 = 5×24 − 8 = 112（宽度还是那个宽度）。
        let group_w = 5 * SLOT_W - ICON_GAP;
        assert_eq!(group_w, 112);
        // ⚠️ **左边缘恒为 0**（不管格子多宽、几颗图标）。
        assert_eq!(group_offset(200, 5), 0);
        assert_eq!(group_offset(group_w, 5), 0);
        assert_eq!(group_offset(10, 5), 0);
        // 0 颗图标 ⇒ 也不能算出负偏移。
        assert_eq!(group_offset(200, 0), 0);
    }

    /// **阶段 164：命中判定跟绘制用同一套算术** ⇒ 左对齐后格内坐标直接
    /// 除以槽宽就是第几颗。
    #[test]
    fn 命中判定按左对齐算() {
        // 4 颗图标 ⇒ 第 0/1/2/3 颗分别在各自槽位里。
        assert_eq!(hit_index_at(0, 120, 4), Some(0));
        assert_eq!(hit_index_at(SLOT_W - 1, 120, 4), Some(0));
        assert_eq!(hit_index_at(SLOT_W, 120, 4), Some(1));
        assert_eq!(hit_index_at(3 * SLOT_W, 120, 4), Some(3));
        // 超出最后一颗 ⇒ 空白处 ⇒ 不算命中。
        assert_eq!(hit_index_at(4 * SLOT_W, 120, 4), None);
        // 负坐标 / 0 颗图标 ⇒ 不命中。
        assert_eq!(hit_index_at(-1, 120, 4), None);
        assert_eq!(hit_index_at(0, 120, 0), None);
    }

    #[test]
    fn 图标槽位宽度是正的() {
        assert!(SLOT_W > ICON_SIZE);
        assert!(ICON_SIZE >= 12, "太小了看不清");
    }

    /// 染色矩阵：**对角线上是前景色**（其余是 0）⇒ 图标被染成前景色。
    ///
    /// ⚠️ 这条是「图标看得见」的地基（阶段 157：#bebebe 的符号图标
    ///   在白底上对比度只有 1.6:1 ⇒ 几乎看不见）。
    #[test]
    fn 染色矩阵把前景色放在对角线上() {
        let c = gdk::RGBA::new(0.2, 0.4, 0.6, 1.0);
        let m = tint_matrix(&c);
        let v = m.to_float();
        // 列优先：第 0 列 = (R→R, R→G, R→B, R→A)
        assert!((v[0] - 0.2).abs() < 1e-6, "R→R 应是前景色的红：{}", v[0]);
        assert!((v[1] - 0.0).abs() < 1e-6);
        assert!((v[2] - 0.0).abs() < 1e-6);
        // 第 1 列 = (G→R, G→G, G→B, G→A)
        assert!((v[4] - 0.0).abs() < 1e-6);
        assert!((v[5] - 0.4).abs() < 1e-6, "G→G 应是前景色的绿：{}", v[5]);
        // 第 2 列 = (B→R, B→G, B→B, B→A)
        assert!((v[10] - 0.6).abs() < 1e-6, "B→B 应是前景色的蓝：{}", v[10]);
        // 第 3 列 = (A→R, A→G, A→B, A→A) ⇒ alpha 不变
        assert!((v[15] - 1.0).abs() < 1e-6, "alpha 应保持 1：{}", v[15]);
    }
}
