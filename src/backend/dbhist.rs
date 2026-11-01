//! 数据库 SQL 执行历史（阶段 11.2）：`~/.config/puttyplus/history.ini`。
//!
//! **按会话分开**：一个 SSH 会话下的数据库工作台有自己的历史——全混在一起
//! 没法看（Navicat 的 History 也是按连接分的）。
//!
//! ```ini
//! [root@10.0.0.1:22]
//! t0=10-25 16:59:03
//! q0=SELECT * FROM demo_users
//! t1=10-25 16:58:11
//! q1=SELECT count(*) FROM demo_users
//! ```
//!
//! **时间与 SQL 分开两个键**：值里塞分隔符会有歧义（SQL 里什么字符都可能
//! 出现），分开放就没这个问题。
//!
//! **多行 SQL 存得下**：GKeyFile 的值按单行存，`\n` 由
//! `g_key_file_set_string` 自己转义成 `\n` 两字符，读回来还是原样。
//!
//! **不上锁**：两个标签同时连同一个会话键时，后写的会覆盖先写的。跟
//! `databases.ini` 一样的取舍——历史丢一条不至于出人命（重打一遍就行），
//! 但连接配置丢一条可能连错库。
//!
//! **里面可能有敏感内容**（`INSERT INTO users (password) VALUES …`），
//! 所以文件权限是 **0600**、目录是 **0700**，跟凭据库一个待遇。它相当于
//! shell 历史，落在本机用户自己的目录里。

use std::path::PathBuf;

use glib::KeyFile;

/// 一条历史。
#[derive(Debug, Clone, PartialEq)]
pub struct HistEntry {
    /// 执行时间（本地时间，形如 `10-25 16:59:03`）
    pub time: String,
    /// SQL 原文
    pub sql: String,
}

/// 每个会话最多留多少条。
///
/// 留太多文件会涨得很快（一条长查询能几百字节），留太少又不够翻。
const CAP: usize = 200;

fn ini_path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想看/改别的文件，不必动真实配置。
    if let Some(p) = std::env::var_os("PUTTYPLUS_HIST_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir()
        .join("puttyplus")
        .join("history.ini")
}

/// KeyFile 组名不能含 `[` `]` 与换行；会话名可能带这些字符。
fn group(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '[' | ']' | '\n' | '\r' => '_',
            _ => c,
        })
        .collect()
}

/// 现在的本地时间（`10-25 16:59:03`）。
pub fn now_time() -> String {
    chrono::Local::now().format("%m-%d %H:%M:%S").to_string()
}

/// 历史的分组键：`<会话键>/<数据库连接名>`。
///
/// **为什么按连接分**：一个 SSH 会话下可以挂多个数据库连接（阶段 10），
/// 历史混在一起就分不清"这条 SQL 是在哪个库上跑的"——Navicat 的 History
/// 也是按连接分的。
pub fn hist_key(session: &str, conn: &str) -> String {
    format!("{session}/{conn}")
}

/* ---------------- 纯逻辑 ---------------- */

/// 把一条 SQL 记进历史列表（**新的在前**，纯逻辑，单测跑这里）。
///
/// **同一条 SQL 不留两条一样的**：老的那条被挪到顶上、时间更新成最新的。
/// 历史是用来找"刚才那条"的，不是用来数"我跑了几遍"的——连着点两次执行，
/// 列表里一长串一模一样的行，谁也找不到想要的那条。
pub fn push_entry(list: &mut Vec<HistEntry>, sql: &str, time: &str) {
    let sql = sql.trim();
    if sql.is_empty() {
        return;
    }
    list.retain(|e| e.sql != sql);
    list.insert(
        0,
        HistEntry {
            time: time.to_string(),
            sql: sql.to_string(),
        },
    );
    list.truncate(CAP);
}

/* ---------------- 读 ---------------- */

/// 读某会话的历史（新的在前）。
pub fn list(key: &str) -> Vec<HistEntry> {
    list_from(&ini_path(), key)
}

fn list_from(path: &std::path::Path, key: &str) -> Vec<HistEntry> {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }
    let g = group(key);
    let mut out: Vec<HistEntry> = Vec::new();
    // q0 是最新的，编号连续；中间断了就停（后面的更旧，不重要）
    for i in 0..CAP {
        let Ok(sql) = kf.string(&g, &format!("q{i}")) else {
            break;
        };
        let time = kf.string(&g, &format!("t{i}")).unwrap_or_default();
        out.push(HistEntry {
            time: time.to_string(),
            sql: sql.to_string(),
        });
    }
    out
}

/* ---------------- 写 ---------------- */

/// 记一条，返回记完之后的完整历史（新的在前）。
pub fn add(key: &str, sql: &str) -> Vec<HistEntry> {
    add_to(&ini_path(), key, sql)
}

fn add_to(path: &std::path::Path, key: &str, sql: &str) -> Vec<HistEntry> {
    let mut list = list_from(path, key);
    push_entry(&mut list, sql, &now_time());
    let _ = write_to(path, key, &list);
    list
}

/// 清空某会话的历史。
pub fn clear(key: &str) {
    let _ = clear_to(&ini_path(), key);
}

fn clear_to(path: &std::path::Path, key: &str) -> bool {
    let kf = KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    let g = group(key);
    if !kf.has_group(&g) {
        return false;
    }
    kf.remove_group(&g);
    write_kf(path, &kf)
}

/// 把历史写回文件（整个组覆盖写）。
fn write_to(path: &std::path::Path, key: &str, list: &[HistEntry]) -> bool {
    let kf = KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    let g = group(key);
    // 先清掉旧的（条数可能变少），再写新的
    if kf.has_group(&g) {
        kf.remove_group(&g);
    }
    for (i, e) in list.iter().enumerate() {
        kf.set_string(&g, &format!("q{i}"), &e.sql);
        kf.set_string(&g, &format!("t{i}"), &e.time);
    }
    write_kf(path, &kf)
}

/// 原子写：同目录临时文件 + rename。
///
/// **为什么要原子写**：直接 write 时，两个标签同时记历史（同一会话键）会因
/// 文件偏移交错把内容写重，rename 保证读到的永远是完整文件。
fn write_kf(path: &std::path::Path, kf: &KeyFile) -> bool {
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    }
    let data = kf.to_data();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "history.ini".into());
    let tmp = path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    if std::fs::write(&tmp, data.as_bytes()).is_err() {
        return false;
    }
    // 0600：里面可能有密码之类的敏感 SQL
    let _ = std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 同一条sql不留两份() {
        let mut list = Vec::new();
        push_entry(&mut list, "SELECT 1", "10-25 10:00:00");
        push_entry(&mut list, "SELECT 2", "10-25 10:00:01");
        assert_eq!(list.len(), 2);
        // 再跑一遍 SELECT 1 → 不该多出一行，时间更新成最新的
        push_entry(&mut list, "SELECT 1", "10-25 10:00:02");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].sql, "SELECT 1");
        assert_eq!(list[0].time, "10-25 10:00:02");
        assert_eq!(list[1].sql, "SELECT 2");
    }

    #[test]
    fn 新的在前() {
        let mut list = Vec::new();
        push_entry(&mut list, "A", "t1");
        push_entry(&mut list, "B", "t2");
        assert_eq!(list[0].sql, "B");
        assert_eq!(list[1].sql, "A");
    }

    #[test]
    fn 空语句不记() {
        let mut list = Vec::new();
        push_entry(&mut list, "   \n ", "t");
        assert!(list.is_empty());
    }

    #[test]
    fn 语句前后空白去掉() {
        let mut list = Vec::new();
        push_entry(&mut list, "  SELECT 1  ", "t");
        assert_eq!(list[0].sql, "SELECT 1");
    }

    #[test]
    fn 超过上限的丢最旧的() {
        let mut list = Vec::new();
        for i in 0..CAP + 10 {
            push_entry(&mut list, &format!("SELECT {i}"), "t");
        }
        assert_eq!(list.len(), CAP);
        // 最新的在最前，最旧的被丢
        assert_eq!(list[0].sql, format!("SELECT {}", CAP + 9));
        assert!(!list.iter().any(|e| e.sql == "SELECT 0"));
    }

    #[test]
    fn 分组键按会话加连接() {
        // 一个 SSH 会话下多个数据库连接 → 历史要分开
        assert_eq!(hist_key("root@h:22", "本地 PG"), "root@h:22/本地 PG");
        assert_ne!(
            hist_key("root@h:22", "本地 PG"),
            hist_key("root@h:22", "测试库 MySQL")
        );
        // 不同会话也分开
        assert_ne!(
            hist_key("root@h:22", "本地 PG"),
            hist_key("root@h:23", "本地 PG")
        );
    }

    #[test]
    fn 多行sql能往返() {
        let dir = std::env::temp_dir().join(format!("pp_hist_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.ini");
        let sql = "SELECT a,\n       b\nFROM t\nWHERE x = 'a\\b'";
        let list = add_to(&path, "s:测试", sql);
        assert_eq!(list.len(), 1);
        let back = list_from(&path, "s:测试");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].sql, sql);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 清空历史() {
        let dir = std::env::temp_dir().join(format!("pp_hist_clear_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("history.ini");
        add_to(&path, "s:a", "SELECT 1");
        add_to(&path, "s:b", "SELECT 2");
        assert_eq!(list_from(&path, "s:a").len(), 1);
        clear_to(&path, "s:a");
        assert!(list_from(&path, "s:a").is_empty());
        // 别的会话不受影响
        assert_eq!(list_from(&path, "s:b").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
