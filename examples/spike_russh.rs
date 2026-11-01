//! Spike 2：russh + russh-sftp 实连本地 sshd 验证。
//! 运行：cargo run --example spike_russh
// 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。

use std::sync::Arc;
use std::time::Duration;

use russh::client::Handler;
use russh::keys::PrivateKeyWithHashAlg;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

struct SpikeHandler;

impl Handler for SpikeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // 回环地址按 C 版策略直接信任（正式实现在 backend::known_hosts 中）
        Ok(true)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Arc::new(russh::client::Config {
        inactivity_timeout: Some(Duration::from_secs(30)),
        keepalive_interval: None,
        ..Default::default()
    });

    let mut session = russh::client::connect(config, ("127.0.0.1", 2222), SpikeHandler).await?;
    println!("[1] connect ok");

    let user = whoami();
    let key_pair = russh::keys::load_secret_key("/tmp/pp_selftest/client_ed25519", None)?;
    let key = PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);
    let auth = session.authenticate_publickey(user, key).await?;
    println!("[2] publickey auth success = {}", auth.success());
    if !auth.success() {
        return Err("publickey auth failed".into());
    }

    // SFTP 子系统
    let channel = session.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream()).await?;
    println!("[3] sftp session ok");

    let cwd = sftp.canonicalize(".").await?;
    println!("[4] canonicalize(.) = {cwd}");

    // 写 + 读回一个文件
    let test_path = "/tmp/pp_selftest/russh_spike.txt";
    let mut f = sftp
        .open_with_flags(test_path, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE | OpenFlags::READ)
        .await?;
    f.write_all(b"puttyplus rust spike").await?;
    f.flush().await?;
    let mut buf = Vec::new();
    f.rewind().await?;
    f.read_to_end(&mut buf).await?;
    println!("[5] write+read back: {}", String::from_utf8(buf)?);
    f.shutdown().await?;

    // 列目录
    let mut n = 0;
    for entry in sftp.read_dir("/tmp/pp_selftest").await? {
        println!("    entry: {:?}", entry.file_name());
        n += 1;
    }
    println!("[6] read_dir entries = {n}");
    sftp.rename(test_path, "/tmp/pp_selftest/russh_spike_renamed.txt").await?;
    sftp.remove_file("/tmp/pp_selftest/russh_spike_renamed.txt").await?;
    println!("[7] rename+remove ok");

    // exec 通道（目录跟随要用）
    let ch = session.channel_open_session().await?;
    ch.exec(true, "echo hello-from-exec").await?;
    let mut out = String::new();
    let mut stream = ch.into_stream();
    let mut reader = tokio::io::BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    println!("[8] exec output: {}", line.trim());

    sftp.close().await?;
    session
        .disconnect(russh::Disconnect::ByApplication, "spike done", "en")
        .await?;
    println!("ALL SPIKES PASSED");
    Ok(())
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "nobody".into())
}
