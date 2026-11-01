//! 内置查看/编辑器远端读写冒烟（不启动 GUI）。
//!
//! 对真实 sshd 验证 7.3 的两条后端链路：
//!   `read_file_limited`（大小限制、目录拒绝）与
//!   `write_file_atomic`（临时文件 + rename、覆盖已存在文件、权限位保留）。
//!
//! 运行（用户态 sshd 即可）：
//!   cargo run --example fileedit_smoke -- <host> <port> <user> <keyfile> [远端目录]
//!
//! 默认写远端主目录；容器/沙箱里主目录可能不可写，这时显式给一个可写目录。
//! 测完删除临时文件（目录是本次新建的也一并删除）；失败时非零退出。

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use puttyplus::util::path_join;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pos = std::env::args().skip(1);
    let host = pos.next().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = pos.next().unwrap_or_else(|| "2222".into()).parse()?;
    let user = pos.next().ok_or("需要 user")?;
    let key = pos.next().ok_or("需要 keyfile")?;
    let remote_dir = pos.next();

    let c = conn::connect(
        &host,
        i32::from(port),
        &user,
        None,
        Some(&key),
        None,
        0,
        AuthMethod::default(),
        AuthOrder::PasswordFirst,
    )
    .await?;
    let dir = match remote_dir {
        Some(d) => d,
        None => c.cwd().await?,
    };
    let created_dir = if c.is_dir(&dir).await {
        false
    } else {
        c.mkdir(&dir).await?;
        true
    };
    let path = path_join(&dir, ".puttyplus-fileedit-smoke.txt");

    // 1. 新建（目标不存在）：临时文件 + rename 应成功
    c.write_file_atomic(&path, "第一行\n第二行\n".as_bytes())
        .await?;

    // 2. 读回
    let got = c.read_file_limited(&path, 1024).await?;
    assert_eq!(got, "第一行\n第二行\n".as_bytes(), "读回内容不一致");

    // 3. 覆盖已存在的文件，并把权限位改成 0600 验证保留
    let mut md = russh_sftp::protocol::FileAttributes::empty();
    md.permissions = Some(0o600);
    c.sftp().set_metadata(&path, md).await?;
    c.write_file_atomic(&path, "overwritten\n".as_bytes())
        .await?;
    let got = c.read_file_limited(&path, 1024).await?;
    assert_eq!(got, b"overwritten\n", "覆盖后内容不一致");
    let perms = c
        .sftp()
        .metadata(&path)
        .await?
        .permissions
        .map(|p| p & 0o7777);
    assert_eq!(perms, Some(0o600), "覆盖保存后权限位没有保留");

    // 4. 大小上限：读 1 字节应被拒
    let err = c
        .read_file_limited(&path, 1)
        .await
        .err()
        .expect("应拒绝超限");
    assert!(err.0.contains("超过"), "超限错误文案不对: {}", err.0);

    // 5. 目录不能当文件读
    let err = c
        .read_file_limited(&dir, 1024)
        .await
        .err()
        .expect("应拒绝目录");
    assert!(err.0.contains("目录"), "目录错误文案不对: {}", err.0);

    c.sftp().remove_file(&path).await?;
    if created_dir {
        c.sftp().remove_dir(&dir).await?;
    }
    c.close().await;
    println!("fileedit smoke ok: {path}");
    Ok(())
}
