//! 阶段 135 冒烟：**Docker 没权限时要说人话**（**不连服务器**）。
//!
//! **背景**（用户 2026-11-19 报的截图）：远端账号不在 docker 组 ⇒
//! 容器页只在**底部一行小灰字**里写着「当前账号没有访问 Docker 的权限」
//! ⇒ 界面上是「**一张空表 + 角落一行小字**」⇒ 用户根本看不出问题在哪，
//! 还以为是自己没有镜像。
//!
//! **要的行为**（用户拍板）：
//! - 镜像 / 卷 / 网络三页失败时**也有大号错态**（标题按错误分类）；
//! - **没权限 ⇒ 弹模态对话框**，说清楚「怎么办」；
//! - **每个连接只弹一次**（免得连着点变成骚扰）；
//! - **每次点「容器」看到的都一样** ⇒ 页面上的大号错态**一直都在**。
//!
//! **断言**（全部**不连服务器** ⇒ 用测试钩子直接灌错误）：
//! 1. 镜像 / 卷 / 网络三页失败 ⇒ 大号错态显示着，标题分类正确；
//! 2. 没权限 ⇒ 弹出标题为「没有访问 Docker 的权限」的对话框；
//! 3. **同一个连接再失败 ⇒ 不再弹**（只弹一次）；
//! 4. 「守护进程没起」⇒ **不能弹那个「没权限」的框**（分类顺序），
//!    标题要是「Docker 服务没有运行」。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example docker_perm_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{SftpPanel, dockerui};

/// 后端在「账号不在 docker 组」时给的那句话（**一字不改** ⇒ 分类才准）。
const PERM_MSG: &str = "当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用有权限的账号登录）";
/// 后端在「守护进程没起」时给的那句话。
const DAEMON_MSG: &str = "连不上远程的 Docker（服务没起，或当前账号没权限）";

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

/// 有没有一个**标题是这个**的顶层窗口（对话框）。
fn has_window_titled(title: &str) -> bool {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    for i in 0..model.n_items() {
        let Some(obj) = model.item(i) else { continue };
        let Ok(w) = obj.downcast::<gtk::Window>() else { continue };
        if w.title().as_deref() == Some(title) {
            return true;
        }
    }
    false
}

/// 关掉**所有**标题是这个的窗口（冒烟里用来「关掉那个提示框」）。
fn close_windows_titled(title: &str) {
    use gtk::gio::prelude::ListModelExt;
    let model = gtk::Window::toplevels();
    for i in 0..model.n_items() {
        let Some(obj) = model.item(i) else { continue };
        let Ok(w) = obj.downcast::<gtk::Window>() else { continue };
        if w.title().as_deref() == Some(title) {
            w.close();
        }
    }
}

fn main() {
    // ⚠️ **别碰用户的真实配置**。
    let settings_ini = std::env::temp_dir().join(format!("pp-docker-perm-{}.ini", std::process::id()));
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

    // 一个**不连服务器**的面板（连接会失败，但那个失败与本冒烟无关）。
    let panel = SftpPanel::new(&ConnOptions::default());
    pump(120);
    // 只建控件（不挂页签、不取数据）⇒ 后面全靠测试钩子灌错误。
    dockerui::ensure(&panel.st);
    pump(150);

    const T_PERM: &str = "没有访问 Docker 的权限";
    const T_DAEMON: &str = "Docker 服务没有运行";

    // ---- ① 镜像页失败 ⇒ 大号错态（不是「一张空表 + 底部小字」）----
    dockerui::set_seg_error_for_test(&panel.st, dockerui::SEG_IMAGES, PERM_MSG);
    pump(120);
    check!(
        "① 镜像页失败 ⇒ 显示大号错态",
        dockerui::seg_empty_visible_for_test(&panel.st, dockerui::SEG_IMAGES),
        "镜像页失败后什么也没显示（还是一张空表）"
    );
    check!(
        "① 镜像页错态标题说清是「没权限」",
        dockerui::seg_empty_title_for_test(&panel.st, dockerui::SEG_IMAGES) == T_PERM,
        format!(
            "标题是 {:?}",
            dockerui::seg_empty_title_for_test(&panel.st, dockerui::SEG_IMAGES)
        )
    );

    // ---- ② 卷 / 网络两页也一样 ----
    dockerui::set_seg_error_for_test(&panel.st, dockerui::SEG_VOLUMES, PERM_MSG);
    dockerui::set_seg_error_for_test(&panel.st, dockerui::SEG_NETWORKS, PERM_MSG);
    pump(120);
    check!(
        "② 卷页失败 ⇒ 显示大号错态",
        dockerui::seg_empty_visible_for_test(&panel.st, dockerui::SEG_VOLUMES),
        "卷页失败后什么也没显示"
    );
    check!(
        "② 网络页失败 ⇒ 显示大号错态",
        dockerui::seg_empty_visible_for_test(&panel.st, dockerui::SEG_NETWORKS),
        "网络页失败后什么也没显示"
    );

    // ---- ③ 没权限 ⇒ 弹了对话框 ----
    check!(
        "③ 没权限 ⇒ 弹出对话框",
        has_window_titled(T_PERM),
        "没看到「没有访问 Docker 的权限」的对话框"
    );

    // ---- ④ 同一个连接**只弹一次** ----
    close_windows_titled(T_PERM);
    pump(80);
    dockerui::set_seg_error_for_test(&panel.st, dockerui::SEG_VOLUMES, PERM_MSG);
    pump(120);
    check!(
        "④ 同一个连接只弹一次",
        !has_window_titled(T_PERM),
        "又弹了一次（每个连接只该弹一次）"
    );
    check!(
        "④ 但大号错态**一直都在**（每次看到的都一样）",
        dockerui::seg_empty_visible_for_test(&panel.st, dockerui::SEG_VOLUMES),
        "第二次失败后错态不见了 ⇒ 行为不一致"
    );

    // ---- ⑤ 「守护进程没起」⇒ **不能弹「没权限」的框** ----
    //
    // ⚠️ 那句话里**也带「权限」两个字**（「服务没起，或当前账号没权限」）
    //    ⇒ 分类顺序错了就会弹一个**错的**对话框给用户。
    close_windows_titled(T_PERM);
    dockerui::set_seg_error_for_test(&panel.st, dockerui::SEG_IMAGES, DAEMON_MSG);
    pump(120);
    check!(
        "⑤ 守护进程没起 ⇒ 不弹「没权限」的框",
        !has_window_titled(T_PERM),
        "把「守护进程没起」说成了「没权限」（弹了个错的框）"
    );
    check!(
        "⑤ 守护进程没起 ⇒ 标题是「Docker 服务没有运行」",
        dockerui::seg_empty_title_for_test(&panel.st, dockerui::SEG_IMAGES) == T_DAEMON,
        format!(
            "标题是 {:?}",
            dockerui::seg_empty_title_for_test(&panel.st, dockerui::SEG_IMAGES)
        )
    );

    if fails.is_empty() {
        println!("docker perm smoke ok");
    } else {
        println!("[FAIL] {} 项没过：", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        std::process::exit(101);
    }
}
