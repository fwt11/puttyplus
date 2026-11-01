//! 数据库结果表（8.4 / 8.5，Rust-only）：排序、筛选、复制、导出、数据编辑。
//!
//! **为什么自己维护"可见顺序"而不是用 `FilterListModel` + `SortListModel`**：
//! 那两个模型能把排序筛选交给 GTK，但**显示行 → 原始行**的映射就拿不到了，
//! 而数据编辑必须知道"屏幕上的这一行"对应库里"哪一行"（WHERE 条件要用原始
//! 值拼）。所以这里把数据留在 Rust 侧（`GridData`），排序/筛选自己算，
//! 再把结果灌进 `ListStore`——多一次 O(n) 拷贝，换来映射关系明确。
//!
//! **编辑改动先存在 `edits` 里**，不直接改 `GridData.rows`——原始值必须留着，
//! 它是 UPDATE 的 WHERE 条件（改了就没法定位原行了）。
//!
//! 排序规则：**NULL 当作最大值**——跟 SQL 一样，升序排在最后、降序排到最前
//! （PG 默认也是这个语义，用户不用换脑子）；数字列按数值比，其余按字符串比。
//! 筛选是"任一单元格包含关键字"（不区分大小写）。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use gtk::glib::value::ToValue;
use gtk::prelude::*;

use crate::backend::dbmeta::ColType;

/// 一份结果集（原始数据 + 列元信息）。
#[derive(Debug, Clone, Default)]
pub struct GridData {
    pub columns: Vec<String>,
    /// 每列的类型（编辑时决定怎么转字面量）
    pub types: Vec<ColType>,
    /// 原始值；`None` = SQL NULL
    pub rows: Vec<Vec<Option<String>>>,
    /// 主键列下标（编辑时用来定位行）；空 = 该表没有主键
    pub pk: Vec<usize>,
}

/// 可见行指向哪一份数据。
///
/// 原来只有一种（原始行），加了插入/删除之后就有了两种：
/// 已有行可能被标记删除（不再显示），末尾还可能挂着待插入的新行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRef {
    /// 原始数据行（源行号）
    Src(usize),
    /// 待插入的新行（第几个新行）
    Ins(usize),
}

/// 网格右键菜单里需要**上层**（dbui）去做的事（阶段 47）。
///
/// 网格自己只管数据与显示；弹窗口、弹对话框这些要拿到 dbui 的上下文
/// （宿主窗口、连接、目标表），所以抛给上层。
///
/// ⚠️ **回调只能抓弱引用**：网格归 `ResultTab` 所有，抓强引用就是
/// `ResultTab → 网格 → 回调 → ResultTab` 的引用环。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridMenuAction {
    /// 「批量修改所选…」：对**所有选中行**的某一列改值。
    BatchEdit {
        /// 原始列下标
        col: usize,
    },
    /// 「单元格详情…」
    CellDetail {
        /// 显示行号
        row: usize,
        /// 原始列下标
        col: usize,
    },
    /// 「行详情…」
    RowDetail {
        /// 显示行号
        row: usize,
    },
    /// 「列详情…」
    ColDetail {
        /// 原始列下标
        col: usize,
    },
    /// 「转置查看」
    Transpose,
}

/// 把一份结果集**转置**（行 ↔ 列互换，dbx 的「转置查看」）。
///
/// **纯函数**（不碰界面）——转置只是个"看法"，不该改数据。
///
/// - 新列名 = 原行号（`第 1 行`…），新行 = 原来的每一列；
/// - 类型全退化成**文本**：转置后一列里混着各种类型，没法再按类型处理；
/// - **主键没了**：转置后没有"一行一主键"的概念（`pk` 清空）。
pub fn transpose(data: &GridData) -> GridData {
    let ncols = data.columns.len();
    let nrows = data.rows.len();
    let mut out = GridData {
        columns: Vec::with_capacity(nrows),
        types: Vec::with_capacity(nrows),
        rows: Vec::with_capacity(ncols),
        pk: Vec::new(),
    };
    for i in 0..nrows {
        out.columns.push(format!("第 {} 行", i + 1));
        out.types.push(ColType::Text);
    }
    for c in 0..ncols {
        let mut row: Vec<Option<String>> = Vec::with_capacity(nrows);
        for r in 0..nrows {
            row.push(data.rows.get(r).and_then(|x| x.get(c)).cloned().flatten());
        }
        out.rows.push(row);
    }
    out
}

/// 结果表控件（用 `Rc` 包一层：列头点击、单元格编辑的回调要指回自己）。
pub struct DbGrid {
    pub view: gtk::TreeView,
    model: RefCell<gtk::ListStore>,
    /// 原始数据（编辑时不被改动——原始值是 WHERE 条件）
    data: RefCell<GridData>,
    /// 待保存的改动：源行号 → (列号 → 新值文本)
    edits: RefCell<HashMap<usize, HashMap<usize, String>>>,
    /// 待插入的新行：每行是"每列的输入文本"（`None` = 没填，用数据库默认值）
    inserts: RefCell<Vec<Vec<Option<String>>>>,
    /// 待删除的源行号（标记后立刻从网格里消失，可"放弃修改"恢复）
    deletes: RefCell<HashSet<usize>>,
    /// 显示顺序：显示行号 → 数据来源
    visible: RefCell<Vec<RowRef>>,
    /// 排序：列号 + 是否升序
    sort: Cell<Option<(usize, bool)>>,
    /// 筛选关键字（小写）
    filter: RefCell<String>,
    /// 是否允许编辑单元格
    editable: Cell<bool>,
    /// 列操作菜单（阶段 46）：列头的「⋮」按钮点它、或**右键列头**都能弹。
    ///
    /// ⚠️ GTK4 的 `TreeViewColumn` **有** `set_widget()`，而且它**只换列头**、
    /// **不动列内容**（2026-10-29 实测：探针里自定义列头 + 数据行同时正常，
    /// 截图 `/tmp/tvc_probe*.png`）。所以 dbx 那种「列名 + 类型小字 + ⋮」
    /// 的列头是做得出来的。
    /// （早先记的"`set_widget` 会把整列换掉"是误判，此处更正。）
    col_menu: RefCell<Option<gtk::Popover>>,
    /// 列头要不要显示**类型小字**（dbx 的 `TEXT` 那种小字）。
    ///
    /// 结果网格要（用户得知道这列是数字还是文本）；**表结构页签的字段表不要**
    /// ——那张表本来就有一列叫「类型」，再来一行小字是重复。
    header_types: Cell<bool>,
    /// 每列的**自然宽**（像素，按可见顺序）。
    ///
    /// 数据变一次算一次（`set_data` / `append_rows`）；窗口宽度变了**不**
    /// 重算（量文字不便宜），只把剩余空间重新分配。
    nat_widths: RefCell<Vec<i32>>,
    /// 上次真正设上去的列宽（用来避免反复 `set_fixed_width` 触发重排）。
    applied_widths: RefCell<Vec<i32>>,
    /// 「隐藏此列」的回调（阶段 46）：参数是**原始列下标**。
    ///
    /// 网格自己落不了盘（列显示设置存在 `columns.ini`，按连接+表分区），
    /// 所以把"用户点了隐藏"抛给上层（dbui）去应用 + 持久化。
    ///
    /// ⚠️ 回调只能抓**弱引用**：网格归 `ResultTab` 所有，抓强引用就成环。
    hide_col_hook: RefCell<Option<Rc<dyn Fn(usize)>>>,
    /// 「列显示设置…」的回调（阶段 72 / 审计 P1-7、P2-10）。
    ///
    /// **为什么要有这个**：列设置以前有**两个入口**——列头的「⋮」菜单和
    /// 工具栏上的「列显示设置」按钮。审计说"列设置只保留一个入口"⇒
    /// 工具栏那颗按钮撤了，列设置**只在列头菜单里**。
    ///
    /// ⚠️ 回调只能抓**弱引用**：网格归 `ResultTab` 所有，抓强引用就成环。
    col_setup_hook: RefCell<Option<Rc<dyn Fn()>>>,
    /// 列头的「⋮」刚被按过（用来**吃掉**同一次点击引起的列头排序）。
    ///
    /// ⚠️ GTK4 里点子控件**仍会冒泡**到父按钮 → 列头按钮的 `clicked`
    /// （= 排序）照样会响。靠这个标志把那次排序跳过去。
    tri_pressed: Cell<bool>,
    /// 各列列头里的「⋮」按钮（测试钩子：要知道它在哪，才能**真去点它**）。
    col_tris: RefCell<Vec<gtk::Image>>,
    /// 行右键菜单的弹层（阶段 47）。
    ///
    /// ⚠️ **跟列菜单分开两个 Popover**：共用一个的话 `set_child` 会互相
    /// 覆盖（弹哪个就把另一个的内容顶掉）。
    row_menu: RefCell<Option<gtk::Popover>>,
    /// 行右键菜单里"要上层去做"的动作（详情窗口 / 批量修改 / 转置）。
    row_menu_hook: RefCell<Option<Rc<dyn Fn(GridMenuAction)>>>,
    /// 右键菜单**要不要弹**（阶段 50）。
    ///
    /// **为什么要这个开关**：右键菜单是按**结果网格**的语义做的（设为 NULL /
    /// 克隆为新行 / 转置…）。表结构页签的字段表 / 索引表 / 外键表右键弹
    /// 那一套就是错的——"把字段名设为 NULL"这种操作根本不该出现在那儿。
    row_menu_enabled: Cell<bool>,
    /// **不可编辑**的列（原始列下标）。
    ///
    /// 表结构页签的字段表有一列「操作」（复制 / 删除这一行）——那一列
    /// **不能双击编辑**（双击会弹一个小文本框改字，纯属添乱）。
    non_editable_cols: RefCell<HashSet<usize>>,
    /// 单元格被单击的回调（阶段 50）。
    ///
    /// 参数是 `(显示行号, 显示列位置)`。表结构页签的字段表靠它做
    /// 「点『操作』那一格 → 弹行内操作菜单」。
    ///
    /// ⚠️ **回调里只能抓弱引用**：网格归 `ResultTab` 所有，抓强引用成环。
    cell_click_hook: RefCell<Option<Rc<dyn Fn(usize, usize, f64, f64)>>>,
    /// 状态提示（网格里的动作要告诉用户"做了什么"，阶段 47）。
    ///
    /// 网格本身没有状态栏，所以把提示抛给上层（dbui）显示。
    status_hook: RefCell<Option<Rc<dyn Fn(&str)>>>,
    /// 改动通知（阶段 45：底栏的「N 项待保存」要实时更新）
    ///
    /// ⚠️ **回调里只能抓弱引用**：网格归 `ResultTab` 所有，回调若抓 `Rc`，
    /// 就是 `ResultTab → 网格 → 回调 → ResultTab` 的引用环，页签永远释放不掉。
    change_hook: RefCell<Option<Rc<dyn Fn()>>>,
    /// **数据变了**通知（阶段 83 / 审计 P2-10：空结果态）。
    ///
    /// 网格换了结果集 / 追加了行 / 清空了 ⇒ 上层要把「网格」与
    /// 「空结果提示」来回切。网格自己不知道外头摆的是哪个 Stack，
    /// 所以把这件事抛出去。
    ///
    /// ⚠️ **回调里只能抓弱引用**：网格归 `ResultTab` 所有，抓强引用成环。
    data_hook: RefCell<Option<Rc<dyn Fn()>>>,
    /// 列的**顺序与可见性**（Navicat 的「字段显示设置」）。
    ///
    /// 每项是 `(原始列下标, 是否显示)`，**按用户排的顺序**。
    ///
    /// 空 = 全部可见、保持原序（默认）。
    ///
    /// **为什么存完整顺序而不只存可见的那些**：只存可见列的话，藏起来的
    /// 列没有位置，下次打开设置框时它会被挪到末尾——用户点一次「全显示」，
    /// 列顺序就变了（被藏过的列跑到最后）。存完整顺序就没事了。
    ///
    /// **存的是原始列下标，不是显示位置**：隐藏了几列之后，显示位置跟原始
    /// 下标就错位了，用原始下标才不会记错数据。
    col_layout: RefCell<Vec<(usize, bool)>>,
    /// 能不能点列头排序
    ///
    /// **为什么要有这个开关**：表结构页签的字段表**不能排序**——列在表里的
    /// 先后是有含义的（生成 ALTER 时按原顺序），用户按名字排一下就分不清
    /// "第几列"了。结果表则一定要能排。
    sortable: Cell<bool>,
    /// 各列的渲染器（切换编辑模式时要改 editable 属性）
    renderers: RefCell<Vec<gtk::CellRendererText>>,
    /// 自引用（弱）：列头点击、单元格编辑的回调要指回这张表。
    ///
    /// 用**弱**引用而不是强引用：回调被控件持有，控件又被这张表持有，
    /// 强引用会成环（表永远不释放）。弱引用升级不到就说明表已经没了，
    /// 回调直接不做事。
    me: RefCell<Weak<DbGrid>>,
}

impl DbGrid {
    pub fn new() -> Rc<DbGrid> {
        Rc::new_cyclic(|weak: &Weak<DbGrid>| {
            let model = gtk::ListStore::new(&[glib::Type::STRING]);
            let view = gtk::TreeView::new();
            view.set_model(Some(&model));
            view.set_headers_visible(true);
            view.set_grid_lines(gtk::TreeViewGridLines::Horizontal);
            view.set_enable_search(false);
            view.set_rubber_banding(false);
            // ⚠️ **多选**（阶段 46）：dbx 的网格能框选多行，"批量修改所选"
            // 就靠它。**但默认是单选**——表结构页签的字段表一次只删一行
            // （删字段要一条一条生成 ALTER），搜索/历史网格也没批量操作，
            // 多选反而误导。**结果网格**显式调 `set_multi_select(true)`。
            view.selection().set_mode(gtk::SelectionMode::Single);
            // 列操作菜单的弹层（阶段 46）：**挂到 TreeView 上**（GTK4 的
            // Popover 必须有父 widget 才能定位）。用统一的 `menu::popover`，
            // 免得跟别处的弹层长得不一样。
            let col_menu = crate::menu::popover(&view);

            let grid = DbGrid {
                view: view.clone(),
                model: RefCell::new(model),
                data: RefCell::new(GridData::default()),
                edits: RefCell::new(HashMap::new()),
                inserts: RefCell::new(Vec::new()),
                deletes: RefCell::new(HashSet::new()),
                visible: RefCell::new(Vec::new()),
                sort: Cell::new(None),
                filter: RefCell::new(String::new()),
                editable: Cell::new(false),
                col_menu: RefCell::new(Some(col_menu)),
                header_types: Cell::new(true),
                hide_col_hook: RefCell::new(None),
                nat_widths: RefCell::new(Vec::new()),
                applied_widths: RefCell::new(Vec::new()),
                col_setup_hook: RefCell::new(None),
                tri_pressed: Cell::new(false),
                col_tris: RefCell::new(Vec::new()),
                row_menu: RefCell::new(None),
                row_menu_hook: RefCell::new(None),
                row_menu_enabled: Cell::new(true),
                non_editable_cols: RefCell::new(HashSet::new()),
                cell_click_hook: RefCell::new(None),
                status_hook: RefCell::new(None),
                change_hook: RefCell::new(None),
                data_hook: RefCell::new(None),
                col_layout: RefCell::new(Vec::new()),
                sortable: Cell::new(true),
                renderers: RefCell::new(Vec::new()),
                me: RefCell::new(weak.clone()),
            };
            // ---- 右键：数据行 → 行菜单；列头 → 列菜单（阶段 46/47）----
            //
            // ⚠️ GTK4 **没有** `connect_button_press_event`（那是 GTK3 的老写法），
            // 右键要用 `GestureClick` 控制器（`set_button(3)`）。
            // 回调签名是 `(gesture, n_press, x, y)`——第二个参数就是按压次数。
            {
                let weak = grid.me.borrow().clone();
                let view2 = view.clone();
                let click = gtk::GestureClick::new();
                click.set_button(3);
                click.connect_pressed(move |_g, _n, x, y| {
                    let Some(grid) = weak.upgrade() else {
                        return;
                    };
                    // 找点的是哪一列（按列的 x 范围）
                    let cols: Vec<(i32, i32)> = view2
                        .columns()
                        .iter()
                        .map(|c| (c.x_offset(), c.width()))
                        .collect();
                    let xi = x as i32;
                    let disp_col = cols
                        .iter()
                        .position(|(ox, w)| xi >= *ox && xi < *ox + *w);
                    match view2.path_at_pos(x as i32, y as i32) {
                        // 点在**数据行**上 → 行菜单（阶段 47）
                        Some((Some(path), _, _, _)) => {
                            // ⚠️ **行菜单是可以关掉的**（阶段 50）：字段表 /
                            // 索引表 / 外键表不该弹结果网格那一套菜单。
                            if !grid.row_menu_enabled.get() {
                                return;
                            }
                            let Some(&row) = path.indices().first() else {
                                return;
                            };
                            grid.show_row_menu(&view2, row as usize, disp_col, x, y);
                        }
                        // 点在**列头**上 → 列菜单（阶段 46）
                        _ => {
                            let Some(i) = disp_col else { return };
                            grid.show_col_menu(&view2, i, x, y);
                        }
                    }
                });
                view.add_controller(click);
            }
            // ---- 左键：单元格被点（阶段 50）----
            //
            // 表结构页签的字段表有一列「操作」：点它弹出行内操作菜单
            // （复制这一行 / 删除这一行）。
            {
                let weak = grid.me.borrow().clone();
                let view2 = view.clone();
                let click = gtk::GestureClick::new();
                click.set_button(1);
                click.connect_pressed(move |_g, _n, x, y| {
                    let Some(grid) = weak.upgrade() else { return };
                    let hook = grid.cell_click_hook.borrow().clone();
                    let Some(hook) = hook else { return };
                    let Some((Some(path), _, _, _)) = view2.path_at_pos(x as i32, y as i32)
                    else {
                        return;
                    };
                    let Some(&row) = path.indices().first() else { return };
                    // 点的是哪一列（按列的 x 范围）
                    let xi = x as i32;
                    let disp_col = view2
                        .columns()
                        .iter()
                        .position(|c| xi >= c.x_offset() && xi < c.x_offset() + c.width());
                    let Some(dc) = disp_col else { return };
                    // ⚠️ **给原始列下标**：隐藏过列之后显示位置跟原始下标
                    // 就错位了（跟 `hide_col_hook` 一个口径）。
                    let Some(orig) = grid.orig_col(dc) else { return };
                    hook(row as usize, orig, x, y);
                });
                view.add_controller(click);
            }
            // ---- 窗口宽度变了 → 重新分配列宽（阶段 72c）----
            //
            // ⚠️ **用 `notify::width` 而不是重排事件**：列宽是按"可用宽度"
            // 分配的 ⇒ 容器一变宽就得重分。
            {
                let weak = grid.me.borrow().clone();
                let v = view.clone();
                v.connect_notify_local(Some("width"), move |_, _| {
                    let Some(grid) = weak.upgrade() else { return };
                    grid.apply_column_widths();
                });
            }
            grid
        })
    }

    /// 重算每列的**自然宽**（量列名 + 最宽的单元格）。
    ///
    /// **为什么要量**：审计 P2-10 说"默认列宽不利用剩余空间：邮箱/时间列
    /// 截断而右侧还有空白"。要"按内容/可用宽度分配"，先得知道每列**要**
    /// 多宽，剩下的才谈得上分配。
    ///
    /// ⚠️ **只看前 40 行**：一次结果集 200 行 × 十几列，全量量文字会卡住
    /// 界面（pango 量字不便宜）。40 行足够看出"这列大概多宽"。
    fn recompute_nat_widths(&self) {
        let data = self.data.borrow();
        let order = resolve_visible(&self.col_layout.borrow(), data.columns.len());
        if order.is_empty() {
            *self.nat_widths.borrow_mut() = Vec::new();
            return;
        }
        let ctx = self.view.pango_context();
        let layout = gtk::pango::Layout::new(&ctx);
        let mut out = Vec::with_capacity(order.len());
        for &i in &order {
            let mut w = measure_px(&layout, &data.columns[i]);
            for row in data.rows.iter().take(40) {
                if let Some(Some(v)) = row.get(i) {
                    w = w.max(measure_px(&layout, v));
                }
            }
            // 单元格左右内边距 + 列头那颗「⋮」按钮 + 列间距
            out.push(w + 48);
        }
        *self.nat_widths.borrow_mut() = out;
    }

    /// 把**剩余的可用宽度**按内容比例分给各列（阶段 72c / 审计 P2-10）。
    ///
    /// **为什么不是平分**：一个「id」列和一个「email」列平分剩下的宽度，
    /// 结果是 id 宽得离谱、email 还是不够（审计里说的正是这个症状）。
    /// 按**自然宽的比例**分才各得其所。
    ///
    /// ⚠️ **不够宽就不分**：总自然宽已经超过可用宽度时（列很多），保持
    /// 自然宽让横向滚动条出场——硬分会把每列压到看不清。
    fn apply_column_widths(&self) {
        let avail = self.view.width();
        if avail <= 1 {
            return;
        }
        let cols = self.view.columns();
        let ncols = cols.len();
        if ncols == 0 {
            return;
        }
        // ⚠️ **自然宽跟列数对不上就重算**：藏过一列 / 换过列布局之后，
        // 缓存里的自然宽是按**旧**列数算的，直接用会把宽度错配到别的列上。
        //
        // ⚠️ **借用要在这里结束**（`let` 一句里就掉）：下面 `borrow_mut`
        // 的时候同一个 RefCell 再借就是 panic。
        let stale = self.nat_widths.borrow().len() != ncols;
        if stale {
            self.recompute_nat_widths();
        }
        let nat = self.nat_widths.borrow().clone();
        if nat.is_empty() || nat.len() != ncols {
            return;
        }
        let total: i32 = nat.iter().sum();
        let widths: Vec<i32> = if total >= avail {
            nat.clone()
        } else {
            let extra = avail - total;
            nat.iter()
                .map(|n| n + (extra * n) / total.max(1))
                .collect()
        };
        // ⚠️ **宽度没变就别再设**：反复 `set_fixed_width` 会触发重排，
        // 重排又可能改宽度 ⇒ 来回抖。
        if *self.applied_widths.borrow() == widths {
            return;
        }
        *self.applied_widths.borrow_mut() = widths.clone();
        for (col, w) in cols.iter().zip(widths.iter()) {
            col.set_sizing(gtk::TreeViewColumnSizing::Fixed);
            col.set_fixed_width(*w);
        }
    }

    /// 弹出某一列的操作菜单（阶段 46）。
    ///
    /// `x` / `y` 是**相对 TreeView** 的坐标（右键点列头时是鼠标位置，
    /// 点「⋮」时是那个按钮的位置）。
    fn show_col_menu(&self, view: &gtk::TreeView, col: usize, x: f64, y: f64) {
        let Some(vb) = self.build_col_menu(view, col) else { return };
        let mut slot = self.col_menu.borrow_mut();
        let Some(pop) = slot.as_ref() else { return };
        pop.set_child(Some(&vb));
        // 弹在鼠标位置（相对 TreeView）
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        pop.set_pointing_to(Some(&rect));
        pop.popup();
    }

    /// 算出来的列宽（像素，按显示顺序）——测试钩子。
    ///
    /// **这是"打算给多宽"**，跟 `column_widths()`（GTK 实际分配的）分开：
    /// 两者不一致说明 GTK 没按我们说的做，那才是真 bug。
    pub fn planned_widths(&self) -> Vec<i32> {
        self.applied_widths.borrow().clone()
    }

    /// GTK 实际分配给各列的宽度（像素，按显示顺序）——测试钩子。
    pub fn column_widths(&self) -> Vec<i32> {
        self.view.columns().iter().map(|c| c.width()).collect()
    }

    /// 列头菜单里有哪些项（测试钩子，**不弹菜单**）。
    ///
    /// 内容跟真弹出来的是同一份（`build_col_menu`）。
    pub fn col_menu_titles_for(&self, col: usize) -> Vec<String> {
        let Some(vb) = self.build_col_menu(&self.view, col) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut cur = vb.first_child();
        while let Some(c) = cur {
            if let Some(b) = c.downcast_ref::<gtk::Button>() {
                if let Some(t) = crate::menu::item_label(b) {
                    out.push(t);
                }
            }
            cur = c.next_sibling();
        }
        out
    }

    /// 造列头菜单的**内容**（不弹）。
    ///
    /// **为什么拆出来**：菜单里有哪些项要能被测试看见（阶段 72 加了
    /// 「列显示设置…」），而弹层只有真指针事件才会 map ⇒ 拆成"造内容"和
    /// "弹出来"两半，测试看的是**同一份内容**（不是另写一遍）。
    fn build_col_menu(&self, view: &gtk::TreeView, col: usize) -> Option<gtk::Box> {
        let (name, ty) = {
            let data = self.data.borrow();
            let name = match data.columns.get(col) {
                Some(n) => n.clone(),
                None => return None,
            };
            (name, data.types.get(col).copied())
        };
        // 重建菜单内容（列名每次可能不同）
        let vb = crate::menu::body();
        {
            let weak = self.me.borrow().clone();
            let b = crate::menu::item("view-sort-ascending-symbolic", "升序排列", None);
            b.connect_clicked(move |_| {
                if let Some(grid) = weak.upgrade() {
                    grid.sort_by_column(col, true);
                }
            });
            vb.append(&b);
        }
        {
            let weak = self.me.borrow().clone();
            let b = crate::menu::item("view-sort-descending-symbolic", "降序排列", None);
            b.connect_clicked(move |_| {
                if let Some(grid) = weak.upgrade() {
                    grid.sort_by_column(col, false);
                }
            });
            vb.append(&b);
        }
        vb.append(&crate::menu::sep());
        {
            let b = crate::menu::item("edit-copy-symbolic", "复制列名", None);
            let nm = name.clone();
            let v2 = view.clone();
            b.connect_clicked(move |_| {
                v2.clipboard().set_text(&nm);
            });
            vb.append(&b);
        }
        {
            let b = crate::menu::item("edit-paste-symbolic", "复制这一列的数据", None);
            let weak = self.me.borrow().clone();
            let v2 = view.clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                // 取这一列当前显示的值（排序/筛选后的顺序），逐行一行一格
                let data = grid.data.borrow();
                let mut lines: Vec<String> = Vec::new();
                for r in grid.visible.borrow().iter() {
                    if let RowRef::Src(src) = r {
                        if let Some(Some(v)) = data.rows.get(*src).and_then(|row| row.get(col)) {
                            lines.push(v.clone());
                        }
                    }
                }
                drop(data);
                v2.clipboard().set_text(&lines.join("\n"));
            });
            vb.append(&b);
        }
        // 隐藏此列：网格自己落不了盘，交给上层（dbui）应用 + 持久化
        if let Some(hook) = self.hide_col_hook.borrow().clone() {
            let b = crate::menu::item("view-conceal-symbolic", "隐藏这一列", None);
            b.connect_clicked(move |_| {
                hook(col);
            });
            vb.append(&b);
        }
        // 「列显示设置…」（阶段 72 / 审计 P1-7、P2-10）：列设置的**唯一入口**。
        //
        // **为什么放在列头菜单里**：以前列设置有两个入口——这颗「⋮」菜单和
        // 工具栏上的「列显示设置」按钮。审计说"列设置只保留一个入口"⇒
        // 工具栏那颗撤了，列设置只在这儿。
        if let Some(hook) = self.col_setup_hook.borrow().clone() {
            let b = crate::menu::item("view-list-symbolic", "列显示设置…", None);
            b.connect_clicked(move |_| {
                hook();
            });
            vb.append(&b);
        }
        // 类型：菜单底部再写一遍（列头已经有小字，这里是兜底——列头被压窄时
        // 小字会被省略掉，菜单里还能看到）
        if let Some(t) = ty {
            let l = gtk::Label::new(Some(&format!("类型：{}", t.label())));
            l.add_css_class("pp-crumb-dim");
            l.set_xalign(0.0);
            l.set_margin_top(4);
            l.set_margin_bottom(4);
            l.set_margin_start(10);
            l.set_margin_end(10);
            vb.append(&l);
        }
        Some(vb)
    }

    /// 列头要不要显示类型小字（阶段 46）。
    pub fn set_header_types(&self, on: bool) {
        if self.header_types.get() == on {
            return;
        }
        self.header_types.set(on);
        self.rebuild_columns();
    }

    /// 「隐藏此列」的回调（阶段 46）：`(原始列下标)`。
    ///
    /// 网格只负责"用户点了隐藏"，**落盘由上层做**（列显示设置存在
    /// `columns.ini`，按连接 + 表分区，网格不知道这些）。
    pub fn set_hide_col_hook(&self, hook: Rc<dyn Fn(usize)>) {
        *self.hide_col_hook.borrow_mut() = Some(hook);
    }

    /// 「列显示设置…」的回调（阶段 72）：列头菜单里的那一项点它。
    ///
    /// 设了才有那一项——表结构页签的字段表不需要列显示设置。
    pub fn set_col_setup_hook(&self, hook: Rc<dyn Fn()>) {
        *self.col_setup_hook.borrow_mut() = Some(hook);
    }

    /// 允许多选（阶段 46：dbx 的网格能框选多行，"批量修改所选"靠它）。
    ///
    /// **默认单选**：表结构页签的字段表一次只删一行，多选反而误导。
    pub fn set_multi_select(&self, on: bool) {
        self.view
            .selection()
            .set_mode(if on {
                gtk::SelectionMode::Multiple
            } else {
                gtk::SelectionMode::Single
            });
    }

    /// 显示列位置 → **原始列下标**。
    ///
    /// ⚠️ 隐藏过列以后，显示位置跟原始下标就错位了——菜单里的动作一律
    /// 用**原始下标**（改数据、保存、导出都以原始列为准）。
    fn orig_col(&self, disp: usize) -> Option<usize> {
        let n = self.data.borrow().columns.len();
        resolve_visible(&self.col_layout.borrow(), n).get(disp).copied()
    }

    /// 行右键菜单（阶段 47：对标 dbx 的网格右键菜单）。
    ///
    /// `row` 是**显示行号**；`disp_col` 是**显示列位置**（`None` = 点在
    /// 行号那一侧，没有具体列）。
    fn show_row_menu(        &self,
        view: &gtk::TreeView,
        row: usize,
        disp_col: Option<usize>,
        x: f64,
        y: f64,
    ) {
        // ⚠️ **右键未选中的行 → 只选它**（Navicat / dbx 的行为）：
        // 右键已选中的行 → **保留多选**（不然"框选 5 行 → 右键"就白框了）。
        let selected = self.selected_display_rows();
        if !selected.contains(&row) {
            let sel = self.view.selection();
            sel.unselect_all();
            sel.select_path(&gtk::TreePath::from_indices(&[row as i32]));
        }
        let col = disp_col.and_then(|d| self.orig_col(d));
        let editable = self.is_editable();
        let nsel = self.selected_display_rows().len();

        let body = crate::menu::body();
        // ---- 编辑类（要能改数据）----
        {
            let b = crate::menu::item("edit-clear-symbolic", "设为 NULL", None);
            b.set_sensitive(editable);
            if !editable {
                b.set_tooltip_text(Some("先点工具栏的「编辑」进入编辑模式"));
            }
            let weak = self.me.borrow().clone();
            let c = col;
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let Some(c) = c else { return };
                let rows = grid.selected_display_rows();
                // ⚠️ **NULL 走文本 "NULL"**：`edit_literal` 本来就认这个
                // 文本（生成 SQL 的 NULL，不是字符串 'NULL'）。
                let n = grid.set_edit_rows(&rows, c, "NULL");
                let msg = format!("已把 {n} 行的该列设为 NULL，点「保存修改」才生效");
                grid.set_status(&msg);
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("edit-find-replace-symbolic", "批量修改所选…", None);
            b.set_sensitive(editable && nsel > 0);
            if !editable {
                b.set_tooltip_text(Some("先点工具栏的「编辑」进入编辑模式"));
            } else if nsel == 0 {
                b.set_tooltip_text(Some("先选中要改的行"));
            }
            let weak = self.me.borrow().clone();
            let c = col;
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let Some(c) = c else { return };
                let hook = grid.row_menu_hook.borrow().clone();
                if let Some(h) = hook {
                    h(GridMenuAction::BatchEdit { col: c });
                }
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("list-add-symbolic", "克隆为新行", None);
            b.set_sensitive(editable);
            if !editable {
                b.set_tooltip_text(Some("先点工具栏的「编辑」进入编辑模式"));
            }
            let weak = self.me.borrow().clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let rows = grid.selected_display_rows();
                let Some(r) = rows.first().copied() else { return };
                if grid.clone_row(r) {
                    grid.set_status("已克隆为新行（改完点「保存修改」插入）");
                }
            });
            body.append(&b);
        }
        body.append(&crate::menu::sep());
        // ---- 查看类 ----
        {
            let b = crate::menu::item("dialog-information-symbolic", "单元格详情…", None);
            let weak = self.me.borrow().clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let Some(c) = col else { return };
                let hook = grid.row_menu_hook.borrow().clone();
                if let Some(h) = hook {
                    h(GridMenuAction::CellDetail { row, col: c });
                }
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("format-justify-fill-symbolic", "行详情…", None);
            let weak = self.me.borrow().clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let hook = grid.row_menu_hook.borrow().clone();
                if let Some(h) = hook {
                    h(GridMenuAction::RowDetail { row });
                }
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("view-list-symbolic", "列详情…", None);
            let weak = self.me.borrow().clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let Some(c) = col else { return };
                let hook = grid.row_menu_hook.borrow().clone();
                if let Some(h) = hook {
                    h(GridMenuAction::ColDetail { col: c });
                }
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("view-refresh-symbolic", "转置查看…", None);
            let weak = self.me.borrow().clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let hook = grid.row_menu_hook.borrow().clone();
                if let Some(h) = hook {
                    h(GridMenuAction::Transpose);
                }
            });
            body.append(&b);
        }
        body.append(&crate::menu::sep());
        // ---- 复制类 ----
        {
            let b = crate::menu::item("edit-copy-symbolic", "复制单元格", None);
            let weak = self.me.borrow().clone();
            let v2 = view.clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let Some(c) = col else { return };
                let text = grid.cell_text(row, c);
                v2.clipboard().set_text(&text);
            });
            body.append(&b);
        }
        {
            let b = crate::menu::item("edit-copy-symbolic", "复制整行", None);
            let weak = self.me.borrow().clone();
            let v2 = view.clone();
            b.connect_clicked(move |_| {
                let Some(grid) = weak.upgrade() else { return };
                let text = grid.row_text(row);
                v2.clipboard().set_text(&text);
            });
            body.append(&b);
        }

        // ---- 弹出来 ----
        let mut slot = self.row_menu.borrow_mut();
        if slot.is_none() {
            let pop = crate::menu::popover(&self.view);
            *slot = Some(pop);
        }
        let Some(pop) = slot.as_ref() else { return };
        pop.set_child(Some(&body));
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        pop.set_pointing_to(Some(&rect));
        pop.popup();
    }

    /// 行右键菜单现在开着吗（测试钩子）。
    pub fn row_menu_is_open(&self) -> bool {
        self.row_menu
            .borrow()
            .as_ref()
            .map(|p| p.is_visible())
            .unwrap_or(false)
    }

    /// 行右键菜单里的条目文字（测试钩子）。
    pub fn row_menu_titles(&self) -> Vec<String> {
        let slot = self.row_menu.borrow();
        let Some(pop) = slot.as_ref() else { return Vec::new() };
        let Some(child) = pop.child() else { return Vec::new() };
        let mut out = Vec::new();
        let mut cur = child.first_child();
        while let Some(c) = cur {
            if let Some(b) = c.downcast_ref::<gtk::Button>() {
                if let Some(t) = crate::menu::item_label(b) {
                    out.push(t);
                }
            }
            cur = c.next_sibling();
        }
        out
    }

    /// 程序化弹出某一行 / 某一列的右键菜单（测试钩子）。
    pub fn row_menu_popup_now(&self, row: usize, disp_col: Option<usize>) -> bool {
        let (x, y) = (10.0, 10.0);
        self.show_row_menu(&self.view, row, disp_col, x, y);
        self.row_menu_is_open()
    }

    /// 点行右键菜单里的某一项（按文字，测试钩子）。
    ///
    /// 返回 false = 没这一项 / 点不动（灰着的）。
    pub fn row_menu_click_item(&self, title: &str) -> bool {
        let slot = self.row_menu.borrow();
        let Some(pop) = slot.as_ref() else { return false };
        let Some(child) = pop.child() else { return false };
        let mut cur = child.first_child();
        while let Some(c) = cur {
            if let Some(b) = c.downcast_ref::<gtk::Button>() {
                if crate::menu::item_label(b).as_deref() == Some(title) {
                    if !b.is_sensitive() {
                        return false;
                    }
                    b.emit_clicked();
                    return true;
                }
            }
            cur = c.next_sibling();
        }
        false
    }

    /// 行右键菜单里"要上层去做"的动作的回调（阶段 47）。
    pub fn set_row_menu_hook(&self, hook: Rc<dyn Fn(GridMenuAction)>) {
        *self.row_menu_hook.borrow_mut() = Some(hook);
    }

    /// 右键菜单现在**开着**吗（测试钩子）。
    pub fn row_menu_is_enabled(&self) -> bool {
        self.row_menu_enabled.get()
    }

    /// 右键菜单**要不要弹**（阶段 50）。
    ///
    /// 表结构页签的字段表 / 索引表 / 外键表要关掉：那一套菜单是按**结果
    /// 网格**的语义做的（设为 NULL / 克隆为新行 / 转置…），出现在字段表上
    /// 根本不对（"把字段名设为 NULL"听着就危险）。
    pub fn set_row_menu_enabled(&self, on: bool) {
        self.row_menu_enabled.set(on);
    }

    /// 某一列**不可编辑**（阶段 50：字段表的「操作」列）。
    ///
    /// `col` 是**原始列下标**。不可编辑的列双击不会弹出编辑框。
    pub fn set_column_editable(&self, col: usize, on: bool) {
        {
            // ⚠️ **借用要在这里结束掉**：`rebuild_columns` 里面还要读这个
            // RefCell（借用还挂着就是双重借用 → panic）。
            let mut s = self.non_editable_cols.borrow_mut();
            if on {
                s.remove(&col);
            } else {
                s.insert(col);
            }
        }
        // 立刻生效：重建设列（渲染器的 editable 属性要跟着改）
        self.rebuild_columns();
    }

    /// 单元格被单击的回调（阶段 50）。
    ///
    /// 参数是 `(显示行号, 原始列下标, x, y)`——列给的是**原始下标**
    /// （跟 `hide_col_hook` 一个口径：隐藏过列之后显示位置会错位）。
    /// `x` / `y` 是**相对 TreeView** 的坐标（弹菜单要用）。
    ///
    /// 用来做「点『操作』那一格 → 弹行内操作菜单」。
    pub fn set_cell_click_hook(&self, hook: Rc<dyn Fn(usize, usize, f64, f64)>) {
        *self.cell_click_hook.borrow_mut() = Some(hook);
    }

    /// 状态提示的回调（阶段 47）：网格里的动作告诉用户"做了什么"。
    pub fn set_status_hook(&self, hook: Rc<dyn Fn(&str)>) {
        *self.status_hook.borrow_mut() = Some(hook);
    }

    /// 报一句状态（给上层显示）。
    pub fn set_status(&self, msg: &str) {
        if let Some(h) = self.status_hook.borrow().clone() {
            h(msg);
        }
    }

    /// 给**若干显示行**的某一列设值（批量修改 / 设为 NULL）。
    ///
    /// 返回改了几行。
    ///
    /// ⚠️ **批量时只重建一次行**：每行都重建的话会闪（而且行号会挪）。
    pub fn set_edit_rows(&self, display_rows: &[usize], col: usize, text: &str) -> usize {
        let mut n = 0usize;
        {
            let mut edits = self.edits.borrow_mut();
            let mut inserts = self.inserts.borrow_mut();
            for &r in display_rows {
                let Some(rf) = self.visible.borrow().get(r).copied() else {
                    continue;
                };
                match rf {
                    RowRef::Src(src) => {
                        edits.entry(src).or_default().insert(col, text.to_string());
                        n += 1;
                    }
                    RowRef::Ins(i) => {
                        if let Some(row) = inserts.get_mut(i) {
                            if col < row.len() {
                                row[col] = Some(text.to_string());
                                n += 1;
                            }
                        }
                    }
                }
            }
        }
        if n > 0 {
            self.rebuild_rows();
            self.fire_change();
        }
        n
    }

    /// 克隆一行：把它的**当前值**（含已改的）复制成一行待插入的新行。
    ///
    /// 返回 false = 这一行取不到。
    pub fn clone_row(&self, display_row: usize) -> bool {
        let ncols = self.data.borrow().columns.len();
        let vals: Vec<Option<String>> = {
            let data = self.data.borrow();
            let edits = self.edits.borrow();
            let inserts = self.inserts.borrow();
            let Some(rf) = self.visible.borrow().get(display_row).copied() else {
                return false;
            };
            match rf {
                RowRef::Src(src) => (0..ncols)
                    .map(|c| {
                        if let Some(v) = edits.get(&src).and_then(|m| m.get(&c)) {
                            return Some(v.clone());
                        }
                        match data.rows.get(src).and_then(|r| r.get(c)) {
                            None => Some("NULL".to_string()),
                            Some(v) => v.clone(),
                        }
                    })
                    .collect(),
                RowRef::Ins(i) => inserts.get(i).cloned().unwrap_or_else(|| vec![None; ncols]),
            }
        };
        {
            let mut inserts = self.inserts.borrow_mut();
            inserts.push(vals);
        }
        self.rebuild_rows();
        self.fire_change();
        true
    }

    /// 某个显示行、某一列的**当前显示值**（含已改的；NULL 显示 "NULL"）。
    pub fn cell_text(&self, display_row: usize, col: usize) -> String {
        let data = self.data.borrow();
        let edits = self.edits.borrow();
        let inserts = self.inserts.borrow();
        let Some(rf) = self.visible.borrow().get(display_row).copied() else {
            return String::new();
        };
        match rf {
            RowRef::Src(src) => {
                if let Some(v) = edits.get(&src).and_then(|m| m.get(&col)) {
                    return v.clone();
                }
                // ⚠️ `row.get(c)` 是 `Option<&Option<String>>`：
                // 外层 None = 这一列不存在，内层 None = 值是 NULL。
                match data.rows.get(src).and_then(|r| r.get(col)).and_then(|v| v.as_ref()) {
                    None => "NULL".to_string(),
                    Some(v) => v.clone(),
                }
            }
            RowRef::Ins(i) => inserts
                .get(i)
                .and_then(|r| r.get(col))
                .cloned()
                .flatten()
                .unwrap_or_default(),
        }
    }

    /// 网格里**真正显示**的文本（读渲染模型，不是原始数据）。
    ///
    /// ⚠️ 跟 [`cell_text`] 的区别：`cell_text` 给的是**原始值**（导出 / 详情
    /// 窗口用），这里给的是**屏幕上显示的那一串字**（布尔显示成「是/否」）。
    pub fn model_text(&self, display_row: usize, col: usize) -> String {
        let store = self.model.borrow();
        let path = gtk::TreePath::from_indices(&[display_row as i32]);
        let Some(iter) = store.iter(&path) else {
            return String::new();
        };
        store
            .get_value(&iter, col as i32)
            .get::<String>()
            .unwrap_or_default()
    }

    /// 当前**显示**的数据（含已改的），转置 / 详情窗口用。
    ///
    /// ⚠️ 跟 [`GridData`] 的原始数据不同：这里 NULL 显示成文本 `"NULL"`
    /// （跟网格里看到的完全一样），改过的格子是新值。
    pub fn display_data(&self) -> GridData {
        let data = self.data.borrow();
        let edits = self.edits.borrow();
        let inserts = self.inserts.borrow();
        let ncols = data.columns.len();
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        for rf in self.visible.borrow().iter() {
            match rf {
                RowRef::Src(src) => {
                    let row: Vec<Option<String>> = (0..ncols)
                        .map(|c| {
                            if let Some(v) = edits.get(src).and_then(|m| m.get(&c)) {
                                return Some(v.clone());
                            }
                            match data.rows.get(*src).and_then(|r| r.get(c)).and_then(|v| v.as_ref())
                            {
                                None => Some("NULL".to_string()),
                                Some(s) => Some(s.clone()),
                            }
                        })
                        .collect();
                    rows.push(row);
                }
                RowRef::Ins(i) => {
                    let row: Vec<Option<String>> = (0..ncols)
                        .map(|c| {
                            inserts
                                .get(*i)
                                .and_then(|r| r.get(c))
                                .cloned()
                                .flatten()
                        })
                        .collect();
                    rows.push(row);
                }
            }
        }
        GridData {
            columns: data.columns.clone(),
            types: data.types.clone(),
            rows,
            pk: data.pk.clone(),
        }
    }

    /// 某一行的**当前显示值**（制表符分隔，便于粘到表格里）。
    pub fn row_text(&self, display_row: usize) -> String {
        let n = self.data.borrow().columns.len();
        (0..n)
            .map(|c| self.cell_text(display_row, c))
            .collect::<Vec<_>>()
            .join("\t")
    }

    /// 换一份结果集（列变了要重建列头）。
    pub fn set_data(&self, data: GridData) {
        // **模型列数必须等于数据列数**：ListStore 的列数是建的时候定死的，
        // 往不存在的列上写值 GTK 会进入不可预期的状态（实测直接把进程
        // 撑到几十 GB 内存）。所以每次换结果集都重建模型。
        let ncols = data.columns.len().max(1);
        let types: Vec<glib::Type> = (0..ncols).map(|_| glib::Type::STRING).collect();
        let model = gtk::ListStore::new(&types);
        self.view.set_model(Some(&model));
        *self.model.borrow_mut() = model;
        *self.data.borrow_mut() = data;
        self.edits.borrow_mut().clear();
        self.inserts.borrow_mut().clear();
        self.deletes.borrow_mut().clear();
        self.sort.set(None);
        self.filter.borrow_mut().clear();
        self.rebuild_columns();
        self.rebuild_rows();
        // 列宽按内容分（阶段 72c）：数据换了 ⇒ 自然宽换了 ⇒ 重算 + 重分
        self.recompute_nat_widths();
        self.apply_column_widths();
        // 数据换了 ⇒ 空结果态要跟着切（阶段 83）
        self.fire_data();
    }

    /// 只换数据、不动列（翻页追加时用）。
    pub fn append_rows(&self, rows: Vec<Vec<Option<String>>>) {
        self.data.borrow_mut().rows.extend(rows);
        self.rebuild_rows();
        // 翻页追加的行可能更长 ⇒ 自然宽要重算（阶段 72c）
        self.recompute_nat_widths();
        self.apply_column_widths();
        self.fire_data();
    }

    fn rebuild_columns(&self) {
        for c in self.view.columns() {
            self.view.remove_column(&c);
        }
        self.renderers.borrow_mut().clear();
        // 列重建了 → 旧的「⋮」按钮作废
        self.col_tris.borrow_mut().clear();
        // ⚠️ **先把数据拷出来再借**：这个函数结尾要调 `apply_column_widths`
        // （可能要重算自然宽），而重算要再借 `data` ⇒ 借用一路留着就是
        // **双重借用 panic**。
        let (names, types): (Vec<String>, Vec<ColType>) = {
            let data = self.data.borrow();
            (data.columns.clone(), data.types.clone())
        };
        let weak = self.me.borrow().clone();
        // 可见列的顺序（**原始列下标**）；没设过 = 全部可见、原序。
        // 越界的（换了更小的结果集）直接丢掉。
        let order = resolve_visible(&self.col_layout.borrow(), names.len());
        for i in order {
            let name = &names[i];
            let renderer = gtk::CellRendererText::new();
            renderer.set_ellipsize(gtk::pango::EllipsizeMode::End);
            // ⚠️ **editable 要在这里重新设一遍**：`set_data` 会重建列（列头变了
            // 就得重建），新渲染器默认不可编辑——不重设的话，换一次结果集
            // 编辑功能就悄悄没了（看着像"改不了"）。
            renderer.set_editable(self.editable.get());
            // ⚠️ **某些列天生不可编辑**（阶段 50：字段表的「操作」列）——
            // 双击它弹个文本框纯属添乱。
            if self.non_editable_cols.borrow().contains(&i) {
                renderer.set_editable(false);
            }
            // 单元格改完 → 记一处改动（编辑模式才允许改）
            {
                let weak = weak.clone();
                // ⚠️ **必须用原始列下标 `i`，不能用 path 里的列位置**：隐藏了
                // 几列以后，TreeView 里的列位置跟原始列下标就对不上了，用位置
                // 会把改动记到别的列上（改 A 列的值写进 B 列）。
                renderer.connect_edited(move |_, path, text| {
                    let Some(grid) = weak.upgrade() else { return };
                    if !grid.is_editable() {
                        return;
                    }
                    let idx = path.indices();
                    let Some(&row) = idx.first() else {
                        return;
                    };
                    grid.set_edit(row as usize, i, text);
                });
            }
            let col = gtk::TreeViewColumn::new();
            col.set_resizable(true);
            col.set_expand(true);
            col.set_min_width(60);
            col.pack_start(&renderer, true);
            // 必须显式绑定 "text"：GTK4 的 pack_start 不会把某一列的文本喂给
            // 渲染器，不绑就是一片空白（列头还在，看着像"查出来是空的"）。
            col.add_attribute(&renderer, "text", i as i32);
            // 不可排序的表（表结构页签的字段表）列头不可点，也不显示排序箭头
            col.set_clickable(self.sortable.get());
            if self.sortable.get() {
                let weak = weak.clone();
                col.connect_clicked(move |_| {
                    let Some(grid) = weak.upgrade() else { return };
                    // ⚠️ 刚按过列头的「⋮」→ 这次点击是"开列菜单"，**不是排序**
                    // （GTK4 里点子控件仍会冒泡到列头按钮，排序照样会触发）。
                    if grid.tri_pressed.replace(false) {
                        return;
                    }
                    grid.toggle_sort(i);
                });
            }
            // ---- 列头（阶段 46）：dbx 风格「列名 + 类型小字 + ⋮」----
            //
            // ⚠️ GTK4 的 `TreeViewColumn` **有** `set_widget()`，它**只换列头**、
            // **不动列内容**（2026-10-29 实测，探针截图 `/tmp/tvc_probe*.png`）。
            // 所以这里能做出 dbx 那种"字段名 + 下面一行小字类型"的列头。
            //
            // ⚠️ **`set_title()` 也要留着**：显示虽然换成了 widget，但 `title`
            // 属性还得有值——导出、过滤条件编辑器、列类型映射都靠
            // `col.title()` 拿列名（`set_widget` 不会把 title 清掉，
            // 但少写一句 `set_title` 的话那里就全是空串了）。
            col.set_title(name);
            let header = self.build_header(i, name, types.get(i).copied());
            col.set_widget(Some(&header));
            self.view.append_column(&col);
            self.renderers.borrow_mut().push(renderer);
        }
        // 列建好了 → 把排序箭头对上（换过结果集 / 改过列布局都要重来）
        self.sync_sort_indicators();
        // 列重建了 ⇒ 之前设的列宽没了，重新分一次（阶段 72c）
        *self.applied_widths.borrow_mut() = Vec::new();
        self.apply_column_widths();
    }

    /// 造一列的列头（阶段 46）：「列名 + 类型小字 + ⋮ 按钮」。
    ///
    /// `col_idx` 是**原始列下标**（菜单里的操作都按它来）。
    fn build_header(&self, col_idx: usize, name: &str, ty: Option<ColType>) -> gtk::Box {
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let l1 = gtk::Label::new(Some(name));
        l1.set_xalign(0.0);
        l1.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // ⚠️ **限宽**：列头 widget 里的 Label 不限宽的话，一个长列名就能把
        // 整列撑得极宽（列头宽度会反过来决定列宽）。
        l1.set_max_width_chars(18);
        vb.append(&l1);
        // 类型小字（dbx 的 `TEXT`）
        if self.header_types.get() {
            if let Some(t) = ty {
                let l2 = gtk::Label::new(Some(t.label()));
                l2.add_css_class("dim-label");
                l2.set_xalign(0.0);
                l2.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l2.set_max_width_chars(18);
                vb.append(&l2);
            }
        }
        hb.append(&vb);
        // 「⋮」：这一列的操作菜单（dbx 的列头右侧小三角）
        let tri = gtk::Image::from_icon_name("view-more-symbolic");
        tri.set_pixel_size(12);
        tri.set_tooltip_text(Some("这一列的操作"));
        let tri2 = tri.clone();
        {
            let weak = self.me.borrow().clone();
            let g = gtk::GestureClick::new();
            g.set_button(1);
            g.connect_pressed(move |_g, _n, _x, _y| {
                let Some(grid) = weak.upgrade() else { return };
                // 记住"这次是点三角"——列头按钮的 clicked 会跟着响，得把排序吃掉
                grid.tri_pressed.set(true);
                // 弹在三角下面（坐标换算成相对 TreeView 的）
                let (x, y) = match tri2.translate_coordinates(&grid.view, 0.0, 0.0) {
                    Some(p) => p,
                    None => (0.0, 0.0),
                };
                grid.show_col_menu(&grid.view, col_idx, x, y + 14.0);
            });
            tri.add_controller(g);
        }
        // 记下这个三角（测试钩子要用：要知道它在哪，才能真去点它）
        self.col_tris.borrow_mut().push(tri.clone());
        hb.append(&tri);
        hb
    }

    /// 各列列头里的「⋮」按钮（测试钩子）。
    pub fn column_tris(&self) -> Vec<gtk::Image> {
        self.col_tris.borrow().clone()
    }

    /// 列操作菜单现在开着吗（测试钩子）。
    pub fn column_menu_is_open(&self) -> bool {
        self.col_menu
            .borrow()
            .as_ref()
            .map(|p| p.is_visible())
            .unwrap_or(false)
    }

    /// 列操作菜单里的条目文字（测试钩子）。
    pub fn column_menu_titles(&self) -> Vec<String> {
        let slot = self.col_menu.borrow();
        let Some(pop) = slot.as_ref() else { return Vec::new() };
        let Some(child) = pop.child() else { return Vec::new() };
        let mut out = Vec::new();
        let mut cur = child.first_child();
        while let Some(c) = cur {
            if let Some(b) = c.downcast_ref::<gtk::Button>() {
                if let Some(t) = crate::menu::item_label(b) {
                    out.push(t);
                }
            } else if let Some(l) = c.downcast_ref::<gtk::Label>() {
                out.push(l.text().to_string());
            }
            cur = c.next_sibling();
        }
        out
    }

    /// 同步列头的排序箭头（dbx 的列头有排序箭头）。
    ///
    /// 排序是自己实现的（不靠 GTK 的排序模型），所以这里只是**画箭头**：
    /// 让用户一眼看出当前按哪一列排的、什么方向。
    fn sync_sort_indicators(&self) {
        let order = {
            let data = self.data.borrow();
            resolve_visible(&self.col_layout.borrow(), data.columns.len())
        };
        let cols = self.view.columns();
        for (n, c) in cols.iter().enumerate() {
            let orig = order.get(n).copied();
            // ⚠️ GTK4 的 `SortType` **没有 "None"**（GTK3 有 GTK_SORT_NONE），
            // "没排序"要靠 `set_sort_indicator(false)` 表达。
            let ord = match (self.sort.get(), orig) {
                (Some((sc, true)), Some(o)) if sc == o => Some(gtk::SortType::Ascending),
                (Some((sc, false)), Some(o)) if sc == o => Some(gtk::SortType::Descending),
                _ => None,
            };
            match ord {
                Some(t) => {
                    c.set_sort_indicator(true);
                    c.set_sort_order(t);
                }
                None => c.set_sort_indicator(false),
            }
        }
    }

    /// 允许 / 禁止点列头排序。
    ///
    /// **表结构页签要用它关掉排序**：那里的行就是表的列，顺序是有含义的
    /// （生成 `ALTER TABLE` 时按原顺序来），用户一排就乱了。
    pub fn set_sortable(&self, on: bool) {
        self.sortable.set(on);
    }

    /// 设置列的顺序与可见性（`(原始列下标, 是否显示)`，按用户排的顺序）。
    ///
    /// 传空 = 全部可见、保持原序。
    ///
    /// **只影响显示**：待保存的改动、导出的 SQL 都仍按**全部列**算——
    /// 隐藏一列不该让那一列的数据从 UPDATE 里消失。
    pub fn set_column_layout(&self, layout: Vec<(usize, bool)>) {
        *self.col_layout.borrow_mut() = layout;
        self.rebuild_columns();
    }

    /// 当前列的顺序与可见性（`(原始列下标, 是否显示)`）。
    ///
    /// 没设过 = 全部可见、原序。
    pub fn column_layout(&self) -> Vec<(usize, bool)> {
        let ncols = self.data.borrow().columns.len();
        let layout = self.col_layout.borrow();
        if layout.is_empty() {
            return (0..ncols).map(|i| (i, true)).collect();
        }
        // 越界的（换了列数更少的结果集）丢掉
        layout
            .iter()
            .copied()
            .filter(|(i, _)| *i < ncols)
            .collect()
    }

    /// 当前可见的列（**原始列下标**，按显示顺序）。
    pub fn visible_columns(&self) -> Vec<usize> {
        let ncols = self.data.borrow().columns.len();
        resolve_visible(&self.col_layout.borrow(), ncols)
    }

    /// 表格清空（换连接、换查询时）。
    pub fn clear(&self) {
        *self.data.borrow_mut() = GridData::default();
        self.edits.borrow_mut().clear();
        self.inserts.borrow_mut().clear();
        self.deletes.borrow_mut().clear();
        self.visible.borrow_mut().clear();
        // 模型也换回单列（列数与数据列数必须一致，见 set_data 的说明）
        let model = gtk::ListStore::new(&[glib::Type::STRING]);
        self.view.set_model(Some(&model));
        *self.model.borrow_mut() = model;
        for c in self.view.columns() {
            self.view.remove_column(&c);
        }
        self.renderers.borrow_mut().clear();
        self.fire_data();
    }

    /* ---------------- 排序 / 筛选 ---------------- */

    /// 点列头：同一列再点一次换方向；换列则从升序开始。
    pub fn toggle_sort(&self, col: usize) {
        let next = match self.sort.get() {
            Some((c, true)) if c == col => Some((col, false)),
            _ => Some((col, true)),
        };
        self.sort.set(next);
        self.rebuild_rows();
        self.sync_sort_indicators();
    }

    /// 按某一列排序（列操作菜单的「升序 / 降序」，阶段 46）。
    pub fn sort_by_column(&self, col: usize, asc: bool) {
        self.sort.set(Some((col, asc)));
        self.rebuild_rows();
        self.sync_sort_indicators();
    }

    pub fn set_filter(&self, text: &str) {
        *self.filter.borrow_mut() = text.trim().to_lowercase();
        self.rebuild_rows();
    }

    pub fn filter_text(&self) -> String {
        self.filter.borrow().clone()
    }

    /// 当前排序列（UI 上给列头加箭头）。
    pub fn sort_state(&self) -> Option<(usize, bool)> {
        self.sort.get()
    }

    /// 重算可见行并灌进模型。
    fn rebuild_rows(&self) {
        // ⚠️ 重建前先记住选中了哪几行（重建 model 会丢选择，见函数末尾）
        let saved = self.selected_rowrefs();
        let data = self.data.borrow();
        let filter = self.filter.borrow().clone();
        let deletes = self.deletes.borrow();
        let mut idx: Vec<usize> = (0..data.rows.len())
            .filter(|&i| filter.is_empty() || row_matches(&data.rows[i], &filter))
            .collect();
        // 标记删除的行不再显示（"放弃修改"能恢复）
        idx = filter_deleted(&idx, &deletes);

        if let Some((col, asc)) = self.sort.get() {
            let ty = data.types.get(col).copied().unwrap_or(ColType::Text);
            idx.sort_by(|a, b| {
                let o = cmp_values(
                    data.rows.get(*a).and_then(|r| r.get(col)),
                    data.rows.get(*b).and_then(|r| r.get(col)),
                    ty,
                );
                if o == std::cmp::Ordering::Equal {
                    std::cmp::Ordering::Equal
                } else if asc {
                    o
                } else {
                    o.reverse()
                }
            });
        }
        // 待插入的新行固定排在最后：不参与排序，也不参与筛选
        // （它是"还没存进库的行"，按内容排序没有意义）
        let mut refs: Vec<RowRef> = idx.into_iter().map(RowRef::Src).collect();
        for i in 0..self.inserts.borrow().len() {
            refs.push(RowRef::Ins(i));
        }
        *self.visible.borrow_mut() = refs;

        let edits = self.edits.borrow();
        let inserts = self.inserts.borrow();
        let store = self.model.borrow();
        store.clear();
        for &r in self.visible.borrow().iter() {
            let vals: Vec<String> = match r {
                RowRef::Src(src) => {
                    let Some(row) = data.rows.get(src) else { continue };
                    (0..data.columns.len())
                        .map(|c| {
                            let ty = data.types.get(c).copied().unwrap_or(ColType::Text);
                            if let Some(v) = edits.get(&src).and_then(|m| m.get(&c)) {
                                return v.clone();
                            }
                            match row.get(c).and_then(|v| v.as_ref()) {
                                None => "NULL".to_string(),
                                Some(s) => match ty {
                                    // 布尔显示「是/否」（阶段 64）
                                    ColType::Bool => {
                                        bool_display(s).unwrap_or(s.as_str()).to_string()
                                    }
                                    _ => s.clone(),
                                },
                            }
                        })
                        .collect()
                }
                RowRef::Ins(i) => {
                    let row = inserts.get(i);
                    (0..data.columns.len())
                        .map(|c| {
                            // 新行里没填的格子显示空白（区别于已有行的 "NULL"）
                            row.and_then(|r| r.get(c))
                                .and_then(|v| v.clone())
                                .unwrap_or_default()
                        })
                        .collect()
                }
            };
            append_row(&store, &vals);
        }
        // ⚠️ **重建 model 会丢选择**（`store.clear()` 把选中状态清了）。
        // 多选是阶段 46 的能力、批量改数据是阶段 47 的能力——改完一个格子
        // 就"失选"的话，用户没法接着改第二行。
        // 所以重建前记住选中的是**哪几行**（按 `RowRef` 记，重建后位置变了
        // 也能找回来），重建后选回去。
        self.restore_selection(&saved);
    }

    /// 当前选中的行（按 `RowRef` 记）。
    ///
    /// 记 `RowRef` 而不是显示行号：重建之后显示行号会变（排序 / 筛选 /
    /// 删行），但"第几行数据"不会变。
    fn selected_rowrefs(&self) -> Vec<RowRef> {
        let sel = self.view.selection();
        let (paths, _model) = sel.selected_rows();
        let mut out = Vec::new();
        for p in paths {
            let Some(&row) = p.indices().first() else { continue };
            let Some(rf) = self.visible.borrow().get(row as usize).copied() else {
                continue;
            };
            out.push(rf);
        }
        out
    }

    /// 按 `RowRef` 把选中的行选回去（重建 model 之后用）。
    fn restore_selection(&self, saved: &[RowRef]) {
        if saved.is_empty() {
            return;
        }
        let sel = self.view.selection();
        sel.unselect_all();
        for (disp, rf) in self.visible.borrow().iter().enumerate() {
            if saved.contains(rf) {
                sel.select_path(&gtk::TreePath::from_indices(&[disp as i32]));
            }
        }
    }

    /* ---------------- 编辑 ---------------- */

    /// 允许/禁止编辑单元格。
    pub fn set_editable(&self, on: bool) {
        self.editable.set(on);
        for r in self.renderers.borrow().iter() {
            r.set_editable(on);
        }
    }

    pub fn is_editable(&self) -> bool {
        self.editable.get()
    }

    /// 登记一处改动（来自 `CellRendererText::edited` 信号）。
    pub fn set_edit(&self, display_row: usize, col: usize, text: &str) {
        let Some(r) = self.visible.borrow().get(display_row).copied() else {
            return;
        };
        match r {
            RowRef::Src(src) => {
                self.edits
                    .borrow_mut()
                    .entry(src)
                    .or_default()
                    .insert(col, text.to_string());
            }
            RowRef::Ins(i) => {
                let mut inserts = self.inserts.borrow_mut();
                if let Some(row) = inserts.get_mut(i) {
                    if col < row.len() {
                        row[col] = Some(text.to_string());
                    }
                }
            }
        }
        self.rebuild_rows();
        self.fire_change();
    }

    /// 有没有待保存的改动。
    pub fn has_edits(&self) -> bool {
        !self.edits.borrow().is_empty()
    }

    /// 有没有待保存的**行级**改动（插入新行 / 删除已有行）。
    pub fn has_row_changes(&self) -> bool {
        !self.inserts.borrow().is_empty() || !self.deletes.borrow().is_empty()
    }

    /// 挂一个"有改动"的回调（阶段 45：底栏「N 项待保存」实时刷新）。
    ///
    /// ⚠️ 调用方**只能抓弱引用**（见 `change_hook` 的说明）。
    pub fn set_change_hook(&self, f: Rc<dyn Fn()>) {
        *self.change_hook.borrow_mut() = Some(f);
    }

    /// 挂一个"数据变了"的回调（阶段 83：空结果态要跟着切）。
    ///
    /// 触发时机：换了一份结果集（`set_data`）/ 追加了行（`append_rows`）/
    /// 清空了（`clear`）。
    ///
    /// ⚠️ 调用方**只能抓弱引用**（见 `data_hook` 的说明）。
    pub fn set_data_hook(&self, f: Rc<dyn Fn()>) {
        *self.data_hook.borrow_mut() = Some(f);
    }

    /// 通知"数据变了"。
    fn fire_data(&self) {
        let f = self.data_hook.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// 通知"有改动"（编辑单元格 / 插入行 / 标记删除）。
    fn fire_change(&self) {
        let f = self.change_hook.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// 待保存的改动：源行号 → (列号 → 新值文本)。
    pub fn pending_edits(&self) -> HashMap<usize, HashMap<usize, String>> {
        self.edits.borrow().clone()
    }

    /// 待插入的新行（每行是"每列的输入文本"，`None` = 没填）。
    pub fn pending_inserts(&self) -> Vec<Vec<Option<String>>> {
        self.inserts.borrow().clone()
    }

    /// 待删除的源行号（升序，语句顺序稳定）。
    pub fn pending_deletes(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.deletes.borrow().iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// 追加一行待插入的新行（各列留空 = 用数据库默认值）。
    ///
    /// 返回这行在"待插入行"里的序号。新行固定显示在网格末尾。
    pub fn insert_row(&self) -> usize {
        let ncols = self.data.borrow().columns.len();
        let idx = {
            let mut inserts = self.inserts.borrow_mut();
            inserts.push(vec![None; ncols]);
            inserts.len() - 1
        };
        self.rebuild_rows();
        self.fire_change();
        idx
    }

    /// 按源行号标记删除（`delete_selected` 的底层）。
    ///
    /// 返回 false = 行号越界。标记后这一行立刻从网格里消失。
    pub fn delete_row(&self, src: usize) -> bool {
        if src >= self.data.borrow().rows.len() {
            return false;
        }
        self.deletes.borrow_mut().insert(src);
        self.rebuild_rows();
        self.fire_change();
        true
    }

    /// 按显示行号标记删除（`delete_selected` 的"按位"版本）。
    ///
    /// 显示行号是用户看到的那一行（排序/筛选后可能跟源行号不同）。
    pub fn delete_display_row(&self, display_row: usize) -> bool {
        let Some(r) = self.visible.borrow().get(display_row).copied() else {
            return false;
        };
        match r {
            RowRef::Src(src) => self.delete_row(src),
            RowRef::Ins(i) => {
                // 新行还没进库，删它等于"没插入过"
                let mut inserts = self.inserts.borrow_mut();
                if i < inserts.len() {
                    inserts.remove(i);
                }
                drop(inserts);
                self.rebuild_rows();
                self.fire_change();
                true
            }
        }
    }

    /// 把选中的行标记为待删除（从网格里消失）。
    ///
    /// **为什么是"标记"而不是直接删**：删错了要能反悔——点「放弃修改」
    /// 就能恢复（事务也会回滚）。返回标记的行数（0 = 没选中可删的行）。
    pub fn delete_selected(&self) -> usize {
        let sel = self.view.selection();
        let (paths, _model) = sel.selected_rows();
        let mut srcs: Vec<usize> = Vec::new();
        for p in paths {
            let Some(&row) = p.indices().first() else { continue };
            let Some(r) = self.visible.borrow().get(row as usize).copied() else {
                continue;
            };
            // 新行还没进库，删它等于"没插入过"——直接丢掉这一行
            if let RowRef::Ins(i) = r {
                let mut inserts = self.inserts.borrow_mut();
                if i < inserts.len() {
                    inserts.remove(i);
                }
                continue;
            }
            if let RowRef::Src(src) = r {
                srcs.push(src);
            }
        }
        if srcs.is_empty() {
            return 0;
        }
        {
            let mut deletes = self.deletes.borrow_mut();
            for s in srcs {
                deletes.insert(s);
            }
        }
        self.rebuild_rows();
        self.deletes.borrow().len()
    }

    /// 放弃全部改动（回滚到原始值）：改过的格子、插入的新行、删除的行全撤销。
    pub fn discard_edits(&self) {
        self.edits.borrow_mut().clear();
        self.inserts.borrow_mut().clear();
        self.deletes.borrow_mut().clear();
        self.rebuild_rows();
    }

    /// 提交成功后：把改动并回原始数据（并清空改动记录）。
    ///
    /// **插入的新行**：填了值的格子写进去，没填的写 `None`——真值（默认值、
    /// 自增 id）只有数据库知道，所以提交后**还要重新查一次表**才准
    /// （见 `dbui` 的保存流程）。
    pub fn apply_edits(&self) {
        {
            let mut data = self.data.borrow_mut();
            let mut edits = self.edits.borrow_mut();
            let deletes = self.deletes.borrow();
            let inserts = self.inserts.borrow();
            let mut new_rows: Vec<Vec<Option<String>>> = Vec::new();
            for (i, row) in data.rows.iter().enumerate() {
                // 标记删除的行不保留
                if deletes.contains(&i) {
                    continue;
                }
                let mut r = row.clone();
                if let Some(cols) = edits.get(&i) {
                    for (col, text) in cols.iter() {
                        if let Some(cell) = r.get_mut(*col) {
                            *cell = Some(text.clone());
                        }
                    }
                }
                new_rows.push(r);
            }
            // 新行追加到末尾（没填的列 = NULL，实际值以后端刷新为准）
            for ins in inserts.iter() {
                let r: Vec<Option<String>> = (0..data.columns.len())
                    .map(|c| ins.get(c).and_then(|v| v.clone()))
                    .collect();
                new_rows.push(r);
            }
            data.rows = new_rows;
            edits.clear();
        }
        self.inserts.borrow_mut().clear();
        self.deletes.borrow_mut().clear();
        self.rebuild_rows();
    }

    /// 某列的原始值（WHERE 条件用）；`None` = SQL NULL。
    pub fn original_cell(&self, row: usize, col: usize) -> Option<Option<String>> {
        self.data.borrow().rows.get(row).and_then(|r| r.get(col)).cloned()
    }

    /// 全部原始行（拼 UPDATE 的 WHERE 条件时用）。
    pub fn original_rows(&self) -> Vec<Vec<Option<String>>> {
        self.data.borrow().rows.clone()
    }

    pub fn row_count(&self) -> usize {
        self.data.borrow().rows.len()
    }

    pub fn visible_count(&self) -> usize {
        self.visible.borrow().len()
    }

    pub fn columns(&self) -> Vec<String> {
        self.data.borrow().columns.clone()
    }

    pub fn types(&self) -> Vec<ColType> {
        self.data.borrow().types.clone()
    }

    pub fn pk(&self) -> Vec<usize> {
        self.data.borrow().pk.clone()
    }

    /// 列名 → 列类型（**按原始列顺序**建表）。
    ///
    /// ⚠️ **必须按原始列来建，不能按"屏幕上的第几列"**：隐藏过几列以后，
    /// 显示位置跟原始列下标就错位了——按位置取类型会把 `int` 列当成
    /// `text` 列，生成出来的 WHERE 值就少了引号（或多了引号），
    /// 条件条里挑「整数列 = 1」会拼出 `"id" = '1'`，PG 直接报错。
    ///
    /// `data.columns` 与 `data.types` 本来就是同序的（`set_data` 一次收下），
    /// 所以这里直接按序配对即可。
    pub fn column_type_map(&self) -> HashMap<String, ColType> {
        let data = self.data.borrow();
        data.columns
            .iter()
            .cloned()
            .zip(data.types.iter().copied())
            .collect()
    }

    /* ---------------- 复制 / 导出 ---------------- */

    /// 当前显示的内容（按可见顺序，含未保存的编辑）。
    pub fn visible_values(&self) -> Vec<Vec<String>> {
        let data = self.data.borrow();
        let edits = self.edits.borrow();
        let inserts = self.inserts.borrow();
        self.visible
            .borrow()
            .iter()
            .filter_map(|&r| match r {
                RowRef::Src(src) => {
                    let row = data.rows.get(src)?;
                    Some(
                        (0..data.columns.len())
                            .map(|c| {
                                if let Some(v) = edits.get(&src).and_then(|m| m.get(&c)) {
                                    return v.clone();
                                }
                                match row.get(c).and_then(|v| v.as_ref()) {
                                    None => "NULL".to_string(),
                                    Some(s) => s.clone(),
                                }
                            })
                            .collect(),
                    )
                }
                RowRef::Ins(i) => {
                    let row = inserts.get(i)?;
                    Some(
                        (0..data.columns.len())
                            .map(|c| {
                                row.get(c)
                                    .and_then(|v| v.clone())
                                    .unwrap_or_default()
                            })
                            .collect(),
                    )
                }
            })
            .collect()
    }

    /// 当前显示的内容（按可见顺序），**保留 NULL 与未保存的改动**。
    ///
    /// 导出 JSON / XML 时要用这个：`visible_values()` 把 NULL 转成了字符串
    /// "NULL"（CSV 那样写没问题），但 JSON 里应该是 `null`、XML 里应该是
    /// 空元素——所以要拿到"到底是 NULL 还是空串"这个信息。
    pub fn visible_values_opt(&self) -> Vec<Vec<Option<String>>> {
        let data = self.data.borrow();
        let edits = self.edits.borrow();
        let inserts = self.inserts.borrow();
        self.visible
            .borrow()
            .iter()
            .filter_map(|&r| match r {
                RowRef::Src(src) => {
                    let row = data.rows.get(src)?;
                    Some(
                        (0..data.columns.len())
                            .map(|c| {
                                // 改过的格子优先（用户改的值）
                                if let Some(v) = edits.get(&src).and_then(|m| m.get(&c)) {
                                    return Some(v.clone());
                                }
                                row.get(c).cloned().flatten()
                            })
                            .collect(),
                    )
                }
                RowRef::Ins(i) => {
                    let row = inserts.get(i)?;
                    Some(
                        (0..data.columns.len())
                            .map(|c| row.get(c).cloned().flatten())
                            .collect(),
                    )
                }
            })
            .collect()
    }

    /// 选中的行（按可见顺序）。没有选中时返回空。
    pub fn selected_rows(&self) -> Vec<Vec<String>> {
        let sel = self.view.selection();
        let (paths, _model) = sel.selected_rows();
        let mut idx: Vec<usize> = paths
            .iter()
            .filter_map(|p| p.indices().first().map(|i| *i as usize))
            .collect();
        idx.sort_unstable();
        let all = self.visible_values();
        idx.into_iter().filter_map(|i| all.get(i).cloned()).collect()
    }

    /// 选中的行的**显示行号**（升序）。没有选中时返回空。
    ///
    /// 删除行要按显示行号定位（排序/筛选后它跟源行号不是一回事）。
    pub fn selected_display_rows(&self) -> Vec<usize> {
        let sel = self.view.selection();
        let (paths, _model) = sel.selected_rows();
        let mut idx: Vec<usize> = paths
            .iter()
            .filter_map(|p| p.indices().first().map(|i| *i as usize))
            .collect();
        idx.sort_unstable();
        idx
    }

    /// 显示行号 → **源行号**（排序 / 筛选后两者不是一回事）。
    ///
    /// 要按行回原始数据时用这个：比如搜索结果里双击一行，得知道它对应
    /// 第几条命中（列表可能被排过序）。新插入的行没有源行号，返回 `None`。
    pub fn source_row(&self, display_row: usize) -> Option<usize> {
        match self.visible.borrow().get(display_row) {
            Some(RowRef::Src(src)) => Some(*src),
            _ => None,
        }
    }

    /// 选中第几行（按显示顺序）。给测试钩子用。
    ///
    /// 模型的行序就是显示序（`rebuild_rows` 按显示顺序填模型），所以
    /// 直接按显示行号取路径。
    pub fn select_display_row(&self, display_row: usize) -> bool {
        if display_row >= self.visible.borrow().len() {
            return false;
        }
        let Some(model) = self.view.model() else {
            return false;
        };
        let path = gtk::TreePath::from_indices(&[display_row as i32]);
        self.view.selection().select_path(&path);
        true
    }

    /// 把内容写成 CSV（Excel 友好：UTF-8 BOM + 引号转义）。
    pub fn to_csv(&self, include_header: bool) -> String {
        let mut out = String::from("\u{feff}");
        if include_header {
            out.push_str(
                &self
                    .columns()
                    .iter()
                    .map(|c| csv_field(c))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
        for row in self.visible_values() {
            out.push_str(
                &row.iter()
                    .map(|c| csv_field(c))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
        out
    }
}

/// 算出可见列的**原始下标**顺序（纯函数，方便单测）。
///
/// - `layout` 空 = 全部可见、保持原序（默认）；
/// - 顺序按用户排的来（Navicat 的字段显示设置能调列顺序）；
/// - 越界的下标丢掉（换了列数更少的结果集时，旧设置里的列不存在了）。
fn resolve_visible(layout: &[(usize, bool)], ncols: usize) -> Vec<usize> {
    if layout.is_empty() {
        return (0..ncols).collect();
    }
    layout
        .iter()
        .copied()
        .filter(|(i, on)| *on && *i < ncols)
        .map(|(i, _)| i)
        .collect()
}

/// 量一段文字的像素宽（pango）。
///
/// **为什么用 pango 而不是"按字数估"**：一个汉字占两个西文字宽，估出来的
/// 宽度跟实际差一大截 ⇒ 列宽分配就歪了。
fn measure_px(layout: &gtk::pango::Layout, s: &str) -> i32 {
    if s.is_empty() {
        return 0;
    }
    layout.set_text(s);
    layout.pixel_size().0
}

/// 给 `ListStore` 追加一行（gtk4-rs 的 `append` 不带值，值要另行 `set`）。
fn append_row(store: &gtk::ListStore, vals: &[String]) {
    let it = store.append();
    let pairs: Vec<(u32, &dyn ToValue)> = vals
        .iter()
        .enumerate()
        .map(|(i, v)| (i as u32, v as &dyn ToValue))
        .collect();
    store.set(&it, &pairs);
}

/// CSV 字段转义：含分隔符/引号/换行时加引号，内部引号翻倍。
pub fn csv_field(s: &str) -> String {
    // ⚠️ **实现在 backend 那边**（`dbexport::csv_field`）：同一批数据从结果
    // 网格导出和从「导出表…」导出，文件**必须长得一样**——两份实现迟早
    // 会漂（改了一边忘了另一边，导出的文件格式就不一致了）。
    crate::backend::dbexport::csv_field(s)
}

/// 过滤掉标记为待删除的行（纯逻辑，方便单测）。
///
/// **为什么要抽成纯函数**：GTK 的 widget 只能在**主线程**建，普通单测里
/// 建不了 `DbGrid`——涉及 widget 的行为放到 `examples/db_ui_e2e.rs` 里
/// 用真链路验，这里只留能纯算的部分。
pub fn filter_deleted(srcs: &[usize], deleted: &HashSet<usize>) -> Vec<usize> {
    srcs.iter().copied().filter(|i| !deleted.contains(i)).collect()
}

/// 行是否命中筛选关键字（任一单元格包含）。
fn row_matches(row: &[Option<String>], needle: &str) -> bool {
    row.iter().any(|c| {
        c.as_deref()
            .map(|s| s.to_lowercase().contains(needle))
            .unwrap_or(false)
    })
}

/// 值比较：NULL 视为最大值；数字列按数值比；其余按字符串比。
/// 布尔值的中文显示（阶段 64）。
///
/// PG 的布尔在文本协议里是 `"t"` / `"f"` ⇒ 直接显示字母，用户得自己翻译。
///
/// ⚠️ **只改显示**：模型里显示「是」，但 `data.rows` 里存的还是原始值 ⇒
/// 排序 / 提交用的都是原始值，不受影响。
fn bool_display(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "t" | "true" => Some("是"),
        "f" | "false" => Some("否"),
        _ => None,
    }
}

/// 布尔值解析（PG 给 "t"/"f"，MySQL 给 "1"/"0"）。
fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "yes" | "y" => Some(true),
        "f" | "false" | "0" | "no" | "n" => Some(false),
        _ => None,
    }
}

fn cmp_values(
    a: Option<&Option<String>>,
    b: Option<&Option<String>>,
    ty: ColType,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => match (x, y) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => match ty {
                ColType::Int => match (x.parse::<i64>(), y.parse::<i64>()) {
                    (Ok(a), Ok(b)) => a.cmp(&b),
                    _ => x.cmp(y),
                },
                ColType::Float => match (x.parse::<f64>(), y.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    _ => x.cmp(y),
                },
                // ⚠️ **布尔必须按真假比**（阶段 64）：按字典序的话 "f" < "t"
                // 恰好也对，但 "false" < "true"、而 PG 给的是 "t"/"f" ⇒
                // 同一列里混着两种写法时排序会乱。
                ColType::Bool => match (parse_bool(x), parse_bool(y)) {
                    (Some(a), Some(b)) => a.cmp(&b), // false < true
                    _ => x.cmp(y),
                },
                _ => x.cmp(y),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> GridData {
        GridData {
            columns: vec!["id".into(), "name".into()],
            types: vec![ColType::Int, ColType::Text],
            rows: vec![
                vec![Some("2".into()), Some("乙".into())],
                vec![Some("10".into()), Some("甲".into())],
                vec![None, Some("丙".into())],
            ],
            pk: vec![0],
        }
    }

    /// 列显示设置：可见列存的是**原始列下标**，不是显示位置。
    ///
    /// 这条语义一旦搞混，后果很严重：隐藏几列以后，显示位置跟原始下标就
    /// 错位了，编辑单元格时会把改动记到别的列上（改 A 列写进 B 列）。
    /// 数字列按**数值**排序（阶段 64 / 审计 P0-3）。
    ///
    /// 按字典序的话 "10" 排在 "2" 前面 —— 用户点一下列头就发现"排错了"。
    #[test]
    fn 数字列按数值排序() {
        use std::cmp::Ordering;
        // ⚠️ `cmp_values` 的参数是 `Option<&Option<String>>`：
        // 外层 = 这一格不存在；内层 = 格子的值（None = SQL NULL）。
        let a: Option<String> = Some("2".to_string());
        let b: Option<String> = Some("10".to_string());
        assert_eq!(
            cmp_values(Some(&a.clone()), Some(&b.clone()), ColType::Int),
            Ordering::Less
        );
        assert_eq!(
            cmp_values(Some(&b), Some(&a), ColType::Int),
            Ordering::Greater
        );
    }

    /// 布尔列按**真假**排序（阶段 64）。
    ///
    /// PG 的布尔在文本协议里是 "t"/"f"，MySQL 是 "1"/"0" ⇒ 两种都得认。
    #[test]
    fn 布尔列按真假排序() {
        use std::cmp::Ordering;
        for (f, t) in [("f", "t"), ("false", "true"), ("0", "1")] {
            let fv: Option<String> = Some(f.to_string());
            let tv: Option<String> = Some(t.to_string());
            assert_eq!(
                cmp_values(Some(&fv.clone()), Some(&tv.clone()), ColType::Bool),
                Ordering::Less,
                "{f} 应当排在 {t} 前面"
            );
            assert_eq!(
                cmp_values(Some(&tv), Some(&fv), ColType::Bool),
                Ordering::Greater
            );
        }
    }

    /// 布尔显示「是/否」（阶段 64）。
    #[test]
    fn 布尔显示是与否() {
        assert_eq!(bool_display("t"), Some("是"));
        assert_eq!(bool_display("f"), Some("否"));
        assert_eq!(bool_display("true"), Some("是"));
        assert_eq!(bool_display("false"), Some("否"));
        // 认不出来就原样显示（别把看不懂的东西也翻译成「否」）
        assert_eq!(bool_display("maybe"), None);
    }

    #[test]
    fn 可见列存的是原始下标() {
        // 空 = 全部可见、原序
        assert_eq!(resolve_visible(&[], 3), vec![0, 1, 2]);
        // 顺序按用户排的来（可以调列顺序）
        assert_eq!(resolve_visible(&[(2, true), (0, true)], 3), vec![2, 0]);
        // 藏起来的列不显示，但**位置还留着**（重开设置框时顺序不变）
        assert_eq!(
            resolve_visible(&[(0, true), (1, false), (2, true)], 3),
            vec![0, 2]
        );
        // 越界的丢掉（换了列数更少的结果集）
        assert_eq!(resolve_visible(&[(0, true), (5, true), (1, true)], 3), vec![0, 1]);
        // 全藏 = 空表
        assert!(resolve_visible(&[], 0).is_empty());
        assert!(resolve_visible(&[(0, false), (1, false)], 2).is_empty());
    }

    /// 转置（阶段 47 的「转置查看」）：行 ↔ 列互换。
    #[test]
    fn 转置把行列互换() {
        let d = data();
        let t = transpose(&d);
        // 2 列 3 行 → 3 列 2 行
        assert_eq!(t.columns.len(), 3);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.columns[0], "第 1 行");
        // 第一行原来是 [2, 乙] → 转置后第 1 列是 [2, 10, NULL]
        assert_eq!(t.rows[0][0], Some("2".to_string()));
        assert_eq!(t.rows[0][1], Some("10".to_string()));
        assert_eq!(t.rows[0][2], None);
        // 第二行原来是 [name 列]：乙 / 甲 / 丙
        assert_eq!(t.rows[1][0], Some("乙".to_string()));
        assert_eq!(t.rows[1][2], Some("丙".to_string()));
        // 类型全退化成文本、主键没了
        assert!(t.types.iter().all(|x| *x == ColType::Text));
        assert!(t.pk.is_empty());
    }

    #[test]
    fn csv_field_escapes_quotes_and_commas() {        assert_eq!(csv_field("a"), "a");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn comparison_puts_null_last_and_numbers_numerically() {
        // "2" < "10" 按数值，但按字符串是 "10" < "2"
        assert_eq!(
            cmp_values(
                Some(&Some("2".into())),
                Some(&Some("10".into())),
                ColType::Int
            ),
            std::cmp::Ordering::Less
        );
        // NULL 恒最后
        assert_eq!(
            cmp_values(Some(&None), Some(&Some("1".into())), ColType::Int),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            cmp_values(Some(&Some("1".into())), Some(&None), ColType::Int),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn filter_matches_any_cell_case_insensitively() {
        let row = vec![Some("1".into()), Some("Hello".into())];
        assert!(row_matches(&row, "hel"));
        assert!(!row_matches(&row, "xyz"));
    }

    #[test]
    fn grid_data_shape_is_kept() {
        let d = data();
        assert_eq!(d.columns.len(), 2);
        assert_eq!(d.rows.len(), 3);
        assert!(d.rows[2][0].is_none(), "第三行第一列应为 NULL");
        assert_eq!(d.pk, vec![0]);
    }

    /* ---------------- 插入 / 删除行（9.4） ---------------- */

    /// 涉及 widget 的行为（插入行显示在末尾、删除后隐藏、编辑新行…）
    /// 在 `examples/db_ui_e2e.rs` 里用真链路验——GTK 的 widget 只能在
    /// **主线程**建，普通单测里建不了 `DbGrid`。
    /// 这里只留能纯算的部分。
    #[test]
    fn deleted_rows_are_filtered_out() {
        let mut deleted = HashSet::new();
        deleted.insert(1);
        deleted.insert(3);
        assert_eq!(filter_deleted(&[0, 1, 2, 3, 4], &deleted), vec![0, 2, 4]);
        // 一个都没标记 → 原样
        assert_eq!(filter_deleted(&[0, 1], &HashSet::new()), vec![0, 1]);
    }
}
