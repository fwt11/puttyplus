//! 阶段 53 冒烟：**端口转发窗口的三种类型**（本地 -L / 远程 -R / 动态 -D）。
//!
//! **不连服务器**——纯界面验证，所以远端连不上也能跑。
//!
//! 验四件事：
//! 1. 窗口能开起来，类型下拉里**三种类型都在**；
//! 2. 切换类型 → 字段页跟着换（是换页，不是一堆 sensitive 开关）；
//! 3. 列表里三种规则的描述**分得清**（本地/远程/SOCKS 不能长得一样）；
//! 4. 截图存到 /tmp/fwd_types.png（给人眼看）。
//!
//! ⚠️ 列表里灌的是**假视图**（直接填 fwd_views），走的是窗口自己的
//! 渲染逻辑。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fwd_ui_smoke

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

/// 退出时把临时 ini 删掉（别往用户配置里塞东西）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpFwdTypes")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类只有 theme::apply() 之后才生效
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // ⚠️ 别碰用户的真实设置：窗口尺寸会往 settings.ini 里写
        let settings_ini = std::env::temp_dir().join(format!(
            "pp-fwd-types-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&settings_ini);
        unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };
        let _settings_guard = IniGuard(settings_ini.clone());

        let panel = SftpPanel::new(&ConnOptions::default());
        let st = &panel.st;

        // ---- 灌三条假规则（三种类型各一条）----
        *st.fwd_views.borrow_mut() = vec![
            FwdView {
                key: "L:9000:10.0.0.1:80".into(),
                text: "本地 127.0.0.1:9000 → 10.0.0.1:80".into(),
                enabled: true,
                active: true,
                err: None,
            },
            FwdView {
                key: "R:2222:10.0.0.2:22".into(),
                text: "远端 127.0.0.1:2222 → 10.0.0.2:22".into(),
                enabled: true,
                active: true,
                err: None,
            },
            FwdView {
                key: "D:1080".into(),
                text: "SOCKS 127.0.0.1:1080".into(),
                enabled: true,
                active: false,
                err: Some("端口已被占用".into()),
            },
        ];

        panel::on_open_forwards(st);
        pump(400);

        let probe = puttyplus::panel::fwdmgr::probe(st);
        println!("探针：{probe:?}");

        // ---- 断言 1+2：三种类型都在，且切类型 → 换页 ----
        // （set_active_id 对不存在的 id 是空操作，所以"切得动"就证明它在）
        for kind in ["L", "R", "D"] {
            puttyplus::panel::fwdmgr::set_kind(st, kind);
            pump(120);
            let p = puttyplus::panel::fwdmgr::probe(st);
            assert!(
                p.iter().any(|x| x == &format!("kind={kind}")),
                "类型 {kind} 应能选中：{p:?}"
            );
            assert!(
                p.iter().any(|x| x == &format!("page={kind}")),
                "切到 {kind} 后字段页没换：{p:?}"
            );
        }
        // 回到本地（截图好看）
        puttyplus::panel::fwdmgr::set_kind(st, "L");
        pump(120);

        // ---- 断言 3：三条规则的描述分得清 ----
        let rows: Vec<&String> = probe.iter().filter(|p| p.starts_with("row=")).collect();
        assert_eq!(rows.len(), 3, "列表应有三行：{rows:?}");
        assert!(
            rows[0].contains("本地") && rows[1].contains("远端") && rows[2].contains("SOCKS"),
            "三种类型的描述要分得清：{rows:?}"
        );

        // ---- 截图 ----
        let shot = std::env::var("PP_FWD_SHOT").unwrap_or_else(|_| "/tmp/fwd_types.png".into());
        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg(&shot)
            .status();
        println!("截图：{shot}");

        println!("[OK] 转发窗口三种类型");
        app.quit();
    });
    app.run();
}
