//! 自身可执行文件路径（跨平台版 阶段 C0）。
//!
//! ## 为什么不能直接写死 `/proc/self/exe`
//!
//! `/proc` 是 Linux 独有的。Windows 上根本没有这个目录。
//! 跨平台版统一走 [`self_exe_path`]：
//!
//! - Linux：仍用 `/proc/self/exe`（**保持原样**，不改变任何既有行为）；
//! - 其他平台：`std::env::current_exe()`。
//!
//! ## askpass 判定是「红线」
//!
//! ssh 会用 `SSH_ASKPASS=<puttyplus 自身路径>` 调用本程序来问密码。
//! 程序要认出「**我就是那个被调用的程序**」⇒ 两条路径必须指向**同一个文件**。
//!
//! ⚠️ `/proc/self/exe` 读出来的是**已经解析过符号链接**的绝对路径，
//! 而 `SSH_ASKPASS` 里那个可能是**没解析的**（例如经过软链接调用）。
//! ⇒ 比较前两边都要 [`std::fs::canonicalize`]。

use std::path::{Path, PathBuf};

/// 自身可执行文件路径（尽量是**绝对且已解析符号链接**的）。
///
/// 拿不到时返回 `Err`（**保留原始 io 错误**）——调用方据此放弃
/// 「隐式 askpass」判定，但**显式 `--askpass` 仍然有效**
/// （那条不依赖自身路径）。
pub fn self_exe_path() -> Result<PathBuf, std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link("/proc/self/exe")
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::current_exe()
    }
}

/// 两个路径是不是**同一个文件**。
///
/// 两边都先 `canonicalize`（解析符号链接、去掉 `..`）⇒
/// `/usr/bin/puttyplus` 与 `/usr/local/bin/puttyplus`（软链）能认出是同一个。
///
/// 任一边解析失败 ⇒ 退化成**字面比较**（`None` 表示「认不出」，
/// 不是「一定不是」）。
pub fn same_file(a: &Path, b: &Path) -> bool {
    let ca = std::fs::canonicalize(a).ok();
    let cb = std::fs::canonicalize(b).ok();
    match (ca, cb) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 自身路径拿得到且是绝对路径() {
        let p = self_exe_path().expect("拿不到自身路径");
        assert!(p.is_absolute(), "自身路径应当是绝对路径：{}", p.display());
    }
    #[test]
    fn 同一个文件的两条路径认得出是同一个() {
        let me = self_exe_path().expect("拿不到自身路径");
        // 自身 ↔ 自身：必然同一个
        assert!(same_file(&me, &me));
        // 解析后再比一次（canonicalize 后的绝对路径）
        if let Ok(c) = std::fs::canonicalize(&me) {
            assert!(same_file(&c, &me));
        }
    }

    #[test]
    fn 不存在的路径不会误判成同一个() {
        let a = Path::new("/__puttyplus_不存在的路径_a__");
        let b = Path::new("/__puttyplus_不存在的路径_b__");
        assert!(!same_file(a, b));
    }
}
