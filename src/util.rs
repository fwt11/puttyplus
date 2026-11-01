//! 与 C 版 util.c 对应的通用小工具（大小、时间、路径、文本清洗）。

use chrono::{Datelike, TimeZone};

/// 在 PATH 里查找可执行文件（对应 g_find_program_in_path）。
pub fn find_program_in_path(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        let p = std::path::Path::new(name);
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.is_absolute() || dir.exists() {
            let cand = dir.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 与 C 的 atoi 语义一致：跳过前导空白后解析可选符号整数，解析失败返回 0。
pub fn atoi_like(s: &str) -> i64 {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return 0;
    }
    s[..i].parse::<i64>().unwrap_or(0)
}

/// 人类可读的文件大小：1023 以内按字节显示，之后 KB/MB/... 保留一位小数。
pub fn human_size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    const UNIT: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    let mut v = n as f64;
    let mut u: i32 = -1;
    while v >= 1024.0 && u < 4 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNIT[u as usize])
}

/// mtime 的**完整**显示："YYYY-MM-DD HH:MM" 本地时间；0 显示 "-"。
pub fn mtime_full_str(t: i64) -> String {
    if t == 0 {
        return "-".into();
    }
    chrono::Local
        .timestamp_opt(t, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

/// mtime 的**短**显示（列里用；阶段 86 / 复查 §2.2）。
///
/// 审计/复查的原话："运维找文件恰恰常看『最近改过什么』，**藏掉 mtime 是把
/// 有用信息当包袱**" ⇒ **保留显示**，但把格式缩短 —— 120px 的列才放得下
/// （完整格式 `2026-10-27 14:32` 会被硬裁成 `2026-10-27 0…`）。
///
/// 规则：
/// - **今天** ⇒ 只写时间（`14:32`）；
/// - **今年** ⇒ 省略年份（`10-27 14:32`）；
/// - **别的年** ⇒ 完整（`2024-10-27 14:32`）。
///
/// ⚠️ 完整时间在**悬停 tooltip** 里给（`mtime_full_str`）——短格式只是为了
/// 塞进列宽，不是把信息丢了。
pub fn mtime_str(t: i64) -> String {
    if t == 0 {
        return "-".into();
    }
    let Some(d) = chrono::Local.timestamp_opt(t, 0).single() else {
        return "-".into();
    };
    let now = chrono::Local::now();
    if d.date_naive() == now.date_naive() {
        d.format("%H:%M").to_string()
    } else if d.year() == now.year() {
        d.format("%m-%d %H:%M").to_string()
    } else {
        d.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// POSIX 风格路径拼接（远端路径一律用 '/'）。
pub fn path_join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        return name.to_string();
    }
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// 上一级目录；根目录或不含 '/' 时返回 "/"。
pub fn path_parent(path: &str) -> String {
    match path.rfind('/') {
        None | Some(0) => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

/// 最后一个 '/' 之后的部分。
pub fn path_basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// 把 SFTP 返回的任意字节序列变成可显示的 String：
/// 合法 UTF-8 原样保留，否则把非 ASCII / 控制字节替换为 '?'。
pub fn utf8_sanitize(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    bytes
        .iter()
        .map(|&b| if b < 0x80 && b >= 0x20 { b as char } else { '?' })
        .collect()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    /// 建一个隔离的临时“主目录”用于 config/args 单测。
    pub fn temp_home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pp_rust_test_{}_{:?}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi() {
        assert_eq!(atoi_like("22"), 22);
        assert_eq!(atoi_like("22x"), 22);
        assert_eq!(atoi_like("  42"), 42);
        assert_eq!(atoi_like("abc"), 0);
        assert_eq!(atoi_like(""), 0);
        assert_eq!(atoi_like("-1"), -1);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn paths() {
        assert_eq!(path_join("/a", "b"), "/a/b");
        assert_eq!(path_join("/a/", "b"), "/a/b");
        assert_eq!(path_join("", "b"), "b");
        assert_eq!(path_parent("/a/b/c"), "/a/b");
        assert_eq!(path_parent("/a"), "/");
        assert_eq!(path_parent("/"), "/");
        assert_eq!(path_parent("abc"), "/");
        assert_eq!(path_basename("/a/b/c.txt"), "c.txt");
        assert_eq!(path_basename("c.txt"), "c.txt");
    }

    #[test]
    fn sanitize() {
        assert_eq!(utf8_sanitize(b"hello"), "hello");
        assert_eq!(utf8_sanitize("中文".as_bytes()), "中文");
        assert_eq!(utf8_sanitize(&[0x61, 0xff, 0x62]), "a?b");
        // 合法 UTF-8 里的控制字符同样原样保留（与 C 版 g_utf8_validate 行为一致）
        assert_eq!(utf8_sanitize(&[0x01, 0x41]), "\u{1}A");
    }

    /// 阶段 86 / 复查 §2.2、N2：修改时间列改成**短格式**。
    ///
    /// ⚠️ 完整格式 `2026-10-27 14:32` 在 120px 的列里会被硬裁成
    /// 「2026-10-27 0…」⇒ 用户看到半截时间，**比不显示还别扭**。
    #[test]
    fn 修改时间是短格式() {
        use chrono::TimeZone;

        assert_eq!(mtime_str(0), "-");
        assert_eq!(mtime_full_str(0), "-");

        let now = chrono::Local::now();
        let today = chrono::Local
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 14, 32, 0)
            .single()
            .unwrap()
            .timestamp();
        // 今天 ⇒ 只写时间
        assert_eq!(mtime_str(today), "14:32");
        // 完整写法给 tooltip
        assert_eq!(mtime_full_str(today).len(), "2026-10-27 14:32".len());

        // 今年内 ⇒ 省略年份
        let same_year = chrono::Local
            .with_ymd_and_hms(now.year(), 3, 4, 9, 5, 0)
            .single()
            .unwrap()
            .timestamp();
        if same_year != today {
            assert_eq!(mtime_str(same_year), "03-04 09:05");
        }

        // 别的年 ⇒ 完整
        let old = chrono::Local
            .with_ymd_and_hms(2021, 12, 31, 23, 59, 0)
            .single()
            .unwrap()
            .timestamp();
        assert_eq!(mtime_str(old), "2021-12-31 23:59");
    }
}
