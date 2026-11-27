//! 阶段 177 冒烟：**侧边栏外壳**（主窗口左栏 = 页签 `[连接 | 文件]`）。
//!
//! **为什么要有这个冒烟**：计划 `docs/SIDEBAR_PLAN_2026-11-27.md` 要把
//! 「会话侧边栏」（连接管理）与「文件面板」**合并成左边一栏**、用页签切换
//! ⇒ 目标是终端占满整页宽（1200 宽的窗口里 **≈630px → ≈860px**）。
//!
//! ⚠️ **本阶段（177）只加外壳、不改行为**：「文件」页先放占位，
//! 文件面板搬进来是**阶段 178**。⇒ 这个冒烟盯的是**外壳本身**：
//! - 页签**恰好两个**、标题是「连接」「文件」；
//! - 「连接」页里**连接树还在**（连接管理没被搬丢）；
//! - 「文件」页是**独立的一页**（切过去看到的是占位，不是连接树）；
//! - 切页**真的切得动**（`current_page` 跟着变）；
//! - 折叠/展开**回到上次看的页签**（已拍板的折叠语义）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sidebar_tabs_smoke

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::sidebar::Sidebar;
use puttyplus::sidebarhost::{SidebarHost, PAGE_CONN, PAGE_FILES};
use puttyplus::tabs::Tabs;
use puttyplus::uitest;

fn main() {
    // ⚠️ **隔离配置**：侧栏会读连接 ini / 最近连接 / 凭据库 ⇒ 不隔离的话
    //   断言会随用户机器变（还会把用户真实的连接名打进日志）。
    let _home = uitest::isolate_configs("sbt");
    uitest::cleanup_temp_configs();
    // ⚠️ 容器/Xvfb 里**没有 a11y 总线** ⇒ GTK 会打一条警告（要在 init 之前设）。
    unsafe { std::env::set_var("GTK_A11Y", "none") };

    gtk::init().expect("gtk init");

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

    let tabs = Tabs::new();
    let sb = Sidebar::new(Rc::clone(&tabs));
    sb.setup();
    let host = SidebarHost::new(&sb);
    // ⚠️ **阶段 178**：把侧边栏交给 `Tabs` ⇒ 它才知道「文件」页该挂谁。
    tabs.set_sidebar_host(&host);

    // ---- ① 页签恰好两个，标题对 ----
    let titles = host.page_titles();
    check!(
        "侧边栏**恰好两个页签**",
        host.nb.n_pages() == 2,
        format!("{} 页", host.nb.n_pages())
    );
    check!(
        "页签标题是「连接」「文件」",
        titles.len() == 2 && titles[0] == "连接" && titles[1] == "文件",
        format!("{titles:?}")
    );

    // ---- ② 「连接」页里是连接树（连接管理没被搬丢）----
    //
    // ⚠️ **怎么验"树还在"**：侧栏的树有专属 CSS 类 `pp-sidebar` ⇒
    //   那个类**只在侧栏自己的 widget 上** ⇒ 在「连接」页里能找到它
    //   ⇒ 说明侧栏确实被放进这一页了。
    let conn_page_kids = host.nb.page_num(&sb.widget);
    check!(
        "「连接」页里**就是侧栏本身**",
        conn_page_kids == Some(0),
        format!("页号 {conn_page_kids:?}")
    );

    // ---- ③ 「文件」页是**独立的一页**（切过去看到的不是连接树）----
    let files_page_kids = host.nb.page_num(&host.files_page);
    check!(
        "「文件」页是**单独的一页**",
        files_page_kids == Some(1),
        format!("页号 {files_page_kids:?}")
    );
    check!(
        "「文件」页里**没有**侧栏（不是连接树）",
        !has_class_descendant(&host.files_slot, "pp-sidebar"),
        "文件页里出现了 pp-sidebar".to_string()
    );

    // ---- ④ 切页真的切得动 ----
    host.switch_to(PAGE_FILES);
    uitest::pump(80);
    check!(
        "切到「文件」页后 `current_page` 跟着变",
        host.current_page() == PAGE_FILES,
        format!("current_page = {}", host.current_page())
    );
    host.switch_to(PAGE_CONN);
    uitest::pump(80);
    check!(
        "切回「连接」页后 `current_page` 跟着变",
        host.current_page() == PAGE_CONN,
        format!("current_page = {}", host.current_page())
    );

    // ---- ⑤ 折叠/展开回到上次看的页签（已拍板的语义）----
    //
    // ⚠️ **怎么验**：折叠 = 整栏 `set_visible(false)`；展开 = 可见
    //   ⇒ **不切页** ⇒ 展开后 `current_page` 必须还是折叠前那一页。
    host.switch_to(PAGE_FILES);
    uitest::pump(50);
    let before = host.current_page();
    // 模拟 `main.rs` 里的折叠开关（同一个动作：整栏隐藏）
    host.root.set_visible(false);
    uitest::pump(50);
    check!(
        "折叠后整栏**不可见**",
        !host.root.is_visible(),
        "折叠后 root 仍然可见".to_string()
    );
    host.root.set_visible(true);
    uitest::pump(50);
    check!(
        "展开后**回到上次看的页签**",
        host.current_page() == before,
        format!("展开后 current_page = {}（折叠前 {before}）", host.current_page())
    );

    // ---- ⑥ 文件面板进侧边栏（阶段 178）----
    //
    // ⚠️ **连不上也行的连接参数**：这里只关心「面板挂在哪儿」，
    //   不关心连上没连上（127.0.0.1:9 没人监听 ⇒ 面板会显示连接失败）。
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        ..Default::default()
    };
    tabs.open(&opts);
    uitest::pump(300);
    let st = tabs.current_panel_state().expect("应当有当前面板");
    check!(
        "「文件」页里是**当前连接的文件面板**",
        host.files_shows(st.root.upcast_ref::<gtk::Widget>()),
        "「文件」页里挂的不是当前面板的文件面板".to_string()
    );

    // ---- ⑦ 切连接 ⇒「文件」页**跟着换人** ----
    //
    // ⚠️ **为什么**：文件面板是**每会话一个**（worker 绑在那条会话上）
    //   ⇒ 侧边栏只有一格 ⇒ 切连接时必须换。
    let opts2 = ConnOptions {
        session_name: Some("第二个会话".into()),
        ..opts.clone()
    };
    tabs.open(&opts2);
    uitest::pump(300);
    let st2 = tabs.current_panel_state().expect("应当有当前面板");
    check!(
        "切到第二条连接 ⇒「文件」页换成**它**的面板",
        host.files_shows(st2.root.upcast_ref::<gtk::Widget>()),
        "「文件」页没跟着换".to_string()
    );
    check!(
        "上一条连接的面板**被换下**了（不是两份叠在一起）",
        !host.files_shows(st.root.upcast_ref::<gtk::Widget>()),
        "旧面板还挂在侧边栏里".to_string()
    );

    // ---- ⑧ 「显示文件面板」菜单项 ⇒ 切到侧边栏的「文件」页 ----
    host.switch_to(PAGE_CONN);
    uitest::pump(50);
    assert!(
        puttyplus::tabs::file_panel_hidden(),
        "侧边栏停在「连接」页 ⇒ 「显示文件面板」应当可用"
    );
    let page = tabs.current_page_widget().expect("应当有当前页");
    tabs.show_file_panel(&page);
    uitest::pump(80);
    check!(
        "「显示文件面板」把侧边栏切到**文件页**",
        host.current_page() == PAGE_FILES,
        format!("current_page = {}", host.current_page())
    );

    if fails.is_empty() {
        println!("sidebar_tabs smoke ok");
    } else {
        println!("sidebar_tabs smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}

/// 控件树里有没有带这个 CSS 类的节点（往下钻，**包括它自己**）。
fn has_class_descendant(root: &impl IsA<gtk::Widget>, cls: &str) -> bool {
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if w.css_classes().iter().any(|c| c == cls) {
            return true;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    false
}
