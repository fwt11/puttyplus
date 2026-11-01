//! 阶段 55 冒烟：**ssh config 别名的连接也能「保存到连接配置」**。
//!
//! **用户场景**（2026-10-29）：用户连的是 `~/.ssh/config` 里的别名
//! （`Host srv-01`），在端口转发窗口点「保存到连接配置」弹「保存失败」——
//! 之前那个功能**只认连接管理器里自建的条目**，别名连接根本存不了规则。
//! 而堡垒机场景平时连的就都是别名 → 功能等于废的。
//!
//! 本冒烟查三件事：
//! 1. 别名连接点**真按钮**（`emit_clicked`，跟用户点的是同一条路径）→
//!    规则写进 `connections.ini` 的 `[ssh_alias_forwards]` 段（键 = 别名）；
//! 2. 下次从这个别名连接时，规则**自动带上**（`load_all` → `to_opts`）；
//! 3. **不碰用户真实配置**：HOME 与 ini 全部指到临时目录，退出时删掉。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fwd_save_alias_smoke
//!
//! 退出码 0 = 全过；非 0 = 有检查没过（stdout 里逐条打印）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::worker::FwdView;
use puttyplus::panel::{self, SftpPanel};

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

/// 退出时把临时文件删掉（别往用户配置里塞东西）。
struct Cleanup(Vec<std::path::PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 假的别名（名字取得很偏，避免跟用户真配置里的别名撞上；
/// IP 用 TEST-NET-3 保留段 203.0.113.0/24，不指向任何真机器）。
const ALIAS: &str = "pp-smoke-alias";
const ALIAS_IP: &str = "203.0.113.9";

fn main() {
    // ⚠️ **HOME 必须在任何 glib 调用之前改**：glib 的 g_get_home_dir()
    // 第一次调用就把结果缓存了，之后再改 HOME 不生效。
    // 清单里的别名从 HOME/.ssh/config 读——所以这里换成一个假 HOME。
    let home = std::env::temp_dir().join(format!("pp-fwd-alias-smoke-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(
        home.join(".ssh").join("config"),
        format!("Host {ALIAS}\n  HostName {ALIAS_IP}\n  User tester\n"),
    )
    .unwrap();
    unsafe { std::env::set_var("HOME", &home) };

    // 连接条目也指到临时文件
    let conn_ini = std::env::temp_dir().join(format!("pp-fwd-alias-smoke-{}.ini", std::process::id()));
    let settings_ini =
        std::env::temp_dir().join(format!("pp-fwd-alias-smoke-set-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&conn_ini);
    let _ = std::fs::remove_file(&settings_ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini);
    }
    let _cleanup = Cleanup(vec![conn_ini.clone(), settings_ini.clone()]);

    let app = gtk::Application::builder()
        .application_id("x.y.PpFwdSaveAlias")
        .build();
    let conn_ini2 = conn_ini.clone();
    app.connect_activate(move |app| {
        let conn_ini = conn_ini2.clone();
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 前置检查：假 HOME 生效了吗（别名能从 ~/.ssh/config 读出来）
        let entries = puttyplus::connmgr::load_all();
        let has_alias = entries.iter().any(|e| e.name == ALIAS);
        assert!(has_alias, "假 HOME 没生效：别名 {ALIAS} 没读出来");
        println!("✓ 假 HOME 生效，别名 {ALIAS} 已读入");

        // 会话参数 = 连上之后的形态（主机已展开成真实地址）
        let mut opts = ConnOptions::default();
        opts.host = ALIAS_IP.into();
        opts.user = "tester".into();
        opts.port = 22;
        opts.title = Some(ALIAS.into());
        let panel = SftpPanel::new(&opts);
        let st = &panel.st;

        // 灌一条假规则（按钮读的就是这份列表）
        *st.fwd_views.borrow_mut() = vec![FwdView {
            key: "L:9000:10.0.0.1:80".into(),
            text: "本地 127.0.0.1:9000 → 10.0.0.1:80".into(),
            enabled: true,
            active: true,
            err: None,
        }];

        panel::on_open_forwards(st);
        pump(400);

        // ⚠️ 点**真按钮**：emit_clicked 走的就是用户点一下那条路径
        let btn = puttyplus::panel::fwdmgr::save_button(st).expect("转发窗口里得有保存按钮");
        btn.emit_clicked();
        pump(600);

        // 断言 1：规则写进了 ini 的 [ssh_alias_forwards] 段
        let text = std::fs::read_to_string(&conn_ini).unwrap_or_default();
        println!("connections.ini：\n{text}");
        assert!(
            text.contains("[ssh_alias_forwards]"),
            "别名规则要存在单独一段里：{text}"
        );
        assert!(
            text.contains(&format!("{ALIAS}=")),
            "键应该是别名本身：{text}"
        );
        assert!(
            text.contains("L:9000:10.0.0.1:80"),
            "规则内容要写对：{text}"
        );
        println!("✓ 规则已写进 [ssh_alias_forwards] 段");

        // 断言 2：下次从这个别名连接时规则自动带上
        let entries = puttyplus::connmgr::load_all();
        let e = entries
            .iter()
            .find(|e| e.name == ALIAS)
            .expect("别名条目要在列表里");
        assert_eq!(e.inproc_forwards.len(), 1, "{:?}", e.inproc_forwards);
        assert_eq!(e.inproc_forwards[0].kind, 'L');
        let o = e.to_opts();
        assert_eq!(o.inproc_forwards, vec!["L:9000:10.0.0.1:80"]);
        println!("✓ 下次从该别名连接时，规则自动带上");

        // ---- 断言 3：列表为空时**不许悄悄抹掉**已存的规则 ----
        // 连接没建立时列表本来就是空的（规则加不进去，worker 回「未连接」），
        // 用户这时点「保存」不该把之前存的规则清掉——要清也得先问一句。
        st.fwd_views.borrow_mut().clear();
        let btn = puttyplus::panel::fwdmgr::save_button(st).expect("得有保存按钮");
        btn.emit_clicked();
        pump(600);
        // 应该弹出一个确认框（标题「请确认」）
        // ⚠️ `app.windows()` 只列**应用窗口**（GtkApplicationWindow），
        // 普通 gtk::Dialog 不在里面 → 要用 gtk::Window::list_toplevels()
        let dlg = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("请确认"));
        assert!(dlg.is_some(), "列表为空时应该先问要不要清空");
        println!("✓ 列表为空时弹出确认框（没有直接清空）");
        // 选「取消」→ 已存的规则必须还在
        if let Some(d) = dlg {
            // Window → Dialog 是「父 → 子」，downcast 方向要对
            if let Ok(dlg) = d.downcast::<gtk::Dialog>() {
                dlg.response(gtk::ResponseType::Cancel);
            }
        }
        pump(400);
        let text2 = std::fs::read_to_string(&conn_ini).unwrap_or_default();
        assert!(
            text2.contains("L:9000:10.0.0.1:80"),
            "取消之后不该动已存的规则：{text2}"
        );
        println!("✓ 取消后已存规则原样保留");

        println!("[OK] 别名连接也能保存转发规则");
        app.quit();
    });
    app.run();
}
