//! 路径相关（跨平台版 阶段 C0）。
//!
//! ## 约定
//!
//! - **home 目录**：`glib::home_dir()`。Windows 上 GLib 走 `USERPROFILE`，
//!   Linux 上走 `HOME` ⇒ 已经是跨平台的，这里只做**收口**（业务代码不再
//!   直接调 `glib::home_dir()`）。
//! - **配置目录**：`glib::user_config_dir()/puttyplus`。
//!   Windows 上 `user_config_dir()` = `%APPDATA%`（漫游），符合惯例。
//! - **`~` 展开**：**只展开开头的 `~` 或 `~/`**；`~user/x` **不展开**
//!   （这是 PuTTY 的行为，也是 C 版的**命令行契约**，不能改）。

use std::path::{Path, PathBuf};

/// 用户 home 目录。
pub fn home() -> PathBuf {
    glib::home_dir()
}

/// 应用配置目录（`…/puttyplus`）。
///
/// 各 `ini`（最近连接、设置、数据库连接、SQL 历史、列显示设置）都在这儿。
pub fn config_dir() -> PathBuf {
    glib::user_config_dir().join("puttyplus")
}

/// 展开开头的 `~` / `~/`。
///
/// ⚠️ **只展开开头**：`~user/x` 原样保留（PuTTY/C 版契约）。
/// ⚠️ `home` 传进来是为了**可测**（测试里用临时目录，不碰用户真实 home）。
pub fn expand_tilde(p: &str, home: &Path) -> String {
    crate::config::expand_tilde(p, home)
}

/// 用真实 home 展开 `~`（给业务代码用的便捷版）。
pub fn expand_tilde_home(p: &str) -> String {
    expand_tilde(p, &home())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 配置目录里带_puttyplus_() {
        let d = config_dir();
        assert!(
            d.ends_with("puttyplus"),
            "配置目录应当以 puttyplus 结尾：{}",
            d.display()
        );
    }

    #[test]
    fn 波浪号展开规则与契约一致() {
        let home = Path::new("/tmp/测试home");
        // 开头的 ~ 与 ~/ 都展开
        // ⚠️ 期望值用 `Path::join` 写：**分隔符是平台的**
        //    （Windows 上是 `\`）⇒ 断言在两个平台上都对。
        assert_eq!(
            expand_tilde("~/.ssh/id_rsa", home),
            home.join(".ssh/id_rsa").to_str().unwrap()
        );
        assert_eq!(expand_tilde("~", home), home.to_str().unwrap());
        // ~user/x 不展开
        assert_eq!(expand_tilde("~someone/x", home), "~someone/x");
        // 中间的 ~ 不展开
        assert_eq!(expand_tilde("/a/~/b", home), "/a/~/b");
        // 不含 ~ 的原样返回
        assert_eq!(expand_tilde("/etc/ssh/config", home), "/etc/ssh/config");
    }
}
