//! 文件权限（POSIX 语义）—— Windows 上没有 POSIX 权限，折算处理。
//!
//! ## 为什么要有这一层
//!
//! 凭据库（`vault.rs`）、连接配置（`connmgr.rs`）、数据库工作台的
//! 配置文件……都要「**只有本人能读**」。POSIX 上就是 `chmod 0600`，
//! 但 `std::os::unix::fs::PermissionsExt` **只有 Unix 有** ⇒ 直接用
//! 的话 Windows 上根本编不过。
//!
//! ⇒ 统一收口在这里：
//! - Unix：真的 `chmod`（行为与改造前**逐字节一致**）；
//! - Windows：**没有 POSIX 权限**，靠 Windows 的 ACL 保护 ⇒ 这里
//!   只保证「文件是普通用户可写」（不额外加只读属性），权限列
//!   按「只读属性」折算一个近似值给人看。
//!
//! ⚠️ 界面上的权限列在 Windows 上是**折算值**，不是真的 POSIX 位。

use std::io;
use std::path::Path;

/// 文件的 POSIX 权限位。
///
/// ⚠️ Windows 上是**折算值**：目录 0o755；文件按「只读属性」给
/// 0o444 / 0o644（只是给界面看的，真保护靠 ACL）。
pub fn mode_of(md: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.mode()
    }
    #[cfg(not(unix))]
    {
        if md.is_dir() {
            0o755
        } else if md.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

/// 修改时间（Unix 秒）。
pub fn mtime_of(md: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.mtime()
    }
    #[cfg(not(unix))]
    {
        md.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// 设成「只有本人能读写」（POSIX 0600）。
pub fn set_private_file(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        // Windows：没有 POSIX 位 ⇒ 什么都不做（靠 ACL）。
        Ok(())
    }
}

/// 设成「只有本人能进」（POSIX 0700）。
pub fn set_private_dir(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        Ok(())
    }
}

/// 建目录（**建的时候**就是 0700 ⇒ 不留「先建宽权限再收紧」的窗口）。
pub fn create_private_dir(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(p)
    }
    #[cfg(not(unix))]
    {
        std::fs::DirBuilder::new().create(p)
    }
}

/// 打开（或新建）一个「只有本人能读写」的文件。
///
/// ⚠️ Unix 上是**建的时候**就带 0600（不是建完再 chmod ⇒ 不留窗口）。
pub fn open_private_file(p: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(p)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(p)
    }
}
