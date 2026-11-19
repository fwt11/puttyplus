//! **阶段 138** 冒烟：打开 compose 文件 ⇒ **文件面板跳到项目目录 + 选中那个文件**。
//!
//! **背景**（用户 2026-11-19）：
//! > 「点击『编辑compose文件』，如果作为单独的tab 显示，目前显示了sftp,
//! >  sftp最好能跳转到docker-compose.yml文件所在目录。」
//!
//! **断言**：
//! - **A 组（不连服务器）**：
//!   1. `pane::select_by_name` 按名字选中**那一行**（只选它）；
//!   2. 选一个**不存在**的文件 ⇒ 返回 false、**不动**已有选区；
//!   3. **没连上时** `panel::goto_remote_dir` 是**安全的空操作**
//!      （不炸、不改 cwd、不留下「待选中」的脏状态）—— 这一条钉住
//!      「点一下『编辑 compose 文件』不会莫名弹一个『读取远程目录失败』」。
//! - **B 组（连本机测试 sshd）**：真的跳目录 ⇒ 面板列的是那个目录、
//!   那个文件被**选中**。
//!
//! **运行前提**：B 组要本机测试 sshd 在 127.0.0.1:2233（公钥认证）。
//!
//! ```text
//! xvfb-run -a cargo run --example compose_jump_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel, dockerui, pane};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 造一行文件（远端目录里的一个普通文件）。
fn row(name: &str) -> pane::RowData {
    pane::RowData {
        name: name.to_string(),
        size: 128,
        is_dir: false,
        perm: "-rw-r--r--".into(),
        mtime: 0,
        hidden: false,
    }
}

fn main() {
    // ⚠️ **别碰用户的真实配置**。
    let settings_ini = std::env::temp_dir().join(format!("pp-compose-jump-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ================= A 组：不连服务器 =================
    let panel = SftpPanel::new(&ConnOptions::default());
    pump(150);
    let p = panel.st.pane(true);
    pane::fill_model(
        &p,
        false,
        vec![
            row("docker-compose.yml"),
            row(".env"),
            row("README.md"),
        ],
    );
    pump(60);

    // ---- ① 按名字选中那一行 ----
    check!(
        "① select_by_name 能选中指定的那一行",
        pane::select_by_name(&p, "docker-compose.yml"),
        "选中失败了（文件明明在列表里）"
    );
    let sel = p.selected("/srv/app");
    check!(
        "① 选中的**就是** docker-compose.yml（只选它）",
        sel.len() == 1 && sel[0].0.ends_with("docker-compose.yml"),
        format!("选中了：{sel:?}")
    );

    // ---- ② 选不存在的文件 ⇒ 不动已有选区 ----
    check!(
        "② 选一个不存在的文件 ⇒ 返回 false",
        !pane::select_by_name(&p, "不存在.yml"),
        "竟然报告成功"
    );
    let sel2 = p.selected("/srv/app");
    check!(
        "② 失败时**不动**原来的选区",
        sel2.len() == 1 && sel2[0].0.ends_with("docker-compose.yml"),
        format!("选区被改动了：{sel2:?}")
    );

    // ---- ③ 没连上 ⇒ 安全的空操作 ----
    let cwd_before = panel.st.remote_cwd.borrow().clone();
    check!(
        "③ 没连上时 goto_remote_dir 返回 false（不假装跳了）",
        !panel::goto_remote_dir(&panel.st, "/srv/app", Some("docker-compose.yml")),
        "没连上却报告跳过去了"
    );
    check!(
        "③ 没连上时**不改**当前目录",
        panel.st.remote_cwd.borrow().clone() == cwd_before,
        format!(
            "目录被改了：{} ⇒ {}",
            cwd_before,
            panel.st.remote_cwd.borrow()
        )
    );
    check!(
        "③ 没连上时**不留**「待选中」的脏状态",
        panel.st.pending_select.borrow().is_none(),
        format!("留下了：{:?}", panel.st.pending_select.borrow())
    );

    // ================= B 组：连本机测试 sshd =================
    let home = std::env::var("HOME").unwrap_or_default();
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2233);
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port,
        user: std::env::var("USER").unwrap_or_else(|_| "fwt".into()),
        keyfile: Some(format!("{home}/.ssh/id_rsa")),
        ..Default::default()
    };
    let live = puttyplus::tabs::Tabs::new();
    live.open(&opts);
    let win = gtk::Window::new();
    win.set_title(Some("compose jump smoke"));
    win.set_default_size(1200, 800);
    win.set_child(Some(&live.notebook));
    win.present();
    pump(600);
    let Some(st) = live.current_panel_state() else {
        println!("[FAIL] 没有当前面板");
        std::process::exit(101);
    };
    let mut connected = false;
    for _ in 0..120 {
        pump(50);
        if st.conn_ok.get() {
            connected = true;
            break;
        }
    }
    if !connected {
        // ⚠️ **本机测试 sshd 没起** ⇒ B 组没法跑 ⇒ 明说，不假装通过。
        println!("[跳过] B 组：本机测试 sshd（127.0.0.1:{port}）没起来 ⇒ 没法验「真的跳目录」");
    } else {
        let sub = st.sub_tabs().expect("面板该有子标签容器");
        // 一个「compose 项目目录」：docker-compose.yml + .env。
        let dir = std::env::temp_dir().join(format!("pp_compose_jump_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("docker-compose.yml"), "services: {}\n").expect("写 compose 文件");
        std::fs::write(dir.join(".env"), "TAG=dev\n").expect("写 .env");
        let dir_s = dir.to_string_lossy().into_owned();

        // 先让面板落到别的目录 ⇒ 后面那个「跳」才是真的跳。
        let other = std::env::temp_dir().join(format!("pp_compose_other_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&other);
        let other_s = other.to_string_lossy().into_owned();
        panel::goto_remote_dir(&st, &other_s, None);
        for _ in 0..120 {
            pump(50);
            if st.remote_cwd.borrow().as_str() == other_s {
                break;
            }
        }

        // ★ **核心**：走**真路径**（`open_compose_path` = 点「编辑 compose
        //   文件」按钮走的那条）⇒ 面板要跳到项目目录 + 选中那个文件。
        let compose_path = dir.join("docker-compose.yml").to_string_lossy().into_owned();
        let editors_before = sub.editor_count();
        dockerui::open_compose_path(&st, &compose_path, &dir_s);
        let mut landed = false;
        for _ in 0..120 {
            pump(50);
            if st.remote_cwd.borrow().as_str() == dir_s {
                landed = true;
                break;
            }
        }
        check!(
            "★ 面板**列的是** compose 项目目录",
            landed,
            format!("当前目录是 {:?}", st.remote_cwd.borrow())
        );
        // 路径框也要跟着变（用户看的是路径框，不是内部状态）。
        check!(
            "★ 路径框显示的是项目目录",
            st.pane(true).entry.text() == dir_s,
            format!("路径框是 {:?}", st.pane(true).entry.text())
        );
        // 那个文件被选中了 ⇒ 用户一眼看到「编辑的就是这个」。
        let mut picked = false;
        for _ in 0..40 {
            pump(50);
            let sel = st.pane(true).selected(&dir_s);
            if sel.iter().any(|(pth, _)| pth.ends_with("docker-compose.yml")) {
                picked = true;
                break;
            }
        }
        check!(
            "★ compose 文件被**选中**了（一眼看到编辑的是哪个）",
            picked,
            format!(
                "选区是 {:?}",
                st.pane(true).selected(&dir_s)
            )
        );
        // 编辑器**开了**（而且是主栏页签，不是底部编辑区）。
        let mut opened = false;
        for _ in 0..40 {
            pump(50);
            if sub.editor_count() > editors_before {
                opened = true;
                break;
            }
        }
        check!(
            "★ compose 文件在**编辑器**里打开了",
            opened,
            format!(
                "编辑器数还是 {}（打开前 {}）",
                sub.editor_count(),
                editors_before
            )
        );
        // 顺手清理。
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    if fails.is_empty() {
        println!("compose jump smoke ok");
    } else {
        eprintln!("compose jump smoke：{} 条失败", fails.len());
        for f in &fails {
            eprintln!("  - {f}");
        }
        std::process::exit(101);
    }
}
