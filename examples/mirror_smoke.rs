//! 单向目录镜像（6.2）真机冒烟。
//! 运行：cargo run --example mirror_smoke
//! 运行前提：/tmp/pp_selftest 下有用户态 sshd（端口 2222，公钥认证）。
//!
//! 对着真 sshd 走完一整轮：**扫描 → 执行 → 再扫描**。重点验三件事：
//!   1. 首次扫描报出该上传的条目，按序执行后远端内容与本地逐字节一致
//!   2. **第二次扫描必须是"无变化"**——这是"镜像"与"每次都重传一遍"的
//!      分界线（靠上传后对齐远端 mtime 达成；服务器时钟偏一点也该成立）
//!   3. 删除只在勾选后才出现在计划里，且顺序排在所有上传之后

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::checksum;
use puttyplus::backend::conn;
use puttyplus::backend::mirror::{self, Action, Ignore, Opts};
use puttyplus::backend::sftp::{ProgressSink, XferNote};

fn lname(root: &str, rel: &str) -> String {
    mirror::local_path(std::path::Path::new(root), rel)
        .to_string_lossy()
        .into_owned()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user = std::env::var("USER")?;
    let key = format!("/tmp/pp_selftest/{user}_ed25519");
    puttyplus::settings::set_xfer_resume(false);
    puttyplus::settings::set_xfer_checksum(false);

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
    let lroot = tmp.join("pp_mirror_smoke");
    let _ = std::fs::remove_dir_all(&lroot);
    std::fs::create_dir_all(lroot.join("sub/deep"))?;
    std::fs::write(lroot.join("a.txt"), b"AAA")?;
    std::fs::write(lroot.join("sub/b.txt"), b"BBBB")?;
    std::fs::write(lroot.join("sub/deep/c.bin"), vec![7u8; 4096])?;
    // 必须被忽略规则挡掉
    std::fs::create_dir_all(lroot.join(".git"))?;
    std::fs::write(lroot.join(".git/config"), "绝不该上传".as_bytes())?;

    let rroot = format!("{}/pp_mirror_smoke", be.cwd().await?);
    let _ = be.remove_rec(&rroot).await; // 上次跑崩的残留

    let ig = Ignore::parse(mirror::DEFAULT_IGNORE);
    let lroot_s = lroot.to_string_lossy().into_owned();

    /* ---- 1. 首次扫描 ---- */
    let l = mirror::walk_local(&lroot, &ig)?;
    let r = be.walk_dir(&rroot, &ig).await?;
    println!("[1] 本地 {} 项（跳过 {}），远端存在 = {}", l.entries.len(), l.skipped.len(), r.root_exists);
    assert!(!r.root_exists, "刚清空的远端目录应报「不存在」");
    assert!(l.skipped.iter().any(|s| s == ".git"), "`.git` 必须被忽略：{:?}", l.skipped);

    let plan = mirror::build_plan(&l.entries, &r.entries, &Opts::default(), !r.root_exists);
    println!("[1] {}", plan.summary(!r.root_exists));
    assert!(plan.remote_only.is_empty(), "空远端不该有 remote_only");
    let ups: Vec<&str> = plan
        .items
        .iter()
        .filter(|i| i.action == Action::Upload)
        .map(|i| i.rel.as_str())
        .collect();
    assert_eq!(ups, ["a.txt", "sub/b.txt", "sub/deep/c.bin"], "不该包含 .git 下的东西");

    /* ---- 2. 按序执行 ---- */
    for it in mirror::order(&plan) {
        let note = XferNote::default();
        match it.action {
            Action::Mkdir => {
                be.mkdir(&mirror::remote_path(&rroot, &it.rel)).await?;
            }
            Action::Upload => {
                be.mirror_upload(
                    &lname(&lroot_s, &it.rel),
                    &mirror::remote_path(&rroot, &it.rel),
                    &ProgressSink::none(),
                    &note,
                )
                .await?;
            }
            other => panic!("不该出现动作 {other:?}"),
        }
        println!("    {} {}", it.action.label(), it.rel);
    }

    /* ---- 3. 内容逐字节一致 ---- */
    for rel in ["a.txt", "sub/b.txt", "sub/deep/c.bin"] {
        let lp = lroot_s.clone() + "/" + rel;
        let want = checksum::sha1_file(std::path::Path::new(&lp))?;
        let rp = mirror::remote_path(&rroot, rel);
        let got = checksum::remote_sha1(&be, &rp).await?;
        assert!(
            want.eq_ignore_ascii_case(&got),
            "{rel} SHA-1 对不上：本地 {want} / 远端 {got}"
        );
        println!("[3] {rel} SHA-1 一致");
    }
    assert!(
        !be.is_dir(&format!("{rroot}/.git")).await,
        "忽略规则失效：.git 被传上去了"
    );

    /* ---- 4. 第二次扫描必须是「无变化」 ---- */
    let r2 = be.walk_dir(&rroot, &ig).await?;
    let plan2 = mirror::build_plan(&l.entries, &r2.entries, &Opts::default(), !r2.root_exists);
    println!("[4] 第二次扫描：{}", plan2.summary(!r2.root_exists));
    assert!(
        plan2.is_empty(),
        "第二次扫描应当无动作，实际：{:?}",
        plan2.visible(true).iter().map(|i| (i.action, &i.rel)).collect::<Vec<_>>()
    );

    /* ---- 5. 远端多出一个文件：默认不动，勾删除才列出来 ---- */
    let stray = format!("{rroot}/stray.txt");
    be.mirror_upload(
        &lname(&lroot_s, "a.txt"),
        &stray,
        &ProgressSink::none(),
        &XferNote::default(),
    )
    .await?;
    let r3 = be.walk_dir(&rroot, &ig).await?;
    let p3 = mirror::build_plan(&l.entries, &r3.entries, &Opts::default(), !r3.root_exists);
    assert!(p3.is_empty(), "多出来的文件不该触发任何动作");
    assert!(p3.remote_only.iter().any(|x| x == "stray.txt"), "{:?}", p3.remote_only);
    println!("[5] 远端多出的 stray.txt 默认不动 ✓");

    let p4 = mirror::build_plan(
        &l.entries,
        &r3.entries,
        &Opts { delete: true, ..Opts::default() },
        !r3.root_exists,
    );
    println!("[5] 勾选删除后 {}", p4.summary(!r3.root_exists));
    let dels: Vec<&str> = mirror::order(&p4)
        .into_iter()
        .filter(|i| i.action == Action::Delete)
        .map(|i| i.rel.as_str())
        .collect();
    assert_eq!(dels, ["stray.txt"]);
    // 顺序：删除必须排在所有上传之后（没有上传项时这条无从谈起）
    let order = mirror::order(&p4);
    let last_upload = order.iter().rposition(|i| i.action == Action::Upload);
    let first_del = order.iter().position(|i| i.action == Action::Delete);
    if let (Some(lu), Some(fd)) = (last_upload, first_del) {
        assert!(fd > lu, "删除必须排在所有上传之后");
        println!("[5] 顺序：最后一个上传 #{lu} → 第一个删除 #{fd} ✓");
    }

    /* ---- 6. 改了本地文件 → 必须能覆盖远端那份已存在的 ---- */
    // 这条正是覆盖上传的坑：OpenSSH 的 SSH_FXP_RENAME 在目标已存在时
    // 直接返回 Failure（russh-sftp 没暴露 posix-rename 扩展），
    // 不兜底的话每次改完文件重新同步都会挂在最后一步改名上。
    std::fs::write(lroot.join("a.txt"), b"CHANGED-AND-LONGER")?;
    std::thread::sleep(std::time::Duration::from_millis(1100)); // 让 mtime 确定地前进
    let l4 = mirror::walk_local(&lroot, &ig)?;
    let r4 = be.walk_dir(&rroot, &ig).await?;
    let p5 = mirror::build_plan(&l4.entries, &r4.entries, &Opts::default(), !r4.root_exists);
    println!("[6] 改完 a.txt 后 {}", p5.summary(!r4.root_exists));
    let ups: Vec<&str> = p5
        .items
        .iter()
        .filter(|i| i.action == Action::Upload)
        .map(|i| i.rel.as_str())
        .collect();
    assert_eq!(ups, ["a.txt"], "只应重传改动的那个");
    for it in mirror::order(&p5) {
        if it.action == Action::Upload {
            be.mirror_upload(
                &lname(&lroot_s, &it.rel),
                &mirror::remote_path(&rroot, &it.rel),
                &ProgressSink::none(),
                &XferNote::default(),
            )
            .await?;
        }
    }
    let want = checksum::sha1_file(&lroot.join("a.txt"))?;
    let got = checksum::remote_sha1(&be, &mirror::remote_path(&rroot, "a.txt")).await?;
    assert!(want.eq_ignore_ascii_case(&got), "覆盖后内容对不上：{want} vs {got}");
    println!("[6] 覆盖远端已有文件成功，SHA-1 一致 ✓");

    // 覆盖完再扫一次仍应是零动作
    let r5 = be.walk_dir(&rroot, &ig).await?;
    let p6 = mirror::build_plan(&l4.entries, &r5.entries, &Opts::default(), !r5.root_exists);
    assert!(p6.is_empty(), "覆盖后再扫应无动作：{:?}", p6.visible(true).iter().map(|i| (i.action, &i.rel)).collect::<Vec<_>>());
    println!("[6] 覆盖后再扫仍是零动作 ✓");

    be.remove_rec(&rroot).await?;
    let _ = std::fs::remove_dir_all(&lroot);
    println!("\n✔ 镜像：扫描 → 执行 → 幂等复扫 → 删除受控 → 覆盖上传，全链路通");
    Ok(())
}
