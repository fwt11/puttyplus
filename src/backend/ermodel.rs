//! ER 图（Database Model，阶段 19）——**纯数据 + 纯布局**。
//!
//! **为什么单独一个模块**：ER 图的两件事——「图里有哪些表、哪些边」和
//! 「这些表摆在哪」——都能在不碰数据库、不碰 GTK 的前提下算清楚。
//! 那就写成纯函数 + 单测：界面上出问题时，能一眼分清是**数据错了**
//! 还是**画错了**。
//!
//! **图的样子**：被引用的表（父表）画在左边，引用它的表（子表）画在右边，
//! 外键画成一条带箭头的线，箭头指向父表——跟 Navicat 的 Model 视图一致。

use super::dbmeta::ColumnDef;

/// 表框宽度（像素）。
pub const TABLE_W: i32 = 240;
/// 表头（表名那一行）的高度。
pub const HEADER_H: i32 = 30;
/// 每列一行的高度。
pub const ROW_H: i32 = 20;
/// 表框上下留白。
pub const PAD_V: i32 = 8;
/// 画布四周留白。
pub const MARGIN: i32 = 24;
/// 层与层之间的水平间距。
pub const H_GAP: i32 = 96;
/// 同层里表与表之间的垂直间距。
pub const V_GAP: i32 = 28;

/// ER 图里的一列。
#[derive(Debug, Clone, PartialEq)]
pub struct ErColumn {
    pub name: String,
    pub data_type: String,
    /// 主键。
    pub is_pk: bool,
    /// 这一列是外键（指向别的表）。
    pub is_fk: bool,
}

/// ER 图里的一张表。
#[derive(Debug, Clone, PartialEq)]
pub struct ErTable {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ErColumn>,
    /// 布局坐标（由 [`layout`] 填）。
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 所在层：0 = 焦点表，负数 = 被它引用的父表，正数 = 引用它的子表。
    pub level: i32,
}

impl ErTable {
    /// 表框高度（表头 + 每列一行 + 上下留白）。
    pub fn height(&self) -> i32 {
        HEADER_H + self.columns.len() as i32 * ROW_H + PAD_V
    }
}

/// ER 图里的一条边（子表 → 父表，即外键指向被引用的表）。
#[derive(Debug, Clone, PartialEq)]
pub struct ErEdge {
    /// 约束名。
    pub name: String,
    /// 持有外键的表（子表）。
    pub from_table: String,
    pub from_col: String,
    /// 被引用的表（父表）。
    pub to_table: String,
    pub to_col: String,
}

/// 一张 ER 图。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ErGraph {
    pub tables: Vec<ErTable>,
    pub edges: Vec<ErEdge>,
}

impl ErGraph {
    /// 画布要多大（宽 × 高）。
    pub fn bounds(&self) -> (i32, i32) {
        let mut w = 0;
        let mut h = 0;
        for t in &self.tables {
            w = w.max(t.x + t.w);
            h = h.max(t.y + t.h);
        }
        (w + MARGIN, h + MARGIN)
    }

    /// 找一张表。
    pub fn table(&self, name: &str) -> Option<&ErTable> {
        self.tables.iter().find(|t| t.name == name)
    }
}

/// 输入用的一条外键。
#[derive(Debug, Clone, PartialEq)]
pub struct ErFk {
    /// 持有外键的表。
    pub table: String,
    /// 约束名。
    pub name: String,
    /// 本表的列。
    pub column: String,
    /// 引用的表。
    pub ref_table: String,
    /// 引用的列。
    pub ref_column: String,
}

/// 焦点表相关的表：焦点表 + 直接跟它有外键关系的表。
///
/// **为什么要单独抽出来**：后端要靠它决定"该去查哪几张表的列"（别把整个
/// 模式的列都查一遍），建图也要靠它决定"图里留谁"——两处必须一致，
/// 所以只写一份。
pub fn related_tables(focus: &str, fks: &[ErFk]) -> Vec<String> {
    let mut keep: Vec<String> = vec![focus.to_string()];
    for fk in fks {
        if fk.table == focus && !keep.contains(&fk.ref_table) {
            keep.push(fk.ref_table.clone());
        }
        if fk.ref_table == focus && !keep.contains(&fk.table) {
            keep.push(fk.table.clone());
        }
    }
    keep
}

/// 建图：挑出要画的表、标出外键列、连边。
///
/// **焦点表（`focus`）给了什么，图里就画什么**：焦点表 + 直接跟它有外键
/// 关系的表（它引用的、引用它的）。这样一张表的关系一眼看得完，
/// 又不会把整个库的表都塞进来。
///
/// `focus` 是 `None` 时画**整个模式**（所有表 + 所有外键）。
pub fn build_graph(
    schema: &str,
    focus: Option<&str>,
    tables: &[(String, Vec<ColumnDef>)],
    fks: &[ErFk],
) -> ErGraph {
    // 1) 谁进图
    let keep: Vec<String> = match focus {
        Some(t) => related_tables(t, fks),
        None => {
            let mut v: Vec<String> = Vec::new();
            for (name, _) in tables {
                if !v.contains(name) {
                    v.push(name.clone());
                }
            }
            v
        }
    };

    // 2) 外键列标记：某个列名出现在「本表侧」的外键里，它就是外键列
    let fk_cols: Vec<(String, String)> = fks
        .iter()
        .filter(|fk| keep.iter().any(|k| k == &fk.table) && keep.iter().any(|k| k == &fk.ref_table))
        .map(|fk| (fk.table.clone(), fk.column.clone()))
        .collect();

    // 3) 表 + 列
    let mut out = ErGraph::default();
    for (name, cols) in tables {
        if !keep.iter().any(|k| k == name) {
            continue;
        }
        let columns = cols
            .iter()
            .map(|c| ErColumn {
                name: c.name.clone(),
                data_type: c.data_type.clone(),
                is_pk: c.is_pk,
                is_fk: fk_cols
                    .iter()
                    .any(|(t, col)| t == name && col == &c.name),
            })
            .collect();
        out.tables.push(ErTable {
            schema: schema.to_string(),
            name: name.clone(),
            columns,
            x: 0,
            y: 0,
            w: TABLE_W,
            h: 0,
            level: 0,
        });
    }

    // 4) 边：两端都在图里才画
    for fk in fks {
        if !keep.iter().any(|k| k == &fk.table) || !keep.iter().any(|k| k == &fk.ref_table) {
            continue;
        }
        out.edges.push(ErEdge {
            name: fk.name.clone(),
            from_table: fk.table.clone(),
            from_col: fk.column.clone(),
            to_table: fk.ref_table.clone(),
            to_col: fk.ref_column.clone(),
        });
    }
    out
}

/// 布局：按外键方向分层，层内竖着排。
///
/// **分层的规则**：父表（被引用的）在子表的左边一层。所以焦点表在第 0 层，
/// 它引用的表在 -1、-2……引用它的表在 +1、+2……同层的表竖着排。
///
/// **互不相关的表怎么办**：一个库里往往有几组**互不相连**的表（没有外键
/// 关系）。它们不能都堆在第 0 层——那会叠在一起。所以先按连通分量分组，
/// 每组自己分层，**组与组之间竖着摞**（各组之间本来就没有连线，摞着不碍事）。
pub fn layout(graph: &mut ErGraph) {
    if graph.tables.is_empty() {
        return;
    }
    // 1) 按连通分量分组（没有外键关系的表各成一组）
    let comps = components(graph);
    // 2) 每组单独分层、摆放
    let mut y_offset = MARGIN;
    for comp in &comps {
        let levels = component_levels(graph, comp);
        // 组内最小的层号归零（整组从 x = MARGIN 开始）
        let min_level = levels.values().copied().min().unwrap_or(0);
        // 按层分组，层内按名字排（同一层每次打开顺序都一样）
        let mut by_level: Vec<i32> = levels.values().copied().collect();
        by_level.sort_unstable();
        by_level.dedup();
        for lv in by_level {
            let mut in_layer: Vec<usize> = comp
                .iter()
                .copied()
                .filter(|i| levels.get(&graph.tables[*i].name) == Some(&lv))
                .collect();
            in_layer.sort_by(|a, b| graph.tables[*a].name.cmp(&graph.tables[*b].name));
            let mut y = y_offset;
            for i in in_layer {
                let t = &mut graph.tables[i];
                t.level = lv - min_level;
                t.x = MARGIN + (lv - min_level) * (TABLE_W + H_GAP);
                t.y = y;
                t.h = t.height();
                y += t.h + V_GAP;
            }
        }
        // 下一组从这一组下面接着摆
        let comp_bottom = comp
            .iter()
            .map(|i| graph.tables[*i].y + graph.tables[*i].h)
            .max()
            .unwrap_or(y_offset);
        y_offset = comp_bottom + V_GAP * 2;
    }
    // 没进任何分量的表（理论上不会有）也给个位置，别留在 (0,0)
    for t in &mut graph.tables {
        if t.h == 0 {
            t.h = t.height();
        }
    }
}

/// 按外键关系把表分成若干组（互不相连的表各成一组）。
///
/// 返回每组里的表下标。
fn components(graph: &ErGraph) -> Vec<Vec<usize>> {
    let n = graph.tables.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut Vec<usize>, mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let index: Vec<(String, usize)> = graph
        .tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    for e in &graph.edges {
        let a = index.iter().find(|(n, _)| n == &e.from_table).map(|(_, i)| *i);
        let b = index.iter().find(|(n, _)| n == &e.to_table).map(|(_, i)| *i);
        if let (Some(a), Some(b)) = (a, b) {
            let ra = find(&mut parent, a);
            let rb = find(&mut parent, b);
            if ra != rb {
                parent[rb] = ra;
            }
        }
    }
    // 按根归组
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        match groups.iter_mut().find(|(root, _)| *root == r) {
            Some((_, v)) => v.push(i),
            None => groups.push((r, vec![i])),
        }
    }
    groups.into_iter().map(|(_, v)| v).collect()
}

/// 一组内部的层号：从"没被谁引用"的表出发做 BFS，父表 -1、子表 +1。
fn component_levels(graph: &ErGraph, comp: &[usize]) -> std::collections::HashMap<String, i32> {
    let in_comp: Vec<String> = comp.iter().map(|i| graph.tables[*i].name.clone()).collect();
    // 根：组里没被任何表引用的那张（没有就取第一张）
    let referenced: Vec<&str> = graph
        .edges
        .iter()
        .filter(|e| in_comp.iter().any(|n| n == &e.from_table))
        .map(|e| e.to_table.as_str())
        .collect();
    let root = in_comp
        .iter()
        .find(|n| !referenced.contains(&n.as_str()))
        .cloned()
        .unwrap_or_else(|| in_comp[0].clone());
    let mut levels: std::collections::HashMap<String, i32> =
        std::collections::HashMap::new();
    levels.insert(root.clone(), 0);
    let mut queue: Vec<String> = vec![root];
    while let Some(cur) = queue.first().cloned() {
        queue.remove(0);
        let cur_level = levels.get(&cur).copied().unwrap_or(0);
        let mut next: Vec<(String, i32)> = Vec::new();
        for e in &graph.edges {
            if !in_comp.iter().any(|n| n == &e.from_table) {
                continue;
            }
            if e.from_table == cur {
                next.push((e.to_table.clone(), cur_level - 1));
            }
            if e.to_table == cur {
                next.push((e.from_table.clone(), cur_level + 1));
            }
        }
        for (name, lv) in next {
            if levels.contains_key(&name) {
                continue;
            }
            levels.insert(name.clone(), lv);
            queue.push(name);
        }
    }
    levels
}

/// 一条边的两个端点（画线用）。
///
/// 返回 `None` 表示这条边有一端的表不在图里（画不了）。
/// 端点落在**列那一行**上（不是表框中心）——这样多列的外键也不会挤在
/// 一起看不出对应关系。
pub fn edge_points(graph: &ErGraph, e: &ErEdge) -> Option<((i32, i32), (i32, i32))> {
    let from = graph.table(&e.from_table)?;
    let to = graph.table(&e.to_table)?;
    let fy = row_y(from, &e.from_col)?;
    let ty = row_y(to, &e.to_col)?;
    // 父表在左边：从子表左边出发，到父表右边收尾（箭头指向父表）
    if to.level <= from.level {
        Some((
            (from.x, from.y + fy),
            (to.x + to.w, to.y + ty),
        ))
    } else {
        Some((
            (from.x + from.w, from.y + fy),
            (to.x, to.y + ty),
        ))
    }
}

/// 某列在表框里的 y 偏移（相对表框左上角）。
fn row_y(t: &ErTable, col: &str) -> Option<i32> {
    let idx = t.columns.iter().position(|c| c.name == col)?;
    Some(HEADER_H + idx as i32 * ROW_H + ROW_H / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, pk: bool) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: "text".to_string(),
            nullable: !pk,
            is_pk: pk,
            default_value: None,
            udt_name: None,
            char_len: None,
            num_precision: None,
            num_scale: None,
            pk_constraint: None,
        }
    }

    fn fk(table: &str, column: &str, ref_table: &str, ref_column: &str) -> ErFk {
        ErFk {
            table: table.to_string(),
            name: format!("{table}_{column}_fkey"),
            column: column.to_string(),
            ref_table: ref_table.to_string(),
            ref_column: ref_column.to_string(),
        }
    }

    fn tables() -> Vec<(String, Vec<ColumnDef>)> {
        vec![
            ("users".to_string(), vec![col("id", true), col("name", false)]),
            ("orders".to_string(), vec![col("id", true), col("user_id", false)]),
            (
                "items".to_string(),
                vec![col("id", true), col("order_id", false)],
            ),
        ]
    }

    #[test]
    fn 焦点表只带直接相关的表() {
        let fks = vec![
            fk("orders", "user_id", "users", "id"),
            fk("items", "order_id", "orders", "id"),
        ];
        // 焦点是 orders：带上 users（它引用的）和 items（引用它的）
        let g = build_graph("public", Some("orders"), &tables(), &fks);
        let mut names: Vec<&str> = g.tables.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["items", "orders", "users"]);
        // items 跟 orders 之间的间接关系不该把 users 之外的东西拉进来
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn 焦点表之外的表不进图() {
        let fks = vec![fk("orders", "user_id", "users", "id")];
        // 焦点是 users：只带 orders（引用它的），items 跟 users 没关系
        let g = build_graph("public", Some("users"), &tables(), &fks);
        let mut names: Vec<&str> = g.tables.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["orders", "users"]);
        // edges 也只留两端都在图里的
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from_table, "orders");
        assert_eq!(g.edges[0].to_table, "users");
    }

    #[test]
    fn 外键列要标出来() {
        let fks = vec![fk("orders", "user_id", "users", "id")];
        let g = build_graph("public", Some("orders"), &tables(), &fks);
        let orders = g.table("orders").unwrap();
        let uid = orders.columns.iter().find(|c| c.name == "user_id").unwrap();
        assert!(uid.is_fk, "user_id 是外键列");
        let oid = orders.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(!oid.is_fk);
        assert!(oid.is_pk);
    }

    #[test]
    fn 整个模式时所有表都进图() {
        let fks = vec![
            fk("orders", "user_id", "users", "id"),
            fk("items", "order_id", "orders", "id"),
        ];
        let g = build_graph("public", None, &tables(), &fks);
        assert_eq!(g.tables.len(), 3);
        assert_eq!(g.edges.len(), 2);
    }

    #[test]
    fn 布局把父表放在左边() {
        let fks = vec![fk("orders", "user_id", "users", "id")];
        let mut g = build_graph("public", Some("orders"), &tables(), &fks);
        layout(&mut g);
        let orders = g.table("orders").unwrap();
        let users = g.table("users").unwrap();
        // 层号按**连通分量**归一（每组都从最左一层开始）：
        // 父表 users 在第 0 层（最左），子表 orders 在第 1 层
        assert_eq!(users.level, 0);
        assert_eq!(orders.level, 1);
        assert!(
            users.x < orders.x,
            "父表（users）应该在子表（orders）左边：{} vs {}",
            users.x,
            orders.x
        );
    }

    #[test]
    fn 布局的表不重叠() {
        let fks = vec![
            fk("orders", "user_id", "users", "id"),
            fk("items", "order_id", "orders", "id"),
        ];
        let mut g = build_graph("public", Some("orders"), &tables(), &fks);
        layout(&mut g);
        // 同层的表（这里 items 与 users 都在 orders 的两侧，不同层）不重叠
        for a in &g.tables {
            for b in &g.tables {
                if a.name == b.name {
                    continue;
                }
                let overlap_x = a.x < b.x + b.w && b.x < a.x + a.w;
                let overlap_y = a.y < b.y + b.h && b.y < a.y + a.h;
                assert!(
                    !(overlap_x && overlap_y),
                    "表 {} 与 {} 叠在一起了",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn 边的端点落在对应列上() {
        let fks = vec![fk("orders", "user_id", "users", "id")];
        let mut g = build_graph("public", Some("orders"), &tables(), &fks);
        layout(&mut g);
        let e = &g.edges[0];
        let ((fx, fy), (tx, ty)) = edge_points(&g, e).unwrap();
        let orders = g.table("orders").unwrap();
        let users = g.table("users").unwrap();
        // 子表那一端在左边（父表在左边时）
        assert_eq!(fx, orders.x);
        // 父表那一端在右边
        assert_eq!(tx, users.x + users.w);
        // 端点落在两列各自的行上，不是表框中心
        assert_ne!(fy, ty);
        assert!(fy > orders.y && fy < orders.y + orders.h);
        assert!(ty > users.y && ty < users.y + users.h);
    }

    #[test]
    fn 相关的表和建图留的表一致() {
        // 后端要靠 related_tables 决定查哪几张表的列——
        // 它和 build_graph 留的表必须一致，否则会查出用不上的列
        let fks = vec![
            fk("orders", "user_id", "users", "id"),
            fk("items", "order_id", "orders", "id"),
        ];
        let related = related_tables("orders", &fks);
        let g = build_graph("public", Some("orders"), &tables(), &fks);
        let mut a = related.clone();
        let mut b: Vec<String> = g.tables.iter().map(|t| t.name.clone()).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn 互不相关的表不叠在一起() {
        // users / orders 是一组，audit 跟它们没有任何外键关系——另一组。
        // 两组不能都堆在第 0 层（那会叠在一起）。
        let mut tables = tables();
        tables.push(("audit".to_string(), vec![col("id", true)]));
        let fks = vec![fk("orders", "user_id", "users", "id")];
        let mut g = build_graph("public", None, &tables, &fks);
        layout(&mut g);
        for a in &g.tables {
            for b in &g.tables {
                if a.name == b.name {
                    continue;
                }
                let overlap_x = a.x < b.x + b.w && b.x < a.x + a.w;
                let overlap_y = a.y < b.y + b.h && b.y < a.y + a.h;
                assert!(
                    !(overlap_x && overlap_y),
                    "表 {} 与 {} 叠在一起了（{:?} / {:?}）",
                    a.name,
                    b.name,
                    (a.x, a.y),
                    (b.x, b.y)
                );
            }
        }
    }

    #[test]
    fn 空的图不会崩() {
        let mut g = build_graph("public", Some("nope"), &tables(), &[]);
        layout(&mut g);
        assert!(g.tables.is_empty());
        assert_eq!(g.bounds(), (MARGIN, MARGIN));
    }
}
