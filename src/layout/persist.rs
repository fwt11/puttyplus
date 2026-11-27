//! **格子布局的存盘格式**（阶段 147）。
//!
//! ## 存的是什么
//!
//! 只存**形状**与**每一格原来是什么**，**不存内容**：
//!
//! ```json
//! {"type":"split","stacked":false,
//!  "a":{"type":"leaf","tabs":["files"]},
//!  "b":{"type":"leaf","tabs":["terminal"]}}
//! ```
//!
//! ⚠️ **为什么不存内容**：终端会话、数据库连接、打开的文件**都不是能存盘
//!    的东西**（会话要重连、文件要重开）⇒ 存了也还原不了 ⇒ **不假装**。
//! ⇒ 还原时按 [`SlotKind`] 还原成对应的**空壳 / 提示**。
//!
//! ⚠️ **为什么自己写 JSON**（项目里没有 serde）：这个格式是**我们自己写、
//!    自己读**的固定形状 ⇒ 用一个**只认这个形状**的极小解析器就够
//!    （几百行 vs 引入一个依赖 ＋ 序列化框架）。
//!
//! ⚠️ **解析失败 ⇒ 返回 `None`**（调用方回退默认布局）⇒ **绝不白屏**。

use std::fmt::Write as _;

/// 一格里那些页签的**种类**（存盘只记种类，不记内容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// 终端页签。
    Terminal,
    /// 文件面板（会话级的那一个）。
    Files,
    /// 数据库工作台。
    Db,
    /// 容器页。
    Docker,
    /// 编辑器。
    Editor,
    /// 不认识（**降级**用它，别丢数据）。
    Other,
}

impl SlotKind {
    /// 存盘时的写法。
    pub fn as_str(&self) -> &'static str {
        match self {
            SlotKind::Terminal => "terminal",
            SlotKind::Files => "files",
            SlotKind::Db => "db",
            SlotKind::Docker => "docker",
            SlotKind::Editor => "editor",
            SlotKind::Other => "other",
        }
    }

    /// 从存盘的写法认回来（**认不出 ⇒ `Other`**，不报错）。
    pub fn from_str(s: &str) -> SlotKind {
        match s {
            "terminal" => SlotKind::Terminal,
            "files" => SlotKind::Files,
            "db" => SlotKind::Db,
            "docker" => SlotKind::Docker,
            "editor" => SlotKind::Editor,
            _ => SlotKind::Other,
        }
    }
}

/// 一棵**可存盘**的格子树（纯数据 ⇒ 单测直接测）。
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    /// 两个子树（`a` 在前、`b` 在后）。
    Split {
        /// `true` = 上下叠放。
        stacked: bool,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    /// 一个格子（里面那些页签的种类）。
    Leaf {
        /// 这一格里的页签（按顺序）。
        tabs: Vec<SlotKind>,
    },
}

impl LayoutNode {
    /// 树里的格子数。
    pub fn cell_count(&self) -> usize {
        match self {
            LayoutNode::Split { a, b, .. } => a.cell_count() + b.cell_count(),
            LayoutNode::Leaf { .. } => 1,
        }
    }

    /// 按顺序列出每一格的页签种类（**左到右 / 上到下**）。
    pub fn slots(&self) -> Vec<Vec<SlotKind>> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<Vec<SlotKind>>) {
        match self {
            LayoutNode::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
            LayoutNode::Leaf { tabs } => out.push(tabs.clone()),
        }
    }

    /// 编码成 JSON（**一行**）。
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s);
        s
    }

    fn write_json(&self, out: &mut String) {
        match self {
            LayoutNode::Split { stacked, a, b } => {
                let _ = write!(
                    out,
                    "{{\"type\":\"split\",\"stacked\":{stacked},\"a\":"
                );
                a.write_json(out);
                out.push_str(",\"b\":");
                b.write_json(out);
                out.push('}');
            }
            LayoutNode::Leaf { tabs } => {
                out.push_str("{\"type\":\"leaf\",\"tabs\":[");
                for (i, t) in tabs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "\"{}\"", t.as_str());
                }
                out.push_str("]}");
            }
        }
    }

    /// 解码 JSON（**失败 ⇒ `None`** ⇒ 调用方回退默认布局）。
    pub fn from_json(s: &str) -> Option<LayoutNode> {
        let mut p = P { s: s.as_bytes(), i: 0 };
        let node = p.node()?;
        p.ws();
        if p.i != p.s.len() {
            return None; // 后面还有东西 ⇒ 不是一棵完整的树
        }
        Some(node)
    }
}

/* ---------------- 极小的 JSON 解析器（只认上面的形状） ---------------- */

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == c {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    /// 读一个字符串（**只处理 `\"` `\\` `\n` 这几个转义**——我们自己写出来的
    /// 字符串里只有字母和下划线，剩下的当"不认识"处理）。
    fn string(&mut self) -> Option<String> {
        self.ws();
        if !self.eat(b'"') {
            return None;
        }
        let mut out = String::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        other => out.push(other as char),
                    }
                }
                other => out.push(other as char),
            }
        }
        None
    }

    fn bool(&mut self) -> Option<bool> {
        self.ws();
        if self.s[self.i..].starts_with(b"true") {
            self.i += 4;
            Some(true)
        } else if self.s[self.i..].starts_with(b"false") {
            self.i += 5;
            Some(false)
        } else {
            None
        }
    }

    /// 读一个节点（`{` 开头的对象）。
    fn node(&mut self) -> Option<LayoutNode> {
        if !self.eat(b'{') {
            return None;
        }
        let mut kind: Option<String> = None;
        let mut stacked = false;
        let mut a: Option<LayoutNode> = None;
        let mut b: Option<LayoutNode> = None;
        let mut tabs: Option<Vec<SlotKind>> = None;
        loop {
            let key = self.string()?;
            if !self.eat(b':') {
                return None;
            }
            match key.as_str() {
                "type" => kind = Some(self.string()?),
                "stacked" => stacked = self.bool()?,
                "a" => a = Some(self.node()?),
                "b" => b = Some(self.node()?),
                "tabs" => {
                    if !self.eat(b'[') {
                        return None;
                    }
                    let mut list = Vec::new();
                    if !self.eat(b']') {
                        loop {
                            let s = self.string()?;
                            list.push(SlotKind::from_str(&s));
                            if self.eat(b',') {
                                continue;
                            }
                            if self.eat(b']') {
                                break;
                            }
                            return None;
                        }
                    }
                    tabs = Some(list);
                }
                _ => return None, // 不认识的字段 ⇒ 这不是我们的格式
            }
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                break;
            }
            return None;
        }
        match kind.as_deref() {
            Some("split") => Some(LayoutNode::Split {
                stacked,
                a: Box::new(a?),
                b: Box::new(b?),
            }),
            Some("leaf") => Some(LayoutNode::Leaf { tabs: tabs? }),
            _ => None,
        }
    }
}

/* ---------------- 还原：每一格扮演什么角色 ---------------- */

/// 一格在**还原**时扮演什么角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafRole {
    /// 放**活着的终端**（主容器 `sub`）。
    Terminal,
    /// 放**活着的文件面板**（文件容器）。
    Files,
    /// 放**提示页**（这一格的内容还原不出来）。
    Placeholder,
}

/// 角色分配的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roles {
    /// 每一格的角色（**按叶子顺序**：左到右 / 上到下）。
    pub roles: Vec<LeafRole>,
    /// **文件面板要不要并进终端那一格**。
    ///
    /// ⚠️ **为什么会有这种情形**：用户把「文件」那个页签**拖进了**终端格
    ///    ⇒ 那一格本来就有终端**和**文件面板 ⇒ 还原时它们得在**同一格**。
    pub merge_files_into_terminal: bool,
}

/// 布局里**有没有**这一类格子。
pub fn has_kind(layout: &LayoutNode, kind: SlotKind) -> bool {
    layout.slots().iter().any(|k| k.contains(&kind))
}

/// 布局里**缺哪边就补一个叶子**（新叶子排在最后）。
///
/// ⚠️ **为什么必须补**：终端与文件面板是**活的**（连接一建立就在那儿）⇒
///    它们得有格子放 ⇒ 布局里没有对应格子时就得补一个。
///    ⇒ **宁可多一格，也别把文件面板弄丢**。
pub fn ensure_kinds(layout: &LayoutNode, kinds: &[SlotKind]) -> LayoutNode {
    let mut out = layout.clone();
    for k in kinds {
        if !has_kind(&out, *k) {
            out = LayoutNode::Split {
                stacked: false,
                a: Box::new(out),
                b: Box::new(LayoutNode::Leaf { tabs: vec![*k] }),
            };
        }
    }
    out
}

/// 把布局里的「文件」叶子**摘掉**（阶段 178）。
///
/// **为什么需要**：文件面板搬去**主窗口左栏的侧边栏**了 ⇒ 格子树里
/// 不再有「文件」那一格 ⇒ 但**老用户存下来的布局里还有它**
/// （阶段 146 写进去的）⇒ 还原时那一格**没有内容可放** ⇒ 必须摘掉。
///
/// ⚠️ **老布局的兼容**：`SlotKind::Files` 的枚举与 `from_str` **保留**
///   （老文件里存着 `"files"`）⇒ 只是**不再生成、也不再还原**。
///
/// - 叶子里的 `Files` **去掉**；
/// - 去掉之后**空了的叶子** ⇒ 整格作废；
/// - 一边空了 ⇒ **塌陷**成另一边（少一格 ⇒ 不留空格子）。
///
/// 全部摘完 ⇒ 返回 `None`（调用方退回默认布局）。
pub fn strip_files(layout: &LayoutNode) -> Option<LayoutNode> {
    match layout {
        LayoutNode::Leaf { tabs } => {
            // ⚠️ **只有「本来含 Files」的那种叶子**才作废。
            //    ⚠️ **别把「本来就是空的叶子」也删了**！空叶子是
            //    「拆出来但没放东西的那一格」⇒ 还原时它要变成**提示页**
            //    （用户 2026-11-22 的体验：那一格提示一下，而不是白屏）。
            let had_files = tabs.contains(&SlotKind::Files);
            let kept: Vec<SlotKind> = tabs
                .iter()
                .copied()
                .filter(|k| *k != SlotKind::Files)
                .collect();
            if kept.is_empty() && had_files {
                None
            } else {
                Some(LayoutNode::Leaf { tabs: kept })
            }
        }
        LayoutNode::Split { stacked, a, b } => {
            let a2 = strip_files(a);
            let b2 = strip_files(b);
            match (a2, b2) {
                (Some(x), Some(y)) => Some(LayoutNode::Split {
                    stacked: *stacked,
                    a: Box::new(x),
                    b: Box::new(y),
                }),
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (None, None) => None,
            }
        }
    }
}

/// 按布局给每一格**分配角色**。
///
/// ⚠️ **一个格子只能有一个容器** ⇒ 终端与文件面板**挤在同一格**时，
///    **终端优先**（它带着 SSH 会话与编辑区宿主）⇒ 文件面板并进去。
pub fn assign_roles(layout: &LayoutNode) -> Roles {
    let slots = layout.slots();
    let term_idx = slots.iter().position(|k| k.contains(&SlotKind::Terminal));
    let file_idx = slots.iter().position(|k| k.contains(&SlotKind::Files));
    // ⚠️ **同一个格子**（两边都在这一格）⇒ 要并。
    let merge = term_idx.is_some() && term_idx == file_idx;
    let mut roles = Vec::with_capacity(slots.len());
    for i in 0..slots.len() {
        let role = if Some(i) == term_idx {
            LeafRole::Terminal
        } else if Some(i) == file_idx && !merge {
            LeafRole::Files
        } else {
            LeafRole::Placeholder
        };
        roles.push(role);
    }
    Roles {
        roles,
        merge_files_into_terminal: merge,
    }
}

/* ---------------- 存 / 取（按会话） ---------------- */

/// 布局存盘的**分组名**（`settings` 的 group）。
pub const GROUP: &str = "ui/layout";

/// 某个会话的布局（**JSON**）。
pub fn load(session_key: &str) -> Option<String> {
    crate::settings::get_str_opt(GROUP, &key_of(session_key))
}

/// 存某个会话的布局（**JSON**）。
pub fn save(session_key: &str, json: &str) {
    crate::settings::set_str(GROUP, &key_of(session_key), json);
}

/// 布局在设置文件里的**键名**。
///
/// ⚠️ **为什么加前缀**：设置文件的键名不能是裸的会话键（会话键里可能有
///    `:`）⇒ 加个固定前缀，一眼看出这是布局。
///
/// ⚠️ **为什么还要清洗**：会话名是**用户起的**，什么字符都可能有——
///
/// 而 glib 的 KeyFile **只在 `set_string` 时静默拒绝非法键** ⇒ 不清洗
///    就是「存了但没存进去」（比报错更难查）⇒ 走 [`crate::settings::safe_key`]
///    （清洗 ＋ 哈希后缀 ⇒ 不同会话各占一行）。
fn key_of(session_key: &str) -> String {
    format!("tree_{}", crate::settings::safe_key(session_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(k: &[SlotKind]) -> LayoutNode {
        LayoutNode::Leaf { tabs: k.to_vec() }
    }

    fn sample() -> LayoutNode {
        LayoutNode::Split {
            stacked: false,
            a: Box::new(leaf(&[SlotKind::Files])),
            b: Box::new(LayoutNode::Split {
                stacked: true,
                a: Box::new(leaf(&[SlotKind::Terminal, SlotKind::Db])),
                b: Box::new(leaf(&[SlotKind::Editor])),
            }),
        }
    }

    #[test]
    fn 往返一致() {
        let t = sample();
        let json = t.to_json();
        assert_eq!(LayoutNode::from_json(&json), Some(t.clone()));
        // 再来一遍（序列化 ⇒ 反序列化 ⇒ 序列化 ⇒ 一样）
        let again = LayoutNode::from_json(&json).unwrap();
        assert_eq!(again.to_json(), json);
    }

    #[test]
    fn json形状对() {
        let json = LayoutNode::Leaf {
            tabs: vec![SlotKind::Files],
        }
        .to_json();
        assert_eq!(json, r#"{"type":"leaf","tabs":["files"]}"#);
        let json = LayoutNode::Leaf {
            tabs: vec![SlotKind::Terminal, SlotKind::Db],
        }
        .to_json();
        assert_eq!(
            json,
            r#"{"type":"leaf","tabs":["terminal","db"]}"#
        );
    }

    #[test]
    fn 顺序稳定() {
        let t = sample();
        assert_eq!(t.cell_count(), 3);
        assert_eq!(
            t.slots(),
            vec![
                vec![SlotKind::Files],
                vec![SlotKind::Terminal, SlotKind::Db],
                vec![SlotKind::Editor],
            ]
        );
    }

    #[test]
    fn 不认识的种类降级但不丢() {
        // 手工塞一个没见过的种类
        let json = r#"{"type":"leaf","tabs":["terminal","wat","editor"]}"#;
        let t = LayoutNode::from_json(json).expect("应当能解析");
        match t {
            LayoutNode::Leaf { tabs } => {
                assert_eq!(tabs.len(), 3, "不认识的种类不能丢");
                assert_eq!(tabs[1], SlotKind::Other, "不认识的种类降级成 Other");
            }
            _ => panic!("应当是叶子"),
        }
    }

    #[test]
    fn 坏json回退成空() {
        for bad in [
            "",
            "{",
            "not json",
            r#"{"type":"leaf"}"#,                       // 缺 tabs
            r#"{"type":"split","stacked":false}"#,     // 缺 a/b
            r#"{"type":"unknown"}"#,                   // 不认识的 type
            r#"{"type":"leaf","tabs":["a"],"x":1}"#,   // 不认识的字段
            r#"{"type":"leaf","tabs":["a"}]}"#,        // 少个引号
            r#"{"type":"leaf","tabs":["a"]} trailing"#, // 后面还有东西
        ] {
            assert!(
                LayoutNode::from_json(bad).is_none(),
                "这段应当解析失败：{bad}"
            );
        }
    }

    #[test]
    fn 空格不影响() {
        let a = LayoutNode::Leaf {
            tabs: vec![SlotKind::Files],
        }
        .to_json();
        let b = "  { \"type\" : \"leaf\" , \"tabs\" : [ \"files\" ] }  ";
        assert_eq!(LayoutNode::from_json(b), LayoutNode::from_json(&a));
    }

    #[test]
    fn 键名带前缀而且是安全的() {
        let k = key_of("srv@1.2.3.4:22");
        assert!(k.starts_with("tree_"), "键名要带前缀：{k}");
        // ⚠️ **会话名里可能有 `=`** ⇒ glib 会**静默拒写** ⇒ 必须清洗过。
        let bad = key_of("a=b");
        assert!(!bad.contains('='), "键名里有非法字符（glib 会静默拒写）：{bad}");
        assert_ne!(key_of("a:b"), key_of("a_b"), "清洗后不能撞车");
    }

    /* ---------------- 角色分配 ---------------- */

    #[test]
    fn 默认布局是终端加文件两格() {
        let t = LayoutNode::Split {
            stacked: false,
            a: Box::new(leaf(&[SlotKind::Files])),
            b: Box::new(leaf(&[SlotKind::Terminal])),
        };
        assert!(has_kind(&t, SlotKind::Terminal));
        assert!(has_kind(&t, SlotKind::Files));
        let r = assign_roles(&t);
        assert_eq!(
            r.roles,
            vec![LeafRole::Files, LeafRole::Terminal],
            "文件在左、终端在右"
        );
        assert!(!r.merge_files_into_terminal);
    }

    #[test]
    fn 缺哪边补哪边() {
        // 只有终端 ⇒ 补一个文件叶子（活的文件面板得有地方放）。
        let t = ensure_kinds(&leaf(&[SlotKind::Terminal]), &[SlotKind::Terminal, SlotKind::Files]);
        assert!(has_kind(&t, SlotKind::Files));
        assert_eq!(t.cell_count(), 2);
        // 已经有了 ⇒ 不重复补。
        let t2 = ensure_kinds(&t, &[SlotKind::Terminal, SlotKind::Files]);
        assert_eq!(t2.cell_count(), 2, "不该重复补格子");
    }

    #[test]
    fn 同一格既有终端又有文件面板时要并() {
        let t = leaf(&[SlotKind::Terminal, SlotKind::Files]);
        let r = assign_roles(&t);
        assert!(r.merge_files_into_terminal, "同一格 ⇒ 文件面板并进终端");
        assert_eq!(r.roles, vec![LeafRole::Terminal], "终端优先");
    }

    #[test]
    fn 别的格子给提示页() {
        let t = LayoutNode::Split {
            stacked: false,
            a: Box::new(leaf(&[SlotKind::Files])),
            b: Box::new(leaf(&[SlotKind::Terminal])),
        };
        // 再加一格数据库 ⇒ 它只能给提示页（数据库连接还原不出来）。
        let t = LayoutNode::Split {
            stacked: true,
            a: Box::new(t),
            b: Box::new(leaf(&[SlotKind::Db])),
        };
        let r = assign_roles(&t);
        assert_eq!(
            r.roles,
            vec![
                LeafRole::Files,
                LeafRole::Terminal,
                LeafRole::Placeholder
            ]
        );
        assert!(!r.merge_files_into_terminal);
    }
}
