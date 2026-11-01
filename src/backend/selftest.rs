//! `--sftp-selftest` 集成自检（对应 C 版 sftp_backend_selftest）：
//! 对真实 sshd 跑一轮 增删改/列表/递归上传下载/重命名/删除 校验。

use super::conn;
use super::sftp::{ProgressSink, XferNote};
use crate::panel::worker::XferConflict;

fn st_check(cond: bool, what: &str) -> i32 {
    println!("  [{}] {}", if cond { "OK" } else { "FAIL" }, what);
    i32::from(!cond)
}

/// 递归删除本地目录树（仅自检用）。
fn rm_local_tree(path: &std::path::Path) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for ent in rd.flatten() {
                rm_local_tree(&ent.path());
            }
        }
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

fn make_local_tmpdir(prefix: &str) -> std::io::Result<std::path::PathBuf> {
    let base = std::env::temp_dir();
    for i in 0..100u32 {
        let d = base.join(format!("{prefix}{}{}", std::process::id(), i));
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no tmpdir slot",
    ))
}

pub fn run(
    host: &str,
    port: i32,
    user: &str,
    password: Option<&str>,
    proxycmd: Option<&str>,
) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("无法创建 tokio 运行时: {e}");
            return 1;
        }
    };
    rt.block_on(run_async(host, port, user, password, proxycmd))
}

async fn run_async(
    host: &str,
    port: i32,
    user: &str,
    password: Option<&str>,
    proxycmd: Option<&str>,
) -> i32 {
    let mut fails = 0;

    println!("== 连接 {user}@{host}:{port} ==");
    // 与 C 版 --sftp-selftest 一致：保活 30s（不走会话覆盖）
    let c = match conn::connect(
        host,
        port,
        user,
        password,
        None,
        proxycmd,
        30,
        crate::args::AuthMethod::default(),
        crate::args::AuthOrder::default(),
    )
    .await {
        Ok(c) => {
            fails += st_check(true, "连接+认证");
            c
        }
        Err(e) => {
            st_check(false, &e.0);
            return 1;
        }
    };

    let cwd = match c.cwd().await {
        Ok(d) => {
            fails += st_check(true, "获取主目录");
            d
        }
        Err(e) => {
            fails += st_check(false, &e.0);
            String::new()
        }
    };
    println!("  远程主目录: {cwd}");

    let base = if cwd.is_empty() { "/tmp".into() } else { cwd };
    let testdir = format!("{base}/.puttyplus_selftest");
    let _ = c.remove_rec(&testdir).await;
    fails += match c.mkdir(&testdir).await {
        Ok(()) => st_check(true, "创建远程目录"),
        Err(e) => st_check(false, &e.0),
    };
    fails += st_check(c.is_dir(&testdir).await, "目录属性判断");

    // 造本地源树：dir/a.txt(小) dir/sub/b.txt(100KB)
    let lbase = make_local_tmpdir("ppst-").expect("tmpdir");
    let ldir = lbase.join("dir");
    let lsub = ldir.join("sub");
    std::fs::create_dir_all(&lsub).unwrap();
    let fa = ldir.join("a.txt");
    let fb = lsub.join("b.txt");
    std::fs::write(&fa, b"AAAAAAAA").unwrap();
    std::fs::write(&fb, "0123456789".repeat(10 * 1024)).unwrap();

    // 递归上传
    let ldir_s = ldir.to_string_lossy().into_owned();
    fails += match c.upload_rec(&ldir_s, &testdir, &ProgressSink::none(), XferConflict::Overwrite, &XferNote::default()).await {
        Ok(_) => st_check(true, "递归上传目录"),
        Err(e) => st_check(false, &e.0),
    };
    fails += st_check(
        c.is_dir(&format!("{testdir}/dir/sub")).await,
        "上传后子目录存在",
    );

    // 递归下载到另一目录
    let lb2 = make_local_tmpdir("ppdl-").expect("tmpdir");
    fails += match c
        .download_rec(&format!("{testdir}/dir"), &lb2.to_string_lossy(), &ProgressSink::none(), XferConflict::Overwrite, &XferNote::default())
        .await
    {
        Ok(_) => st_check(true, "递归下载目录"),
        Err(e) => st_check(false, &e.0),
    };
    let fb2 = lb2.join("dir/sub/b.txt");
    let chk = std::fs::read(&fb2).unwrap_or_default();
    fails += st_check(chk.len() == 100 * 1024, "下载文件内容校验 (100KB)");
    let fa2 = lb2.join("dir/a.txt");
    let chk2 = std::fs::read_to_string(&fa2).unwrap_or_default();
    fails += st_check(chk2 == "AAAAAAAA", "下载文件内容校验 (小文件)");

    // 列目录（testdir 下只有 dir 一项）
    let lst = c.list(&testdir).await.unwrap_or_default();
    fails += st_check(lst.len() == 1, "目录列表只有 1 项");

    // 空目录用例
    let emptydir = format!("{testdir}/empty-dir");
    let _ = c.mkdir(&emptydir).await;
    let lst2 = c.list(&emptydir).await.unwrap_or_default();
    fails += st_check(lst2.is_empty(), "空目录列出 0 项（不报错）");

    let lb3 = make_local_tmpdir("pped-").expect("tmpdir");
    fails += match c
        .download_rec(&emptydir, &lb3.to_string_lossy(), &ProgressSink::none(), XferConflict::Overwrite, &XferNote::default())
        .await
    {
        Ok(_) => st_check(true, "下载空目录"),
        Err(e) => st_check(false, &e.0),
    };
    fails += st_check(
        lb3.join("empty-dir").is_dir(),
        "空目录已在本地创建",
    );

    // 重命名 + 递归删除
    let rn = format!("{testdir}/dir-renamed");
    fails += match c.rename(&format!("{testdir}/dir"), &rn).await {
        Ok(()) => st_check(true, "重命名"),
        Err(e) => st_check(false, &e.0),
    };
    fails += match c.remove_rec(&testdir).await {
        Ok(()) => st_check(true, "递归删除"),
        Err(e) => st_check(false, &e.0),
    };
    fails += st_check(!c.is_dir(&testdir).await, "删除后目录不存在");

    c.close().await;
    rm_local_tree(&lbase);
    rm_local_tree(&lb2);
    rm_local_tree(&lb3);
    println!(
        "== 自检结果: {} ({fails} 项失败) ==",
        if fails > 0 { "失败" } else { "全部通过" }
    );
    fails
}
