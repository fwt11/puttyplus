//! 本地路径 ↔ `file://` URI（跨平台版 阶段 C5）。
//!
//! ## 为什么单独一个模块
//!
//! 拖放上传 / 剪贴板粘贴上传走的是 `text/uri-list`，里面装的是 **URI**
//! 不是本地路径。手工拼 URI 在 Windows 上必错：
//!
//! ```text
//! format!("file://{path}")   // Windows: file://C:\dir\a.txt
//!                             //            ↑反斜杠 + 少一个斜杠
//! ```
//!
//! ⇒ **生成**与**解析**都交给 GIO（它负责百分号编码、Windows 盘符、
//! UNC），本地路径的「是不是绝对路径」判定在
//! [`super::localpath::is_abs`]（Windows 上认盘符与 UNC，不再一律
//! 要求 `/` 开头——那样会把 Windows 的路径全丢掉）。
//!
//! ⚠️ 本模块**依赖 GIO**（`gtk::gio`），而 [`super::localpath`] 只用
//! `std::path` ⇒ 路径那一半可以单独拿去做 Windows 目标的交叉编译检查。

use std::path::PathBuf;

use gtk::gio::prelude::*;

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
    fn file_uri_往返() {
        // ⚠️ 用临时目录而不是写死 `/tmp` ⇒ Windows 上也能跑。
        let p = std::env::temp_dir().join("pp_localuri_test.txt");
        let uri = to_file_uri(p.to_str().unwrap());
        assert!(uri.starts_with("file:///"), "{uri}");
        assert_eq!(from_file_uri(&uri), Some(p));

        // 带空格的路径要能编码/解码回来
        let p2 = std::env::temp_dir().join("pp localuri 空格.txt");
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
