//! **堡垒机场景：回环目标的 ssh 不能被 known_hosts 挡住**（2026-11-11 记）。
//!
//! 用户报：「在堡垒机上分屏失败」——分屏时终端报
//! `WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED` + `Host key
//! verification failed.`，退出码 255 ⇒ **分屏开不出来**。
//!
//! 根因：堡垒机在**本地开一个随机端口**，我们连的就是那个端口 ⇒ 目标恒是
//! `127.0.0.1:<随机端口>`；而堡垒机的隧道**按访问轮换主机密钥** ⇒
//! OpenSSH 按 `[host]:port` 记的 known_hosts **必然对不上** ⇒ 硬失败。
//!
//! 这个冒烟钉的是**外部可见的量**：**真的把 ssh 拉起来跑一遍**，看它的
//! **退出码**。
//!
//! 1. 造一份 known_hosts，`[127.0.0.1]:<port>` 那条故意写**一把错的 key**
//!    （模拟「上次连的是另一台机器／网关按访问换了 key」）；
//! 2. **生产代码拼出来的 argv**（`term::build_ssh_argv`）里，回环目标必须
//!    **把 known_hosts 置空** ⇒ 压根不查 ⇒ 上面那份投毒文件**影响不到它**；
//! 3. 反向验证：把 argv 改成「**会去查**那份投毒文件」⇒ **必须失败**
//!    （退出码 255）——证明这个冒烟真抓得住那个 bug，不是「怎么都绿」。
//!
//! ⚠️ **为什么不用 `$HOME` 来定位那份 known_hosts**：OpenSSH 找 home 目录
//!    用的是 `getpwuid()`（passwd 条目），**不看 `$HOME`** ⇒ 设 `$HOME`
//!    是**没用的**（踩过一次：投毒文件根本没被读到，两步都绿）。
//!    ⇒ 只能**显式** `-o UserKnownHostsFile=<那份文件>`。
//!
//! ```text
//! xvfb-run -a cargo run --example loopback_hostkey_smoke
//! ```
//!
//! ⚠️ 需要本机测试 sshd 在 **127.0.0.1:2233**（公钥认证）：
//! `~/.cache/puttyplus-test-sshd`。

use std::path::{Path, PathBuf};
use std::process::Command;

use puttyplus::args::ConnOptions;
use puttyplus::term::build_ssh_argv;

/// 本机测试 sshd（`~/.cache/puttyplus-test-sshd`，端口 2233）。
const HOST: &str = "127.0.0.1";
const PORT: i32 = 2233;

/// 用户的私钥（测试 sshd 的 authorized_keys 里就是它）。
fn key_path() -> String {
    std::env::var("PP_SSH_KEY").unwrap_or_else(|_| {
        let h = std::env::var("PP_REAL_HOME").unwrap_or_else(|_| "/home/user".into());
        format!("{h}/.ssh/id_rsa")
    })
}

/// 造一份 known_hosts：`[127.0.0.1]:<port>` 那条是**一把错的 key**
/// （模拟端口复用时后面换了台机器／网关换了 key）。
fn poisoned_known_hosts(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pp_loopback_kh_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");

    // 造一把**不是服务器那把**的 ed25519 公钥
    let fake = dir.join("fake");
    let _ = std::fs::remove_file(&fake);
    let out = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&fake)
        .arg("-C")
        .arg("fake")
        .output()
        .expect("跑 ssh-keygen");
    assert!(out.status.success(), "ssh-keygen 失败：{out:?}");
    let publine = std::fs::read_to_string(format!("{}.pub", fake.display())).expect("读假公钥");
    let fields: Vec<&str> = publine.split_whitespace().collect();
    let kh = dir.join("known_hosts");
    std::fs::write(&kh, format!("[{HOST}]:{PORT} {} {}\n", fields[0], fields[1]))
        .expect("写 known_hosts");
    kh
}

/// 跑一次 ssh（末尾补个 `true` ⇒ 退出码反映的是**连接**的结果，
/// 而不是交互 shell 立刻 EOF 退出），返回（退出码, stderr）。
fn run_ssh(argv: &[String]) -> (i32, String) {
    let mut args: Vec<String> = argv.to_vec();
    args.push("true".into());
    let out = Command::new(&args[0])
        .args(&args[1..])
        .output()
        .expect("拉起 ssh");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.code().unwrap_or(-1), err)
}

/// 从 argv 里摘掉那两条 known_hosts 置空（其余原样保留）。
fn without_known_hosts_opts(argv: &[String]) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();
    let mut it = argv.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-o" {
            if let Some(v) = it.peek() {
                if v.contains("KnownHostsFile=/dev/null") {
                    it.next(); // 把这条 `-o <值>` 整对丢掉
                    continue;
                }
            }
        }
        bad.push(a.clone());
    }
    bad
}

/// 把额外的 `-o <值>` 插到 **`--` 之前**。
///
/// ⚠️ **必须插在 `--` 前面**：`--` 之后的一切都是**远程命令** ⇒ 插在后面
///    的话 ssh 根本不会把它当选项（会拿它去跑远端命令 ⇒ 失败原因变成
///    「远端命令出错」，**测的就不是主机密钥那件事了**）。这个坑踩过一次。
fn insert_opts_before_ddash(argv: &[String], extra: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut inserted = false;
    for a in argv {
        if !inserted && a == "--" {
            out.extend(extra.iter().cloned());
            inserted = true;
        }
        out.push(a.clone());
    }
    if !inserted {
        out.extend(extra.iter().cloned());
    }
    out
}

fn main() {
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: HOST.into(),
        port: PORT,
        user: "fwt".into(),
        keyfile: Some(key_path()),
        ..Default::default()
    };
    let Some(av) = build_ssh_argv(&opts) else {
        panic!("这台机器上找不到 ssh（PP_TERM_SSH=inproc 那条路才不需要它）");
    };

    // ---- 1) 回环目标的 argv 必须把 known_hosts 置空 ----
    for want in ["UserKnownHostsFile=/dev/null", "GlobalKnownHostsFile=/dev/null"] {
        assert!(
            av.iter().any(|a| a == want),
            "argv 里少了 {want} ⇒ 回环目标会被 known_hosts 挡住：{av:?}"
        );
    }
    assert!(
        av.iter().any(|a| a == "StrictHostKeyChecking=accept-new"),
        "StrictHostKeyChecking=accept-new 必须保留：{av:?}"
    );

    // ---- 2) 置空之后 ⇒ 连得上 ----
    let (rc, err) = run_ssh(&av);
    assert_eq!(
        rc, 0,
        "回环目标连不上（退出码 {rc}）\nssh 说：\n{err}"
    );
    println!("[1] 回环目标（known_hosts 置空）⇒ 连上（退出码 0）✓");

    // ---- 3) 反向验证：让 ssh **去查**那份投毒的 known_hosts ⇒ 必须失败 ----
    let kh = poisoned_known_hosts("bad");
    let bad = insert_opts_before_ddash(
        &without_known_hosts_opts(&av),
        &[
            "-o".into(),
            format!("UserKnownHostsFile={}", kh.display()),
            "-o".into(),
            "GlobalKnownHostsFile=/dev/null".into(),
        ],
    );
    let (rc2, err2) = run_ssh(&bad);
    assert_ne!(
        rc2, 0,
        "ssh 去查了 known_hosts 却**居然连上了** ⇒ 断言抓不住这个 bug"
    );
    // ⚠️ **必须确认失败原因是「主机密钥变了」** ⇒ 不能是认证失败之类
    //    （否则这个反向验证就白做了）。
    assert!(
        err2.contains("Host key verification failed")
            || err2.contains("HOST IDENTIFICATION HAS CHANGED"),
        "失败原因不是主机密钥校验 ⇒ 反向验证没测到点子上：\n{err2}"
    );
    println!("[2] 反向验证：查 known_hosts ⇒ 失败（退出码 {rc2}，主机密钥校验）✓ 断言真抓得住");

    let _ = std::fs::remove_dir_all(kh.parent().unwrap_or(Path::new("/")));
    println!("loopback_hostkey_smoke: OK");
}
