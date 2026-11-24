//! 容器「文件」页**导航**冒烟（阶段 159）。
//!
//! 用户 2026-11-24：「双击文件夹，路径名会在地址栏累积，也许是因为这个原因
//! 导致进不去文件夹」（截图里地址栏是 `/media/proc/root/sbin/srv/tmp/srv/sbin`，
//! 而列表还是容器根目录）。
//!
//! ## 复现路径（真连本机测试 sshd ＋ 真容器）
//!
//! 1. 容器里到处是**空目录**（`/media`、`/boot`…）⇒ `ls -lA` 在空目录里**只**
//!    输出一行 `total 0` ⇒ 解析出 **0 行**；
//! 2. 以前这里把「0 行」当成**失败** ⇒ 于是
//!    ① **表格停在父目录**（用户以为没进去 ⇒ 又在旧表格里点一行）
//!    ② 而**路径栏照样前进**（乐观更新）⇒ 下一次双击就在失败路径上**再拼一层**
//!    ⇒ 地址栏变成 `/media/proc/root/sbin/srv/tmp/srv/sbin` 这种鬼样子。
//!
//! ## 断言
//!
//! - **A**：进**空目录** ⇒ 表格变成**空**（不是父目录的行）、路径栏 = 该目录；
//! - **B**：进**不存在的目录** ⇒ 当前目录与路径栏**都留在上一个成功的目录**
//!   （不许前进 ⇒ 地址栏不会越拼越长）；
//! - **C**：连着两次失败 ⇒ 路径栏**不许累积**。
//!
//! 运行前提：本机测试 sshd 在 127.0.0.1:2233（公钥认证），且那台机器上
//! **装了 docker**（本机就是 ⇒ 远端 = 本机 ⇒ 跑的是真 docker）。
//! 端口可用 `PP_DOCKER_PORT` 覆盖。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example docker_files_nav_smoke

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

/// 截一张图（**可选**：`PP_DOCKER_SHOT=/path` 时才截）。
///
/// ⚠️ 用 ImageMagick 的 `import` 截整屏（Xvfb 下只有本程序）⇒ 截不到
///   **不算测试失败**（只是没图）。
fn shot(path: &str) {
    match std::process::Command::new("import")
        .args(["-window", "root", path])
        .status()
    {
        Ok(s) if s.success() => println!("[信息] 截图已存：{path}"),
        Ok(s) => println!("[信息] 截图命令退出码 {s}（没截到，不影响测试结果）"),
        Err(e) => println!("[信息] 截图失败（{e}）——不影响测试结果"),
    }
}

/// 文件页**这一轮请求结束了没有**（状态行不再说「正在读取」）。
fn settled(st: &Rc<PanelState>) -> bool {
    !dockerui::files_status_for_test(st).contains("正在读取")
}

/// 在容器里找一个**空目录**（**只读**，不往容器里写东西）。
///
/// ⚠️ 测试 sshd 就在本机 ⇒ 远端 = 本机 ⇒ 本机的 docker CLI 看到的是同一个容器。
/// ⚠️ 找不到就返回 None ⇒ 冒烟里那一段跳过（不影响其它断言）。
fn find_empty_dir(container: &str) -> Option<String> {
    let out = std::process::Command::new("docker")
        .args(["exec", container, "ls", "-A", "--", "/"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for name in text.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let path = format!("/{name}");
        // 是目录吗？（`ls -ld` 的第一个字符是类型位）
        let ld = std::process::Command::new("docker")
            .args(["exec", container, "ls", "-ld", "--", &path])
            .output()
            .ok()?;
        if !ld.status.success() || !String::from_utf8_lossy(&ld.stdout).starts_with('d') {
            continue;
        }
        // 空吗？（`ls -A` 没输出 = 空）
        let inner = std::process::Command::new("docker")
            .args(["exec", container, "ls", "-A", "--", &path])
            .output()
            .ok()?;
        if inner.status.success() && String::from_utf8_lossy(&inner.stdout).trim().is_empty() {
            return Some(path);
        }
    }
    None
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
    win.set_title(Some("容器文件页导航冒烟"));
    win.set_default_size(1200, 820);
    win.set_child(Some(&page));
    win.present();
    pump(200);
    dockerui::refresh_for_test(&panel.st);

    // ---- 等容器列表真的到（要走 SSH ⇒ 可能要几秒）----
    check!(
        "容器列表到了",
        wait_until(20_000, || !dockerui::containers_for_test(&panel.st).is_empty()),
        "等了 20 秒还是没列表（测试 sshd 没连上？）"
    );
    // ⚠️ **要先选中一个容器**（文件页看的是「选中容器的目录」）。
    //   `set_cursor` 对**看不见的行**（收着的 compose 项目里的子行）不生效
    //   ⇒ 先把所有项目展开。
    let cs = dockerui::containers_for_test(&panel.st);
    let name = cs
        .iter()
        .find(|c| c.state == "running")
        .or_else(|| cs.first())
        .map(|c| c.name.clone())
        .unwrap_or_default();
    check!("有容器可选", !name.is_empty(), "容器列表是空的");
    if name.is_empty() {
        println!("\n没有容器 ⇒ 没法验文件页的导航");
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
    check!(
        "选中的容器对了",
        dockerui::selected_for_test(&panel.st) == name,
        format!("选中的其实是 {:?}", dockerui::selected_for_test(&panel.st))
    );
    println!("[信息] 选中容器：{name}");

    // ---- 切到「文件」页 ⇒ 列根目录 ----
    dockerui::files_show_for_test(&panel.st);
    // ⚠️ **等「路径是根」且「行真的出来了」**：只等路径的话，
    //   旧行为（路径乐观更新）会**立刻**满足 ⇒ 读到还没填的空表。
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
    let root_rows = dockerui::files_rows_for_test(&panel.st);
    println!("[信息] 容器根目录：{} 个条目", root_rows.len());

    // ================= A：进空目录 =================
    //
    // ⚠️ 以前这里**失败**（`ls -lA` 只输出 `total 0` ⇒ 0 行 ⇒ 当成错误）
    //   ⇒ 表格停在父目录 ⇒ 用户以为没进去。
    let empty_dir = find_empty_dir(&name);
    match empty_dir {
        Some(dir) => {
            println!("[信息] 容器里的空目录：{dir}");
            dockerui::files_goto_for_test(&panel.st, &dir);
            wait_until(20_000, || dockerui::files_path_for_test(&panel.st) == dir);
            check!(
                "A1 进空目录后当前目录过去了",
                dockerui::files_path_for_test(&panel.st) == dir,
                format!(
                    "当前目录还是 {:?}（状态={:?}）",
                    dockerui::files_path_for_test(&panel.st),
                    dockerui::files_status_for_test(&panel.st)
                )
            );
            check!(
                "A2 进空目录后路径栏过去了",
                dockerui::files_entry_for_test(&panel.st) == dir,
                format!("路径栏是 {:?}", dockerui::files_entry_for_test(&panel.st))
            );
            let rows = dockerui::files_rows_for_test(&panel.st);
            // ⚠️ **阶段 162：表格里现在有一行「..」**（回到上一级）
            //   ⇒ 空目录 ⇒ **只有**「..」这一行，**没有**父目录的条目。
            check!(
                "A3 空目录的表格里没有父目录的条目（只有「..」）",
                rows.len() == 1 && rows[0].0 == "..",
                format!("表格里还有 {} 行：{rows:?}", rows.len())
            );
        }
        None => {
            println!("[信息] 容器里没找到空目录（或本机没有 docker CLI）⇒ 跳过 A 段");
        }
    }

    // ================= B：进不存在的目录 ⇒ 路径栏不许前进 =================
    let good = dockerui::files_path_for_test(&panel.st);
    check!("B0 有「上一个成功的目录」", !good.is_empty(), "当前目录是空的");
    dockerui::files_goto_for_test(&panel.st, "/pp_no_such_dir_zzz");
    wait_until(20_000, || settled(&panel.st));
    check!(
        "B1 进不存在的目录 ⇒ 当前目录没变",
        dockerui::files_path_for_test(&panel.st) == good,
        format!(
            "当前目录变成了 {:?}（状态={:?}）",
            dockerui::files_path_for_test(&panel.st),
            dockerui::files_status_for_test(&panel.st)
        )
    );
    check!(
        "B2 进不存在的目录 ⇒ 路径栏没变（否则地址栏会越拼越长）",
        dockerui::files_entry_for_test(&panel.st) == good,
        format!("路径栏是 {:?}", dockerui::files_entry_for_test(&panel.st))
    );

    // ================= C：连着两次失败 ⇒ 不许累积 =================
    dockerui::files_goto_for_test(&panel.st, "/pp_no_such_dir_yyy");
    wait_until(20_000, || settled(&panel.st));
    check!(
        "C1 连着两次失败 ⇒ 路径栏没累积",
        dockerui::files_entry_for_test(&panel.st) == good,
        format!("路径栏是 {:?}", dockerui::files_entry_for_test(&panel.st))
    );
    // ⚠️ **路径栏与当前目录必须永远是同一个目录** ⇒ 这是「不累积」的根本保证。
    check!(
        "C2 路径栏与当前目录是同一个目录",
        dockerui::files_entry_for_test(&panel.st) == dockerui::files_path_for_test(&panel.st),
        format!(
            "路径栏={:?} 当前目录={:?}",
            dockerui::files_entry_for_test(&panel.st),
            dockerui::files_path_for_test(&panel.st)
        )
    );

    // ================= D：「..」能回到上一级（阶段 162）=================
    //
    // ⚠️ **用户 2026-11-24**：「容器的文件管理，文件夹中没有 ".."，进入
    //   文件夹就没法出来了」⇒ 文件管理器都有这一行 ⇒ 双击它就往上走。
    {
        // 根目录 ⇒ **没有**「..」（`/` 的上一级还是 `/`）。
        dockerui::files_goto_for_test(&panel.st, "/");
        wait_until(20_000, || {
            dockerui::files_path_for_test(&panel.st) == "/"
                && !dockerui::files_rows_for_test(&panel.st).is_empty()
        });
        let root_rows = dockerui::files_rows_for_test(&panel.st);
        check!(
            "D1 根目录里没有「..」",
            !root_rows.iter().any(|r| r.0 == ".."),
            format!("根目录里有「..」：{root_rows:?}")
        );

        // 进一个子目录 ⇒ **第一行就是「..」**。
        let sub = root_rows
            .iter()
            .find(|r| r.1 == "目录" || r.1 == "Dir" || r.1 == "directory")
            .map(|r| r.0.clone())
            .unwrap_or_default();
        if sub.is_empty() {
            println!("[信息] 容器根目录里没找到子目录 ⇒ 跳过 D 段");
        } else {
            let target = format!("/{sub}");
            dockerui::files_goto_for_test(&panel.st, &target);
            wait_until(20_000, || {
                dockerui::files_path_for_test(&panel.st) == target
                    && !dockerui::files_rows_for_test(&panel.st).is_empty()
            });
            let rows = dockerui::files_rows_for_test(&panel.st);
            check!(
                "D2 子目录的第一行是「..」",
                rows.first().map(|r| r.0.as_str()) == Some(".."),
                format!("子目录 {target} 的第一行是 {:?}", rows.first())
            );
            // ⚠️ **截图看看**（`PP_DOCKER_SHOT=/tmp/x` 时才截 ⇒ 不影响测试）。
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-files-up.png"));
            }
            // ⚠️ **双击「..」⇒ 真的回到上一级**（走真的 row-activated）。
            let fired = dockerui::files_activate_row_for_test(&panel.st, "..");
            check!("D3 「..」这一行能双击", fired, "没找到「..」这一行");
            wait_until(20_000, || dockerui::files_path_for_test(&panel.st) == "/");
            check!(
                "D4 双击「..」真的回到上一级了",
                dockerui::files_path_for_test(&panel.st) == "/",
                format!(
                    "当前目录还是 {:?}（状态={:?}）",
                    dockerui::files_path_for_test(&panel.st),
                    dockerui::files_status_for_test(&panel.st)
                )
            );
        }
    }

    let fails = FAILS.with(|f| f.borrow().clone());
    if fails.is_empty() {
        println!("\ndocker files nav smoke ok");
    } else {
        println!("\n{} 条没通过：", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}
