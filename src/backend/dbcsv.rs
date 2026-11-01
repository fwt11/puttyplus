//! CSV 解析与字段映射（阶段 18：Navicat 的 Import Wizard）。
//!
//! **为什么单独一个模块**：导入里最容易出错、又最难在 GUI 上看出来的是
//! "文件到底被解析成了什么样"——引号里的逗号、字段里的换行、`""` 转义、
//! BOM、CRLF。这些都得**纯函数 + 单测**钉死，出了问题在界面上只表现为
//! "第 3 行多了个逗号"，根本没法查。
//!
//! 解析规则（跟 Excel 导出的 CSV 对齐）：
//! - 分隔符默认逗号，可换成制表符（TSV）；
//! - 字段可用双引号包裹，包裹后里面的分隔符、换行都是普通字符；
//! - 包裹字段里的 `""` 表示一个 `"`；
//! - 行分隔支持 `\n` 与 `\r\n`；
//! - 文件开头的 UTF-8 BOM 会被去掉（Excel 导出的 CSV 常常带）。

/// 一份解析好的 CSV：行 × 列（短行的缺列补空串）。
pub type CsvTable = Vec<Vec<String>>;

/// 解析 CSV 文本。
///
/// `sep` 是字段分隔符（默认 `,`；TSV 用 `\t`）。
///
/// **空行会被丢掉**（Excel 导出常见末尾多一个换行）；**短行补空串**
/// （不会因为某行少一个逗号就整体失败——真实 CSV 里这种脏数据不少）。
pub fn parse_csv(text: &str, sep: char) -> Result<CsvTable, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: CsvTable = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    // 状态：普通 / 引号内
    let mut in_quotes = false;
    let mut had_field = false; // 这一行有没有内容（用来判断空行）
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    // `""` → 一个 `"`；单个 `"` 结束引号
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                had_field = true;
            }
            '\r' => {
                // 行尾：\r\n 或单独的 \r 都算换行
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                finish_row(&mut rows, &mut row, &mut field, &mut had_field);
            }
            '\n' => finish_row(&mut rows, &mut row, &mut field, &mut had_field),
            c if c == sep => {
                row.push(std::mem::take(&mut field));
                had_field = true;
            }
            c => {
                field.push(c);
                had_field = true;
            }
        }
    }
    // 文件末尾没有换行时，最后一行还在 field 里
    if had_field || !field.is_empty() || !row.is_empty() {
        row.push(std::mem::take(&mut field));
        rows.push(std::mem::take(&mut row));
    }
    if in_quotes {
        return Err("CSV 引号没有闭合（最后一行少一个 `\"`）".into());
    }
    // 短行补空串：真实 CSV 里"某行少一个逗号"很常见，
    // 整体失败不如补齐——补齐之后用户能在映射界面看到哪几行是空的。
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    for r in rows.iter_mut() {
        while r.len() < width {
            r.push(String::new());
        }
    }
    Ok(rows)
}

/// 收掉一行。
fn finish_row(
    rows: &mut CsvTable,
    row: &mut Vec<String>,
    field: &mut String,
    had_field: &mut bool,
) {
    row.push(std::mem::take(field));
    // 全空的行直接丢（末尾那个换行就是它）
    if !(row.len() == 1 && row[0].is_empty()) {
        rows.push(std::mem::take(row));
    } else {
        row.clear();
    }
    *had_field = false;
}

/// 归一化列名（匹配用）：去空白、转小写、**去掉下划线与空格**。
///
/// 为什么要去掉下划线和空格：Excel 里手打的表头经常是 `User ID`，
/// 数据库里却是 `user_id`——按字面匹配匹配不上，导入就少一列。
pub fn normalize_name(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect()
}

/// 按列名自动匹配：返回「每个表列对应哪个 CSV 列」。
///
/// `out[i]` = 表里第 i 列对应的 **CSV 列下标**，`None` = 这一列不导入
/// （用数据库默认值）。
///
/// **只按名字匹配，不按位置**：CSV 的列顺序跟表的不一样是常事，按位置
/// 匹配会把数据写进错的列——那是灾难性的（数据错乱还不报错）。
pub fn auto_map(csv_header: &[String], table_cols: &[String]) -> Vec<Option<usize>> {
    let norm_csv: Vec<String> = csv_header.iter().map(|s| normalize_name(s)).collect();
    table_cols
        .iter()
        .map(|tc| {
            let key = normalize_name(tc);
            if key.is_empty() {
                return None;
            }
            norm_csv.iter().position(|c| *c == key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 普通csv按逗号切() {
        let t = parse_csv("a,b,c\n1,2,3\n", ',').unwrap();
        assert_eq!(t, vec![vec!["a", "b", "c"], vec!["1", "2", "3"]]);
    }

    #[test]
    fn 引号里的逗号不算分隔符() {
        let t = parse_csv("a,b\n\"x,y\",z\n", ',').unwrap();
        assert_eq!(t, vec![vec!["a", "b"], vec!["x,y", "z"]]);
    }

    #[test]
    fn 双写引号表示一个引号() {
        let t = parse_csv("a\n\"he said \"\"hi\"\"\"\n", ',').unwrap();
        assert_eq!(t, vec![vec!["a"], vec!["he said \"hi\""]]);
    }

    #[test]
    fn 引号里的换行是数据的一部分() {
        let t = parse_csv("a,b\n\"line1\nline2\",z\n", ',').unwrap();
        assert_eq!(t, vec![vec!["a", "b"], vec!["line1\nline2", "z"]]);
    }

    #[test]
    fn crlf也算换行() {
        let t = parse_csv("a,b\r\n1,2\r\n", ',').unwrap();
        assert_eq!(t, vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn bom被去掉() {
        let t = parse_csv("\u{feff}a,b\n1,2\n", ',').unwrap();
        assert_eq!(t[0][0], "a");
    }

    #[test]
    fn 末尾多一个换行不产生空行() {
        let t = parse_csv("a,b\n1,2\n\n", ',').unwrap();
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn 短行补空串而不是报错() {
        let t = parse_csv("a,b,c\n1,2\n", ',').unwrap();
        assert_eq!(t[1], vec!["1", "2", ""]);
    }

    #[test]
    fn 制表符分隔也能解析() {
        let t = parse_csv("a\tb\n1\t2\n", '\t').unwrap();
        assert_eq!(t, vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn 引号没闭合要报错() {
        assert!(parse_csv("a\n\"abc\n", ',').is_err());
    }

    #[test]
    fn 列名匹配忽略大小写下划线与空格() {
        let header = vec!["User ID".to_string(), "Name".to_string()];
        let table = vec!["user_id".to_string(), "name".to_string()];
        assert_eq!(auto_map(&header, &table), vec![Some(0), Some(1)]);
    }

    #[test]
    fn 匹配不上就留空不瞎配() {
        let header = vec!["a".to_string()];
        let table = vec!["x".to_string(), "y".to_string()];
        assert_eq!(auto_map(&header, &table), vec![None, None]);
    }

    #[test]
    fn 同名列取第一个() {
        let header = vec!["id".to_string(), "id".to_string()];
        let table = vec!["id".to_string()];
        assert_eq!(auto_map(&header, &table), vec![Some(0)]);
    }
}
