//! 阶段 109 冒烟：**容器列表（只读）**——树形结构 + 共享连接。
//!
//! **背景**（用户 2026-11-15）：
//! - 「我们要在软件里面显示远程服务器上的容器运行情况」；
//! - 「容器列表要中要体现单独的容器和使用 docker-compose 组织的容器，
//!   **每个 docker-compose 项目点开之后才显示它组织的容器**」。
//!
//! **断言**（分两组）：
//!
//! **A 组：后端 + 共享连接**（真连本机测试 sshd 的 2233 端口）：
//! 1. 共享连接上能取到容器列表（`docker ps -a` 真的跑起来了）；
//! 2. 取到的容器里**分得出两种**：带 compose 项目标签的 + 不带的（独立容器）；
//! 3. **不新增连接**：容器视图用的就是**那条共享连接**（`conn_count` 不变）。
//!
//! **B 组：界面树形**（不连服务器，直接往模型里灌假容器）：
//! 4. **compose 项目是父行**、独立容器是顶层行；
//! 5. 项目行**默认收起**（点开才显示它组织的容器）——这是用户那句话的核心；
//! 6. 点开（展开）项目行 ⇒ 它带的容器**才出现**；
//! 7. **刷新后展开状态保留**（用户刚点开的项目不能一刷新就合上）。
//!
//! 运行前提：本机测试 sshd 在 127.0.0.1:2233（公钥认证），且那台机器上
//! **装了 docker**（本机就是 ⇒ 远端 = 本机 ⇒ 跑的是真 docker）。
//! 端口可用 `PP_DOCKER_PORT` 覆盖。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example docker_ui_smoke

#![allow(deprecated)] // 容器列表沿用 TreeView/TreeStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::docker::{self, ContainerDetail, MountInfo, NetworkInfo, PortInfo};
use puttyplus::backend::hub;
use puttyplus::backend::shell::ShellEvent;
use puttyplus::panel::{SftpPanel, dockerui, subtabs};

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

/// **屏幕上真正显示着**的行（按上到下）。
///
/// ⚠️ 这是「用户眼前看到什么」的答案 —— 跟「模型里有什么」**不是一回事**：
///    收起的项目，它的子行在模型里存在，但**屏幕上不占地方**。
///
/// 做法：逐行问 GTK「这个坐标上是哪一行」（`path_at_pos`）。
fn displayed_rows(view: &gtk::TreeView) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Some(model) = view.model() else {
        return out;
    };
    let h = view.height();
    let mut y = 0i32;
    while y < h {
        if let Some((Some(path), _col, _cx, _cy)) = view.path_at_pos(8, y) {
            if let Some(it) = model.iter(&path) {
                let name: String = model.get(&it, 0);
                if out.last() != Some(&name) {
                    out.push(name);
                }
            }
        }
        y += 6;
    }
    out
}

/// 灌一批假容器（**不连服务器**）——用来验界面形状。
fn fake_containers() -> Vec<puttyplus::backend::docker::ContainerInfo> {
    let mk = |name: &str, compose: &str, running: bool| puttyplus::backend::docker::ContainerInfo {
        id: format!("{name}-id"),
        name: name.to_string(),
        image: "img:1".to_string(),
        status: if running { "Up 2 hours".into() } else { "Exited (0) 1 hour ago".into() },
        state: if running { "running".into() } else { "exited".into() },
        ports: String::new(),
        compose: compose.to_string(),
        // ⚠️ compose 项目的容器**都带**这两个标签（阶段 111：靠它们找
        //    compose 文件与工作目录）。
        compose_files: if compose.is_empty() {
            String::new()
        } else {
            "/srv/app/docker-compose.yml".to_string()
        },
        compose_dir: if compose.is_empty() {
            String::new()
        } else {
            "/srv/app".to_string()
        },
        cpu: "1.0%".to_string(),
        mem: "10MiB".to_string(),
    };
    vec![
        mk("solo-a", "", true),   // 独立容器（顶层行）
        mk("web", "proj", true),  // compose 项目 proj 的第 1 个
        mk("solo-b", "", false),  // 另一个独立容器
        mk("db", "proj", true),   // compose 项目 proj 的第 2 个
    ]
}

/// 截一张图（**可选**：`PP_DOCKER_SHOT=/path` 时才截）。
///
/// ⚠️ 用 ImageMagick 的 `import` 截整屏（Xvfb 下只有本程序）⇒
///    不用自己折腾 GTK 离屏渲染。截不到**不算测试失败**（只是没图）。
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


/// 当前**顶层窗口**的个数（用来验「点动作按钮 ⇒ 弹出确认框」）。
fn toplevel_count() -> u32 {
    use gtk::gio::prelude::ListModelExt;
    gtk::Window::toplevels().n_items()
}

/// 关掉**最后一个**顶层窗口（冒烟里用来「取消」确认框）。
///
/// ⚠️ 关掉 ⇒ 对话框给的响应**不是「确定」** ⇒ 动作**不会执行**。
fn close_last_toplevel() {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    let n = model.n_items();
    for i in (0..n).rev() {
        if let Some(obj) = model.item(i) {
            if let Ok(w) = obj.downcast::<gtk::Window>() {
                w.close();
                return;
            }
        }

    }
}

/// 点**最后一个**对话框的「确定」按钮（冒烟里用来「确认」）。
///
/// ⚠️ 找不到就什么都不做（别乱点别的窗口）。
fn confirm_last_dialog() {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    let n = model.n_items();
    for i in (0..n).rev() {
        let Some(obj) = model.item(i) else { continue };
        let Ok(w) = obj.downcast::<gtk::Window>() else { continue };
        if let Some(btn) = find_ok_button(w.upcast_ref::<gtk::Widget>()) {
            btn.emit_clicked();
            return;
        }
    }
}

/// 树里那个**不是「取消」**的按钮（确认框的「确定」）。
fn find_ok_button(w: &gtk::Widget) -> Option<gtk::Button> {
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(b) = x.downcast_ref::<gtk::Button>() {
            if b.label().unwrap_or_default().to_string() != "取消" {
                return Some(b.clone());
            }
        }
        if let Some(b) = find_ok_button(&x) {
            return Some(b);
        }
        c = x.next_sibling();
    }
    None
}

/// 远端还有没有**某个进程**（判据用）。
///
/// ⚠️ `pgrep -af '[d]ocker logs'` 里的 `[d]` 是老技巧：命令自己不会匹配自己
///    （不然 pgrep 那行命令自己也算一个 ⇒ 永远非空）。
fn remote_procs(opts: &ConnOptions, what: &str) -> String {
    let pat = format!("[{}]{}", &what[..1], &what[1..]);
    let keyfile = opts.keyfile.clone().unwrap_or_default();
    let user = opts.user.clone();
    let host = opts.host.clone();
    let port = opts.port;
    hub::runtime().block_on(async move {
        let creds = hub::Creds {
            keyfile: Some(keyfile),
            ..Default::default()
        };
        let key = hub::ServerKey::new(&host, port, &user);
        let Ok(shared) = hub::acquire(key, &creds).await else {
            return String::new();
        };
        let (_, out) = docker::run_cmd(
            shared.session().as_ref(),
            &format!("pgrep -af '{pat}' || true"),
        )
        .await
        .unwrap_or((0, String::new()));
        out.trim().to_string()
    })
}

/// 远端还有没有 `docker logs` 进程（**看门狗**的判据）。
fn remote_log_procs(opts: &ConnOptions) -> String {
    remote_procs(opts, "docker logs")
}

/// 远端 `docker images` 的第一行（**独立对照**用）。
///
/// ⚠️ **为什么要自己再跑一遍**：界面上的镜像名如果**解析错位**了，光看
///   「有行、字段非空」**抓不住**（都非空）⇒ 得拿**远端原文**对一遍。
/// ⚠️ **命令自己写**（不复用 `docker::images_cmd`）⇒ 否则就是「程序跟
///   程序自己比」＝同义反复。
fn remote_images_line(opts: &ConnOptions) -> String {
    let keyfile = opts.keyfile.clone().unwrap_or_default();
    let user = opts.user.clone();
    let host = opts.host.clone();
    let port = opts.port;
    hub::runtime().block_on(async move {
        let creds = hub::Creds {
            keyfile: Some(keyfile),
            ..Default::default()
        };
        let key = hub::ServerKey::new(&host, port, &user);
        let Ok(shared) = hub::acquire(key, &creds).await else {
            return String::new();
        };
        let cmd = "docker images --format '{{.Repository}}|{{.Tag}}' 2>&1";
        let (_, out) = docker::run_cmd(shared.session().as_ref(), cmd)
            .await
            .unwrap_or((0, String::new()));
        out.lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    })
}

fn main() {
    // ⚠️ **别碰用户的真实配置**。
    let settings_ini = std::env::temp_dir().join(format!("pp-docker-smoke-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ⚠️ 用**线程局部**存失败清单：后台块是 `async move` ⇒ 抓到外层的
    //    `Rc`/`&mut` 都会被 move 走（编译不过）⇒ 改成 `thread_local!`
    //    （后台块跟主流程在**同一个线程**上跑 ⇒ 共用得到）。
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

    // ================= A 组：后端 + 共享连接 =================
    let port: i32 = std::env::var("PP_DOCKER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("USER").unwrap_or_else(|_| "fwt".into());
    let keyfile = std::env::var("HOME")
        .map(|h| format!("{h}/.ssh/id_rsa"))
        .unwrap_or_default();
    // ⚠️ A 组把 `keyfile` **move** 进异步块了 ⇒ F 组要另留一份。
    let keyfile_f = keyfile.clone();

    // ⚠️ **必须用全局运行时**：连接中枢在那儿（russh 的 Handle 绑 runtime）。
    let user_bg = user.clone();
    hub::runtime().block_on(async move {
        let creds = hub::Creds {
            keyfile: Some(keyfile),
            ..Default::default()
        };
        let key = hub::ServerKey::new(HOST, port, &user_bg);
        let shared = hub::acquire(key, &creds).await.expect("应当能连上测试 sshd");
        let conns_before = hub::conn_count();

        // ① 版本探路 + 容器列表
        let session = shared.session().clone();
        let version = match docker::probe(&session).await {
            Ok(v) => v,
            Err(e) => {
                println!("[FAIL] 取不到远程 Docker 版本：{}", e.0);
                FAILS.with(|f| f.borrow_mut().push(format!("取不到远程 Docker 版本：{}", e.0)));
                String::new()
            }
        };
        let containers = docker::list(&session).await.expect("应当能取到容器列表");

        check!(
            "① 远程 Docker 探到了版本",
            !version.is_empty(),
            "版本是空的"
        );
        check!(
            "① 容器列表取到了东西",
            !containers.is_empty(),
            "容器列表是空的（远端那台机器上可能没有容器）"
        );

        // ② compose 项目与独立容器**分得出来**
        let n_compose = containers.iter().filter(|c| !c.compose.is_empty()).count();
        let n_solo = containers.iter().filter(|c| c.compose.is_empty()).count();
        println!(
            "[信息] 远程 Docker {version}：共 {} 个容器（compose 项目 {} 个 / 独立 {} 个）",
            containers.len(),
            n_compose,
            n_solo
        );
        check!(
            "② 容器带 compose 项目标签",
            n_compose > 0,
            format!("没有任何容器带 compose 项目标签（共 {} 个）", containers.len())
        );

        // ② 之二：**详情**（真后端）——拿第一个容器的详情。
        if let Some(c0) = containers.first() {
            match docker::fetch_detail(&session, &c0.name).await {
                Ok(d) => {
                    check!(
                        "② 取到容器详情",
                        !d.name.is_empty(),
                        "详情里连名字都没有"
                    );
                    check!(
                        "② 详情里有镜像",
                        !d.image.is_empty(),
                        "详情里镜像是空的"
                    );
                    println!(
                        "[信息] 详情：{} / 镜像 {} / 环境变量 {} 个 / 挂载 {} 个 / 端口 {} 个",
                        d.name,
                        d.image,
                        d.env.len(),
                        d.mounts.len(),
                        d.ports.len()
                    );
                }
                Err(e) => {
                    println!("[FAIL] 取不到容器详情：{}", e.0);
                    FAILS.with(|f| f.borrow_mut().push(format!("取不到容器详情：{}", e.0)));
                }
            }
        } else {
            println!("[信息] 没有容器可查详情（跳过）");
        }

        // ② 之四：**退出码**要真的传回来（2026-11-16 修的真 bug）。
        //
        // ⚠️ **为什么要有这条**：服务端发包顺序是 **EOF → exit-status →
        //   close**（exit-status 排在 EOF **后面**）⇒ 以前「一收到 EOF 就
        //   收尾」⇒ **退出码永远是 0** ⇒ `run_cmd` 的 `code != 0`
        //   从来不成立 ⇒ 「挨个试哪个 shell 能用」根本没试。
        let (ecode, eout) = docker::run_cmd(&session, "sh -c 'exit 7'")
            .await
            .unwrap_or((-1, String::new()));
        check!(
            "② 远程命令的退出码真的传回来了",
            ecode == 7,
            format!("拿到的是 {ecode}（输出 {eout:?}）")
        );

        // ② 之三：**日志流**（真后端）——`docker logs -f` 的独立通道。
        //
        // ⚠️ 这条验的是**整条链**：命令拼装 → 远端执行 → 增量收数据。
        // ⚠️ 只等一小会儿：`docker logs -f` 是长跑的 ⇒ 不会自己结束。
        let log_target = containers
            .iter()
            .find(|c| c.state == "running")
            .map(|c| c.name.clone());
        match log_target {
            Some(name) => {
                let ch = docker::open_logs(&session, &name, 5)
                    .await
                    .expect("应当能开日志通道");
                let mut buf = String::new();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while std::time::Instant::now() < deadline && buf.is_empty() {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        ch.next_event(),
                    )
                    .await
                    {
                        Ok(Some(ShellEvent::Data(b))) => {
                            buf.push_str(&String::from_utf8_lossy(&b));
                        }
                        Ok(Some(ShellEvent::Exit(_))) | Ok(None) => break,
                        // 超时 ⇒ 还没数据 ⇒ 接着等。
                        Err(_) => continue,
                    }
                }
                println!("[信息] 日志流收到 {} 字节：{buf}", buf.len());
                check!(
                    "② 日志流真的来数据了",
                    !buf.trim().is_empty(),
                    "等了 10 秒也没收到日志（容器可能真的没日志）"
                );
                // ⚠️ **关通道 ⇒ 远端不许留进程**（这是「看门狗」的用处）。
                //
                // ⚠️ 实测过：不带看门狗的话，`docker logs -f` 在通道断开后
                //    **还会一直挂着**（容器安静时它不写数据 ⇒ 收不到 SIGPIPE）
                //    ⇒ 用户每开关一次日志就在服务器上漏一个进程。
                ch.close();
                // 等看门狗收尾（它要等通道的 stdin 变成 EOF 才动手）。
                let mut waited = 0;
                let mut leftover = String::new();
                while waited < 20 {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    waited += 1;
                    let (_, out) = docker::run_cmd(
                        &session,
                        // ⚠️ `[d]ocker` 是 pgrep 的老技巧：命令自己不会匹配自己。
                        "pgrep -af '[d]ocker logs' || true",
                    )
                    .await
                    .unwrap_or((0, String::new()));
                    leftover = out.trim().to_string();
                    if leftover.is_empty() {
                        break;
                    }
                }
                check!(
                    "② 关掉通道后远端没有残留的 docker logs 进程",
                    leftover.is_empty(),
                    format!("远端还挂着：{leftover}")
                );
            }
            None => {
                println!("[信息] 没有运行中的容器（跳过日志流）");
            }
        }

        // ③ **不新增连接**：容器视图用的就是这条共享连接
        let conns_after = hub::conn_count();
        check!(
            "③ 容器视图没有新增 SSH 连接",
            conns_after == conns_before,
            format!("连接数 {conns_before} → {conns_after}")
        );
        drop(shared);
    });

    // ================= B 组：界面树形 =================
    {
        let opts = ConnOptions {
            proto: "ssh".into(),
            host: HOST.into(),
            port,
            user: user.clone(),
            ..Default::default()
        };
        let panel = SftpPanel::new(&opts);
        *panel.st.sub_tabs.borrow_mut() = Some(subtabs::SubTabs::new());
        pump(100);
        let sub = panel.st.sub_tabs().expect("子标签容器");
        let pages_before = sub.page_count();

        // 只建控件（不刷新）⇒ **页签还不该在**（跟数据库工作台一个规矩：
        // 数据没到手之前不显示页面）。
        dockerui::ensure(&panel.st);
        pump(150);
        check!(
            "④ 只建控件时没有「容器」页",
            sub.page_count() == pages_before,
            format!("页签数 {} → {}", pages_before, sub.page_count())
        );

        // 灌假容器 ⇒ 表格按树形重建（**不连服务器**）。
        let rows = fake_containers();
        dockerui::set_rows_for_test(&panel.st, rows.clone());
        pump(150);

        let m = dockerui::model_rows_for_test(&panel.st);
        println!("[信息] 模型行（名字, 是项目行, 是顶层行）：{m:?}");

        // ④ compose 项目是父行，独立容器是顶层行
        let projects: Vec<&String> = m.iter().filter(|(_, p, _)| *p).map(|(n, _, _)| n).collect();
        let tops: Vec<&String> = m.iter().filter(|(_, _, t)| *t).map(|(n, _, _)| n).collect();
        let kids: Vec<&String> = m.iter().filter(|(_, _, t)| !*t).map(|(n, _, _)| n).collect();
        check!(
            "④ compose 项目是父行（proj）",
            projects.iter().any(|n| n.as_str() == "proj"),
            format!("父行里没有 proj：{projects:?}")
        );
        check!(
            "④ 独立容器是顶层行（solo-a / solo-b）",
            tops.iter().any(|n| n.as_str() == "solo-a")
                && tops.iter().any(|n| n.as_str() == "solo-b"),
            format!("顶层行里没有独立容器：{tops:?}")
        );
        check!(
            "④ compose 的容器**不在顶层**（web / db 是子行）",
            kids.iter().any(|n| n.as_str() == "web")
                && kids.iter().any(|n| n.as_str() == "db")
                && !tops.iter().any(|n| n.as_str() == "web" || n.as_str() == "db"),
            format!("web/db 的层级不对（顶层 {tops:?} / 子行 {kids:?}）")
        );

        // ⑤ 项目行**默认收起**
        check!(
            "⑤ compose 项目默认收起",
            !dockerui::project_expanded_for_test(&panel.st, "proj"),
            "proj 一建出来就是展开的（用户要求：点开之后才显示它组织的容器）"
        );

        // ---- 屏幕层面：收起时**看不到**它组织的容器 ----
        //
        // ⚠️ 这一段是用户那句话的**真正落点**：模型里有 ≠ 屏幕上有。
        //    表格得真的挂到窗口上、真的画出来，才能问 GTK「哪一行在哪儿」。
        let win = gtk::Window::new();
        win.set_title(Some("docker ui smoke"));
        win.set_default_size(900, 500);
        let view = dockerui::view_for_test(&panel.st).expect("应当有表格");
        // ⚠️ 挂**整页**（不是表格本身）：表格已经在滚动容器里了 ⇒
        //    再 `set_child` 到窗口上 GTK 会报「已有父控件」。
        let page = dockerui::page_for_test(&panel.st).expect("应当有容器页");
        win.set_child(Some(&page));
        win.present();
        pump(400);

        let shown0 = displayed_rows(&view);
        println!("[信息] 收起时屏幕上显示的行：{shown0:?}");
        // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截两张（收起 / 展开）。
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-collapsed.png"));
        }
        check!(
            "⑤ 收起时屏幕上**看不到** proj 的容器（web / db）",
            !shown0.iter().any(|n| n == "web" || n == "db"),
            format!("收起时屏幕上就有 web/db 了：{shown0:?}")
        );
        check!(
            "⑤ 收起时屏幕上能看到 proj 这个项目行",
            shown0.iter().any(|n| n == "proj"),
            format!("屏幕上没有 proj 行：{shown0:?}")
        );
        check!(
            "⑤ 收起时独立容器照常显示（solo-a）",
            shown0.iter().any(|n| n == "solo-a"),
            format!("屏幕上没有 solo-a：{shown0:?}")
        );

        // ---- 屏幕层面：**字真的画出来了** ----
        //
        // ⚠️ 这一条是**真踩过的坑**：GTK4 的 `pack_start` 只把渲染器塞进列，
        //    不显式 `add_attribute("text", …)` ⇒ **表格一片空白**（列头还在，
        //    看着像「查出来是空的」）⇒ 而模型里明明有数据、`path_at_pos` 也
        //    照样能问出行 ⇒ **光看模型/路径查不出这个毛病**。
        //
        // ⇒ 判据：**画完之后**名称列渲染器的 "text" 属性里有值 ⇒ 字真画出来了。
        //
        // ⚠️ 属性用 `Option<String>` 读：没绑定时它是 **NULL** ⇒ 直接读
        //    `String` 会 panic（那样就不是「断言变红」而是「程序崩了」）。
        let drawn = dockerui::name_renderer_for_test(&panel.st)
            .and_then(|r| r.property::<Option<String>>("text"))
            .unwrap_or_default();
        println!("[信息] 名称列渲染器最后画出来的文字：{drawn:?}");
        check!(
            "⑤ 表格真的把文字画出来了（不是一片空白）",
            !drawn.is_empty() && shown0.iter().any(|n| n == &drawn),
            format!("名称列渲染器里没有字（画出来是空白）：{drawn:?}")
        );

        // ⑥ 点开 ⇒ 它带的容器**才出现**
        check!(
            "⑥ 能展开 compose 项目",
            dockerui::expand_project_for_test(&panel.st, "proj"),
            "展开 proj 失败"
        );
        pump(300);
        check!(
            "⑥ 展开后 proj 是展开状态",
            dockerui::project_expanded_for_test(&panel.st, "proj"),
            "展开后状态还是合着的"
        );
        let shown1 = displayed_rows(&view);
        println!("[信息] 展开后屏幕上显示的行：{shown1:?}");
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-expanded.png"));
        }
        check!(
            "⑥ 展开后屏幕上能看到它组织的容器（web / db）",
            shown1.iter().any(|n| n == "web") && shown1.iter().any(|n| n == "db"),
            format!("展开后屏幕上没看到 web/db：{shown1:?}")
        );

        // ⑦ **刷新后展开状态保留**
        dockerui::set_rows_for_test(&panel.st, rows.clone());
        pump(300);
        check!(
            "⑦ 刷新后展开状态保留",
            dockerui::project_expanded_for_test(&panel.st, "proj"),
            "刷新一下，用户刚点开的项目就合上了"
        );
        let shown2 = displayed_rows(&view);
        check!(
            "⑦ 刷新后屏幕上仍然看得到 web / db",
            shown2.iter().any(|n| n == "web") && shown2.iter().any(|n| n == "db"),
            format!("刷新后 web/db 消失了：{shown2:?}")
        );

        // 挂上子标签（数据到手 ⇒ 页面出现）
        //
        // ⚠️ 先把页面从测试窗口上摘下来：真实程序里页面归子标签管，
        //    页面已有父控件时再 `set_parent` 会报 GTK 断言。
        win.set_child(None::<&gtk::Widget>);
        pump(100);
        dockerui::mount(&panel.st);
        pump(200);
        check!(
            "⑧ 容器页挂上了子标签",
            sub.page_count() == pages_before + 1,
            format!("页签数 {} → {}", pages_before, sub.page_count())
        );

        // 菜单入口在（连接菜单里有「容器…」）
        let menu_has = puttyplus::topmenus::TOP_MENUS.iter().any(|(_, es)| {
            es.iter().any(|e| match e {
                puttyplus::topmenus::MenuEntry::Item { action, .. } => *action == "docker",
                _ => false,
            })
        });
        check!("⑧ 连接菜单里有「容器…」入口", menu_has, "菜单里找不到 action=docker");

        // ⚠️ **图标名必须是系统主题里真有的**。
        //
        // 踩过的坑：先前用 `docker-symbolic`（来自 docker-icon-theme 包）⇒
        // **大多数系统根本没装** ⇒ 菜单和页签上显示成「缺图」⇒ 用户以为
        // 程序坏了。⇒ 断言直接问图标主题「这个图标你认不认识」。
        let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().expect("display"));
        let menu_icon = puttyplus::topmenus::TOP_MENUS
            .iter()
            .flat_map(|(_, es)| es.iter())
            .find_map(|e| match e {
                puttyplus::topmenus::MenuEntry::Item { action, icon, .. } if *action == "docker" => {
                    Some(*icon)
                }
                _ => None,
            });
        check!(
            "⑧ 菜单图标在系统主题里存在",
            menu_icon.map(|i| theme.has_icon(i)).unwrap_or(false),
            format!("菜单图标 {:?} 在系统主题里没有（会显示成缺图）", menu_icon)
        );
        check!(
            "⑧ 页签图标在系统主题里存在",
            theme.has_icon("package-x-generic-symbolic"),
            "package-x-generic-symbolic 不在系统主题里（会显示成缺图）"
        );

    // ================= C 组：阶段 110 —— 详情 + 启停（只读验 + 二次确认） =================
    {
        // ⚠️ **把面板挂进一个窗口**（真实程序里面板在主窗口里）⇒
        //    `st.topwin()` 才找得到父窗口 ⇒ 确认框才有父窗口（项目规矩：
        //    对话框必须**必定**挂上父窗口，阶段 67 / 审计 P2-16）。
        let root_win = gtk::Window::new();
        root_win.set_child(Some(panel.widget()));
        root_win.present();
        pump(200);

        // ⚠️ **灌一份假详情**（不连服务器）⇒ 验详情区的形状。
        let d = ContainerDetail {
            name: "web".into(),
            image: "nginx:latest".into(),
            created: "2026-11-10T11:44:26Z".into(),
            status: "running".into(),
            health: "healthy".into(),
            started_at: "2026-11-15T00:59:47Z".into(),
            finished_at: "".into(),
            exit_code: "0".into(),
            restart_count: "2".into(),
            restart_policy: "always".into(),
            restart_max_retry: "0".into(),
            user: "www-data".into(),
            workdir: "/app".into(),
            command: "[nginx -g daemon off;]".into(),
            network_mode: "bridge".into(),
            env: vec!["PATH=/usr/bin".into(), "TZ=UTC".into()],
            mounts: vec![MountInfo {
                kind: "bind".into(),
                source: "/srv/site".into(),
                dest: "/usr/share/nginx/html".into(),
                rw: "false".into(),
            }],
            ports: vec![PortInfo {
                container: "443/tcp".into(),
                host_port: "8443".into(),
                host_ip: "0.0.0.0".into(),
            }],
            networks: vec![NetworkInfo {
                name: "infra".into(),
                ip: "172.19.0.2".into(),
                gateway: "172.19.0.1".into(),
            }],
        };
        dockerui::set_detail_for_test(&panel.st, d);
        pump(200);

        check!(
            "⑨ 详情区显示出来了",
            dockerui::detail_visible_for_test(&panel.st),
            "详情区还是收着的（选中了容器就该显示）"
        );
        let title = dockerui::detail_title_for_test(&panel.st);
        check!(
            "⑨ 标题写着容器名和状态",
            title.contains("web") && title.contains("运行中"),
            format!("标题是 {title:?}")
        );
        let cfg = dockerui::detail_card_for_test(&panel.st, "config");
        check!(
            "⑨ 运行配置卡片有镜像、状态、重启策略",
            cfg.contains("nginx:latest") && cfg.contains("运行中") && cfg.contains("always"),
            format!("运行配置卡片是：\n{cfg}")
        );
        let net = dockerui::detail_card_for_test(&panel.st, "net");
        check!(
            "⑨ 网络与端口卡片有网络和端口",
            net.contains("infra") && net.contains("8443"),
            format!("网络与端口卡片是：\n{net}")
        );
        let mnt = dockerui::detail_card_for_test(&panel.st, "mount");
        check!(
            "⑨ 挂载卡片有源和目标",
            mnt.contains("/srv/site") && mnt.contains("/usr/share/nginx/html"),
            format!("挂载卡片是：\n{mnt}")
        );
        let env = dockerui::detail_card_for_test(&panel.st, "env");
        check!(
            "⑨ 环境变量卡片有内容",
            env.contains("PATH=/usr/bin"),
            format!("环境变量卡片是：\n{env}")
        );
        check!(
            "⑨ 运行中 ⇒ 「启动」点不了、「停止」能点",
            !dockerui::action_sensitive_for_test(&panel.st, "start")
                && dockerui::action_sensitive_for_test(&panel.st, "stop"),
            "按钮可用性跟状态对不上"
        );

        // ⚠️ **刷新不能把选中顶掉**（自动刷新每 7 秒来一次 ⇒ 用户刚选中的
        //    容器要是被「刷新」清掉，详情也就跟着没了）。
        dockerui::set_rows_for_test(&panel.st, fake_containers());
        pump(200);
        check!(
            "⑨ 刷新后选中还在",
            dockerui::selected_for_test(&panel.st) == "web",
            format!(
                "刷新后选中变成了 {:?}",
                dockerui::selected_for_test(&panel.st)
            )
        );
        check!(
            "⑨ 刷新后详情区还开着",
            dockerui::detail_visible_for_test(&panel.st),
            "刷新后详情区收起来了"
        );

        // ⚠️ **二次确认**：写操作（停容器）**必须先问一句**。
        //
        // ⚠️ **安全**：这里点的是**不存在的容器名** ⇒ 就算确认逻辑坏了，
        //    docker 也只会拒绝 ⇒ **碰不到用户机器上的真容器**。
        let before = toplevel_count();
        // ⚠️ 判据用「**最近一次写操作**」，**不用状态行**：状态行会被后台
        //    刷新改掉 ⇒ 拿它判断会误判（刷新先到就当成动作跑了）。
        let action_before = dockerui::last_action_for_test(&panel.st);
        dockerui::click_action_for_test(&panel.st, "stop");
        pump(300);
        let after = toplevel_count();
        check!(
            "⑩ 点「停止」弹出确认框（写操作有二次确认）",
            after > before,
            format!("顶层窗口数 {before} → {after}（没弹出确认框）")
        );
        check!(
            "⑩ 没点确定 ⇒ 动作**没有**执行",
            dockerui::last_action_for_test(&panel.st) == action_before,
            "没确认就执行了！"
        );
        // 关掉确认框（**不是点确定**）⇒ 什么都不该发生。
        close_last_toplevel();
        pump(300);
        check!(
            "⑩ 取消确认框后仍然没有执行",
            dockerui::last_action_for_test(&panel.st) == action_before,
            "取消之后动作还是跑了"
        );
        check!(
            "⑩ 确认框是**挂着父窗口**的（项目规矩）",
            !puttyplus::windows::dialogs_without_parent()
                .iter()
                .any(|t| t.contains("请确认")),
            format!(
                "确认框没有父窗口：{:?}",
                puttyplus::windows::dialogs_without_parent()
            )
        );
        }

        // ================= D 组：阶段 111 —— compose 项目的 5 个操作 =================
        {
            // ⚠️ **把面板挂进一个窗口**（确认框要有父窗口 ⇒ 项目规矩）。
            //
            // ⚠️ 面板**已经有父控件了**（C 组那个窗口）⇒ 直接 set_child
            //    会踩 GTK 的断言（`gtk_window_set_child`）⇒ 先摘下来。
            let pw = panel.widget();
            pw.unparent();
            let root_win = gtk::Window::new();
            // ⚠️ **给窗口起个名**：`import -window root` 抓的是整屏 ⇒ 会把
            //    别的窗口拍进来 ⇒ 改成按标题找窗口、定点截。
            root_win.set_title(Some("docker 项目详情冒烟"));
            root_win.set_default_size(900, 520);
            root_win.set_child(Some(pw));
            root_win.present();
            pump(200);

            dockerui::set_rows_for_test(&panel.st, fake_containers());
            pump(200);

            // ---- 选中 compose 项目行 ⇒ 项目详情 ----
            check!(
                "⑪ 能选中 compose 项目行",
                dockerui::select_project_for_test(&panel.st, "proj"),
                "选不中项目行"
            );
            pump(300);
            check!(
                "⑪ 选中的项目名对",
                dockerui::selected_project_for_test(&panel.st) == "proj",
                format!(
                    "选中的项目是 {:?}",
                    dockerui::selected_project_for_test(&panel.st)
                )
            );
            check!(
                "⑪ 项目详情显示出来了",
                dockerui::project_detail_visible_for_test(&panel.st),
                "项目详情没显示"
            );
            check!(
                "⑪ 容器详情**藏起来**了（两种详情互斥）",
                !dockerui::detail_visible_for_test(&panel.st),
                "容器详情还开着"
            );
            let ptitle = dockerui::project_title_for_test(&panel.st);
            check!(
                "⑪ 项目标题写着项目名",
                ptitle.contains("proj"),
                format!("项目标题是 {ptitle:?}")
            );
            let pinfo = dockerui::project_info_for_test(&panel.st);
            check!(
                "⑪ 项目详情里有 compose 文件路径和工作目录",
                pinfo.contains("docker-compose.yml") && pinfo.contains("/srv/app"),
                format!("项目详情是：\n{pinfo}")
            );
            check!(
                "⑪ 项目详情里列出了它组织的容器",
                pinfo.contains("web") && pinfo.contains("db"),
                format!("项目详情是：\n{pinfo}")
            );
            // ---- 5 个操作按钮 ----
            for which in ["edit", "start", "stop", "down", "up"] {
                check!(
                    format!("⑪ 项目操作按钮「{which}」能点"),
                    dockerui::project_action_sensitive_for_test(&panel.st, which),
                    format!("按钮 {which} 是灰的")
                );
            }

            // ---- 写操作 ⇒ 二次确认 ----
            //
            // ⚠️ **安全**：这里**不点确定** ⇒ 命令不会跑 ⇒ 碰不到远端任何项目。
            let before = toplevel_count();
            let action_before = dockerui::last_action_for_test(&panel.st);
            dockerui::click_project_action_for_test(&panel.st, "down");
            pump(300);
            check!(
                "⑫ 点「销毁」弹出确认框（写操作有二次确认）",
                toplevel_count() > before,
                format!("顶层窗口数 {before} → {}（没弹出确认框）", toplevel_count())
            );
            check!(
                "⑫ 没点确定 ⇒ 动作**没有**执行",
                dockerui::last_action_for_test(&panel.st) == action_before,
                "没确认就执行了！"
            );
            close_last_toplevel();
            pump(300);
            check!(
                "⑫ 取消确认框后仍然没有执行",
                dockerui::last_action_for_test(&panel.st) == action_before,
                "取消之后动作还是跑了"
            );
            check!(
                "⑫ 确认框是**挂着父窗口**的（项目规矩）",
                !puttyplus::windows::dialogs_without_parent()
                    .iter()
                    .any(|t| t.contains("请确认")),
                format!(
                    "确认框没有父窗口：{:?}",
                    puttyplus::windows::dialogs_without_parent()
                )
            );

            // ---- 真跑一次：对着**不存在的项目**（碰不到任何真东西）----
            //
            // ⚠️ 目的是验「确认之后**真的**把命令发到远端了」⇒ 判据是
            //    **执行结果**（失败也算执行了）+ 失败原因来自远端。
            let mut rows = fake_containers();
            rows.push(puttyplus::backend::docker::ContainerInfo {
                id: "ghost-id".into(),
                name: "ghost".into(),
                image: "img:1".into(),
                status: "Up 1 hour".into(),
                state: "running".into(),
                ports: String::new(),
                compose: "pp-smoke-nonexistent".into(),
                compose_files: "/srv/app/docker-compose.yml".into(),
                compose_dir: "/srv/app".into(),
                cpu: "1.0%".into(),
                mem: "10MiB".into(),
            });
            dockerui::set_rows_for_test(&panel.st, rows);
            pump(200);
            check!(
                "⑬ 能选中那个不存在的项目",
                dockerui::select_project_for_test(&panel.st, "pp-smoke-nonexistent"),
                "选不中那个项目行"
            );
            pump(200);
            dockerui::click_project_action_for_test(&panel.st, "start");
            pump(400);
            // ⚠️ 确认框 ⇒ **点确定**（这才是「真跑一次」）。
            confirm_last_dialog();
            // 后台命令要跑一会儿（要走 SSH）⇒ 等它回来。
            let mut waited = 0;
            while waited < 60
                && dockerui::last_action_for_test(&panel.st) == action_before
            {
                pump(200);
                waited += 1;
            }
            let last = dockerui::last_action_for_test(&panel.st);
            check!(
                "⑬ 确认之后**真的**把命令发到远端了（结果有来）",
                !last.is_empty() && last != action_before,
                format!("等了 {waited} 次还是没结果：{last:?}")
            );
            check!(
                "⑬ 不存在的项目 ⇒ 失败并说清原因",
                last.contains("失败") || last.contains("已启动"),
                format!("执行结果是 {last:?}")
            );
        }

    // ================= E 组：阶段 112 —— 日志（界面行为） =================
    // ⚠️ 还在 B 组的作用域里 ⇒ 复用同一个面板（同一个窗口、同一批假容器）。
    {
        // ⚠️ 灌进来的日志**走真实的消息通道** ⇒ 和真流走的是同一条路
        //    （消息循环、拆行、过滤、暂停……全是真的）。
        dockerui::log_watch_for_test(&panel.st, "web");
        pump(100);
        dockerui::log_show_page_for_test(&panel.st);
        pump(200);
        check!(
            "⑭ 能切到日志页",
            dockerui::log_page_visible_for_test(&panel.st),
            "切不过去"
        );
        check!(
            "⑭ 日志正文是等宽的",
            dockerui::log_view_is_monospace_for_test(&panel.st),
            "不是等宽字体（看日志费劲）"
        );

        // 分两块灌：第一块**故意不带换行** ⇒ 验「跨块的半行会拼起来」。
        dockerui::log_push_for_test(&panel.st, "web", "第一行 hello\n第二行 world\n第三");
        pump(200);
        dockerui::log_push_for_test(&panel.st, "web", "行（接上）\n");
        pump(200);

        let text = dockerui::log_text_for_test(&panel.st);
        check!(
            "⑭ 日志显示出来了",
            text.contains("第一行 hello") && text.contains("第二行 world"),
            format!("日志区是：\n{text}")
        );
        check!(
            "⑭ 跨块的半行拼起来了",
            text.contains("第三行（接上）"),
            format!("日志区是：\n{text}")
        );
        check!(
            "⑭ 收到了三行",
            dockerui::log_line_count_for_test(&panel.st) == 3,
            format!("收到 {} 行", dockerui::log_line_count_for_test(&panel.st))
        );

        // ---- **暂停**：数据照收，显示不更新 ----
        dockerui::log_click_pause_for_test(&panel.st);
        pump(100);
        check!(
            "⑭ 进暂停了",
            dockerui::log_paused_for_test(&panel.st),
            "没进暂停"
        );
        dockerui::log_push_for_test(&panel.st, "web", "暂停期间来的行\n");
        pump(200);
        let t2 = dockerui::log_text_for_test(&panel.st);
        check!(
            "⑭ 暂停时显示不更新",
            !t2.contains("暂停期间来的行"),
            format!("暂停时显示还是变了：\n{t2}")
        );
        check!(
            "⑭ 暂停时数据照收（没丢日志）",
            dockerui::log_line_count_for_test(&panel.st) == 4,
            format!("收到 {} 行", dockerui::log_line_count_for_test(&panel.st))
        );

        // ---- **继续**：把暂停期间的行补上 ----
        dockerui::log_click_pause_for_test(&panel.st);
        pump(200);
        let t3 = dockerui::log_text_for_test(&panel.st);
        check!(
            "⑭ 继续后补上暂停期间的行",
            t3.contains("暂停期间来的行"),
            format!("继续后还是缺：\n{t3}")
        );

        // ---- **搜索** ----
        dockerui::log_set_search_for_test(&panel.st, "hello");
        pump(200);
        let t4 = dockerui::log_text_for_test(&panel.st);
        check!(
            "⑭ 搜索只留匹配的行",
            t4.contains("第一行 hello") && !t4.contains("第二行 world"),
            format!("搜索后是：\n{t4}")
        );
        dockerui::log_set_search_for_test(&panel.st, "");
        pump(200);
        let t5 = dockerui::log_text_for_test(&panel.st);
        check!(
            "⑭ 清掉搜索词后全都回来了",
            t5.contains("第一行 hello") && t5.contains("第二行 world"),
            format!("清搜索后是：\n{t5}")
        );

        // ---- **清空** ----
        dockerui::log_click_clear_for_test(&panel.st);
        pump(200);
        check!(
            "⑭ 清空后显示是空的",
            dockerui::log_text_for_test(&panel.st).trim().is_empty(),
            format!("清空后还有：\n{}", dockerui::log_text_for_test(&panel.st))
        );
        check!(
            "⑭ 清空后已收到的行也没了",
            dockerui::log_line_count_for_test(&panel.st) == 0,
            "行数没清"
        );

        // ---- **自动跟随** ----
        //
        // ⚠️ 先灌**很多行**（正文比视口高）⇒ 才谈得上「滚到底」。
        let many: String = (0..300).map(|i| format!("第 {i} 行\n")).collect();
        dockerui::log_push_for_test(&panel.st, "web", &many);
        pump(400);
        check!(
            "⑭ 自动跟随默认开着",
            dockerui::log_follow_for_test(&panel.st),
            "默认没开"
        );
        check!(
            "⑭ 自动跟随 ⇒ 滚到底",
            dockerui::log_at_bottom_for_test(&panel.st),
            "没滚到底"
        );
        // 关掉跟随 + 滚到顶 ⇒ 来了新日志也不许抢走用户的滚动位置。
        dockerui::log_set_follow_for_test(&panel.st, false);
        pump(100);
        dockerui::log_scroll_to_top_for_test(&panel.st);
        pump(100);
        dockerui::log_push_for_test(&panel.st, "web", "关掉跟随之后来的行\n");
        pump(300);
        check!(
            "⑭ 关掉跟随 ⇒ 不再自动滚",
            !dockerui::log_at_bottom_for_test(&panel.st),
            "还是滚到底了（把用户正在看的位置抢走了）"
        );
        dockerui::log_set_follow_for_test(&panel.st, true);
        pump(300);
        check!(
            "⑭ 重新打开跟随 ⇒ 又滚到底",
            dockerui::log_at_bottom_for_test(&panel.st),
            "没滚到底"
        );
    }

    // ⚠️ **F / G 两组都要用**（真连真容器的那个面板）⇒ 提到外层。
    let opts_f = ConnOptions {
        proto: "ssh".into(),
        host: HOST.into(),
        port,
        user: user.clone(),
        keyfile: Some(keyfile_f.clone()),
        ..Default::default()
    };
    let panel2 = SftpPanel::new(&opts_f);
    *panel2.st.sub_tabs.borrow_mut() = Some(subtabs::SubTabs::new());

    // ================= F 组：阶段 112 —— 真实端到端（真连 + 真日志） =================
    {
        // ⚠️ **另起一个面板**（带私钥）⇒ 能真的连上 ⇒ 走完整的
        //    「选中容器 ⇒ 取详情 ⇒ 起日志流 ⇒ 切走就停」。
        // ⚠️ **页面直接放进窗口**（不 mount 进子标签）⇒ 截图能拍到日志页
        //    （挂在子标签里的话，那个页签不是当前页 ⇒ 拍不到）。
        dockerui::ensure(&panel2.st);
        let page2 = dockerui::page_for_test(&panel2.st).expect("应当有容器页");
        let win2 = gtk::Window::new();
        win2.set_title(Some("docker 日志冒烟"));
        win2.set_default_size(1000, 900);
        win2.set_child(Some(&page2));
        win2.present();
        pump(200);
        // ⚠️ **不 mount**（mount 才刷新）⇒ 这里手动触发一次刷新。
        dockerui::refresh_for_test(&panel2.st);
        // 等容器列表真的到（要走 SSH ⇒ 可能要几秒）。
        let mut waited = 0;
        while waited < 40 && dockerui::containers_for_test(&panel2.st).is_empty() {
            pump(200);
            waited += 1;
        }
        let cs = dockerui::containers_for_test(&panel2.st);
        check!(
            "⑮ 真连上了，容器列表到了",
            !cs.is_empty(),
            format!("等了 {waited} 次还是没列表")
        );
        if let Some(c0) = cs.iter().find(|c| c.state == "running").or_else(|| cs.first()) {
            let name = c0.name.clone();
            // ⚠️ **先展开所有 compose 项目**：GTK 的 `set_cursor` 对**看不见
            //    的行**（收着的项目里的子行）**不生效** ⇒ 不展开就选不中。
            dockerui::expand_all_projects_for_test(&panel2.st);
            pump(200);
            check!(
                "⑮ 能选中那个容器行",
                dockerui::select_container_for_test(&panel2.st, &name),
                "选不中"
            );
            // ⚠️ **等详情真的到**。
            //
            // ⚠️ 这条同时验了**阶段 112 修的那个 bug**：之前 `selected`
            //    只有测试钩子会写 ⇒ 真实流程里详情消息被当成「过时的」
            //    丢掉 ⇒ **详情区永远不显示**（冒烟当时没抓住）。
            let mut waited2 = 0;
            while waited2 < 40 && !dockerui::detail_visible_for_test(&panel2.st) {
                pump(200);
                waited2 += 1;
            }
            println!(
                "[信息] 选中={:?} 光标={:?} 状态行={:?}",
                dockerui::selected_for_test(&panel2.st),
                dockerui::cursor_name_for_test(&panel2.st),
                dockerui::status_for_test(&panel2.st)
            );
            check!(
                "⑮ 选中容器后详情真的显示出来了",
                dockerui::detail_visible_for_test(&panel2.st),
                format!("等了 {waited2} 次还是没详情")
            );
            // 切到日志页 ⇒ 起流 ⇒ 等日志真的进来。
            dockerui::log_show_page_for_test(&panel2.st);
            pump(200);
            let mut waited3 = 0;
            while waited3 < 40 && dockerui::log_line_count_for_test(&panel2.st) == 0 {
                pump(200);
                waited3 += 1;
            }
            let n = dockerui::log_line_count_for_test(&panel2.st);
            println!("[信息] 真实日志流收到 {n} 行（容器 {name}）");
            check!(
                "⑮ 日志流真的往界面上送行了",
                n > 0,
                format!("等了 {waited3} 次还是没日志（容器可能真的没日志）")
            );
            // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张日志页。
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                pump(300);
                shot(&format!("{base}-log.png"));
            }
            // 切回概览 ⇒ 流停 ⇒ **远端不许留进程**（看门狗）。
            dockerui::log_show_overview_for_test(&panel2.st);
            pump(300);
            let mut waited4 = 0;
            let mut leftover = String::new();
            while waited4 < 20 {
                leftover = remote_log_procs(&opts_f);
                if leftover.is_empty() {
                    break;
                }
                pump(300);
                waited4 += 1;
            }
            check!(
                "⑮ 切走日志页后远端没有残留的 docker logs 进程",
                leftover.is_empty(),
                format!("远端还挂着：{leftover}")
            );
        }
    }

    // ================= G 组：阶段 113 —— 容器内终端（真连真容器） =================
    {
        // ⚠️ 复用 F 组那个面板（容器列表已经在、容器也选中了）。
        let keyfile_g = keyfile_f.clone();
        let user_g = user.clone();
        let cs = dockerui::containers_for_test(&panel2.st);
        let c0 = cs
            .iter()
            .find(|c| c.state == "running")
            .or_else(|| cs.first())
            .cloned();
        if let Some(c0) = c0 {
            let name = c0.name.clone();
            let name_b = name.clone();
            // ---- 后端：先看容器里有没有 shell ----
            let shell = hub::runtime().block_on(async move {
                let creds = hub::Creds {
                    keyfile: Some(keyfile_g.clone()),
                    ..Default::default()
                };
                let key = hub::ServerKey::new(HOST, port, &user_g);
                let Ok(shared) = hub::acquire(key, &creds).await else {
                    return Err("连不上".to_string());
                };
                docker::find_shell(shared.session().as_ref(), &name_b)
                    .await
                    .map_err(|e| e.0)
            });
            match shell {
                Ok(sh) => {
                    println!("[信息] 容器 {name} 里的 shell：{sh}");
                    check!(
                        "⑯ 容器里找到能用的 shell",
                        sh.starts_with("/bin/"),
                        format!("找到的是 {sh:?}")
                    );
                }
                Err(msg) => {
                    println!("[FAIL] 容器里找不到 shell：{msg}");
                    FAILS.with(|f| f.borrow_mut().push(format!("容器里找不到 shell：{msg}")));
                }
            }

            // ---- 前端：开容器内终端 ----
            check!(
                "⑯ 详情区的「终端」按钮能点",
                dockerui::term_button_sensitive_for_test(&panel2.st),
                "按钮是灰的"
            );
            dockerui::open_terminal_for_test(&panel2.st, &name);
            let mut waited = 0;
            while waited < 40 && !dockerui::term_running_for_test(&panel2.st) {
                pump(200);
                waited += 1;
            }
            check!(
                "⑯ 容器内终端开起来了",
                dockerui::term_running_for_test(&panel2.st),
                format!("等了 {waited} 次还没起来")
            );

            // ---- 往终端里打字 ⇒ 屏幕上有回显 ⇒ 命令真的跑起来了 ----
            dockerui::term_send_for_test(&panel2.st, b"echo pp-shell-ok\n");
            let mut waited2 = 0;
            let mut screen = String::new();
            while waited2 < 30 {
                pump(200);
                waited2 += 1;
                screen = dockerui::term_screen_for_test(&panel2.st);
                if screen.contains("pp-shell-ok") {
                    break;
                }
            }
            println!("[信息] 终端屏幕：\n{screen}");
            check!(
                "⑯ 终端里有回显（命令真的在容器里跑起来了）",
                screen.contains("pp-shell-ok"),
                format!("屏幕上没有回显：\n{screen}")
            );
            // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张容器内终端。
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-term.png"));
            }

            // ---- 关掉 ⇒ 远端不许留孤儿 `docker exec` ----
            dockerui::stop_terminal_for_test(&panel2.st);
            let mut waited3 = 0;
            let mut leftover = String::new();
            while waited3 < 20 {
                leftover = remote_procs(&opts_f, "docker exec");
                if leftover.is_empty() {
                    break;
                }
                pump(300);
                waited3 += 1;
            }
            check!(
                "⑯ 关掉终端后远端没有残留的 docker exec 进程",
                leftover.is_empty(),
                format!("远端还挂着：{leftover}")
            );
            // ---- 关掉终端 ⇒ **窗口也得关掉**（阶段 125 修的真 bug）----
            //
            // ⚠️ 以前只把窗口**句柄** take 掉、窗口本身还在 ⇒ 用户关掉
            //   容器页签后，屏幕上留着个**死终端窗口**（还盖住别的窗口）。
            check!(
                "⑯ 关掉终端后窗口也关掉了",
                !dockerui::term_window_open_for_test(&panel2.st),
                "终端窗口还开着（关掉容器页签后会留在屏幕上）"
            );
            // ⚠️ **关掉终端窗口**：它盖在容器面板上面 ⇒ 后面截的图就拍不到
            //   容器页（更拍不到监控页）。
            dockerui::close_terminal_window_for_test(&panel2.st);
            pump(200);
        }
    }

    // ================= H 组：阶段 113 之后 —— 编辑器要「就在容器界面里」 =================
    //
    // ⚠️ **用户 2026-11-16 报的**：「编辑 docker-compose 文件」按钮点了以后，
    //   文件编辑器打开了，**却不在容器界面**，用户会找不到 ⇒ 应该打开之后
    //   **用户马上能看到**，**不要跑到 term 界面**。
    //
    // ⇒ 根因：编辑区是**一个共享的 widget**，它住在**终端列**下面 ⇒ 在
    //   容器页打开编辑器 ⇒ 编辑器建好了，却**藏在 term 界面里**。
    // ⇒ 修法：容器页也有编辑区的落点 ⇒ 切到容器页时把编辑区搬过来。
    {
        let panel3 = SftpPanel::new(&opts_f);
        let sub3 = subtabs::SubTabs::new();
        *panel3.st.sub_tabs.borrow_mut() = Some(sub3.clone());
        let win3 = gtk::Window::new();
        win3.set_default_size(1000, 620);
        win3.set_child(Some(&sub3.root().clone()));
        win3.present();
        pump(200);
        // 挂载容器页 ⇒ 它成为当前页 ⇒ **同时开始取数据**。
        dockerui::mount(&panel3.st);

        // ---- ⑱ loading 动画（用户 2026-11-16）----
        //
        // ⚠️ **为什么要有**：打开容器界面后要走 SSH 去远端跑 `docker ps`
        //   ⇒ 要等一会儿 ⇒ 这期间列表是**空的** ⇒ 用户不知道是在干活还是
        //   卡死了 ⇒ 要有**看得见的动画**。
        //
        // ⚠️ **这里刻意不 pump**：`refresh()` 是**同步**发起取数的 ⇒
        //   `set_loading(true)` 也在这一刻就跑完了 ⇒ 不 pump 就不会
        //   「等太久、列表已经到了、转圈已经停了」（⇒ 断言是确定的）。
        let (small_v, big_v, spinning) = dockerui::loading_spinners_for_test(&panel3.st);
        check!(
            "⑱ 加载时工具条有转圈",
            small_v && spinning,
            format!("小转圈可见={small_v} 在转={spinning}")
        );
        check!(
            "⑱ 首次加载时列表正中间有转圈",
            big_v,
            "正中间那个大转圈没出现"
        );
        // 截一张 loading 动画（要 pump 一下 ⇒ GTK 才真的画出来）。
        pump(300);
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-loading.png"));
        }

        check!(
            "⑰ 容器页挂上了",
            sub3.page_count() >= 1,
            "容器页没进子标签"
        );
        check!(
            "⑰ 编辑区搬到了容器页（不用去 term 界面找）",
            dockerui::editor_area_in_docker_page_for_test(&panel3.st),
            "编辑区还在终端列里"
        );
        // 加一个编辑器 ⇒ 编辑区露出来，而且**还在容器页里**。
        let dummy = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sub3.add_editor(9001, None, "冒烟", dummy.upcast(), None);
        pump(200);
        check!(
            "⑰ 编辑区露出来了（开着编辑器却看不见 = 功能等于没有）",
            sub3.editor_area_visible(),
            "编辑区是隐的"
        );
        check!(
            "⑰ 编辑器还在容器页里",
            dockerui::editor_area_in_docker_page_for_test(&panel3.st),
            "编辑区被搬走了"
        );
        // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张「编辑器在容器页里」。
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-editor.png"));
        }

        // ---- 等列表真的到了 ⇒ 转圈**就该停**（不然一直转 ⇒ 误导用户）----
        let mut waited = 0;
        while waited < 40 && dockerui::containers_for_test(&panel3.st).is_empty() {
            pump(200);
            waited += 1;
        }
        pump(200);
        let (small_v2, big_v2, _) = dockerui::loading_spinners_for_test(&panel3.st);
        check!(
            "⑱ 列表到了以后转圈停掉",
            !small_v2 && !big_v2,
            format!("小转圈可见={small_v2} 大转圈可见={big_v2}")
        );
        // ⚠️ **关掉这个窗口**：它盖在 F/G/K 组那个面板上面 ⇒ 后面截的图
        //   就只能拍到它（拍不到监控页）。
        win3.close();
    }

    // ================= K 组：阶段 114 —— 监控曲线（真连真容器） =================
    //
    // ⚠️ 复用 **F / G 组那个面板**（panel2）：它已经真连上了、也已经选中了
    //   一个运行中的容器 ⇒ 这里直接切到「监控」页就能起流。
    {
        let cs = dockerui::containers_for_test(&panel2.st);
        let c0 = cs
            .iter()
            .find(|c| c.state == "running")
            .or_else(|| cs.first())
            .cloned();
        if let Some(c0) = c0 {
            let name = c0.name.clone();
            println!("[信息] 监控页看的是容器：{name}");
            // ---- 切到监控页 ⇒ 起流 ----
            dockerui::monitor_show_page_for_test(&panel2.st);
            let mut waited = 0;
            while waited < 40 && !dockerui::stat_running_for_test(&panel2.st) {
                pump(200);
                waited += 1;
            }
            check!(
                "⑲ 监控流起来了",
                dockerui::stat_running_for_test(&panel2.st),
                format!("等了 {waited} 次还没起来")
            );
            // ---- 等点（**2 秒一个点** ⇒ 至少等 4~5 秒）----
            let mut waited2 = 0;
            while waited2 < 40 && dockerui::stat_points_for_test(&panel2.st).0 == 0 {
                pump(200);
                waited2 += 1;
            }
            let (cpu_n, net_n) = dockerui::stat_points_for_test(&panel2.st);
            check!(
                "⑲ 曲线上真的有点了",
                cpu_n > 0 && net_n > 0,
                format!("CPU {cpu_n} 点 / 网络 {net_n} 点（等了 {waited2} 次）")
            );
            // ---- 再等几个点 ⇒ **曲线真的画出来了**（一个点画不出线）----
            //
            // ⚠️ **为什么多等**：折线要 ≥2 个点才画得出线 ⇒ 等 3 个点，
            //   截图里才看得见「曲线」而不是四个空框。
            let mut waited_pts = 0;
            while waited_pts < 30 && dockerui::stat_points_for_test(&panel2.st).0 < 3 {
                pump(300);
                waited_pts += 1;
            }
            let (cpu_n3, _) = dockerui::stat_points_for_test(&panel2.st);
            check!(
                "⑲ 曲线攒够了点（能画出线）",
                cpu_n3 >= 3,
                format!("CPU 只有 {cpu_n3} 点（等了 {waited_pts} 次）")
            );
            // ---- 指标卡有当前值 ----
            let (cpu, mem, net, blk) = dockerui::stat_values_for_test(&panel2.st);
            println!("[信息] 指标卡：CPU {cpu}｜内存 {mem}｜网络 {net}｜磁盘 {blk}");
            check!(
                "⑲ 指标卡有当前值",
                cpu.contains('%') && net.contains("B/s"),
                format!("CPU={cpu} 网络={net}")
            );
            // ---- 剥 ANSI 后解析出来的值要跟**原始行**对得上 ----
            //
            // ⚠️ **为什么必须跟原始行对照**：`docker stats` 的持续模式
            //   带 ANSI（`\x1b[H`…）⇒ 忘了剥 ⇒ 解析出来的 CPU **全是 0**
            //   ⇒ 光看「指标卡有当前值」**抓不住**（0.00% 也是"有值"）
            //   ⇒ 得拿**原始行**做对照才抓得住。
            let raw = dockerui::stat_last_raw_for_test(&panel2.st).unwrap_or_default();
            check!(
                "⑲ 拿到了原始采样行",
                !raw.is_empty(),
                "没拿到原始行（钩子空）"
            );
            check!(
                "⑲ 原始行确实带 ANSI（所以必须剥）",
                raw.contains('\u{1b}'),
                format!("原始行：{raw:?}")
            );
            // 独立地从原始行里取 CPU 数字（**不复用程序里的解析**）
            // ⇒ 免得「程序和程序自己比」变成同义反复。
            let cpu_field = raw.split('|').next().unwrap_or("");
            let digits: String = cpu_field
                .trim_end_matches('%')
                .trim()
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let expect = digits
                .parse::<f64>()
                .ok()
                .map(|v| format!("{v:.2}%"))
                .unwrap_or_default();
            check!(
                "⑲ 指标卡 CPU 与原始行一致（说明 ANSI 剥对了）",
                !expect.is_empty() && cpu == expect,
                format!("卡片={cpu} 原始行推出来的={expect}（原始={raw:?}）")
            );
            // 截一张（曲线真的画出来了才好看）。
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-monitor.png"));
            }
            // ---- 切走 ⇒ 远端不许留孤儿 `docker stats` ----
            dockerui::monitor_show_overview_for_test(&panel2.st);
            let mut waited3 = 0;
            let mut leftover = String::new();
            while waited3 < 20 {
                leftover = remote_procs(&opts_f, "docker stats");
                if leftover.is_empty() {
                    break;
                }
                pump(300);
                waited3 += 1;
            }
            check!(
                "⑲ 切走后远端没有残留的 docker stats 进程",
                leftover.is_empty(),
                format!("远端还挂着：{leftover}")
            );
    // ================= L 组：阶段 115 —— 镜像 / 卷 / 网络 =================
    //
    // ⚠️ 复用 **F / G / K 组那个面板**（panel2）：它已经真连上了 ⇒ 这里
    //   切分段就能拿到**真数据**（这台机器上真的有镜像 / 卷 / 网络）。
    {
        // ---- 切到「镜像」页 ----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_IMAGES);
        check!(
            "⑳ 切到镜像页了",
            dockerui::seg_current_for_test(&panel2.st) == "images",
            format!("当前分段={}", dockerui::seg_current_for_test(&panel2.st))
        );
        let mut waited = 0;
        while waited < 30 && dockerui::images_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited += 1;
        }
        let imgs = dockerui::images_rows_for_test(&panel2.st);
        println!("[信息] 镜像 {} 个：{:?}", imgs.len(), &imgs.first());
        check!(
            "⑳ 镜像表有行了",
            !imgs.is_empty(),
            format!("等了 {waited} 次还是空的")
        );
        // ⚠️ **内容也要对**：仓库与大小都不能是空的
        //   ⇒ 抓「解析错位/字段对不上」这类退化（光看「有行」抓不住）。
        check!(
            "⑳ 镜像行的仓库与大小不是空的",
            imgs.iter().any(|r| !r.0.trim().is_empty() && !r.3.trim().is_empty()),
            format!("{imgs:?}")
        );
        // ---- **独立对照**：界面第一行 vs 远端原文 ----
        //
        // ⚠️ 这条才抓得住「**解析错位**」（仓库/标签/大小 串了列）：
        //   光看「非空」抓不住（都非空）⇒ 得跟远端原文对。
        let remote_line = remote_images_line(&opts_f);
        let remote_first = remote_line.split('|').next().unwrap_or("").to_string();
        let ui_first = imgs.first().map(|r| r.0.clone()).unwrap_or_default();
        check!(
            "⑳ 界面第一行镜像与远端原文对得上",
            !remote_first.is_empty() && ui_first == remote_first,
            format!("界面={ui_first:?} 远端={remote_first:?}（整行={remote_line:?}）")
        );
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-images.png"));
        }

        // ---- 切到「卷」页 ----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_VOLUMES);
        let mut waited2 = 0;
        while waited2 < 30 && dockerui::volumes_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited2 += 1;
        }
        let vols = dockerui::volumes_rows_for_test(&panel2.st);
        println!("[信息] 卷 {} 个：{:?}", vols.len(), &vols.first());
        check!(
            "⑳ 卷表有行了",
            !vols.is_empty(),
            format!("等了 {waited2} 次还是空的")
        );
        check!(
            "⑳ 卷行的名字与驱动不是空的",
            vols.iter().any(|r| !r.0.trim().is_empty() && !r.1.trim().is_empty()),
            format!("{vols:?}")
        );

        // ---- 切到「网络」页 ----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_NETWORKS);
        let mut waited3 = 0;
        while waited3 < 30 && dockerui::networks_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited3 += 1;
        }
        let nets = dockerui::networks_rows_for_test(&panel2.st);
        println!("[信息] 网络 {} 个：{:?}", nets.len(), &nets.first());
        check!(
            "⑳ 网络表有行了",
            !nets.is_empty(),
            format!("等了 {waited3} 次还是空的")
        );
        check!(
            "⑳ 网络行的名字与范围不是空的",
            nets.iter().any(|r| !r.0.trim().is_empty() && !r.2.trim().is_empty()),
            format!("{nets:?}")
        );
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-networks.png"));
        }

        // ---- 切回「容器」页 ⇒ **容器列表还在**（别把容器表弄丢了）----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_CONTAINERS);
        pump(500);
        let conts = dockerui::model_rows_for_test(&panel2.st);
        check!(
            "⑳ 切回容器页，容器列表还在",
            !conts.is_empty(),
            "容器表空了（切分段把容器列表弄丢了？）"
        );
        // ⚠️ **不要硬编码容器名**（换台机器/换个容器就假红）⇒ 改成看
        //   「程序记着的那个选中项还在不在」。
        let sel = dockerui::selected_for_test(&panel2.st);
        check!(
            "⑳ 切回容器页，原来选中的容器还在",
            !sel.is_empty() && conts.iter().any(|r| r.0 == sel),
            format!("选中={sel} 容器行={conts:?}")
        );
    }
        }
    }
    }


    let n_fail = FAILS.with(|f| f.borrow().len());    if n_fail == 0 {
        println!("docker_ui_smoke: 全部通过");
    } else {
        println!("docker_ui_smoke: {} 条失败", n_fail);
        FAILS.with(|f| {
            for x in f.borrow().iter() {
                println!("  - {x}");
            }
        });
        std::process::exit(1);
    }
}
