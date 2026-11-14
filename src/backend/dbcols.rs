//! 列显示设置的持久化（阶段 20）：`~/.config/puttyplus/columns.ini`。
//!
//! **存什么**：某张表在某个连接下，用户挑了要显示哪些列、把它们排成什么顺序、
//! 以及用户手动拖成多宽（阶段 89 加的 `widths`）。
//! Navicat / DBeaver 都是这个路子——"这张表我永远只关心这几列"。
//!
//! ```ini
//! [pg/postgres@127.0.0.1:5432/postgres/本地 PG/public.demo_users]
//! cols=id:1,email:1,name:0,created_at:1
//! widths=email:220,id:96
//! ```
//!
//! **为什么按列名存、不按下标**：结果集的列下标跟着 SELECT 列表变，
//! 下标存下来下次就对不上号了。按名字存才稳——表加了列、换了列顺序都能对上。
//! **宽度也按列名存**（同一个道理）⇒ 藏过列、换了列顺序都不会错位。
//!
//! **为什么单独一个文件**：`databases.ini` 的组名是 `<会话键>/<连接名>`，
//! 往里塞别的组会被当成"多了一条连接"（组名前缀一样）。列设置另开一个
//! 文件，各管各的，谁也不干扰谁。
//!
//! **只存表浏览页签的**：普通查询结果每次的列都不一样（SELECT 列表换了
//! 就是另一批列），存下来没有意义。表浏览才是"这张表一直是这几列"。

use std::path::PathBuf;

use glib::KeyFile;

use super::dbstore::DbProfile;

/// 一列的显示设置：`(列名, 是否显示)`。
pub type ColSpec = (String, bool);

/// 一列的**用户宽度**（阶段 89）：`(列名, 像素)`。
///
/// ⚠️ **只存用户拖过的那几列**：没拖过的列是「按内容自动分配」的，
/// 存下来反而会把它钉死（窗口一变窄，那列就卡在那儿不动了）。
pub type WidthSpec = (String, i32);

/// 宽度上限（像素）——存进来的值超过这个就当没存。
///
/// ⚠️ **为什么要卡**：文件是用户可改的 ⇒ 手滑写个 999999 也得能扛住
/// （不然那一列宽到屏幕外，横向滚动条拉不到头）。
const MAX_WIDTH: i32 = 10_000;

fn ini_path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想看/改别的文件，不必动真实配置。
    if let Some(p) = std::env::var_os("PUTTYPLUS_COLS_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir()
        .join("puttyplus")
        .join("columns.ini")
}

/// KeyFile 组名不能含 `[` `]` 与换行；表名 / 连接名可能带这些字符。
fn group(conn: &str, schema: &str, table: &str) -> String {
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '[' | ']' | '\n' | '\r' => '_',
                _ => c,
            })
            .collect()
    };
    format!("{}/{}.{}", sanitize(conn), sanitize(schema), sanitize(table))
}

/// 连接的标识（列显示设置按它分区）。
///
/// **为什么把地址也带上**：同一个 SSH 会话下可以挂多个数据库连接，
/// 光看连接名不够（同名不同库也要分开）；但地址变了又该当成新连接——
/// 列设置是跟着"这个库的这张表"走的，库换了就该重来。
pub fn conn_key(p: &DbProfile) -> String {
    format!(
        "{}/{}@{}/{}",
        p.proto.as_str(),
        p.user,
        format_args!("{}:{}", p.host, p.port),
        p.dbname
    )
}

/* ---------------- 编解码 ---------------- */

/// 把列布局编成一行文本：`列名:1,列名2:0`。
///
/// **为什么要转义**：列名里可能有 `,` `:`（SQL 里什么字符都可能出现），
/// 不转义就分不清哪段是名字、哪段是标志位。转义这三个字符就够。
fn encode(cols: &[ColSpec]) -> String {
    let mut out = String::new();
    for (i, (name, visible)) in cols.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&enc_one(name));
        out.push(':');
        out.push(if *visible { '1' } else { '0' });
    }
    out
}

/// 解一行文本回列布局（`encode` 的逆）。
fn decode(s: &str) -> Vec<ColSpec> {
    let mut out = Vec::new();
    for part in s.split(',') {
        if part.is_empty() {
            continue;
        }
        // 名字里可能有 `:`（转义过的），所以只按**最后一个**冒号切
        let Some(pos) = part.rfind(':') else {
            continue;
        };
        let name = dec_one(&part[..pos]);
        let visible = part[pos + 1..].starts_with('1');
        out.push((name, visible));
    }
    out
}

fn enc_one(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' => out.push_str("%25"),
            ',' => out.push_str("%2C"),
            ':' => out.push_str("%3A"),
            _ => out.push(c),
        }
    }
    out
}

fn dec_one(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 2 < chars.len() {
            let hex: String = chars[i + 1..i + 3].iter().collect();
            if let Ok(v) = u8::from_str_radix(&hex, 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/* ---------------- 宽度的编解码（阶段 89） ---------------- */

/// 把用户宽度编成一行文本：`列名:像素,列名2:像素`。
///
/// **转义跟列名一样**（`%` `,` `:`）——列名里可能有这些字符。
fn encode_widths(widths: &[WidthSpec]) -> String {
    let mut out = String::new();
    for (i, (name, w)) in widths.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&enc_one(name));
        out.push(':');
        out.push_str(&w.to_string());
    }
    out
}

/// 解一行文本回用户宽度（`encode_widths` 的逆）。
///
/// ⚠️ **非法值直接丢掉**（宽度 ≤ 0 或解析不出来）⇒ 手改坏的文件不该
/// 让界面炸掉，只当那一列没设过。
fn decode_widths(s: &str) -> Vec<WidthSpec> {
    let mut out = Vec::new();
    for part in s.split(',') {
        if part.is_empty() {
            continue;
        }
        // 名字里可能有 `:`（转义过的），所以只按**最后一个**冒号切
        let Some(pos) = part.rfind(':') else {
            continue;
        };
        let name = dec_one(&part[..pos]);
        let Ok(w) = part[pos + 1..].parse::<i32>() else {
            continue;
        };
        if w <= 0 || w > MAX_WIDTH {
            continue;
        }
        out.push((name, w));
    }
    out
}

/* ---------------- 纯逻辑 ---------------- */

/// 把存下来的布局套到当前结果集的列上 → `(原始列下标, 是否显示)`。
///
/// **规则**：
/// - 存过的列：按**名字**找回下标，顺序按用户排的来；
/// - 存的时候还没有的列（表后来加了列）：**追加到末尾，默认显示**——
///   新列不该被悄悄藏起来；
/// - 存过、但现在没了的列（表后来删了列）：直接丢掉。
///
/// 没存过（`stored` 为空）= 全部显示、保持原序。
pub fn apply(cols: &[String], stored: &[ColSpec]) -> Vec<(usize, bool)> {
    if stored.is_empty() {
        return (0..cols.len()).map(|i| (i, true)).collect();
    }
    let mut out: Vec<(usize, bool)> = Vec::with_capacity(cols.len());
    let mut used = vec![false; cols.len()];
    for (name, visible) in stored {
        // 同名列取第一个还没占用的（`SELECT a, a` 这种少见但别崩）
        if let Some(i) = cols
            .iter()
            .enumerate()
            .find(|(i, c)| **c == *name && !used[*i])
            .map(|(i, _)| i)
        {
            used[i] = true;
            out.push((i, *visible));
        }
    }
    for (i, _) in cols.iter().enumerate() {
        if !used[i] {
            out.push((i, true));
        }
    }
    out
}

/// 把存下来的宽度套到当前结果集的列上 → `(原始列下标, 宽)`。
///
/// **规则**（跟 [`apply`] 一个口径）：
/// - 按**名字**找回原始列下标（藏过列、换了列顺序都不会错位）；
/// - 存过、但现在没了的列（表后来删了列）：直接丢掉；
/// - 没存过的列：**不在结果里** ⇒ 那列走「按内容自动分配」。
///
/// ⚠️ **为什么键是原始列下标**：网格里的用户宽度就是按原始下标存的
/// （`DbGrid::user_widths`）⇒ 这里转过去就能直接灌进网格。
pub fn apply_widths(cols: &[String], stored: &[WidthSpec]) -> Vec<(usize, i32)> {
    let mut out: Vec<(usize, i32)> = Vec::with_capacity(stored.len());
    let mut used = vec![false; cols.len()];
    for (name, w) in stored {
        if *w <= 0 || *w > MAX_WIDTH {
            continue;
        }
        // 同名列取第一个还没占用的（`SELECT a, a` 这种少见但别崩）
        if let Some(i) = cols
            .iter()
            .enumerate()
            .find(|(i, c)| **c == *name && !used[*i])
            .map(|(i, _)| i)
        {
            used[i] = true;
            out.push((i, *w));
        }
    }
    out
}

/* ---------------- 读 ---------------- */

/// 读某张表的列布局（按用户排的顺序）。
pub fn load(conn: &str, schema: &str, table: &str) -> Vec<ColSpec> {
    load_from(&ini_path(), conn, schema, table)
}

fn load_from(path: &std::path::Path, conn: &str, schema: &str, table: &str) -> Vec<ColSpec> {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }
    let g = group(conn, schema, table);
    let Ok(s) = kf.string(&g, "cols") else {
        return Vec::new();
    };
    decode(&s)
}

/* ---------------- 写 ---------------- */

/// 存某张表的列布局。
pub fn save(conn: &str, schema: &str, table: &str, cols: &[ColSpec]) {
    let _ = save_to(&ini_path(), conn, schema, table, cols);
}

fn save_to(
    path: &std::path::Path,
    conn: &str,
    schema: &str,
    table: &str,
    cols: &[ColSpec],
) -> bool {
    if cols.is_empty() {
        return false;
    }
    let kf = KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    let g = group(conn, schema, table);
    kf.set_string(&g, "cols", &encode(cols));
    write_kf(path, &kf)
}

/* ---------------- 宽度的读写（阶段 89） ---------------- */

/// 读某张表的用户宽度（`列名 → 像素`）。
pub fn load_widths(conn: &str, schema: &str, table: &str) -> Vec<WidthSpec> {
    load_widths_from(&ini_path(), conn, schema, table)
}

fn load_widths_from(
    path: &std::path::Path,
    conn: &str,
    schema: &str,
    table: &str,
) -> Vec<WidthSpec> {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }
    let g = group(conn, schema, table);
    let Ok(s) = kf.string(&g, "widths") else {
        return Vec::new();
    };
    decode_widths(&s)
}

/// 存某张表的用户宽度。
///
/// ⚠️ **与 `cols` 同一个分组、不同键** ⇒ 两边各存各的，谁也不覆盖谁
/// （`save_to` / `save_widths_to` 都是「读整个文件 → 改一个键 → 写回」）。
pub fn save_widths(conn: &str, schema: &str, table: &str, widths: &[WidthSpec]) {
    let _ = save_widths_to(&ini_path(), conn, schema, table, widths);
}

fn save_widths_to(
    path: &std::path::Path,
    conn: &str,
    schema: &str,
    table: &str,
    widths: &[WidthSpec],
) -> bool {
    if widths.is_empty() {
        return false;
    }
    let kf = KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    let g = group(conn, schema, table);
    kf.set_string(&g, "widths", &encode_widths(widths));
    write_kf(path, &kf)
}

/// **删掉**某张表存着的用户宽度（用户把宽度全恢复默认了）。
///
/// ⚠️ **为什么必须真的删掉**：宽度是「**整体覆盖**」写的 ⇒ 用户恢复默认后
/// 存的那份里已经没这一列了，但**旧的 `widths` 键还在文件里** ⇒ 下次打开
/// 又把旧宽度套上 ⇒ 用户看到「恢复了默认，下次打开又变回那样」。
///
/// ⚠️ **只删 `widths` 键**：同组的 `cols`（列显示设置）不能碰。
pub fn clear_widths(conn: &str, schema: &str, table: &str) {
    let _ = clear_widths_to(&ini_path(), conn, schema, table);
}

fn clear_widths_to(
    path: &std::path::Path,
    conn: &str,
    schema: &str,
    table: &str,
) -> bool {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return false;
    }
    let g = group(conn, schema, table);
    // ⚠️ gtk-rs 的 `has_key` / `keys` 返回 `Result`（组不存在就是 Err）
    match kf.has_key(&g, "widths") {
        Ok(true) => {}
        _ => return false,
    }
    kf.remove_key(&g, "widths");
    // 整组空了 ⇒ 把它也去掉，免得文件里留一堆空组
    // （⚠️ 判的是**键**为空，不是组为空 —— `groups()` 列的是所有组名）
    let empty = kf.keys(&g).map(|k| k.is_empty()).unwrap_or(true);
    if empty {
        let _ = kf.remove_group(&g);
    }
    write_kf(path, &kf)
}

/// 原子写：同目录临时文件 + rename。
///
/// 跟 `history.ini` 一个套路：直接 write 时两个标签同时写会把内容写重，
/// rename 保证读到的永远是完整文件。
fn write_kf(path: &std::path::Path, kf: &KeyFile) -> bool {
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let _ = crate::platform::perm::set_private_dir(dir);
    }
    let data = kf.to_data();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "columns.ini".into());
    let tmp = path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    if std::fs::write(&tmp, data.as_bytes()).is_err() {
        return false;
    }
    // 0600：跟凭据库、history.ini 一个待遇（表名、列名算半个敏感信息）
    let _ = crate::platform::perm::set_private_file(&tmp);
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::db::DbProto;

    fn cols(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pp_dbcols_{tag}_{}.ini", std::process::id()))
    }

    #[test]
    fn 编解码能往返() {
        let orig = vec![
            ("id".to_string(), true),
            ("名字".to_string(), false),
            ("a,b".to_string(), true),
            ("c:d".to_string(), false),
            ("100%".to_string(), true),
        ];
        let back = decode(&encode(&orig));
        assert_eq!(back, orig);
    }

    #[test]
    fn 空的布局就是全显示() {
        let c = cols(&["a", "b", "c"]);
        assert_eq!(apply(&c, &[]), vec![(0, true), (1, true), (2, true)]);
    }

    #[test]
    fn 按名字找回下标并保持顺序() {
        let c = cols(&["id", "name", "email"]);
        // 用户把 email 排到最前、藏掉 name
        let stored = vec![
            ("email".to_string(), true),
            ("id".to_string(), true),
            ("name".to_string(), false),
        ];
        assert_eq!(
            apply(&c, &stored),
            vec![(2, true), (0, true), (1, false)],
            "顺序按用户排的，下标按名字找回"
        );
    }

    #[test]
    fn 后来加的列追加到末尾且默认显示() {
        let c = cols(&["id", "name", "created_at"]);
        // 存的时候只有两列
        let stored = vec![("name".to_string(), false), ("id".to_string(), true)];
        assert_eq!(
            apply(&c, &stored),
            vec![(1, false), (0, true), (2, true)],
            "新列不该被悄悄藏起来"
        );
    }

    #[test]
    fn 删掉的列直接丢掉() {
        let c = cols(&["id", "email"]);
        let stored = vec![
            ("gone".to_string(), true),
            ("email".to_string(), true),
            ("id".to_string(), true),
        ];
        assert_eq!(apply(&c, &stored), vec![(1, true), (0, true)]);
    }

    #[test]
    fn 存了再读能拿回来() {
        let path = tmp_path("rt");
        let _ = std::fs::remove_file(&path);
        let orig = vec![
            ("id".to_string(), true),
            ("email".to_string(), false),
            ("a,b:c".to_string(), true),
        ];
        assert!(save_to(&path, "pg/conn", "public", "demo_users", &orig));
        let back = load_from(&path, "pg/conn", "public", "demo_users");
        assert_eq!(back, orig);
        // 别的表 / 别的连接不受影响
        assert!(load_from(&path, "pg/conn", "public", "other").is_empty());
        assert!(load_from(&path, "pg/other", "public", "demo_users").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn 连接标识带上库名和连接名() {
        let p = DbProfile {
            name: "本地 PG".into(),
            proto: DbProto::Postgres,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            dbname: "postgres".into(),
            remember: true,
        };
        let k = conn_key(&p);
        assert!(k.contains("postgres@127.0.0.1:5432"), "{k}");
        assert!(k.contains("postgres"), "{k}");
        // 同名不同地址 → 不同键
        let mut p2 = p.clone();
        p2.port = 5433;
        assert_ne!(k, conn_key(&p2));
    }

    #[test]
    fn 组名里的方括号要转掉() {
        let g = group("c[1]", "s]", "t\nx");
        assert!(!g.contains('[') && !g.contains(']') && !g.contains('\n'));
    }

    /* ---------------- 宽度（阶段 89） ---------------- */

    #[test]
    fn 宽度能往返() {
        let orig = vec![
            ("id".to_string(), 96),
            ("名字".to_string(), 220),
            ("a,b".to_string(), 80),
            ("c:d".to_string(), 61),
            ("100%".to_string(), 123),
        ];
        assert_eq!(decode_widths(&encode_widths(&orig)), orig);
    }

    #[test]
    fn 非法宽度直接丢掉() {
        let got = decode_widths("id:0,name:-5,x:abc,y:99999");
        assert!(got.is_empty(), "非法值不该进结果：{got:?}");
    }

    #[test]
    fn 宽度按名字找回下标() {
        let c = cols(&["id", "name", "email"]);
        let stored = vec![("email".to_string(), 220), ("id".to_string(), 96)];
        assert_eq!(
            apply_widths(&c, &stored),
            vec![(2, 220), (0, 96)],
            "按名字找回原始下标（藏过列也不会错位）"
        );
    }

    #[test]
    fn 宽度里没有的列不出现() {
        let c = cols(&["id", "name"]);
        let stored = vec![("gone".to_string(), 100)];
        assert!(apply_widths(&c, &stored).is_empty());
    }

    #[test]
    fn 宽度与列布局共存在一个分组() {
        let path = tmp_path("rw");
        let _ = std::fs::remove_file(&path);
        let cols_spec = vec![("id".to_string(), true), ("name".to_string(), false)];
        let widths = vec![("id".to_string(), 96)];
        assert!(save_to(&path, "pg/conn", "public", "t", &cols_spec));
        assert!(save_widths_to(&path, "pg/conn", "public", "t", &widths));
        // 两边都还在
        assert_eq!(load_from(&path, "pg/conn", "public", "t"), cols_spec);
        assert_eq!(load_widths_from(&path, "pg/conn", "public", "t"), widths);
        // 宽度单独写也不会把列布局冲掉
        assert!(save_widths_to(&path, "pg/conn", "public", "t", &widths));
        assert_eq!(load_from(&path, "pg/conn", "public", "t"), cols_spec);
        // 别的表不受影响
        assert!(load_widths_from(&path, "pg/conn", "public", "other").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn 恢复默认宽度要把存的那份删掉() {
        let path = tmp_path("clear");
        let _ = std::fs::remove_file(&path);
        let cols_spec = vec![("id".to_string(), true)];
        let widths = vec![("id".to_string(), 96)];
        assert!(save_to(&path, "pg/conn", "public", "t", &cols_spec));
        assert!(save_widths_to(&path, "pg/conn", "public", "t", &widths));
        assert!(clear_widths_to(&path, "pg/conn", "public", "t"));
        // 宽度没了，列显示设置还在
        assert!(load_widths_from(&path, "pg/conn", "public", "t").is_empty());
        assert_eq!(load_from(&path, "pg/conn", "public", "t"), cols_spec);
        let _ = std::fs::remove_file(&path);
    }
}
