//! 复现/验证大文件下载超时：连接 srv-01 下载 58MB 文件并计时。
//! cargo run --release --example download_aly [host] [file]
use std::time::Instant;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use puttyplus::backend::sftp::{ProgressSink, XferNote};
use puttyplus::panel::worker::XferConflict;

fn main() {
    let host = std::env::args().nth(1).unwrap_or_else(|| "10.0.0.9".into());
    let file = std::env::args().nth(2).unwrap_or_else(|| "/root/pcN6R1f8K9eE8UsIhJjmX.tar.gz".into());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        println!("[1] 连接 {host}:22 (root, 默认密钥)…");
        let c = conn::connect(
            &host,
            22,
            "root",
            None,
            None,
            None,
            30,
            AuthMethod::default(),
            AuthOrder::default(),
        )
            .await
            .expect("连接失败");
        println!("[2] 已连接，开始下载 {file}");

        let sink = ProgressSink::new(|done, total, _name| {
            if done % (8 << 20) < (64 << 10) {
                println!("    {} / {}", done, total);
            }
            false
        });
        let t0 = Instant::now();
        match c.download_rec(&file, "/tmp/pp_dl_test", &sink, XferConflict::Overwrite, &XferNote::default())
            .await {
            Ok(_) => println!("[3] 下载完成，耗时 {:.1}s", t0.elapsed().as_secs_f64()),
            Err(e) => println!("[3] 下载失败（{:.1}s）: {}", t0.elapsed().as_secs_f64(), e.0),
        }
        c.close().await;
    });
}
