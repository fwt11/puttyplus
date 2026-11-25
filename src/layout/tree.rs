//! 格子树（阶段 144）—— **纯数据**（不碰 GTK ⇒ 单测直接测）。
//!
//! **树长这样**：
//! ```text
//! Split(stacked=false)
//! ├── a: Split(stacked=true)
//! │   ├── a: Leaf(格子1)
//! │   └── b: Leaf(格子2)
//! └── b: Leaf(格子3)
//! ```
//!
//! ⚠️ **为什么把 GTK 与树分开**：树的操作（拆分 / 关闭 / 查找）是最容易
//! 出错的地方 ⇒ 做成**纯函数**才能被单测覆盖（20+ 条用例）⇒
//! GTK 那层只负责"把树画出来"。

use gtk::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::cell::{cell_widget, Cell, CellId};

/// 格子树的节点。
///
/// ⚠️ **叶子只存编号**：真正的内容（`SubTabs`）由 [`CellTree`] 按编号保管 ⇒
/// 树本身是**纯数据** ⇒ 可以直接单测。
#[derive(Debug, Clone, PartialEq)]
pub enum TreeNode {
    /// 一个叶子格子。
    Leaf(CellId),
    /// 两个子树（`a` 在前、`b` 在后）。
    Split {
        /// `true` = 上下叠放（竖向），`false` = 左右并排（横向）。
        stacked: bool,
        a: Box<TreeNode>,
        b: Box<TreeNode>,
    },
    /// **空了**（关掉了最后一个格子）。
    ///
    /// ⚠️ **为什么要有这个节点**：关掉最后一个格子后，树得有个说法 ——
    /// 用 `Option` 的话"没找到目标"与"关空了"就分不清了。
    Empty,
}

/// 树里所有叶子的编号（**左到右 / 上到下的顺序**）。
pub fn flatten(node: &TreeNode) -> Vec<CellId> {
    let mut out = Vec::new();
    collect(node, &mut out);
    out
}

fn collect(node: &TreeNode, out: &mut Vec<CellId>) {
    match node {
        TreeNode::Leaf(id) => out.push(*id),
        TreeNode::Split { a, b, .. } => {
            collect(a, out);
            collect(b, out);
        }
        TreeNode::Empty => {}
    }
}

/// 找某个格子所在的**叶子节点**。
pub fn leaf_of(node: &TreeNode, id: CellId) -> Option<&TreeNode> {
    match node {
        TreeNode::Leaf(x) => (x == &id).then_some(node),
        TreeNode::Split { a, b, .. } => leaf_of(a, id).or_else(|| leaf_of(b, id)),
        TreeNode::Empty => None,
    }
}

/// 找某个格子所在的叶子（**可变**版本）。
pub fn leaf_of_mut(node: &mut TreeNode, id: CellId) -> Option<&mut TreeNode> {
    match node {
        TreeNode::Leaf(x) => (x == &id).then_some(node),
        TreeNode::Split { a, b, .. } => leaf_of_mut(a, id).or_else(|| leaf_of_mut(b, id)),
        TreeNode::Empty => None,
    }
}

/// **拆分**：把 `target` 那个格子拆成两个（新格子 `new_id` 在**后面**）。
///
/// - `stacked = false` ⇒ 左右并排（新格子在右）；
/// - `stacked = true` ⇒ 上下叠放（新格子在下）。
///
/// ⚠️ **返回新树**（原树不动）⇒ 纯函数、容易测。
/// 目标找不到 ⇒ `Err(())`（树不变）。
pub fn split(node: &TreeNode, target: CellId, new_id: CellId, stacked: bool) -> Result<TreeNode, ()> {
    match node {
        TreeNode::Leaf(x) if *x == target => Ok(TreeNode::Split {
            stacked,
            a: Box::new(TreeNode::Leaf(target)),
            b: Box::new(TreeNode::Leaf(new_id)),
        }),
        TreeNode::Leaf(_) => Err(()),
        TreeNode::Empty => Err(()),
        TreeNode::Split { stacked: s, a, b } => {
            // ⚠️ **不能直接 `?`**：目标可能在 `b` 里 ⇒ 先试 `a`，
            // 失败（"不在 a 里"）才去试 `b`（与 `close_inner` 同一套路）。
            // 之前写成 `split(a, ...)?` ⇒ 只有每组的**第一个**叶子能被拆。
            match split(a, target, new_id, stacked) {
                Ok(na) => Ok(TreeNode::Split {
                    stacked: *s,
                    a: Box::new(na),
                    b: b.clone(),
                }),
                Err(()) => {
                    let nb = split(b, target, new_id, stacked)?;
                    Ok(TreeNode::Split {
                        stacked: *s,
                        a: a.clone(),
                        b: Box::new(nb),
                    })
                }
            }
        }
    }
}

/// **拆分**（新格子在**前面**）—— 拖到某格**左边 / 上边**时用。
///
/// ⚠️ **与 [`split`] 的区别**：[`split`] 把新格子放在**后面**
///    （拖到右边 / 下边）；这里放在**前面**（拖到左边 / 上边）。
pub fn split_before(
    node: &TreeNode,
    target: CellId,
    new_id: CellId,
    stacked: bool,
) -> Result<TreeNode, ()> {
    match node {
        TreeNode::Leaf(x) if *x == target => Ok(TreeNode::Split {
            stacked,
            a: Box::new(TreeNode::Leaf(new_id)),
            b: Box::new(TreeNode::Leaf(target)),
        }),
        TreeNode::Leaf(_) => Err(()),
        TreeNode::Empty => Err(()),
        TreeNode::Split { stacked: s, a, b } => {
            match split_before(a, target, new_id, stacked) {
                Ok(na) => Ok(TreeNode::Split {
                    stacked: *s,
                    a: Box::new(na),
                    b: b.clone(),
                }),
                Err(()) => {
                    let nb = split_before(b, target, new_id, stacked)?;
                    Ok(TreeNode::Split {
                        stacked: *s,
                        a: a.clone(),
                        b: Box::new(nb),
                    })
                }
            }
        }
    }
}

/// **关闭**：把 `target` 那个叶子摘掉。
///
/// - `Ok(tree)` ⇒ 关掉了（`tree` 可能是 [`TreeNode::Empty`]）；
/// - `Err(())` ⇒ 树里没有这个格子（**树没动**）。
///
/// ⚠️ **父节点自动收拢**：摘掉一个叶子后，如果某个 `Split` 只剩一个孩子，
/// 就**把它替换成那个孩子**（否则会留下一个空格子 ⇒ 视觉上多一条缝）。
pub fn close(node: &TreeNode, target: CellId) -> Result<TreeNode, ()> {
    close_inner(node, target)
}

fn close_inner(node: &TreeNode, target: CellId) -> Result<TreeNode, ()> {
    match node {
        TreeNode::Leaf(x) => {
            if *x == target {
                Ok(TreeNode::Empty)
            } else {
                Err(())
            }
        }
        TreeNode::Empty => Err(()),
        TreeNode::Split { stacked, a, b } => {
            // 先试 a：目标在里面
            match close_inner(a, target) {
                Ok(na) => Ok(merge(*stacked, na, (**b).clone())),
                Err(()) => {
                    let nb = close_inner(b, target)?;
                    Ok(merge(*stacked, (**a).clone(), nb))
                }
            }
        }
    }
}

/// 两个子树合并：**空的顶替**（父节点收拢）。
fn merge(stacked: bool, a: TreeNode, b: TreeNode) -> TreeNode {
    match (a, b) {
        (TreeNode::Empty, b) => b,
        (a, TreeNode::Empty) => a,
        (a, b) => TreeNode::Split {
            stacked,
            a: Box::new(a),
            b: Box::new(b),
        },
    }
}

/// 树里的叶子数（**格子数**）。
pub fn leaf_count(node: &TreeNode) -> usize {
    flatten(node).len()
}

/// 把一棵 widget 树**拆开**（Paned 的孩子逐个摘掉）。
///
/// ⚠️ **为什么需要**：同一个 widget 不能同时挂两个父 ⇒ 重画之前
/// 必须把旧 Paned 里的孩子**摘干净**，否则 GTK 报 "already has a parent"。
fn detach(w: &gtk::Widget) {
    let Some(p) = w.downcast_ref::<gtk::Paned>() else {
        return;
    };
    if let Some(c) = p.start_child() {
        p.set_start_child(None::<&gtk::Widget>);
        detach(&c);
    }
    if let Some(c) = p.end_child() {
        p.set_end_child(None::<&gtk::Widget>);
        detach(&c);
    }
}

/// 把一棵树画成 widget（**递归**：Split ⇒ 一个 Paned）。
///
/// ⚠️ **空树没有 widget**（`Empty` ⇒ `None`）⇒ 调用方自己决定"空着显示什么"。
fn widget_for(
    node: &TreeNode,
    cells: &HashMap<CellId, Rc<Cell>>,
    pos: &SplitPos,
) -> Option<gtk::Widget> {
    match node {
        TreeNode::Leaf(id) => cells.get(id).map(|c| cell_widget(c).clone().upcast()),
        TreeNode::Split { stacked, a, b } => {
            let wa = widget_for(a, cells, pos)?;
            let wb = widget_for(b, cells, pos)?;
            let paned = gtk::Paned::new(if *stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            });
            paned.set_start_child(Some(&wa));
            paned.set_end_child(Some(&wb));
            // ⚠️ **阶段 150：把分割条的位置恢复回来**——重画时 Paned 全换了，
            //    不恢复的话**用户拖出来的宽度就没了**（文件面板会突然变得很
            //    宽：GTK4 的默认 position 是**第一个子控件的自然宽度**）。
            if let Some(v) = split_key(node).and_then(|k| pos.get(&k).copied()) {
                paned.set_position(v);
            }
            Some(paned.upcast())
        }
        TreeNode::Empty => None,
    }
}

/// 树节点的身份：**两侧的第一个叶子格子**。
///
/// ⚠️ **为什么用这个当分割条的 key**（阶段 150）：重画时**所有 Paned 都会
///    被重新造一遍** ⇒ 分割条的位置会丢 ⇒ 得能认出「重画前后的**同一个**
///    分割条」。
///    - 用「位置序号」⇒ 拆分 / 关闭后**顺序变了** ⇒ 会认错；
///    - 用「下面的全部叶子集合」⇒ 拆分后集合变了 ⇒ 认不出；
///    - 用「**两侧的第一个叶子**」⇒ 拆分右半时左半还是那个格子、右半的
///      第一个叶子也还是那个格子 ⇒ **key 不变** ⇒ 位置能保住。
fn split_key(node: &TreeNode) -> Option<(CellId, CellId)> {
    let TreeNode::Split { a, b, .. } = node else {
        return None;
    };
    Some((first_leaf(a)?, first_leaf(b)?))
}

/// 某个节点里**最左边的那个叶子**（`TreeNode::Split` 里 `a` 那一侧）。
fn first_leaf(node: &TreeNode) -> Option<CellId> {
    match node {
        TreeNode::Leaf(id) => Some(*id),
        TreeNode::Split { a, .. } => first_leaf(a),
        TreeNode::Empty => None,
    }
}

/// 分割条位置表：**key = 分割条的身份** ⇒ 值 = 它的 position。
type SplitPos = HashMap<(CellId, CellId), i32>;

/// 从**旧 widget 树**里收集每个分割条的位置（同步走旧树节点）。
///
/// ⚠️ **为什么必须走旧树**：光看 widget 树**认不出**哪个 Paned 是哪个分割
///    条 ⇒ 得靠树节点给出身份（`split_key`）。
/// ⚠️ **为什么配对不会错**：`widget_for` 一直是「`a` 放左、`b` 放右」⇒
///    树与 widget 一一对应。
fn collect_positions(widget: &gtk::Widget, node: &TreeNode, out: &mut SplitPos) {
    let TreeNode::Split { a, b, .. } = node else {
        return;
    };
    let Some(p) = widget.downcast_ref::<gtk::Paned>() else {
        return;
    };
    if let Some(key) = split_key(node) {
        out.insert(key, p.position());
    }
    if let Some(sc) = p.start_child() {
        collect_positions(&sc, a, out);
    }
    if let Some(ec) = p.end_child() {
        collect_positions(&ec, b, out);
    }
}

/// 按位置表**恢复**分割条的位置（重画之后调）。
fn restore_positions(widget: &gtk::Widget, node: &TreeNode, pos: &SplitPos) {
    let TreeNode::Split { a, b, .. } = node else {
        return;
    };
    let Some(p) = widget.downcast_ref::<gtk::Paned>() else {
        return;
    };
    if let Some(v) = split_key(node).and_then(|k| pos.get(&k).copied()) {
        p.set_position(v);
    }
    if let Some(sc) = p.start_child() {
        restore_positions(&sc, a, pos);
    }
    if let Some(ec) = p.end_child() {
        restore_positions(&ec, b, pos);
    }
}

/// 树 → **可存盘的布局**（只记形状 ＋ 页签种类）。
fn snapshot_of(
    node: &TreeNode,
    cells: &HashMap<CellId, Rc<Cell>>,
) -> crate::layout::persist::LayoutNode {
    use crate::layout::persist::{LayoutNode, SlotKind};
    match node {
        TreeNode::Leaf(id) => {
            let tabs = cells
                .get(id)
                .map(|c| {
                    // ⚠️ **阶段 149：用 `slot_kinds()`**（按 notebook 页序）
                    //    ⇒ **编辑器也在里面**（它也是格子里的页签）⇒ 存盘
                    //    记得住「这一格开着个编辑器」。
                    c.tabs
                        .slot_kinds()
                        .into_iter()
                        .map(|k| match k {
                            crate::panel::subtabs::PageKind::Terminal => SlotKind::Terminal,
                            crate::panel::subtabs::PageKind::Db => SlotKind::Db,
                            crate::panel::subtabs::PageKind::Docker => SlotKind::Docker,
                            crate::panel::subtabs::PageKind::Files => SlotKind::Files,
                            crate::panel::subtabs::PageKind::Editor => SlotKind::Editor,
                            crate::panel::subtabs::PageKind::Other => SlotKind::Other,
                        })
                        .collect()
                })
                .unwrap_or_default();
            LayoutNode::Leaf { tabs }
        }
        TreeNode::Split { stacked, a, b } => LayoutNode::Split {
            stacked: *stacked,
            a: Box::new(snapshot_of(a, cells)),
            b: Box::new(snapshot_of(b, cells)),
        },
        TreeNode::Empty => LayoutNode::Leaf { tabs: Vec::new() },
    }
}

/// 布局 → 树（`leaf_tabs` 按叶子顺序取用）。
fn build_tree(
    layout: &crate::layout::persist::LayoutNode,
    leaf_tabs: &[Rc<crate::panel::subtabs::SubTabs>],
    next: &mut usize,
    next_id: &mut u64,
) -> Option<TreeNode> {
    use crate::layout::persist::LayoutNode;
    match layout {
        LayoutNode::Split { stacked, a, b } => Some(TreeNode::Split {
            stacked: *stacked,
            a: Box::new(build_tree(a, leaf_tabs, next, next_id)?),
            b: Box::new(build_tree(b, leaf_tabs, next, next_id)?),
        }),
        LayoutNode::Leaf { .. } => {
            let id = CellId(*next_id);
            *next_id += 1;
            *next = next.saturating_add(1);
            Some(TreeNode::Leaf(id))
        }
    }
}

/// 树 → 格子表（按叶子顺序把容器配给格子）。
fn collect_cells(
    tree: &TreeNode,
    leaf_tabs: &[Rc<crate::panel::subtabs::SubTabs>],
    out: &mut HashMap<CellId, Rc<Cell>>,
) {
    let mut leaves: Vec<CellId> = Vec::new();
    collect_leaf_ids(tree, &mut leaves);
    for (i, id) in leaves.into_iter().enumerate() {
        let Some(tabs) = leaf_tabs.get(i) else { continue };
        out.insert(id, Rc::new(Cell::new(id, tabs.clone())));
    }
}

fn collect_leaf_ids(node: &TreeNode, out: &mut Vec<CellId>) {
    match node {
        TreeNode::Leaf(id) => out.push(*id),
        TreeNode::Split { a, b, .. } => {
            collect_leaf_ids(a, out);
            collect_leaf_ids(b, out);
        }
        TreeNode::Empty => {}
    }
}

/// 格子树（**持有树 + 内容 + GTK 根容器**）。
///
/// ⚠️ **阶段 144 的初始状态 = 一个叶子** ⇒ 界面与今天**逐像素一致**。
pub struct CellTree {
    tree: RefCell<TreeNode>,
    cells: RefCell<HashMap<CellId, Rc<Cell>>>,
    /// 根容器：里面放的是"当前树根对应的 widget"。
    ///
    /// ⚠️ **为什么要包一层**：格子树形状变了（拆分）⇒ 根 widget 会变 ⇒
    /// 外面得有个稳定的容器来换里面的 child（否则连接页的 widget 得整个换掉）。
    /// 这层 Box **没有留白**（间距 0、无 margin）⇒ 视觉上与今天一致。
    host: gtk::Box,
    next_id: std::cell::Cell<u64>,
    /// **当前格子**（顶栏的「拆分格子 / 关闭本格」作用在它身上）。
    ///
    /// ⚠️ **怎么定"当前"**：谁被点过 / 谁刚切过页签 ⇒ 两个信号
    ///    （`switch-page` ＋ 格子里的点击手势）⇒ 取最后动的那个。
    current: std::cell::Cell<CellId>,
    /// **「家」格子**（新开的页——编辑器 / 数据库 / 容器——该挂到哪一格）。
    ///
    /// ⚠️ **为什么要有**（2026-11-25 修的真 bug）：
    ///   格子会增删（**关掉某一格里的最后一个页签 ⇒ 那一格收拢**）⇒
    ///   之前记着的那个容器**已经不在树上了** ⇒ 把页挂上去 = 挂在**看不见
    ///   的地方** ⇒ 用户点「打开文件编辑」**毫无反应**（用户 2026-11-25 报的
    ///   「关掉终端 ⇒ 新建终端 ⇒ 编辑器不出现」）。
    ///
    /// ⚠️ **判据**：那一格**装着终端页**（终端是主角 ⇒ 那一格最宽）⇒
    ///   终端那一格收拢了 ⇒ 退到**当前格子**。
    ///
    /// ⚠️ **为什么记在树里而不是记在面板上**：面板只知道自己记着的那一个
    ///   容器 ⇒ 收拢之后它就过期了 ⇒ 得由**知道所有格子**的那一方来回答
    ///   「现在该挂哪一格」。
    home: RefCell<Option<CellId>>,
    /// **新建格子时的回调**（阶段 145）。
    ///
    /// `(新格子编号, 新格子的容器)` ⇒ 上层给新格子装「+」菜单
    /// （会话级的菜单钩子是**闭包**，搬不过去 ⇒ 得按格子重新装）。
    new_cell_hook: RefCell<Option<Rc<dyn Fn(CellId, Rc<crate::panel::subtabs::SubTabs>)>>>,
    /// **根分割条位置**变化的回调（阶段 146：文件面板的宽度记在这儿）。
    ///
    /// ⚠️ **为什么挂在树里**：根分割条（树根处的那个 Paned）**每次重画都会
    ///    重建** ⇒ 谁想盯着它都得重新挂一次 ⇒ 干脆由树来挂。
    root_pos_hook: RefCell<Option<Rc<dyn Fn(i32)>>>,
    /// **「重画完了」的回调**（阶段 150）。
    ///
    /// ⚠️ **为什么要有**：重画会把**所有 Paned 换掉** ⇒ 上层挂在旧 Paned
    ///    上的东西（**宽度上限的监听**）跟着没了 ⇒ 文件面板会失去上限 ⇒
    ///    **变得很宽**（用户 2026-11-22 报的）⇒ 得有个地方重挂。
    ///
    /// ⚠️ **闭包只抓弱引用**（AGENTS.md 引用环红线）：树 → 钩子 → 树 = 环
    ///    ⇒ 树永远释放不掉。
    rebuilt_hook: RefCell<Option<Rc<dyn Fn()>>>,
    /// **布局变了**的回调（阶段 147：存盘）。
    ///
    /// ⚠️ **什么时候叫**：树形变了（拆分 / 关闭格子）或页签搬动了 ⇒
    ///    上层据此把布局写进设置文件（**节流**写，别拖一下分割条就写一次）。
    changed_hook: RefCell<Option<Rc<dyn Fn()>>>,
}

impl CellTree {
    /// 建一棵只有**一个叶子**的树（初始状态）。
    pub fn new(initial: Rc<crate::panel::subtabs::SubTabs>) -> Rc<CellTree> {
        let id = CellId(1);
        let cell = Rc::new(Cell::new(id, initial));
        let tree = Rc::new(CellTree {
            tree: RefCell::new(TreeNode::Leaf(id)),
            cells: RefCell::new(HashMap::from([(id, cell.clone())])),
            host: gtk::Box::new(gtk::Orientation::Vertical, 0),
            next_id: std::cell::Cell::new(2),
            current: std::cell::Cell::new(id),
            home: RefCell::new(None),
            new_cell_hook: RefCell::new(None),
            root_pos_hook: RefCell::new(None),
            rebuilt_hook: RefCell::new(None),
            changed_hook: RefCell::new(None),
        });
        tree.host.append(cell_widget(&cell));
        tree.wire_cell(id);
        tree
    }

    /// 根容器（挂到连接页上）。
    pub fn root(&self) -> &gtk::Widget {
        self.host.upcast_ref()
    }

    /// 某个格子（按编号）。
    pub fn cell(&self, id: CellId) -> Option<Rc<Cell>> {
        self.cells.borrow().get(&id).cloned()
    }

    /// 加一个格子（**只加内容，不改树形**）⇒ 返回它的编号。
    ///
    /// 阶段 145 用它建新格子（先加内容，再把它挂进树里）。
    pub fn add_cell(&self, tabs: Rc<crate::panel::subtabs::SubTabs>) -> CellId {
        let id = self.alloc_id();
        let cell = Rc::new(Cell::new(id, tabs));
        self.cells.borrow_mut().insert(id, cell);
        id
    }

    /// 直接设树（**阶段 145 / 冒烟用**）⇒ 顺手重画。
    pub fn set_tree(&self, tree: TreeNode) {
        *self.tree.borrow_mut() = tree;
        self.rebuild();
    }

    /// **重画**：把树画成 widget（拆分了/关闭了之后调）。
    ///
    /// ⚠️ **只换根容器里的 child** ⇒ 外面的连接页 widget 不用换
    /// ⇒ 页面不会闪。
    ///
    /// ⚠️ **阶段 150：分割条的位置必须保住**——重画会把**所有 Paned 重新
    ///    造一遍** ⇒ 位置会掉回 GTK 默认值（**第一个子控件的自然宽度**）
    ///    ⇒ 用户拖出来的宽度就没了（用户 2026-11-22 报：「拖动并放置之后，
    ///    文件管理器的宽度就会变化，变得很宽」）。
    pub fn rebuild(&self) {
        // ⚠️ **先把旧位置记下来**（Paned 马上要被换掉 ⇒ 不记就丢了）。
        let mut pos: SplitPos = HashMap::new();
        if let Some(old) = self.host.first_child() {
            let tree = self.tree.borrow().clone();
            collect_positions(&old, &tree, &mut pos);
        }
        // ⚠️ **先把旧树拆下来**：Paned 的孩子得**先摘**，
        // 否则同一个 widget 不能同时挂两个父 ⇒ GTK 报 "already has a parent"。
        if let Some(old) = self.host.first_child() {
            detach(&old);
            self.host.remove(&old);
        }
        let widget = {
            let tree = self.tree.borrow();
            let cells = self.cells.borrow();
            widget_for(&tree, &cells, &pos)
        };
        if let Some(w) = widget {
            self.host.append(&w);
        }
        // ⚠️ **再恢复一次**：GTK 分配时会把 position **夹到合法范围**
        //    （两个子控件的最小宽之和）⇒ 刚建好时设的值可能被改 ⇒ 这里
        //    按 key 再设一次（这时子控件的最小宽已知了 ⇒ 不会再被改）。
        if let Some(w) = self.host.first_child() {
            let tree = self.tree.borrow();
            restore_positions(&w, &tree, &pos);
        }
        // 根分割条**换了新的** ⇒ 重新挂一次位置监听。
        self.wire_root_paned();
        // ⚠️ **阶段 150：告诉上层「重画完了」**——根分割条的位置要按
        //    **记忆的宽度**复位，而且「宽度上限」的监听也得重挂（旧的
        //    Paned 没了 ⇒ 监听跟着没了 ⇒ 文件面板会失去上限 ⇒ 变得很宽）。
        let hook = self.rebuilt_hook.borrow().clone();
        if let Some(f) = hook {
            f();
        }
    }

    /// **根分割条**（树根处的那个 Paned）—— 只有一个格子时没有。
    pub fn root_paned(&self) -> Option<gtk::Paned> {
        self.host
            .first_child()
            .and_then(|w| w.downcast::<gtk::Paned>().ok())
    }

    /// 把「根分割条位置变了」的回调挂到当前的根分割条上。
    fn wire_root_paned(&self) {
        let Some(p) = self.root_paned() else { return };
        let Some(f) = self.root_pos_hook.borrow().clone() else {
            return;
        };
        let f2 = f.clone();
        p.connect_notify_local(Some("position"), move |p, _| f2(p.position()));
    }

    /// 注册「根分割条位置变了」的回调（阶段 146：文件面板的宽度记在这儿）。
    pub fn set_root_pos_hook<F: Fn(i32) + 'static>(&self, f: F) {
        *self.root_pos_hook.borrow_mut() = Some(Rc::new(f));
        self.wire_root_paned();
    }

    /// 注册「**重画完了**」的回调（阶段 150）。
    ///
    /// ⚠️ **干什么用**：重画会把所有 Paned 换掉 ⇒ 上层得在这里把
    ///    **根分割条按记忆的宽度复位**、并**重挂宽度上限的监听**。
    ///
    /// ⚠️ **闭包只抓弱引用**（AGENTS.md 引用环红线）。
    pub fn set_rebuilt_hook<F: Fn() + 'static>(&self, f: F) {
        *self.rebuilt_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// **立刻跑一次「重画完了」的回调**（阶段 150）。
    ///
    /// ⚠️ **为什么需要**：刚建好树的时候还没装钩子 ⇒ 初次也得按**记忆的
    ///    宽度**复位一次（否则文件面板一开始就是 GTK 默认宽度 ⇒ 很宽）。
    pub fn apply_rebuilt(&self) {
        let hook = self.rebuilt_hook.borrow().clone();
        if let Some(f) = hook {
            f();
        }
    }

    /// 当前的树（**快照**，纯数据）。
    pub fn tree(&self) -> TreeNode {
        self.tree.borrow().clone()
    }

    /// 格子数。
    pub fn cell_count(&self) -> usize {
        leaf_count(&self.tree.borrow())
    }

    /// 所有格子的编号（按顺序）。
    pub fn ids(&self) -> Vec<CellId> {
        flatten(&self.tree.borrow())
    }

    /// 下一个可用编号（**不占用**；真要用的时候再取）。
    pub fn next_id(&self) -> CellId {
        CellId(self.next_id.get())
    }

    /// 占用下一个编号（建新格子时调）。
    pub fn alloc_id(&self) -> CellId {
        let id = CellId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        id
    }

    /* ---------------- 阶段 145：跨格子 ---------------- */

    /// 给一个格子**接上钩子**（跨格子搬页签 ＋ 格子空了）。
    ///
    /// ⚠️ **闭包只抓弱引用**（AGENTS.md 的引用环红线）：
    ///    `CellTree` → 格子 → `SubTabs` → 钩子 → `CellTree` ⇒ 抓强引用就是环
    ///    ⇒ 格子永远释放不掉。
    fn wire_cell(self: &Rc<Self>, id: CellId) {
        let Some(cell) = self.cell(id) else { return };
        let tabs = cell.tabs.clone();
        // 跨格子搬页签
        {
            let weak = Rc::downgrade(self);
            tabs.set_move_hook(move |page_id, action| {
                let Some(tree) = weak.upgrade() else {
                    return false;
                };
                match action {
                    crate::layout::dnd::PageMove::Into(idx) => {
                        tree.move_page_into(page_id, id, idx)
                    }
                    crate::layout::dnd::PageMove::Split(zone) => {
                        tree.split_cell_with_page(page_id, id, zone).is_some()
                    }
                }
            });
        }
        // 格子空了 ⇒ 把它从树里摘掉
        {
            let weak = Rc::downgrade(self);
            tabs.set_empty_hook(move || {
                let Some(tree) = weak.upgrade() else {
                    return false;
                };
                tree.close_cell(id)
            });
        }
        // ⚠️ **记住"当前格子"**（阶段 145：顶栏的拆分/关闭作用在它身上）。
        {
            // ① 切页签 ⇒ 这个格子是当前的。
            let weak = Rc::downgrade(self);
            let nb = tabs.nb.clone();
            nb.connect_switch_page(move |_nb, _p, _num| {
                if let Some(tree) = weak.upgrade() {
                    tree.current.set(id);
                }
            });
        }
        {
            // ② 点一下这个格子 ⇒ 它是当前的。
            //
            // ⚠️ **手势事件会往祖先冒泡** ⇒ 点终端里也会触发 ⇒ 不用
            //    给每个控件单独挂。
            let weak = Rc::downgrade(self);
            let nb = tabs.nb.clone();
            let click = gtk::GestureClick::new();
            click.connect_pressed(move |_g, _n, _x, _y| {
                if let Some(tree) = weak.upgrade() {
                    tree.current.set(id);
                }
            });
            nb.add_controller(click);
        }
    }

    /// 注册「新建格子时的回调」（阶段 145）。
    pub fn set_new_cell_hook<F: Fn(CellId, Rc<crate::panel::subtabs::SubTabs>) + 'static>(
        &self,
        f: F,
    ) {
        *self.new_cell_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 注册「**布局变了**」的回调（阶段 147：存盘）。
    ///
    /// ⚠️ **上层要节流**：拖分割条、连续搬页签都会触发 ⇒ 别一次就写一次
    ///    设置文件。
    pub fn set_changed_hook<F: Fn() + 'static>(&self, f: F) {
        *self.changed_hook.borrow_mut() = Some(Rc::new(f));
    }

    /// 通知上层「布局变了」。
    fn notify_changed(&self) {
        let f = self.changed_hook.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    /* ---------------- 阶段 147：布局存盘 ---------------- */

    /// 当前的布局（**可存盘**：只记形状 ＋ 页签种类，不记内容**）。
    pub fn snapshot(&self) -> crate::layout::persist::LayoutNode {
        let tree = self.tree.borrow().clone();
        let cells = self.cells.borrow().clone();
        snapshot_of(&tree, &cells)
    }

    /// 按一份布局**重建整棵树**（阶段 147：还原上次会话的布局）。
    ///
    /// `leaf_tabs` = 每个叶子的容器（**按叶子顺序**，长度必须对得上）。
    ///
    /// ⚠️ **对不上 ⇒ `None`**（调用方回退默认布局 ⇒ 绝不白屏）。
    pub fn from_layout(
        layout: &crate::layout::persist::LayoutNode,
        leaf_tabs: Vec<Rc<crate::panel::subtabs::SubTabs>>,
    ) -> Option<Rc<CellTree>> {
        if leaf_tabs.len() != layout.cell_count() {
            return None;
        }
        let mut next = 0usize;
        let mut next_id = 1u64;
        let tree = build_tree(layout, &leaf_tabs, &mut next, &mut next_id)?;
        let mut cells = HashMap::new();
        collect_cells(&tree, &leaf_tabs, &mut cells);
        let tree = Rc::new(CellTree {
            tree: RefCell::new(tree),
            cells: RefCell::new(cells),
            host: gtk::Box::new(gtk::Orientation::Vertical, 0),
            next_id: std::cell::Cell::new(next_id),
            current: std::cell::Cell::new(CellId(1)),
            home: RefCell::new(None),
            new_cell_hook: RefCell::new(None),
            root_pos_hook: RefCell::new(None),
            rebuilt_hook: RefCell::new(None),
            changed_hook: RefCell::new(None),
        });
        // 每个格子都接上钩子（跨格子搬运 ＋ 格子空了）。
        for id in tree.ids() {
            tree.wire_cell(id);
        }
        // ⚠️ **必须把树画出来**：`CellTree::new` 是「建格子 ⇒ 挂上去」，
        //    这里只建了格子 ⇒ 不画的话 `host` 是**空的** ⇒ 连接页**白屏**
        //    （格子都在树上，但一个都看不见）。
        tree.rebuild();
        Some(tree)
    }

    /// **当前格子**（顶栏的拆分/关闭作用在它身上）。
    pub fn current_cell(&self) -> CellId {
        let cur = self.current.get();
        if self.cell(cur).is_some() {
            return cur;
        }
        // ⚠️ **当前格子可能已经被关掉了**（那一格空了 ⇒ 自动收拢）⇒ 这时
        //    回退到**第一个格子** ⇒ 不然顶栏那两个动作会**静默失灵**
        //    （用户点了没反应）。
        self.ids().first().copied().unwrap_or(cur)
    }

    /// 把当前格子设成某个（搬完页签、切完页签后要跟上）。
    pub fn set_current_cell(&self, id: CellId) {
        self.current.set(id);
    }

    /// **「家」格子**：新开的页（编辑器 / 数据库 / 容器）该挂到哪一格。
    ///
    /// ⚠️ **判据**：**装着终端页的那一格**（终端是主角 ⇒ 那一格最宽 ⇒
    ///   编辑器放那儿才看得清）。
    ///
    /// ⚠️ **粘性**：记下的那一格**还活着**就继续用它 ⇒ 这样「开着的编辑器
    ///   那一格」不会因为别的格子变化而换人 ⇒ 关它 / 切它还找得到。
    ///
    /// ⚠️ **那一格没了**（收拢了）⇒ 重新找：先找**当前格子**（用户刚动过
    ///   的那一格），再退到**第一个格子** ⇒ 绝不返回「不在树上」的容器。
    pub fn home_cell(&self) -> CellId {
        // ① 记下的那一格还活着 ⇒ 就用它。
        if let Some(id) = *self.home.borrow() {
            if self.cell(id).is_some() {
                return id;
            }
        }
        // ② 重新找：**当前格子**优先（用户刚动过的那一格）。
        let cur = self.current_cell();
        // ⚠️ **先看当前格子自己有没有终端页**（有就用它 ⇒ 别被别的格子的
        //   终端页抢走 ⇒ 用户在哪个格子工作，编辑器就跟在那个格子后面）。
        let cur_has_term = self
            .cell(cur)
            .map(|c| {
                !c.tabs
                    .page_ids_of_kind(crate::panel::subtabs::PageKind::Terminal)
                    .is_empty()
            })
            .unwrap_or(false);
        // ③ 哪些格子装着终端页（按树里的顺序）。
        let term_cells: Vec<CellId> = self
            .ids()
            .into_iter()
            .filter(|id| {
                self.cell(*id)
                    .map(|c| {
                        !c.tabs
                            .page_ids_of_kind(crate::panel::subtabs::PageKind::Terminal)
                            .is_empty()
                    })
                    .unwrap_or(false)
            })
            .collect();
        let pick = pick_home_cell(cur, cur_has_term, &term_cells);
        *self.home.borrow_mut() = Some(pick);
        pick
    }

    /// **「家」格子的容器**（`None` = 一个格子都没有 ⇒ 树刚拆完）。
    pub fn home_tabs(&self) -> Option<Rc<crate::panel::subtabs::SubTabs>> {
        self.cell(self.home_cell()).map(|c| c.tabs.clone())
    }
    /// **拆一个格子** ⇒ 返回新格子的编号。
    ///
    /// `zone` 决定新格子在**哪一边**（左 / 右 / 上 / 下）。
    ///
    /// ⚠️ **新格子是空的**（只有一页签栏）⇒ 拖过来的页签再放进去。
    pub fn split_cell(
        self: &Rc<Self>,
        target: CellId,
        zone: crate::layout::dnd::SplitZone,
    ) -> Option<CellId> {
        self.attach_cell(
            crate::panel::subtabs::SubTabs::new(),
            target,
            zone,
        )
    }

    /// **拆一个格子，新格子用给定的容器**（阶段 146b）。
    ///
    /// ⚠️ **什么时候用这个**：容器是**已有的**、只是不在树里了 ⇒ 比如
    ///    用户把「文件」页签关掉之后，那个格子收拢了 ⇒ 容器也没人引用了
    ///    ⇒ 想把它**接回来**时用它（`split_cell` 只能造**空**的）。
    pub fn attach_cell(
        self: &Rc<Self>,
        tabs: Rc<crate::panel::subtabs::SubTabs>,
        target: CellId,
        zone: crate::layout::dnd::SplitZone,
    ) -> Option<CellId> {
        let new_id = self.alloc_id();
        // ⚠️ **关闭钩子继承过来**：新格子里的终端页签被关掉时，
        //    一样要收掉它的 SSH 会话。
        if let Some(src) = self.cell(target) {
            src.tabs.copy_close_hooks_to(&tabs);
        }
        let cell = Rc::new(Cell::new(new_id, tabs.clone()));
        self.cells.borrow_mut().insert(new_id, cell);
        // 改树形。
        let tree = self.tree.borrow().clone();
        let new_tree = if zone.new_first() {
            split_before(&tree, target, new_id, zone.stacked())
        } else {
            split(&tree, target, new_id, zone.stacked())
        };
        let Ok(new_tree) = new_tree else {
            // 拆不了（目标不在树里）⇒ 把刚建的格子撤掉。
            self.cells.borrow_mut().remove(&new_id);
            return None;
        };
        *self.tree.borrow_mut() = new_tree;
        self.rebuild();
        self.wire_cell(new_id);
        self.current.set(new_id);
        // 通知上层：新格子来了（给它装「+」菜单）。
        let hook = self.new_cell_hook.borrow().clone();
        if let Some(f) = hook {
            f(new_id, tabs);
        }
        // ⚠️ **布局变了**（阶段 147：存盘）。
        self.notify_changed();
        Some(new_id)
    }

    /// **哪个格子**装着这个容器（阶段 146b：找文件面板那个格子）。
    pub fn cell_of_container(
        &self,
        tabs: &Rc<crate::panel::subtabs::SubTabs>,
    ) -> Option<CellId> {
        self.ids().into_iter().find(|id| {
            self.cell(*id)
                .map(|c| Rc::ptr_eq(&c.tabs, tabs))
                .unwrap_or(false)
        })
    }

    /// **关掉一个格子**（父节点自动收拢）。
    ///
    /// ⚠️ **最后一个格子不关**：关掉它 ⇒ 连接页就空了 ⇒ 用户连「+」按钮
    ///    都看不到 ⇒ 没法再开终端（死路）⇒ 宁可留一个空格子。
    pub fn close_cell(&self, id: CellId) -> bool {
        if self.cell_count() <= 1 {
            return false;
        }
        let tree = self.tree.borrow().clone();
        let Ok(new_tree) = close(&tree, id) else {
            return false;
        };
        *self.tree.borrow_mut() = new_tree;
        self.cells.borrow_mut().remove(&id);
        self.rebuild();
        // ⚠️ **关掉的正好是「当前格子」⇒ 换一个**（不然顶栏的
        //    「拆分格子 / 关闭本格」会打在一个已经不存在的格子上）。
        if self.current.get() == id {
            if let Some(next) = self.ids().first().copied() {
                self.current.set(next);
            }
        }
        // ⚠️ **布局变了**（阶段 147：存盘）。
        self.notify_changed();
        true
    }

    /// **哪个格子**装着这个页签（阶段 145：跨格子搬运要靠它找源格子）。
    ///
    /// ⚠️ **页签 id 是全局唯一的**（`layout::dnd::alloc_page_id`）⇒
    ///    不会认错格子。
    pub fn cell_of_page(&self, page_id: u64) -> Option<CellId> {
        for id in self.ids() {
            let Some(cell) = self.cell(id) else { continue };
            if cell.tabs.has(page_id) {
                return Some(id);
            }
        }
        None
    }

    /// 把一个页签**搬进**某个格子（插到 `index` 之前；`None` = 追加到最后）。
    pub fn move_page_into(&self, page_id: u64, to: CellId, index: Option<usize>) -> bool {
        let Some(from) = self.cell_of_page(page_id) else {
            return false;
        };
        let Some(src) = self.cell(from) else {
            return false;
        };
        let Some(dst) = self.cell(to) else {
            return false;
        };
        // ⚠️ **先摘再挂**：页内容 widget 不能同时挂进两个 notebook
        //    （GTK 会报 "already has a parent"）。
        let Some(data) = src.tabs.take_page(page_id) else {
            return false;
        };
        dst.tabs.insert_page(data, index);
        // 源格子空了就关掉（**父节点收拢**）。
        if from != to && src.tabs.page_count() == 0 {
            self.close_cell(from);
        }
        // ⚠️ **布局变了**（阶段 147：存盘）——页签换格子 ⇒ 每一格装的
        //    是什么也变了。
        self.notify_changed();
        true
    }

    /// 把一个页签**拆到**某个格子的边上（本格子被劈成两半）。
    ///
    /// 返回**新格子的编号**（页签就落在那儿）。
    ///
    /// ⚠️ **顺序很重要**：**先拆格子，再搬页签** ⇒
    ///    - 拖的是**别的**格子的页签 ⇒ 新格子空着等它，源格子不受影响；
    ///    - 拖的是**本格子自己**的页签 ⇒ 拆完本格子一分为二，页签搬进
    ///      新的一半 ⇒ 原来那一半空了 ⇒ 收拢回去（等于"格子跟着页签走"）。
    pub fn split_cell_with_page(
        self: &Rc<Self>,
        page_id: u64,
        target: CellId,
        zone: crate::layout::dnd::SplitZone,
    ) -> Option<CellId> {
        let new_id = self.split_cell(target, zone)?;
        if self.move_page_into(page_id, new_id, None) {
            Some(new_id)
        } else {
            None
        }
    }
}

/// **纯函数**：在「当前格子」与「装着终端页的格子」之间挑「家」。
///
/// ⚠️ **为什么抽成纯函数**：这一段是**决策**（不碰 GTK）⇒ 能单测 ⇒
///   「终端那一格没了会退到哪儿」这条规则钉得住（阶段 151）。
///
/// - `current` = 当前格子（用户刚动过的那一格）；
/// - `current_has_term` = 当前格子**自己**有没有终端页；
/// - `term_cells` = 所有**装着终端页**的格子（按树里的顺序）。
///
/// ⇒ **当前格子有终端页 ⇒ 就用它**（用户在哪个格子工作，编辑器就跟在
///   那个格子后面）；**没有** ⇒ 退到**第一个装着终端页的格子**；
///   **一个都没有**（终端全关了）⇒ 留在当前格子。
pub fn pick_home_cell(current: CellId, current_has_term: bool, term_cells: &[CellId]) -> CellId {
    if current_has_term {
        return current;
    }
    term_cells.first().copied().unwrap_or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u64) -> TreeNode {
        TreeNode::Leaf(CellId(n))
    }

    /// 造一棵 2×2 的树（1 2 在上，3 4 在下）。
    fn grid2x2() -> TreeNode {
        TreeNode::Split {
            stacked: true,
            a: Box::new(TreeNode::Split {
                stacked: false,
                a: Box::new(leaf(1)),
                b: Box::new(leaf(2)),
            }),
            b: Box::new(TreeNode::Split {
                stacked: false,
                a: Box::new(leaf(3)),
                b: Box::new(leaf(4)),
            }),
        }
    }

    #[test]
    fn 初始树只有一个叶子() {
        let t = leaf(1);
        assert_eq!(flatten(&t), vec![CellId(1)]);
        assert_eq!(leaf_count(&t), 1);
    }

    #[test]
    fn 拆分成两个格子() {
        let t = split(&leaf(1), CellId(1), CellId(2), false).unwrap();
        assert_eq!(flatten(&t), vec![CellId(1), CellId(2)]);
        match &t {
            TreeNode::Split { stacked, a, b } => {
                assert!(!stacked);
                assert_eq!(**a, leaf(1));
                assert_eq!(**b, leaf(2));
            }
            _ => panic!("拆完应该是 Split"),
        }
    }

    #[test]
    fn 竖向拆分是叠放() {
        let t = split(&leaf(1), CellId(1), CellId(2), true).unwrap();
        match &t {
            TreeNode::Split { stacked, .. } => assert!(stacked),
            _ => panic!(),
        }
    }

    #[test]
    fn 拆不存在的格子会报错() {
        assert!(split(&leaf(1), CellId(9), CellId(2), false).is_err());
    }

    #[test]
    fn 拆分不动原树() {
        let t = leaf(1);
        let _ = split(&t, CellId(1), CellId(2), false).unwrap();
        assert_eq!(flatten(&t), vec![CellId(1)]);
    }

    #[test]
    fn 嵌套树里拆中间那个() {
        let t = split(&grid2x2(), CellId(2), CellId(9), false).unwrap();
        assert_eq!(flatten(&t).len(), 5);
        assert!(flatten(&t).contains(&CellId(9)));
    }

    #[test]
    fn 关掉叶子后少一个() {
        let t = close(&grid2x2(), CellId(1)).unwrap();
        assert_eq!(flatten(&t), vec![CellId(2), CellId(3), CellId(4)]);
    }

    /// ⚠️ **「家格子」的决策规则**（阶段 151）。
    ///
    /// 背景：用户 2026-11-25 报「关掉终端 ⇒ 新建终端 ⇒ 编辑器不出现」⇒
    /// 根因是「家」指向了**已经收拢的那一格** ⇒ 规则必须保证「家永远在树上」。
    #[test]
    fn 家格子跟着当前格子走() {
        // 当前格子自己就有终端页 ⇒ 家就是它（用户在哪个格子工作，
        // 编辑器就跟在那个格子后面）。
        assert_eq!(
            pick_home_cell(CellId(7), true, &[CellId(3), CellId(7)]),
            CellId(7),
            "当前格子有终端页 ⇒ 家就该是当前格子"
        );
        // 当前格子**没有**终端页 ⇒ 退到装着终端页的那一格。
        assert_eq!(
            pick_home_cell(CellId(1), false, &[CellId(3), CellId(7)]),
            CellId(3),
            "当前格子没终端 ⇒ 退到第一个装着终端页的格子"
        );
        // 一个终端页都没有（终端全关了）⇒ 留在当前格子。
        assert_eq!(
            pick_home_cell(CellId(1), false, &[]),
            CellId(1),
            "没有终端页 ⇒ 留在当前格子（总比指向一个死格子强）"
        );
    }

    #[test]
    fn 关掉一个子节点后父节点收拢() {
        // 关掉 1 之后，上排只剩 2 ⇒ 上排的 Split 收拢成单叶
        let t = close(&grid2x2(), CellId(1)).unwrap();
        match &t {
            TreeNode::Split { a, .. } => {
                assert_eq!(**a, leaf(2), "上排应收拢成单个叶子 2");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn 关掉最后一个格子整棵树空了() {
        let t = close(&leaf(1), CellId(1)).unwrap();
        assert_eq!(t, TreeNode::Empty);
        assert!(flatten(&t).is_empty());
    }

    #[test]
    fn 关不存在的格子会报错() {
        assert!(close(&grid2x2(), CellId(9)).is_err());
    }

    #[test]
    fn 关格子不动原树() {
        let t = grid2x2();
        let _ = close(&t, CellId(1)).unwrap();
        assert_eq!(flatten(&t).len(), 4);
    }

    #[test]
    fn 关一个不影响兄弟() {
        let t = close(&grid2x2(), CellId(3)).unwrap();
        let ids = flatten(&t);
        assert!(ids.contains(&CellId(4)));
        assert!(!ids.contains(&CellId(3)));
    }

    #[test]
    fn 顺序是左到右上到下() {
        assert_eq!(
            flatten(&grid2x2()),
            vec![CellId(1), CellId(2), CellId(3), CellId(4)]
        );
    }

    #[test]
    fn 找叶子找得到() {
        assert!(leaf_of(&grid2x2(), CellId(3)).is_some());
        assert!(leaf_of(&grid2x2(), CellId(9)).is_none());
    }

    #[test]
    fn 可变版也能找() {
        let mut t = grid2x2();
        let l = leaf_of_mut(&mut t, CellId(4)).unwrap();
        *l = leaf(44);
        assert_eq!(flatten(&t), vec![CellId(1), CellId(2), CellId(3), CellId(44)]);
    }

    #[test]
    fn 拆完再关掉新格子能还原() {
        let t2 = split(&leaf(1), CellId(1), CellId(2), false).unwrap();
        let t3 = close(&t2, CellId(2)).unwrap();
        assert_eq!(t3, leaf(1));
    }

    #[test]
    fn 关掉再拆还能用() {
        let t2 = close(&grid2x2(), CellId(4)).unwrap();
        let t3 = split(&t2, CellId(3), CellId(9), false).unwrap();
        assert_eq!(flatten(&t3).len(), 4);
    }

    #[test]
    fn 三层嵌套拆最深处() {
        let mut t = grid2x2();
        t = split(&t, CellId(3), CellId(30), true).unwrap();
        t = split(&t, CellId(30), CellId(31), false).unwrap();
        assert_eq!(flatten(&t).len(), 6);
        assert!(leaf_of(&t, CellId(31)).is_some());
    }

    #[test]
    fn 拆出来的顺序接在原格子后面() {
        let t = split(&grid2x2(), CellId(2), CellId(9), false).unwrap();
        let ids = flatten(&t);
        assert_eq!(ids[1], CellId(2));
        assert_eq!(ids[2], CellId(9));
    }

    #[test]
    fn 编号不会重名() {
        let mut seen = Vec::new();
        for id in flatten(&grid2x2()) {
            assert!(!seen.contains(&id));
            seen.push(id);
        }
    }

    /// 回归：目标在**同组第二个**叶子上时也要能拆。
    ///
    /// ⚠️ **曾经的坑**：`split` 递归进 `Split` 时用 `?` 短路 ⇒
    /// 只试了 `a` 分支 ⇒ 每组只有**第一个**叶子能拆。
    #[test]
    fn 同组第二个叶子也能拆() {
        let t = split(&grid2x2(), CellId(2), CellId(9), false).unwrap();
        assert_eq!(flatten(&t), vec![CellId(1), CellId(2), CellId(9), CellId(3), CellId(4)]);
    }

    /// 回归：目标在**更深的 b 分支**里时也要能拆。
    #[test]
    fn 深处的b分支也能拆() {
        let t = split(&grid2x2(), CellId(4), CellId(9), true).unwrap();
        assert_eq!(flatten(&t), vec![CellId(1), CellId(2), CellId(3), CellId(4), CellId(9)]);
    }

    /// `split_before`：新格子在**前面**（拖到左边 / 上边时用）。
    #[test]
    fn 拆分可以让新格子在前() {
        let t = split_before(&leaf(1), CellId(1), CellId(2), false).unwrap();
        assert_eq!(flatten(&t), vec![CellId(2), CellId(1)]);
        match &t {
            TreeNode::Split { stacked, a, b } => {
                assert!(!stacked);
                assert_eq!(**a, leaf(2), "新格子应当在前面");
                assert_eq!(**b, leaf(1), "原格子应当在后面");
            }
            _ => panic!("拆完应该是 Split"),
        }
    }

    #[test]
    fn 拆分在前也认得到深处() {
        // 目标在 b 分支的深处 ⇒ 新格子插在**它前面**，其余顺序不动。
        let t = split_before(&grid2x2(), CellId(3), CellId(9), false).unwrap();
        assert_eq!(
            flatten(&t),
            vec![CellId(1), CellId(2), CellId(9), CellId(3), CellId(4)],
            "新格子应当插在 3 前面，其它格子的顺序不能变"
        );
        // 目标在 a 分支里也一样
        let t = split_before(&grid2x2(), CellId(1), CellId(9), false).unwrap();
        assert_eq!(
            flatten(&t),
            vec![CellId(9), CellId(1), CellId(2), CellId(3), CellId(4)]
        );
    }

    #[test]
    fn 拆分在前拆不到就报错() {
        assert!(split_before(&leaf(1), CellId(9), CellId(2), false).is_err());
        assert!(split_before(&TreeNode::Empty, CellId(1), CellId(2), false).is_err());
    }
}
