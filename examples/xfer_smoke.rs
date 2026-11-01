//! 断点续传 + 完成后校验和（3.5）真机冒烟。
//! 运行：cargo run --example xfer_smoke
//! 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。
//!
//! 覆盖四条链路，每条都对着真 sshd 跑：
//!   1. 上传传到一半取消 → 正式名不存在、远程分片停在半途、mark 已落盘
//!   2. 重传同一文件 → **从断点继续**（首帧进度就大于半截长度），改名落位
//!   3. 下载同样取消再续传 → 本地正式文件与源 SHA-1 一致
//!   4. 校验和开关：打开时成功项带"已校验 SHA-1"附注
//!
//! 顺带验证续传的安全边界：把源改掉（大小/时间变了）之后必须从零重传，
//! 而不是把分片接到别的内容后面去。

use std::time::Duration;

use puttyplus::backend::checksum;
use puttyplus::backend::conn;
use puttyplus::backend::resume;
use puttyplus::backend::sftp::{ProgressSink, XferNote};
use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::panel::worker::XferConflict;
use tokio::io::AsyncWriteExt;

const SIZE: usize = 6 * 1024 * 1024; // 够大，确保取消发生在中途
const CUT: u64 = 2 * 1024 * 1024; // 在这之后请求取消

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::var("USER").unwrap();
    let key = format!("/tmp/pp_selftest/{user}_ed25519");
    puttyplus::settings::set_xfer_resume(true);
    puttyplus::settings::set_xfer_checksum(true);

    let be = conn::connect(
        "127.0.0.1",
        2222,
        &user,
        None,
        Some(&key),
        None,
        30,
        AuthMethod::default(),
        AuthOrder::default(),
    )
    .await?;
    let tmp = std::env::temp_dir();
    let ldir = tmp.join("pp_xfer_smoke");
    let _ = std::fs::remove_dir_all(&ldir);
    std::fs::create_dir_all(&ldir)?;
    let rdir = format!("{}/pp_xfer_smoke", be.cwd().await?);
    // 上次跑崩了可能留下目录，mkdir 已存在不算失败
    if !be.is_dir(&rdir).await {
        be.mkdir(&rdir).await?;
    }

    // 确定性内容：伪随机但每次跑都一样，两端 SHA-1 可比
    let src = ldir.join("big.bin");
    let mut data = vec![0u8; SIZE];
    let mut x: u32 = 0x1234_5678;
    for b in data.iter_mut() {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (x >> 16) as u8;
    }
    std::fs::write(&src, &data)?;
    let want = checksum::sha1_file(&src)?;
    println!("[0] 源 {SIZE} 字节  SHA-1 {want}");

    /* ---- 1. 上传中途取消 ---- */
    let last = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let last_cb = last.clone();
    let sink = ProgressSink::new(move |done, _total, _n| {
        last_cb.store(done, std::sync::atomic::Ordering::Relaxed);
        done > CUT
    });
    let note = XferNote::default();
    let e = be
        .upload_rec(
            src.to_str().unwrap(),
            &rdir,
            &sink,
            XferConflict::Overwrite,
            &note,
        )
        .await
        .expect_err("传了 CUT 字节就该被取消");
    assert!(e.0.contains("已取消"), "取消原因不对：{e}");
    println!(
        "[1] 上传已取消（停在 {} 字节）：{e}",
        last.load(std::sync::atomic::Ordering::Relaxed)
    );

    let rfinal = format!("{rdir}/big.bin");
    let rpart = resume::part_name(&rfinal);
    assert!(
        !be.sftp().try_exists(&rfinal).await.unwrap_or(false),
        "取消后正式名不该出现"
    );
    let part_size = be
        .sftp()
        .metadata(&rpart)
        .await
        .ok()
        .and_then(|m| m.size)
        .expect("远程分片应留存");
    // 取消在"超过 CUT 的那一帧"生效，分片最多多出一个分块
    assert!(
        part_size > 0 && part_size <= CUT + 128 * 1024,
        "分片大小异常：{part_size}"
    );
    println!("[1] 正式名不存在 ✓  远程分片 {part_size} 字节 ✓");

    /* ---- 2. 续传：首帧进度就该大于半截长度 ---- */
    let first_frame = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let ff_cb = first_frame.clone();
    let sink = ProgressSink::new(move |done, _t, _n| {
        if ff_cb.load(std::sync::atomic::Ordering::Relaxed) == u64::MAX {
            ff_cb.store(done, std::sync::atomic::Ordering::Relaxed);
        }
        false
    });
    let note = XferNote::default();
    be.upload_rec(
        src.to_str().unwrap(),
        &rdir,
        &sink,
        XferConflict::Overwrite,
        &note,
    )
    .await?;
    assert!(
        first_frame.load(std::sync::atomic::Ordering::Relaxed) > part_size,
        "没有续传：首帧 {} 应大于分片 {part_size}",
        first_frame.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "[2] 续传生效：首帧 {} > 分片 {part_size}（实际只重传了 {} 字节）",
        first_frame.load(std::sync::atomic::Ordering::Relaxed),
        SIZE as u64 - first_frame.load(std::sync::atomic::Ordering::Relaxed)
    );

    // 正式名出现、分片消失、SHA-1 一致
    let rsum = checksum::remote_sha1(&be, &rfinal).await?;
    assert!(!be.sftp().try_exists(&rpart).await.unwrap_or(false), "分片应已改名");
    assert_eq!(rsum, want, "远端 SHA-1 与源不一致");
    println!("[2] 远端 SHA-1 {rsum} ✓  分片已改名 ✓");
    assert_eq!(note.get().as_deref(), Some("已校验 SHA-1"), "{:?}", note.get());
    println!("[2] 附注：{}", note.get().unwrap());

    /* ---- 3. 下载：取消 → 续传 ---- */
    let ldst_dir = ldir.join("back");
    std::fs::create_dir_all(&ldst_dir)?;
    let lfinal = ldst_dir.join("big.bin");
    let sink = ProgressSink::new(|done, _t, _n| done > CUT);
    let e = be
        .download_rec(&rfinal, ldst_dir.to_str().unwrap(), &sink, XferConflict::Overwrite, &XferNote::default())
        .await
        .expect_err("下载也该被取消");
    assert!(e.0.contains("已取消"), "{e}");
    assert!(
        !lfinal.exists(),
        "取消后本地正式名不该出现（实际有 {}）",
        resume::part_path(&lfinal).display()
    );
    let lpart_size = std::fs::metadata(resume::part_path(&lfinal))?.len();
    println!("[3] 下载已取消，本地分片 {lpart_size} 字节，正式名未出现 ✓");

    let first_frame = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let ff_cb = first_frame.clone();
    let sink = ProgressSink::new(move |done, _t, _n| {
        if ff_cb.load(std::sync::atomic::Ordering::Relaxed) == u64::MAX {
            ff_cb.store(done, std::sync::atomic::Ordering::Relaxed);
        }
        false
    });
    be.download_rec(
        &rfinal,
        ldst_dir.to_str().unwrap(),
        &sink,
        XferConflict::Overwrite,
        &XferNote::default(),
    )
    .await?;
    assert!(first_frame.load(std::sync::atomic::Ordering::Relaxed) > lpart_size, "下载没有续传");
    println!("[3] 下载续传生效：首帧 {} > {lpart_size}", first_frame.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(checksum::sha1_file(&lfinal)?, want, "本地文件 SHA-1 不符");

    /* ---- 4. 边界：源变了必须从零重传，不能续在旧分片后面 ---- */
    let src2 = ldir.join("big2.bin");
    let mut d2 = data.clone();
    d2[0] ^= 0xff;
    // 故意写成与源**同长度**（只有内容不同）——这是最容易续错的情形
    std::fs::write(&src2, &d2)?;
    let rpart2 = resume::part_name(&format!("{rdir}/big2.bin"));
    // 造一个"属于旧源"的假分片
    {
        let mut f = be
            .sftp()
            .open_with_flags(
                &rpart2,
                russh_sftp::protocol::OpenFlags::CREATE
                    | russh_sftp::protocol::OpenFlags::TRUNCATE
                    | russh_sftp::protocol::OpenFlags::WRITE,
            )
            .await?;
        f.write_all(&data[..1024 * 1024]).await?;
        let _ = f.shutdown().await;
    }
    // 上一次传输的 mark（内容 = 老源的 size/mtime），续传判定必须放行…
    let meta = std::fs::metadata(&src)?;
    let fake = resume::Mark::new(
        meta.len(),
        meta.modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32,
    );
    resume::save_mark(&format!("up|{}|{rpart2}", be.id), &fake);
    // …但 src2 的 mtime 与 src 不同，所以续传必须被拒
    assert_ne!(
        std::fs::metadata(&src2)?.modified().unwrap(),
        meta.modified().unwrap(),
        "本用例需要两个源文件 mtime 不同"
    );
    let first_frame = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let ff_cb = first_frame.clone();
    let sink = ProgressSink::new(move |done, _t, _n| {
        if ff_cb.load(std::sync::atomic::Ordering::Relaxed) == u64::MAX {
            ff_cb.store(done, std::sync::atomic::Ordering::Relaxed);
        }
        false
    });
    let note = XferNote::default();
    be.upload_rec(
        src2.to_str().unwrap(),
        &rdir,
        &sink,
        XferConflict::Overwrite,
        &note,
    )
    .await?;
    assert!(
        first_frame.load(std::sync::atomic::Ordering::Relaxed) <= 1024 * 1024,
        "源变了却续在旧分片后面（首帧 {}）",
        first_frame.load(std::sync::atomic::Ordering::Relaxed)
    );
    let rsum2 = checksum::remote_sha1(&be, &format!("{rdir}/big2.bin")).await?;
    let want2 = checksum::sha1_file(&src2)?;
    assert_eq!(rsum2, want2, "换源后传错了内容");
    println!("[4] 源变了 → 旧分片被丢弃、从零重传 ✓（首帧 {}）", first_frame.load(std::sync::atomic::Ordering::Relaxed));

    /* ---- 收尾 ---- */
    let _ = be.sftp().remove_file(&rfinal).await;
    let _ = be.sftp().remove_file(&format!("{rdir}/big2.bin")).await;
    let _ = be.sftp().remove_dir(&rdir).await;
    let _ = std::fs::remove_dir_all(&ldir);
    tokio::time::sleep(Duration::from_millis(200)).await;
    println!("\n全部通过：续传、改名落位、SHA-1 校验、换源不误续。");
    Ok(())
}
