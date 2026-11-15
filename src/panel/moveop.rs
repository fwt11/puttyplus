//! 拖动移动（阶段 106）：把文件 / 文件夹**移进另一个文件夹**。
//!
//! ## 两条路（语义不同，别混）
//!
//! - **本地**：本地路径（Windows 上是 `C:\dir\x`）⇒ `std::fs::rename`；
//!   **跨盘**时 rename 会报 `EXDEV`（Windows 是 `ERROR_NOT_SAME_DEVICE`）
//!   ⇒ 回退成「复制 + 删除」。
//! - **远端**：**恒为 POSIX** 的远端路径 ⇒ 走 worker 的 SFTP `rename`
//!   （见 `worker.rs` 的 `WorkerCmd::Move`）。
//!
//! ## ⚠️ 同名不覆盖（用户 2026-11-14 拍板）
//!
//! 目标目录里**已经有同名** ⇒ **报错停下**，一个字节都不动。
//! ⚠️ 为什么不交给 `rename` 自己决定：Unix 的 `rename(2)` 会**默默覆盖**
//! 目标文件 ⇒ 用户拖一下就丢了另一个文件 ⇒ 必须**先查再动**。
//!
//! ## ⚠️ 「移到它自己所在的目录」= 什么都不做
//!
//! 不是错误（用户可能把文件拖到它自己那行的目录上）⇒ 当作成功但不改名。

use crate::i18n::tr;
use crate::platform::localpath;
use crate::trf;
use crate::util::path_join;

/// 本地移动的**目标路径**（纯函数 ⇒ 好测）。
///
/// ⚠️ 本地路径是**平台原生**的（Windows 上 `C:\dir\x`）⇒ 用
/// `platform::localpath::join`，**不是** `util::path_join`（那个是远端
/// POSIX 专用，阶段 C5 的红线）。
pub fn local_target(src: &str, dst_dir: &str) -> String {
    let name = localpath::basename(src);
    localpath::join(dst_dir, &name)
}

/// 远端移动的**目标路径**（纯函数 ⇒ 好测）。
///
/// ⚠️ 远端路径**恒为 POSIX** ⇒ 用 `util::path_join`。
pub fn remote_target(src: &str, dst_dir: &str) -> String {
    let name = crate::util::path_basename(src);
    path_join(dst_dir, &name)
}

/// 目标路径是不是**已经有人占了**（同名检查）。
pub fn taken(dst: &str) -> bool {
    std::fs::symlink_metadata(dst).is_ok()
}

/// 本地移动：把 `src`（文件或文件夹）移进 `dst_dir`。
///
/// 返回**新的绝对路径**。
///
/// - 同名 ⇒ `Err`（**不动**）；
/// - 跨盘 ⇒ 复制 + 删除（`rename` 在跨盘时会报 `EXDEV`）；
/// - 移进它自己所在的目录 ⇒ 原样返回（什么都不做）。
pub fn move_local(src: &str, dst_dir: &str) -> Result<String, String> {
    let name = localpath::basename(src);
    if name.is_empty() {
        return Err(tr("移动失败：不知道这个文件的名字。").to_string());
    }
    let dst = local_target(src, dst_dir);
    // 移到**自己所在的目录** ⇒ 什么都不做（不是错误）。
    if same_path(src, &dst) {
        return Ok(dst);
    }
    // ⚠️ **先查同名**（用户定的：不覆盖）。
    if taken(&dst) {
        return Err(trf!(
            "目标目录里已经有同名的「{name}」了，没有动它。",
            name = name
        ));
    }
    match std::fs::rename(src, &dst) {
        Ok(()) => Ok(dst),
        Err(e) if is_cross_device(&e) => {
            // 跨盘 ⇒ 复制 + 删除。
            copy_recursive(std::path::Path::new(src), std::path::Path::new(&dst))
                .map_err(|err| trf!("移动失败：{err}", err = err))?;
            remove_recursive(std::path::Path::new(src))
                .map_err(|err| trf!("移动失败（文件已复制，但删不掉源文件）：{err}", err = err))?;
            Ok(dst)
        }
        Err(e) => Err(trf!("移动失败：{e}", e = e)),
    }
}

/// 两个路径是不是**同一个**（比字符串更稳：先比规范化结果）。
fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (
        std::fs::canonicalize(a),
        std::fs::canonicalize(b),
    ) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 是不是「跨盘」错误（`rename` 不能跨文件系统）。
///
/// - Linux/macOS：`EXDEV` = 18；
/// - Windows：`ERROR_NOT_SAME_DEVICE` = 17。
fn is_cross_device(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(18) => true, // EXDEV（Linux / macOS）
        #[cfg(windows)]
        Some(17) => true, // ERROR_NOT_SAME_DEVICE
        _ => false,
    }
}

/// 递归复制（跨盘回退用）。
///
/// ⚠️ **符号链接按「它指向的内容」复制**（`fs::copy` 会跟随链接）——
/// 跨盘移动符号链接是个偏门场景，这里选了简单做法；同盘移动（`rename`）
/// 不受影响（链接原样搬过去）。
fn copy_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if md.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let s = entry.path();
            let d = dst.join(&name);
            copy_recursive(&s, &d)?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// 递归删除（跨盘回退的「删源文件」那一步）。
fn remove_recursive(p: &std::path::Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if md.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(p).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("pp-moveop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn 本地目标路径按平台拼() {
        let src = if cfg!(windows) { r"C:\a\b.txt" } else { "/a/b.txt" };
        let dir = if cfg!(windows) { r"C:\d" } else { "/d" };
        assert_eq!(local_target(src, dir), localpath::join(dir, "b.txt"));
    }

    #[test]
    fn 远端目标路径恒为_posix() {
        assert_eq!(remote_target("/a/b.txt", "/d"), "/d/b.txt");
        assert_eq!(remote_target("/a/b.txt", "/d/"), "/d/b.txt");
    }

    #[test]
    fn 同名不覆盖_报错且不动() {
        let d = tmpdir("dup");
        let src = d.join("x.txt");
        let dst_dir = d.join("sub");
        std::fs::write(&src, b"new").unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        std::fs::write(dst_dir.join("x.txt"), b"old").unwrap();
        let err = move_local(
            &src.to_string_lossy(),
            &dst_dir.to_string_lossy(),
        )
        .unwrap_err();
        assert!(err.contains("同名的"), "错误信息要说清：{err}");
        // ⚠️ 目标**没被动**，源也还在。
        assert_eq!(std::fs::read(dst_dir.join("x.txt")).unwrap(), b"old");
        assert!(src.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 移动文件进目录() {
        let d = tmpdir("file");
        let src = d.join("x.txt");
        let dst_dir = d.join("sub");
        std::fs::write(&src, b"hi").unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        let got = move_local(&src.to_string_lossy(), &dst_dir.to_string_lossy()).unwrap();
        assert_eq!(got, localpath::join(&dst_dir.to_string_lossy(), "x.txt"));
        assert!(!src.exists());
        assert_eq!(std::fs::read(dst_dir.join("x.txt")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 移动文件夹进目录() {
        let d = tmpdir("dir");
        let src = d.join("sub");
        let dst_dir = d.join("dst");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("inner").join("a.txt"), b"a").unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        move_local(&src.to_string_lossy(), &dst_dir.to_string_lossy()).unwrap();
        assert!(!src.exists());
        assert_eq!(
            std::fs::read(dst_dir.join("sub").join("inner").join("a.txt")).unwrap(),
            b"a"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 移进自己所在的目录_什么都不做() {
        let d = tmpdir("self");
        let src = d.join("x.txt");
        std::fs::write(&src, b"hi").unwrap();
        let got = move_local(&src.to_string_lossy(), &d.to_string_lossy()).unwrap();
        assert!(src.exists(), "源文件不该被动");
        assert_eq!(got, localpath::join(&d.to_string_lossy(), "x.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
