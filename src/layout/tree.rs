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
fn widget_for(node: &TreeNode, cells: &HashMap<CellId, Rc<Cell>>) -> Option<gtk::Widget> {
    match node {
        TreeNode::Leaf(id) => cells.get(id).map(|c| cell_widget(c).clone().upcast()),
        TreeNode::Split { stacked, a, b } => {
            let wa = widget_for(a, cells)?;
            let wb = widget_for(b, cells)?;
            let paned = gtk::Paned::new(if *stacked {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            });
            paned.set_start_child(Some(&wa));
            paned.set_end_child(Some(&wb));
            Some(paned.upcast())
        }
        TreeNode::Empty => None,
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
        });
        tree.host.append(cell_widget(&cell));
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
    pub fn rebuild(&self) {
        // ⚠️ **先把旧树拆下来**：Paned 的孩子得**先摘**，
        // 否则同一个 widget 不能同时挂两个父 ⇒ GTK 报 "already has a parent"。
        if let Some(old) = self.host.first_child() {
            detach(&old);
            self.host.remove(&old);
        }
        let widget = {
            let tree = self.tree.borrow();
            let cells = self.cells.borrow();
            widget_for(&tree, &cells)
        };
        if let Some(w) = widget {
            self.host.append(&w);
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
}
