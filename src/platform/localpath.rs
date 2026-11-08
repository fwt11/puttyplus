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
//! ⇒ URI 那部分拆到 [`super::localuri`]（要 GIO），**本模块只留纯 std
//!    的部分**——这样 Windows 目标下可以单独拿它做交叉编译检查
//!    （`cargo check --target x86_64-pc-windows-msvc`）。

use std::path::Path;

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

/// 看起来是不是「本地绝对路径」（**不分运行平台**）。
///
/// 与 [`is_abs`] 的区别：这里**连盘符/UNC 也认**。
///
/// 用途：有些判定是「这个字符串像不像一个绝对路径」（比如从
/// `~/.ssh/config` 里读出来的 `IdentityFile`）⇒ 这种判定必须
/// **在 Linux 上也能把 Windows 的形态测到**，不能跟着运行平台变。
///
/// ⚠️ 与 [`is_abs`] 的已知差异：以 `/` 开头在 Windows 上也算绝对
///    （MSYS 风格路径在原生程序里其实是「当前盘根目录下的相对路径」，
///    但把它当成绝对路径更安全 —— 宁可原样保留，别给它乱补目录）。
pub fn is_abs_shape(s: &str) -> bool {
    let b = s.as_bytes();
    // 盘符绝对路径：C:\… 或 C:/…（C:x 这种「盘符相对」不算）
    let drive = b.len() >= 3 && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    // UNC：\\server\share；MSYS 风格：//server/share
    let unc = s.starts_with("\\\\") || s.starts_with("//");
    drive || unc || s.starts_with('/')
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

    /// 形状判定**不看运行平台** ⇒ 两种平台上断言一样。
    #[test]
    fn 绝对路径的形状判定不分平台() {
        assert!(is_abs_shape("/tmp/x"));
        assert!(is_abs_shape("C:\\dir\\x"));
        assert!(is_abs_shape("C:/dir/x"));
        assert!(is_abs_shape("\\\\server\\share\\x"));
        assert!(is_abs_shape("//server/share/x"));
        // 盘符相对（C:x）不算绝对
        assert!(!is_abs_shape("C:x.txt"));
        assert!(!is_abs_shape("dir\\x"));
        assert!(!is_abs_shape("x.txt"));
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
}
