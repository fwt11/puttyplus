//! 阶段 177/178 冒烟：**侧边栏外壳**（主窗口左栏 = 页签 `[连接 | 文件]`）。
//!
//! **为什么要有这个冒烟**：计划 `docs/SIDEBAR_PLAN_2026-11-27.md` 要把
//! 「会话侧边栏」（连接管理）与「文件面板」**合并成左边一栏**、用页签切换
//! ⇒ 目标是终端占满整页宽（1200 宽的窗口里 **≈630px → ≈860px**）。
//!
//! ⚠️ **本冒烟必须复现真 app 的所有权结构**（2026-11-27 用户报的真 bug）：
//!   真 app 里 `build_main_window()` 返回 `(tabs, win)` ⇒ 里面的
//!   `Rc<SidebarHost>` 是**局部变量** ⇒ **函数一返回就被 drop**
//!   ⇒ 早先版本把 `Weak<SidebarHost>` 存进 `Tabs` ⇒ **永远 upgrade 不到**
//!   ⇒ 文件面板**永远挂不进侧边栏**（用户截图：终端连上了，
//!   文件页还是「还没有连接」；而侧边栏本身看着完全正常 —— 控件树
//!   自己活着 ⇒ 只有"同步"这一环断了 ⇒ 肉眼极难发现）。
//!
//!   ⇒ **这里必须 `drop(host)`**（模拟那个 drop）⇒ 断言文件面板
//!   **照样**挂得上去 ⇒ 才是真的守住这个 bug。
//!
//! 盯的断言：
//! - 页签**恰好两个**、标题是「连接」「文件」；
//! - 「连接」页里**连接树还在**（连接管理没被搬丢）；
//! - 「文件」页是**独立的一页**（切过去看到的不是连接树）；
//! - 切页**真的切得动**（`current_page` 跟着变）；
//! - 折叠/展开**回到上次看的页签**（已拍板的折叠语义）；
//! - **外壳结构体被 drop 之后**，连接打开 ⇒ 文件面板**照样**进侧边栏；
//! - 切连接 ⇒「文件」页**跟着换人**。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sidebar_tabs_smoke

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::sidebar::Sidebar;
use puttyplus::sidebarhost::{self, SidebarHost, PAGE_CONN, PAGE_FILES};
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

    // ---- ⚠️⚠️ **先把要用的控件句柄拿出来，再把外壳 drop 掉** ----
    //
    // 这是在**复现真 app**：`build_main_window()` 返回时，
    // `Rc<SidebarHost>` 就没了；只有**控件**（在树上）还活着。
    // ⇒ 后面所有断言都只许用控件句柄，不许再用 `host.…`。
    let nb = host.nb.clone();
    let root = host.root.clone();
    let files_page = host.files_page.clone();
    let files_slot = host.files_slot.clone();
    let conn_label = host.conn_label.clone();
    let files_label = host.files_label.clone();
    drop(host); // ← **这一步就是那个 bug 的复现点**

    // ---- ① 页签恰好两个，标题对 ----
    check!(
        "侧边栏**恰好两个页签**",
        nb.n_pages() == 2,
        format!("{} 页", nb.n_pages())
    );
    let titles = vec![conn_label.text().to_string(), files_label.text().to_string()];
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
    let conn_page_kids = nb.page_num(&sb.widget);
    check!(
        "「连接」页里**就是侧栏本身**",
        conn_page_kids == Some(0),
        format!("页号 {conn_page_kids:?}")
    );

    // ---- ③ 「文件」页是**独立的一页**（切过去看到的不是连接树）----
    let files_page_kids = nb.page_num(&files_page);
    check!(
        "「文件」页是**单独的一页**",
        files_page_kids == Some(1),
        format!("页号 {files_page_kids:?}")
    );
    check!(
        "「文件」页里**没有**侧栏（不是连接树）",
        !has_class_descendant(&files_slot, "pp-sidebar"),
        "文件页里出现了 pp-sidebar".to_string()
    );

    // ---- ④ 切页真的切得动 ----
    nb.set_current_page(Some(PAGE_FILES as u32));
    uitest::pump(80);
    check!(
        "切到「文件」页后 `current_page` 跟着变",
        nb.current_page() == Some(PAGE_FILES as u32),
        format!("current_page = {:?}", nb.current_page())
    );
    nb.set_current_page(Some(PAGE_CONN as u32));
    uitest::pump(80);
    check!(
        "切回「连接」页后 `current_page` 跟着变",
        nb.current_page() == Some(PAGE_CONN as u32),
        format!("current_page = {:?}", nb.current_page())
    );

    // ---- ⑤ 折叠/展开回到上次看的页签（已拍板的语义）----
    //
    // ⚠️ **怎么验**：折叠 = 整栏 `set_visible(false)`；展开 = 可见
    //   ⇒ **不切页** ⇒ 展开后 `current_page` 必须还是折叠前那一页。
    nb.set_current_page(Some(PAGE_FILES as u32));
    uitest::pump(50);
    let before = nb.current_page();
    // 模拟 `main.rs` 里的折叠开关（同一个动作：整栏隐藏）
    root.set_visible(false);
    uitest::pump(50);
    check!(
        "折叠后整栏**不可见**",
        !root.is_visible(),
        "折叠后 root 仍然可见".to_string()
    );
    root.set_visible(true);
    uitest::pump(50);
    check!(
        "展开后**回到上次看的页签**",
        nb.current_page() == before,
        format!("展开后 current_page = {:?}（折叠前 {before:?}）", nb.current_page())
    );

    // ---- ⑥ 连接打开 ⇒ 文件面板进侧边栏（**外壳已被 drop**）----
    //
    // ⚠️ **连不上也行的连接参数**：这里只关心「面板挂在哪儿」，
    //   不关心连上没连上（127.0.0.1:9 没人监听 ⇒ 面板会显示连接失败）。
    //
    // ⚠️⚠️ **这一段就是那个 bug 的守门**：外壳结构体早就被 drop 了
    //   （`drop(host)`）⇒ 文件面板**照样**得挂上去 ⇒
    //   早先版本（存 `Weak<SidebarHost>`）在这里**必然红**。
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        session_name: Some("第一个会话".into()),
        ..Default::default()
    };
    tabs.open(&opts);
    uitest::pump(300);
    let st = tabs.current_panel_state().expect("应当有当前面板");
    check!(
        "**外壳被 drop 之后**，连接打开 ⇒「文件」页里仍是**当前连接的文件面板**",
        sidebarhost::files_shows(&files_slot, st.root.upcast_ref::<gtk::Widget>()),
        "「文件」页里挂的不是当前面板的文件面板（外壳 drop ⇒ 同步断了？）".to_string()
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
        sidebarhost::files_shows(&files_slot, st2.root.upcast_ref::<gtk::Widget>()),
        "「文件」页没跟着换".to_string()
    );
    check!(
        "上一条连接的面板**被换下**了（不是两份叠在一起）",
        !sidebarhost::files_shows(&files_slot, st.root.upcast_ref::<gtk::Widget>()),
        "旧面板还挂在侧边栏里".to_string()
    );

    // ---- ⑦b **切回**第一条连接 ⇒ 面板**回到它自己**（不串台）----
    //
    // ⚠️⚠️ **这是「两个连接的面板互换了」那个 bug 的守门**
    //   （用户 2026-11-27 报：终端 ls 的内容与面板对不上）。
    //
    //   根因：GTK4 的 `switch-page` **在 `current_page()` 更新之前**发
    //   ⇒ 钩子里读 `current_page()` 拿到的是**上一个**连接 ⇒ 串台。
    //   ⇒ 现在改成**按页面控件认连接** ⇒ 这里必须绿。
    let first_page = tabs.page_widget_of_session("第一个会话");
    check!(
        "能按会话名找到第一条连接的页面控件",
        first_page.is_some(),
        "找不到「第一个会话」的页面".to_string()
    );
    if let Some(p1) = first_page {
        tabs.switch_to_page(&p1);
        uitest::pump(300);
        check!(
            "**切回**第一条连接 ⇒「文件」页回到**它自己**的面板（不串台）",
            sidebarhost::files_shows(&files_slot, st.root.upcast_ref::<gtk::Widget>()),
            "切回来之后面板还是第二条连接的 ⇒ 串台了".to_string()
        );
    }

    // ---- ⑦c 打开连接 ⇒ 侧边栏**自动切到「文件」页** ----
    //
    // ⚠️ **用户 2026-11-27**：「双击某个连接，打开了终端，同步的文件面板
    //   也应该显示，而不应该还是显示连接管理器」⇒ 开连接时切页。
    nb.set_current_page(Some(PAGE_CONN as u32));
    uitest::pump(50);
    check!(
        "先停在「连接」页（模拟用户正在浏览连接列表）",
        nb.current_page() == Some(PAGE_CONN as u32),
        format!("current_page = {:?}", nb.current_page())
    );
    let opts3 = ConnOptions {
        session_name: Some("第三个会话".into()),
        ..opts.clone()
    };
    tabs.open(&opts3);
    uitest::pump(300);
    check!(
        "打开连接 ⇒ 侧边栏**自动切到「文件」页**",
        nb.current_page() == Some(PAGE_FILES as u32),
        format!("current_page = {:?}", nb.current_page())
    );

    // ---- ⑧ 「显示文件面板」菜单项 ⇒ 切到侧边栏的「文件」页 ----
    nb.set_current_page(Some(PAGE_CONN as u32));
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
        nb.current_page() == Some(PAGE_FILES as u32),
        format!("current_page = {:?}", nb.current_page())
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
