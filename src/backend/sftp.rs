//! SFTP 文件操作与递归传输（对应 C 版 sftp_backend 的 list/mkdir/rename/
//! remove_rec/upload_rec/download_rec）。

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::conn::SftpBackend;
use super::resume::{self, Mark};
use super::{checksum, BackendError, BackendResult, SftpEntry};
use crate::panel::worker::XferConflict;
use crate::util::{human_size, path_basename, path_join, path_parent};

const XFER_CHUNK: usize = 64 * 1024;

/// 递归传输/删除的最大深度（修复 C 版无界递归）。
const MAX_DEPTH: usize = 32;

type BoxFut<'a> = Pin<Box<dyn Future<Output = BackendResult<bool>> + Send + 'a>>;
type BoxFutU<'a> = Pin<Box<dyn Future<Output = BackendResult<()>> + Send + 'a>>;

/// 进度回调（done, total, 文件名）→ 返回 true 表示取消。
/// 放进共享句柄：递归传输只传共享引用，UI 侧（Phase 4）跨线程注入闭包。
#[derive(Default, Clone)]
pub struct ProgressSink {
    cb: Arc<Mutex<Option<Box<dyn FnMut(u64, u64, &str) -> bool + Send>>>>,
}

impl ProgressSink {
    pub fn new<F>(cb: F) -> Self
    where
        F: FnMut(u64, u64, &str) -> bool + Send + 'static,
    {
        ProgressSink {
            cb: Arc::new(Mutex::new(Some(Box::new(cb)))),
        }
    }

    pub fn none() -> Self {
        Self::default()
    }

    /// 上报进度；返回 true 表示调用方应取消。
    pub fn report(&self, done: u64, total: u64, name: &str) -> bool {
        let mut g = self.cb.lock().unwrap();
        match g.as_mut() {
            Some(f) => f(done, total, name),
            None => false,
        }
    }
}

/// 传输附注（如"已校验 SHA-1"）：递归只传共享引用，一路往里写最后一条，
/// 顶层读完塞进队列项的消息里。
#[derive(Default, Clone)]
pub struct XferNote(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl XferNote {
    pub fn set(&self, s: impl Into<String>) {
        *self.0.lock().unwrap() = Some(s.into());
    }

    pub fn get(&self) -> Option<String> {
        self.0.lock().unwrap().clone()
    }

    /// 追加一条附注（前面已有内容时用分号接起来）。
    pub fn append(&self, s: impl Into<String>) {
        let s = s.into();
        let mut g = self.0.lock().unwrap();
        match g.as_mut() {
            Some(v) => {
                v.push('；');
                v.push_str(&s);
            }
            None => *g = Some(s),
        }
    }
}

impl SftpBackend {
    /// 远程主目录（canonicalize "."）。
    pub async fn cwd(&self) -> BackendResult<String> {
        self.sftp()
            .canonicalize(".")
            .await
            .map_err(|e| BackendError(format!("无法获取远程主目录: {e}")))
    }

    /// 目录列表；空目录返回空 Vec（对应 C 版“空目录 *ok=TRUE”）。
    pub async fn list(&self, path: &str) -> BackendResult<Vec<SftpEntry>> {
        let rd = self
            .sftp()
            .read_dir(path)
            .await
            .map_err(|e| BackendError(format!("打开目录 {path} 失败: {e}")))?;
        let mut out = Vec::new();
        for entry in rd {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let md = entry.metadata();
            let perms = md.permissions.unwrap_or(0);
            let is_dir = (perms & 0o170000) == 0o040000;
            let is_link = (perms & 0o170000) == 0o120000;
            out.push(SftpEntry {
                longname: String::new(),
                size: if !is_dir { md.size.unwrap_or(0) } else { 0 },
                perm: super::perm_string(perms, is_dir, is_link),
                is_dir,
                is_link,
                mtime: md.mtime.map(|t| t as i64).unwrap_or(0),
                name,
            });
        }
        Ok(out)
    }

    pub async fn mkdir(&self, path: &str) -> BackendResult<()> {
        self.sftp()
            .create_dir(path)
            .await
            .map_err(|e| BackendError(format!("创建目录失败: {e}")))
    }

    pub async fn rename(&self, oldpath: &str, newpath: &str) -> BackendResult<()> {
        self.sftp()
            .rename(oldpath, newpath)
            .await
            .map_err(|e| BackendError(format!("重命名失败: {e}")))
    }

    /// stat 判断目录；获取失败按“非目录”处理（与 C 版 is_dir_locked 一致）。
    pub async fn is_dir(&self, path: &str) -> bool {
        matches!(
            self.sftp().metadata(path).await,
            Ok(md) if (md.permissions.unwrap_or(0) & 0o170000) == 0o040000
        )
    }

    /// 读取文本查看/编辑器要打开的文件，最多 `max` 字节。
    ///
    /// 先 stat 挡掉目录和明显超限的文件；再用 `take(max + 1)` 兜住
    /// "stat 之后文件又长大了"的竞态——多读一个字节就足以判断超限，
    /// 不会把几个 GB 的文件读进内存。
    pub async fn read_file_limited(&self, path: &str, max: u64) -> BackendResult<Vec<u8>> {
        let md = self
            .sftp()
            .metadata(path)
            .await
            .map_err(|e| BackendError(format!("获取 {path} 属性失败: {e}")))?;
        if md.is_dir() {
            return Err(BackendError(format!(
                "{path} 是目录，无法用文本查看/编辑器打开"
            )));
        }
        // 有些服务器 stat 不回 permissions；那种情况下无法判断类型，
        // 交给后面的 open 自己报错，别把普通文件误判成特殊文件。
        if md.permissions.is_some() && !md.is_regular() {
            return Err(BackendError(format!(
                "{path} 不是普通文件，内置查看/编辑器只处理文本文件"
            )));
        }
        if let Some(size) = md.size {
            if size > max {
                return Err(BackendError(format!(
                    "文件 {} 大小 {}，超过内置查看/编辑上限 {}",
                    path,
                    human_size(size),
                    human_size(max)
                )));
            }
        }
        let f = self
            .sftp()
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|e| BackendError(format!("打开远程文件 {path} 失败: {e}")))?;
        let mut limited = f.take(max.saturating_add(1));
        let mut data = Vec::new();
        limited
            .read_to_end(&mut data)
            .await
            .map_err(|e| BackendError(format!("读取远程文件 {path} 失败: {e}")))?;
        if data.len() as u64 > max {
            return Err(BackendError(format!(
                "文件 {} 超过内置查看/编辑上限 {}",
                path,
                human_size(max)
            )));
        }
        Ok(data)
    }

    /// 把编辑器内容写回远端：先写同目录临时文件，再原子改名覆盖正式文件。
    ///
    /// 直接 `TRUNCATE` 打开正式文件再写，网络一断就会留下半个文件；
    /// 这里宁可多一次 rename。权限位从原文件抄到临时文件（属性不保留的
    /// 服务器上只是尽力而为，不因此判保存失败）。符号链接会被改名替换成
    /// 普通文件——这是原子写的代价，与"写坏原文件"相比选前者。
    pub async fn write_file_atomic(&self, path: &str, data: &[u8]) -> BackendResult<()> {
        let old = self.sftp().metadata(path).await.ok();
        if let Some(m) = &old {
            if m.is_dir() {
                return Err(BackendError(format!(
                    "{path} 是目录，无法写入文本内容"
                )));
            }
            if m.permissions.is_some() && !m.is_regular() {
                return Err(BackendError(format!(
                    "{path} 不是普通文件，无法写入文本内容"
                )));
            }
        }
        let perm = old
            .as_ref()
            .and_then(|m| m.permissions)
            .map(|p| p & 0o7777);
        let tmp = edit_temp_path(path);
        let mut attrs = FileAttributes::empty();
        attrs.permissions = perm;

        let mut f = self
            .sftp()
            .open_with_flags_and_attributes(
                &tmp,
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                attrs,
            )
            .await
            .map_err(|e| BackendError(format!("创建临时文件 {tmp} 失败: {e}")))?;
        if let Err(e) = f.write_all(data).await {
            let _ = f.shutdown().await;
            let _ = self.sftp().remove_file(&tmp).await;
            return Err(BackendError(format!("写入远程文件 {path} 失败: {e}")));
        }
        if let Err(e) = f.shutdown().await {
            let _ = self.sftp().remove_file(&tmp).await;
            return Err(BackendError(format!("写入远程文件 {path} 失败: {e}")));
        }
        if let Some(p) = perm {
            let mut md = FileAttributes::empty();
            md.permissions = Some(p);
            if let Err(e) = self.sftp().set_metadata(&tmp, md).await {
                eprintln!("PuTTYPlus: 保留 {path} 的权限位失败（不影响内容）: {e}");
            }
        }
        if let Err(e) = self.rename_over(&tmp, path).await {
            let _ = self.sftp().remove_file(&tmp).await;
            return Err(e);
        }
        Ok(())
    }

    /// 递归删除远端文件/目录树。
    pub async fn remove_rec(&self, path: &str) -> BackendResult<()> {
        self.remove_rec_inner(path, 0).await
    }

    fn remove_rec_inner<'a>(&'a self, path: &'a str, depth: usize) -> BoxFutU<'a> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                return Err(BackendError(format!(
                    "目录层级过深（超过 {MAX_DEPTH} 层），已中止: {path}"
                )));
            }
            let md = self
                .sftp()
                .metadata(path)
                .await
                .map_err(|e| BackendError(format!("获取 {path} 属性失败: {e}")))?;
            let is_dir = (md.permissions.unwrap_or(0) & 0o170000) == 0o040000;

            if !is_dir {
                self.sftp()
                    .remove_file(path)
                    .await
                    .map_err(|e| BackendError(format!("删除 {path} 失败: {e}")))?;
                return Ok(());
            }
            let rd = self
                .sftp()
                .read_dir(path)
                .await
                .map_err(|e| BackendError(format!("打开目录 {path} 失败: {e}")))?;
            for entry in rd {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                let sub = path_join(path, &name);
                self.remove_rec_inner(&sub, depth + 1).await?;
            }
            self.sftp()
                .remove_dir(path)
                .await
                .map_err(|e| BackendError(format!("删除目录 {path} 失败: {e}")))?;
            Ok(())
        })
    }

    /// 递归上传本地 src 到远程 dst_dir/<basename(src)>。
    /// 递归上传。Ok(false) = 单文件因"跳过已存在"策略未传输。
    pub async fn upload_rec(
        &self,
        src: &str,
        dst_dir: &str,
        progress: &ProgressSink,
        conflict: XferConflict,
        note: &XferNote,
    ) -> BackendResult<bool> {
        let dst = path_join(dst_dir, path_basename(src));
        self.upload_rec_inner(src, &dst, progress, conflict, note, 0).await
    }

    fn upload_rec_inner<'a>(
        &'a self,
        lsrc: &'a str,
        rdst: &'a str,
        progress: &'a ProgressSink,
        conflict: XferConflict,
        note: &'a XferNote,
        depth: usize,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                return Err(BackendError(format!(
                    "目录层级过深（超过 {MAX_DEPTH} 层），已中止: {lsrc}"
                )));
            }
            let meta = std::fs::metadata(lsrc)
                .map_err(|e| BackendError(format!("获取本地 {lsrc} 失败: {e}")))?;
            if meta.is_dir() {
                if let Err(e) = self.sftp().create_dir(rdst).await {
                    if !self.is_dir(rdst).await {
                        return Err(BackendError(format!("创建远程目录 {rdst} 失败: {e}")));
                    }
                }
                let mut rd = std::fs::read_dir(lsrc)
                    .map_err(|e| BackendError(format!("打开本地目录 {lsrc} 失败: {e}")))?;
                while let Some(Ok(ent)) = rd.next() {
                    let name = ent.file_name().to_string_lossy().into_owned();
                    let lsub = path_join(lsrc, &name);
                    let rsub = path_join(rdst, &name);
                    self.upload_rec_inner(&lsub, &rsub, progress, conflict, note, depth + 1)
                        .await?;
                }
                return Ok(true); // 目录本身不算"被跳过"
            }
            // 冲突预检：跳过已存在的目标文件
            if conflict == XferConflict::Skip
                && self.sftp().try_exists(rdst).await.unwrap_or(false)
            {
                return Ok(false);
            }
            self.upload_file(lsrc, rdst, progress, note).await?;
            Ok(true)
        })
    }

    /// 递归下载远程 src 到本地 dst_dir/<basename(src)>。
    /// 递归下载。Ok(false) = 单文件因"跳过已存在"策略未传输。
    pub async fn download_rec(
        &self,
        src: &str,
        dst_dir: &str,
        progress: &ProgressSink,
        conflict: XferConflict,
        note: &XferNote,
    ) -> BackendResult<bool> {
        let dst = path_join(dst_dir, path_basename(src)).replace('/', std::path::MAIN_SEPARATOR_STR);
        self.download_rec_inner(src, Path::new(&dst), progress, conflict, note, 0)
            .await
    }

    fn download_rec_inner<'a>(
        &'a self,
        rsrc: &'a str,
        ldst: &'a Path,
        progress: &'a ProgressSink,
        conflict: XferConflict,
        note: &'a XferNote,
        depth: usize,
    ) -> BoxFut<'a> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                return Err(BackendError(format!(
                    "目录层级过深（超过 {MAX_DEPTH} 层），已中止: {rsrc}"
                )));
            }
            if self.is_dir(rsrc).await {
                tokio::fs::create_dir_all(ldst).await.map_err(|e| {
                    BackendError(format!("创建本地目录 {} 失败: {e}", ldst.display()))
                })?;
                let items = self.list(rsrc).await?;
                for e in items {
                    let rsub = path_join(rsrc, &e.name);
                    let lsub = ldst.join(&e.name);
                    self.download_rec_inner(&rsub, &lsub, progress, conflict, note, depth + 1)
                        .await?;
                }
                return Ok(true); // 目录本身不算"被跳过"
            }
            // 冲突预检：跳过已存在的本地目标文件
            if conflict == XferConflict::Skip && ldst.is_file() {
                return Ok(false);
            }
            self.download_file(rsrc, ldst, progress, note).await?;
            Ok(true)
        })
    }

    pub(crate) async fn upload_file(
        &self,
        lpath: &str,
        rpath: &str,
        progress: &ProgressSink,
        note: &XferNote,
    ) -> BackendResult<bool> {
        let meta = std::fs::metadata(lpath)
            .map_err(|e| BackendError(format!("获取本地 {lpath} 失败: {e}")))?;
        let total = meta.len();
        let src = Mark::new(total, meta_mtime(&meta));
        let part = resume::part_name(rpath);
        // mark 键带上连接标识：两台同名主机的半截文件不能互相续
        let key = format!("up|{}|{part}", self.id);

        let mut f = tokio::fs::File::open(lpath)
            .await
            .map_err(|e| BackendError(format!("打开本地文件 {lpath} 失败: {e}")))?;

        let (offset, flags) = match self.plan_resume(&key, &part, false, src).await {
            Some(off) => (off, OpenFlags::CREATE | OpenFlags::WRITE),
            None => (0, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE),
        };
        let mut rf = self
            .sftp()
            .open_with_flags(&part, flags)
            .await
            .map_err(|e| BackendError(format!("创建远程文件 {part} 失败: {e}")))?;
        if offset > 0 {
            rf.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| BackendError(format!("定位远程分片 {part} 失败: {e}")))?;
        }
        if offset > 0 {
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| BackendError(format!("定位本地文件 {lpath} 失败: {e}")))?;
        }

        // mark 在开写时就落盘：进程被杀/断电走的不是错误分支，
        // 那时要是还没写 mark，下次就续不上了
        resume::save_mark(&key, &src);

        let mut buf = vec![0u8; XFER_CHUNK];
        let mut done: u64 = offset;
        let mut cancelled = false;
        let result: BackendResult<()> = loop {
            let n = match f.read(&mut buf).await {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => break Err(BackendError(format!("读取 {lpath} 失败: {e}"))),
            };
            if let Err(e) = rf.write_all(&buf[..n]).await {
                break Err(BackendError(format!("写入 {part} 失败: {e}")));
            }
            done += n as u64;
            if progress.report(done, total, path_basename(rpath)) {
                cancelled = true;
                break Err(BackendError::new("已取消"));
            }
        };
        let _ = rf.shutdown().await;

        if result.is_err() {
            if cancelled {
                // 取消 = 用户自己中断的，分片留着好续传（mark 也留着）
                progress.report(done, total, path_basename(rpath));
            } else {
                self.discard_part(&key, &part, false).await;
            }
            return result.map(|()| true);
        }

        // 校验过了才改名：正式名一旦出现就该是完整的
        self.verify_checksum(&key, &part, lpath, note).await?;
        self.rename_over(&part, rpath).await?;
        resume::clear_mark(&key);
        Ok(true)
    }

    /// 改名，**允许覆盖已存在的目标**。
    ///
    /// 为什么需要它：OpenSSH 的 sftp-server 实现 `SSH_FXP_RENAME` 时，
    /// 只要目标存在就直接返回 `Failure`（SFTP 规格就是"已存在不许静默
    /// 覆盖"）。而 russh-sftp 没有暴露 `posix-rename@openssh.com` 扩展，
    /// 所以"覆盖上传一个已存在的文件"会**必然失败在最后一步改名**——
    /// 而那恰恰是最常见的操作（重传一个改过的文件、镜像一个更新过的文件）。
    ///
    /// 做法：先按正常改名试，只有失败**且目标确实存在**时才删掉目标再改
    /// 一次。这样在"rename 本来就能覆盖"的服务器上仍然是原子的，只有在
    /// 拒绝覆盖的服务器上才退化为 remove + rename。
    ///
    /// 代价要说清楚：那一步存在一个"旧文件已删、新文件还没就位"的小窗口。
    /// 远端要是有守护进程盯着这个目录，理论上能看见中间态。
    async fn rename_over(&self, old: &str, new: &str) -> BackendResult<()> {
        match self.sftp().rename(old, new).await {
            Ok(()) => Ok(()),
            Err(e) => {
                if !self.sftp().try_exists(new).await.unwrap_or(false) {
                    return Err(BackendError(format!("把 {old} 改名为 {new} 失败: {e}")));
                }
                self.sftp()
                    .remove_file(new)
                    .await
                    .map_err(|x| BackendError(format!("覆盖前删除旧文件 {new} 失败: {x}")))?;
                self.sftp()
                    .rename(old, new)
                    .await
                    .map_err(|x| BackendError(format!("把 {old} 改名为 {new} 失败: {x}")))
            }
        }
    }

    async fn download_file(
        &self,
        rpath: &str,
        lpath: &Path,
        progress: &ProgressSink,
        note: &XferNote,
    ) -> BackendResult<bool> {
        let md = self
            .sftp()
            .metadata(rpath)
            .await
            .map_err(|e| BackendError(format!("获取 {rpath} 属性失败: {e}")))?;
        let total = md.size.unwrap_or(0);
        let src = Mark::new(total, md.mtime.unwrap_or(0));
        let lpart = resume::part_path(lpath);
        let rpart = rpath.to_string();
        let key = format!("down|{}|{rpart}", self.id);

        let lpart_str = lpart.to_string_lossy().into_owned();
        let offset = self.plan_resume(&key, &lpart_str, true, src).await.unwrap_or(0);

        let mut rf = self
            .sftp()
            .open_with_flags(rpath, OpenFlags::READ)
            .await
            .map_err(|e| BackendError(format!("打开远程文件 {rpath} 失败: {e}")))?;
        if offset > 0 {
            rf.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| BackendError(format!("定位远程文件 {rpath} 失败: {e}")))?;
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .open(&lpart)
            .await
            .map_err(|e| BackendError(format!("创建本地分片 {} 失败: {e}", lpart.display())))?;
        if offset > 0 {
            // 关键：不定位就等于从 0 开始写，把已下好的前半截覆盖掉。
            // OpenOptions 不带 append 时文件位置在开头。
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| {
                    BackendError(format!("定位本地分片 {} 失败: {e}", lpart.display()))
                })?;
        }

        // 同上：mark 必须在开写时就落盘
        resume::save_mark(&key, &src);

        let mut buf = vec![0u8; XFER_CHUNK];
        let mut done: u64 = offset;
        let result: BackendResult<()> = loop {
            let n = match rf.read(&mut buf).await {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) => break Err(BackendError(format!("读取 {rpath} 失败: {e}"))),
            };
            if let Err(e) = f.write_all(&buf[..n]).await {
                break Err(BackendError(format!(
                    "写入本地 {} 失败: {e}",
                    lpart.display()
                )));
            }
            done += n as u64;
            if progress.report(done, total, path_basename(rpath)) {
                break Err(BackendError::new("已取消"));
            }
        };
        let _ = f.flush().await;

        if result.is_err() {
            // 取消与出错都留分片——断网重连后续传比重来快得多
            if done == 0 {
                // 一点没传进去，留个空壳只会让人以为"传过"
                self.discard_part(&key, &lpart_str, true).await;
            }
            return result.map(|()| true);
        }

        self.verify_checksum(&key, rpath, &lpart_str, note).await?;
        resume::clear_mark(&key);
        tokio::fs::rename(&lpart, lpath)
            .await
            .map_err(|e| BackendError(format!("把 {} 改名为目标失败: {e}", lpart.display())))?;
        Ok(true)
    }
}

/* ---------------- 断点续传 / 校验和（3.5） ---------------- */

impl SftpBackend {
/// 决定这次从哪儿开始续；Some(off) = 续，None = 从头。
    ///
    /// `part` 是分片路径（本地 `Path` 或远程字符串，看 `local`）。
    /// 续传不成立时把陈旧分片删掉——留着它只会让用户目录里堆垃圾，
    /// 而下一次续传因为 mark 对不上照样不会用它。
    async fn plan_resume(
        &self,
        key: &str,
        part: &str,
        local: bool,
        src: Mark,
    ) -> Option<u64> {
        if !crate::settings::xfer_resume() {
            return None;
        }
        let size = if local {
            tokio::fs::metadata(part).await.ok().map(|m| m.len())
        } else {
            self.sftp().metadata(part).await.ok().and_then(|m| m.size)
        };
        let mark = resume::load_mark(key);
        let off = size.and_then(|n| resume::resume_offset(mark, n, &src));
        if off.is_none() {
            self.discard_part(key, part, local).await;
        }
        off
    }

    /// 删掉分片与它的 mark。只动持有分片的那一侧——
    /// 拿同一个字符串去另一侧删同名文件，撞上了就是误删。
    async fn discard_part(&self, key: &str, part: &str, local: bool) {
        if local {
            let _ = tokio::fs::remove_file(part).await;
        } else {
            let _ = self.sftp().remove_file(part).await;
        }
        resume::clear_mark(key);
    }

    /// 传输完成后的 SHA-1 校验。
    ///
    /// `local_part` 是本地那一侧（本地源或本地分片），`remote_part` 是
    /// 远程那一侧。对不上就是**传输坏了**：报失败并删分片，绝不把坏文件
    /// 改名成正式名。服务器没有可用工具时只留一句附注，不算失败。
    async fn verify_checksum(
        &self,
        key: &str,
        remote_part: &str,
        local_part: &str,
        note: &XferNote,
    ) -> BackendResult<()> {
        if !crate::settings::xfer_checksum() {
            return Ok(());
        }
        let local = checksum::sha1_file(Path::new(local_part))
            .map_err(|e| BackendError(format!("计算 {local_part} 的校验和失败: {e}")))?;
        match checksum::remote_sha1(self, remote_part).await {
            Ok(remote) if remote.eq_ignore_ascii_case(&local) => {
                note.set("已校验 SHA-1");
                Ok(())
            }
            Ok(remote) => {
                self.discard_part(key, remote_part, false).await;
                Err(BackendError(format!(
                    "校验和不一致（本地 {local} / 远程 {remote}），已删除分片，请重传"
                )))
            }
            Err(e) => {
                note.set(format!("未校验：{}", e.0));
                Ok(())
            }
        }
    }
}

/// 编辑器保存用的远端临时文件：与目标同目录（保证 rename 是同一文件系统内
/// 的原子操作），文件名带进程号 + 纳秒，避免两个窗口同时保存时撞名。
fn edit_temp_path(path: &str) -> String {
    let dir = path_parent(path);
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path_join(
        &dir,
        &format!(".puttyplus-edit-{}-{n}.tmp", std::process::id()),
    )
}

/// 本地 mtime → Unix 秒；平台拿不到就 0（mark 只用于同一次传输的连续性判断）。
fn meta_mtime(m: &std::fs::Metadata) -> u32 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}
