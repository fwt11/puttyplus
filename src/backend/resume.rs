//! 断点续传的"半截文件"约定（3.5）。
//!
//! 两个方向都先写 `<目标名>.pp-part`，成功后再改名成正式名：
//! - 正式名在传输完成前**永远不出现**。断点续传、取消、进程被杀，
//!   都不会让人（也不会让"跳过已存在"的判断）误以为文件是完整的
//! - 半截文件的名字由目标名确定，续传时不需要额外的会话状态
//!
//! 光有半截文件还不够：得确认它确实是**当前这个源**的前缀。否则把
//! 一个内容不同、恰好更小的同名文件接到后面去，产物是"看起来完整的
//! 坏文件"——比慢几十倍从头传更糟。所以每个半截文件在缓存目录里配一个
//! mark（源的大小 + mtime），续传时两端对上才复用，对不上就删掉重来。

use std::path::{Path, PathBuf};

/// 半截文件后缀。
pub const PART_SUFFIX: &str = ".pp-part";

/// 半截文件的名字（本地路径与远程路径通用：都是字符串拼接）。
pub fn part_name(final_name: &str) -> String {
    format!("{final_name}{PART_SUFFIX}")
}

/// 半截文件的本地路径。
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(PART_SUFFIX);
    PathBuf::from(s)
}

/// 源的"身份"：大小 + mtime。两者都对上才认为是同一个源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub src_size: u64,
    pub src_mtime: u32,
}

impl Mark {
    pub fn new(src_size: u64, src_mtime: u32) -> Mark {
        Mark {
            src_size,
            src_mtime,
        }
    }

    fn serialize(&self) -> String {
        format!("v1\t{}\t{}", self.src_size, self.src_mtime)
    }

    fn parse(s: &str) -> Option<Mark> {
        let mut it = s.trim().split('\t');
        if it.next()? != "v1" {
            return None;
        }
        Some(Mark {
            src_size: it.next()?.parse().ok()?,
            src_mtime: it.next()?.parse().ok()?,
        })
    }
}

/// mark 的落盘位置：缓存目录 + 半截文件标识的哈希。
///
/// 放缓存目录而不是半截文件旁边：远程半截文件旁边没法放本地文件，
/// 而用户目录里多出一堆 `.state` 比多出 `.pp-part` 更让人抓狂。
fn mark_file(key: &str) -> PathBuf {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    glib::user_cache_dir()
        .join("puttyplus")
        .join("resume")
        .join(format!("{h:08x}.state"))
}

pub fn save_mark(key: &str, m: &Mark) {
    let p = mark_file(key);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, m.serialize());
}

pub fn load_mark(key: &str) -> Option<Mark> {
    Mark::parse(&std::fs::read_to_string(mark_file(key)).ok()?)
}

pub fn clear_mark(key: &str) {
    let _ = std::fs::remove_file(mark_file(key));
}

/// 能不能续传，从哪儿续。
///
/// - mark 得在，且与当前源**完全一致**（大小、mtime 都对）
/// - 半截文件得非空（0 就是没传过，走"从头开始"更省事）
/// - 半截文件不能比源还大（源被改小了 / 半截文件是别的残留）
///
/// 不满足就返回 None——调用方负责把半截文件删掉重来。
pub fn resume_offset(mark: Option<Mark>, part_size: u64, src: &Mark) -> Option<u64> {
    if mark? != *src || part_size == 0 || part_size > src.src_size {
        return None;
    }
    Some(part_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 分片名与路径() {
        assert_eq!(part_name("/tmp/a.bin"), "/tmp/a.bin.pp-part");
        assert_eq!(part_name("a.bin"), "a.bin.pp-part");
        assert_eq!(
            part_path(Path::new("/tmp/a.bin")),
            PathBuf::from("/tmp/a.bin.pp-part")
        );
    }

    #[test]
    fn mark_序列化往返() {
        let m = Mark::new(12345, 1700000000);
        assert_eq!(Mark::parse(&m.serialize()), Some(m));
        // 0 值也是合法值（空文件）
        let z = Mark::new(0, 0);
        assert_eq!(Mark::parse(&z.serialize()), Some(z));
    }

    #[test]
    fn mark_坏内容不认() {
        assert_eq!(Mark::parse(""), None);
        assert_eq!(Mark::parse("v2\t1\t2"), None);
        assert_eq!(Mark::parse("v1\tx\t2"), None);
        assert_eq!(Mark::parse("v1\t1"), None);
    }

    #[test]
    fn 同一个源才续() {
        let src = Mark::new(1000, 99);
        assert_eq!(resume_offset(Some(src), 400, &src), Some(400));
        // 刚好传完（半截文件 == 源大小）：没有字节要补，直接进校验
        assert_eq!(resume_offset(Some(src), 1000, &src), Some(1000));
    }

    #[test]
    fn 源变了就重来() {
        let src = Mark::new(1000, 99);
        // 源被改大
        assert_eq!(resume_offset(Some(src), 400, &Mark::new(2000, 99)), None);
        // 源被改小（改小通常意味着 mtime 也变了，这里分别测）
        assert_eq!(resume_offset(Some(src), 400, &Mark::new(500, 99)), None);
        // 同大小但 mtime 变了（内容被改过）
        assert_eq!(resume_offset(Some(src), 400, &Mark::new(1000, 100)), None);
        // 没有 mark
        assert_eq!(resume_offset(None, 400, &src), None);
    }

    #[test]
    fn 半截文件异常就重来() {
        let src = Mark::new(1000, 99);
        // 空半截文件：等价于没传
        assert_eq!(resume_offset(Some(src), 0, &src), None);
        // 比源还大：残留自别处
        assert_eq!(resume_offset(Some(src), 1001, &src), None);
    }
}
