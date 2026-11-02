//! ER 图视图（阶段 19：Navicat 的 Database Model）。
//!
//! **为什么表框用真控件、连线用画布画**：
//! - 表框里的文字是**真 Label**——中文、字体、主题色全都自动对。
//!   用 cairo 的"玩具文字"接口画中文会变成一堆问号（它只认 ASCII）。
//! - 连线画在背景的 `DrawingArea` 上（画布只画线、不画字），
//!   线的端点按模型里的坐标算，跟表框里的行一一对应——哪根线连哪两列，
//!   一眼看得出来。
//!
//! **交互**：拖表框挪位置；双击表框打开它的数据页签；
//! 工具栏有「重新布局」和「放大 / 缩小」。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::ermodel::{self, ErGraph};
use crate::i18n::tr;
use crate::trf;

/// 缩放档位（几何缩放倍数 ↔ CSS 字号类）。
const ZOOMS: [f64; 4] = [0.75, 1.0, 1.5, 2.0];
/// 缩放档位对应的 CSS 类（字号）。
const ZOOM_CLASSES: [&str; 4] = ["er-zoom-0", "er-zoom-1", "er-zoom-2", "er-zoom-3"];

/// 一个 ER 图页签。
pub struct ErView {
    /// 整页（工具栏 + 画布）。
    pub root: gtk::Box,
    /// 画布：表框是它的子控件，背景 DrawingArea 画连线。
    pub canvas: gtk::Fixed,
    /// 背景画布（**要给它尺寸**，否则 0×0，一条线都画不出来）
    bg: gtk::DrawingArea,
    /// 状态行（多少张表、多少条关系）。
    pub status: gtk::Label,
    /// 图数据（拖动时也要更新坐标）。
    graph: Rc<RefCell<ErGraph>>,
    /// 缩放倍数。
    scale: Rc<Cell<f64>>,
    /// 缩放档位下标。
    zoom_idx: Cell<usize>,
    /// 表名 → 表框控件。
    boxes: Rc<RefCell<HashMap<String, gtk::Box>>>,
    /// 双击表框 → 打开这张表的数据页签（参数 = 表名）
    on_open: RefCell<Option<Box<dyn Fn(&str)>>>,
}

impl ErView {
    /// 新建一个 ER 图页（工具栏 + 空画布）。
    pub fn new() -> Rc<ErView> {
        let graph = Rc::new(RefCell::new(ErGraph::default()));
        let scale = Rc::new(Cell::new(1.0));
        let boxes: Rc<RefCell<HashMap<String, gtk::Box>>> = Rc::new(RefCell::new(HashMap::new()));

        // ---- 背景画布：只画外键连线 ----
        let bg = gtk::DrawingArea::new();
        {
            let graph = Rc::clone(&graph);
            let scale = Rc::clone(&scale);
            bg.set_draw_func(move |_area, cr, _w, _h| {
                let g = graph.borrow();
                let s = scale.get();
                for e in &g.edges {
                    let Some(((x1, y1), (x2, y2))) = ermodel::edge_points(&g, e) else {
                        continue;
                    };
                    let (x1, y1, x2, y2) = (
                        x1 as f64 * s,
                        y1 as f64 * s,
                        x2 as f64 * s,
                        y2 as f64 * s,
                    );
                    cr.set_source_rgba(0.55, 0.6, 0.7, 0.9);
                    cr.set_line_width(1.4);
                    cr.move_to(x1, y1);
                    cr.line_to(x2, y2);
                    cr.stroke();
                    // 箭头指向**被引用的那张表**（父表）
                    let dx = x2 - x1;
                    let dy = y2 - y1;
                    let len = (dx * dx + dy * dy).sqrt();
                    if len < 1.0 {
                        continue;
                    }
                    let ux = dx / len;
                    let uy = dy / len;
                    let size = 7.0;
                    cr.move_to(x2, y2);
                    cr.line_to(
                        x2 - ux * size - uy * size * 0.5,
                        y2 - uy * size + ux * size * 0.5,
                    );
                    cr.line_to(
                        x2 - ux * size + uy * size * 0.5,
                        y2 - uy * size - ux * size * 0.5,
                    );
                    cr.close_path();
                    cr.fill();
                }
            });
        }

        let canvas = gtk::Fixed::new();
        canvas.set_size_request(400, 300);
        // 背景先放进去，后放的表框才压在上面
        canvas.put(&bg, 0.0, 0.0);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_vexpand(true);
        scroll.set_child(Some(&canvas));
        scroll.add_css_class("frame");

        // ---- 工具栏 ----
        let btn_relayout = crate::a11y::icon_button("view-refresh-symbolic");
        btn_relayout.set_tooltip_text(Some(tr("重新排布（表多了不好看了就点它）")));
        let btn_zoom_out = crate::a11y::icon_button("zoom-out-symbolic");
        btn_zoom_out.set_tooltip_text(Some(tr("缩小")));
        let btn_zoom_in = crate::a11y::icon_button("zoom-in-symbolic");
        btn_zoom_in.set_tooltip_text(Some(tr("放大")));
        let status = gtk::Label::new(Some(tr("（还没画图）")));
        status.add_css_class("dim-label");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&btn_relayout);
        bar.append(&btn_zoom_out);
        bar.append(&btn_zoom_in);
        bar.append(&status);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.set_vexpand(true);
        root.append(&bar);
        root.append(&scroll);

        let view = Rc::new(ErView {
            root,
            canvas,
            bg,
            status,
            graph,
            scale,
            zoom_idx: Cell::new(1),
            boxes,
            on_open: RefCell::new(None),
        });

        // 重新布局
        {
            let view = Rc::clone(&view);
            btn_relayout.connect_clicked(move |_| {
                let mut g = view.graph.borrow_mut();
                ermodel::layout(&mut g);
                drop(g);
                view.place();
            });
        }
        // 缩放
        {
            let view = Rc::clone(&view);
            btn_zoom_out.connect_clicked(move |_| {
                if view.zoom_idx.get() > 0 {
                    view.zoom_idx.set(view.zoom_idx.get() - 1);
                    view.apply_zoom();
                }
            });
        }
        {
            let view = Rc::clone(&view);
            btn_zoom_in.connect_clicked(move |_| {
                if view.zoom_idx.get() + 1 < ZOOMS.len() {
                    view.zoom_idx.set(view.zoom_idx.get() + 1);
                    view.apply_zoom();
                }
            });
        }
        view
    }

    /// 双击表框时回调（打开这张表的数据页签）。
    pub fn set_on_open(&self, f: impl Fn(&str) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    /// 换一张图（重新画）。
    pub fn set_graph(self: &Rc<Self>, graph: ErGraph) {
        let n_tables = graph.tables.len();
        let n_edges = graph.edges.len();
        *self.graph.borrow_mut() = graph;
        self.rebuild();
        self.status.set_text(&trf!("{n_tables} 张表 · {n_edges} 条关系（拖动表框可以挪位置，双击表框看数据）", n_tables = n_tables, n_edges = n_edges));
    }

    /// 状态行（"正在读…" / 失败原因）。
    pub fn set_status(&self, text: &str) {
        self.status.set_text(text);
    }

    /// 当前图（测试钩子用）。
    pub fn graph(&self) -> ErGraph {
        self.graph.borrow().clone()
    }

    /// 缩放档位（测试钩子用）。
    pub fn zoom_level(&self) -> usize {
        self.zoom_idx.get()
    }

    /// 重新画所有表框。
    fn rebuild(self: &Rc<Self>) {
        // 清掉旧的表框
        self.boxes.borrow_mut().clear();
        let mut children: Vec<gtk::Widget> = Vec::new();
        // 背景画布要留在最底下
        if let Some(bg) = self.canvas.first_child() {
            children.push(bg);
        }
        while let Some(c) = self.canvas.last_child() {
            if children.iter().any(|w| w == &c) {
                break;
            }
            self.canvas.remove(&c);
        }
        let graph = self.graph.borrow().clone();
        let s = self.scale.get();
        for t in &graph.tables {
            let w = (ermodel::TABLE_W as f64 * s).round() as i32;
            let head_h = (ermodel::HEADER_H as f64 * s).round() as i32;
            let row_h = (ermodel::ROW_H as f64 * s).round() as i32;
            let box_ = self.build_table_box(t, w, head_h, row_h);
            self.canvas.put(&box_, 0.0, 0.0);
            self.boxes
                .borrow_mut()
                .insert(t.name.clone(), box_.clone());
        }
        self.place();
    }

    /// 一个表框（表名 + 一行一个列）。
    fn build_table_box(
        self: &Rc<Self>,
        t: &ermodel::ErTable,
        w: i32,
        head_h: i32,
        row_h: i32,
    ) -> gtk::Box {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        outer.add_css_class("er-table");
        // 高度按模型来：连线端点是按「表头 + 每行行高」算的，
        // 框高不一致的话线会连到别的行上
        let h = if t.h > 0 { t.h } else { t.height() };
        outer.set_size_request(w, h);

        // 表头
        let title = gtk::Label::new(Some(&t.name));
        title.add_css_class("er-table-title");
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_size_request(-1, head_h);
        outer.append(&title);

        // 列
        for c in &t.columns {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            row.add_css_class("er-col");
            row.set_size_request(-1, row_h);
            let mark = gtk::Label::new(Some(if c.is_pk && c.is_fk {
                "PK·FK"
            } else if c.is_pk {
                "PK"
            } else if c.is_fk {
                "FK"
            } else {
                ""
            }));
            mark.add_css_class("er-mark");
            mark.set_xalign(0.0);
            mark.set_width_chars(5);
            let name = gtk::Label::new(Some(&c.name));
            name.set_xalign(0.0);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.set_hexpand(true);
            let ty = gtk::Label::new(Some(&c.data_type));
            ty.add_css_class("er-type");
            ty.set_xalign(1.0);
            ty.set_ellipsize(gtk::pango::EllipsizeMode::End);
            ty.set_width_chars(8);
            row.append(&mark);
            row.append(&name);
            row.append(&ty);
            outer.append(&row);
        }

        // 拖动挪位置
        {
            let view = Rc::clone(self);
            let name = t.name.clone();
            let drag = gtk::GestureDrag::new();
            drag.set_button(1);
            outer.add_controller(drag.clone());
            let pos = Rc::new(Cell::new((t.x, t.y)));
            drag.connect_drag_begin({
                let pos = Rc::clone(&pos);
                move |_g, x, y| {
                    pos.set((x.round() as i32, y.round() as i32));
                }
            });
            drag.connect_drag_update({
                let view = Rc::clone(&view);
                let pos = Rc::clone(&pos);
                let outer = outer.clone();
                move |_g, dx, dy| {
                    let (x, y) = pos.get();
                    let (nx, ny) = (x + dx.round() as i32, y + dy.round() as i32);
                    pos.set((nx, ny));
                    let s = view.scale.get();
                    view.canvas
                        .move_(&outer, (nx as f64 * s).round(), (ny as f64 * s).round());
                    // 模型里的坐标也更新（重排时别跳回去）
                    let mut g = view.graph.borrow_mut();
                    if let Some(t) = g.tables.iter_mut().find(|t| t.name == name) {
                        t.x = nx;
                        t.y = ny;
                    }
                }
            });
        }

        // 双击 → 打开这张表
        {
            let view = Rc::clone(self);
            let name = t.name.clone();
            let click = gtk::GestureClick::new();
            click.set_button(1);
            outer.add_controller(click.clone());
            // 第二个参数就是按压次数（gtk4-rs 没绑 n_press，但信号参数里有）
            click.connect_pressed(move |_g, n_press, _x, _y| {
                if n_press < 2 {
                    return;
                }
                let cb = view.on_open.borrow();
                if let Some(f) = cb.as_ref() {
                    f(&name);
                }
            });
        }
        outer
    }

    /// 按模型里的坐标把所有表框摆到该在的位置。
    fn place(&self) {
        let s = self.scale.get();
        let graph = self.graph.borrow().clone();
        for t in &graph.tables {
            let Some(b) = self.boxes.borrow().get(&t.name).cloned() else {
                continue;
            };
            self.canvas.move_(
                &b,
                (t.x as f64 * s).round(),
                (t.y as f64 * s).round(),
            );
        }
        // 画布大小跟着图走。
        // ⚠️ **背景那个 DrawingArea 也要给尺寸**：它默认不申请任何空间，
        // 不给尺寸就是 0×0——连线一条都画不出来（表框照常显示，
        // 特别像"图只有表没有关系"）。
        let (w, h) = graph.bounds();
        let (w, h) = ((w as f64 * s).round() as i32, (h as f64 * s).round() as i32);
        self.canvas.set_size_request(w, h);
        self.bg.set_size_request(w, h);
    }

    /// 换缩放档：字号走 CSS，几何自己算。
    fn apply_zoom(self: &Rc<Self>) {
        let idx = self.zoom_idx.get();
        self.scale.set(ZOOMS[idx]);
        // 字号类挂在画布上（Label 继承父级的 font-size）
        for (i, cls) in ZOOM_CLASSES.iter().enumerate() {
            if i == idx {
                self.canvas.add_css_class(cls);
            } else {
                self.canvas.remove_css_class(cls);
            }
        }
        self.rebuild();
    }
}
