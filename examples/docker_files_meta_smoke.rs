//! 容器「文件」页的**修改时间 / 符号链接**冒烟（阶段 185）。
//!
//! 用户 2026-11-27 报了三条：
//! > 「容器文件管理中没有显示修改时间」
//! > 「双击符号链接不能跟随。最好能显示符号链接指向的文件」
//!
//! ## 断言
//!
//! - **A**：文件表**有**「修改时间」「指向」两列（表头能读到）；
//! - **B**：普通文件的**修改时间不是空的**（不是 1970、也不是空串）；
//! - **C**：符号链接那一行的**「指向」列**写着它真正的目标；
//! - **D**：**双击符号链接 ⇒ 真的跟过去了**（目标是目录 ⇒ 当前目录变成
//!   那个目录）。
//!
//! ## 为什么要自己造一个链接
//!
//! ⚠️ 容器里现成的链接**不一定有**（每个镜像都不一样）⇒ 冒烟自己
//!   造一个 `pp_probe_*` 的链接，验完**删掉**（不碰用户的东西）。
//!
//! ## 运行前提
//!
//! 本机测试 sshd 在 127.0.0.1:2233（公钥认证），且那台机器上**装了
//! docker**（本机就是 ⇒ 远端 = 本机 ⇒ 跑的是真 docker）。
//! 端口可用 `PP_DOCKER_PORT` 覆盖。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example docker_files_meta_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{dockerui, PanelState, SftpPanel};
use std::rc::Rc;

const HOST: &str = "127.0.0.1";

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 等某个条件成立（最多 timeout 毫秒）。
fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(100);
    }
    f()
}

/// 在容器里跑一条命令（**真 docker CLI**；测试 sshd 就在本机 ⇒ 看到的是
/// 同一个容器）。
fn docker_exec(container: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("docker")
        .args(["exec", container])
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 逐条检查，最后汇总（一条挂了不影响后面的）----
    thread_local! {
        static FAILS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                FAILS.with(|f| f.borrow_mut().push(format!("{}：{}", $name, $detail)));
            }
        }};
    }

    let port: i32 = std::env::var("PP_DOCKER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("USER").unwrap_or_else(|_| "fwt".into());
    let keyfile = std::env::var("HOME")
        .map(|h| format!("{h}/.ssh/id_rsa"))
        .unwrap_or_default();
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: HOST.into(),
        port,
        user,
        keyfile: Some(keyfile),
        ..Default::default()
    };

    let panel = SftpPanel::new(&opts);
    dockerui::ensure(&panel.st);
    let page = dockerui::page_for_test(&panel.st).expect("应当有容器页");
    let win = gtk::Window::new();
    win.set_title(Some("容器文件页 修改时间/符号链接 冒烟"));
    win.set_default_size(1200, 820);
    win.set_child(Some(&page));
    win.present();
    pump(200);
    dockerui::refresh_for_test(&panel.st);

    // ---- 等容器列表真的到（要走 SSH ⇒ 可能要几秒）----
    if !wait_until(20_000, || !dockerui::containers_for_test(&panel.st).is_empty()) {
        println!("[FAIL] 容器列表没到（测试 sshd 没连上？）");
        std::process::exit(2);
    }
    let cs = dockerui::containers_for_test(&panel.st);
    let name = cs
        .iter()
        .find(|c| c.state == "running")
        .or_else(|| cs.first())
        .map(|c| c.name.clone())
        .unwrap_or_default();
    if name.is_empty() {
        println!("[FAIL] 容器列表是空的");
        std::process::exit(2);
    }
    dockerui::expand_all_projects_for_test(&panel.st);
    pump(200);
    check!(
        "能选中容器行",
        dockerui::select_container_for_test(&panel.st, &name),
        format!("选不中容器行 {name}")
    );
    pump(400);
    println!("[信息] 选中容器：{name}");

    // ---- 切到「文件」页 ⇒ 列根目录 ----
    dockerui::files_show_for_test(&panel.st);
    check!(
        "根目录列出来了",
        wait_until(20_000, || {
            dockerui::files_path_for_test(&panel.st) == "/"
                && !dockerui::files_rows_for_test(&panel.st).is_empty()
        }),
        format!(
            "当前目录={:?} 状态={:?} 行数={}",
            dockerui::files_path_for_test(&panel.st),
            dockerui::files_status_for_test(&panel.st),
            dockerui::files_rows_for_test(&panel.st).len()
        )
    );

    // ================= A：表头有「修改时间」「指向」两列 =================
    let cols: Vec<String> = dockerui::files_cols_for_test(&panel.st)
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    println!("[信息] 文件表的列：{cols:?}");
    check!(
        "A1 文件表有「修改时间」列",
        cols.iter().any(|c| c == "修改时间" || c == "Modified time"),
        format!("列是 {cols:?}")
    );
    check!(
        "A2 文件表有「指向」列",
        cols.iter().any(|c| c == "指向" || c == "Points to"),
        format!("列是 {cols:?}")
    );

    // ================= B：普通文件的修改时间不是空的 =================
    let rows = dockerui::files_mtime_link_for_test(&panel.st);
    let with_mtime = rows.iter().filter(|(_, m, _)| !m.is_empty()).count();
    println!("[信息] 根目录前 5 行（名字/修改时间/指向）：{rows:?}");
    check!(
        "B1 至少有一行**带修改时间**",
        with_mtime > 0,
        format!("{with_mtime}/{} 行有修改时间", rows.len())
    );
    check!(
        "B2 修改时间不是 1970 那种假值",
        rows.iter()
            .filter(|(_, m, _)| !m.is_empty())
            .all(|(_, m, _)| !m.starts_with("1970")),
        format!("有行显示成了 1970：{rows:?}")
    );

    // ================= C/D：造一个符号链接 ⇒ 看指向 + 双击跟随 =================
    //
    // ⚠️ **自己造、自己删**（`pp_probe_*` 前缀）⇒ 不碰容器里用户的东西。
    let probe_dir = format!("/tmp/pp_probe_link_{}", std::process::id());
    let probe = format!("{probe_dir}/link");
    let target = "/etc"; // 容器里几乎总有 `/etc` ⇒ 目标是**目录**
    let made = docker_exec(&name, &["mkdir", "-p", &probe_dir]).is_some()
        && docker_exec(&name, &["ln", "-sfn", target, &probe]).is_some();
    if !made {
        println!("[信息] 容器里造不出探针链接 ⇒ 跳过 C/D 段（不影响其它断言）");
        let fails = FAILS.with(|f| f.borrow().clone());
        if fails.is_empty() {
            println!("\ndocker files meta smoke ok");
        } else {
            println!("\n{} 条没通过：", fails.len());
            for f in &fails {
                println!("  - {f}");
            }
            std::process::exit(1);
        }
        return;
    }

    // 进那个目录
    dockerui::files_goto_for_test(&panel.st, &probe_dir);
    wait_until(20_000, || {
        dockerui::files_path_for_test(&panel.st) == probe_dir
            && !dockerui::files_rows_for_test(&panel.st).is_empty()
    });
    check!(
        "C0 进到探针目录了",
        dockerui::files_path_for_test(&panel.st) == probe_dir,
        format!(
            "当前目录={:?} 状态={:?}",
            dockerui::files_path_for_test(&panel.st),
            dockerui::files_status_for_test(&panel.st)
        )
    );

    // 找到那一行 ⇒ 「指向」列写着目标
    let rows = dockerui::files_mtime_link_for_test(&panel.st);
    let link_row = rows.iter().find(|(n, _, _)| n == "link").cloned();
    check!(
        "C1 列表里有那个符号链接",
        link_row.is_some(),
        format!("行是 {rows:?}")
    );
    if let Some((_, _, link)) = &link_row {
        check!(
            "C2 「指向」列写着链接的目标",
            link == target,
            format!("「指向」是 {link:?}（期望 {target:?}）")
        );
    }
    // ⚠️ **类型列也要认得出它是链接**（双击跟随靠这个分支）。
    let kinds = dockerui::files_rows_for_test(&panel.st);
    check!(
        "C3 类型列认得出它是「链接」",
        kinds
            .iter()
            .any(|(n, t, _, _)| n == "link" && (t == "链接" || t == "Link")),
        format!("行是 {kinds:?}")
    );

    // ---- D：双击 ⇒ 真的跟过去 ----
    let fired = dockerui::files_activate_row_for_test(&panel.st, "link");
    check!("D1 符号链接那一行能双击", fired, "没找到「link」这一行");
    let followed = wait_until(20_000, || {
        dockerui::files_path_for_test(&panel.st) == target
    });
    check!(
        "D2 双击符号链接**跟过去了**",
        followed,
        format!(
            "当前目录={:?}（期望 {target:?}）状态={:?}",
            dockerui::files_path_for_test(&panel.st),
            dockerui::files_status_for_test(&panel.st)
        )
    );

    // ---- 清理：把探针删掉 ----
    let _ = docker_exec(&name, &["rm", "-rf", &probe_dir]);

    let fails = FAILS.with(|f| f.borrow().clone());
    if fails.is_empty() {
        println!("\ndocker files meta smoke ok");
    } else {
        println!("\n{} 条没通过：", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}
