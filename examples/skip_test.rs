//! 「跳过已存在」策略的真机验证（手工脚本，不进 CI）。
//! 运行：cargo run --example skip_test
//! 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use puttyplus::backend::sftp::{ProgressSink, XferNote};
use puttyplus::panel::worker::XferConflict;

/// 远端目录（与本地源路径错开）。
const RDIR: &str = "/tmp/pp_skip_dst";

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // 与其它冒烟脚本同一套认证：用户态 sshd 只认公钥
        let user = std::env::var("USER").unwrap();
        let key = format!("/tmp/pp_selftest/{user}_ed25519");
        let c = conn::connect(
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
        .await
        .expect("connect");

        std::fs::write("/tmp/skip_src.txt", "hello").unwrap();
        // 远端目录必须与本地源分开：本机 sshd 上两边是同一文件系统，
        // 同名时"改名成正式名"会与正被读取的源文件打架
        if !c.is_dir(RDIR).await {
            c.mkdir(RDIR).await.unwrap();
        }

        // 第一次：写入
        let r1 = up(&c, XferConflict::Overwrite).await;
        // 第二次：Skip 策略 → 应返回 Ok(false)
        let r2 = up(&c, XferConflict::Skip).await;
        // Overwrite → Ok(true)
        let r3 = up(&c, XferConflict::Overwrite).await;
        println!("first={r1:?} skip={r2:?} overwrite={r3:?}");
        assert_eq!(r1.unwrap(), true);
        assert_eq!(r2.unwrap(), false, "Skip 命中应返回 false");
        assert_eq!(r3.unwrap(), true);

        // 下载同理
        std::fs::create_dir_all("/tmp/pp_dl_skip").unwrap();
        let _ = std::fs::remove_file("/tmp/pp_dl_skip/skip_src.txt");
        let d1 = down(&c, XferConflict::Skip).await;
        println!("download skip (目标不存在)={d1:?}");
        assert_eq!(d1.unwrap(), true, "目标不存在时 Skip 不命中");

        std::fs::write("/tmp/pp_dl_skip/skip_src.txt", "old").unwrap();
        let d2 = down(&c, XferConflict::Skip).await;
        println!("download skip (目标存在)={d2:?}");
        assert_eq!(d2.unwrap(), false, "本地已存在应返回 false");

        // 内容未被覆盖
        assert_eq!(
            std::fs::read_to_string("/tmp/pp_dl_skip/skip_src.txt").unwrap(),
            "old",
            "跳过时旧内容保持"
        );

        c.close().await;
        println!("SKIP-SEMANTICS-ALL-OK");
    });
}

async fn up(
    c: &puttyplus::backend::conn::SftpBackend,
    conflict: XferConflict,
) -> puttyplus::backend::BackendResult<bool> {
    c.upload_rec(
        "/tmp/skip_src.txt",
        RDIR,
        &ProgressSink::none(),
        conflict,
        &XferNote::default(),
    )
    .await
}

async fn down(
    c: &puttyplus::backend::conn::SftpBackend,
    conflict: XferConflict,
) -> puttyplus::backend::BackendResult<bool> {
    c.download_rec(
        &format!("{RDIR}/skip_src.txt"),
        "/tmp/pp_dl_skip",
        &ProgressSink::none(),
        conflict,
        &XferNote::default(),
    )
    .await
}
