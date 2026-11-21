//! 格子（阶段 144）。
//!
//! **一个格子 = 一个 `SubTabs`**：格子自带页签栏 + 页内容栈 ⇒
//! 正好是 xterminal 的"格子"，不用再造第二个标签组件。
//!
//! ⚠️ **格子树的三层正交**（用户 2026-11-21 拍板）：
//! - **格子树**（本模块）：管**面板的自由组合**（横竖自由拆分、嵌套）；
//! - **`SubTabs`**：管**同一格子里的多页签**（终端/数据库/容器/编辑器）；
//! - **`PaneLayout`**：管**一个格子内的终端分屏树**（保持原子，一行不改）。
//!
//! ⇒ 好处：广播/重连/关闭那套遍历逻辑**不用按内容类型过滤**，
//! 「键盘字节发给非终端面板」那个隐患自然消失。

use std::rc::Rc;

use crate::panel::subtabs::SubTabs;

/// 格子编号（**树里用它指代一个格子**）。
///
/// ⚠️ 为什么不是索引：格子会增删 ⇒ 索引会漂 ⇒ 编号稳定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellId(pub u64);

/// 一个格子：**标签栏 + 页内容**（`SubTabs` 原样复用）。
pub struct Cell {
    pub id: CellId,
    pub tabs: Rc<SubTabs>,
}

impl Cell {
    pub fn new(id: CellId, tabs: Rc<SubTabs>) -> Cell {
        Cell { id, tabs }
    }
}

/// 格子树的**叶子**上挂的 widget（= `SubTabs` 的外层容器）。
///
/// ⚠️ **为什么要一个包一层**：以后每个格子头部还要放三个动作键
/// （弹出 / 最大化 / 关闭，阶段 143 的统一协议）⇒ 需要一个"格子壳"
/// 来装 [页签栏 + 头部动作键]。现在先只装页签栏，界面与今天一致。
pub fn cell_widget(cell: &Cell) -> &gtk::Widget {
    cell.tabs.root()
}
