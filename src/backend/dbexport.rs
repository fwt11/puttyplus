//! 结果导出（阶段 21）：**JSON / XML / HTML**（CSV 与 INSERT SQL 另有实现）。
//!
//! **为什么单独一个模块**：导出是**纯文本生成**——同样的数据必须生成
//! 逐字节相同的文件（不然没法做回归测试，用户也会发现"上次导的能解析、
//! 这次不能"）。所以全写成**纯函数**，单测钉死。
//!
//! **NULL 与空串是两回事**：JSON 里 NULL 是 `null`、XML 里是空元素、
//! HTML 里是空单元格——不能都写成字符串 "NULL"（那是 CSV 的做法）。
//! 所以这里收的是 `Option<String>`（`None` = SQL NULL），不是网格里
//! 那种已经转成文本的值。

use super::db::DbProto;
use std::io::Write;

/// 结果集的一行（每个格子 `None` = SQL NULL）。
pub type Row = Vec<Option<String>>;

/// 导成 JSON：一个数组，每行一个对象（键 = 列名）。
///
/// **为什么用对象而不是数组**：对象带列名，拿到文件的人不用对照表头。
pub fn to_json(columns: &[String], rows: &[Row]) -> String {
    // 一行都没有 → 就给个空数组（`[]` 比 `[\n]` 好看）
    if rows.is_empty() {
        return String::from("[]\n");
    }
    let mut out = String::from("[\n");
    for (ri, row) in rows.iter().enumerate() {
        out.push_str(&json_row(columns, row));
        if ri + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

/// 导成 XML。
///
/// ⚠️ **列名放在属性里，不用作元素名**：列名可能是 `user id`、`2nd_col`
/// 这种**不合法的 XML 元素名**（不能以数字开头、不能带空格）。写成
/// `<col name="user id">` 就没这个问题。
pub fn to_xml(columns: &[String], rows: &[Row]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rows>\n");
    for row in rows {
        out.push_str("  <row>\n");
        for (ci, col) in columns.iter().enumerate() {
            out.push_str("    <col name=\"");
            out.push_str(&xml_escape(col, true));
            out.push_str("\">");
            if let Some(v) = row.get(ci).and_then(|v| v.as_ref()) {
                out.push_str(&xml_escape(v, false));
            }
            out.push_str("</col>\n");
        }
        out.push_str("  </row>\n");
    }
    out.push_str("</rows>\n");
    out
}

/// 导成 HTML 表格（能直接在浏览器里看）。
pub fn to_html(columns: &[String], rows: &[Row]) -> String {
    let mut out = String::from(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n\
         <title>导出结果</title>\n</head>\n<body>\n<table>\n",
    );
    out.push_str("  <tr>");
    for col in columns {
        out.push_str("<th>");
        out.push_str(&html_escape(col));
        out.push_str("</th>");
    }
    out.push_str("</tr>\n");
    for row in rows {
        out.push_str("  <tr>");
        for ci in 0..columns.len() {
            out.push_str("<td>");
            if let Some(v) = row.get(ci).and_then(|v| v.as_ref()) {
                out.push_str(&html_escape(v));
            }
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n</body>\n</html>\n");
    out
}

/* ---------------- 阶段 52：数据传输（整表导出） ---------------- */

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFmt {
    Csv,
    Sql,
    Json,
}

impl ExportFmt {
    /// 下拉里显示的文字。
    pub fn label(self) -> &'static str {
        match self {
            ExportFmt::Csv => "CSV",
            ExportFmt::Sql => "SQL",
            ExportFmt::Json => "JSON",
        }
    }

    /// 程序里认的 id（**下拉的 id，不是显示文字**——阶段 48 踩过这个坑）。
    pub fn id(self) -> &'static str {
        match self {
            ExportFmt::Csv => "csv",
            ExportFmt::Sql => "sql",
            ExportFmt::Json => "json",
        }
    }

    /// 按 id 找格式（认不出来就当 CSV）。
    pub fn from_id(s: &str) -> ExportFmt {
        match s {
            "sql" => ExportFmt::Sql,
            "json" => ExportFmt::Json,
            _ => ExportFmt::Csv,
        }
    }

    /// 文件扩展名。
    pub fn ext(self) -> &'static str {
        self.id()
    }

    /// 下拉里的全部选项（`id`, 格式）。
    pub const ALL: [(&'static str, ExportFmt); 3] = [
        ("csv", ExportFmt::Csv),
        ("sql", ExportFmt::Sql),
        ("json", ExportFmt::Json),
    ];
}

/// 导出结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// 导出了多少行
    pub rows: u64,
    /// 写到哪个文件
    pub path: String,
}

/// 逐行写导出文件（阶段 52 的「数据传输」用）。
///
/// **为什么要"流式"而不是攒齐了再写**：一张表可能几百万行，全攒在内存里
/// 再写会把进程撑爆（而且 UI 线程会卡死）。这里**边查边写**，每页写完就扔。
///
/// **JSON 的逗号怎么放**：JSON 数组的元素之间要逗号，但"最后一行后面没有
/// 逗号"——流式写的时候还不知道哪行是最后一行。这里的办法是**把上一行
/// 押着**，等下一行来了才把上一行连同逗号一起写出去（只押一行，内存不变）。
/// 这样写出来的字节跟 `to_json` **逐字节一致**。
pub struct ExportWriter {
    fmt: ExportFmt,
    out: std::io::BufWriter<std::fs::File>,
    columns: Vec<String>,
    proto: DbProto,
    schema: String,
    table: String,
    rows: u64,
    /// JSON：押着的上一行（等着跟下一行一起写）
    pending: Option<String>,
}

impl ExportWriter {
    /// 新建导出文件。
    ///
    /// **CSV 先写 BOM**：Excel 打开 UTF-8 的 CSV 不认 BOM 就会把中文显示成
    /// 乱码（老版 Excel 尤其明显）。网格那边的 `to_csv` 也是这么做的。
    pub fn create(
        fmt: ExportFmt,
        path: &str,
        proto: DbProto,
        schema: &str,
        table: &str,
    ) -> Result<Self, String> {
        let f = std::fs::File::create(path).map_err(|e| format!("打不开文件「{path}」：{e}"))?;
        let mut out = std::io::BufWriter::new(f);
        if fmt == ExportFmt::Csv {
            out.write_all(b"\xEF\xBB\xBF")
                .map_err(|e| format!("写文件失败：{e}"))?;
        }
        Ok(ExportWriter {
            fmt,
            out,
            columns: Vec::new(),
            proto,
            schema: schema.to_string(),
            table: table.to_string(),
            rows: 0,
            pending: None,
        })
    }

    /// 写表头（CSV 的列名行 / JSON 的 `[` / SQL 的建表语句）。
    pub fn write_header(&mut self, columns: &[String]) -> Result<(), String> {
        self.columns = columns.to_vec();
        match self.fmt {
            ExportFmt::Csv => {
                let line = columns
                    .iter()
                    .map(|c| csv_field(c))
                    .collect::<Vec<_>>()
                    .join(",");
                self.write_line(&line)
            }
            ExportFmt::Sql => {
                // SQL 格式的建表语句由 `write_ddl` 单独写（要现查表结构）
                Ok(())
            }
            ExportFmt::Json => self.out.write_all(b"[\n").map_err(|e| format!("写文件失败：{e}")),
        }
    }

    /// 写建表语句（只有 SQL 格式用得上）。
    ///
    /// **为什么要带上它**：只导出 INSERT 的话，那个 SQL 脚本在空库里跑不了
    /// ——"数据传输"要能搬到一个没有这张表的地方，DDL 少了这一段就搬不动。
    pub fn write_ddl(&mut self, ddl: &str) -> Result<(), String> {
        if self.fmt != ExportFmt::Sql || ddl.trim().is_empty() {
            return Ok(());
        }
        let ddl = ddl.trim();
        self.write_line(ddl)?;
        self.write_line("")
    }

    /// 写一行数据。
    pub fn write_row(&mut self, row: &[Option<String>]) -> Result<(), String> {
        self.rows += 1;
        match self.fmt {
            ExportFmt::Csv => {
                let line = row
                    .iter()
                    .map(|c| csv_field(c.as_deref().unwrap_or("NULL")))
                    .collect::<Vec<_>>()
                    .join(",");
                self.write_line(&line)
            }
            ExportFmt::Sql => {
                let cols = self
                    .columns
                    .iter()
                    .map(|c| quote_ident(self.proto, c))
                    .collect::<Vec<_>>()
                    .join(", ");
                let vals = row
                    .iter()
                    .map(|v| sql_literal(v.as_deref()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let stmt = format!(
                    "INSERT INTO {} ({cols}) VALUES ({vals});",
                    quote_qualified(self.proto, &self.schema, &self.table)
                );
                self.write_line(&stmt)
            }
            ExportFmt::Json => {
                let text = json_row(&self.columns, row);
                // 上一行押着，这会儿连同逗号一起写出去
                if let Some(prev) = self.pending.take() {
                    self.out
                        .write_all(prev.as_bytes())
                        .map_err(|e| format!("写文件失败：{e}"))?;
                    self.out
                        .write_all(b",\n")
                        .map_err(|e| format!("写文件失败：{e}"))?;
                }
                self.pending = Some(text);
                Ok(())
            }
        }
    }

    /// 收尾（把押着的最后一行写出去 + 收尾括号）。
    pub fn finish(mut self) -> Result<ExportReport, String> {
        if self.fmt == ExportFmt::Json {
            if let Some(last) = self.pending.take() {
                self.out
                    .write_all(last.as_bytes())
                    .map_err(|e| format!("写文件失败：{e}"))?;
                self.out
                    .write_all(b"\n")
                    .map_err(|e| format!("写文件失败：{e}"))?;
            }
            self.out
                .write_all(b"]\n")
                .map_err(|e| format!("写文件失败：{e}"))?;
        }
        self.out.flush().map_err(|e| format!("写文件失败：{e}"))?;
        Ok(ExportReport {
            rows: self.rows,
            path: String::new(),
        })
    }

    /// 写一行文本 + 换行。
    fn write_line(&mut self, line: &str) -> Result<(), String> {
        self.out
            .write_all(line.as_bytes())
            .map_err(|e| format!("写文件失败：{e}"))?;
        self.out
            .write_all(b"\n")
            .map_err(|e| format!("写文件失败：{e}"))
    }
}

/// 一行 JSON 对象（**不带首尾逗号**，逗号由调用方决定）。
fn json_row(columns: &[String], row: &[Option<String>]) -> String {
    let mut out = String::from("  {\n");
    for (ci, col) in columns.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&json_string(col));
        out.push_str(": ");
        match row.get(ci).and_then(|v| v.as_ref()) {
            Some(v) => out.push_str(&json_string(v)),
            None => out.push_str("null"),
        }
        if ci + 1 < columns.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  }");
    out
}

/// CSV 字段转义（含逗号 / 引号 / 换行时加引号，引号翻倍）。
///
/// **和网格那边（`dbgrid::csv_field`）是同一套规则**——同一批数据从结果
/// 网格导出和从"数据传输"导出，文件必须长得一样。
pub fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// SQL 字面量：NULL 就是 NULL，文本加单引号（单引号翻倍）。
fn sql_literal(v: Option<&str>) -> String {
    match v {
        None => "NULL".to_string(),
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

/// 标识符加引号（PG 用双引号，MySQL 用反引号）。
fn quote_ident(proto: DbProto, s: &str) -> String {
    match proto {
        DbProto::Postgres => format!("\"{}\"", s.replace('"', "\"\"")),
        DbProto::MySql => format!("`{}`", s.replace('`', "``")),
    }
}

/// 限定名（`模式.表`）加引号。
fn quote_qualified(proto: DbProto, schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        quote_ident(proto, schema),
        quote_ident(proto, table)
    )
}

/* ---------------- 转义 ---------------- */
/// JSON 字符串（含首尾的引号）。
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // 其余控制字符（JSON 不允许裸着出现）
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// XML 转义。
///
/// `in_attr = true` 时连引号一起转（属性值里 `"` 会提前闭合属性）。
fn xml_escape(s: &str, in_attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if in_attr => out.push_str("&quot;"),
            '\'' if in_attr => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// HTML 转义。
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn json_是对象数组() {
        let c = cols(&["id", "name"]);
        let rows = vec![
            vec![Some("1".into()), Some("张三".into())],
            vec![Some("2".into()), None],
        ];
        let s = to_json(&c, &rows);
        assert!(s.contains("\"id\": \"1\""), "{s}");
        assert!(s.contains("\"name\": \"张三\""), "{s}");
        // NULL 是 null，不是字符串 "NULL"
        assert!(s.contains("\"name\": null"), "{s}");
        assert!(!s.contains("\"NULL\""), "{s}");
        // 行之间有逗号
        assert_eq!(s.matches('{').count(), 2, "{s}");
        assert!(s.trim_start().starts_with('['), "{s}");
        assert!(s.trim_end().ends_with(']'), "{s}");
    }

    #[test]
    fn json_转义引号反斜杠与控制字符() {
        let c = cols(&["v"]);
        let rows = vec![vec![Some("a\"b\\c\nd\te".into())]];
        let s = to_json(&c, &rows);
        assert!(s.contains(r#""v": "a\"b\\c\nd\te""#), "{s}");
    }

    #[test]
    fn xml_列名放属性里() {
        let c = cols(&["user id", "2nd_col"]);
        let rows = vec![vec![Some("x".into()), Some("y".into())]];
        let s = to_xml(&c, &rows);
        // 列名不合法的 XML 元素名 → 必须放属性里
        assert!(s.contains("<col name=\"user id\">x</col>"), "{s}");
        assert!(s.contains("<col name=\"2nd_col\">y</col>"), "{s}");
        assert!(s.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"), "{s}");
    }

    #[test]
    fn xml_转义尖括号和引号() {
        let c = cols(&["v"]);
        let rows = vec![vec![Some("<a> & \"b\"".into())]];
        let s = to_xml(&c, &rows);
        // 正文里 `&` `<` `>` 要转义；**引号在正文里不用转义**
        // （只有属性值里的引号会提前闭合属性，才要转成 &quot;）
        assert!(s.contains("&lt;a&gt; &amp; \"b\""), "{s}");
        assert!(!s.contains("<a>"), "{s}");
        // 属性里的引号必须转义
        let c2 = cols(&["a\"b"]);
        let s2 = to_xml(&c2, &rows);
        assert!(s2.contains("<col name=\"a&quot;b\">"), "{s2}");
    }

    #[test]
    fn 空结果集也不崩() {
        let c = cols(&["id"]);
        assert_eq!(to_json(&c, &[]).trim(), "[]");
        assert!(to_xml(&c, &[]).contains("<rows>"));
        assert!(to_html(&c, &[]).contains("<table>"));
    }

    #[test]
    fn 没有列时也是合法输出() {
        let rows = vec![vec![]];
        let j = to_json(&[], &rows);
        assert!(j.trim_start().starts_with('['), "{j}");
        assert!(j.contains('{') && j.contains('}'), "{j}");
        assert!(to_xml(&[], &rows).contains("<row>"));
        assert!(to_html(&[], &rows).contains("<tr>"));
    }

    /* ---------------- 阶段 52：流式导出 ---------------- */

    /// 临时文件路径（测试用）。
    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pp-export-{}-{}-{}.out",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn c2(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 流式导出_csv_有表头有数据() {
        let p = tmp_path("csv");
        let path = p.to_string_lossy().to_string();
        let mut w =
            ExportWriter::create(ExportFmt::Csv, &path, DbProto::Postgres, "public", "t").unwrap();
        w.write_header(&c2(&["id", "name"])).unwrap();
        w.write_row(&[Some("1".into()), Some("张,三".into())]).unwrap();
        w.write_row(&[Some("2".into()), None]).unwrap();
        let rep = w.finish().unwrap();
        assert_eq!(rep.rows, 2);
        let text = std::fs::read_to_string(&p).unwrap();
        // BOM 在最前面（Excel 认）
        assert!(text.starts_with('\u{feff}'), "{text:?}");
        assert!(text.contains("id,name\n"), "{text:?}");
        // 含逗号的字段要加引号
        assert!(text.contains("\"张,三\""), "{text:?}");
        // NULL 在 CSV 里就是 NULL（老规矩，跟网格那边一致）
        assert!(text.contains("2,NULL\n"), "{text:?}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 流式导出_sql_一条一个insert() {
        let p = tmp_path("sql");
        let path = p.to_string_lossy().to_string();
        let mut w =
            ExportWriter::create(ExportFmt::Sql, &path, DbProto::Postgres, "public", "t").unwrap();
        w.write_header(&c2(&["id", "name"])).unwrap();
        w.write_ddl("CREATE TABLE \"public\".\"t\" (\"id\" integer);").unwrap();
        w.write_row(&[Some("1".into()), Some("O'Brien".into())]).unwrap();
        w.write_row(&[Some("2".into()), None]).unwrap();
        w.finish().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("CREATE TABLE"), "{text}");
        assert!(
            text.contains("INSERT INTO \"public\".\"t\" (\"id\", \"name\") VALUES ('1', 'O''Brien');"),
            "{text}"
        );
        // NULL 就是 NULL，不是 'NULL'
        assert!(text.contains("VALUES ('2', NULL);"), "{text}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 流式导出_json_跟_to_json_逐字节一致() {
        let p = tmp_path("json");
        let path = p.to_string_lossy().to_string();
        let cols = c2(&["id", "name"]);
        let rows = vec![
            vec![Some("1".into()), Some("张三".into())],
            vec![Some("2".into()), None],
            vec![Some("3".into()), Some("a\"b".into())],
        ];
        let mut w =
            ExportWriter::create(ExportFmt::Json, &path, DbProto::Postgres, "public", "t").unwrap();
        w.write_header(&cols).unwrap();
        for r in &rows {
            w.write_row(r).unwrap();
        }
        w.finish().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text, to_json(&cols, &rows), "流式导出要和 to_json 逐字节一致");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 导出格式的_id_对得上() {
        for (id, f) in ExportFmt::ALL {
            assert_eq!(ExportFmt::from_id(id), f, "{id}");
            assert_eq!(f.id(), id);
            assert!(!f.label().is_empty());
        }
        // 认不出来的 id → 当 CSV（别崩）
        assert_eq!(ExportFmt::from_id("xlsx"), ExportFmt::Csv);
    }
}
