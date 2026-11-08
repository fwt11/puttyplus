//! 本地路径（跨平台版 阶段 C5）。
//!
//! ## 为什么要有这一层
//!
//! 面板里同时存在**两种路径**，语义不同：
//!
//! - **远端路径**（SFTP 那一侧）：永远是 POSIX（`/srv/x`），分隔符恒为 `/`
//!   —— 远端是什么系统我们管不着，也管不着；
//! - **本地路径**（本机文件系统）：Windows 上是 `C:\dir\x`（分隔符 `\`），
//!   还可能有 UNC（`\\server\share`）与盘符相对路径（`C:x`）。
//!
//! 改造之前两者**共用** `util::path_join` / `path_parent` / `path_basename`
//! （纯 POSIX 语义）。在 Windows 上这一共用会踩三个坑：
//!
//! 1. **本地面板「上级」跳到根**：`path_parent("C:\\dir")` 找不到 `/`
//!    ⇒ 返回 `"/"` ⇒ 一下跳到文件系统根目录；
//! 2. **本地文件重命名默认名错**：`path_basename("C:\\a\\b.txt")` 返回**整串**
//!    （没找到 `/`）⇒ 重命名对话框里预填的是完整路径；
//! 3. **上传后远端文件名带盘符**：`path_basename(本地路径)` 同样是整串
//!    ⇒ 远端出现一个叫 `C:\a\b.txt` 的文件。
//!
//! ⇒ 本模块给出**本地语义**的三个函数（基于 `std::path`）；
//!    远端那边继续用 POSIX 版本（`crate::util::path_*`），**不要混用**。
//!
//! ## 另一个坑：`file://` URI
//!
//! 拖放上传 / 剪贴板粘贴上传走的是 `text/uri-list`（`file:///…`）。
//! 之前是手搓字符串 `format!("file://{p}")` ⇒ Windows 上得到
//! `file://C:\dir\a.txt`（反斜杠 + 少一个斜杠）⇒ 源端与目标端都认不出。
//! 现在统一走 [`to_file_uri`] / [`from_file_uri`]（GIO 负责百分号编码
//! 与 Windows 盘符），本地路径的判定用 [`is_abs`]（不再一律要求 `/` 开头）。

use std::path::{Path, PathBuf};

use gtk::gio::prelude::*;

/// 本地路径是不是「绝对路径」。
///
/// ⚠️ **远端路径不要用这个判定**：远端永远是 POSIX 绝对路径（`/…`），
/// 而本地在 Windows 上是盘符/UNC 形式。
pub fn is_abs(s: &str) -> bool {
    #[cfg(windows)]
    {
        let b = s.as_bytes();
        // 盘符绝对路径：C:\… 或 C:/…
        let drive = b.len() >= 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
        // UNC：\\server\share；MSYS 风格：//server/share
        let unc = s.starts_with("\\\\") || s.starts_with("//");
        drive || unc
    }
    #[cfg(not(windows))]
    {
        s.starts_with('/')
    }
}

/// 上一级目录（本地语义）。
///
/// 根目录（`/`、`C:\`）与没有上级的路径**原样返回**。
pub fn parent(path: &str) -> String {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// 拼接目录与名字（本地语义）。
pub fn join(dir: &str, name: &str) -> String {
    Path::new(dir).join(name).to_string_lossy().into_owned()
}

/// 最后一段（本地语义）。
///
/// `C:\a\b.txt` → `b.txt`；`/a/b.txt` → `b.txt`。
///
/// ⚠️ `..` / `.` / 根目录这类「没有文件名」的路径没有 `file_name()`
/// ⇒ 退回按 `/` 切最后一段（与 POSIX 版行为一致）。
pub fn basename(path: &str) -> String {
    match Path::new(path).file_name() {
        Some(s) => s.to_string_lossy().into_owned(),
        None => path.rsplit('/').next().unwrap_or(path).to_string(),
    }
}

/// 本地路径 → `file://` URI（百分号编码交给 GIO）。
///
/// - Linux：`/tmp/a b.txt` → `file:///tmp/a%20b.txt`
/// - Windows：`C:\a b.txt` → `file:///C:/a%20b.txt`
pub fn to_file_uri(path: &str) -> String {
    gtk::gio::File::for_path(path).uri().to_string()
}

/// `file://` URI → 本地路径。
///
/// 解析不出来（不是 `file://`、带主机名等）返回 `None`。
pub fn from_file_uri(uri: &str) -> Option<PathBuf> {
    gtk::gio::File::for_uri(uri).path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 绝对路径判定() {
        assert!(!is_abs("tmp/x"));
        assert!(!is_abs("x.txt"));
        #[cfg(unix)]
        {
            assert!(is_abs("/tmp/x"));
        }
        #[cfg(windows)]
        {
            assert!(is_abs("C:\\dir\\x"));
            assert!(is_abs("C:/dir/x"));
            assert!(is_abs("\\\\server\\share\\x"));
            assert!(!is_abs("C:x.txt"));
            assert!(!is_abs("dir\\x"));
            // MSYS 风格的 `/tmp/x` 在 Windows 上不算绝对路径
            assert!(!is_abs("/tmp/x"));
        }
    }

    #[test]
    fn 上一级目录() {
        #[cfg(unix)]
        {
            assert_eq!(parent("/tmp/a/b"), "/tmp/a");
            assert_eq!(parent("/tmp/a"), "/tmp");
            // 根目录没有上一级 ⇒ 原样
            assert_eq!(parent("/"), "/");
        }
        #[cfg(windows)]
        {
            assert_eq!(parent("C:\\a\\b"), "C:\\a");
            assert_eq!(parent("C:\\a"), "C:\\");
            assert_eq!(parent("C:\\"), "C:\\");
        }
    }

    #[test]
    fn 拼接与取名() {
        #[cfg(unix)]
        {
            assert_eq!(join("/tmp", "a.txt"), "/tmp/a.txt");
        }
        #[cfg(windows)]
        {
            assert_eq!(join("C:\\dir", "a.txt"), "C:\\dir\\a.txt");
            assert_eq!(join("/tmp", "a.txt"), "/tmp\\a.txt");
        }
        // POSIX 路径在两个平台上取名都一样
        assert_eq!(basename("/tmp/a.txt"), "a.txt");
        #[cfg(windows)]
        {
            // ⚠️ 这两条在 POSIX 版函数上会错（返回整串 / 找不到 `/`）
            assert_eq!(basename("C:\\dir\\a.txt"), "a.txt");
            assert_eq!(parent("C:\\dir"), "C:\\");
        }
    }

    #[test]
    fn file_uri_往返() {
        // ⚠️ 用临时目录而不是写死 `/tmp` ⇒ Windows 上也能跑。
        let p = std::env::temp_dir().join("pp_localpath_test.txt");
        let uri = to_file_uri(p.to_str().unwrap());
        assert!(uri.starts_with("file:///"), "{uri}");
        assert_eq!(from_file_uri(&uri), Some(p));

        // 带空格的路径要能编码/解码回来
        let p2 = std::env::temp_dir().join("pp localpath 空格.txt");
        let uri = to_file_uri(p2.to_str().unwrap());
        assert!(!uri.contains(' '), "空格应当被编码：{uri}");
        assert_eq!(from_file_uri(&uri), Some(p2));

        #[cfg(windows)]
        {
            let uri = to_file_uri("C:\\dir\\a b.txt");
            assert!(uri.starts_with("file:///C:/"), "{uri}");
            assert!(!uri.contains('\\'), "URI 里不该有反斜杠：{uri}");
        }
    }
}
