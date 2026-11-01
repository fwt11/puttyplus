//! SFTP 后端（对应 C 版 sftp_backend.c）：russh + russh-sftp 实现。
//!
//! 连接与所有操作都运行在 tokio 运行时上；UI 侧（Phase 4）通过消息通道
//! 与本模块交互，连接对象由 worker 独占持有。

pub mod checksum;
pub mod conn;
pub mod db;
pub mod dbcols;
pub mod dbcsv;
pub mod dbexport;
pub mod ermodel;
pub mod dbhist;
pub mod dbmeta;
pub mod dbstore;
pub mod forward;
pub mod known_hosts;
pub mod mirror;
pub mod proxy;
pub mod resume;
pub mod selftest;
pub mod sftp;

/// 后端错误：消息直接面向用户（与 C 版文案保持一致）。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

pub type BackendResult<T> = Result<T, BackendError>;

impl BackendError {
    pub fn new<S: Into<String>>(msg: S) -> Self {
        BackendError(msg.into())
    }
}

/// 目录列表项（对应 C 版 SftpEntry）。
#[derive(Debug, Clone)]
pub struct SftpEntry {
    pub name: String,
    /// SFTP longname（ls -l 风格描述行，部分服务器为空）
    pub longname: String,
    /// 如 "drwxr-xr-x"
    pub perm: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// Unix 秒；0 表示服务器未提供
    pub mtime: i64,
}

/// 与 C 版 perm_string 一致：'l'/'d'/'-' 开头 + rwx 九位（低位在前）。
pub fn perm_string(p: u32, is_dir: bool, is_link: bool) -> String {
    let mut s = [0u8; 10];
    s[0] = if is_link {
        b'l'
    } else if is_dir {
        b'd'
    } else {
        b'-'
    };
    const XWR: [u8; 3] = [b'x', b'w', b'r'];
    for i in 0..9usize {
        s[9 - i] = if (p >> i) & 1 == 1 { XWR[i % 3] } else { b'-' };
    }
    String::from_utf8_lossy(&s).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm() {
        // 0755 目录：rwxr-xr-x
        assert_eq!(perm_string(0o40000 | 0o755, true, false), "drwxr-xr-x");
        // 符号链接
        assert_eq!(perm_string(0o120000 | 0o777, false, true), "lrwxrwxrwx");
        // 普通文件 0644：rw-r--r--
        assert_eq!(perm_string(0o100000 | 0o644, false, false), "-rw-r--r--");
    }
}
