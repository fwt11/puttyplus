//! 标签状态**聚合**的冒烟（审计 P0-4，阶段 65）。
//!
//! **问题**：标签上的状态点只反映**文件面板**（SFTP）的状态——终端会话
//! 已经结束了（用户敲了 exit、网络断了），点还是**绿的**，用户以为会话还
//! 活着，点进去才发现是死画面。
//!
//! **断言**：
//! 1. 文件面板正常 + 终端活着 ⇒ 绿点；
//! 2. **终端结束了 ⇒ 不再是绿点**（黄点）；
//! 3. tooltip 说清**哪一部分**出的问题（含「终端」）；
//! 4. 终端还没上报（未知）⇒ 不谎报绿点、也不报故障；
//! 5. **真实路径**：`Tabs::open` 建出来的终端，起来要报「运行中」、
//!    掉了要报「已结束」（见 [`real_path_case`] 的说明）。
//!
//! ```text
//! xvfb-run -a cargo run --example state_agg_smoke
//! ```

use gtk::prelude::*;
use puttyplus::active::TermState;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel};
use puttyplus::theme::{self, State, Theme};

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

/// 等某个条件成立（最多 timeout 毫秒）。
fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);

    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("state agg smoke"));
    win.set_default_size(800, 500);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(200);

    let st = &panel.st;

    // ---- 文件面板"已连接"（不真的连）----
    panel::set_status_for_smoke(st, State::Ok, "已连接");
    pump(100);

    // ---- 1) 终端还没上报 ⇒ 绿点 + tooltip 说"未知" ----
    {
        let (level, tip) = puttyplus::tabs::dot_state_for(st);
        assert_eq!(level, Some(State::Ok), "终端未知时不该报故障：{tip}");
        assert!(tip.contains("终端：未知"), "{tip}");
        assert!(tip.contains("文件面板：已连接"), "{tip}");
        println!("终端未知：{tip} ✓");
    }

    // ---- 2) 终端活着 ⇒ 还是绿点 ----
    panel::note_term_alive(st, true);
    pump(80);
    {
        let (level, tip) = puttyplus::tabs::dot_state_for(st);
        assert_eq!(level, Some(State::Ok), "终端活着就该是绿点：{tip}");
        assert!(tip.contains("终端：运行中"), "{tip}");
        println!("终端活着：{tip} ✓");
    }

    // ---- 3) 终端结束 ⇒ **不再是绿点** ----
    panel::note_term_alive(st, false);
    pump(80);
    {
        let (level, tip) = puttyplus::tabs::dot_state_for(st);
        assert_ne!(
            level,
            Some(State::Ok),
            "终端都结束了还是绿点（审计 P0-4 就是这个）：{tip}"
        );
        assert_eq!(level, Some(State::Warn), "终端掉了应当是黄点：{tip}");
        assert!(tip.contains("终端：已结束"), "tooltip 要说清哪一部分坏了：{tip}");
        println!("终端结束：{tip} ✓");
    }

    // ---- 4) GTK 层面：状态点标签真的换了颜色 + tooltip ----
    {
        let dot = gtk::Label::new(None);
        puttyplus::tabs::apply_dot(&dot, st);
        pump(50);
        // ⚠️ 读 **label 属性**（set_markup 写进去的就是带颜色的 markup），
        // `text()` 只有「●」没有颜色
        let markup = dot.label().to_string();
        let green = theme::state_hex(State::Ok);
        let yellow = theme::state_hex(State::Warn);
        assert!(
            !markup.contains(&green),
            "状态点还是绿的（{markup}）"
        );
        assert!(markup.contains(&yellow), "状态点不是黄点：{markup}");
        let tip = dot.tooltip_text().map(|t| t.to_string()).unwrap_or_default();
        assert!(tip.contains("终端"), "状态点的 tooltip 没提终端：{tip}");
        println!("状态点：{markup} / {tip} ✓");
    }

    println!("state_agg_smoke: OK");

    // 真实路径（走生产代码：Tabs::open → 装存活钩子）
    real_path_case();

    println!("state_agg_smoke: OK");
}

/// **真实路径**的回归（阶段 65 踩到的坑）。
///
/// ⚠️ 上面那几段是**直接调** `note_term_alive` 改面板状态——绕开了
/// 「终端起来 / 结束 → 通知上层」这条链。真实链路上有个坑：
///
/// - `TermPane::new` 里就**同步** spawn 了子进程；
/// - 存活钩子是**构造完之后**才装的（`tabs.rs::install_alive_hook`）；
///
/// ⇒ 启动那次的「存活」事件**被漏掉** ⇒ 面板永远停在「未知」，
/// 终端真掉了也报不出来（审计 P0-4 等于白改）。
///
/// 这里用一个**假 ssh**（PATH 前置一个 `sleep 3` 的脚本）走**生产路径**
/// （`Tabs::open` → 内部自己装钩子），断言：
/// 1. 终端起来 ⇒ 状态是「运行中」（**不是**「未知」）；
/// 2. 假 ssh 退出 ⇒ 状态是「已结束」（**不是**「运行中」）。
fn real_path_case() {
    // ---- 假 ssh：起来 3 秒就退（模拟"会话结束"）----
    let dir = std::env::temp_dir().join("pp_fake_ssh_bin");
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let fake = dir.join("ssh");
    std::fs::write(&fake, "#!/bin/sh\nsleep \"${PP_FAKE_SSH_SLEEP:-3}\"\n").expect("写假 ssh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
    }
    // PATH 前置 ⇒ `find_program_in_path("ssh")` 找到的是假 ssh
    let old_path = std::env::var("PATH").unwrap_or_default();
    // SAFETY：本进程是单线程冒烟（还没起任何 worker），改 PATH 没有并发读。
    unsafe { std::env::set_var("PATH", format!("{}:{}", dir.display(), old_path)) };
    // 假 ssh 活多久（秒）——由环境变量控制，两段用例要的时长不一样
    unsafe { std::env::set_var("PP_FAKE_SSH_SLEEP", "3") };

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 2233,
        user: "fwt".into(),
        ..Default::default()
    };

    // 走**生产路径**：Tabs::open 内部建终端并装存活钩子
    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(500);

    let st = tabs.current_panel_state().expect("应有当前面板");

    // ---- 1) 终端起来了 ⇒ 状态必须是「运行中」 ----
    assert_eq!(
        panel::term_state(&st),
        TermState::Alive,
        "终端已经起来了，状态却是 {:?}（存活钩子没接上？）",
        panel::term_state(&st)
    );
    println!("真实路径：终端起来 ⇒ 运行中 ✓");

    // ---- 2) 假 ssh 退出 ⇒ 状态变成「已结束」 ----
    let dead = wait_until(20_000, || panel::term_state(&st) == TermState::Dead);
    assert!(
        dead,
        "假 ssh 都退出了，状态还是 {:?}（结束事件没传到面板？）",
        panel::term_state(&st)
    );
    println!("真实路径：终端退出 ⇒ 已结束 ✓");

    // ---- 3) **没接钩子** ⇒ 不许谎报「运行中」 ----
    //
    // 钉住的是"别谎报"：终端确实活着，但面板**没收到**存活上报
    // ⇒ 只能是「未知」。要是有人把判定改成"有终端就算活着"，
    // 这条就会挂（状态会变成「运行中」）。
    unsafe { std::env::set_var("PP_FAKE_SSH_SLEEP", "30") };
    {
        let panel2 = SftpPanel::new(&opts);
        // 故意**不装**存活钩子
        let pane2 = puttyplus::term::TermPane::new(&opts);
        let win2 = gtk::Window::new();
        win2.set_default_size(600, 400);
        win2.set_child(Some(pane2.widget()));
        win2.present();
        pump(800);
        assert!(pane2.session_alive(), "假 ssh 没起来（这一段的前提就不成立）");
        assert_eq!(
            panel::term_state(&panel2.st),
            TermState::Unknown,
            "没接存活钩子却报「运行中」——那是谎报"
        );
        println!("真实路径：没接钩子 ⇒ 未知（不谎报）✓");
    }
}
