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
use puttyplus::backend::docker;
use puttyplus::backend::hub;
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
    }

    let n_fail = FAILS.with(|f| f.borrow().len());
    if n_fail == 0 {
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
