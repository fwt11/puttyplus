//! 数据库元数据与 SQL 生成层（8.1，Rust-only）。
//!
//! **为什么单独一层**：方言差异（标识符怎么引用、字面量怎么写、元数据怎么查）
//! 全收在这里，上层（`backend::db`、`panel::dbui`）只管调用。本模块是**纯
//! 函数**——不碰连接、不开线程——所以 SQL 正确性可以靠单测钉死（真库不一定
//! 随时能连）。
//!
//! **只支持两种方言**：PostgreSQL 与 MySQL。两者都吃 `information_schema`，
//! 于是元数据查询可以写成同一份 SQL（见 `meta_*` 系列），不必各家写一套
//! 私有系统表查询（那种查询最容易被某个小版本升级弄坏）。
//!
//! **不引入参数绑定**：写库语句用**内联字面量**而不是占位符。PG 的 `$1` 与
//! MySQL 的 `?` 两套占位符都要驱动各自绑定，跨方言没法共用一套代码；而且
//! 值都要经过 `edit_literal` 按列类型转义（引号、反斜杠都处理过），拼出来的
//! 文本是安全的。构造器仍然坚持"只拼已转义的片段"，不拼裸用户输入。

use std::collections::HashMap;

use super::db::DbProto;
use crate::i18n::tr;
use crate::trf;

/// 列的逻辑类型：决定用户填的值怎么转成 SQL 字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    /// 整数（PG int2/4/8，MySQL tinyint…bigint）
    Int,
    /// 浮点 / 定点（numeric、decimal、float、double）
    Float,
    /// 布尔
    Bool,
    /// 文本
    Text,
    /// **类型不认识**（PG 未知 OID / MySQL 未映射 / 二进制类型）。
    ///
    /// 按文本处理（保守：加引号），但 UI 上显示「—」而不是"文本"——
    /// **宁可说不知道，也不要说错**：类型错了会让排序按错的规则走。
    Other,
}

impl ColType {
    /// 从 `information_schema.columns.data_type` 的值判断。
    ///
    /// 认不出来的一律当 `Text`——宁可把值当字符串写（服务器可能报类型错误，
    /// 错误信息清楚），也不要瞎猜成数字把用户填错的值静默改掉。
    pub fn from_db_type(t: &str) -> ColType {
        let t = t.trim().to_ascii_lowercase();
        match t.as_str() {
            "smallint" | "int2" | "integer" | "int" | "int4" | "bigint" | "int8"
            | "tinyint" | "mediumint" | "serial" | "bigserial" | "smallserial" => ColType::Int,
            "numeric" | "decimal" | "float4" | "float8" | "real" | "double precision"
            | "double" | "float" | "money" => ColType::Float,
            "boolean" | "bool" => ColType::Bool,
            _ => ColType::Text,
        }
    }

    /// PG 的**类型 OID** → 类型。
    ///
    /// **认不出来返回 `None`**（= 类型未知），**不猜**：猜错会让排序按错的
    /// 规则走、编辑时按错的格式转字面量。宁可显示「—」。
    ///
    /// OID 取自 PG 的 `pg_type`（`oid` 列）——这些值是**写死在协议里的常量**，
    /// 不随版本变。
    pub fn from_pg_oid(oid: u32) -> Option<ColType> {
        Some(match oid {
            16 => ColType::Bool,    // bool
            20 => ColType::Int,     // int8
            21 => ColType::Int,     // int2
            23 => ColType::Int,     // int4
            26 => ColType::Int,     // oid
            700 => ColType::Float,  // float4
            701 => ColType::Float,  // float8
            1700 => ColType::Float, // numeric
            790 => ColType::Float,  // money
            25 => ColType::Text,    // text
            1042 => ColType::Text,  // bpchar（定长字符）
            1043 => ColType::Text,  // varchar
            1082 => ColType::Text,  // date
            1114 => ColType::Text,  // timestamp
            1184 => ColType::Text,  // timestamptz
            114 => ColType::Text,   // json
            3802 => ColType::Text,  // jsonb
            2950 => ColType::Text,  // uuid
            869 => ColType::Text,   // inet
            650 => ColType::Text,   // cidr
            774 => ColType::Text,   // macaddr
            // 二进制 / 位串 / 区间：不是文本，按文本展示会骗人 ⇒ 标"未知"
            17 => ColType::Other,   // bytea
            1560 => ColType::Other, // bit
            1562 => ColType::Other, // varbit
            1186 => ColType::Other, // interval
            _ => return None,
        })
    }

    /// 类型名（UI 提示用）。
    pub fn label(self) -> &'static str {
        match self {
            ColType::Int => tr("整数"),
            ColType::Float => tr("数字"),
            ColType::Bool => tr("布尔"),
            ColType::Text => tr("文本"),
            // 类型不知道就**说不知道**：写"文本"是在骗人（用户会以为这一列
            // 真的就是字符串，改的时候会按字符串规则转义）。
            ColType::Other => "—",
        }
    }
}

/// 标识符引用：PG 用 `"x"`，MySQL 用 `` `x` ``。
///
/// 内部引号按各自方言转义（PG 的 `"` → `""`，MySQL 的 `` ` `` → ` `` `）。
/// 表名/列名来自元数据查询（服务器给的），但仍走这里统一转义——不假设
/// "服务器给的名字一定安全"。
pub fn quote_ident(proto: DbProto, name: &str) -> String {
    match proto {
        DbProto::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
        DbProto::MySql => format!("`{}`", name.replace('`', "``")),
    }
}

/// 字符串字面量：`'...'`。
///
/// PG 只转义 `'`；MySQL 默认把反斜杠当转义符，所以 `\` 也要转（否则
/// `\'` 会被吃掉、或者 `\` 结尾把引号吃掉）。
pub fn quote_str(proto: DbProto, s: &str) -> String {
    match proto {
        DbProto::Postgres => format!("'{}'", s.replace('\'', "''")),
        DbProto::MySql => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
    }
}

/// 限定表名：`"public"."users"` / `` `db`.`users` ``。
///
/// schema 为空时只给表名（PG 里 `search_path` 决定模式；MySQL 里没选库时
/// 也没有 schema 概念）。
pub fn quote_qualified(proto: DbProto, schema: &str, table: &str) -> String {
    if schema.is_empty() {
        quote_ident(proto, table)
    } else {
        format!("{}.{}",
            quote_ident(proto, schema),
            quote_ident(proto, table)
        )
    }
}

/// 用户填的文本 → SQL 字面量。
///
/// 空文本 = **空字符串**（不是 NULL）；想置 NULL 填 `<NULL>`（或 `NULL`）。
/// 这条约定写进 UI 提示，避免"清空单元格"和"置空"分不清。
///
/// 类型不符时返回中文原因，让 UI 在本地就拦下来——比让服务器报错好，
/// 服务器的报错文本各家还不一样。
pub fn edit_literal(proto: DbProto, ty: ColType, text: &str) -> Result<String, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok("''".to_string());
    }
    if t.eq_ignore_ascii_case("null") || t == "<NULL>" {
        return Ok("NULL".to_string());
    }
    match ty {
        ColType::Int => match t.parse::<i64>() {
            Ok(v) => Ok(v.to_string()),
            Err(_) => Err(tr("应为整数").to_string()),
        },
        ColType::Float => match t.parse::<f64>() {
            Ok(v) => {
                if v.is_finite() {
                    Ok(format!("{v}"))
                } else {
                    Err(tr("应为有限数字").to_string())
                }
            }
            Err(_) => Err(tr("应为数字").to_string()),
        },
        ColType::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "y" | "是" | "真" => Ok("TRUE".to_string()),
            "false" | "f" | "0" | "no" | "n" | "否" | "假" => Ok("FALSE".to_string()),
            _ => Err(tr("应为布尔值（true/false）").to_string()),
        },
        ColType::Text | ColType::Other => Ok(quote_str(proto, &t)),
    }
}

/// 按方言把字符串值转成字面量（供写库时统一处理转义）。
pub fn value_literal(proto: DbProto, value: Option<&str>) -> String {
    match value {
        None => "NULL".to_string(),
        Some(v) => quote_str(proto, v),
    }
}

/// 旧值 → WHERE 片段的值（NULL 用 `IS NULL`，不用 `= NULL`）。
pub fn where_value(proto: DbProto, value: Option<&str>) -> String {
    match value {
        None => "IS NULL".to_string(),
        Some(v) => format!("= {}", quote_str(proto, v)),
    }
}

/// 打开一张表的数据：`SELECT * FROM 表 LIMIT n`。
pub fn build_select_table(proto: DbProto, schema: &str, table: &str, limit: usize) -> String {
    format!("SELECT * FROM {} LIMIT {}",
        quote_qualified(proto, schema, table),
        limit.max(1)
    )
}

/* ---------------- 过滤条件（9.5） ---------------- */

/// 条件之间的连接词。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterJoin {
    And,
    Or,
}

impl FilterJoin {
    pub fn label(self) -> &'static str {
        match self {
            FilterJoin::And => tr("并且"),
            FilterJoin::Or => tr("或者"),
        }
    }

    pub fn sql(self) -> &'static str {
        match self {
            FilterJoin::And => "AND",
            FilterJoin::Or => "OR",
        }
    }
}

/// 比较方式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Like,
    NotLike,
    IsNull,
    IsNotNull,
    In,
}

impl FilterOp {
    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Gt => ">",
            FilterOp::Ge => ">=",
            FilterOp::Lt => "<",
            FilterOp::Le => "<=",
            FilterOp::Like => "LIKE",
            FilterOp::NotLike => "NOT LIKE",
            FilterOp::IsNull => "IS NULL",
            FilterOp::IsNotNull => "IS NOT NULL",
            FilterOp::In => "IN",
        }
    }

    /// SQL 关键字（`=` `<>` 之类直接原样用）。
    pub fn sql(self) -> &'static str {
        self.label()
    }

    /// 要不要填值（`IS NULL` 不要）。
    pub fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }

    /// 下拉框里的全部选项（顺序即显示顺序）。
    pub fn all() -> &'static [FilterOp] {
        &[
            FilterOp::Eq,
            FilterOp::Ne,
            FilterOp::Gt,
            FilterOp::Ge,
            FilterOp::Lt,
            FilterOp::Le,
            FilterOp::Like,
            FilterOp::NotLike,
            FilterOp::In,
            FilterOp::IsNull,
            FilterOp::IsNotNull,
        ]
    }

    /// 按显示文字反查（下拉框选完要还原成枚举）。
    pub fn from_label(s: &str) -> Option<FilterOp> {
        FilterOp::all().iter().copied().find(|o| o.label() == s)
    }
}

/// 一个过滤条件。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilterCond {
    /// 与**前一条**的连接词（第一条忽略）。
    pub join: FilterJoin,
    pub column: String,
    pub op: FilterOp,
    /// 原始文本（按列类型转字面量）。
    pub value: String,
}

impl FilterCond {
    pub fn and(column: &str, op: FilterOp, value: &str) -> Self {
        FilterCond {
            join: FilterJoin::And,
            column: column.to_string(),
            op,
            value: value.to_string(),
        }
    }
}

/// 生成 WHERE 子句的内容（**不含** `WHERE` 关键字；没有条件时返回空串）。
///
/// **值的转义跟写库用同一套**（`edit_literal`）：整数列填 `abc` 在本地就报错，
/// 不生成一句会被服务器骂的 SQL；文本按方言转义（PG 只转 `'`，MySQL 连 `\` 一起转）。
///
/// `col_types` 是「列名 → 类型」：拿不到类型的列按文本处理（宁可多加引号，
/// 也不能把值拼成裸的 SQL 片段）。
pub fn build_where(
    proto: DbProto,
    conds: &[FilterCond],
    col_types: &std::collections::HashMap<String, ColType>,
) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for (i, c) in conds.iter().enumerate() {
        if c.column.trim().is_empty() {
            continue;
        }
        let col = quote_ident(proto, c.column.trim());
        let piece = match c.op {
            FilterOp::IsNull => format!("{col} IS NULL"),
            FilterOp::IsNotNull => format!("{col} IS NOT NULL"),
            _ => {
                let ty = col_types.get(c.column.trim()).copied().unwrap_or(ColType::Text);
                let v = c.value.trim();
                if v.is_empty() {
                    return Err(trf!("条件「{v0}」没填值", v0 = c.column));
                }
                match c.op {
                    FilterOp::Like | FilterOp::NotLike => {
                        // 模式串按文本处理（% 和 _ 是通配符，不能当类型校验）
                        format!("{col} {} {}", c.op.sql(), quote_str(proto, v))
                    }
                    FilterOp::In => {
                        let items: Vec<String> = v
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(|s| edit_literal(proto, ty, s))
                            .collect::<Result<Vec<_>, _>>()?;
                        if items.is_empty() {
                            return Err(trf!("条件「{v0}」的 IN 列表是空的", v0 = c.column));
                        }
                        format!("{col} IN ({})", items.join(", "))
                    }
                    _ => {
                        let lit = edit_literal(proto, ty, v)?;
                        format!("{col} {} {lit}", c.op.sql())
                    }
                }
            }
        };
        let piece = if i == 0 {
            piece
        } else {
            format!("{} {piece}", c.join.sql())
        };
        parts.push(piece);
    }
    Ok(parts.join(" "))
}

/// 常用类型清单（**按方言**）——设计器里「类型」下拉的选项。
///
/// **为什么按方言分**：PG 没有 `int`（只有 `integer` / `int4`），MySQL 没有
/// `text` 的默认值那套、`jsonb` 更是没有——一份通用清单会让用户挑到
/// **这个库根本不认的类型**，等改完结构点「应用变更」才被服务器骂。
///
/// 类型名**可以带参数**（`varchar(255)` / `numeric(12,2)`）：设计器里
/// 填的就是裸 SQL 片段，带参数才实用（用户自己敲进去也一样）。
pub fn common_types(proto: DbProto) -> &'static [&'static str] {
    match proto {
        DbProto::Postgres => &[
            "integer",
            "bigint",
            "smallint",
            "text",
            "varchar(255)",
            "boolean",
            "date",
            "timestamp",
            "numeric(12,2)",
            "uuid",
            "jsonb",
            "bytea",
        ],
        DbProto::MySql => &[
            "int",
            "bigint",
            "smallint",
            "varchar(255)",
            "text",
            "boolean",
            "date",
            "datetime",
            "decimal(12,2)",
            "char(36)",
            "json",
            "blob",
        ],
    }
}

/// 校验用户在 WHERE 框里写的**自由文本**条件（阶段 84）。
///
/// **为什么要校验**：WHERE 框里是**用户自己写的 SQL 片段**——用户本来就有
/// 完整的 SQL 编辑器，权限上没风险。但**一条查询语句里塞分号 = 多语句**
/// ⇒ 服务器会把分号后面那句也执行了 ⇒ 用户以为自己在"筛一下"，结果库被改了。
/// 宁可在这里拦下来，也不让一条筛选变成一条写语句。
///
/// **只拦分号**，别的（`OR 1=1` 之类）**不拦**——那是用户自己的条件，
/// 他自己负责；我们的职责只有一条：**别把一条筛选变成一条写语句**。
///
/// **分号在字符串里不算**（`name = 'a;b'` 是合法的）⇒ 扫描时跳过引号内部。
/// 引号按 SQL 的两种都认：单引号（字符串）与双引号（标识符）。
pub fn check_free_where(text: &str) -> Result<String, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    if t.chars().count() > 4000 {
        return Err(tr("条件太长了（最多 4000 个字符）").to_string());
    }
    let mut quote: Option<char> = None;
    let mut semi: Option<usize> = None; // 第一个**在引号外**的分号（字节位置）
    for (i, c) in t.char_indices() {
        match quote {
            // 在引号里：只有遇到**同一个**引号才出来
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                ';' => {
                    if semi.is_none() {
                        semi = Some(i);
                    }
                }
                _ => {}
            },
        }
    }
    if let Some(i) = semi {
        // ⚠️ **结尾那个分号放过**：用户从别处粘一段 `id = 1;` 过来很常见，
        // 那个分号只是句号，不多执行任何东西 ⇒ 替他去掉。
        // 中间的分号 = 多语句 ⇒ 拒绝。
        let rest = t[i + 1..].trim();
        if !rest.is_empty() {
            return Err(tr("条件里不能有分号（一条查询只能带一条语句）").to_string());
        }
        return Ok(t[..i].trim().to_string());
    }
    Ok(t.to_string())
}

/// ORDER BY 片段（阶段 48：可视化条件条里的「排序」）。
///
/// 空列名 → 空串（不拼 ORDER BY）。
///
/// ⚠️ **列名要按方言加引号**：列名里有空格 / 保留字时不加引号会报语法错。
pub fn build_order_by(proto: DbProto, column: &str, asc: bool) -> String {
    let c = column.trim();
    if c.is_empty() {
        return String::new();
    }
    let dir = if asc { "ASC" } else { "DESC" };
    format!("ORDER BY {} {dir}", quote_ident(proto, c))
}

/// 打开一张表的数据（可带 WHERE 条件）：`SELECT * FROM 表 [WHERE …] LIMIT n`。
pub fn build_select_table_with_where(
    proto: DbProto,
    schema: &str,
    table: &str,
    limit: usize,
    where_clause: &str,
) -> String {
    build_select_table_page(proto, schema, table, limit, 0, where_clause)
}

/// 同上，但能指定**偏移**（跳页用，阶段 45）。
///
/// ⚠️ **为什么用 `LIMIT … OFFSET …` 跳页，而不是"重新查再跳过"**：
/// PG 的分页游标是 `NO SCROLL`（**不能回退**），要回到上一页只能重新执行
/// 查询；而把 `OFFSET` 塞进 SQL 里，游标 `DECLARE … FOR SELECT … LIMIT n
/// OFFSET m` 之后 `FETCH FORWARD n` 取到的**正好是第 m 行起的那一页**，
/// PG 与 MySQL 通用（MySQL 本来就是客户端切片，SQL 自带 LIMIT/OFFSET 正好）。
///
/// `offset == 0` 时**不写 OFFSET**——老语句原样，改动最小。
pub fn build_select_table_page(
    proto: DbProto,
    schema: &str,
    table: &str,
    limit: usize,
    offset: usize,
    where_clause: &str,
) -> String {
    build_select_table_page_order(proto, schema, table, limit, offset, where_clause, "")
}

/// 同上，但能带 **ORDER BY**（阶段 48：可视化条件条的「排序」）。
///
/// ⚠️ **顺序：WHERE → ORDER BY → LIMIT/OFFSET**（SQL 语法就是这么排的，
/// 写反了直接语法错）。
pub fn build_select_table_page_order(
    proto: DbProto,
    schema: &str,
    table: &str,
    limit: usize,
    offset: usize,
    where_clause: &str,
    order_by: &str,
) -> String {
    let w = where_clause.trim();
    let ob = order_by.trim();
    let base = quote_qualified(proto, schema, table);
    let lim = limit.max(1);
    let off = if offset > 0 {
        format!(" OFFSET {}", offset)
    } else {
        String::new()
    };
    let mut sql = format!("SELECT * FROM {base}");
    if !w.is_empty() {
        sql.push_str(&format!(" WHERE {w}"));
    }
    if !ob.is_empty() {
        sql.push_str(&format!(" {ob}"));
    }
    sql.push_str(&format!(" LIMIT {lim}{off}"));
    sql
}

/* ---------------- 建表 / 改名 / 删表（9.6） ---------------- */

/// 新建表：`CREATE TABLE 表 (列 定义, …)`。
pub fn build_create_table(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[(String, String)],
) -> String {
    let cols: Vec<String> = columns
        .iter()
        .map(|(n, t)| format!("{} {t}", quote_ident(proto, n)))
        .collect();
    format!("CREATE TABLE {} ({})",
        quote_qualified(proto, schema, table),
        cols.join(", ")
    )
}

/// 重命名表。
///
/// **方言不同**：PG 是 `ALTER TABLE … RENAME TO …`（新名**不带**模式），
/// MySQL 是 `RENAME TABLE … TO …`（新名可以带模式，但这里固定留在原模式）。
pub fn build_rename_table(proto: DbProto, schema: &str, from: &str, to: &str) -> String {
    match proto {
        DbProto::Postgres => format!("ALTER TABLE {} RENAME TO {}",
            quote_qualified(proto, schema, from),
            quote_ident(proto, to)
        ),
        DbProto::MySql => format!("RENAME TABLE {} TO {}",
            quote_qualified(proto, schema, from),
            quote_qualified(proto, schema, to)
        ),
    }
}

/// 删表：`DROP TABLE 表`。
pub fn build_drop_table(proto: DbProto, schema: &str, table: &str) -> String {
    format!("DROP TABLE {}", quote_qualified(proto, schema, table))
}

/// 解析用户填的列定义（每行「名称 类型」）。
///
/// **类型是裸的 SQL 片段**（`varchar(50)`、`timestamp with time zone`），
/// 直接拼进 CREATE TABLE —— 所以只放行「字母数字下划线 + 空格 + 括号 + 逗号」，
/// 其余一律报错。列名另有限制（见 `is_simple_ident`）。
///
/// 一行都没填 → 返回默认两列（`id integer` / `name text`）：MySQL 不允许建
/// 没有列的表，空着报错对用户也不友好。
pub fn parse_ddl_columns(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else { continue };
        let ty: String = parts.collect::<Vec<_>>().join(" ");
        if !is_simple_ident(name) {
            return Err(trf!("第 {v0} 行的列名「{name}」不合法", name = name, v0 = i + 1));
        }
        if ty.is_empty() {
            return Err(trf!("第 {v0} 行没写类型", v0 = i + 1));
        }
        if !ty
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_(), ".contains(c))
        {
            return Err(trf!("第 {v0} 行的类型里有非法字符", v0 = i + 1));
        }
        out.push((name.to_string(), ty));
    }
    if out.is_empty() {
        return Ok(vec![
            ("id".to_string(), "integer".to_string()),
            ("name".to_string(), "text".to_string()),
        ]);
    }
    Ok(out)
}

/// 简单的标识符（字母数字下划线，且不以数字开头）。
pub fn is_simple_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 一行的改动集合：列名 → 新值字面量。
pub type Sets = Vec<(String, String)>;

/// 一行的定位条件：列名 → 旧值（`None` 表示该列原本是 NULL）。
pub type Cond = Vec<(String, Option<String>)>;

/// 构造 UPDATE。
///
/// `pk` 非空时用它定位（主键最可靠）；为空则退回 `fallback`（整行原值）。
/// 两种情况都要求**每个条件列都有旧值**，否则定位不到唯一一行——那种情况
/// 不生成语句（返回错误），而不是生成一个会改到多行的语句。
pub fn build_update(
    proto: DbProto,
    schema: &str,
    table: &str,
    pk: &[(String, Option<String>)],
    sets: &[(String, String)],
    fallback: &[(String, Option<String>)],
) -> Result<String, String> {
    if sets.is_empty() {
        return Err(tr("没有要保存的改动").to_string());
    }
    let cond = if !pk.is_empty() { pk } else { fallback };
    if cond.is_empty() {
        return Err(tr("无法定位行：既没有主键，也不知道原始值").to_string());
    }
    let set_sql: Vec<String> = sets
        .iter()
        .map(|(c, v)| format!("{} = {v}", quote_ident(proto, c)))
        .collect();
    let where_sql: Vec<String> = cond
        .iter()
        .map(|(c, v)| format!("{} {}", quote_ident(proto, c), where_value(proto, v.as_deref())))
        .collect();
    Ok(format!("UPDATE {} SET {} WHERE {}",
        quote_qualified(proto, schema, table),
        set_sql.join(", "),
        where_sql.join(" AND ")
    ))
}

/// 构造 DELETE（按主键 / 整行原值定位）。
pub fn build_delete(
    proto: DbProto,
    schema: &str,
    table: &str,
    pk: &[(String, Option<String>)],
    fallback: &[(String, Option<String>)],
) -> Result<String, String> {
    let cond = if !pk.is_empty() { pk } else { fallback };
    if cond.is_empty() {
        return Err(tr("无法定位行：既没有主键，也不知道原始值").to_string());
    }
    let where_sql: Vec<String> = cond
        .iter()
        .map(|(c, v)| format!("{} {}", quote_ident(proto, c), where_value(proto, v.as_deref())))
        .collect();
    Ok(format!("DELETE FROM {} WHERE {}",
        quote_qualified(proto, schema, table),
        where_sql.join(" AND ")
    ))
}

/// 一行的定位条件（主键优先；没有主键就用整行原值）。
///
/// 返回 `(主键条件, 整行条件)`——两个都算好，调用方按需选一个传下去。
///
/// **为什么要两个**：主键最可靠，但表可能没有主键（或者视图的主键是拼出来的）；
/// 这时只能用"整行原值"定位——条件列全都有旧值时能定位到唯一一行，
/// 缺一个就定位不准，所以由 `build_update` / `build_delete` 去判断能不能用。
pub fn row_condition(
    columns: &[String],
    pk: &[usize],
    orig: &[Option<String>],
) -> (Cond, Cond) {
    let pk_cond: Cond = pk
        .iter()
        .filter_map(|&c| {
            columns
                .get(c)
                .map(|n| (n.clone(), orig.get(c).and_then(|v| v.clone())))
        })
        .collect();
    let fallback: Cond = columns
        .iter()
        .enumerate()
        .map(|(c, name)| (name.clone(), orig.get(c).and_then(|v| v.clone())))
        .collect();
    (pk_cond, fallback)
}

/// 把「表格里的改动」拼成一组 UPDATE 语句。
///
/// 参数：
/// - `columns` 列名；`types` 列类型（决定新值怎么转字面量）
/// - `pk` 主键列下标（定位行用；空则退回整行原值）
/// - `rows` **原始**行（WHERE 条件必须用原值——改了就没法定位原行了）
/// - `edits` 行号 → (列号 → 新值文本)
///
/// 返回 Err 时说明改动本身有问题（类型不符等），**这时一条都不该执行**——
/// 调用方要先把错误报给用户，而不是执行一半。
pub fn build_updates(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[String],
    types: &[ColType],
    pk: &[usize],
    rows: &[Vec<Option<String>>],
    edits: &HashMap<usize, HashMap<usize, String>>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    // 行号排序：语句顺序稳定，便于对照日志
    let mut row_ids: Vec<usize> = edits.keys().copied().collect();
    row_ids.sort_unstable();
    for row in row_ids {
        let cols = &edits[&row];
        let Some(orig) = rows.get(row) else {
            return Err(trf!("第 {v0} 行不在结果集里", v0 = row + 1));
        };
        // 定位条件：主键优先；没有主键就用整行原值
        let (pk_cond, fallback) = row_condition(columns, pk, orig);
        let mut sets: Vec<(String, String)> = Vec::new();
        let mut col_ids: Vec<usize> = cols.keys().copied().collect();
        col_ids.sort_unstable();
        for c in col_ids {
            let text = &cols[&c];
            let Some(name) = columns.get(c) else {
                return Err(trf!("第 {v0} 行引用了不存在的列", v0 = row + 1));
            };
            let ty = types.get(c).copied().unwrap_or(ColType::Text);
            let lit = edit_literal(proto, ty, text)
                .map_err(|e| trf!("第 {v0} 行的「{name}」：{e}", name = name, e = e, v0 = row + 1))?;
            sets.push((name.clone(), lit));
        }
        if sets.is_empty() {
            continue;
        }
        let sql = build_update(proto, schema, table, &pk_cond, &sets, &fallback)?;
        out.push(sql);
    }
    Ok(out)
}

/* ---------------- 元数据查询 ---------------- */

/// 列出模式（PG）/ 库（MySQL）。
///
/// 系统模式过滤掉：用户要的是自己的东西，不是 `pg_catalog` 那几百张表。
pub fn meta_schemas() -> String {
    "SELECT schema_name FROM information_schema.schemata \
     WHERE schema_name NOT IN ('pg_catalog','information_schema') \
       AND schema_name NOT LIKE 'pg_toast%' \
       AND schema_name NOT LIKE 'pg_temp%' \
       AND schema_name NOT LIKE 'pg_%' \
     ORDER BY 1"
        .to_string()
}

/// 列出表与视图。
pub fn meta_tables(proto: DbProto, schema: &str) -> String {
    format!("SELECT table_name, table_type FROM information_schema.tables \
         WHERE table_schema = {} ORDER BY table_name",
        quote_str(proto, schema)
    )
}

/// 列出列（含主键标记、默认值、类型细节）。
///
/// 主键用 `EXISTS` 子查询标出来——PG 与 MySQL 的 `information_schema` 都有
/// `table_constraints` / `key_column_usage`，同一份 SQL 两边都能跑。
///
/// 顺带把**默认值**、字符长度、数值精度也查出来：表结构页签要照着这些
/// 拼出 `CREATE TABLE`——少了默认值，用户看一眼 DDL 再执行就把默认值弄没了。
/// `udt_name` 只有 PG 有（自定义类型），MySQL 那边补一个空串，让两家列数一致
/// （解析按位置取，列数必须一样）。
pub fn meta_columns(proto: DbProto, schema: &str, table: &str) -> String {
    let udt = match proto {
        DbProto::Postgres => "c.udt_name",
        DbProto::MySql => "''",
    };
    format!("SELECT c.column_name, c.data_type, c.is_nullable, c.ordinal_position, \
         EXISTS (SELECT 1 FROM information_schema.key_column_usage k \
                 JOIN information_schema.table_constraints t \
                   ON t.constraint_name = k.constraint_name \
                  AND t.constraint_schema = k.constraint_schema \
                 WHERE t.constraint_type = 'PRIMARY KEY' \
                   AND k.table_schema = c.table_schema \
                   AND k.table_name = c.table_name \
                   AND k.column_name = c.column_name) AS is_pk, \
         c.column_default, {udt}, \
         c.character_maximum_length, c.numeric_precision, c.numeric_scale, \
         (SELECT tc.constraint_name FROM information_schema.table_constraints tc \
           WHERE tc.constraint_type = 'PRIMARY KEY' \
             AND tc.table_schema = c.table_schema AND tc.table_name = c.table_name \
           LIMIT 1) AS pk_name \
         FROM information_schema.columns c \
         WHERE c.table_schema = {} AND c.table_name = {} \
         ORDER BY c.ordinal_position",
        quote_str(proto, schema),
        quote_str(proto, table)
    )
}

/// 列出外键（表结构页签用）。
///
/// **要按方言分派**：`information_schema.constraint_column_usage` 是 PG 专有
/// 表，MySQL 没有（查它会报 `ERROR 1109 Unknown table`）。MySQL 的引用列在
/// `key_column_usage` 自己身上（`referenced_table_name` /
/// `referenced_column_name`）。
pub fn meta_foreign_keys(proto: DbProto, schema: &str, table: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT tc.constraint_name, kcu.column_name, ccu.table_name, ccu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.constraint_schema = kcu.constraint_schema \
             JOIN information_schema.constraint_column_usage ccu \
               ON ccu.constraint_name = tc.constraint_name \
              AND ccu.constraint_schema = tc.constraint_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema = {} AND tc.table_name = {} \
             ORDER BY tc.constraint_name, kcu.ordinal_position",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
        DbProto::MySql => format!("SELECT tc.constraint_name, kcu.column_name, \
                    kcu.referenced_table_name, kcu.referenced_column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema = {} AND tc.table_name = {} \
               AND kcu.referenced_table_name IS NOT NULL \
             ORDER BY tc.constraint_name",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
    }
}

/// 模式下的所有外键（ER 图用；阶段 19）。
///
/// **为什么要单独一套**：ER 图要的是**整个模式**的外键关系——不只是某张表
/// 的（那是 `meta_foreign_keys` 的活）。而且这里必须带上**持有外键的表名**
/// （单表查询里表名是已知的，整模式查询里得问服务器要）。
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaForeignKey {
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

/// 一个模式下的**所有**外键（ER 图用；阶段 19）。
///
/// **列序**：`表名, 约束名, 本表列, 引用表, 引用列`。
///
/// **PG 的写法**：用 `referential_constraints` 找到被引用的唯一约束，再用
/// `position_in_unique_constraint` 把两边的列**按位置对上**。之前那条
/// 单表查询是 `key_column_usage` 直接 join `constraint_column_usage`——
/// 复合外键（两列）会叉乘成 4 行、列名还可能配错，ER 图上会画出错的线。
pub fn meta_schema_foreign_keys(proto: DbProto, schema: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT tc.table_name, tc.constraint_name, kcu.column_name, \
                    ccu.table_name, ccu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.constraint_schema = kcu.constraint_schema \
             JOIN information_schema.referential_constraints rc \
               ON rc.constraint_name = tc.constraint_name \
              AND rc.constraint_schema = tc.constraint_schema \
             JOIN information_schema.key_column_usage ccu \
               ON ccu.constraint_name = rc.unique_constraint_name \
              AND ccu.constraint_schema = rc.unique_constraint_schema \
              AND ccu.ordinal_position = kcu.position_in_unique_constraint \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema = {} \
             ORDER BY tc.table_name, tc.constraint_name, kcu.ordinal_position",
            quote_str(proto, schema)
        ),
        DbProto::MySql => format!("SELECT table_name, constraint_name, column_name, \
                    referenced_table_name, referenced_column_name \
             FROM information_schema.key_column_usage \
             WHERE table_schema = {} AND referenced_table_name IS NOT NULL \
             ORDER BY table_name, constraint_name, ordinal_position",
            quote_str(proto, schema)
        ),
    }
}

/// 解析模式级外键（列序与 `meta_schema_foreign_keys` 一致）。
pub fn parse_schema_foreign_keys(rows: &[Vec<Option<String>>]) -> Vec<SchemaForeignKey> {
    rows.iter()
        .filter_map(|r| {
            let table = r.first().and_then(|c| c.clone())?;
            let name = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            let column = r.get(2).and_then(|c| c.clone()).unwrap_or_default();
            let ref_table = r.get(3).and_then(|c| c.clone()).unwrap_or_default();
            let ref_column = r.get(4).and_then(|c| c.clone()).unwrap_or_default();
            if table.is_empty() || column.is_empty() || ref_table.is_empty() {
                return None;
            }
            Some(SchemaForeignKey {
                table,
                name,
                column,
                ref_table,
                ref_column,
            })
        })
        .collect()
}

/// 设计器里的一条外键（目标状态）。
#[derive(Debug, Clone, PartialEq)]
pub struct FkSpec {
    /// `Some` = 原来的约束名；`None` = 新加的外键
    pub orig_name: Option<String>,
    /// 约束名
    pub name: String,
    /// 本表的列
    pub column: String,
    /// 引用的表
    pub ref_table: String,
    /// 引用的列
    pub ref_column: String,
}

/// 外键改动 → 语句列表。
///
/// **方言分派**：
/// - PG：约束是表上的对象，`ALTER TABLE t RENAME CONSTRAINT a TO b` /
///  `DROP CONSTRAINT a` / `ADD CONSTRAINT b FOREIGN KEY …` 都行；
/// - MySQL：没有"改约束名"这回事（只能删了重建），删是
///  `ALTER TABLE t DROP FOREIGN KEY a`。
///
/// **改引用列 / 引用表一律"删了重建"**：PG 虽然能 `ALTER TABLE … DROP CONSTRAINT`
/// 再 `ADD CONSTRAINT`，但 MySQL 那边只能这样，两边统一省事。
pub fn build_fk_sqls(
    proto: DbProto,
    schema: &str,
    table: &str,
    orig: &[ForeignKey],
    target: &[FkSpec],
) -> Result<(Vec<String>, Vec<String>), String> {
    // ---- 校验 ----
    let mut seen: Vec<&str> = Vec::new();
    for t in target {
        check_ident(&t.name)?;
        if seen.contains(&t.name.as_str()) {
            return Err(trf!("约束名「{v0}」重复了", v0 = t.name));
        }
        seen.push(&t.name);
        check_ident(&t.column)?;
        check_ident(&t.ref_table)?;
        check_ident(&t.ref_column)?;
    }

    let kept: Vec<&str> = target.iter().filter_map(|t| t.orig_name.as_deref()).collect();
    let dropped: Vec<&ForeignKey> = orig
        .iter()
        .filter(|f| !kept.contains(&f.name.as_str()))
        .collect();

    let mut sqls: Vec<String> = Vec::new();
    let mut risks: Vec<String> = Vec::new();
    // 删掉的
    for f in &dropped {
        sqls.push(drop_fk_sql(proto, schema, table, &f.name));
        risks.push(trf!("删外键「{v0}」——数据不丢，但引用关系没了", v0 = f.name));
    }
    for t in target {
        let old = t
            .orig_name
            .as_deref()
            .and_then(|n| orig.iter().find(|f| f.name == n));
        match old {
            None => {
                sqls.push(add_fk_sql(proto, schema, table, t));
                risks.push(trf!("加外键「{v0}」——被引用的列上必须有值，否则加不上", v0 = t.name));
            }
            Some(o) => {
                let same_ref = o.column == t.column
                    && o.ref_table == t.ref_table
                    && o.ref_column == t.ref_column;
                if same_ref && o.name == t.name {
                    continue;
                }
                if same_ref && o.name != t.name {
                    // 只改了个名字
                    if proto == DbProto::Postgres {
                        // PG 支持直接改名
                        sqls.push(format!("ALTER TABLE {} RENAME CONSTRAINT {} TO {}",
                            quote_qualified(proto, schema, table),
                            quote_ident(proto, &o.name),
                            quote_ident(proto, &t.name)
                        ));
                    } else {
                        // MySQL 没有改约束名的语法 → 删了重建
                        sqls.push(drop_fk_sql(proto, schema, table, &o.name));
                        sqls.push(add_fk_sql(proto, schema, table, t));
                    }
                    continue;
                }
                // 引用变了 → 删了重建
                sqls.push(drop_fk_sql(proto, schema, table, &o.name));
                sqls.push(add_fk_sql(proto, schema, table, t));
                risks.push(trf!("重建外键「{v0}」——引用关系变了", v0 = o.name));
            }
        }
    }
    Ok((sqls, risks))
}

/// 删一条外键的语句。
fn drop_fk_sql(proto: DbProto, schema: &str, table: &str, name: &str) -> String {
    match proto {
        DbProto::Postgres => format!("ALTER TABLE {} DROP CONSTRAINT {}",
            quote_qualified(proto, schema, table),
            quote_ident(proto, name)
        ),
        DbProto::MySql => format!("ALTER TABLE {} DROP FOREIGN KEY {}",
            quote_qualified(proto, schema, table),
            quote_ident(proto, name)
        ),
    }
}

/// 加一条外键的语句。
fn add_fk_sql(proto: DbProto, schema: &str, table: &str, t: &FkSpec) -> String {
    format!("ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        quote_qualified(proto, schema, table),
        quote_ident(proto, &t.name),
        quote_ident(proto, &t.column),
        quote_ident(proto, &t.ref_table),
        quote_ident(proto, &t.ref_column)
    )
}

/// 列出索引（**结构化**：名字 / 唯一 / 列）。
///
/// **为什么要结构化**：原来的 `meta_indexes` 返回的是原文（PG 的
/// `pg_indexes.indexdef`、MySQL 的列名），MySQL 那边还用了 `DISTINCT`，
/// 多列索引只剩一列——要让人**改**索引，就得先知道它有哪几列、是不是唯一。
///
/// **方言分派**：PG 的索引信息散在 `pg_index`（唯一性、主键标记）和
/// `pg_attribute`（列）里，要用 `unnest(indkey)` 把列按顺序摊平；
/// MySQL 的 `information_schema.STATISTICS` 一行一列，本来就是摊平的。
pub fn meta_index_defs(proto: DbProto, schema: &str, table: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT i.relname, ix.indisunique, ix.indisprimary, a.attname \
             FROM pg_index ix \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON TRUE \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE n.nspname = {} AND t.relname = {} \
             ORDER BY i.relname, k.ord",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
        DbProto::MySql => format!("SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             ORDER BY INDEX_NAME, SEQ_IN_INDEX",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
    }
}

/// 索引（结构页签用，可编辑）。
#[derive(Debug, Clone, PartialEq)]
pub struct IndexDef {
    /// 索引名
    pub name: String,
    /// 是否唯一索引
    pub unique: bool,
    /// 是不是主键索引
    ///
    /// **主键索引不显示在索引页**：主键由字段页的「主键」列管，两边都能改
    /// 就会打架（字段页改主键要重建约束，索引页又把它当普通索引删了）。
    pub primary: bool,
    /// 列（按索引里的顺序）
    pub columns: Vec<String>,
}

/// 解析索引（列序与 `meta_index_defs` 一致）。
///
/// 同一个索引在结果里是**多行**（一行一列），要并成一条。
pub fn parse_index_defs(proto: DbProto, rows: &[Vec<Option<String>>]) -> Vec<IndexDef> {
    let flag = |v: Option<&String>| -> bool {
        v.map(|s| {
            let s = s.trim();
            matches!(s.to_ascii_lowercase().as_str(), "t" | "true" | "1" | "yes" | "y")
        })
        .unwrap_or(false)
    };
    let mut out: Vec<IndexDef> = Vec::new();
    for r in rows {
        match proto {
            DbProto::Postgres => {
                let name = r.first().and_then(|c| c.clone()).unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let unique = flag(r.get(1).and_then(|v| v.as_ref()));
                let primary = flag(r.get(2).and_then(|v| v.as_ref()));
                let col = r.get(3).and_then(|c| c.clone()).unwrap_or_default();
                if let Some(last) = out.last_mut() {
                    if last.name == name {
                        last.columns.push(col);
                        continue;
                    }
                }
                out.push(IndexDef {
                    name,
                    unique,
                    primary,
                    columns: vec![col],
                });
            }
            DbProto::MySql => {
                let name = r.first().and_then(|c| c.clone()).unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let col = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
                // NON_UNIQUE = 1 表示「不是唯一索引」
                let unique = !flag(r.get(2).and_then(|v| v.as_ref()));
                let primary = name.eq_ignore_ascii_case("PRIMARY");
                if let Some(last) = out.last_mut() {
                    if last.name == name {
                        last.columns.push(col);
                        continue;
                    }
                }
                out.push(IndexDef {
                    name,
                    unique,
                    primary,
                    columns: vec![col],
                });
            }
        }
    }
    out
}

/// 索引里的一列。
#[derive(Debug, Clone, PartialEq)]
pub struct IndexCol {
    /// 列名
    pub name: String,
    /// 前缀长度（`note(10)` 的 10）
    ///
    /// **MySQL 的 TEXT / BLOB 列建索引必须给前缀长度**（否则报
    /// `ERROR 1170 BLOB/TEXT column used in key specification without a key
    /// length`）；PG 的 text 列可以直接建索引，不支持前缀。
    pub prefix: Option<u32>,
}

/// 解析索引的列列表文本（`id, note(10)`）。
///
/// **为什么支持 `列(长度)`**：MySQL 的 TEXT / BLOB 列不给前缀长度就建不了
/// 索引（`ERROR 1170`）。用户没法在别处表达这个意思，只能在这儿写。
pub fn parse_index_columns(text: &str) -> Vec<IndexCol> {
    text.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(open) = s.find('(') {
                if s.ends_with(')') {
                    let name = s[..open].trim().to_string();
                    let prefix = s[open + 1..s.len() - 1].trim().parse::<u32>().ok();
                    return IndexCol { name, prefix };
                }
            }
            IndexCol {
                name: s.to_string(),
                prefix: None,
            }
        })
        .collect()
}

/// 设计器里的一条索引（目标状态）。
#[derive(Debug, Clone, PartialEq)]
pub struct IndexSpec {
    /// `Some` = 原来的索引名；`None` = 新加的索引
    pub orig_name: Option<String>,
    /// 索引名
    pub name: String,
    /// 是否唯一
    pub unique: bool,
    /// 列（按顺序）
    pub columns: Vec<IndexCol>,
}

/// 索引改动 → 语句列表。
///
/// **为什么是列表而不是一条**：PG 的 `DROP INDEX` / `CREATE INDEX` 是**独立
/// 语句**（`ALTER TABLE` 不管索引），一条 `ALTER TABLE` 装不下；MySQL 可以
/// 一条 `ALTER TABLE t DROP INDEX …, ADD INDEX …` 搞定。
///
/// PG 的多条语句由调用方**包在一个事务里**（PG 支持事务性 DDL）；MySQL 的
/// DDL 不支持事务回滚——MySQL 那条语句本身是原子的（8.0 的原子 DDL），
/// 但多条之间失败会留下半截状态，这一点要在界面上说明。
pub fn build_index_sqls(
    proto: DbProto,
    schema: &str,
    table: &str,
    orig: &[IndexDef],
    target: &[IndexSpec],
) -> Result<(Vec<String>, Vec<String>), String> {
    // ---- 校验 ----
    let mut seen: Vec<&str> = Vec::new();
    for t in target {
        check_ident(&t.name)?;
        if seen.contains(&t.name.as_str()) {
            return Err(trf!("索引名「{v0}」重复了", v0 = t.name));
        }
        seen.push(&t.name);
        if t.columns.is_empty() {
            return Err(trf!("索引「{v0}」没有列", v0 = t.name));
        }
        for c in &t.columns {
            check_ident(&c.name)?;
            if let Some(n) = c.prefix {
                // PG 的索引不支持前缀长度（那是 MySQL 的 TEXT 列专用的写法）
                if proto == DbProto::Postgres {
                    return Err(trf!("索引「{v0}」的列「{v1}」写了长度，PG 不支持前缀索引（去掉长度试试）", v0 = t.name, v1 = c.name));
                }
                if n == 0 {
                    return Err(trf!("索引「{v0}」的列「{v1}」长度不能是 0", v0 = t.name, v1 = c.name));
                }
            }
        }
    }

    // 主键索引不参与：主键由字段页管
    let orig: Vec<&IndexDef> = orig.iter().filter(|i| !i.primary).collect();
    let kept: Vec<&str> = target.iter().filter_map(|t| t.orig_name.as_deref()).collect();
    let dropped: Vec<&&IndexDef> = orig.iter().filter(|i| !kept.contains(&i.name.as_str())).collect();

    let mut sqls: Vec<String> = Vec::new();
    let mut risks: Vec<String> = Vec::new();
    // 删掉的索引
    for i in &dropped {
        sqls.push(drop_index_sql(proto, schema, table, &i.name));
        risks.push(trf!("删索引「{v0}」——数据不丢，但查这张表会变慢", v0 = i.name));
    }
    // 改过或新增的索引
    for t in target {
        let old = t.orig_name.as_deref().and_then(|n| orig.iter().find(|i| i.name == n));
        let changed = match old {
            None => true,
            Some(o) => {
                o.name != t.name
                    || o.unique != t.unique
                    || !same_index_cols(&o.columns, &t.columns)
            }
        };
        if !changed {
            continue;
        }
        // 改名 / 改列 / 改唯一性 → 旧的删掉、新的建一遍
        // （PG 没有 ALTER INDEX SET COLUMN，只能删了重建）
        if let Some(o) = old {
            if o.name != t.name || !same_index_cols(&o.columns, &t.columns) || o.unique != t.unique {
                sqls.push(drop_index_sql(proto, schema, table, &o.name));
                risks.push(trf!("重建索引「{v0}」——数据不丢，但重建期间查询会变慢", v0 = o.name));
            }
        }
        sqls.push(create_index_sql(proto, schema, table, t));
        if t.unique {
            risks.push(trf!("加唯一索引「{v0}」——现有数据里有重复值就会失败", v0 = t.name));
        }
    }
    Ok((sqls, risks))
}

/// 删一条索引的语句。
fn drop_index_sql(proto: DbProto, schema: &str, table: &str, name: &str) -> String {
    match proto {
        // PG：索引是独立的 schema 对象，要带上 schema
        DbProto::Postgres => format!("DROP INDEX {}", quote_qualified(proto, schema, name)),
        // MySQL：索引属于表，只能用 ALTER TABLE 删
        DbProto::MySql => format!("ALTER TABLE {} DROP INDEX {}",
            quote_qualified(proto, schema, table),
            quote_ident(proto, name)
        ),
    }
}

/// 建一条索引的语句。
fn create_index_sql(proto: DbProto, schema: &str, table: &str, t: &IndexSpec) -> String {
    let cols = t
        .columns
        .iter()
        .map(|c| match c.prefix {
            // MySQL 的前缀索引：`note(10)`
            Some(n) => format!("{}({n})", quote_ident(proto, &c.name)),
            None => quote_ident(proto, &c.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if t.unique { "UNIQUE " } else { "" };
    match proto {
        DbProto::Postgres => format!("CREATE {unique}INDEX {} ON {} ({cols})",
            quote_ident(proto, &t.name),
            quote_qualified(proto, schema, table)
        ),
        DbProto::MySql => format!("ALTER TABLE {} ADD {unique}INDEX {} ({cols})",
            quote_qualified(proto, schema, table),
            quote_ident(proto, &t.name)
        ),
    }
}

/// 索引的列是否一样（顺序也算）。
///
/// **只比列名，不比前缀长度**：服务器那边的前缀长度（MySQL 的 `SUB_PART`）
/// 这一版没查，读回来的索引就没有前缀信息。只比列名的话，读回来的索引跟
/// 用户没动过它是一样的，不会平白生成"删了重建"。
fn same_index_cols(a: &[String], b: &[IndexCol]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == &y.name)
}

/// 列出索引。
///
/// 索引信息 PG 与 MySQL 不在同一张表里（PG 在 `pg_indexes`，MySQL 在
/// `information_schema.STATISTICS`），这里按方言分派。
pub fn meta_indexes(proto: DbProto, schema: &str, table: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT indexname, indexdef FROM pg_indexes \
             WHERE schemaname = {} AND tablename = {} ORDER BY indexname",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
        DbProto::MySql => format!("SELECT DISTINCT index_name, column_name FROM information_schema.STATISTICS \
             WHERE table_schema = {} AND table_name = {} ORDER BY index_name",
            quote_str(proto, schema),
            quote_str(proto, table)
        ),
    }
}

/* ---------------- 例程 / 触发器（9.3） ---------------- */

/// 例程的种类（函数 / 存储过程）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Function,
    Procedure,
}

impl RoutineKind {
    pub fn label(self) -> &'static str {
        match self {
            RoutineKind::Function => tr("函数"),
            RoutineKind::Procedure => tr("存储过程"),
        }
    }
    /// SQL 里的标记（PG 的 prokind / MySQL 的 routine_type）。
    fn code(self) -> &'static str {
        match self {
            RoutineKind::Function => "F",
            RoutineKind::Procedure => "P",
        }
    }
    fn from_code(c: &str) -> RoutineKind {
        if c.trim().eq_ignore_ascii_case("P") || c.trim().eq_ignore_ascii_case("PROCEDURE") {
            RoutineKind::Procedure
        } else {
            RoutineKind::Function
        }
    }
}

/// 一个例程（函数 / 存储过程）。
#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    pub name: String,
    pub kind: RoutineKind,
    /// 参数列表原文（PG 用 identity arguments，MySQL 留空）
    pub args: String,
    /// 返回类型（存储过程为空）
    pub returns: String,
}

/// 列出函数与存储过程。
///
/// **按方言分派**：PG 的例程在 `pg_proc`（`prokind` 区分函数与过程），
/// MySQL 的在 `information_schema.ROUTINES`。列数保持一致（4 列），解析
/// 才能按位置取。
pub fn meta_routines(proto: DbProto, schema: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT p.proname, \
                    CASE WHEN p.prokind = 'p' THEN 'P' ELSE 'F' END, \
                    pg_get_function_identity_arguments(p.oid), \
                    CASE WHEN p.prokind = 'p' THEN '' ELSE pg_get_function_result(p.oid) END \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = {} AND p.prokind IN ('f','p') \
             ORDER BY 1",
            quote_str(proto, schema)
        ),
        DbProto::MySql => format!("SELECT routine_name, \
                    CASE WHEN routine_type = 'PROCEDURE' THEN 'P' ELSE 'F' END, \
                    '', '' \
             FROM information_schema.ROUTINES \
             WHERE routine_schema = {} AND routine_type IN ('FUNCTION','PROCEDURE') \
             ORDER BY 1",
            quote_str(proto, schema)
        ),
    }
}

/// 列出触发器（两家的 `information_schema.triggers` 都有这几列）。
pub fn meta_triggers(proto: DbProto, schema: &str) -> String {
    format!("SELECT trigger_name, event_object_table, action_timing, event_manipulation \
         FROM information_schema.triggers \
         WHERE trigger_schema = {} \
         ORDER BY trigger_name, event_object_table",
        quote_str(proto, schema)
    )
}

/// 取例程的定义（PG 用 `pg_get_functiondef`，MySQL 用 `SHOW CREATE`）。
///
/// PG 按**名字 + identity 参数**匹配——同名重载（`f(int)` / `f(text)`）要能
/// 区分开，只按名字会取错。
pub fn meta_routine_def(
    proto: DbProto,
    schema: &str,
    name: &str,
    args: &str,
    is_proc: bool,
) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT pg_get_functiondef(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = {} AND p.proname = {} \
               AND pg_get_function_identity_arguments(p.oid) = {} \
               AND p.prokind = {}",
            quote_str(proto, schema),
            quote_str(proto, name),
            quote_str(proto, args),
            if is_proc { "'p'" } else { "'f'" }
        ),
        DbProto::MySql => {
            let kw = if is_proc { "PROCEDURE" } else { "FUNCTION" };
            format!("SHOW CREATE {} {}", kw, quote_qualified(proto, schema, name))
        }
    }
}

/// 取触发器的定义。
pub fn meta_trigger_def(proto: DbProto, schema: &str, table: &str, name: &str) -> String {
    match proto {
        DbProto::Postgres => format!("SELECT pg_get_triggerdef(t.oid) \
             FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {} AND c.relname = {} AND t.tgname = {}",
            quote_str(proto, schema),
            quote_str(proto, table),
            quote_str(proto, name)
        ),
        DbProto::MySql => format!("SHOW CREATE TRIGGER {}",
            quote_qualified(proto, schema, name)
        ),
    }
}

/// 一个事件（定时任务）。**只有 MySQL 有**（见 `proto_supports_events`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub name: String,
    /// 状态（ENABLED / DISABLED / REPLICA）
    pub status: String,
    /// 调度（MySQL 原文，如 `1 DAY`、`12 HOUR`）
    pub schedule: String,
}

/// 这个方言有没有「定时事件」（Event）可查。
///
/// **只有 MySQL 有**（Event Scheduler）：`CREATE EVENT … ON SCHEDULE EVERY …`、
/// `information_schema.EVENTS`、`SHOW CREATE EVENT`。
///
/// **PostgreSQL 压根没有这个功能。** 它有的是 `pg_event_trigger`
/// （`CREATE EVENT TRIGGER`，**DDL 钩子**——在 DDL 语句执行前后触发一个函数），
/// 名字像，性质完全不同：一个是 cron 定时任务，一个是建 DDL 时挂的钩子。
/// 查过 PG 9.6 / 10 / 12 / 17 / 18 五个版本的官方系统目录清单，里面**只有**
/// `pg_event_trigger`，**从来没有 `pg_event`**；`CREATE EVENT` 与
/// `pg_get_eventdef` 也从来不是 PG 的语法。硬查只会得到
/// `relation "pg_event" does not exist`。
pub fn proto_supports_events(proto: DbProto) -> bool {
    matches!(proto, DbProto::MySql)
}

/// 列出事件。方言没有这个功能时返回 `None`（PG 没有定时事件，见
/// `proto_supports_events`）。
///
/// MySQL 的事件属于某个库（= schema），所以按 `EVENT_SCHEMA` 过滤。
pub fn meta_events(proto: DbProto, schema: &str) -> Option<String> {
    if !proto_supports_events(proto) {
        return None;
    }
    // **只用各版本都有的列**：`SCHEDULING_TYPE`（MySQL 5.7/8.0）与
    // `EVENT_TYPE`（新版本）列名不一样，跨版本不通用；`INTERVAL_VALUE` +
    // `INTERVAL_FIELD` + `STATUS` 从 5.7 到新版本一直都在。
    // ⚠️ **i18n:keep**：这是**发给数据库的 SQL**，里面的中文是**数据值**
    //    （`'一次'` 是 MySQL 事件状态的显示值）⇒ **翻了 SQL 就查不到东西**。
    //    改写脚本本来会把它包成 `trf!()` ⇒ 用 `i18n:keep` 挡住。
    // i18n:keep
    Some(format!(
        "SELECT EVENT_NAME, STATUS, CASE WHEN INTERVAL_VALUE IS NULL THEN '一次' \
         ELSE CONCAT(INTERVAL_VALUE, ' ', INTERVAL_FIELD) END \
         FROM information_schema.EVENTS WHERE EVENT_SCHEMA = {} ORDER BY EVENT_NAME",
        quote_str(proto, schema)
    ))
    // i18n:end
}

/// 取事件的定义。方言没有这个功能时返回 `None`（见 `proto_supports_events`）。
pub fn meta_event_def(proto: DbProto, schema: &str, name: &str) -> Option<String> {
    if !proto_supports_events(proto) {
        return None;
    }
    Some(format!("SHOW CREATE EVENT {}",
        quote_qualified(proto, schema, name)
    ))
}

/// 对象属性（对标 Navicat 的 **Details / 对象属性** 面板）。
///
/// **取不到就留空，不编**：PG 的核心目录里**没有"创建时间"**（`pg_class`
/// 没有这一列），MySQL 的 `information_schema` 也**没有"表注释之外"的很多
/// 信息**。缺的那一项显示「—」，不拿别的东西凑数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectStats {
    /// 行数（**估算值**：PG 的 `reltuples`、MySQL 的 `TABLE_ROWS` 都是估算）
    pub rows: Option<u64>,
    /// 占用的字节数（含索引）
    pub size_bytes: Option<u64>,
    /// 注释（PG 的 `obj_description`、MySQL 的 `TABLE_COMMENT`）
    pub comment: String,
    /// 列数
    pub columns: Option<u64>,
    /// 索引数
    pub indexes: Option<u64>,
    /// 外键数
    pub foreign_keys: Option<u64>,
    /// 类型 / 引擎（PG：`BASE TABLE` / `VIEW`；MySQL：`InnoDB`）
    pub engine: String,
    /// 排序规则（MySQL 有；PG 这一项留空）
    pub collation: String,
    /// 字符集（MySQL 的排序规则名形如 `utf8mb4_0900_ai_ci`，前半段就是字符集）
    pub charset: String,
    /// 创建时间（MySQL 有；PG 留空）
    pub created: String,
    /// 自增当前值（MySQL 有；PG 留空）
    pub auto_increment: String,
}

impl ObjectStats {
    /// 按 Navicat 的样式列成「属性 | 值」。
    ///
    /// **空的项也要列出来**（值写「—」）：Navicat 的属性面板里每一项都占
    /// 一行，用户扫一眼就知道"这个库没给这个信息"；把没拿到的项直接
    /// 藏起来，用户会以为这个功能没做。
    pub fn rows(&self) -> Vec<(String, String)> {
        let n = |v: Option<u64>| match v {
            Some(x) => x.to_string(),
            None => "—".to_string(),
        };
        vec![
            (tr("行数（估算）").to_string(), n(self.rows)),
            (tr("占用空间").to_string(), match self.size_bytes {
                Some(b) => fmt_bytes(b),
                None => "—".to_string(),
            }),
            (tr("列数").to_string(), n(self.columns)),
            (tr("索引数").to_string(), n(self.indexes)),
            (tr("外键数").to_string(), n(self.foreign_keys)),
            (
                tr("类型").to_string(),
                if self.engine.is_empty() {
                    "—".to_string()
                } else {
                    self.engine.clone()
                },
            ),
            (
                tr("排序规则").to_string(),
                if self.collation.is_empty() {
                    "—".to_string()
                } else {
                    self.collation.clone()
                },
            ),
            (
                tr("字符集").to_string(),
                if self.charset.is_empty() {
                    "—".to_string()
                } else {
                    self.charset.clone()
                },
            ),
            (
                tr("创建时间").to_string(),
                if self.created.is_empty() {
                    "—".to_string()
                } else {
                    self.created.clone()
                },
            ),
            (
                tr("自增当前值").to_string(),
                if self.auto_increment.is_empty() {
                    "—".to_string()
                } else {
                    self.auto_increment.clone()
                },
            ),
            (
                tr("注释").to_string(),
                if self.comment.is_empty() {
                    "—".to_string()
                } else {
                    self.comment.clone()
                },
            ),
        ]
    }
}

/// 字节数写成人类看得懂的样子（Navicat 显示的是「12.3 MB」这种）。
pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 查一个表/视图的属性（**方言分派**：两家的信息源根本不同）。
///
/// **列序两家一致**（10 列，按位置取）：行数 / 字节数 / 注释 / 列数 /
/// 索引数 / 外键数 / 类型 / 排序规则 / 创建时间 / 自增值。取不到的那几列
/// 给空串——PG 没有排序规则、没有创建时间、也没有"自增"这个概念
/// （它是序列，语义还不一样，硬凑反而误导）。
pub fn meta_object_stats(proto: DbProto, schema: &str, table: &str) -> String {
    let s = quote_str(proto, schema);
    let t = quote_str(proto, table);
    match proto {
        DbProto::Postgres => format!("SELECT GREATEST(c.reltuples, 0)::bigint, \
                    pg_total_relation_size(c.oid), \
                    COALESCE(obj_description(c.oid, 'pg_class'), ''), \
                    (SELECT count(*) FROM information_schema.columns col \
                      WHERE col.table_schema = n.nspname AND col.table_name = c.relname), \
                    (SELECT count(*) FROM pg_index i WHERE i.indrelid = c.oid), \
                    (SELECT count(*) FROM pg_constraint k \
                      WHERE k.conrelid = c.oid AND k.contype = 'f'), \
                    CASE c.relkind WHEN 'r' THEN 'BASE TABLE' WHEN 'p' THEN 'PARTITIONED TABLE' \
                         WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' \
                         ELSE c.relkind::text END, \
                    '', '', '' \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {s} AND c.relname = {t}",
            s = s,
            t = t
        ),
        DbProto::MySql => format!("SELECT t.TABLE_ROWS, \
                    COALESCE(t.DATA_LENGTH,0) + COALESCE(t.INDEX_LENGTH,0), \
                    COALESCE(t.TABLE_COMMENT,''), \
                    (SELECT count(*) FROM information_schema.columns c \
                      WHERE c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME), \
                    (SELECT count(DISTINCT s.INDEX_NAME) FROM information_schema.statistics s \
                      WHERE s.TABLE_SCHEMA = t.TABLE_SCHEMA AND s.TABLE_NAME = t.TABLE_NAME), \
                    (SELECT count(*) FROM information_schema.table_constraints tc \
                      WHERE tc.TABLE_SCHEMA = t.TABLE_SCHEMA AND tc.TABLE_NAME = t.TABLE_NAME \
                        AND tc.CONSTRAINT_TYPE = 'FOREIGN KEY'), \
                    COALESCE(t.ENGINE,''), COALESCE(t.TABLE_COLLATION,''), \
                    COALESCE(CAST(t.CREATE_TIME AS CHAR),''), \
                    COALESCE(CAST(t.AUTO_INCREMENT AS CHAR),'') \
             FROM information_schema.TABLES t \
             WHERE t.TABLE_SCHEMA = {s} AND t.TABLE_NAME = {t}",
            s = s,
            t = t
        ),
    }
}

/// 解析对象属性（列序与 `meta_object_stats` 一致）。
///
/// 没有行 = 对象不存在（表被删了 / 名字打错了）→ 返回 `None`，
/// 调用方据此给一句"对象不存在"，而不是显示一堆「—」的假属性。
pub fn parse_object_stats(rows: &[Vec<Option<String>>]) -> Option<ObjectStats> {
    let r = rows.first()?;
    let num = |i: usize| -> Option<u64> {
        r.get(i)
            .and_then(|c| c.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "NULL")
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| f.max(0.0) as u64)
    };
    let text = |i: usize| -> String {
        r.get(i)
            .and_then(|c| c.as_deref())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let collation = text(7);
    // MySQL 的排序规则名就是「字符集_排序规则」（`utf8mb4_0900_ai_ci`），
    // 前半段拆出来当字符集；PG 这一项本来就是空串，拆了也是空。
    let charset = collation
        .split_once('_')
        .map(|(a, _)| a.to_string())
        .unwrap_or_default();
    Some(ObjectStats {
        rows: num(0),
        size_bytes: num(1),
        comment: text(2),
        columns: num(3),
        indexes: num(4),
        foreign_keys: num(5),
        engine: text(6),
        collation,
        charset,
        created: text(8),
        auto_increment: text(9),
    })
}

/* ---------------- 表统计（对象树「详细模式」，阶段 51） ---------------- */

/// 一张表 / 视图的统计（对象树「详细模式」显示用）。
///
/// **行数是估算值**：PG 的 `reltuples` 与 MySQL 的 `TABLE_ROWS` 都是优化器
/// 统计出来的估值（PG 要 ANALYZE 之后才准），**不是 COUNT(\*) 的真值**——
/// 所以界面上带「≈」前缀，不糊弄用户。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableStat {
    /// 表名 / 视图名
    pub name: String,
    /// 行数（估算）
    pub rows: u64,
    /// 占用的字节数（表数据 + 索引）
    pub size_bytes: u64,
    /// 最后修改时间。
    ///
    /// **只有 MySQL 有**（`information_schema.TABLES.UPDATE_TIME`）；
    /// PostgreSQL 压根没有"表最后修改时间"这个概念（它只有 analyze 时间，
    /// 语义不是一回事），所以 PG 这一项恒为空串——**宁可空着也不编一个**。
    pub updated: String,
}

/// 一次查一个模式下**所有**表 / 视图的统计（阶段 51 的树「详细模式」）。
///
/// **为什么是"一次查一个模式"而不是"逐个表查"**：`meta_object_stats` 是
/// 一张表一条 SQL——一个模式下有 200 张表就是 200 次往返，慢得没法用。
/// 这里按模式**一次查完**，树里挨个填。
pub fn meta_table_stats(proto: DbProto, schema: &str) -> String {
    let s = quote_str(proto, schema);
    match proto {
        DbProto::Postgres => format!("SELECT c.relname, \
                    GREATEST(c.reltuples, 0)::bigint, \
                    CASE WHEN c.relkind IN ('r','p') THEN pg_total_relation_size(c.oid) ELSE 0 END, \
                    '' \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {s} AND c.relkind IN ('r','p','v','m')",
            s = s
        ),
        DbProto::MySql => format!("SELECT t.TABLE_NAME, \
                    COALESCE(t.TABLE_ROWS, 0), \
                    COALESCE(t.DATA_LENGTH, 0) + COALESCE(t.INDEX_LENGTH, 0), \
                    COALESCE(CAST(t.UPDATE_TIME AS CHAR), '') \
             FROM information_schema.TABLES t \
             WHERE t.TABLE_SCHEMA = {s}",
            s = s
        ),
    }
}

/// 解析表统计（列序与 `meta_table_stats` 一致）。
pub fn parse_table_stats(rows: &[Vec<Option<String>>]) -> Vec<TableStat> {
    rows.iter()
        .filter_map(|r| {
            let name = r
                .first()
                .and_then(|c| c.as_deref())
                .map(str::trim)
                .unwrap_or("");
            if name.is_empty() {
                return None;
            }
            let num = |i: usize| -> u64 {
                r.get(i)
                    .and_then(|c| c.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && *s != "NULL")
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|f| f.max(0.0) as u64)
                    .unwrap_or(0)
            };
            let updated = r
                .get(3)
                .and_then(|c| c.as_deref())
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            Some(TableStat {
                name: name.to_string(),
                rows: num(1),
                size_bytes: num(2),
                updated,
            })
        })
        .collect()
}

/// 行数写成紧凑的中文数字（`1,234` / `1.2 万` / `3.4 亿`）。
///
/// **为什么不直接打原始数字**：一亿行的表写成 "100000000" 占掉半列宽，
/// 树面板只有 330px，看数字比看表名还费劲。
pub fn fmt_rows(n: u64) -> String {
    // ⚠️ **单位按语言走**：中文用「万 / 亿」，英文用 K / M / B。
    //    硬套同一个除数会算错（1.2 万 = 12000，英文该写 12.0K，不是 1.2K）。
    if crate::i18n::lang() == crate::i18n::Lang::Zh {
        // i18n:keep
        // ⚠️ 「万 / 亿」是**中文单位**（英文分支走 K/M/B）⇒ 不是界面文案，
        //    不进翻译表。
        if n >= 100_000_000 {
            format!("{:.1} 亿", n as f64 / 100_000_000.0)
        } else if n >= 10_000 {
            format!("{:.1} 万", n as f64 / 10_000.0)
        } else {
            thousands(n)
        }
        // i18n:end
    } else if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        thousands(n)
    }
}

/// 这一行是不是**一个值都没填**（跳过这种行，不是错误）。
///
/// ⚠️ **不能拿中文去 `contains`**：错误文案是**翻译过的**，英文界面下
///    就认不出来 ⇒ 空行会被当成真错误报出去（用户会看到"改动有问题"）。
///    这里拿**当前语言下**的文案去比，两种语言都对。
pub fn is_empty_row_err(e: &str) -> bool {
    e.contains(tr("这一行还没有填任何值"))
}

/// 加千分位（`1234567` → `1,234,567`）。
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    let bytes = s.as_bytes();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

/// 树里「说明」列的文案：`表 · ≈1.2 万行 · 12.3 MB · 2026-10-20`。
///
/// - `base` = 对象种类（「表」/「视图」），保留在前面，用户一眼知道是什么；
/// - 行数带「≈」——是估算值，别让人当精确值用；
/// - 大小为 0 就不写（视图 / 空表写 `0 B` 没意义）；
/// - 更新时间只取**日期**部分（`2026-10-20 14:33:12` → `2026-10-20`），
///   列窄，带时分秒会被截成「2026-10-…」。
pub fn fmt_table_detail(base: &str, st: &TableStat) -> String {
    let mut s = String::from(base);
    s.push_str(" · ≈");
    s.push_str(&fmt_rows(st.rows));
    s.push_str(tr(" 行"));
    if st.size_bytes > 0 {
        s.push_str(" · ");
        s.push_str(&fmt_bytes(st.size_bytes));
    }
    if !st.updated.is_empty() {
        s.push_str(" · ");
        s.push_str(&st.updated.chars().take(10).collect::<String>());
    }
    s
}

/// 解析事件列表（列序与 `meta_events` 一致）。
///
/// 调度信息直接用 MySQL 的原文（`1 DAY`、`12 HOUR`…），不做翻译——
/// MySQL 的调度表达式种类多（`EVERY`、`AT`、`STARTS … ENDS …`），
/// 翻成中文反而会丢掉信息。
pub fn parse_events(rows: &[Vec<Option<String>>]) -> Vec<Event> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let status = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            let schedule = r.get(2).and_then(|c| c.clone()).unwrap_or_default();
            Some(Event {
                name,
                status,
                schedule,
            })
        })
        .collect()
}

/* ---------------- 对象全局搜索 ---------------- */

/// 搜索命中的对象种类。
///
/// **函数与存储过程分成两个种类**：PG 的同名重载（`f(int)` / `f(text)`）要靠
/// 身份参数区分，打开定义时得知道是函数还是过程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Table,
    View,
    Column,
    Function,
    Procedure,
    Trigger,
    Event,
}

impl SearchKind {
    /// 列表里显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Table => tr("表"),
            SearchKind::View => tr("视图"),
            SearchKind::Column => tr("字段"),
            SearchKind::Function => tr("函数"),
            SearchKind::Procedure => tr("存储过程"),
            SearchKind::Trigger => tr("触发器"),
            SearchKind::Event => tr("事件"),
        }
    }

    /// 从 SQL 里那个 kind 字面量解析（认不出来就当没命中）。
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "table" => Some(SearchKind::Table),
            "view" => Some(SearchKind::View),
            "column" => Some(SearchKind::Column),
            "function" => Some(SearchKind::Function),
            "procedure" => Some(SearchKind::Procedure),
            "trigger" => Some(SearchKind::Trigger),
            "event" => Some(SearchKind::Event),
            _ => None,
        }
    }
}

/// 一条搜索命中。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// 种类
    pub kind: SearchKind,
    /// 所属模式（PG）/ 库（MySQL）
    pub schema: String,
    /// 对象名
    pub name: String,
    /// 所属表（字段、触发器有；其余为空）
    pub owner: String,
    /// 补充说明：字段=类型，函数/过程=身份参数，触发器=时机，事件=调度
    pub extra: String,
}

/// 搜索结果条数上限。
///
/// **为什么要限**：搜一个常见列名（比如 `id`）可能命中全库几千行，全倒出来
/// 既慢又没法看；而且用户真正要的就是前几百条。
const SEARCH_LIMIT: usize = 500;

/// 搜索结果条数上限（给界面用：命中数到上限时要提示"可能还有更多"）。
pub fn search_limit() -> usize {
    SEARCH_LIMIT
}

/// 关键词 → `LIKE` 模式（前后各加一个 `%`），并转义模式里的特殊字符。
///
/// **为什么要转义**：用户搜 `50%` 时 `%` 会被当成通配符，搜出一堆不相干的；
/// `_` 同理（单字符通配符）。加反斜杠转义，两边都认（MySQL 的 `\` 默认是
/// LIKE 转义符；PG 的 `standard_conforming_strings` 开着时 `'\%'` 就是 `\%`）。
fn like_pattern(keyword: &str) -> String {
    let mut out = String::with_capacity(keyword.len() + 2);
    out.push('%');
    for ch in keyword.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// 全库搜对象（表 / 视图 / 字段 / 函数 / 存储过程 / 触发器 / 事件）。
///
/// **一条 SQL 出全部结果**：几类对象 `UNION ALL` 成一张固定 5 列的表
/// （`kind / schema / name / owner / extra`），前端只按位置取，不用分方言
/// 处理列序——跟 `meta_columns` 那套"两边列数保持一致"的做法一样。
///
/// **排除系统模式**：跟对象树一致（`meta_schemas` 把 `pg_catalog`、
/// `information_schema` 那些滤掉了）。不滤的话搜一个常见列名，会把
/// `information_schema` 自己那几百个字段全搜出来，用户根本看不过来。
///
/// **大小写不敏感**：PG 用 `ILIKE`，MySQL 的默认排序规则本来就是不分大小写。
pub fn meta_search_objects(proto: DbProto, keyword: &str) -> String {
    let kw = quote_str(proto, &like_pattern(keyword));
    match proto {
        DbProto::Postgres => format!("SELECT 'table' AS kind, n.nspname AS sch, c.relname AS nm, '' AS own, '' AS ext \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r','p') AND c.relname ILIKE {kw} \
               AND n.nspname NOT LIKE 'pg_%' AND n.nspname <> 'information_schema' \
             UNION ALL \
             SELECT 'view', n.nspname, c.relname, '', '' \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('v','m') AND c.relname ILIKE {kw} \
               AND n.nspname NOT LIKE 'pg_%' AND n.nspname <> 'information_schema' \
             UNION ALL \
             SELECT 'column', c.table_schema, c.column_name, c.table_name, c.data_type \
             FROM information_schema.columns c \
             WHERE c.column_name ILIKE {kw} \
               AND c.table_schema NOT LIKE 'pg_%' AND c.table_schema <> 'information_schema' \
             UNION ALL \
             SELECT 'function', n.nspname, p.proname, '', pg_get_function_identity_arguments(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.prokind = 'f' AND p.proname ILIKE {kw} \
               AND n.nspname NOT LIKE 'pg_%' AND n.nspname <> 'information_schema' \
             UNION ALL \
             SELECT 'procedure', n.nspname, p.proname, '', pg_get_function_identity_arguments(p.oid) \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.prokind = 'p' AND p.proname ILIKE {kw} \
               AND n.nspname NOT LIKE 'pg_%' AND n.nspname <> 'information_schema' \
             UNION ALL \
             SELECT 'trigger', t.trigger_schema, t.trigger_name, t.event_object_table, \
                    concat(t.action_timing, '/', t.event_manipulation) \
             FROM information_schema.triggers t \
             WHERE t.trigger_name ILIKE {kw} \
               AND t.trigger_schema NOT LIKE 'pg_%' AND t.trigger_schema <> 'information_schema' \
             ORDER BY 1, 2, 3 LIMIT {SEARCH_LIMIT}"
        ),
        DbProto::MySql => format!("SELECT 'table' AS kind, table_schema AS sch, table_name AS nm, '' AS own, '' AS ext \
             FROM information_schema.TABLES \
             WHERE table_type = 'BASE TABLE' AND table_name LIKE {kw} \
               AND table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'view', table_schema, table_name, '', '' \
             FROM information_schema.TABLES \
             WHERE table_type = 'VIEW' AND table_name LIKE {kw} \
               AND table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'column', table_schema, column_name, table_name, column_type \
             FROM information_schema.COLUMNS \
             WHERE column_name LIKE {kw} \
               AND table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'function', routine_schema, routine_name, '', '' \
             FROM information_schema.ROUTINES \
             WHERE routine_type = 'FUNCTION' AND routine_name LIKE {kw} \
               AND routine_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'procedure', routine_schema, routine_name, '', '' \
             FROM information_schema.ROUTINES \
             WHERE routine_type = 'PROCEDURE' AND routine_name LIKE {kw} \
               AND routine_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'trigger', trigger_schema, trigger_name, event_object_table, \
                    concat(action_timing, '/', event_manipulation) \
             FROM information_schema.TRIGGERS \
             WHERE trigger_name LIKE {kw} \
               AND trigger_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             UNION ALL \
             SELECT 'event', event_schema, event_name, '', \
                    trim(concat(ifnull(interval_value,''), ' ', ifnull(interval_field,''))) \
             FROM information_schema.EVENTS \
             WHERE event_name LIKE {kw} \
               AND event_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
             ORDER BY 1, 2, 3 LIMIT {SEARCH_LIMIT}"
        ),
    }
}

/// 解析搜索结果（列序与 `meta_search_objects` 一致）。
pub fn parse_search_objects(rows: &[Vec<Option<String>>]) -> Vec<SearchHit> {
    rows.iter()
        .filter_map(|r| {
            let kind = r
                .first()
                .and_then(|c| c.as_deref())
                .and_then(SearchKind::from_code)?;
            let name = r.get(2).and_then(|c| c.clone()).unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            Some(SearchHit {
                kind,
                schema: r.get(1).and_then(|c| c.clone()).unwrap_or_default(),
                name,
                owner: r.get(3).and_then(|c| c.clone()).unwrap_or_default(),
                extra: r.get(4).and_then(|c| c.clone()).unwrap_or_default(),
            })
        })
        .collect()
}

/// 解析例程列表（列序与 `meta_routines` 一致）。
pub fn parse_routines(rows: &[Vec<Option<String>>]) -> Vec<Routine> {

    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let kind = RoutineKind::from_code(r.get(1).and_then(|c| c.as_deref()).unwrap_or("F"));
            let args = r.get(2).and_then(|c| c.clone()).unwrap_or_default();
            let returns = r.get(3).and_then(|c| c.clone()).unwrap_or_default();
            Some(Routine {
                name,
                kind,
                args,
                returns,
            })
        })
        .collect()
}

/// 定义的种类（决定"执行定义"前要不要先 DROP）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    /// 函数
    Function,
    /// 存储过程
    Procedure,
    /// 触发器
    Trigger,
    /// 事件（定时任务）
    Event,
}

impl DefKind {
    /// 这一类对象的统称（页面标题用）。
    pub fn label(self) -> &'static str {
        match self {
            DefKind::Function => tr("函数"),
            DefKind::Procedure => tr("存储过程"),
            DefKind::Trigger => tr("触发器"),
            DefKind::Event => tr("事件"),
        }
    }
}

/// 去掉 MySQL `SHOW CREATE` 输出里的 `DEFINER=xxx@yyy`。
///
/// **为什么要去掉**：MySQL 8 的 `SHOW CREATE FUNCTION` 会带上建函数的账号，
/// 换个连接身份执行时没有那个账号的权限就会直接失败（`SUPER` 权限）。
/// 定义本身不依赖 DEFINER，去掉它反而能正常执行。
pub fn strip_definer(sql: &str) -> String {
    let Some(i) = sql.find("DEFINER=") else {
        return sql.to_string();
    };
    let rest = &sql[i + "DEFINER=".len()..];
    // DEFINER 的值里没有空格（`` `root`@`localhost` ``），到第一个空白为止就是它
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let mut out = String::with_capacity(sql.len());
    out.push_str(&sql[..i]);
    // 连后面的空白一起吃掉，免得留下 "CREATE  FUNCTION" 的双空格
    out.push_str(rest[end..].trim_start());
    out
}

/// 把定义文本变成"可以逐条执行"的语句列表。
///
/// **为什么返回列表而不是拼成一条带分号的 SQL**：
/// - 函数/存储过程的体里全是 `;`（`BEGIN … ; END`），按分号拆会把体拆坏；
/// - MySQL 的多语句协议在服务端就按 `;` 切分，`CREATE PROCEDURE` 的体
///   同样会被切坏（`delimiter` 是**客户端**的事，服务端不认）。
///
/// 所以改成"每条消息一条完整语句"，由前端按顺序逐条发：
/// - PG 的 `pg_get_functiondef` 出来就是 `CREATE OR REPLACE FUNCTION …`，
///   本身幂等，不用 DROP；
/// - 触发器在两家都要先 `DROP TRIGGER IF EXISTS`（`CREATE TRIGGER` 不覆盖同名）；
/// - MySQL 的例程没有 `OR REPLACE`，也得先 `DROP … IF EXISTS`。
pub fn prepare_def_exec(
    proto: DbProto,
    kind: DefKind,
    schema: &str,
    table: &str,
    name: &str,
    text: &str,
) -> Vec<String> {
    let body = match proto {
        DbProto::Postgres => text.trim().to_string(),
        DbProto::MySql => strip_definer(text).trim().to_string(),
    };
    let drop = match (proto, kind) {
        // PG 的 CREATE OR REPLACE 已经幂等，不用先删
        (DbProto::Postgres, DefKind::Function | DefKind::Procedure) => None,
        (DbProto::Postgres, DefKind::Trigger) => Some(format!("DROP TRIGGER IF EXISTS {} ON {}",
            quote_ident(proto, name),
            quote_qualified(proto, schema, table)
        )),
        (DbProto::MySql, DefKind::Function) => Some(format!("DROP FUNCTION IF EXISTS {}",
            quote_ident(proto, name)
        )),
        (DbProto::MySql, DefKind::Procedure) => Some(format!("DROP PROCEDURE IF EXISTS {}",
            quote_ident(proto, name)
        )),
        (DbProto::MySql, DefKind::Trigger) => Some(format!("DROP TRIGGER IF EXISTS {}",
            quote_ident(proto, name)
        )),
        // 事件：MySQL 的 `CREATE EVENT` 不覆盖同名 → 得先删。
        // （PG 压根没有定时事件，走不到这一支；留着只为 match 穷尽）
        (DbProto::Postgres, DefKind::Event) => None,
        (DbProto::MySql, DefKind::Event) => Some(format!("DROP EVENT IF EXISTS {}",
            quote_qualified(proto, schema, name)
        )),
    };
    match drop {
        Some(d) => vec![d, body],
        None => vec![body],
    }
}

/// 一个触发器。
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub name: String,
    /// 挂在哪张表上
    pub table: String,
    /// BEFORE / AFTER / INSTEAD OF
    pub timing: String,
    /// INSERT / UPDATE / DELETE / TRUNCATE
    pub event: String,
}

/// 解析触发器列表（列序与 `meta_triggers` 一致）。
pub fn parse_triggers(rows: &[Vec<Option<String>>]) -> Vec<Trigger> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            Some(Trigger {
                name,
                table: r.get(1).and_then(|c| c.clone()).unwrap_or_default(),
                timing: r.get(2).and_then(|c| c.clone()).unwrap_or_default(),
                event: r.get(3).and_then(|c| c.clone()).unwrap_or_default(),
            })
        })
        .collect()
}

/* ---------------- 结果解析 ---------------- */

/// 把元数据查询返回的文本行解析成结构。
///
/// 元数据查询的列顺序是上面 SQL 里定的，这里按位置取——比"按列名找"更省
/// 依赖（不同方言的列名大小写/别名不一致）。
pub fn parse_schemas(rows: &[Vec<Option<String>>]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| r.first().and_then(|c| c.clone()))
        .filter(|s| !s.is_empty())
        .collect()
}

/// 表：`(名称, 是否视图)`。
pub fn parse_tables(rows: &[Vec<Option<String>>]) -> Vec<(String, bool)> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let is_view = r
                .get(1)
                .and_then(|c| c.as_deref())
                .map(|t| t.eq_ignore_ascii_case("VIEW"))
                .unwrap_or(false);
            Some((name, is_view))
        })
        .collect()
}

/// 列：`(名称, 类型名, 是否可空, 是否主键)`。
pub fn parse_columns(rows: &[Vec<Option<String>>]) -> Vec<(String, String, bool, bool)> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let ty = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            let nullable = r
                .get(2)
                .and_then(|c| c.as_deref())
                .map(|v| v.eq_ignore_ascii_case("YES"))
                .unwrap_or(true);
            let is_pk = r
                .get(4)
                .and_then(|c| c.as_deref())
                .map(|v| matches!(v.trim(), "t" | "true" | "1" | "TRUE" | "T"))
                .unwrap_or(false);
            Some((name, ty, nullable, is_pk))
        })
        .collect()
}

/// 列的完整定义（表结构页签用）。
///
/// 与 `parse_columns` 的区别：多带默认值、PG 自定义类型名、长度与精度——
/// 拼 `CREATE TABLE` 时缺了这些就会把表结构拼错（少了默认值，导出的 DDL
/// 一执行就把默认值弄没了）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub is_pk: bool,
    /// 默认值表达式原文（没有则 None）
    pub default_value: Option<String>,
    /// PG 的自定义类型名（`data_type` 为 `USER-DEFINED` 时用）
    pub udt_name: Option<String>,
    /// 字符类型长度（`varchar(50)` 的 50）
    pub char_len: Option<i64>,
    /// 数值精度（`numeric(10,2)` 的 10）
    pub num_precision: Option<i64>,
    /// 小数位（`numeric(10,2)` 的 2）
    pub num_scale: Option<i64>,
    /// 主键约束名（PG 改主键要靠它 `DROP CONSTRAINT`；MySQL 是 `PRIMARY`）
    ///
    /// **为什么单独查**：主键约束名**不保证**是 `<表名>_pkey`（用户可以改名），
    /// 按约定拼名字去 DROP 会报「约束不存在」。`information_schema.
    /// table_constraints` 两家都有这个列，直接问服务器最稳。
    pub pk_constraint: Option<String>,
}

/// 解析列定义（列序与 `meta_columns` 一致）。
pub fn parse_column_defs(rows: &[Vec<Option<String>>]) -> Vec<ColumnDef> {
    let num = |r: &Vec<Option<String>>, i: usize| -> Option<i64> {
        r.get(i)
            .and_then(|c| c.as_deref())
            .map(|s| s.trim().parse::<i64>().ok())
            .flatten()
    };
    let text = |r: &Vec<Option<String>>, i: usize| -> Option<String> {
        r.get(i)
            .and_then(|c| c.as_deref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let data_type = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            let nullable = r
                .get(2)
                .and_then(|c| c.as_deref())
                .map(|v| v.eq_ignore_ascii_case("YES"))
                .unwrap_or(true);
            let is_pk = r
                .get(4)
                .and_then(|c| c.as_deref())
                .map(|v| matches!(v.trim(), "t" | "true" | "1" | "TRUE" | "T"))
                .unwrap_or(false);
            Some(ColumnDef {
                name,
                data_type,
                nullable,
                is_pk,
                default_value: text(r, 5),
                udt_name: text(r, 6),
                char_len: num(r, 7),
                num_precision: num(r, 8),
                num_scale: num(r, 9),
                pk_constraint: text(r, 10),
            })
        })
        .collect()
}

/// 外键。
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignKey {
    /// 约束名
    pub name: String,
    /// 本表的列
    pub column: String,
    /// 引用的表
    pub ref_table: String,
    /// 引用的列
    pub ref_column: String,
}

/// 解析外键（列序与 `meta_foreign_keys` 一致）。
pub fn parse_foreign_keys(rows: &[Vec<Option<String>>]) -> Vec<ForeignKey> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            let column = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            let ref_table = r.get(2).and_then(|c| c.clone()).unwrap_or_default();
            let ref_column = r.get(3).and_then(|c| c.clone()).unwrap_or_default();
            if name.is_empty() || column.is_empty() {
                return None;
            }
            Some(ForeignKey {
                name,
                column,
                ref_table,
                ref_column,
            })
        })
        .collect()
}

/// 索引：`(名称, 定义/列)`。
pub fn parse_indexes(rows: &[Vec<Option<String>>]) -> Vec<(String, String)> {
    rows.iter()
        .filter_map(|r| {
            let name = r.first().and_then(|c| c.clone())?;
            if name.is_empty() {
                return None;
            }
            let def = r.get(1).and_then(|c| c.clone()).unwrap_or_default();
            Some((name, def))
        })
        .collect()
}

/* ---------------- 表结构 / DDL ---------------- */

/// 一个列在 `CREATE TABLE` 里的类型写法。
///
/// `information_schema` 的 `data_type` 不能直接照抄：
/// - PG 的 `character varying` 不带长度（长度在 `character_maximum_length`），
///   `timestamp without time zone` 也不是 MySQL 的写法；
/// - PG 的自定义类型 `data_type` 写成 `USER-DEFINED`，真名在 `udt_name`；
/// - 数组类型的 `udt_name` 带下划线前缀（`_int4`），要还原成 `int4[]`。
pub fn ddl_type(c: &ColumnDef) -> String {
    let t = c.data_type.trim().to_ascii_lowercase();
    let base = match t.as_str() {
        "character varying" => "varchar".to_string(),
        "character" => "char".to_string(),
        "timestamp without time zone" => "timestamp".to_string(),
        "time without time zone" => "time".to_string(),
        "timestamp with time zone" => "timestamptz".to_string(),
        "time with time zone" => "timetz".to_string(),
        "user-defined" => c.udt_name.clone().unwrap_or_else(|| "text".to_string()),
        "array" => {
            let udt = c.udt_name.clone().unwrap_or_else(|| "text".to_string());
            // PG 的数组类型名带 '_' 前缀（_int4 → int4[]）
            if let Some(stripped) = udt.strip_prefix('_') {
                format!("{stripped}[]")
            } else {
                format!("{udt}[]")
            }
        }
        _ => t.clone(),
    };
    if matches!(base.as_str(), "varchar" | "char") {
        if let Some(n) = c.char_len {
            return format!("{base}({n})");
        }
    }
    // 只有数值类型才用精度/小数位：MySQL 的整数列 NUMERIC_PRECISION 是显示
    // 宽度（int → 10），照抄会写成 int(10)。
    if matches!(base.as_str(), "numeric" | "decimal") {
        match (c.num_precision, c.num_scale) {
            (Some(p), Some(s)) => return format!("{base}({p},{s})"),
            (Some(p), None) => return format!("{base}({p})"),
            _ => {}
        }
    }
    base
}

/* ---------------- 表结构改动（阶段 14：表设计器改结构） ---------------- */

/// 设计器里的一列（用户看到的目标状态）。
///
/// **为什么要有这个结构**：设计器的字段表是「行 = 列」的网格，用户能改名字、
/// 类型、可空、主键、默认值。生成 ALTER 时要把「原来的列」和「现在的列」
/// 对上号——**靠行号对，不靠名字**：用户把名字改了以后就按名字对不上了，而
/// 行号不会变（新增的列排在末尾，删掉的列不在列表里）。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSpec {
    /// `Some` = 原来是第几列（`information_schema` 的顺序）；`None` = 新加的列
    pub orig_index: Option<usize>,
    /// 列名
    pub name: String,
    /// 类型写法（`integer` / `varchar(50)`）
    pub ty: String,
    /// 可空
    pub nullable: bool,
    /// 主键
    pub is_pk: bool,
    /// 默认值表达式原文（`None` = 没有默认值）
    pub default_value: Option<String>,
}

/// 改结构的执行计划。
#[derive(Debug, Clone, PartialEq)]
pub struct AlterPlan {
    /// 执行用的语句（一条 `ALTER TABLE`）
    pub sql: String,
    /// 危险操作的中文说明（**空 = 没有危险操作**，可以静默执行）
    ///
    /// 为什么单独列出来：删列、改类型这类操作**可能丢数据**，界面上要先弹
    /// 二次确认。判断放在这一层，界面就只负责"有风险就问一句"。
    pub risks: Vec<String>,
}

/// 把结构改动拼成**一条** `ALTER TABLE` 语句。
///
/// **为什么合成一条**：PG 与 MySQL 都支持一条 `ALTER TABLE` 里写多个动作
/// （`ALTER TABLE t ADD COLUMN …, DROP COLUMN …`）。合成一条有两个好处：
/// ① **原子**——PG 的 DDL 跑在事务里，MySQL 8.0 是原子 DDL，要么全成功要么
/// 全不动，不会出现"加了列却没删掉列"的半截结构；
/// ② 回包处理简单（一条语句一个回包，不用写"发下一条"的异步状态机）。
///
/// 没有改动时返回 `Ok(None)`——调用方不该发语句。
///
/// **不在这里包事务**：一条 ALTER 本身就是原子的（PG 隐式事务、MySQL 原子
/// DDL），再套一层 BEGIN/COMMIT 反而多两条语句、多一个失败点。
pub fn build_alter_table(
    proto: DbProto,
    schema: &str,
    table: &str,
    orig: &[ColumnDef],
    target: &[ColumnSpec],
) -> Result<Option<AlterPlan>, String> {
    // ---- 1) 校验目标状态：有毛病就别发语句（改了一半比不改更糟）----
    let mut seen: Vec<&str> = Vec::new();
    for t in target {
        check_ident(&t.name)?;
        check_type(&t.ty)?;
        if let Some(d) = t.default_value.as_deref() {
            check_default(d)?;
        }
        if seen.contains(&t.name.as_str()) {
            return Err(trf!("列名「{v0}」重复了", v0 = t.name));
        }
        seen.push(&t.name);
    }

    // ---- 2) 哪些原列被删了 ----
    let kept: Vec<usize> = target.iter().filter_map(|t| t.orig_index).collect();
    for &i in &kept {
        if i >= orig.len() {
            return Err(tr("结构数据对不上（列号越界），请重新打开表结构").into());
        }
    }
    let dropped: Vec<&ColumnDef> = orig
        .iter()
        .enumerate()
        .filter(|(i, _)| !kept.contains(i))
        .map(|(_, c)| c)
        .collect();

    // ---- 3) 主键变没变（按「原来的第几列」比，不按名字）----
    //
    // 按名字比是错的：用户只给主键列改了个名字，PG 的 `RENAME COLUMN` 会
    // 自动把约束里的列名一起改掉，主键**没变**，不该重建约束。
    let mut orig_pk: Vec<usize> = orig
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_pk)
        .map(|(i, _)| i)
        .collect();
    orig_pk.sort_unstable();
    let mut new_pk: Vec<usize> = Vec::new();
    let mut pk_added = false;
    for t in target {
        if t.is_pk {
            match t.orig_index {
                Some(i) => new_pk.push(i),
                None => pk_added = true,
            }
        }
    }
    new_pk.sort_unstable();
    let pk_changed = orig_pk != new_pk || pk_added;
    // 新主键的列名（按 target 里的顺序）
    let new_pk_names: Vec<String> = target.iter().filter(|t| t.is_pk).map(|t| t.name.clone()).collect();

    let mut acts: Vec<String> = Vec::new();
    let mut risks: Vec<String> = Vec::new();

    match proto {
        DbProto::Postgres => {
            for c in &dropped {
                acts.push(format!("DROP COLUMN {}", quote_ident(proto, &c.name)));
                risks.push(trf!("删列「{v0}」——列里的数据会一起没了，撤不回来", v0 = c.name));
            }
            for t in target {
                let q = quote_ident(proto, &t.name);
                match t.orig_index {
                    None => {
                        acts.push(format!("ADD COLUMN {q} {}{}", t.ty.trim(), col_attrs(t)));
                    }
                    Some(i) => {
                        let o = &orig[i];
                        if o.name != t.name {
                            acts.push(format!("RENAME COLUMN {} TO {}",
                                quote_ident(proto, &o.name),
                                q
                            ));
                        }
                        if !same_type(&ddl_type(o), &t.ty) {
                            acts.push(format!("ALTER COLUMN {q} TYPE {}", t.ty.trim()));
                            risks.push(trf!("改列「{v0}」的类型（{v1} → {v2}）——现有数据转不过去语句会失败，转过去了也可能被截断", v0 = t.name, v1 = ddl_type(o), v2 = t.ty.trim()));
                        }
                        if o.nullable != t.nullable {
                            acts.push(if t.nullable {
                                format!("ALTER COLUMN {q} DROP NOT NULL")
                            } else {
                                format!("ALTER COLUMN {q} SET NOT NULL")
                            });
                            if !t.nullable {
                                risks.push(trf!("把列「{v0}」改成 NOT NULL——现有 NULL 值会让语句失败", v0 = t.name));
                            }
                        }
                        if !same_default(o.default_value.as_deref(), t.default_value.as_deref()) {
                            match t.default_value.as_deref() {
                                Some(d) => {
                                    acts.push(format!("ALTER COLUMN {q} SET DEFAULT {d}"))
                                }
                                None => acts.push(format!("ALTER COLUMN {q} DROP DEFAULT")),
                            }
                        }
                    }
                }
            }
            if pk_changed {
                // 主键约束名**必须问服务器**：不保证是 `<表名>_pkey`（用户可以
                // 改名），按约定拼名字去 DROP 会报「约束不存在」。
                if let Some(name) = orig.iter().find_map(|c| c.pk_constraint.clone()) {
                    acts.push(format!("DROP CONSTRAINT {}", quote_ident(proto, &name)));
                }
                if !new_pk_names.is_empty() {
                    acts.push(format!("ADD PRIMARY KEY ({})",
                        new_pk_names
                            .iter()
                            .map(|n| quote_ident(proto, n))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                risks.push(tr("改主键——引用这张表的外键可能因此失效").to_string());
            }
        }
        DbProto::MySql => {
            for c in &dropped {
                acts.push(format!("DROP COLUMN {}", quote_ident(proto, &c.name)));
                risks.push(trf!("删列「{v0}」——列里的数据会一起没了，撤不回来", v0 = c.name));
            }
            for t in target {
                let q = quote_ident(proto, &t.name);
                match t.orig_index {
                    None => {
                        acts.push(format!("ADD COLUMN {q} {}{}", t.ty.trim(), col_attrs(t)));
                    }
                    Some(i) => {
                        let o = &orig[i];
                        let renamed = o.name != t.name;
                        let type_changed = !same_type(&ddl_type(o), &t.ty);
                        let null_changed = o.nullable != t.nullable;
                        let def_changed =
                            !same_default(o.default_value.as_deref(), t.default_value.as_deref());
                        if renamed || type_changed || null_changed || def_changed {
                            // ⚠️ MySQL 的 MODIFY / CHANGE 要写**完整定义**：
                            // 只写类型的话，没提到的属性（NOT NULL / DEFAULT）
                            // 会被一起清掉。所以这里给的是完整的一列。
                            let verb = if renamed {
                                "CHANGE COLUMN"
                            } else {
                                "MODIFY COLUMN"
                            };
                            let target_ref = if renamed {
                                format!("{} {}", quote_ident(proto, &o.name), q)
                            } else {
                                q.clone()
                            };
                            acts.push(format!("{verb} {target_ref} {}{}", t.ty.trim(), col_attrs(t)));
                            if type_changed {
                                risks.push(trf!("改列「{v0}」的类型（{v1} → {v2}）——现有数据转不过去语句会失败，转过去了也可能被截断", v0 = t.name, v1 = ddl_type(o), v2 = t.ty.trim()));
                            }
                            if null_changed && !t.nullable {
                                risks.push(trf!("把列「{v0}」改成 NOT NULL——现有 NULL 值会让语句失败", v0 = t.name));
                            }
                        }
                    }
                }
            }
            if pk_changed {
                // MySQL 的主键没有名字，就是 `PRIMARY`。
                acts.push("DROP PRIMARY KEY".to_string());
                if !new_pk_names.is_empty() {
                    acts.push(format!("ADD PRIMARY KEY ({})",
                        new_pk_names
                            .iter()
                            .map(|n| quote_ident(proto, n))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                risks.push(tr("改主键——引用这张表的外键可能因此失效").to_string());
            }
        }
    }

    if acts.is_empty() {
        return Ok(None);
    }
    Ok(Some(AlterPlan {
        sql: format!("ALTER TABLE {} {}",
            quote_qualified(proto, schema, table),
            acts.join(", ")
        ),
        risks,
    }))
}

/// 一列的属性（可空 + 默认值）。
///
/// PG 与 MySQL 都吃这一套：`ADD COLUMN x integer NOT NULL DEFAULT 0`。
fn col_attrs(t: &ColumnSpec) -> String {
    let mut s = String::new();
    if t.nullable {
        s.push_str(" NULL");
    } else {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = t.default_value.as_deref() {
        let d = d.trim();
        if !d.is_empty() {
            s.push_str(&format!(" DEFAULT {d}"));
        }
    }
    s
}

/// 列名校验：只放行「普通标识符」。
///
/// **为什么不直接拼进 SQL**：列名是用户在这个网格里敲的字，敲个
/// `x) ; DROP TABLE users --` 拼进语句就是注入。标识符一律只允许字母、
/// 数字、下划线和 `$`，且不能以数字开头。
fn check_ident(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(tr("列名不能为空").into());
    }
    // PG 标识符上限 63 字节，MySQL 64 —— 取小的那个
    if n.len() > 63 {
        return Err(trf!("列名「{n}」太长（最多 63 个字符）", n = n));
    }
    let mut chars = n.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(trf!("列名「{n}」不合法：要以字母或下划线开头", n = n));
    }
    for c in n.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '$') {
            return Err(trf!("列名「{n}」不合法：只能有字母、数字、下划线和 $", n = n));
        }
    }
    Ok(())
}

/// 类型名校验：类型是裸 SQL 片段，只放行「标识符 + 长度/精度 + 数组后缀」。
fn check_type(ty: &str) -> Result<(), String> {
    let t = ty.trim();
    if t.is_empty() {
        return Err(tr("类型不能为空").into());
    }
    if t.len() > 64 {
        return Err(tr("类型写法太长").into());
    }
    for c in t.chars() {
        if c.is_ascii_alphanumeric()
            || c == '_'
            || c == ' '
            || c == '('
            || c == ')'
            || c == ','
            || c == '.'
            || c == '['
            || c == ']'
            || c == '"'
        {
            continue;
        }
        return Err(trf!("类型「{t}」里有不认识的字符「{c}」", t = t, c = c));
    }
    Ok(())
}

/// 默认值校验：是表达式原文（`now()`、`'x'`），但不能带分号。
///
/// **为什么禁分号**：默认值会原样拼进 `SET DEFAULT …`。带分号就等于允许
/// 在一条 ALTER 里塞第二条语句。一条语句只准改一样东西。
fn check_default(d: &str) -> Result<(), String> {
    let t = d.trim();
    if t.is_empty() {
        return Ok(());
    }
    if t.len() > 200 {
        return Err(tr("默认值太长").into());
    }
    if t.contains(';') {
        return Err(tr("默认值里不能有分号（一条语句只准一个默认值）").into());
    }
    Ok(())
}

/// 类型写法比较：忽略大小写与空格。
///
/// `double precision` 与 `doubleprecision`、`VARCHAR(50)` 与 `varchar(50)`
/// 都当成同一个类型，不然用户什么都没改也会被生成一条 ALTER。
fn same_type(a: &str, b: &str) -> bool {
    let norm = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// 默认值比较：按原文（去首尾空白）比。
fn same_default(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => x.trim() == y.trim(),
        _ => false,
    }
}

/// 按列定义拼 `CREATE TABLE`（表结构页签展示的 DDL）。
///
/// **自己拼**而不是用各家的私有函数：PG 没有 `SHOW CREATE TABLE`，硬要用
/// `pg_get_tabledef` 这类版本相关的函数，一旦升级就坏。information_schema 是
/// 两家都有、也最稳的一层。
///
/// 拼不出来的部分（自增、列注释、CHECK 约束）会在页签里写明——宁可说"没
/// 拼出来"，也别拼个错的 DDL 让人执行。
pub fn build_ddl(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[ColumnDef],
    fks: &[ForeignKey],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in columns {
        let mut line = format!("  {} {}", quote_ident(proto, &c.name), ddl_type(c));
        if !c.nullable {
            line.push_str(" NOT NULL");
        }
        if let Some(d) = c.default_value.as_deref() {
            // PG 的自增默认值是 nextval('...')，本身就是合法表达式，照抄即可
            line.push_str(&format!(" DEFAULT {d}"));
        }
        parts.push(line);
    }
    let pk: Vec<String> = columns
        .iter()
        .filter(|c| c.is_pk)
        .map(|c| quote_ident(proto, &c.name))
        .collect();
    if !pk.is_empty() {
        parts.push(format!("  CONSTRAINT {} PRIMARY KEY ({})",
            quote_ident(proto, &format!("{table}_pkey")),
            pk.join(", ")
        ));
    }
    for fk in fks {
        parts.push(format!("  CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
            quote_ident(proto, &fk.name),
            quote_ident(proto, &fk.column),
            quote_ident(proto, &fk.ref_table),
            quote_ident(proto, &fk.ref_column)
        ));
    }
    format!("CREATE TABLE {} (\n{}\n);",
        quote_qualified(proto, schema, table),
        parts.join(",\n")
    )
}

/// 构造 INSERT（插入一行新记录）。
///
/// `values` 是**每列的输入文本**（`None` = 用户没碰过这一格）：
/// - `None` → 这一列**不写进语句**，交给数据库用默认值（自增 id、默认时间…）；
/// - `Some(text)` → 按列类型转成字面量（`NULL` 文本 = SQL NULL，空串 = `''`）。
///
/// **没填的列为什么不写**：硬塞一个空串会把数据库的默认值吃掉
/// （`created_at DEFAULT now()` 就变成空串了）。用户只想填两列时，
/// 其余列该走默认值。
///
/// 所有列都没填 → 返回错误（这一行等于没填，不该插一条全空的记录）。
pub fn build_insert(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[String],
    types: &[ColType],
    values: &[Option<String>],
) -> Result<String, String> {
    let mut cols: Vec<String> = Vec::new();
    let mut vals: Vec<String> = Vec::new();
    for (i, name) in columns.iter().enumerate() {
        let Some(Some(text)) = values.get(i) else {
            continue;
        };
        let ty = types.get(i).copied().unwrap_or(ColType::Text);
        let lit = edit_literal(proto, ty, text).map_err(|e| trf!("「{name}」：{e}", name = name, e = e))?;
        cols.push(quote_ident(proto, name));
        vals.push(lit);
    }
    if cols.is_empty() {
        return Err(tr("这一行还没有填任何值").to_string());
    }
    Ok(format!("INSERT INTO {} ({}) VALUES ({})",
        quote_qualified(proto, schema, table),
        cols.join(", "),
        vals.join(", ")
    ))
}

/// 把结果集导成 INSERT 语句脚本（Navicat 的 "Export SQL"）。
///
/// 值按列类型转义：数字列写数字、布尔写 TRUE/FALSE、文本加引号转义；
/// NULL 写 `NULL`。列名与表名按方言引用。
pub fn build_inserts(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[String],
    types: &[ColType],
    rows: &[Vec<Option<String>>],
) -> String {
    if columns.is_empty() || rows.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&trf!("-- 从 PuTTYPlus 导出的数据（{v0} 行）\n", v0 = rows.len()));
    for stmt in build_insert_rows(proto, schema, table, columns, types, rows) {
        out.push_str(&stmt);
        out.push('\n');
    }
    out
}

/// 生成**逐行**的 INSERT 语句（不带导出脚本的注释头）。
///
/// **为什么要拆出这个函数**：导出（`build_inserts`）与**导入**（阶段 18）
/// 必须用**同一套转义**，否则"导出再导入"会对不上——值在导出时被转义了
/// 一次，导入时又转义一次，`\` 和 `'` 会被转两次（MySQL 尤其明显）。
///
/// **注意与导出的差别**：这里遇到类型对不上的值**直接报错**（导入不能把
/// 脏数据写进库）；导出那边是"当文本写，别把整份脚本丢掉"。
pub fn build_insert_rows(
    proto: DbProto,
    schema: &str,
    table: &str,
    columns: &[String],
    types: &[ColType],
    rows: &[Vec<Option<String>>],
) -> Vec<String> {
    let mut out = Vec::new();
    if columns.is_empty() {
        return out;
    }
    let cols: Vec<String> = columns.iter().map(|c| quote_ident(proto, c)).collect();
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, v)| match v {
                None => "NULL".to_string(),
                Some(s) => {
                    let ty = types.get(i).copied().unwrap_or(ColType::Text);
                    match edit_literal(proto, ty, s) {
                        Ok(lit) => lit,
                        Err(_) => quote_str(proto, s),
                    }
                }
            })
            .collect();
        out.push(format!("INSERT INTO {} ({}) VALUES ({});",
            quote_qualified(proto, schema, table),
            cols.join(", "),
            vals.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            data_type: ty.to_string(),
            nullable: true,
            ..Default::default()
        }
    }

    /// PG 的类型 OID → 类型（阶段 64）。
    ///
    /// 这张表是**协议常量**（`pg_type.oid`），不随版本变 ⇒ 可以写死。
    /// 认不出来的一律 `None`（= 未知），**不猜**。
    #[test]
    fn pg_oid_映射表() {
        // 布尔
        assert_eq!(ColType::from_pg_oid(16), Some(ColType::Bool));
        // 整数：int2 / int4 / int8 / oid
        assert_eq!(ColType::from_pg_oid(21), Some(ColType::Int));
        assert_eq!(ColType::from_pg_oid(23), Some(ColType::Int));
        assert_eq!(ColType::from_pg_oid(20), Some(ColType::Int));
        assert_eq!(ColType::from_pg_oid(26), Some(ColType::Int));
        // 浮点 / 定点
        assert_eq!(ColType::from_pg_oid(700), Some(ColType::Float));
        assert_eq!(ColType::from_pg_oid(701), Some(ColType::Float));
        assert_eq!(ColType::from_pg_oid(1700), Some(ColType::Float));
        // 文本类
        assert_eq!(ColType::from_pg_oid(25), Some(ColType::Text));
        assert_eq!(ColType::from_pg_oid(1043), Some(ColType::Text));
        assert_eq!(ColType::from_pg_oid(1114), Some(ColType::Text));
        assert_eq!(ColType::from_pg_oid(1082), Some(ColType::Text));
        assert_eq!(ColType::from_pg_oid(3802), Some(ColType::Text));
        assert_eq!(ColType::from_pg_oid(2950), Some(ColType::Text));
        // 二进制 / 位串 ⇒ "未知"（不是文本）
        assert_eq!(ColType::from_pg_oid(17), Some(ColType::Other));
        assert_eq!(ColType::from_pg_oid(1560), Some(ColType::Other));
        // 不认识的 OID ⇒ None（**不猜**）
        assert_eq!(ColType::from_pg_oid(999999), None);
        assert_eq!(ColType::from_pg_oid(0), None);
    }

    /// 类型显示：**不知道就说不知道**（阶段 64）。
    ///
    /// 写"文本"是在骗人——用户会以为这一列真的是字符串，改的时候按字符串
    /// 的规矩加引号。
    #[test]
    fn 未知类型显示横杠() {
        assert_eq!(ColType::Other.label(), "—");
        assert_eq!(ColType::Text.label(), "文本");
        assert_eq!(ColType::Int.label(), "整数");
        assert_eq!(ColType::Float.label(), "数字");
        assert_eq!(ColType::Bool.label(), "布尔");
    }

    #[test]
    fn 模式级外键解析() {
        let rows = vec![
            vec![
                Some("orders".to_string()),
                Some("orders_user_id_fkey".to_string()),
                Some("user_id".to_string()),
                Some("users".to_string()),
                Some("id".to_string()),
            ],
            // 缺列的行要丢掉（宁可少一条，也不要画出断的线）
            vec![Some("orders".to_string()), Some("x".to_string())],
        ];
        let fks = parse_schema_foreign_keys(&rows);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].table, "orders");
        assert_eq!(fks[0].column, "user_id");
        assert_eq!(fks[0].ref_table, "users");
        assert_eq!(fks[0].ref_column, "id");
    }

    #[test]
    fn 模式级外键的_sql_两家都不一样() {
        let pg = meta_schema_foreign_keys(DbProto::Postgres, "public");
        assert!(pg.contains("referential_constraints"), "PG 要按位置配对列");
        assert!(!pg.contains("table_name = "), "PG 查的是整个模式，不按表过滤");
        let my = meta_schema_foreign_keys(DbProto::MySql, "demo");
        assert!(my.contains("key_column_usage"));
        assert!(!my.contains("referential_constraints"), "MySQL 没有那张表");
    }

    #[test]
    fn ddl_type_normalizes_dialect_names() {
        // PG 的类型名不能照抄到 CREATE TABLE 里
        let mut c = col("name", "character varying");
        c.char_len = Some(50);
        assert_eq!(ddl_type(&c), "varchar(50)");

        let mut c = col("amount", "numeric");
        c.num_precision = Some(10);
        c.num_scale = Some(2);
        assert_eq!(ddl_type(&c), "numeric(10,2)");

        // 自定义类型用 udt_name
        let mut c = col("kind", "USER-DEFINED");
        c.udt_name = Some("mood".into());
        assert_eq!(ddl_type(&c), "mood");

        // 数组类型去掉 PG 的 '_' 前缀
        let mut c = col("tags", "ARRAY");
        c.udt_name = Some("_int4".into());
        assert_eq!(ddl_type(&c), "int4[]");

        // MySQL 的整数显示宽度不能当精度用
        let mut c = col("id", "int");
        c.num_precision = Some(10);
        assert_eq!(ddl_type(&c), "int");
    }

    #[test]
    fn ddl_keeps_defaults_and_constraints() {
        let mut id = col("id", "bigint");
        id.nullable = false;
        id.is_pk = true;
        let mut name = col("name", "text");
        name.default_value = Some("'x'".into());
        let mut created = col("created_at", "timestamp without time zone");
        created.default_value = Some("now()".into());
        let cols = vec![id, name, created];
        let fks = vec![ForeignKey {
            name: "users_org_fk".into(),
            column: "org_id".into(),
            ref_table: "orgs".into(),
            ref_column: "id".into(),
        }];
        let ddl = build_ddl(DbProto::Postgres, "public", "demo_users", &cols, &fks);
        assert!(ddl.contains("CREATE TABLE \"public\".\"demo_users\""), "{ddl}");
        assert!(ddl.contains("\"id\" bigint NOT NULL"), "{ddl}");
        assert!(ddl.contains("CONSTRAINT \"demo_users_pkey\" PRIMARY KEY (\"id\")"), "{ddl}");
        // 默认值必须带上：少拼了，用户看一眼再执行就把默认值弄没了
        assert!(ddl.contains("DEFAULT 'x'"), "{ddl}");
        assert!(ddl.contains("DEFAULT now()"), "{ddl}");
        assert!(ddl.contains("FOREIGN KEY (\"org_id\") REFERENCES \"orgs\" (\"id\")"), "{ddl}");
    }

    #[test]
    fn ddl_quotes_by_dialect() {
        let cols = vec![col("id", "int")];
        let ddl = build_ddl(DbProto::MySql, "demo", "t", &cols, &[]);
        assert!(ddl.contains("CREATE TABLE `demo`.`t`"), "{ddl}");
        assert!(ddl.contains("`id` int"), "{ddl}");
    }

    #[test]
    fn insert_script_uses_column_types() {
        let cols = vec!["id".to_string(), "name".to_string(), "active".to_string()];
        let types = vec![ColType::Int, ColType::Text, ColType::Bool];
        let rows = vec![vec![
            Some("7".to_string()),
            Some("张三".to_string()),
            Some("true".to_string()),
        ]];
        let sql = build_inserts(DbProto::Postgres, "public", "demo_users", &cols, &types, &rows);
        assert!(
            sql.contains("INSERT INTO \"public\".\"demo_users\" (\"id\", \"name\", \"active\")"),
            "{sql}"
        );
        // 数字不带引号、布尔写成 TRUE、文本转义
        assert!(sql.contains("VALUES (7, '张三', TRUE)"), "{sql}");

        // NULL 写成 NULL
        let rows = vec![vec![None, Some("a'b".into()), None]];
        let sql = build_inserts(DbProto::MySql, "demo", "t", &cols, &types, &rows);
        assert!(sql.contains("VALUES (NULL, 'a''b', NULL)"), "{sql}");
    }

    #[test]
    fn column_defs_parse_defaults_and_lengths() {
        let rows = vec![vec![
            Some("id".into()),
            Some("character varying".into()),
            Some("NO".into()),
            Some("1".into()),
            Some("t".into()),
            Some("nextval('s'::regclass)".into()),
            Some("varchar".into()),
            Some("50".into()),
            Some("10".into()),
            Some("2".into()),
        ]];
        let cols = parse_column_defs(&rows);
        assert_eq!(cols.len(), 1);
        assert!(!cols[0].nullable);
        assert!(cols[0].is_pk);
        assert_eq!(cols[0].default_value.as_deref(), Some("nextval('s'::regclass)"));
        assert_eq!(cols[0].char_len, Some(50));
        assert_eq!(cols[0].num_precision, Some(10));
        assert_eq!(cols[0].num_scale, Some(2));
    }

    #[test]
    fn foreign_key_sql_is_dialect_specific() {
        let pg = meta_foreign_keys(DbProto::Postgres, "public", "t");
        let my = meta_foreign_keys(DbProto::MySql, "demo", "t");
        // MySQL 没有 constraint_column_usage（查了会报 Unknown table）
        assert!(pg.contains("constraint_column_usage"), "{pg}");
        assert!(!my.contains("constraint_column_usage"), "{my}");
        assert!(my.contains("referenced_table_name"), "{my}");
        // 表名要转义后进 SQL
        assert!(my.contains("'demo'"), "{my}");
        assert!(my.contains("'t'"), "{my}");
    }

    #[test]
    fn foreign_keys_parse() {
        let rows = vec![vec![
            Some("t_org_fk".into()),
            Some("org_id".into()),
            Some("orgs".into()),
            Some("id".into()),
        ]];
        let fks = parse_foreign_keys(&rows);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].name, "t_org_fk");
        assert_eq!(fks[0].ref_table, "orgs");
    }

    #[test]
    fn ident_quoting_is_dialect_specific() {
        assert_eq!(quote_ident(DbProto::Postgres, "users"), "\"users\"");
        assert_eq!(quote_ident(DbProto::MySql, "users"), "`users`");
        // 内部引号按方言转义
        assert_eq!(quote_ident(DbProto::Postgres, "a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_ident(DbProto::MySql, "a`b"), "`a``b`");
    }

    #[test]
    fn qualified_name_handles_empty_schema() {
        assert_eq!(
            quote_qualified(DbProto::Postgres, "public", "users"),
            "\"public\".\"users\""
        );
        assert_eq!(quote_qualified(DbProto::Postgres, "", "users"), "\"users\"");
        assert_eq!(quote_qualified(DbProto::MySql, "app", "t"), "`app`.`t`");
    }

    #[test]
    fn string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(quote_str(DbProto::Postgres, "a'b"), "'a''b'");
        // MySQL 把反斜杠当转义符，必须一起转
        assert_eq!(quote_str(DbProto::MySql, "a\\b"), "'a\\\\b'");
        assert_eq!(quote_str(DbProto::MySql, "a'b"), "'a''b'");
    }

    #[test]
    fn type_mapping_is_conservative() {
        assert_eq!(ColType::from_db_type("bigint"), ColType::Int);
        assert_eq!(ColType::from_db_type("NUMERIC"), ColType::Float);
        assert_eq!(ColType::from_db_type("boolean"), ColType::Bool);
        assert_eq!(ColType::from_db_type("text"), ColType::Text);
        assert_eq!(ColType::from_db_type("jsonb"), ColType::Text);
        // 认不出来 → 文本（不当数字，免得把填错的值静默改掉）
        assert_eq!(ColType::from_db_type("inet"), ColType::Text);
    }

    #[test]
    fn edit_literal_types_values() {
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Int, "42").unwrap(), "42");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Int, "-7").unwrap(), "-7");
        assert!(edit_literal(DbProto::Postgres, ColType::Int, "abc").is_err());
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Float, "1.5").unwrap(), "1.5");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Bool, "TRUE").unwrap(), "TRUE");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Bool, "否").unwrap(), "FALSE");
        assert!(edit_literal(DbProto::Postgres, ColType::Bool, "maybe").is_err());
        // 空文本 = 空字符串；<NULL> / NULL = NULL
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Text, "").unwrap(), "''");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Text, "<NULL>").unwrap(), "NULL");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Text, "null").unwrap(), "NULL");
        assert_eq!(edit_literal(DbProto::Postgres, ColType::Text, "a'b").unwrap(), "'a''b'");
    }

    #[test]
    fn update_uses_primary_key_when_available() {
        let sql = build_update(
            DbProto::Postgres,
            "public",
            "users",
            &[("id".into(), Some("3".into()))],
            &[("name".into(), "'张三'".into())],
            &[("id".into(), Some("3".into())), ("name".into(), Some("旧".into()))],
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE \"public\".\"users\" SET \"name\" = '张三' WHERE \"id\" = '3'"
        );
    }

    #[test]
    fn update_falls_back_to_whole_row() {
        let sql = build_update(
            DbProto::MySql,
            "app",
            "t",
            &[],
            &[("n".into(), "5".into())],
            &[("a".into(), Some("x".into())), ("b".into(), None)],
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE `app`.`t` SET `n` = 5 WHERE `a` = 'x' AND `b` IS NULL"
        );
    }

    #[test]
    fn update_refuses_without_condition() {
        let e = build_update(DbProto::Postgres, "public", "t", &[], &[("a".into(), "1".into())], &[])
            .unwrap_err();
        assert!(e.contains("无法定位行"), "{e}");
    }

    #[test]
    fn delete_refuses_without_condition() {
        assert!(build_delete(DbProto::Postgres, "public", "t", &[], &[]).is_err());
        let sql = build_delete(
            DbProto::Postgres,
            "public",
            "t",
            &[("id".into(), Some("1".into()))],
            &[],
        )
        .unwrap();
        assert_eq!(sql, "DELETE FROM \"public\".\"t\" WHERE \"id\" = '1'");
    }

    #[test]
    fn metadata_sql_embeds_names_safely() {
        // 表名里带引号不能拼坏 SQL
        let sql = meta_columns(DbProto::Postgres, "pub", "it's");
        assert!(sql.contains("'it''s'"), "{sql}");
        let sql = meta_tables(DbProto::MySql, "a`b");
        assert!(sql.contains("'a`b'"), "{sql}");
    }

    #[test]
    fn parses_metadata_rows() {
        let schemas = parse_schemas(&[vec![Some("public".into())], vec![None]]);
        assert_eq!(schemas, vec!["public".to_string()]);

        let tables = parse_tables(&[
            vec![Some("users".into()), Some("BASE TABLE".into())],
            vec![Some("v".into()), Some("VIEW".into())],
        ]);
        assert_eq!(tables[0], ("users".to_string(), false));
        assert_eq!(tables[1], ("v".to_string(), true));

        let cols = parse_columns(&[vec![
            Some("id".into()),
            Some("bigint".into()),
            Some("NO".into()),
            Some("1".into()),
            Some("t".into()),
        ]]);
        assert_eq!(cols[0].0, "id");
        assert_eq!(cols[0].1, "bigint");
        assert!(!cols[0].2, "NOT NULL 应解析为不可空");
        assert!(cols[0].3, "t 应解析为主键");

        let cols = parse_columns(&[vec![
            Some("name".into()),
            Some("text".into()),
            Some("YES".into()),
            Some("2".into()),
            Some("f".into()),
        ]]);
        assert!(cols[0].2);
        assert!(!cols[0].3);
    }

    #[test]
    fn build_updates_uses_primary_key_and_original_values() {
        let columns = vec!["id".to_string(), "name".to_string()];
        let types = vec![ColType::Int, ColType::Text];
        let rows = vec![
            vec![Some("1".to_string()), Some("旧名".to_string())],
            vec![Some("2".to_string()), Some("乙".to_string())],
        ];
        let mut edits: HashMap<usize, HashMap<usize, String>> = HashMap::new();
        edits
            .entry(0)
            .or_default()
            .insert(1, "新名".to_string());
        let sqls = build_updates(
            DbProto::Postgres,
            "public",
            "users",
            &columns,
            &types,
            &[0],
            &rows,
            &edits,
        )
        .unwrap();
        assert_eq!(sqls.len(), 1);
        assert_eq!(
            sqls[0],
            "UPDATE \"public\".\"users\" SET \"name\" = '新名' WHERE \"id\" = '1'"
        );
    }

    #[test]
    fn build_updates_falls_back_to_whole_row_without_primary_key() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let types = vec![ColType::Text, ColType::Text];
        let rows = vec![vec![Some("x".to_string()), None]];
        let mut edits: HashMap<usize, HashMap<usize, String>> = HashMap::new();
        edits.entry(0).or_default().insert(0, "y".to_string());
        let sqls = build_updates(
            DbProto::MySql,
            "app",
            "t",
            &columns,
            &types,
            &[],
            &rows,
            &edits,
        )
        .unwrap();
        // 没有主键 → 用整行原值定位；NULL 用 IS NULL
        assert_eq!(
            sqls[0],
            "UPDATE `app`.`t` SET `a` = 'y' WHERE `a` = 'x' AND `b` IS NULL"
        );
    }

    #[test]
    fn build_updates_rejects_bad_value_without_partial_output() {
        let columns = vec!["n".to_string()];
        let types = vec![ColType::Int];
        let rows = vec![vec![Some("1".to_string())]];
        let mut edits: HashMap<usize, HashMap<usize, String>> = HashMap::new();
        edits.entry(0).or_default().insert(0, "不是数字".to_string());
        let e = build_updates(
            DbProto::Postgres,
            "public",
            "t",
            &columns,
            &types,
            &[0],
            &rows,
            &edits,
        )
        .unwrap_err();
        assert!(e.contains("应为整数"), "{e}");
    }

    #[test]
    fn build_updates_keeps_row_order_stable() {
        let columns = vec!["id".to_string()];
        let types = vec![ColType::Int];
        let rows = vec![
            vec![Some("1".to_string())],
            vec![Some("2".to_string())],
        ];
        let mut edits: HashMap<usize, HashMap<usize, String>> = HashMap::new();
        edits.entry(1).or_default().insert(0, "20".to_string());
        edits.entry(0).or_default().insert(0, "10".to_string());
        let sqls = build_updates(
            DbProto::Postgres,
            "public",
            "t",
            &columns,
            &types,
            &[0],
            &rows,
            &edits,
        )
        .unwrap();
        assert_eq!(sqls.len(), 2);
        assert!(sqls[0].contains("= 10"), "{}", sqls[0]);
        assert!(sqls[1].contains("= 20"), "{}", sqls[1]);
    }

    #[test]
    fn select_table_has_limit() {
        assert_eq!(
            build_select_table(DbProto::Postgres, "public", "users", 200),
            "SELECT * FROM \"public\".\"users\" LIMIT 200"
        );
    }

    /* ---------------- 插入 / 删除行（9.4） ---------------- */

    #[test]
    fn insert_skips_columns_the_user_did_not_touch() {
        // 没填的列不写进语句 → 数据库用默认值（自增 id、默认时间…）
        let sql = build_insert(
            DbProto::Postgres,
            "public",
            "users",
            &["id".into(), "name".into(), "age".into()],
            &[ColType::Int, ColType::Text, ColType::Int],
            &[None, Some("张三".into()), None],
        )
        .unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"public\".\"users\" (\"name\") VALUES ('张三')"
        );
    }

    #[test]
    fn insert_converts_values_by_column_type() {
        let sql = build_insert(
            DbProto::Postgres,
            "public",
            "users",
            &["id".into(), "name".into(), "ok".into()],
            &[ColType::Int, ColType::Text, ColType::Bool],
            &[Some("7".into()), Some("李四".into()), Some("yes".into())],
        )
        .unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\", \"ok\") VALUES (7, '李四', TRUE)"
        );
    }

    #[test]
    fn insert_refuses_empty_row() {
        let err = build_insert(
            DbProto::Postgres,
            "public",
            "users",
            &["id".into(), "name".into()],
            &[ColType::Int, ColType::Text],
            &[None, None],
        )
        .unwrap_err();
        assert!(err.contains("还没有填任何值"), "{err}");
    }

    #[test]
    fn insert_rejects_bad_value_without_partial_output() {
        // id 是整数列，填 "abc" → 直接报错，不会拼出一条半对的语句
        let err = build_insert(
            DbProto::Postgres,
            "public",
            "users",
            &["id".into(), "name".into()],
            &[ColType::Int, ColType::Text],
            &[Some("abc".into()), Some("张三".into())],
        )
        .unwrap_err();
        assert!(err.contains("应为整数"), "{err}");
    }

    #[test]
    fn row_condition_prefers_primary_key() {
        let columns = vec!["id".into(), "name".into()];
        let orig = vec![Some("3".to_string()), Some("张三".to_string())];
        let (pk_cond, fallback) = row_condition(&columns, &[0], &orig);
        assert_eq!(pk_cond.len(), 1);
        assert_eq!(pk_cond[0].0, "id");
        assert_eq!(fallback.len(), 2);
    }

    /* ---------------- 例程 / 触发器（9.3） ---------------- */

    #[test]
    fn routines_parse_kind_args_and_returns() {
        let rows = vec![
            vec![
                Some("add_one".to_string()),
                Some("F".to_string()),
                Some("a integer".to_string()),
                Some("integer".to_string()),
            ],
            vec![
                Some("do_it".to_string()),
                Some("P".to_string()),
                Some("".to_string()),
                Some("".to_string()),
            ],
        ];
        let rs = parse_routines(&rows);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].name, "add_one");
        assert_eq!(rs[0].kind, RoutineKind::Function);
        assert_eq!(rs[0].args, "a integer");
        assert_eq!(rs[0].returns, "integer");
        assert_eq!(rs[1].kind, RoutineKind::Procedure);
    }

    #[test]
    fn triggers_parse_table_timing_and_event() {
        let rows = vec![vec![
            Some("trg".to_string()),
            Some("users".to_string()),
            Some("BEFORE".to_string()),
            Some("INSERT".to_string()),
        ]];
        let ts = parse_triggers(&rows);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].name, "trg");
        assert_eq!(ts[0].table, "users");
        assert_eq!(ts[0].timing, "BEFORE");
        assert_eq!(ts[0].event, "INSERT");
    }

    #[test]
    fn routine_metadata_sql_is_dialect_specific() {
        // PG 从 pg_proc 取，MySQL 从 information_schema.ROUTINES 取
        let pg = meta_routines(DbProto::Postgres, "public");
        assert!(pg.contains("pg_proc"), "{pg}");
        assert!(pg.contains("'public'"), "{pg}");
        let my = meta_routines(DbProto::MySql, "demo");
        assert!(my.contains("information_schema.ROUTINES"), "{my}");
        assert!(my.contains("'demo'"), "{my}");
    }

    #[test]
    fn strip_definer_removes_mysql_account() {
        let sql = "CREATE DEFINER=`root`@`localhost` FUNCTION `f`() RETURNS int RETURN 1";
        let out = strip_definer(sql);
        assert!(!out.contains("DEFINER"), "{out}");
        assert!(out.starts_with("CREATE FUNCTION `f`"), "{out}");
    }

    #[test]
    fn strip_definer_keeps_plain_sql() {
        let sql = "CREATE OR REPLACE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql";
        assert_eq!(strip_definer(sql), sql);
    }

    #[test]
    fn prepare_def_exec_drops_before_create_when_needed() {
        // PG 的函数是 CREATE OR REPLACE，本身幂等，不用先删
        let pg = prepare_def_exec(
            DbProto::Postgres,
            DefKind::Function,
            "public",
            "",
            "f",
            "CREATE OR REPLACE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql",
        );
        assert_eq!(pg.len(), 1);
        assert!(pg[0].starts_with("CREATE OR REPLACE"), "{pg:?}");

        // PG 的触发器要先 DROP（CREATE TRIGGER 不覆盖同名）
        let pg_trg = prepare_def_exec(
            DbProto::Postgres,
            DefKind::Trigger,
            "public",
            "users",
            "trg",
            "CREATE TRIGGER trg BEFORE INSERT ON public.users FOR EACH ROW EXECUTE FUNCTION f();",
        );
        assert_eq!(pg_trg.len(), 2);
        assert!(pg_trg[0].contains("DROP TRIGGER IF EXISTS"), "{pg_trg:?}");
        assert!(pg_trg[0].contains("\"public\".\"users\""), "{pg_trg:?}");

        // MySQL 的例程没有 OR REPLACE，要先 DROP；DEFINER 也要去掉
        let my = prepare_def_exec(
            DbProto::MySql,
            DefKind::Procedure,
            "demo",
            "",
            "p",
            "CREATE DEFINER=`root`@`localhost` PROCEDURE `p`() BEGIN SELECT 1; END",
        );
        assert_eq!(my.len(), 2);
        assert!(my[0].contains("DROP PROCEDURE IF EXISTS"), "{my:?}");
        assert!(!my[1].contains("DEFINER"), "{my:?}");
        // 体里的分号不能被拆开（MySQL 服务端按分号切多语句）
        assert!(my[1].contains("SELECT 1; END"), "{my:?}");
    }
    fn ty_map(pairs: &[(&str, ColType)]) -> std::collections::HashMap<String, ColType> {
        pairs
            .iter()
            .map(|(n, t)| (n.to_string(), *t))
            .collect()
    }

    #[test]
    fn where_条件按类型转值() {
        let m = ty_map(&[("id", ColType::Int), ("name", ColType::Text)]);
        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("id", FilterOp::Gt, "100")],
            &m,
        )
        .unwrap();
        assert_eq!(w, "\"id\" > 100");

        // 文本按方言转义（PG 只转单引号）
        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("name", FilterOp::Eq, "O'Brien")],
            &m,
        )
        .unwrap();
        assert_eq!(w, "\"name\" = 'O''Brien'");

        // MySQL 连反斜杠一起转
        let w = build_where(
            DbProto::MySql,
            &[FilterCond::and("name", FilterOp::Eq, "a\\b")],
            &m,
        )
        .unwrap();
        assert_eq!(w, "`name` = 'a\\\\b'");
    }

    #[test]
    fn where_类型不符本地就报错() {
        let m = ty_map(&[("id", ColType::Int)]);
        let e = build_where(
            DbProto::Postgres,
            &[FilterCond::and("id", FilterOp::Eq, "abc")],
            &m,
        )
        .unwrap_err();
        assert!(e.contains("应为整数"), "{e}");
    }

    #[test]
    fn where_多条件按连接词拼() {
        let m = ty_map(&[("id", ColType::Int), ("name", ColType::Text)]);
        let mut c2 = FilterCond::and("name", FilterOp::Like, "张%");
        c2.join = FilterJoin::Or;
        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("id", FilterOp::Ge, "1"), c2],
            &m,
        )
        .unwrap();
        assert_eq!(w, "\"id\" >= 1 OR \"name\" LIKE '张%'");
    }

    #[test]
    fn where_空值与_in() {
        let m = ty_map(&[("id", ColType::Int), ("name", ColType::Text)]);
        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("name", FilterOp::IsNull, "")],
            &m,
        )
        .unwrap();
        assert_eq!(w, "\"name\" IS NULL");

        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("id", FilterOp::In, "1, 2,3")],
            &m,
        )
        .unwrap();
        assert_eq!(w, "\"id\" IN (1, 2, 3)");

        // IN 列表是空的 → 报错，不生成 `IN ()`
        let e = build_where(
            DbProto::Postgres,
            &[FilterCond::and("id", FilterOp::In, " , ")],
            &m,
        )
        .unwrap_err();
        assert!(e.contains("空的"), "{e}");
    }

    #[test]
    fn where_没条件就是空串() {
        let m = ty_map(&[]);
        let w = build_where(DbProto::Postgres, &[], &m).unwrap();
        assert!(w.is_empty(), "{w}");
        // 列名留空的条件直接跳过
        let w = build_where(
            DbProto::Postgres,
            &[FilterCond::and("  ", FilterOp::Eq, "1")],
            &m,
        )
        .unwrap();
        assert!(w.is_empty(), "{w}");
    }

    #[test]
    fn select_带条件与不带条件() {
        let s = build_select_table_with_where(DbProto::Postgres, "public", "u", 200, "");
        assert_eq!(s, "SELECT * FROM \"public\".\"u\" LIMIT 200");
        let s = build_select_table_with_where(DbProto::Postgres, "public", "u", 200, "\"id\" > 1");
        assert_eq!(
            s,
            "SELECT * FROM \"public\".\"u\" WHERE \"id\" > 1 LIMIT 200"
        );
        // MySQL 用反引号
        let s = build_select_table_with_where(DbProto::MySql, "demo", "u", 50, "`id` > 1");
        assert_eq!(s, "SELECT * FROM `demo`.`u` WHERE `id` > 1 LIMIT 50");
    }

    #[test]
    fn 跳页的_offset() {
        // offset = 0 时不写 OFFSET（老语句原样）
        let s = build_select_table_page(DbProto::Postgres, "public", "u", 200, 0, "");
        assert_eq!(s, "SELECT * FROM \"public\".\"u\" LIMIT 200");
        // 跳到第 3 页：第 2 页是 200 行，offset = 400
        let s = build_select_table_page(DbProto::Postgres, "public", "u", 200, 400, "");
        assert_eq!(s, "SELECT * FROM \"public\".\"u\" LIMIT 200 OFFSET 400");
        // 带条件时同样
        let s = build_select_table_page(DbProto::Postgres, "public", "u", 200, 400, "\"id\" > 1");
        assert_eq!(
            s,
            "SELECT * FROM \"public\".\"u\" WHERE \"id\" > 1 LIMIT 200 OFFSET 400"
        );
        // MySQL 同样支持 LIMIT/OFFSET
        let s = build_select_table_page(DbProto::MySql, "demo", "u", 50, 100, "");
        assert_eq!(s, "SELECT * FROM `demo`.`u` LIMIT 50 OFFSET 100");
    }

    #[test]
    fn 常用类型按方言分() {
        // PG 的类型名跟 MySQL 不一样：一份通用清单会让用户挑到
        // **这个库根本不认的类型**（PG 没有 `int`，MySQL 没有 `jsonb`）。
        let pg = common_types(DbProto::Postgres);
        assert!(pg.contains(&"integer"), "PG 清单里没有 integer");
        assert!(pg.contains(&"jsonb"), "PG 清单里没有 jsonb");
        assert!(!pg.contains(&"int"), "PG 清单里不该有 MySQL 的 int");
        let my = common_types(DbProto::MySql);
        assert!(my.contains(&"int"), "MySQL 清单里没有 int");
        assert!(my.contains(&"json"), "MySQL 清单里没有 json");
        assert!(!my.contains(&"jsonb"), "MySQL 清单里不该有 PG 的 jsonb");
        // 类型名可以带参数（设计器里填的就是裸 SQL 片段）
        assert!(pg.contains(&"varchar(255)"), "PG 清单里没有带长度的 varchar");
        // 清单不能是空的（空的话下拉就是空的，用户没法选）
        assert!(pg.len() >= 10 && my.len() >= 10);
    }

    #[test]
    fn order_by_按方言加引号() {        // PG 用双引号、MySQL 用反引号
        assert_eq!(
            build_order_by(DbProto::Postgres, "id", true),
            "ORDER BY \"id\" ASC"
        );
        assert_eq!(
            build_order_by(DbProto::Postgres, "id", false),
            "ORDER BY \"id\" DESC"
        );
        assert_eq!(
            build_order_by(DbProto::MySql, "id", true),
            "ORDER BY `id` ASC"
        );
        // ⚠️ **列名里有空格 / 保留字时必须加引号**，否则服务器报语法错
        assert_eq!(
            build_order_by(DbProto::Postgres, "user name", true),
            "ORDER BY \"user name\" ASC"
        );
        assert_eq!(
            build_order_by(DbProto::MySql, "order", false),
            "ORDER BY `order` DESC"
        );
        // 空列名 → 不拼 ORDER BY（调用方直接拼进 SQL，中间不能留个空片段）
        assert_eq!(build_order_by(DbProto::Postgres, "", true), "");
        assert_eq!(build_order_by(DbProto::Postgres, "   ", true), "");
    }

    #[test]
    fn select_条件排序分页的顺序() {
        // ⚠️ **顺序必须是 WHERE → ORDER BY → LIMIT/OFFSET**（写反直接语法错）
        let s = build_select_table_page_order(
            DbProto::Postgres,
            "public",
            "u",
            200,
            400,
            "\"id\" > 1",
            "ORDER BY \"id\" DESC",
        );
        assert_eq!(
            s,
            "SELECT * FROM \"public\".\"u\" WHERE \"id\" > 1 ORDER BY \"id\" DESC LIMIT 200 OFFSET 400"
        );
        // 只有排序
        let s = build_select_table_page_order(
            DbProto::Postgres,
            "public",
            "u",
            200,
            0,
            "",
            "ORDER BY \"id\" ASC",
        );
        assert_eq!(
            s,
            "SELECT * FROM \"public\".\"u\" ORDER BY \"id\" ASC LIMIT 200"
        );
        // 都没有 → 跟老语句一致（不拼多余的空格）
        let s = build_select_table_page_order(DbProto::Postgres, "public", "u", 200, 0, "", "");
        assert_eq!(s, "SELECT * FROM \"public\".\"u\" LIMIT 200");
        // MySQL 同样
        let s = build_select_table_page_order(
            DbProto::MySql,
            "demo",
            "u",
            50,
            100,
            "`id` > 1",
            "ORDER BY `id` DESC",
        );
        assert_eq!(
            s,
            "SELECT * FROM `demo`.`u` WHERE `id` > 1 ORDER BY `id` DESC LIMIT 50 OFFSET 100"
        );
    }

    #[test]
    fn 建表改名删表按方言生成() {
        // 建表：列名要转义
        let sql = build_create_table(
            DbProto::Postgres,
            "public",
            "t1",
            &[("id".to_string(), "integer".to_string()), ("a b".to_string(), "text".to_string())],
        );
        assert_eq!(
            sql,
            "CREATE TABLE \"public\".\"t1\" (\"id\" integer, \"a b\" text)"
        );

        // 改名：PG 用 ALTER TABLE，MySQL 用 RENAME TABLE
        assert_eq!(
            build_rename_table(DbProto::Postgres, "public", "t1", "t2"),
            "ALTER TABLE \"public\".\"t1\" RENAME TO \"t2\""
        );
        assert_eq!(
            build_rename_table(DbProto::MySql, "demo", "t1", "t2"),
            "RENAME TABLE `demo`.`t1` TO `demo`.`t2`"
        );

        // 删表
        assert_eq!(
            build_drop_table(DbProto::Postgres, "public", "t1"),
            "DROP TABLE \"public\".\"t1\""
        );
    }

    #[test]
    fn 列定义解析并拦住非法输入() {
        // 每行「名称 类型」
        let cols = parse_ddl_columns("id integer\nname varchar(50)").unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[1], ("name".to_string(), "varchar(50)".to_string()));

        // 类型里有非法字符 → 报错（类型是裸 SQL 片段，不能放开）
        let e = parse_ddl_columns("id integer; DROP TABLE x").unwrap_err();
        assert!(e.contains("非法字符"), "{e}");
        // 列名不合法
        let e = parse_ddl_columns("1id integer").unwrap_err();
        assert!(e.contains("列名"), "{e}");
        // 没写类型
        let e = parse_ddl_columns("id").unwrap_err();
        assert!(e.contains("没写类型"), "{e}");
        // 全空 → 默认两列（MySQL 不允许没有列的表）
        let cols = parse_ddl_columns("   \n  ").unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].0, "id");
    }
    #[test]
    fn 搜索sql按方言生成() {
        // PG：ILIKE（大小写不敏感）+ 一条 UNION ALL 出全部种类
        let pg = meta_search_objects(DbProto::Postgres, "demo");
        assert!(pg.contains("ILIKE '%demo%'"), "{pg}");
        assert!(pg.contains("'table'"), "{pg}");
        assert!(pg.contains("'column'"), "{pg}");
        assert!(pg.contains("'function'"), "{pg}");
        assert!(pg.contains("'procedure'"), "{pg}");
        assert!(pg.contains("'trigger'"), "{pg}");
        // 视图和表分开查（relkind 不同）
        assert!(pg.contains("relkind IN ('r','p')"), "{pg}");
        assert!(pg.contains("relkind IN ('v','m')"), "{pg}");
        // 系统模式要滤掉，否则搜个常见列名会出一堆 information_schema 的字段
        assert!(pg.contains("NOT LIKE 'pg_%'"), "{pg}");
        assert!(pg.contains("information_schema"), "{pg}");
        // 结果条数有上限
        assert!(pg.contains(&format!("LIMIT {SEARCH_LIMIT}")), "{pg}");

        // MySQL：LIKE（默认排序规则就不分大小写）+ 排除系统库
        let my = meta_search_objects(DbProto::MySql, "demo");
        assert!(my.contains("LIKE '%demo%'"), "{my}");
        assert!(!my.contains("ILIKE"), "{my}");
        assert!(my.contains("'mysql','information_schema'"), "{my}");
        assert!(my.contains("information_schema.EVENTS"), "{my}");
    }

    #[test]
    fn 搜索关键词里的通配符被转义() {
        // 用户搜 "50%" 时，% 要是当通配符，搜出来的全是没用的东西。
        // PG 的字符串字面量不用转义反斜杠（standard_conforming_strings 开着），
        // MySQL 的要（`\` 在字面量里是转义符）。
        let pg = meta_search_objects(DbProto::Postgres, "50%");
        assert!(pg.contains("'%50\\%%'"), "{pg}");
        let my = meta_search_objects(DbProto::MySql, "50%");
        assert!(my.contains("'%50\\\\%%'"), "{my}");
        // 下划线同理（单字符通配符）
        let pg = meta_search_objects(DbProto::Postgres, "a_b");
        assert!(pg.contains("'%a\\_b%'"), "{pg}");
        let my = meta_search_objects(DbProto::MySql, "a_b");
        assert!(my.contains("'%a\\\\_b%'"), "{my}");
    }

    #[test]
    fn 搜索结果解析() {
        let rows = vec![
            vec![
                Some("table".to_string()),
                Some("public".to_string()),
                Some("demo_users".to_string()),
                Some(String::new()),
                Some(String::new()),
            ],
            vec![
                Some("column".to_string()),
                Some("public".to_string()),
                Some("email".to_string()),
                Some("demo_users".to_string()),
                Some("text".to_string()),
            ],
        ];
        let hits = parse_search_objects(&rows);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, SearchKind::Table);
        assert_eq!(hits[0].schema, "public");
        assert_eq!(hits[0].name, "demo_users");
        assert_eq!(hits[1].kind, SearchKind::Column);
        assert_eq!(hits[1].owner, "demo_users");
        assert_eq!(hits[1].extra, "text");
        // 认不出来的 kind 跳过（别把别的查询结果混进来）
        let bad = vec![vec![
            Some("nonsense".to_string()),
            Some("public".to_string()),
            Some("x".to_string()),
            None,
            None,
        ]];
        assert!(parse_search_objects(&bad).is_empty());
        // 空名字的跳过
        let empty = vec![vec![
            Some("table".to_string()),
            Some("public".to_string()),
            Some(String::new()),
            None,
            None,
        ]];
        assert!(parse_search_objects(&empty).is_empty());
    }

    #[test]
    fn 搜索种类的显示名() {
        assert_eq!(SearchKind::Table.label(), "表");
        assert_eq!(SearchKind::View.label(), "视图");
        assert_eq!(SearchKind::Column.label(), "字段");
        assert_eq!(SearchKind::Function.label(), "函数");
        assert_eq!(SearchKind::Procedure.label(), "存储过程");
        assert_eq!(SearchKind::Trigger.label(), "触发器");
        assert_eq!(SearchKind::Event.label(), "事件");
        assert_eq!(SearchKind::from_code("function"), Some(SearchKind::Function));
        assert_eq!(SearchKind::from_code("procedure"), Some(SearchKind::Procedure));
        assert_eq!(SearchKind::from_code("????"), None);
    }

    #[test]
    fn 只有mysql有定时事件() {
        // MySQL 有 Event Scheduler
        assert!(proto_supports_events(DbProto::MySql));
        // **PostgreSQL 压根没有定时事件**：它只有 `pg_event_trigger`（DDL 钩子）。
        // 查过 PG 9.6/10/12/17/18 的官方系统目录清单，从来没有 `pg_event`，
        // `CREATE EVENT` 也不是 PG 的语法。所以 PG 一律不查——硬查会得到
        // `relation "pg_event" does not exist`。
        assert!(!proto_supports_events(DbProto::Postgres));
    }

    #[test]
    fn 事件列表_sql按方言生成() {
        // PG：不生成 SQL（没有这个功能）
        assert!(meta_events(DbProto::Postgres, "").is_none());
        // MySQL：查 information_schema.EVENTS，按库过滤
        let my = meta_events(DbProto::MySql, "demo").expect("MySQL 有事件");
        assert!(my.contains("information_schema.EVENTS"), "{my}");
        assert!(my.contains("EVENT_SCHEMA = 'demo'"), "{my}");
        // 列名跨版本不一致的（SCHEDULING_TYPE / EVENT_TYPE）不能用
        assert!(!my.contains("SCHEDULING_TYPE"), "{my}");
        assert!(!my.contains("EVENT_TYPE"), "{my}");
    }

    #[test]
    fn 事件定义_sql按方言生成() {
        assert!(meta_event_def(DbProto::Postgres, "", "e1").is_none());
        let my = meta_event_def(DbProto::MySql, "demo", "e1").expect("MySQL 有事件");
        assert!(my.contains("SHOW CREATE EVENT"), "{my}");
        assert!(my.contains("`demo`.`e1`"), "{my}");
    }

    #[test]
    fn 事件解析保留原文调度() {
        // MySQL：调度是原文（`1 DAY` 这种），不翻译
        let list = parse_events(&[vec![
            Some("e1".to_string()),
            Some("ENABLED".to_string()),
            Some("1 DAY".to_string()),
        ]]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].schedule, "1 DAY");
        assert_eq!(list[0].status, "ENABLED");
        // 空名字的跳过
        let list = parse_events(&[vec![Some(String::new()), Some("ENABLED".to_string()), None]]);
        assert!(list.is_empty());
    }

    #[test]
    fn 对象属性按方言分派() {
        let pg = meta_object_stats(DbProto::Postgres, "public", "demo_users");
        // PG 走系统目录（pg_class / pg_namespace），不是 information_schema
        assert!(pg.contains("pg_total_relation_size"), "{pg}");
        assert!(pg.contains("obj_description"), "{pg}");
        assert!(pg.contains("'public'"), "{pg}");
        assert!(pg.contains("'demo_users'"), "{pg}");
        // PG 没有排序规则 / 创建时间 / 自增，这三项给空串
        assert!(!pg.contains("information_schema.TABLES"), "{pg}");

        let my = meta_object_stats(DbProto::MySql, "demo", "users");
        assert!(my.contains("information_schema.TABLES"), "{my}");
        assert!(my.contains("DATA_LENGTH"), "{my}");
        assert!(my.contains("AUTO_INCREMENT"), "{my}");
        assert!(my.contains("'demo'"), "{my}");
        // MySQL 的标识符是反引号
        assert!(!my.contains("\"demo\""), "{my}");
    }

    #[test]
    fn 对象属性解析按列序() {
        let rows = vec![vec![
            Some("42".to_string()),        // 行数
            Some("16384".to_string()),     // 字节
            Some("用户表".to_string()),     // 注释
            Some("6".to_string()),          // 列数
            Some("2".to_string()),          // 索引数
            Some("1".to_string()),          // 外键数
            Some("InnoDB".to_string()),     // 类型
            Some("utf8mb4_0900_ai_ci".to_string()), // 排序规则
            Some("2026-10-25 10:00:00".to_string()), // 创建时间
            Some("99".to_string()),         // 自增
        ]];
        let s = parse_object_stats(&rows).expect("解析失败");
        assert_eq!(s.rows, Some(42));
        assert_eq!(s.size_bytes, Some(16384));
        assert_eq!(s.comment, "用户表");
        assert_eq!(s.columns, Some(6));
        assert_eq!(s.indexes, Some(2));
        assert_eq!(s.foreign_keys, Some(1));
        assert_eq!(s.engine, "InnoDB");
        assert_eq!(s.collation, "utf8mb4_0900_ai_ci");
        // 字符集从排序规则名里拆出来
        assert_eq!(s.charset, "utf8mb4");
        assert_eq!(s.created, "2026-10-25 10:00:00");
        assert_eq!(s.auto_increment, "99");
    }

    #[test]
    fn 对象属性缺失时留空不编() {
        // PG 的后三列本来就是空串 → 解析出来是 None / 空串，不是 0
        let rows = vec![vec![
            Some("0".to_string()),
            Some("0".to_string()),
            Some(String::new()),
            Some("3".to_string()),
            Some("1".to_string()),
            Some("0".to_string()),
            Some("BASE TABLE".to_string()),
            Some(String::new()),
            Some(String::new()),
            Some(String::new()),
        ]];
        let s = parse_object_stats(&rows).expect("解析失败");
        assert_eq!(s.collation, "");
        assert_eq!(s.charset, "");
        assert_eq!(s.created, "");
        assert_eq!(s.auto_increment, "");
        // 拿不到的项在展示时是「—」，不是 0
        let rows_txt = s.rows();
        let find = |k: &str| {
            rows_txt
                .iter()
                .find(|(a, _)| a == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(find("排序规则"), "—");
        assert_eq!(find("创建时间"), "—");
        assert_eq!(find("注释"), "—");
        assert_eq!(find("列数"), "3");
    }

    #[test]
    fn 对象不存在时返回空() {
        assert!(parse_object_stats(&[]).is_none());
    }

    #[test]
    fn 字节数写成人类看得懂的() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KB");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    /* ---------------- 改结构（阶段 14） ---------------- */

    /// 造一条原来的列。
    fn ocol(name: &str, ty: &str) -> ColumnDef {
        let mut c = col(name, ty);
        c.nullable = true;
        c
    }

    /// 造一条设计器里的目标列。
    fn spec(orig_index: Option<usize>, name: &str, ty: &str) -> ColumnSpec {
        ColumnSpec {
            orig_index,
            name: name.to_string(),
            ty: ty.to_string(),
            nullable: true,
            is_pk: false,
            default_value: None,
        }
    }

    #[test]
    fn 没改就不生成语句() {
        let orig = vec![ocol("id", "integer"), ocol("name", "text")];
        let target = vec![spec(Some(0), "id", "integer"), spec(Some(1), "name", "text")];
        assert!(build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .is_none());
    }

    #[test]
    fn 类型写法不同但意思一样不算改动() {
        // PG 的 information_schema 写 "character varying"，用户可能敲
        // VARCHAR(50)：意思一样就不该生成 ALTER
        let mut o = ocol("name", "character varying");
        o.char_len = Some(50);
        let orig = vec![o];
        let target = vec![spec(Some(0), "name", "VARCHAR(50)")];
        assert!(build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .is_none());
    }

    #[test]
    fn 改类型生成alter并标出风险() {
        let orig = vec![ocol("id", "integer")];
        let target = vec![spec(Some(0), "id", "bigint")];
        let plan = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE \"public\".\"t\" ALTER COLUMN \"id\" TYPE bigint"
        );
        assert!(!plan.risks.is_empty(), "改类型要提示可能丢数据");
    }

    #[test]
    fn 改列名生成rename() {
        let orig = vec![ocol("id", "integer")];
        let target = vec![spec(Some(0), "uid", "integer")];
        let plan = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE \"public\".\"t\" RENAME COLUMN \"id\" TO \"uid\""
        );
        assert!(plan.risks.is_empty(), "改名不丢数据，不用拦");
    }

    #[test]
    fn 加列生成add_column() {
        let orig = vec![ocol("id", "integer")];
        let target = vec![spec(Some(0), "id", "integer"), spec(None, "note", "text")];
        let pg = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            pg.sql,
            "ALTER TABLE \"public\".\"t\" ADD COLUMN \"note\" text NULL"
        );
        let my = build_alter_table(DbProto::MySql, "demo", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            my.sql,
            "ALTER TABLE `demo`.`t` ADD COLUMN `note` text NULL"
        );
    }

    #[test]
    fn 删列生成drop且必须提示() {
        let orig = vec![ocol("id", "integer"), ocol("note", "text")];
        let target = vec![spec(Some(0), "id", "integer")];
        let plan = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE \"public\".\"t\" DROP COLUMN \"note\""
        );
        assert!(
            plan.risks.iter().any(|r| r.contains("note")),
            "删列要二次确认：{risks:?}",
            risks = plan.risks
        );
    }

    #[test]
    fn mysql改列用change且带完整定义() {
        // MySQL 的 MODIFY/CHANGE 必须写完整定义：
        // 只写类型的话 NOT NULL / DEFAULT 会被一起清掉
        let mut o = ocol("id", "integer");
        o.nullable = false;
        let orig = vec![o];
        let mut t = spec(Some(0), "uid", "bigint");
        t.nullable = false;
        let target = vec![t];
        let plan = build_alter_table(DbProto::MySql, "demo", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE `demo`.`t` CHANGE COLUMN `id` `uid` bigint NOT NULL"
        );
    }

    #[test]
    fn 主键变了会重建约束() {
        let mut o = ocol("id", "integer");
        o.is_pk = true;
        o.pk_constraint = Some("t_pkey".into());
        let orig = vec![o, ocol("name", "text")];
        let mut t1 = spec(Some(0), "id", "integer");
        t1.is_pk = true;
        let mut t2 = spec(Some(1), "name", "text");
        t2.is_pk = true;
        let target = vec![t1, t2];
        let plan = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"t_pkey\", ADD PRIMARY KEY (\"id\", \"name\")"
        );
    }

    #[test]
    fn 主键列改名不重建主键() {
        // PG 的 RENAME COLUMN 会把约束里的列名一起改掉，
        // 主键没变就不该 DROP/ADD（多一事不如少一事）
        let mut o = ocol("id", "integer");
        o.is_pk = true;
        o.pk_constraint = Some("t_pkey".into());
        let orig = vec![o];
        let mut t = spec(Some(0), "uid", "integer");
        t.is_pk = true;
        let target = vec![t];
        let plan = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target)
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.sql,
            "ALTER TABLE \"public\".\"t\" RENAME COLUMN \"id\" TO \"uid\""
        );
        assert!(!plan.sql.contains("PRIMARY KEY"), "不该重建主键：{}", plan.sql);
    }

    #[test]
    fn 列名里有注入企图要拦下() {
        let orig = vec![ocol("id", "integer")];
        let target = vec![spec(Some(0), "x); DROP TABLE users --", "integer")];
        let err = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("不合法"), "{err}");
    }

    #[test]
    fn 默认值里带分号要拦下() {
        let orig = vec![ocol("id", "integer")];
        let mut t = spec(Some(0), "id", "integer");
        t.default_value = Some("0; DROP TABLE users".into());
        let target = vec![t];
        let err = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("分号"), "{err}");
    }

    #[test]
    fn 列名重复要拦下() {
        let orig = vec![ocol("id", "integer")];
        let target = vec![spec(Some(0), "id", "integer"), spec(None, "id", "text")];
        let err = build_alter_table(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }

    /* ---------------- 改索引（阶段 15） ---------------- */

    /// 造一条索引。
    fn oidx(name: &str, cols: &[&str], unique: bool) -> IndexDef {
        IndexDef {
            name: name.to_string(),
            unique,
            primary: false,
            columns: cols.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 造一条设计器里的目标索引。
    fn ispec(orig: Option<&str>, name: &str, cols: &[&str], unique: bool) -> IndexSpec {
        IndexSpec {
            orig_name: orig.map(|s| s.to_string()),
            name: name.to_string(),
            unique,
            columns: cols
                .iter()
                .map(|s| IndexCol {
                    name: s.to_string(),
                    prefix: None,
                })
                .collect(),
        }
    }

    #[test]
    fn pg索引按行合并成一条() {
        // PG 的查询是一行一列，要并成一条索引
        let rows = vec![
            vec![
                Some("idx_a".into()),
                Some("f".into()),
                Some("f".into()),
                Some("id".into()),
            ],
            vec![
                Some("idx_a".into()),
                Some("f".into()),
                Some("f".into()),
                Some("name".into()),
            ],
            vec![
                Some("idx_b".into()),
                Some("t".into()),
                Some("f".into()),
                Some("id".into()),
            ],
        ];
        let idx = parse_index_defs(DbProto::Postgres, &rows);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].name, "idx_a");
        assert_eq!(idx[0].columns, vec!["id".to_string(), "name".to_string()]);
        assert!(!idx[0].unique);
        assert!(idx[1].unique);
    }

    #[test]
    fn mysql索引认主键且唯一性是反的() {
        // MySQL 的 NON_UNIQUE = 1 表示「不是唯一索引」；主键索引叫 PRIMARY
        let rows = vec![
            vec![
                Some("PRIMARY".into()),
                Some("id".into()),
                Some("0".into()),
            ],
            vec![
                Some("idx_a".into()),
                Some("id".into()),
                Some("1".into()),
            ],
        ];
        let idx = parse_index_defs(DbProto::MySql, &rows);
        assert_eq!(idx.len(), 2);
        assert!(idx[0].primary, "PRIMARY 要认成主键索引");
        assert!(idx[0].unique);
        assert!(!idx[1].unique, "NON_UNIQUE=1 是非唯一");
    }

    #[test]
    fn 删索引按方言分派() {
        let orig = vec![oidx("idx_a", &["id"], false)];
        let target: Vec<IndexSpec> = vec![];
        let (pg, _) = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(pg, vec!["DROP INDEX \"public\".\"idx_a\"".to_string()]);
        let (my, _) = build_index_sqls(DbProto::MySql, "demo", "t", &orig, &target).unwrap();
        assert_eq!(
            my,
            vec!["ALTER TABLE `demo`.`t` DROP INDEX `idx_a`".to_string()]
        );
    }

    #[test]
    fn 加索引按方言分派() {
        let orig: Vec<IndexDef> = vec![];
        let target = vec![ispec(None, "idx_new", &["id", "name"], true)];
        let (pg, risks) = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(
            pg,
            vec!["CREATE UNIQUE INDEX \"idx_new\" ON \"public\".\"t\" (\"id\", \"name\")".to_string()]
        );
        assert!(!risks.is_empty(), "加唯一索引要提示可能失败");
        let (my, _) = build_index_sqls(DbProto::MySql, "demo", "t", &orig, &target).unwrap();
        assert_eq!(
            my,
            vec!["ALTER TABLE `demo`.`t` ADD UNIQUE INDEX `idx_new` (`id`, `name`)".to_string()]
        );
    }

    #[test]
    fn 改了列的索引是删了重建() {
        // PG 没有 ALTER INDEX SET COLUMN，只能删了重建
        let orig = vec![oidx("idx_a", &["id"], false)];
        let target = vec![ispec(Some("idx_a"), "idx_a", &["name"], false)];
        let (pg, _) = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(pg.len(), 2);
        assert!(pg[0].starts_with("DROP INDEX"), "{pg:?}");
        assert!(pg[1].contains("CREATE INDEX"), "{pg:?}");
    }

    #[test]
    fn 没改的索引不动它() {
        let orig = vec![oidx("idx_a", &["id"], false)];
        let target = vec![ispec(Some("idx_a"), "idx_a", &["id"], false)];
        let (pg, _) = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert!(pg.is_empty(), "没改就不该发语句：{pg:?}");
    }

    #[test]
    fn 主键索引不参与改动() {
        // 主键由字段页管，索引页不该动它
        let mut pk = oidx("t_pkey", &["id"], true);
        pk.primary = true;
        let orig = vec![pk];
        let target: Vec<IndexSpec> = vec![];
        let (pg, _) = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert!(pg.is_empty(), "主键索引不该被删：{pg:?}");
    }

    #[test]
    fn 索引名重复要拦下() {
        let orig: Vec<IndexDef> = vec![];
        let target = vec![
            ispec(None, "idx_a", &["id"], false),
            ispec(None, "idx_a", &["name"], false),
        ];
        let err = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }

    #[test]
    fn 索引没有列要拦下() {
        let orig: Vec<IndexDef> = vec![];
        let target = vec![ispec(None, "idx_a", &[], false)];
        let err = build_index_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("没有列"), "{err}");
    }

    #[test]
    fn 索引列能写前缀长度() {
        // MySQL 的 TEXT 列建索引必须给前缀长度（否则 ERROR 1170）
        let cols = parse_index_columns("id, note(10)");
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].prefix, None);
        assert_eq!(cols[1].name, "note");
        assert_eq!(cols[1].prefix, Some(10));

        let mut t = ispec(None, "idx_a", &["id"], false);
        t.columns = cols;
        let (my, _) = build_index_sqls(DbProto::MySql, "demo", "t", &[], &[t]).unwrap();
        assert_eq!(
            my,
            vec!["ALTER TABLE `demo`.`t` ADD INDEX `idx_a` (`id`, `note`(10))".to_string()]
        );
    }

    #[test]
    fn pg不支持前缀索引要拦下() {
        let mut t = ispec(None, "idx_a", &["id"], false);
        t.columns = parse_index_columns("note(10)");
        let err = build_index_sqls(DbProto::Postgres, "public", "t", &[], &[t]).unwrap_err();
        assert!(err.contains("不支持前缀"), "{err}");
    }

    /* ---------------- 改外键（阶段 16） ---------------- */

    /// 造一条外键。
    fn ofk(name: &str, column: &str, ref_table: &str, ref_column: &str) -> ForeignKey {
        ForeignKey {
            name: name.to_string(),
            column: column.to_string(),
            ref_table: ref_table.to_string(),
            ref_column: ref_column.to_string(),
        }
    }

    /// 造一条设计器里的目标外键。
    fn fkspec(orig: Option<&str>, name: &str, column: &str, rt: &str, rc: &str) -> FkSpec {
        FkSpec {
            orig_name: orig.map(|s| s.to_string()),
            name: name.to_string(),
            column: column.to_string(),
            ref_table: rt.to_string(),
            ref_column: rc.to_string(),
        }
    }

    #[test]
    fn 删外键按方言分派() {
        let orig = vec![ofk("fk_a", "uid", "users", "id")];
        let target: Vec<FkSpec> = vec![];
        let (pg, risks) = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(
            pg,
            vec!["ALTER TABLE \"public\".\"t\" DROP CONSTRAINT \"fk_a\"".to_string()]
        );
        assert!(!risks.is_empty(), "删外键要提示");
        let (my, _) = build_fk_sqls(DbProto::MySql, "demo", "t", &orig, &target).unwrap();
        assert_eq!(
            my,
            vec!["ALTER TABLE `demo`.`t` DROP FOREIGN KEY `fk_a`".to_string()]
        );
    }

    #[test]
    fn 加外键生成add_constraint() {
        let orig: Vec<ForeignKey> = vec![];
        let target = vec![fkspec(None, "fk_a", "uid", "users", "id")];
        let (pg, _) = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(
            pg,
            vec![
                "ALTER TABLE \"public\".\"t\" ADD CONSTRAINT \"fk_a\" FOREIGN KEY (\"uid\") REFERENCES \"users\" (\"id\")"
                    .to_string()
            ]
        );
    }

    #[test]
    fn pg改外键名直接rename() {
        let orig = vec![ofk("fk_a", "uid", "users", "id")];
        let target = vec![fkspec(Some("fk_a"), "fk_b", "uid", "users", "id")];
        let (pg, _) = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(pg.len(), 1);
        assert!(pg[0].contains("RENAME CONSTRAINT"), "{pg:?}");
    }

    #[test]
    fn mysql改外键名只能删了重建() {
        // MySQL 没有"改约束名"的语法
        let orig = vec![ofk("fk_a", "uid", "users", "id")];
        let target = vec![fkspec(Some("fk_a"), "fk_b", "uid", "users", "id")];
        let (my, _) = build_fk_sqls(DbProto::MySql, "demo", "t", &orig, &target).unwrap();
        assert_eq!(my.len(), 2);
        assert!(my[0].contains("DROP FOREIGN KEY"), "{my:?}");
        assert!(my[1].contains("ADD CONSTRAINT"), "{my:?}");
    }

    #[test]
    fn 改引用列是删了重建() {
        let orig = vec![ofk("fk_a", "uid", "users", "id")];
        let target = vec![fkspec(Some("fk_a"), "fk_a", "uid", "users", "uuid")];
        let (pg, _) = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert_eq!(pg.len(), 2);
        assert!(pg[0].contains("DROP CONSTRAINT"), "{pg:?}");
        assert!(pg[1].contains("ADD CONSTRAINT"), "{pg:?}");
    }

    #[test]
    fn 没改的外键不动() {
        let orig = vec![ofk("fk_a", "uid", "users", "id")];
        let target = vec![fkspec(Some("fk_a"), "fk_a", "uid", "users", "id")];
        let (pg, _) = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap();
        assert!(pg.is_empty(), "没改就不该发语句：{pg:?}");
    }

    #[test]
    fn 外键约束名重复要拦下() {
        let orig: Vec<ForeignKey> = vec![];
        let target = vec![
            fkspec(None, "fk_a", "uid", "users", "id"),
            fkspec(None, "fk_a", "uid2", "users", "id"),
        ];
        let err = build_fk_sqls(DbProto::Postgres, "public", "t", &orig, &target).unwrap_err();
        assert!(err.contains("重复"), "{err}");
    }

    /* ---------------- 表统计（阶段 51） ---------------- */

    #[test]
    fn 表统计按方言查() {
        let pg = meta_table_stats(DbProto::Postgres, "pub");
        assert!(pg.contains("pg_class"), "{pg}");
        assert!(pg.contains("'pub'"), "{pg}");
        // PG 没有"表最后修改时间"，那一列给空串（不编）
        assert!(pg.contains("''"), "{pg}");
        let my = meta_table_stats(DbProto::MySql, "demo");
        assert!(my.contains("information_schema.TABLES"), "{my}");
        assert!(my.contains("'demo'"), "{my}");
        assert!(my.contains("UPDATE_TIME"), "{my}");
    }

    #[test]
    fn 解析表统计() {
        let rows = vec![
            vec![
                Some("users".to_string()),
                Some("12345".to_string()),
                Some("1048576".to_string()),
                Some("2026-10-20 14:33:12".to_string()),
            ],
            vec![
                Some("orders".to_string()),
                Some("NULL".to_string()),
                Some("0".to_string()),
                Some(String::new()),
            ],
        ];
        let st = parse_table_stats(&rows);
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].name, "users");
        assert_eq!(st[0].rows, 12345);
        assert_eq!(st[0].size_bytes, 1048576);
        assert_eq!(st[0].updated, "2026-10-20 14:33:12");
        // NULL 行数 → 0（不是 panic）
        assert_eq!(st[1].rows, 0);
        assert!(st[1].updated.is_empty());
    }

    #[test]
    fn 行数紧凑显示() {
        assert_eq!(fmt_rows(0), "0");
        assert_eq!(fmt_rows(999), "999");
        assert_eq!(fmt_rows(1234), "1,234");
        assert_eq!(fmt_rows(9999), "9,999");
        assert_eq!(fmt_rows(12345), "1.2 万");
        assert_eq!(fmt_rows(123_456_789), "1.2 亿");
    }

    #[test]
    fn 说明列文案() {
        let st = TableStat {
            name: "users".to_string(),
            rows: 12345,
            size_bytes: 1048576,
            updated: "2026-10-20 14:33:12".to_string(),
        };
        assert_eq!(
            fmt_table_detail("表", &st),
            "表 · ≈1.2 万 行 · 1.0 MB · 2026-10-20"
        );
        // 大小为 0 不写（视图 / 空表）
        let st2 = TableStat {
            name: "v".to_string(),
            rows: 0,
            size_bytes: 0,
            updated: String::new(),
        };
        assert_eq!(fmt_table_detail("视图", &st2), "视图 · ≈0 行");
    }

    /* ---------------- 自由文本 WHERE 的校验（阶段 84） ---------------- */

    #[test]
    fn 自由where空串等于不过滤() {
        assert_eq!(check_free_where("").unwrap(), "");
        assert_eq!(check_free_where("   \n ").unwrap(), "");
    }

    #[test]
    fn 自由where原样返回并去掉首尾空白() {
        assert_eq!(
            check_free_where("  id > 3 AND name LIKE 'a%'  ").unwrap(),
            "id > 3 AND name LIKE 'a%'"
        );
    }

    #[test]
    fn 自由where拒绝分号() {
        let e = check_free_where("id = 1; DROP TABLE users").unwrap_err();
        assert!(e.contains("分号"), "{e}");
    }

    #[test]
    fn 自由where结尾的分号放过并去掉() {
        // 用户从别处粘 `id = 1;` 过来很常见 ⇒ 那个分号只是句号
        assert_eq!(check_free_where("id = 1;").unwrap(), "id = 1");
        assert_eq!(check_free_where("id = 1;  ").unwrap(), "id = 1");
    }

    #[test]
    fn 自由where字符串里的分号放行() {
        // 分号在引号里是普通字符，不该被当成多语句
        assert_eq!(
            check_free_where("name = 'a;b'").unwrap(),
            "name = 'a;b'"
        );
        assert_eq!(
            check_free_where("name = \"a;b\"").unwrap(),
            "name = \"a;b\""
        );
    }

    #[test]
    fn 自由where拒绝超长() {
        let long = "a".repeat(4001);
        let e = check_free_where(&long).unwrap_err();
        assert!(e.contains("太长"), "{e}");
    }
}
