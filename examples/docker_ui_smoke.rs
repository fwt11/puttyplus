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

/// **等表格把某几行真的画出来**（最多 `ms` 毫秒）⇒ 返回屏幕上的行。
///
/// ⚠️ **为什么要等**（阶段 167 记）：`displayed_rows()` 是**按像素坐标**
///   问 GTK「这一行是哪个」⇒ **表格还没布局完**（刚 `set_rows_for_test`、
///   刚展开项目、或者**机器正忙**（release 重编译））时它会**少报几行**
///   ⇒ 断言就**假红**（2026-11-25 实测：机器一忙，⑥ ⑦ 两条就红了 ⇒
///   恢复代码后又全绿 ⇒ 是**抖动**，不是回归）。
/// ⇒ 改成「**等到看见为止**」⇒ 断言只在该红的时候红。
fn wait_displayed_rows(view: &gtk::TreeView, want: &[&str], ms: u64) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut rows = Vec::new();
    loop {
        rows = displayed_rows(view);
        if want.iter().all(|w| rows.iter().any(|n| n == w)) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        pump(120);
    }
    rows
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
        // ⚠️ **阶段 158**：状态列要显示时长 ⇒ 假数据也得带时长。
        duration: Some(if running {
            puttyplus::backend::docker::DockerDuration::Num(
                2,
                puttyplus::backend::docker::DurUnit::Hour,
            )
        } else {
            puttyplus::backend::docker::DockerDuration::Num(
                1,
                puttyplus::backend::docker::DurUnit::Hour,
            )
        }),
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

/// **最后一个顶层窗口里的所有文字**（确认框写了什么 ⇒ 阶段 163 用）。
fn last_window_text() -> String {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    let n = model.n_items();
    if n == 0 {
        return String::new();
    }
    let Some(w) = model.item(n - 1).and_downcast::<gtk::Window>() else {
        return String::new();
    };
    let mut out = String::new();
    collect_text(w.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 递归收集一棵控件树里的文字（Label / Button 的字）。
fn collect_text(w: &gtk::Widget, out: &mut String) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push_str(&l.text().to_string());
        out.push('\n');
    }
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        if let Some(t) = b.label() {
            out.push_str(&t.to_string());
            out.push('\n');
        }
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        collect_text(&x, out);
        c = x.next_sibling();
    }
}

/// 远端还有没有**某个进程**（判据用）。
///
/// ⚠️ `pgrep -af '[d]ocker logs'` 里的 `[d]` 是老技巧：命令自己不会匹配自己
///    （不然 pgrep 那行命令自己也算一个 ⇒ 永远非空）。
///
/// ⚠️ **阶段 165：还要锚住「命令行的开头」**（实测踩到）：`[d]ocker logs`
///    只躲开 pgrep 自己 ⇒ **别的进程**只要命令行里**含有**「docker logs」
///    就会被算进来（实测：跑冒烟的那条 bash 命令里正好有这几个字 ⇒ 误报
///    「远端还挂着 docker logs」）⇒ 加 `(^|/)` ⇒ 只认**真正在跑的那个
///    docker 客户端**（`docker logs` 或 `/usr/bin/docker logs`）。
fn remote_procs(opts: &ConnOptions, what: &str) -> String {
    let pat = format!("(^|/){}", what[..1].to_string() + &what[1..]);
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

/// 远端跑一条命令，拿回输出（**独立对照**用）。
///
/// ⚠️ **命令自己写**（不复用 `docker::` 里的命令构造器）⇒ 否则就是
///   「程序跟程序自己比」＝同义反复（阶段 115 踩过这个道理）。
fn remote_cmd(opts: &ConnOptions, cmd: &str) -> String {
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
        let (_, out) = docker::run_cmd(shared.session().as_ref(), cmd)
            .await
            .unwrap_or((0, String::new()));
        out
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
        win.set_default_size(1420, 560);
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
        // ⚠️ **阶段 162：名称列开了 markup** ⇒ 「text」属性里放的是
        //   **markup 源码**（项目行是 `<b>名字</b> <span…>compose</span>`）
        //   ⇒ 判据改成「**里面含有**某个名字」。
        check!(
            "⑤ 表格真的把文字画出来了（不是一片空白）",
            !drawn.is_empty() && shown0.iter().any(|n| drawn.contains(n.as_str())),
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
        let shown1 = wait_displayed_rows(&view, &["web", "db"], 5000);
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
        let shown2 = wait_displayed_rows(&view, &["web", "db"], 5000);
        check!(
            "⑦ 刷新后屏幕上仍然看得到 web / db",
            shown2.iter().any(|n| n == "web") && shown2.iter().any(|n| n == "db"),
            format!("刷新后 web/db 消失了：{shown2:?}")
        );

        // ================= 阶段 155：CPU / 内存 / 详情 三列退役 =================
        //
        // ⚠️ 用户 2026-11-23：「去掉详情列，没啥用。去掉 CPU 和内存列。」
        //   ⇒ 这三列**必须真的没了**（不是挪了个位置）。
        {
            let titles = dockerui::column_titles_for_test(&panel.st);
            println!("[信息] 现在的列：{titles:?}");
            check!(
                "⑨ 「CPU」「内存」「详情」三列退役了",
                !titles.iter().any(|t| t == "CPU")
                    && !titles.iter().any(|t| t == "内存")
                    && !titles.iter().any(|t| t == "详情"),
                format!("现在的列是：{titles:?}")
            );
            check!(
                "⑨ 多了一列「操作」",
                titles.iter().any(|t| t == "操作"),
                format!("现在的列是：{titles:?}")
            );
            // ⚠️ **剩下的列还是能排序的**（退役的是数值列，不是排序功能）。
            let sorted = dockerui::sort_by_for_test(&panel.st, 0, false);
            println!("[信息] 按名称降序后的顶层行：{sorted:?}");
            check!(
                "⑨ 名称列还能排序（降序）",
                sorted.len() == 3
                    && sorted[0] == "solo-b"
                    && sorted[1] == "solo-a"
                    && sorted[2] == "proj",
                format!("按名称降序是：{sorted:?}")
            );
        }

        // ================= UI 审查 2026-11-18：空态（D6） =================
        {
            // 一个容器都没有 ⇒ 列表区**不该**是一块空白 ⇒ 要有空态。
            dockerui::set_rows_for_test(&panel.st, Vec::new());
            pump(200);
            check!(
                "⑨ 列表空了 ⇒ 显示空态（不是一块空白）",
                dockerui::empty_state_visible_for_test(&panel.st),
                "列表空了却什么也没显示"
            );
            // ⚠️ **标题要说清「是什么」**（用户可见文案）。
            let t = dockerui::empty_state_title_for_test(&panel.st);
            check!(
                "⑨ 空态有标题",
                !t.is_empty(),
                "空态没有标题"
            );
            // 放回正常数据 ⇒ 空态收起来。
            dockerui::set_rows_for_test(&panel.st, fake_containers());
            pump(200);
            check!(
                "⑨ 有数据了 ⇒ 空态收起来",
                !dockerui::empty_state_visible_for_test(&panel.st),
                "有数据了还挂着空态"
            );
        }

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
            "⑨ 详情内容显示出来了",
            dockerui::detail_body_visible_for_test(&panel.st),
            "详情内容还是收着的（选中了容器就该显示）"
        );
        let title = dockerui::detail_title_for_test(&panel.st);
        check!(
            "⑨ 标题写着容器名",
            title.contains("web"),
            format!("标题是 {title:?}")
        );
        // 状态搬进了药丸（美化·地基 2/5）：文字 + 档位类都要在
        let (pill_text, pill_classes) = dockerui::detail_pill_for_test(&panel.st);
        check!(
            "⑨ 状态药丸写着运行中且挂了绿档类",
            pill_text.contains("运行中") && pill_classes.iter().any(|c| c == "pp-pill-ok"),
            format!("药丸是 {pill_text:?}，类是 {pill_classes:?}")
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
        // ⚠️ **概览页现在是「键 | 值」的表**（UI 审查 D9）⇒ 环境变量是
        //   `PATH` 与 `/usr/bin` **两格**，不再是一行 `PATH=/usr/bin`。
        check!(
            "⑨ 环境变量卡片有内容（键与值两格）",
            env.contains("PATH") && env.contains("/usr/bin"),
            format!("环境变量卡片是：\n{env}")
        );
        // ⚠️ **阶段 155**：按钮搬进「操作」列了 ⇒ 可用性看**那一行的图标**。
        check!(
            "⑨ 运行中 ⇒ 「启动」点不了、「停止」能点",
            !dockerui::row_action_enabled_for_test(&panel.st, "web", "start")
                && dockerui::row_action_enabled_for_test(&panel.st, "web", "stop"),
            "操作列的可用性跟状态对不上"
        );
        // ⚠️ **「操作」列的图标得是 Adwaita 真有的**（写错 ⇒ 空白方块）。
        {
            let icons = dockerui::row_action_icons_for_test(&panel.st, "web");
            let names: Vec<&str> = icons.iter().map(|(i, _)| i.as_str()).collect();
            println!("[信息] 容器行的操作图标：{names:?}");
            // ⚠️ **阶段 160**：操作列只剩 3 颗（启动 / 停止 / 重启）
            //   ⇒ 详情 / 进容器等**搬进右键菜单**了。
            check!(
                "⑨ 容器行的「操作」列只有 3 颗图标（启动 / 停止 / 重启）",
                icons.len() == 3
                    && names.contains(&"pp-play")
                    && names.contains(&"pp-stop")
                    && names.contains(&"view-refresh-symbolic"),
                format!("容器行的图标是：{names:?}")
            );
            let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().expect("display"));
            // ⚠️ **`pp-` 开头的是自绘图标**（阶段 157）⇒ 不在主题里，跳过。
            let missing: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| !n.starts_with("pp-") && !theme.has_icon(*n))
                .collect();
            check!(
                "⑨ 「操作」列的图标名在系统主题里都真的有（不是空白方块）",
                missing.is_empty(),
                format!("主题里没有这些图标：{missing:?}")
            );
        }
        // ⚠️ **阶段 155**：详情搬进**弹窗**了 ⇒ 点「详情」图标才打开。
        check!(
            "⑨ 详情弹窗**还没开**（要点「详情」图标才开）",
            !dockerui::detail_visible_for_test(&panel.st),
            "弹窗自己开了（那就不是弹窗的用法了）"
        );
        // ⚠️ **用不依赖模型的派发钩子**（后台真实刷新可能把灌进去的假数据
        //   换掉 ⇒ 按名字查行会查不到 ⇒ 断言偶发红）。
        dockerui::dispatch_action_for_test(&panel.st, "web", "container", "detail");
        pump(300);
        check!(
            "⑨ 点「详情」图标 ⇒ 详情弹窗打开了",
            dockerui::detail_visible_for_test(&panel.st),
            "点了「详情」图标，弹窗没开"
        );
        let wtitle = dockerui::detail_window_title_for_test(&panel.st);
        check!(
            "⑨ 弹窗标题里有容器名",
            wtitle.contains("web"),
            format!("弹窗标题是 {wtitle:?}")
        );
        // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张详情弹窗。
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-detail-popup.png"));
        }

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
            "⑨ 刷新后详情内容还开着",
            dockerui::detail_body_visible_for_test(&panel.st),
            "刷新后详情内容收起来了"
        );

        // ⚠️ **二次确认**：写操作（停容器）**必须先问一句**。
        //
        // ⚠️ **安全**：这里点的是**不存在的容器名** ⇒ 就算确认逻辑坏了，
        //    docker 也只会拒绝 ⇒ **碰不到用户机器上的真容器**。
        let before = toplevel_count();
        // ⚠️ 判据用「**最近一次写操作**」，**不用状态行**：状态行会被后台
        //    刷新改掉 ⇒ 拿它判断会误判（刷新先到就当成动作跑了）。
        let action_before = dockerui::last_action_for_test(&panel.st);
        dockerui::dispatch_action_for_test(&panel.st, "web", "container", "stop");
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
                !dockerui::detail_body_visible_for_test(&panel.st),
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
            // ---- 「操作」列的 6 颗图标（阶段 155）----
            {
                let icons = dockerui::row_action_icons_for_test(&panel.st, "proj");
                let names: Vec<&str> = icons.iter().map(|(i, _)| i.as_str()).collect();
                println!("[信息] 项目行的操作图标：{names:?}");
                // ⚠️ **阶段 163**：项目行**只有一颗「重建」**（用户 2026-11-24：
                //   「down 了之后容器界面就看不到了 ⇒ 那 up 也没意义 ⇒ 就来
                //   一个 recreate 吧」）⇒ **没有「销毁」那颗**。
                check!(
                    "⑪ 项目行的「操作」列有 4 颗图标（启动 / 停止 / 重启 / 重建）",
                    icons.len() == 4
                        && names.contains(&"pp-play")
                        && names.contains(&"pp-stop")
                        && names.contains(&"view-refresh-symbolic")
                        && names.contains(&"edit-copy-symbolic")
                        && !names.contains(&"go-down-symbolic"),
                    format!("项目行的图标是：{names:?}")
                );
                // ⚠️ **项目全在跑 ⇒「启动」是灰的**（没得启动），
                //   「停止」能点。
                check!(
                    "⑪ 项目全在跑 ⇒「启动」点不了、「停止」能点",
                    !dockerui::row_action_enabled_for_test(&panel.st, "proj", "start")
                        && dockerui::row_action_enabled_for_test(&panel.st, "proj", "stop"),
                    "项目行的可用性跟状态对不上"
                );
                // ⚠️ **阶段 160**：详情 / 编辑 / 销毁 / 重建**搬进右键菜单**了
                //   ⇒ 「操作」列里没有它们 ⇒ 改验「菜单里有」。
                let pm = dockerui::menu_actions_for_test("project", "running");
                let pmk: Vec<&str> = pm.iter().map(|(k, _)| k.as_str()).collect();
                check!(
                    "⑪ 项目行的右键菜单里有 编辑/属性/启停/销毁/重建",
                    pmk.contains(&"edit")
                        && pmk.contains(&"detail")
                        && pmk.contains(&"start")
                        && pmk.contains(&"stop")
                        && pmk.contains(&"restart")
                        && pmk.contains(&"down")
                        && pmk.contains(&"recreate"),
                    format!("项目行的菜单是：{pm:?}")
                );
            }

            // ---- ⑪b 选中**独立容器** ⇒ compose 项目那块要**收起来** ----
            //
            // ⚠️ **阶段 139**（用户 2026-11-19 报的截图）：选的是独立容器
            //   some-postgres（**它没有 compose**）⇒ 底下却显示着**别的
            //   项目** eng_tut 的 compose 信息 ⇒ 用户分不清那是哪个容器的。
            //
            //   ⇒ 规则：**点项目只显示 compose 内容；点容器只显示容器内容**。
            check!(
                "⑪b 能选中独立容器行",
                dockerui::select_container_for_test(&panel.st, "solo-a"),
                "选不中独立容器行"
            );
            pump(300);
            check!(
                "⑪b 选中容器 ⇒ **compose 项目那块藏起来**（不显示不相干的 compose）",
                !dockerui::project_detail_visible_for_test(&panel.st),
                "容器详情和上一个项目的 compose 信息**同时显示**着"
            );
            check!(
                "⑪b 选中容器 ⇒ 记住的 compose 项目名也清掉了（刷新不会又冒出来）",
                dockerui::selected_project_for_test(&panel.st).is_empty(),
                format!(
                    "还记着 {:?}",
                    dockerui::selected_project_for_test(&panel.st)
                )
            );
            check!(
                "⑪b 选中容器 ⇒ 容器详情显示着",
                dockerui::detail_body_visible_for_test(&panel.st),
                "容器详情没显示"
            );
            // ---- 反过来：再点回项目行 ⇒ compose 内容回来、容器详情藏起来 ----
            check!(
                "⑪b 点回项目行 ⇒ compose 内容又出来了",
                dockerui::select_project_for_test(&panel.st, "proj"),
                "选不中项目行"
            );
            pump(300);
            check!(
                "⑪b 点回项目行 ⇒ 项目详情显示着",
                dockerui::project_detail_visible_for_test(&panel.st),
                "项目详情没显示"
            );
            check!(
                "⑪b 点回项目行 ⇒ 容器详情**藏起来**了（两种详情互斥）",
                !dockerui::detail_body_visible_for_test(&panel.st),
                "容器详情还开着"
            );

            // ---- 写操作 ⇒ 二次确认 ----
            //
            // ⚠️ **安全**：这里**不点确定** ⇒ 命令不会跑 ⇒ 碰不到远端任何项目。
            let before = toplevel_count();
            let action_before = dockerui::last_action_for_test(&panel.st);
            dockerui::dispatch_action_for_test(&panel.st, "proj", "project", "down");
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

            // ---- ⑫ 之二：点「重建」⇒ 确认框里说清**会发生什么** ----
            //
            // ⚠️ **阶段 163**：重建 ＝ `docker compose up -d --force-recreate`
            //   ⇒ 容器**会被换掉**（不是重启）⇒ 确认框得写清楚。
            let before = toplevel_count();
            dockerui::dispatch_action_for_test(&panel.st, "proj", "project", "recreate");
            pump(300);
            check!(
                "⑫ 之二 点「重建」弹出确认框",
                toplevel_count() > before,
                format!("顶层窗口数 {before} → {}（没弹出确认框）", toplevel_count())
            );
            let dlg = last_window_text();
            println!("[信息] 重建的确认框文字：\n{dlg}");
            check!(
                "⑫ 之二 确认框里写了「重建」",
                dlg.contains("重建"),
                format!("确认框文字是：\n{dlg}")
            );
            check!(
                "⑫ 之二 确认框里说了「容器会被重新创建」",
                dlg.contains("重新创建"),
                format!("确认框文字是：\n{dlg}")
            );
            close_last_toplevel();
            pump(300);

            // ---- 真跑一次：对着**不存在的项目**（碰不到任何真东西）----
            //
            // ⚠️ 目的是验「确认之后**真的**把命令发到远端了」⇒ 判据是
            //    **执行结果**（失败也算执行了）+ 失败原因来自远端。
            let mut rows = fake_containers();
            // ⚠️ **阶段 160**：几条贴近真实的独立容器（长镜像名 / 多端口 /
            //   各种状态与时长）⇒ 截图能看出「列宽合不合理」。
        for (nm, idv, imgv, stv, raw, prt, dur) in [
            ("eng_tut", "b6ddf7f1a2b3", "emqx/emqx-enterprise:latest", "running", "Up 4 days", "0.0.0.0:1883->1883/tcp", (4u64, puttyplus::backend::docker::DurUnit::Day)),
            ("some-postgres", "c1007e60d4e5", "postgres:17", "running", "Up 11 minutes (healthy)", "0.0.0.0:5432->5432/tcp", (11, puttyplus::backend::docker::DurUnit::Minute)),
            ("some-mysql", "c439166f7a8b", "mysql:8", "running", "Up 11 minutes (healthy)", "0.0.0.0:3306->3306/tcp, 0.0.0.0:33060->33060/tcp", (11, puttyplus::backend::docker::DurUnit::Minute)),
            ("emqx-enterprise", "b6ddf7f1c9d0", "emqx/emqx-enterprise:latest", "exited", "Exited (0) 9 days ago", "", (9, puttyplus::backend::docker::DurUnit::Day)),
            ("only-created", "deadbeef1234", "nginx:alpine", "created", "Created", "", (3, puttyplus::backend::docker::DurUnit::Minute)),
            ("paused-one", "feedface5678", "redis:7", "paused", "Up 2 hours (Paused)", "6379/tcp", (2, puttyplus::backend::docker::DurUnit::Hour)),
        ] {
            rows.push(puttyplus::backend::docker::ContainerInfo {
                id: idv.into(),
                name: nm.into(),
                image: imgv.into(),
                status: raw.into(),
                state: stv.into(),
                ports: prt.into(),
                compose: String::new(),
                compose_files: String::new(),
                compose_dir: String::new(),
                cpu: "1.0%".into(),
                mem: "10MiB".into(),
                duration: Some(puttyplus::backend::docker::DockerDuration::Num(dur.0, dur.1)),
            });
        }
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
                duration: Some(puttyplus::backend::docker::DockerDuration::Num(
                    1,
                    puttyplus::backend::docker::DurUnit::Hour,
                )),
            });
            dockerui::set_rows_for_test(&panel.st, rows);
            pump(200);
            check!(
                "⑬ 能选中那个不存在的项目",
                dockerui::select_project_for_test(&panel.st, "pp-smoke-nonexistent"),
                "选不中那个项目行"
            );
            pump(200);
            dockerui::dispatch_action_for_test(
                &panel.st,
                "pp-smoke-nonexistent",
                "project",
                "start",
            );
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
        // ⚠️ **只开窗、不起流**（这一组是**纯界面行为** ⇒ 不能真连服务器）。
        dockerui::log_window_show_only_for_test(&panel.st);
        pump(200);
        check!(
            "⑭ 能打开日志窗",
            dockerui::log_page_visible_for_test(&panel.st),
            "开不出来"
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
            while waited2 < 40 && !dockerui::detail_body_visible_for_test(&panel2.st) {
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
                dockerui::detail_body_visible_for_test(&panel2.st),
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
            // ---- ⑰ 之二：**没有日志的容器** ⇒ 状态行要说清楚（阶段 165）----
            //
            // ⚠️ **用户 2026-11-24**：「这个日志窗口我从来没看到日志」。
            //   ⇒ 根因：那个容器**本来就没有日志**（`docker logs` 是空的）
            //   ⇒ 以前界面只写「日志流已结束」⇒ 用户以为是功能坏了。
            //   ⇒ 现在要明说「这个容器没有日志输出」。
            {
                let cs = dockerui::containers_for_test(&panel2.st);
                // 找一个**没有日志**的容器（已停止的多半没有）。
                let silent = cs.iter().find(|c| c.state != "running").map(|c| c.name.clone());
                if let Some(sname) = silent {
                    dockerui::dispatch_action_for_test(
                        &panel2.st,
                        &sname,
                        "container",
                        "logs",
                    );
                    pump(300);
                    // 等流结束（已停止的容器 ⇒ `docker logs -f` 立刻退出）。
                    let mut w = 0;
                    while w < 30 && dockerui::log_running_for_test(&panel2.st) {
                        pump(200);
                        w += 1;
                    }
                    let status = dockerui::log_status_for_test(&panel2.st);
                    println!("[信息] 空日志容器的状态行：{status:?}");
                    check!(
                        "⑰ 之二 没有日志的容器 ⇒ 状态行说清楚「没有日志输出」",
                        status.contains("没有日志输出"),
                        format!("状态行是：{status:?}")
                    );
                    // 关掉日志窗（别留着挡屏幕）。
                    dockerui::log_show_overview_for_test(&panel2.st);
                    pump(200);
                } else {
                    println!("[信息] 没有已停止的容器（跳过空日志检查）");
                }
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
            //
            // ⚠️ **阶段 155**：详情区的「终端」按钮**撤了** ⇒ 入口在
            //   **「操作」列**里 ⇒ 断言改成「那行有颗能点的终端图标」。
            // ⚠️ **阶段 160**：「进容器」搬进**右键菜单**了（操作列只剩启停）。
            let cm0 = dockerui::menu_actions_for_test("container", "running");
            let cmk: Vec<&str> = cm0.iter().map(|(k, _)| k.as_str()).collect();
            check!(
                "⑯ 右键菜单里有「进入容器 / 文件 / 日志 / 属性」",
                cmk.contains(&"enter")
                    && cmk.contains(&"files")
                    && cmk.contains(&"logs")
                    && cmk.contains(&"detail"),
                format!("容器行的菜单是：{cm0:?}")
            );
            // ---- 菜单的图标名**系统主题里真得存在**（写错 ⇒ 缺图占位符）----
            //
            // ⚠️ **阶段 160 踩过**：菜单里用了表格自绘的 `pp-play` / `pp-stop`
            //   ⇒ 主题里没这两个名字 ⇒ 界面上是两块**灰糊**（截图才发现）。
            {
                let theme = gtk::IconTheme::for_display(
                    &gtk::gdk::Display::default().expect("display"),
                );
                let mut bad: Vec<String> = Vec::new();
                for kind in ["container", "project"] {
                    for n in dockerui::menu_action_icons_for_test(kind, "running") {
                        if !theme.has_icon(&n) {
                            bad.push(format!("{kind}:{n}"));
                        }
                    }
                }
                check!(
                    "⑯ 右键菜单的图标名系统主题里都有",
                    bad.is_empty(),
                    format!("主题里没有这些图标：{bad:?}")
                );
            }
            // ---- 右键菜单**真的弹出来**（项也对）⇒ 截图看看 ----
            {
                let ok = dockerui::show_row_menu_for_test(&panel2.st, &name);
                pump(400);
                let items = dockerui::row_menu_items_for_test(&panel2.st);
                println!("[信息] 右键菜单项：{items:?}");
                check!("⑯ 右键菜单弹得出来", ok, "弹不出右键菜单");
                check!(
                    "⑯ 右键菜单里有 进入容器/文件/日志/属性/启动/停止/重启",
                    items.iter().any(|t| t.contains("进入容器"))
                        && items.iter().any(|t| t.contains("文件"))
                        && items.iter().any(|t| t.contains("日志"))
                        && items.iter().any(|t| t.contains("属性"))
                        && items.iter().any(|t| t == "启动")
                        && items.iter().any(|t| t == "停止")
                        && items.iter().any(|t| t == "重启"),
                    format!("菜单项是：{items:?}")
                );
                // ---- 菜单里的启停**看状态**（跟「操作」列一致）----
                //
                // ⚠️ 这一行是**运行中**的容器 ⇒ 「启动」应当灰掉、「停止」能点。
                let start_sensitive =
                    dockerui::row_menu_item_sensitive_for_test(&panel2.st, "启动");
                let stop_sensitive =
                    dockerui::row_menu_item_sensitive_for_test(&panel2.st, "停止");
                check!(
                    "⑯ 运行中的容器：菜单里「启动」是灰的、「停止」能点",
                    start_sensitive == Some(false) && stop_sensitive == Some(true),
                    format!("启动 sensitive={start_sensitive:?} 停止 sensitive={stop_sensitive:?}")
                );
                if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                    shot(&format!("{base}-rowmenu.png"));
                }
                dockerui::close_row_menu_for_test(&panel2.st);
                pump(200);
            }

            // ================= ⑰ 组：阶段 165 —— 概览 / 日志 / 文件 各一扇窗 =================
            //
            // ⚠️ **用户 2026-11-24**：「其他这几个 tab 都单独出来吧，每次点鼠标
            //   右键就只出来一个窗口就行了」⇒ 三块内容**各一扇窗**，同一时刻
            //   **只显示一扇**。
            {
                // ---- 点「日志」⇒ 日志窗开、别的窗关 ----
                dockerui::dispatch_action_for_test(&panel2.st, &name, "container", "logs");
                pump(400);
                check!(
                    "⑰ 点「日志」弹出的是日志窗",
                    dockerui::logs_window_open_for_test(&panel2.st),
                    "日志窗没开"
                );
                check!(
                    "⑰ 同一时刻只出来一个窗口（概览窗关着）",
                    !dockerui::overview_window_open_for_test(&panel2.st),
                    "概览窗还开着"
                );
                // ---- 点「属性」⇒ 概览窗开、日志窗关 ----
                dockerui::dispatch_action_for_test(&panel2.st, &name, "container", "detail");
                pump(400);
                check!(
                    "⑰ 点「属性」弹出的是概览窗",
                    dockerui::overview_window_open_for_test(&panel2.st),
                    "概览窗没开"
                );
                check!(
                    "⑰ 概览窗开了 ⇒ 日志窗自动关掉（一次只出来一个窗口）",
                    !dockerui::logs_window_open_for_test(&panel2.st),
                    "日志窗还开着"
                );
                // ---- 点「文件」⇒ 文件窗开、概览窗关 ----
                dockerui::dispatch_action_for_test(&panel2.st, &name, "container", "files");
                pump(400);
                check!(
                    "⑰ 点「文件」弹出的是文件窗",
                    dockerui::files_window_open_for_test(&panel2.st),
                    "文件窗没开"
                );
                check!(
                    "⑰ 文件窗开了 ⇒ 概览窗自动关掉",
                    !dockerui::overview_window_open_for_test(&panel2.st),
                    "概览窗还开着"
                );
                if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                    shot(&format!("{base}-files.png"));
                }
            }

            // ---- ⑯ 之二：**compose 项目行**的右键菜单 ⇒ 截图看看（阶段 163）----
            //
            // ⚠️ **阶段 163**：「重建」＝ `docker compose up -d --force-recreate`；
            //   「销毁」（`down`）**只在菜单里**（操作列那颗没了）⇒ 菜单是
            //   **唯一**能拆掉整个项目的地方 ⇒ 得看清它长啥样。
            //
            // ⚠️ **菜单只能弹在「已经显示的窗口」上** ⇒ 弹在**没挂到窗口**的
            //   假面板上 ⇒ GTK 会**段错误**（实测：popover 没有有效的
            //   surface）⇒ 这里用**真面板**（`panel2`）。
            {
                let proj = dockerui::model_rows_for_test(&panel2.st)
                    .into_iter()
                    .find(|(_, is_proj, _)| *is_proj)
                    .map(|(n, _, _)| n);
                match proj {
                    Some(pname) => {
                        let ok = dockerui::show_row_menu_for_test(&panel2.st, &pname);
                        pump(400);
                        let items = dockerui::row_menu_items_for_test(&panel2.st);
                        println!("[信息] 项目行「{pname}」的右键菜单项：{items:?}");
                        check!("⑯ 之二 项目行的右键菜单弹得出来", ok, "弹不出项目行的菜单");
                        check!(
                            "⑯ 之二 菜单里有「重建」和「销毁」",
                            items.iter().any(|i| i.contains("重建"))
                                && items.iter().any(|i| i.contains("销毁")),
                            format!("项目行的菜单项是：{items:?}")
                        );
                        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                            shot(&format!("{base}-rowmenu-project.png"));
                        }
                        dockerui::close_row_menu_for_test(&panel2.st);
                        pump(200);
                    }
                    None => {
                        // ⚠️ 远端没有 compose 项目（别的机器可能就是这样）⇒
                        //   **跳过**，不算失败。
                        println!("[信息] 远端没有 compose 项目 ⇒ 跳过项目行菜单的截图");
                    }
                }
            }
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
            // ---- 阶段 117：**真 TTY**（等价于 docker exec -it）----
            //
            // ⚠️ **这三条是「外部可观察量」**（不是我们自己调了几次函数）：
            //   容器里 `tty` 报什么、`stty size` 报多少、`test -t 0` 成不
            //   成 ⇒ 全部由**容器里的程序**回答 ⇒ 骗不了人。
            dockerui::term_send_for_test(
                &panel2.st,
                b"tty; test -t 0 && echo STDIN-TTY; stty size; echo TTYCHK\n",
            );
            let mut waited_tty = 0;
            let mut screen_tty = String::new();
            while waited_tty < 30 {
                pump(200);
                waited_tty += 1;
                screen_tty = dockerui::term_screen_for_test(&panel2.st);
                if screen_tty.contains("TTYCHK") {
                    break;
                }
            }
            println!("[信息] 容器终端屏幕：\n{screen_tty}");
            check!(
                "⑰ 容器里真的有 TTY（tty 报 /dev/pts/…）",
                screen_tty.contains("/dev/pts/"),
                format!("屏幕上没有 /dev/pts/：\n{screen_tty}")
            );
            check!(
                "⑰ 容器的 stdin 也是 TTY（-i 生效）",
                screen_tty.contains("STDIN-TTY"),
                format!("stdin 不是 TTY：\n{screen_tty}")
            );
            // ⚠️ **尺寸**：容器里的 `stty size` 应当**跟着窗口走**
            //   （不是写死的 24 80）⇒ 与界面上的终端尺寸对照。
            let (term_cols, term_rows) = dockerui::term_size_for_test(&panel2.st);
            let stty_line = screen_tty
                .lines()
                .find(|l| {
                    l.split_whitespace().count() == 2
                        && l.split_whitespace().all(|w| w.chars().all(|c| c.is_ascii_digit()))
                })
                .unwrap_or("");
            let mut stty_parts = stty_line.split_whitespace();
            let stty_rows: i64 = stty_parts.next().unwrap_or("").parse().unwrap_or(-1);
            let stty_cols: i64 = stty_parts.next().unwrap_or("").parse().unwrap_or(-1);
            check!(
                "⑰ 容器里的终端尺寸跟界面一致（不是写死的 24×80）",
                stty_rows > 0
                    && stty_cols > 0
                    && stty_rows == term_rows
                    && stty_cols == term_cols,
                format!("容器={stty_rows}行×{stty_cols}列 界面={term_rows}行×{term_cols}列")
            );

            // ---- **Ctrl-C 生效**（真 TTY 的关键差别）----
            //
            // ⚠️ **判据**：跑 `sleep 30` ⇒ 按 Ctrl-C ⇒ **立刻**就该回到
            //   提示符 ⇒ 后面那条 `echo` 马上就能执行 ⇒ 15 秒内看得到。
            //   （Ctrl-C 要是没生效 ⇒ 得等 sleep 跑完 30 秒 ⇒ 看不到。）
            dockerui::term_send_for_test(&panel2.st, b"sleep 30\n");
            pump(800);
            dockerui::term_send_for_test(&panel2.st, b"\x03");
            pump(600);
            dockerui::term_send_for_test(&panel2.st, b"echo AFTER-CTRLC\n");
            let mut waited_cc = 0;
            let mut screen_cc = String::new();
            while waited_cc < 50 {
                pump(300);
                waited_cc += 1;
                screen_cc = dockerui::term_screen_for_test(&panel2.st);
                if screen_cc.contains("AFTER-CTRLC") {
                    break;
                }
            }
            check!(
                "⑰ Ctrl-C 能打断正在跑的命令",
                screen_cc.contains("AFTER-CTRLC"),
                format!("等了 {waited_cc} 次还没回到提示符：\n{screen_cc}")
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
        // ⚠️ **阶段 151：不再有「编辑区」**——编辑器恒为**格子里的页签**
        //   （阶段 149）⇒ 容器页的 compose 文件也是这么打开的（阶段 138：
        //   `open_compose_path` 走 `fileedit::open_remote`）⇒ 用户点完就
        //   看到编辑器，**不需要**「搬到容器页下面」这套。
        //
        // 加一个编辑器 ⇒ 它是**格子里的一个页签**（页签数 +1）。
        let pages_before3 = sub3.page_count();
        let dummy = gtk::Box::new(gtk::Orientation::Vertical, 0);
        //
        // ⚠️ 参数顺序：`id, 模式图标, 位置图标, 标题, 内容, 关闭回调`
        //   （阶段 136 加了「位置图标」⇒ 签名变过，漏改这里 CI 会红）。
        sub3.add_editor(9001, None, None, "冒烟", dummy.upcast(), None);
        pump(200);
        check!(
            "⑰ 编辑器是格子里的一个页签（与容器页并列）",
            sub3.editor_place(9001) == Some(subtabs::EditorPlace::Main),
            format!("编辑器停在 {:?}", sub3.editor_place(9001))
        );
        check!(
            "⑰ 编辑器真的露出来了（页签数 +1）",
            sub3.page_count() == pages_before3 + 1,
            format!("页签数 {} → {}", pages_before3, sub3.page_count())
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
    // ================= M 组：阶段 116 —— 文件浏览 ＋ 上传 ＋ 下载 =================
    //
    // ⚠️ 复用 **F / G / K / L 组那个面板**（panel2）：它已经真连上了 ⇒ 这里
    //   切到「文件」页就能拿到**真数据**（这台机器上真的有容器）。
    {
        let sel = dockerui::selected_for_test(&panel2.st);
        check!(
            "㉑ 有选中的容器（文件页要看它的目录）",
            !sel.is_empty(),
            "没选中容器"
        );
        if sel.is_empty() {
            println!("[信息] 没有选中的容器 ⇒ 跳过文件页的检查");
        } else {
            // ---- 切到「文件」页 ⇒ 列根目录 ----
            dockerui::files_show_for_test(&panel2.st);
            let mut waited = 0;
            while waited < 30 && dockerui::files_rows_for_test(&panel2.st).is_empty() {
                pump(300);
                waited += 1;
            }
            let rows = dockerui::files_rows_for_test(&panel2.st);
            println!(
                "[信息] 容器 {sel} 的根目录：{} 个条目，前几个 {:?}",
                rows.len(),
                &rows.iter().take(3).collect::<Vec<_>>()
            );
            check!(
                "㉑ 文件表有行了",
                !rows.is_empty(),
                format!("等了 {waited} 次还是空的")
            );
            check!(
                "㉑ 文件行的名字与类型不是空的",
                rows.iter().any(|r| !r.0.trim().is_empty() && !r.1.trim().is_empty()),
                format!("{rows:?}")
            );
            // ---- **独立对照**：界面第一行 vs 远端 `ls -A` 的第一行 ----
            //
            // ⚠️ **用 `ls -A`（一行一个名字）**：这样对照的是**名字本身**，
            //   不用跟程序自己的字段解析较劲（那样两边一起错就抵消了）。
            let remote_names = remote_cmd(&opts_f, &format!("docker exec {sel} ls -A -- / 2>/dev/null"));
            let remote_first = remote_names
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .to_string();
            let ui_first = rows.first().map(|r| r.0.clone()).unwrap_or_default();
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-files.png"));
            }
            check!(
                "㉑ 界面第一行与远端 ls -A 的第一个条目对得上",
                !remote_first.is_empty() && ui_first == remote_first,
                format!("界面={ui_first:?} 远端={remote_first:?}（原文={remote_names:?}）")
            );

            // ---- 阶段 140：文件页瘦身（图标 / 权限不翻译 / 工具条一行）----
            {
                // ⚠️ **图标**：目录 / 文件 / 链接各有各的图标（用户 2026-11-19：
                //   「文件、目录之类的要有图标吧，不然看不出来是啥」）。
                let icons = dockerui::files_icons_for_test(&panel2.st);
                check!(
                    "㉒ 每一行都有图标名",
                    !icons.is_empty() && icons.iter().all(|i| !i.trim().is_empty()),
                    format!("{icons:?}")
                );
                check!(
                    "㉒ 图标名是 Adwaita 真有的那三个",
                    icons.iter().all(|i| {
                        i.as_str() == "folder-symbolic"
                            || i.as_str() == "text-x-generic-symbolic"
                            || i.as_str() == "insert-link-symbolic"
                    }),
                    format!("{icons:?}")
                );
                // ⚠️ **权限用通用写法**（`drwxr-xr-x`）⇒ 不许出现中文。
                check!(
                    "㉒ 权限列是 drwxr-xr-x 这种通用写法",
                    rows.iter().all(|r| {
                        let p = r.3.trim();
                        p.is_empty()
                            || ((p.starts_with('d') || p.starts_with('-') || p.starts_with('l'))
                                && p.len() >= 10)
                    }),
                    format!("{rows:?}")
                );
                // ⚠️ **工具条**：一行 ＋ **纯图标**（没有文字）＋ 每个都有 tooltip。
                let tb = dockerui::files_toolbar_for_test(&panel2.st);
                // ⚠️ **阶段 167：6 个 ⇒ 5 个**（用户 2026-11-25「就去掉『回到
                //   上一级目录』吧，因为已经有『..』完成相同的功能了」）。
                // ⚠️ **阶段 191：5 个 ⇒ 3 个**（用户 2026-11-28「这个按钮不需
                //   要这么多，刷新，上传，下载就够了」）⇒ 去掉「上传整个目录」
                //   （与主文件面板同一口径）与「查看内容」（**双击行照样能看**）。
                check!(
                    "㉒ 工具条是 3 个纯图标按钮（没有文字）",
                    tb.len() == 3 && tb.iter().all(|(_, has_label)| !has_label),
                    format!("{tb:?}")
                );
                check!(
                    "㉒ 每个按钮都有 tooltip",
                    tb.iter().all(|(tip, _)| !tip.trim().is_empty()),
                    format!("{tb:?}")
                );
                // ⚠️ **阶段 167：图标要「一眼看得出意思」** ⇒ 不用上下箭头
                //   （↑↓ 本来就不对称 ⇒ 谁也猜不出谁是谁）⇒ 用环形箭头 /
                //   纸飞机 / 文件夹+加号 / 文件夹+下箭头 / 放大镜。
                let tips: Vec<String> = tb.iter().map(|(t, _)| t.clone()).collect();
                check!(
                    "㉒ 工具条没有「回到上一级目录」了（有『..』行顶替）",
                    !tips.iter().any(|t| t.contains("上一级")),
                    format!("{tips:?}")
                );
                check!(
                    "㉒ 工具条图标是语义化的那几颗（刷新/上传/下载）",
                    tb.len() == 3,
                    format!("{tb:?}")
                );
                // ⚠️ **阶段 191：「查看内容」那颗去掉了** ⇒ 双击行才是看内容的
                //   正路（`connect_row_activated` ⇒ `files_view`）。
                check!(
                    "㉒ 工具条上没有「查看内容」了（双击行照样能看）",
                    !tips.iter().any(|t| t.contains("查看")),
                    format!("{tips:?}")
                );
                // ⚠️ **没有「类型」列**（图标已经表达了 ⇒ 省下的宽度给文件名）。
                let cols = dockerui::files_cols_for_test(&panel2.st);
                let vis: Vec<&str> = cols
                    .iter()
                    .filter(|(_, v)| *v)
                    .map(|(t, _)| t.as_str())
                    .collect();
                check!(
                    "㉒ 文件表没有「类型」列",
                    !vis.contains(&"类型"),
                    format!("{cols:?}")
                );
                // ⚠️ **阶段 185：4 列 ⇒ 6 列**（用户 2026-11-27：「容器文件管理中
                //   没有显示修改时间」「最好能显示符号链接指向的文件」）⇒ 加了
                //   「修改时间」「指向」两列 ⇒ 这条断言是**过期**的。
                check!(
                    "㉒ 文件表的可见列是 图标/名称/大小/权限/修改时间/指向（6 列）",
                    vis.len() == 6,
                    format!("{cols:?}")
                );
            }

            // ---- **真上传**：本地造文件 ⇒ 传到容器的 /tmp ⇒ 远端一字不差 ----
            let tmp_dir = std::env::temp_dir().join(format!("pp-116-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp_dir);
            let _ = std::fs::create_dir_all(&tmp_dir);
            let local_file = tmp_dir.join("pp-upload-116.txt");
            let content = "puttyplus-stage-116-upload-check";
            std::fs::write(&local_file, content).expect("造本地文件");
            // 传到容器的 **/tmp**（先切目录；别往容器根目录扔东西）。
            dockerui::files_goto_for_test(&panel2.st, "/tmp");
            let mut waited2 = 0;
            while waited2 < 20 && dockerui::files_path_for_test(&panel2.st) != "/tmp" {
                pump(200);
                waited2 += 1;
            }
            check!(
                "㉑ 切到容器的 /tmp 了",
                dockerui::files_path_for_test(&panel2.st) == "/tmp",
                format!("当前目录={:?}", dockerui::files_path_for_test(&panel2.st))
            );
            // ⚠️ **用计数器等**（状态行会被列表刷新冲掉 ⇒ 靠不住）。
            let seq0 = dockerui::files_xfer_done_for_test(&panel2.st);
            dockerui::files_upload_for_test(&panel2.st, &local_file);
            let mut waited3 = 0;
            while waited3 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq0 {
                pump(300);
                waited3 += 1;
            }
            let up_status = dockerui::files_xfer_status_for_test(&panel2.st);
            check!(
                "㉑ 上传完成了",
                up_status.contains("上传完成"),
                format!("状态行={up_status:?}（等了 {waited3} 次）")
            );
            // ---- **独立对照**：远端真的收到这个文件，内容一字不差 ----
            let remote_cat = remote_cmd(
                &opts_f,
                &format!("docker exec {sel} cat /tmp/pp-upload-116.txt 2>/dev/null"),
            );
            check!(
                "㉑ 远端容器里的文件内容与本地一字不差",
                remote_cat.trim() == content,
                format!("远端={remote_cat:?} 本地={content:?}")
            );
            // ---- 传完之后**列表里要能看到它**（刷新逻辑）----
            let mut waited4 = 0;
            while waited4 < 20
                && !dockerui::files_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == "pp-upload-116.txt")
            {
                pump(300);
                waited4 += 1;
            }
            check!(
                "㉑ 上传完的文件出现在文件表里",
                dockerui::files_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == "pp-upload-116.txt"),
                format!("文件表={:?}", dockerui::files_rows_for_test(&panel2.st))
            );

            // ---- **真下载**：把刚传上去的文件拉回本地 ⇒ 内容要对 ----
            let dl_dir = tmp_dir.join("dl");
            let _ = std::fs::create_dir_all(&dl_dir);
            let seq1 = dockerui::files_xfer_done_for_test(&panel2.st);
            dockerui::files_download_for_test(&panel2.st, "/tmp/pp-upload-116.txt", &dl_dir);
            let mut waited5 = 0;
            while waited5 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq1 {
                pump(300);
                waited5 += 1;
            }
            let dl_status = dockerui::files_xfer_status_for_test(&panel2.st);
            check!(
                "㉑ 下载完成了",
                dl_status.contains("下载完成"),
                format!("状态行={dl_status:?}（等了 {waited5} 次）")
            );
            let got = std::fs::read_to_string(dl_dir.join("pp-upload-116.txt")).unwrap_or_default();
            check!(
                "㉑ 下载回来的文件内容与传上去的一字不差",
                got.trim() == content,
                format!("下载回来的={got:?} 原件={content:?}")
            );

            // ================= 上传选择窗（阶段 192） =================
            //
            // ⚠️ 用户 2026-11-28：「**上传这个目录和上传文件不能用同一个按钮
            //   吗？选择文件或目录，确定，然后上传？**」
            // ⇒ 这段验**整条链**：一颗「上传」按钮 ⇒ 弹出自绘窗口 ⇒
            //   **文件与目录都列出来** ⇒ 勾上就亮「上传」⇒ 确定 ⇒ 二次确认
            //   ⇒ **真传上去**（远端能查到内容）。
            {
                // 本地造一个目录：一个**文件** ＋ 一个**子目录**（目录里再放一个
                // 文件 ⇒ 传完能查内容）。
                let pick_root = tmp_dir.join("pp-pick-root-192");
                let _ = std::fs::remove_dir_all(&pick_root);
                let pick_dir = pick_root.join("pp-pick-dir-192");
                std::fs::create_dir_all(&pick_dir).expect("造子目录");
                std::fs::write(pick_dir.join("inner.txt"), "pick-inner").expect("造目录里的文件");
                std::fs::write(pick_root.join("pp-pick-file-192.txt"), "pick-file").expect("造文件");

                // ---- 点「上传」那颗 ⇒ 选择窗打开 ----
                let clicked = dockerui::files_toolbar_click_for_test(
                    &panel2.st,
                    "把本地的文件或目录上传到容器当前目录",
                );
                check!(
                    "㉒ 点「上传」弹出了选择窗（就那一颗按钮）",
                    clicked,
                    "工具条上找不到那颗「上传」"
                );
                pump(300);
                check!(
                    "㉒ 选择窗开着",
                    dockerui::upload_pick_open_for_test(&panel2.st),
                    "选择窗没打开"
                );
                // ⚠️ **一个都没勾** ⇒ 「上传」按钮**灰着**（别让人点了个空）。
                check!(
                    "㉒ 没勾东西时「上传」是灰的",
                    !dockerui::upload_pick_upload_sensitive_for_test(&panel2.st),
                    "没勾东西也能点上传"
                );

                // ---- 停到刚造好的目录（原生「浏览…」自动化点不了 ⇒ 用钩子）----
                check!(
                    "㉒ 选择窗能停到指定的本地目录",
                    dockerui::upload_pick_set_dir_for_test(&panel2.st, &pick_root),
                    "换目录失败"
                );
                pump(200);
                let names = dockerui::upload_pick_entries_for_test(&panel2.st);
                check!(
                    "㉒ 选择窗里**文件和目录都列出来了**",
                    names.iter().any(|n| n == "pp-pick-file-192.txt")
                        && names.iter().any(|n| n == "pp-pick-dir-192"),
                    format!("列出来的={names:?}")
                );

                // ---- 勾一个**目录**（用户要的就是「传目录」）⇒ 「上传」变亮 ----
                let idx = names
                    .iter()
                    .position(|n| n == "pp-pick-dir-192")
                    .unwrap_or(0);
                check!(
                    "㉒ 能勾选一个目录",
                    dockerui::upload_pick_check_for_test(&panel2.st, idx, true),
                    "勾不上"
                );
                pump(200);
                check!(
                    "㉒ 勾上以后「上传」能点了",
                    dockerui::upload_pick_upload_sensitive_for_test(&panel2.st),
                    "勾了还是灰的"
                );
                // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张「上传内容」窗口。
                if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                    pump(300);
                    shot(&format!("{base}-upload-pick.png"));
                }

                // ---- 确定 ⇒ 二次确认 ⇒ 真传 ----
                let seq6 = dockerui::files_xfer_done_for_test(&panel2.st);
                let before = toplevel_count();
                dockerui::upload_pick_submit_for_test(&panel2.st);
                pump(400);
                check!(
                    "㉒ 点「上传」弹出二次确认框（写操作）",
                    toplevel_count() > before,
                    format!("顶层窗口数 {before} → {}（没弹出确认框）", toplevel_count())
                );
                confirm_last_dialog();
                let mut waited10 = 0;
                while waited10 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq6 {
                    pump(300);
                    waited10 += 1;
                }
                let pick_status = dockerui::files_xfer_status_for_test(&panel2.st);
                check!(
                    "㉒ 选择窗里勾的东西上传完成了",
                    pick_status.contains("上传完成"),
                    format!("状态行={pick_status:?}")
                );
                // 远端：那个**目录**（连同里面的文件）搬过去了
                let remote_pick = remote_cmd(
                    &opts_f,
                    &format!("docker exec {sel} cat /tmp/pp-pick-dir-192/inner.txt 2>/dev/null"),
                );
                check!(
                    "㉒ 勾上传上去的**目录**在容器里，内容也对",
                    remote_pick.trim() == "pick-inner",
                    format!("远端={remote_pick:?}")
                );
            }

            // ================= 二进制往返（用户 2026-11-16 提问） =================
            //
            // ⚠️ **这条是「二进制能不能传」的证据**：文件里塞满 0x00~0xFF
            //   ⇒ 上传再下载 ⇒ **字节完全一样**才算过。
            {
                let bin_file = tmp_dir.join("pp-bin-116.bin");
                let mut bytes: Vec<u8> = Vec::with_capacity(8192);
                for i in 0..8192u32 {
                    bytes.push((i % 256) as u8);
                }
                std::fs::write(&bin_file, &bytes).expect("造二进制文件");
                let seq2 = dockerui::files_xfer_done_for_test(&panel2.st);
                dockerui::files_upload_for_test(&panel2.st, &bin_file);
                let mut waited6 = 0;
                while waited6 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq2 {
                    pump(300);
                    waited6 += 1;
                }
                let bin_status = dockerui::files_xfer_status_for_test(&panel2.st);
                check!(
                    "㉑ 二进制文件上传完成了",
                    bin_status.contains("上传完成"),
                    format!("状态行={bin_status:?}")
                );
                // 远端大小对得上（**独立对照**）
                let remote_size = remote_cmd(
                    &opts_f,
                    // ⚠️ **不能写 `< /tmp/...`**：那个重定向是**服务器**的 shell
                    //   做的（读服务器的文件，不是容器里的）⇒ 必须让容器里的
                    //   wc 自己读参数。
                    &format!("docker exec {sel} wc -c /tmp/pp-bin-116.bin 2>/dev/null"),
                );
                // ⚠️ **wc 的输出是「大小 文件名」** ⇒ 只取**第一个字段**。
                let remote_bytes = remote_size.split_whitespace().next().unwrap_or("");
                check!(
                    "㉑ 远端二进制文件大小与本地一致",
                    remote_bytes == bytes.len().to_string(),
                    format!("远端={remote_size:?} 本地={}", bytes.len())
                );
                // 下载回来 ⇒ 字节完全一样
                let bin_dl = tmp_dir.join("bindl");
                let _ = std::fs::create_dir_all(&bin_dl);
                let seq3 = dockerui::files_xfer_done_for_test(&panel2.st);
                dockerui::files_download_for_test(&panel2.st, "/tmp/pp-bin-116.bin", &bin_dl);
                let mut waited7 = 0;
                while waited7 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq3 {
                    pump(300);
                    waited7 += 1;
                }
                let back = std::fs::read(bin_dl.join("pp-bin-116.bin")).unwrap_or_default();
                check!(
                    "㉑ 二进制文件下载回来与原件字节完全一样",
                    back == bytes,
                    format!("原件 {} 字节，回来 {} 字节（首字节 {:?}）", bytes.len(), back.len(), back.first())
                );
            }

            // ================= 目录往返 =================
            //
            // ⚠️ **目录走 `docker cp`** ⇒ 容器里**不需要有 tar** ⇒ distroless
            //   镜像也能用 ⇒ 这条验的就是「目录结构原样搬过去」。
            {
                let dir_src = tmp_dir.join("pp-dir-116");
                let _ = std::fs::create_dir_all(dir_src.join("sub"));
                std::fs::write(dir_src.join("sub").join("inner.txt"), "inner-content")
                    .expect("造目录里的文件");
                let seq4 = dockerui::files_xfer_done_for_test(&panel2.st);
                dockerui::files_upload_for_test(&panel2.st, &dir_src);
                let mut waited8 = 0;
                while waited8 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq4 {
                    pump(300);
                    waited8 += 1;
                }
                let dir_status = dockerui::files_xfer_status_for_test(&panel2.st);
                check!(
                    "㉑ 目录上传完成了",
                    dir_status.contains("上传完成"),
                    format!("状态行={dir_status:?}")
                );
                // 远端：子目录里的文件在不在、内容对不对
                let remote_inner = remote_cmd(
                    &opts_f,
                    &format!(
                        "docker exec {sel} cat /tmp/pp-dir-116/sub/inner.txt 2>/dev/null"
                    ),
                );
                check!(
                    "㉑ 目录上传后容器里的子文件内容对",
                    remote_inner.trim() == "inner-content",
                    format!("远端={remote_inner:?}")
                );
                // 下载回来 ⇒ 结构与内容都对
                let dir_dl = tmp_dir.join("dirdl");
                let _ = std::fs::create_dir_all(&dir_dl);
                let seq5 = dockerui::files_xfer_done_for_test(&panel2.st);
                dockerui::files_download_for_test(&panel2.st, "/tmp/pp-dir-116", &dir_dl);
                let mut waited9 = 0;
                while waited9 < 60 && dockerui::files_xfer_done_for_test(&panel2.st) == seq5 {
                    pump(300);
                    waited9 += 1;
                }
                let back_inner = std::fs::read_to_string(
                    dir_dl.join("pp-dir-116").join("sub").join("inner.txt"),
                )
                .unwrap_or_default();
                check!(
                    "㉑ 目录下载回来结构与内容都对",
                    back_inner.trim() == "inner-content",
                    format!("下载回来的={back_inner:?}（目录={:?}）", dir_dl)
                );
            }

            // ---- 收尾：把测试文件从容器里删掉（别留垃圾）----
            let _ = remote_cmd(
                &opts_f,
                &format!(
                    "docker exec {sel} sh -c 'rm -f /tmp/pp-upload-116.txt /tmp/pp-bin-116.bin; rm -rf /tmp/pp-dir-116' 2>/dev/null"
                ),
            );
            let _ = std::fs::remove_dir_all(&tmp_dir);
        }
    }
    // ================= N 组：阶段 117/155 —— 入口好找 ＋ 交互像 docker exec -it =================
    //
    // ⚠️ 用户 2026-11-16：「界面里没看到在哪里打开容器shell」⇒ 这组专测
    //   **入口能不能被找到**。
    //
    // ⚠️ **阶段 155**：工具条上那个「进入容器」按钮**撤了** ⇒ 入口搬进
    //   **「操作」列**（每行一颗终端图标）⇒ **不用先选中容器**就能直接进
    //   那一行 ⇒ 断言也跟着改成「操作」列的形态。
    {
        // ---- 「操作」列里有颗终端图标（进容器）----
        {
            let cs0 = dockerui::containers_for_test(&panel2.st);
            let c0 = cs0
                .iter()
                .find(|c| c.state == "running")
                .or_else(|| cs0.first())
                .cloned();
            if let Some(c0) = c0 {
                // ⚠️ **阶段 160**：终端图标**搬进右键菜单**了 ⇒ 验菜单。
                let cm = dockerui::menu_actions_for_test("container", "running");
                let cmk: Vec<&str> = cm.iter().map(|(k, _)| k.as_str()).collect();
                check!(
                    "㉒ 容器行的右键菜单里有「进入容器」",
                    cmk.contains(&"enter"),
                    format!("容器 {} 的菜单是：{cm:?}", c0.name)
                );
                check!(
                    "㉒ 容器行的右键菜单里有「文件 / 日志 / 属性」",
                    cmk.contains(&"files") && cmk.contains(&"logs") && cmk.contains(&"detail"),
                    format!("容器 {} 的菜单是：{cm:?}", c0.name)
                );
            }
        }
        // ---- **点那颗终端图标 ⇒ 进 shell** ----
        //
        // ⚠️ **这条是补上一个漏洞**：光断言「图标在不在」抓不住「点了没
        //   反应」（反向验证时退掉回调，断言照样绿）⇒ 必须真的点一下。
        {
            let cs0 = dockerui::containers_for_test(&panel2.st);
            let c0 = cs0
                .iter()
                .find(|c| c.state == "running")
                .or_else(|| cs0.first())
                .cloned();
            if let Some(c0) = c0 {
                dockerui::stop_terminal_for_test(&panel2.st);
                pump(300);
                // ⚠️ **不用先选中容器**（这正是搬进「操作」列的好处）。
                // ⚠️ **阶段 160**：「进容器」在右键菜单里 ⇒ 走**同一个派发**
                //   （`dispatch_action_for_test` 现在也找菜单里的动作）。
                dockerui::dispatch_action_for_test(&panel2.st, &c0.name, "container", "enter");
                let mut waited = 0;
                while waited < 40 && !dockerui::term_running_for_test(&panel2.st) {
                    pump(200);
                    waited += 1;
                }
                check!(
                    "㉒ 点「进容器」能进容器 shell",
                    dockerui::term_running_for_test(&panel2.st),
                    format!("等了 {waited} 次还没起来")
                );
                dockerui::stop_terminal_for_test(&panel2.st);
                pump(500);
            }
        }

        // ---- **双击顶层容器行 ⇒ 进 shell** ----
        //
        // ⚠️ 原来顶层容器行双击**什么也不发生** ⇒ 少一个入口。
        let cs = dockerui::containers_for_test(&panel2.st);
        let c0 = cs
            .iter()
            .find(|c| c.state == "running")
            .or_else(|| cs.first())
            .cloned();
        if let Some(c0) = c0 {
            dockerui::stop_terminal_for_test(&panel2.st);
            pump(300);
            dockerui::double_click_container_for_test(&panel2.st, &c0.name);
            let mut waited = 0;
            while waited < 40 && !dockerui::term_running_for_test(&panel2.st) {
                pump(200);
                waited += 1;
            }
            check!(
                "㉒ 双击容器行能进容器 shell",
                dockerui::term_running_for_test(&panel2.st),
                format!("等了 {waited} 次还没起来")
            );
            // 进去之后敲一条命令 ⇒ 真的在容器里跑（**端到端**）。
            dockerui::term_send_for_test(&panel2.st, b"echo DOUBLE-CLICK-OK\n");
            let mut waited2 = 0;
            let mut screen = String::new();
            while waited2 < 30 {
                pump(200);
                waited2 += 1;
                screen = dockerui::term_screen_for_test(&panel2.st);
                if screen.contains("DOUBLE-CLICK-OK") {
                    break;
                }
            }
            check!(
                "㉒ 双击进去后命令真的在容器里跑",
                screen.contains("DOUBLE-CLICK-OK"),
                format!("屏幕上没有回显：\n{screen}")
            );
            dockerui::stop_terminal_for_test(&panel2.st);
            pump(500);
        }
    }

    // ================= O 组：阶段 119 —— 容器终端的工具条 =================
    //
    // ⚠️ **对标 dbx**：容器终端还差「切 shell / 重连 / 清屏 / 搜索输出」
    //   四样 ⇒ 这一组专测它们。
    //
    // ⚠️ **断言全落在「动作的结果」上**（不是「控件的属性」）：
    //   - 切 shell ⇒ 容器里 `echo $0` 的回显要变成那个 shell；
    //   - 换不存在的 shell ⇒ **中文报错** ＋ **正在用的终端不受影响**；
    //   - 重连 ⇒ 命令还能跑 ＋ 远端**只留一个** docker exec；
    //   - 清屏 ⇒ 屏幕上的字真的没了；
    //   - 搜索 ⇒ 真的能找到屏幕上的历史输出。
    {
        let cs = dockerui::containers_for_test(&panel2.st);
        let c0 = cs
            .iter()
            .find(|c| c.state == "running")
            .or_else(|| cs.first())
            .cloned();
        if let Some(c0) = c0 {
            let name = c0.name.clone();

            // ---- 先探容器里**到底哪些 shell 能用**（⇒ 挑「有的」和「没有的」）----
            //
            // ⚠️ **为什么要探**：断言「切到 /bin/bash」得先知道容器里**有**
            //   bash；断言「切到不存在的 shell 会报错」也得先知道哪个不存在
            //   ⇒ 拿 `verify_shell`（阶段 119 新增的后端 API）探。
            let probe = {
                let keyfile_p = keyfile_f.clone();
                let user_p = user.clone();
                let name_p = name.clone();
                hub::runtime().block_on(async move {
                    let creds = hub::Creds {
                        keyfile: Some(keyfile_p),
                        ..Default::default()
                    };
                    let key = hub::ServerKey::new(HOST, port, &user_p);
                    let Ok(shared) = hub::acquire(key, &creds).await else {
                        return (None, None);
                    };
                    let mut have: Option<String> = None;
                    let mut missing: Option<String> = None;
                    for sh in docker::SHELL_CANDIDATES {
                        // ⚠️ `/bin/sh` 跳过（它几乎一定在 ⇒ 探了也是白探）。
                        if sh == "/bin/sh" {
                            continue;
                        }
                        match docker::verify_shell(shared.session().as_ref(), &name_p, sh).await {
                            Ok(_) => {
                                if have.is_none() {
                                    have = Some(sh.to_string());
                                }
                            }
                            Err(_) => {
                                if missing.is_none() {
                                    missing = Some(sh.to_string());
                                }
                            }
                        }
                    }
                    (have, missing)
                })
            };
            let (have_shell, missing_shell) = probe;
            println!(
                "[信息] 容器 {name} 里能用的 shell={have_shell:?}、不能用的={missing_shell:?}"
            );

            // ---- 进容器（走「操作」列里的终端图标 ⇒ 顺带验阶段 155 的入口）----
            dockerui::stop_terminal_for_test(&panel2.st);
            pump(300);
            dockerui::click_row_action_for_test(&panel2.st, &name, "enter");
            let mut waited = 0;
            while waited < 40 && !dockerui::term_running_for_test(&panel2.st) {
                pump(200);
                waited += 1;
            }
            check!(
                "㉓ 进容器后终端跑起来了",
                dockerui::term_running_for_test(&panel2.st),
                format!("等了 {waited} 次还没起来")
            );

            // ---- **工具条在**（切 shell / 重连 / 清屏 / 搜索）----
            let (opts, bar_visible, active) = dockerui::term_toolbar_for_test(&panel2.st);
            check!("㉓ 容器终端有工具条", bar_visible, "工具条不可见");
            check!(
                "㉓ 工具条上有 shell 下拉（自动 ＋ 四个候选）",
                opts.len() == 5
                    && opts[0] == "自动"
                    && opts.contains(&"/bin/bash".to_string())
                    && opts.contains(&"/bin/zsh".to_string()),
                format!("下拉内容={opts:?}")
            );
            check!(
                "㉓ 下拉默认是「自动」",
                active == "自动",
                format!("当前选中={active:?}")
            );

            // ---- **搜索**：屏幕上有历史输出 ⇒ 搜得到 ----
            dockerui::term_send_for_test(&panel2.st, b"echo SEARCHMARK-119\n");
            let mut w = 0;
            let mut screen = String::new();
            while w < 30 {
                pump(200);
                w += 1;
                screen = dockerui::term_screen_for_test(&panel2.st);
                if screen.contains("SEARCHMARK-119") {
                    break;
                }
            }
            check!(
                "㉓ 搜索前屏幕上有那行输出",
                screen.contains("SEARCHMARK-119"),
                format!("屏幕上没有：\n{screen}")
            );
            let found = dockerui::term_search_for_test(&panel2.st, "SEARCHMARK-119");
            check!("㉓ 能在容器终端的输出里搜到", found, "搜索没命中");
            // ⚠️ **搜索条能开**（点「搜索」⇒ 搜索条出来）。
            let bar_on = dockerui::term_search_toggle_for_test(&panel2.st);
            check!("㉓ 点「搜索」能打开搜索条", bar_on, "搜索条没打开");
            dockerui::term_search_toggle_for_test(&panel2.st); // 再点一下关掉

            // ---- **清屏**：屏幕上的字真的没了 ----
            dockerui::term_clear_for_test(&panel2.st);
            pump(500);
            let screen2 = dockerui::term_screen_for_test(&panel2.st);
            check!(
                "㉓ 点「清屏」后屏幕真的空了",
                !screen2.contains("SEARCHMARK-119"),
                format!("清屏后还在：\n{screen2}")
            );

            // ---- **重连**：同一个 shell 重开 ⇒ 命令照跑 ----
            let rc0 = dockerui::term_reconnects_for_test(&panel2.st);
            dockerui::term_reconnect_for_test(&panel2.st);
            let mut w2 = 0;
            while w2 < 40 && dockerui::term_reconnects_for_test(&panel2.st) == rc0 {
                pump(200);
                w2 += 1;
            }
            check!(
                "㉓ 点「重连」真的重连了",
                dockerui::term_reconnects_for_test(&panel2.st) > rc0,
                format!("等了 {w2} 次重连次数还是 {rc0}")
            );
            dockerui::term_send_for_test(&panel2.st, b"echo RECONNECT-OK-119\n");
            let mut w3 = 0;
            let mut screen3 = String::new();
            while w3 < 30 {
                pump(200);
                w3 += 1;
                screen3 = dockerui::term_screen_for_test(&panel2.st);
                if screen3.contains("RECONNECT-OK-119") {
                    break;
                }
            }
            check!(
                "㉓ 重连后命令还能跑",
                screen3.contains("RECONNECT-OK-119"),
                format!("屏幕上没有：\n{screen3}")
            );
            check!(
                "㉓ 重连后窗口还是那个（没另开一个）",
                dockerui::term_window_open_for_test(&panel2.st),
                "终端窗口不见了"
            );

            // ---- **换不存在的 shell** ⇒ 中文报错 ＋ **正在用的终端不受影响** ----
            if let Some(bad) = missing_shell.clone() {
                let idx = docker::SHELL_CANDIDATES
                    .iter()
                    .position(|s| *s == bad)
                    .map(|i| i as u32 + 1)
                    .unwrap_or(1);
                dockerui::term_select_shell_for_test(&panel2.st, idx);
                pump(300);
                let rc1 = dockerui::term_reconnects_for_test(&panel2.st);
                dockerui::term_reconnect_for_test(&panel2.st);
                let mut w4 = 0;
                while w4 < 30 {
                    pump(200);
                    w4 += 1;
                    if dockerui::term_status_for_test(&panel2.st).contains("没有") {
                        break;
                    }
                }
                let st_msg = dockerui::term_status_for_test(&panel2.st);
                check!(
                    "㉓ 换一个容器里没有的 shell 会给出中文报错",
                    st_msg.contains("没有") && st_msg.contains(&bad),
                    format!("状态栏={st_msg:?}")
                );
                check!(
                    "㉓ 换 shell 失败不会弄死正在用的终端",
                    dockerui::term_running_for_test(&panel2.st)
                        && dockerui::term_reconnects_for_test(&panel2.st) == rc1,
                    "终端被弄死了"
                );
                // ⚠️ **旧终端还得能用**（不是「还开着」而已）。
                dockerui::term_send_for_test(&panel2.st, b"echo STILL-ALIVE-119\n");
                let mut w5 = 0;
                let mut screen5 = String::new();
                while w5 < 30 {
                    pump(200);
                    w5 += 1;
                    screen5 = dockerui::term_screen_for_test(&panel2.st);
                    if screen5.contains("STILL-ALIVE-119") {
                        break;
                    }
                }
                check!(
                    "㉓ 换 shell 失败后旧终端还能用",
                    screen5.contains("STILL-ALIVE-119"),
                    format!("屏幕上没有：\n{screen5}")
                );
            }

            // ---- **换成真有的 shell** ⇒ `echo $0` 要变成它 ----
            if let Some(good) = have_shell.clone() {
                let idx = docker::SHELL_CANDIDATES
                    .iter()
                    .position(|s| *s == good)
                    .map(|i| i as u32 + 1)
                    .unwrap_or(1);
                dockerui::term_select_shell_for_test(&panel2.st, idx);
                pump(300);
                let rc2 = dockerui::term_reconnects_for_test(&panel2.st);
                dockerui::term_reconnect_for_test(&panel2.st);
                let mut w6 = 0;
                while w6 < 40 && dockerui::term_reconnects_for_test(&panel2.st) == rc2 {
                    pump(200);
                    w6 += 1;
                }
                check!(
                    "㉓ 换 shell 后重连次数加了",
                    dockerui::term_reconnects_for_test(&panel2.st) > rc2,
                    format!("等了 {w6} 次还是 {rc2}")
                );
                check!(
                    "㉓ 换完之后用的就是那个 shell",
                    dockerui::term_shell_for_test(&panel2.st) == good,
                    format!("现在用的是 {:?}", dockerui::term_shell_for_test(&panel2.st))
                );
                // ⚠️ **由容器里的程序回答**（`echo $0`）⇒ 骗不了人。
                dockerui::term_send_for_test(&panel2.st, b"echo $0\n");
                let mut w7 = 0;
                let mut screen7 = String::new();
                while w7 < 30 {
                    pump(200);
                    w7 += 1;
                    screen7 = dockerui::term_screen_for_test(&panel2.st);
                    if screen7.contains("echo $0") {
                        break;
                    }
                }
                check!(
                    "㉓ 容器里的 $0 就是选的那个 shell",
                    screen7.contains(&good),
                    format!("屏幕上没有 {good}：\n{screen7}")
                );
            }

            // ---- 重连后远端**只留一个** docker exec（旧的被收掉了）----
            let mut w8 = 0;
            let mut procs = String::new();
            while w8 < 20 {
                procs = remote_procs(&opts_f, "docker exec");
                let n = procs.lines().filter(|l| l.contains(&name)).count();
                if n <= 1 {
                    break;
                }
                pump(300);
                w8 += 1;
            }
            let n_exec = procs.lines().filter(|l| l.contains(&name)).count();
            check!(
                "㉓ 重连后远端只留一个 docker exec（旧的收掉了）",
                n_exec == 1,
                format!("远端有 {n_exec} 个：\n{procs}")
            );

            // 想看图？`PP_DOCKER_SHOT=/tmp/docker` ⇒ 截一张带工具条的容器终端。
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-term-tools.png"));
            }

            dockerui::stop_terminal_for_test(&panel2.st);
            pump(400);
        }
    }

    // ================= P 组：阶段 120 —— 镜像的操作（推送 / 导出 / 打标签 / 删除） =================
    //
    // ⚠️ **断言全落在「动作的结果」上**：
    //   - 打标签 ⇒ 镜像表里**真的多了一行**；
    //   - 删除 ⇒ 那一行**真的没了**；
    //   - 导出 ⇒ 本地**真的落下一个 tar**（校验 tar 的魔数）；
    //   - 推送失败 ⇒ **给出中文原因**（不是一句「失败」）。
    {
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_IMAGES);
        let mut waited = 0;
        while waited < 30 && dockerui::images_rows_for_test(&panel2.st).is_empty() {
            pump(200);
            waited += 1;
        }
        let imgs = dockerui::images_rows_for_test(&panel2.st);
        check!(
            "㉔ 镜像表有行了",
            !imgs.is_empty(),
            format!("共 {} 行", imgs.len())
        );

        // ---- **先清掉上一轮遗留的测试标签**（自愈）----
        //
        // ⚠️ **为什么要有**：上一次跑挂了（断言红）⇒ 标签留在远端 ⇒
        //   这一轮镜像表第一行就是它 ⇒ 「界面第一行跟远端原文对得上」
        //   这类断言**莫名红** ⇒ 一次失败会**毒掉后面所有轮**（实测踩过）。
        // ⇒ 开跑前先把同名标签删掉。
        {
            let rows = dockerui::images_rows_for_test(&panel2.st);
            if let Some(i) = rows.iter().position(|r| r.0 == "pp-test-tag-120") {
                dockerui::images_select_for_test(&panel2.st, i as u32);
                dockerui::images_remove_for_test(&panel2.st);
                let mut w0 = 0;
                while w0 < 40
                    && dockerui::images_rows_for_test(&panel2.st)
                        .iter()
                        .any(|r| r.0 == "pp-test-tag-120")
                {
                    pump(200);
                    w0 += 1;
                }
                println!("[信息] 清掉了上一轮遗留的 pp-test-tag-120");
            }
        }

        // ---- **操作条在**（四个按钮）----
        let (labels, sensitive, sel0) = dockerui::images_actions_for_test(&panel2.st);
        check!(
            "㉔ 镜像段有操作条（推送 / 导出 / 打标签 / 删除）",
            labels == vec!["推送", "导出…", "打标签…", "删除"],
            format!("按钮={labels:?}")
        );
        check!(
            "㉔ 没选镜像时按钮是灰的",
            !sensitive && sel0.is_empty(),
            format!("可点={sensitive} 选中={sel0:?}")
        );

        // ---- **选中一行** ⇒ 按钮变可点 ----
        //
        // ⚠️ **挑一个真有标签的行**（悬空镜像是 `<none>:<none>` ⇒ 没有
        //   「仓库:标签」可看）。
        let rows = dockerui::images_rows_for_test(&panel2.st);
        let good = rows
            .iter()
            .position(|r| !r.1.is_empty() && r.1 != "<none>" && !r.0.is_empty());
        if let Some(good) = good {
            let picked = dockerui::images_select_for_test(&panel2.st, good as u32);
            let (_, sensitive2, sel2) = dockerui::images_actions_for_test(&panel2.st);
            check!(
                "㉔ 选中镜像后按钮能点了",
                sensitive2 && !sel2.is_empty(),
                format!("可点={sensitive2} 选中={sel2:?}")
            );
            check!(
                "㉔ 选中的镜像名是「仓库:标签」",
                sel2.contains(':') && sel2 == picked,
                format!("选中={sel2:?} 返回={picked:?}")
            );

            // ---- **打标签** ⇒ 镜像表里真的多了一行 ----
            let tag_name = "pp-test-tag-120";
            dockerui::images_tag_for_test(&panel2.st, tag_name);
            let mut w2 = 0;
            let mut tagged = false;
            while w2 < 50 {
                pump(200);
                w2 += 1;
                if dockerui::images_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == tag_name)
                {
                    tagged = true;
                    break;
                }
            }
            check!(
                "㉔ 打标签后镜像表里多了那一行",
                tagged,
                "镜像表里没有 pp-test-tag-120"
            );
            check!(
                "㉔ 打标签的结果写进了状态行",
                dockerui::status_for_test(&panel2.st).contains("标签"),
                format!("状态行={:?}", dockerui::status_for_test(&panel2.st))
            );

            // ---- **删除** ⇒ 那一行真的没了 ----
            let idx_tag = dockerui::images_rows_for_test(&panel2.st)
                .iter()
                .position(|r| r.0 == tag_name)
                .unwrap_or(0) as u32;
            dockerui::images_select_for_test(&panel2.st, idx_tag);
            let sel3 = dockerui::images_actions_for_test(&panel2.st).2;
            check!(
                "㉔ 选中了刚打的那个标签",
                sel3.starts_with(tag_name),
                format!("选中={sel3:?}")
            );
            dockerui::images_remove_for_test(&panel2.st);
            let mut w3 = 0;
            let mut gone = false;
            while w3 < 50 {
                pump(200);
                w3 += 1;
                if !dockerui::images_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == tag_name)
                {
                    gone = true;
                    break;
                }
            }
            check!("㉔ 删除后那一行没了", gone, "镜像表里还有 pp-test-tag-120");
        }

        // ---- **导出** ⇒ 本地真的落下一个 tar ----
        //
        // ⚠️ **挑最小的那个镜像**（导出的是整个镜像 ⇒ 别导 1GB 的）。
        let rows = dockerui::images_rows_for_test(&panel2.st);
        let mut best: Option<(usize, u64)> = None;
        for (i, r) in rows.iter().enumerate() {
            if r.1.is_empty() || r.1 == "<none>" {
                continue;
            }
            if let Some(sz) = docker::parse_size_bytes(&r.3) {
                if best.map(|(_, b)| sz < b).unwrap_or(true) {
                    best = Some((i, sz));
                }
            }
        }
        if let Some((i, _)) = best {
            dockerui::images_select_for_test(&panel2.st, i as u32);
            let out = std::path::Path::new("/tmp/pp-img-120.tar");
            let _ = std::fs::remove_file(out);
            dockerui::images_export_for_test(&panel2.st, out);
            // ⚠️ **等文件大小稳住**（还在写的时候大小会变）。
            let mut w4 = 0;
            let mut last_size = 0u64;
            let mut stable = 0;
            while w4 < 80 {
                pump(300);
                w4 += 1;
                let sz = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
                if sz > 0 && sz == last_size {
                    stable += 1;
                    if stable >= 3 {
                        break;
                    }
                } else {
                    stable = 0;
                }
                last_size = sz;
            }
            let sz = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
            check!("㉔ 导出后本地真的有文件", sz > 0, format!("大小={sz}"));
            // ⚠️ **是 tar 吗**：tar 的魔数在第 257 字节（"ustar"）。
            let magic_ok = std::fs::read(out)
                .map(|b| {
                    b.len() > 262 && &b[257..262] == b"ustar"
                })
                .unwrap_or(false);
            check!("㉔ 导出的文件是 tar 归档", magic_ok, "第 257 字节不是 ustar");
            let _ = std::fs::remove_file(out);
        } else {
            println!("[信息] 没有能解析大小的镜像 ⇒ 跳过导出这条");
        }

        // ---- **推送失败** ⇒ 给出中文原因 ----
        //
        // ⚠️ **用不存在的镜像名**（不能真往公共仓库推东西 ⇒ 后果不可逆）。
        dockerui::images_push_named_for_test(&panel2.st, "pp-nonexistent-image-120");
        let mut w5 = 0;
        let mut msg = String::new();
        while w5 < 50 {
            pump(200);
            w5 += 1;
            msg = dockerui::status_for_test(&panel2.st);
            if msg.contains("推送") && msg.contains("失败") {
                break;
            }
        }
        check!(
            "㉔ 推送失败会给出中文原因",
            msg.contains("推送") && msg.contains("失败") && msg.len() > 8,
            format!("状态行={msg:?}")
        );
    }
    // ================= Q 组：阶段 166 —— 删卷 / 删网络 =================
    //
    // ⚠️ **只删自己造出来的那个卷**（`pp-smoke-vol-166`）⇒ 不碰用户真数据；
    //   删**不存在的名字** ⇒ 验「失败要说清原因」。
    {
        // ---- 卷页 ----
        // ⚠️ **先关掉详情窗**（概览 / 日志 / 文件）⇒ 它们是独立窗口 ⇒
        //   不关就会盖在容器页上面 ⇒ 截图拍不到卷页。
        dockerui::close_detail_windows_for_test(&panel2.st);
        pump(300);
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_VOLUMES);
        let mut waited = 0;
        while waited < 30 && dockerui::volumes_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited += 1;
        }
        // ---- 操作条在（一个「删除」按钮）----
        let (vlabel, vsens, vsel, vhint) = dockerui::volumes_actions_for_test(&panel2.st);
        check!(
            "㉕ 卷页有「删除」按钮",
            vlabel == "删除",
            format!("按钮={vlabel:?}")
        );
        check!(
            "㉕ 没选卷时删除按钮是灰的",
            !vsens && vsel.is_empty(),
            format!("可点={vsens} 选中={vsel:?}")
        );
        check!(
            "㉕ 没选卷时提示说「选中一个卷才能删除」",
            vhint.contains("选中一个卷"),
            format!("提示={vhint:?}")
        );
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            // ⚠️ **先 pump 一下**（GTK 的重绘是异步的 ⇒ 不 pump 就截 ⇒
            //   截到的是**上一帧**）。
            pump(500);
            shot(&format!("{base}-volumes.png"));
        }

        // ---- **造一个测试卷** ⇒ 走完整的删除流程 ----
        let vol = "pp-smoke-vol-166";
        let created = remote_cmd(&opts_f, &format!("docker volume create {vol}"));
        // ⚠️ **造不出来就跳过**（别把环境问题算成程序问题）。
        if !created.contains(vol) {
            println!("[信息] 远端造不出测试卷（{created:?}）⇒ 跳过删除往返这一段");
        } else {
            // 等它进表
            dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_VOLUMES);
            let mut w1 = 0;
            while w1 < 40
                && !dockerui::volumes_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == vol)
            {
                pump(300);
                w1 += 1;
            }
            check!(
                "㉕ 测试卷进了卷表",
                dockerui::volumes_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == vol),
                format!("卷表={:?}", dockerui::volumes_rows_for_test(&panel2.st))
            );
            // ---- 选中它 ⇒ 按钮能点 ----
            let picked = dockerui::select_volume_for_test(&panel2.st, vol);
            let (_, vsens2, vsel2, vhint2) = dockerui::volumes_actions_for_test(&panel2.st);
            check!(
                "㉕ 选中卷后删除按钮能点了",
                picked && vsens2 && vsel2 == vol,
                format!("选中={vsel2:?} 可点={vsens2} 提示={vhint2:?}")
            );
            // ---- 删掉它 ⇒ 表里没了 ＋ 远端也没了 ----
            dockerui::volumes_remove_named_for_test(&panel2.st, vol);
            let mut w2 = 0;
            let mut gone = false;
            while w2 < 50 {
                pump(300);
                w2 += 1;
                if !dockerui::volumes_rows_for_test(&panel2.st)
                    .iter()
                    .any(|r| r.0 == vol)
                {
                    gone = true;
                    break;
                }
            }
            check!("㉕ 删除后卷表里那一行没了", gone, "卷表里还有那个测试卷");
            // ⚠️ **远端也要真没了**（别只信界面）。
            let left = remote_cmd(&opts_f, &format!("docker volume ls -q | grep -c '^{vol}$'"));
            check!(
                "㉕ 远端那个卷真的没了",
                left.trim() == "0",
                format!("远端还剩：{left:?}")
            );
            // ---- 结果写进了状态行 ----
            let mut w3 = 0;
            let mut vmsg = String::new();
            while w3 < 30 {
                pump(200);
                w3 += 1;
                vmsg = dockerui::status_for_test(&panel2.st);
                if vmsg.contains("已删除") {
                    break;
                }
            }
            check!(
                "㉕ 删除结果写进了状态行",
                vmsg.contains("已删除"),
                format!("状态行={vmsg:?}")
            );
        }

        // ---- **删不存在的卷** ⇒ 失败要说清原因 ----
        dockerui::volumes_remove_named_for_test(&panel2.st, "pp-nonexistent-volume-166");
        let mut w4 = 0;
        let mut verr = String::new();
        while w4 < 50 {
            pump(200);
            w4 += 1;
            verr = dockerui::status_for_test(&panel2.st);
            if verr.contains("失败") {
                break;
            }
        }
        check!(
            "㉕ 删不存在的卷会给出中文原因",
            verr.contains("删除卷")
                && verr.contains("失败")
                && verr.len() > 8
                // ⚠️ **不能只剩「exit status 1」那种水话**（Docker 29 会多打
                //   一行）⇒ 得带上 docker 真正的报错。
                && !verr.contains("exit status"),
            format!("状态行={verr:?}")
        );
        println!("[信息] 删不存在的卷 ⇒ 状态行：{verr:?}");

        // ---- 网络页 ----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_NETWORKS);
        let mut w5 = 0;
        while w5 < 30 && dockerui::networks_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            w5 += 1;
        }
        let (nlabel, nsens, nsel, nhint) = dockerui::networks_actions_for_test(&panel2.st);
        check!(
            "㉕ 网络页有「删除」按钮",
            nlabel == "删除",
            format!("按钮={nlabel:?}")
        );
        check!(
            "㉕ 没选网络时删除按钮是灰的",
            !nsens && nsel.is_empty(),
            format!("可点={nsens} 选中={nsel:?}")
        );
        check!(
            "㉕ 没选网络时提示说「选中一个网络才能删除」",
            nhint.contains("选中一个网络"),
            format!("提示={nhint:?}")
        );
        // ---- **默认网络（bridge）** ⇒ 按钮要灰 ＋ 说清为什么 ----
        let nets = dockerui::networks_rows_for_test(&panel2.st);
        if nets.iter().any(|r| r.0 == "bridge") {
            dockerui::select_network_for_test(&panel2.st, "bridge");
            pump(200);
            let (_, bsens, bsel, bhint) = dockerui::networks_actions_for_test(&panel2.st);
            check!(
                "㉕ 默认网络 bridge 的删除按钮是灰的",
                !bsens && bsel == "bridge",
                format!("可点={bsens} 选中={bsel:?}")
            );
            check!(
                "㉕ 默认网络要说明为什么删不掉",
                bhint.contains("默认网络"),
                format!("提示={bhint:?}")
            );
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                // ⚠️ **先 pump 一下**（GTK 的重绘是异步的 ⇒ 不 pump 就截 ⇒
                //   截到的是**上一帧**）。
                pump(500);
                shot(&format!("{base}-networks-default.png"));
            }
        } else {
            println!("[信息] 没有 bridge 网络 ⇒ 跳过默认网络这一段");
        }
        // ---- **普通网络** ⇒ 按钮能点 ----
        let normal = nets
            .iter()
            .find(|r| r.0 != "bridge" && r.0 != "host" && r.0 != "none");
        if let Some(r) = normal {
            dockerui::select_network_for_test(&panel2.st, &r.0);
            pump(200);
            let (_, osens, osel, _) = dockerui::networks_actions_for_test(&panel2.st);
            check!(
                "㉕ 普通网络的删除按钮能点",
                osens && osel == r.0,
                format!("可点={osens} 选中={osel:?}")
            );
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                // ⚠️ **先 pump 一下**（GTK 的重绘是异步的）。
                pump(500);
                shot(&format!("{base}-networks-picked.png"));
            }
        } else {
            println!("[信息] 这台机器上只有默认网络 ⇒ 跳过普通网络这一段");
        }
        // ---- **删不存在的网络** ⇒ 失败要说清原因 ----
        dockerui::networks_remove_named_for_test(&panel2.st, "pp-nonexistent-net-166");
        let mut w6 = 0;
        let mut nerr = String::new();
        while w6 < 50 {
            pump(200);
            w6 += 1;
            nerr = dockerui::status_for_test(&panel2.st);
            if nerr.contains("失败") {
                break;
            }
        }
        check!(
            "㉕ 删不存在的网络会给出中文原因",
            nerr.contains("删除网络")
                && nerr.contains("失败")
                && nerr.len() > 8
                // ⚠️ **不能只剩「exit status 1」那种水话**（Docker 29 会多打
                //   一行）⇒ 得带上 docker 真正的报错。
                && !nerr.contains("exit status"),
            format!("状态行={nerr:?}")
        );
        println!("[信息] 删不存在的网络 ⇒ 状态行：{nerr:?}");
        if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
            shot(&format!("{base}-networks-del.png"));
        }
    }
    // ================= R 组：阶段 166 —— 刷新别把「选中」顶掉 =================
    //
    // ⚠️ **这条锁的是一个真 bug**：重建表格要 `store.clear()` ⇒ GTK 会**逐行**
    //   发 `cursor-changed` ⇒ 「程序记着的选中」被清空 ⇒ 自动刷新（每 7 秒）
    //   一来，删除按钮就变灰 ⇒ **用户根本点不到**。
    {
        // ---- 卷页 ----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_VOLUMES);
        let mut waited = 0;
        while waited < 30 && dockerui::volumes_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited += 1;
        }
        let vols = dockerui::volumes_rows_for_test(&panel2.st);
        if let Some(v) = vols.iter().find(|r| !r.0.trim().is_empty()) {
            let name = v.0.clone();
            dockerui::select_volume_for_test(&panel2.st, &name);
            pump(150);
            let (_, s1, sel1, _) = dockerui::volumes_actions_for_test(&panel2.st);
            check!(
                "㉖ 选中卷后删除按钮是亮的",
                s1 && sel1 == name,
                format!("可点={s1} 选中={sel1:?}")
            );
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                // ⚠️ **先 pump 一下**（GTK 的重绘是异步的）。
                pump(500);
                shot(&format!("{base}-volumes-picked.png"));
            }
            // ⚠️ **触发一次刷新**（切分段 ⇒ 重新取数 ⇒ 重建表格）。
            dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_VOLUMES);
            // 等刷新**真的回来**（状态行从「正在读取…」变回「共 N 个卷」）。
            let mut w = 0;
            while w < 40 {
                pump(200);
                w += 1;
                if dockerui::status_for_test(&panel2.st).contains("个卷") {
                    break;
                }
            }
            let (_, s2, sel2, _) = dockerui::volumes_actions_for_test(&panel2.st);
            check!(
                "㉖ 刷新后卷的选中还在（按钮不会变灰）",
                s2 && sel2 == name,
                format!("可点={s2} 选中={sel2:?} 状态行={:?}", dockerui::status_for_test(&panel2.st))
            );
        } else {
            println!("[信息] 这台机器上没有卷 ⇒ 跳过卷的刷新保选中这一段");
        }

        // ---- 镜像页**也一样**（阶段 120 的老坑，顺手一起验）----
        dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_IMAGES);
        let mut waited2 = 0;
        while waited2 < 30 && dockerui::images_rows_for_test(&panel2.st).is_empty() {
            pump(300);
            waited2 += 1;
        }
        let imgs = dockerui::images_rows_for_test(&panel2.st);
        if let Some(i) = imgs
            .iter()
            .position(|r| !r.0.trim().is_empty() && !r.1.is_empty() && r.1 != "<none>")
        {
            let picked = dockerui::images_select_for_test(&panel2.st, i as u32);
            pump(150);
            let (_, s1, sel1) = dockerui::images_actions_for_test(&panel2.st);
            check!(
                "㉖ 选中镜像后按钮是亮的",
                s1 && sel1 == picked && !picked.is_empty(),
                format!("可点={s1} 选中={sel1:?}")
            );
            dockerui::seg_show_for_test(&panel2.st, dockerui::SEG_IMAGES);
            let mut w2 = 0;
            while w2 < 40 {
                pump(200);
                w2 += 1;
                if dockerui::status_for_test(&panel2.st).contains("个镜像") {
                    break;
                }
            }
            let (_, s2, sel2) = dockerui::images_actions_for_test(&panel2.st);
            check!(
                "㉖ 刷新后镜像的选中还在（按钮不会变灰）",
                s2 && sel2 == picked,
                format!("可点={s2} 选中={sel2:?} 状态行={:?}", dockerui::status_for_test(&panel2.st))
            );
        } else {
            println!("[信息] 没有能选的镜像 ⇒ 跳过镜像的刷新保选中这一段");
        }
    }

    // ================= S 组：阶段 167 —— 三页布局 ＋ 文件窗高度 ＋ 日志下载 =================
    //
    // ⚠️ **这一组守的是「用户 2026-11-25 报的三件事」**：
    //   ① 镜像 / 卷 / 网络三页**搜索框吃满富余宽度**、**表格没铺满**；
    //   ② 「查看容器里面的文件**只显示1行**」；
    //   ③ 日志窗**没有下载按钮**。
    {
        // ---- ① 三页：搜索框不吸富余 + 表格有展开列（⇒ 富余被摊开）----
        for (seg, label) in [
            (dockerui::SEG_IMAGES, "镜像页"),
            (dockerui::SEG_VOLUMES, "卷页"),
            (dockerui::SEG_NETWORKS, "网络页"),
        ] {
            let se = dockerui::seg_search_expands_for_test(&panel2.st, seg);
            check!(
                "㉗ 搜索框不吃满富余宽度",
                se.len() == 1 && !se[0],
                format!("{label} 搜索框 hexpand={se:?}")
            );
            let ex = dockerui::seg_table_expands_for_test(&panel2.st, seg);
            // ⚠️ **GTK4 的规矩**：**一列都不展开 ⇒ 富余全给最后一列**
            //   ⇒ 页面右边出现一大片空谷（用户 2026-11-25 截图里就是这样）
            //   ⇒ 断言「**至少两列**展开」⇒ 富余被摊开 ⇒ 没有空谷。
            check!(
                "㉗ 表格有多列展开（富余被摊开，不留空谷）",
                ex.len() >= 2 && ex.iter().filter(|b| **b).count() >= 2,
                format!("{label} 展开列={ex:?}")
            );
        }

        // ---- ② 「查看文件」窗：正文要**占满**（不是只有 1 行）----
        {
            let content = (1..=80)
                .map(|i| format!("第 {i} 行"))
                .collect::<Vec<_>>()
                .join("\n");
            dockerui::file_view_show_for_test(&panel2.st, "/etc/bashrc", &content);
            pump(700);
            if let Ok(base) = std::env::var("PP_DOCKER_SHOT") {
                shot(&format!("{base}-fileview.png"));
            }
            let size = dockerui::file_view_window_size_for_test(&panel2.st);
            match size {
                Some((text_h, win_h)) => {
                    println!("[信息] 查看文件窗：正文 {text_h}px / 窗口 {win_h}px");
                    // ⚠️ **核心断言**：正文高度必须**远大于一行**（≈20px）
                    //   ⇒ 否则就是用户报的「只显示1行」。
                    check!(
                        "㉗ 查看文件窗的正文占满了（不是只显示1行）",
                        text_h > 60,
                        format!("正文 {text_h}px / 窗口 {win_h}px")
                    );
                    check!(
                        "㉗ 正文区比窗口矮不了太多（不是缩在顶上）",
                        win_h > 0 && text_h * 2 >= win_h,
                        format!("正文 {text_h}px / 窗口 {win_h}px")
                    );
                }
                None => check!(
                    "㉗ 查看文件窗开得起来",
                    false,
                    "开了「查看文件」却拿不到句柄"
                ),
            }
        }

        // ---- ③ 日志窗：工具条上有「下载…」----
        {
            let (labels, has_save) = dockerui::log_toolbar_for_test(&panel2.st);
            println!("[信息] 日志工具条：{labels:?}");
            check!(
                "㉗ 日志窗有「下载…」按钮",
                has_save,
                format!("{labels:?}")
            );
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
