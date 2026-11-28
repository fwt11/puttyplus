//! **顶栏结构冒烟**（阶段 193）。
//!
//! 钉住用户 2026-11-28 的两条要求：
//! ① 「显示侧边栏的按钮放在那里，很多人可能误以为是软件图标」⇒ 它现在
//!    **在顶栏最右**（不再是紧挨窗口左边框的那个位置）；
//! ② 「新建终端，新建数据库，容器几个按钮从『+』里放出来」⇒ 这三颗
//!    **直出在顶栏右端**，不用点两次。
//!
//! ⚠️ **为什么用「父容器索引」而不是 x 坐标**：窗口没被 WM 摆过时坐标不
//!   可靠；GTK4 的 `HeaderBar` 内部是 **start / center / end** 三个盒子 ⇒
//!   「在 end 组里」就是「在右边」⇒ 稳定且不依赖窗口管理器。
//!
//! 运行：`xvfb-run -a cargo run --example topbar_actions_smoke`

use gtk::prelude::*;
use std::rc::Rc;

use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;
use puttyplus::topbar;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

/// 递归找出所有**按钮**（含菜单项的按钮）。
fn all_buttons(root: &gtk::Widget, out: &mut Vec<gtk::Button>) {
    let mut c = root.first_child();
    while let Some(x) = c {
        if let Some(b) = x.downcast_ref::<gtk::Button>() {
            out.push(b.clone());
        }
        all_buttons(&x, out);
        c = x.next_sibling();
    }
}

/// 递归找出带 `pp-menubar` 类的那个盒子（**菜单栏**）。
///
/// ⚠️ 它**不在 HeaderBar 的直接子控件里**（GTK4 把它塞进内部的 start 盒
///   子）⇒ 必须下钻。
fn find_menu_bar(root: &gtk::Widget) -> Option<gtk::Box> {
    if let Some(b) = root.downcast_ref::<gtk::Box>() {
        if b.has_css_class("pp-menubar") {
            return Some(b.clone());
        }
    }
    let mut c = root.first_child();
    while let Some(x) = c {
        if let Some(f) = find_menu_bar(&x) {
            return Some(f);
        }
        c = x.next_sibling();
    }
    None
}

/// 找到 tooltip 里有 `key` 的那颗按钮。
fn find_by_tip(btns: &[gtk::Button], key: &str) -> Option<gtk::Button> {
    btns.iter()
        .find(|b| b.tooltip_text().map(|t| t.contains(key)).unwrap_or(false))
        .cloned()
}

/// 截一张图（**可选**：`PP_TOPBAR_SHOT=/path` 时才截）。
///
/// ⚠️ 用 ImageMagick 的 `import` 截整屏（Xvfb 下只有本程序）⇒ 不用自己
///   折腾 GTK 离屏渲染。截不到**不算测试失败**（只是没图）。
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

/// 顶栏里的所有按钮。
fn all_buttons_of(header: &gtk::HeaderBar) -> Vec<gtk::Button> {
    let mut v = Vec::new();
    all_buttons(header.upcast_ref::<gtk::Widget>(), &mut v);
    v
}

fn main() {
    let fails = std::cell::Cell::new(0usize);
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.set(fails.get() + 1);
            }
        }};
    }

    let app = gtk::Application::new(Some("pp.topbar.smoke"), Default::default());
    let app2 = app.clone();
    app.connect_activate(move |_| {
        let app = app2.clone();
        puttyplus::theme::load_saved();
        let tabs = Rc::new(Tabs::new());
        let sb = Rc::new(Sidebar::new(Rc::clone(&tabs)));

        // ⚠️ 侧栏开关的回调：点一下要**真的被调用到**。
        let hit = Rc::new(std::cell::Cell::new(0u32));
        let hit2 = Rc::clone(&hit);
        let toggle: Rc<dyn Fn()> = Rc::new(move || hit2.set(hit2.get() + 1));

        let title = gtk::Label::new(Some("prod-01"));
        title.add_css_class("pp-app-title");
        let win = gtk::ApplicationWindow::new(&app);
        // ⚠️ **窗口要矮**（Xvfb 没 WM ⇒ 内容区的 min-height 会把 HeaderBar
        //   压扁 ⇒ 截出来的图字都竖着；真机上有 WM 不存在这个问题）。
        win.set_default_size(1200, 260);
        let header = topbar::build_header(&win, &tabs, &toggle, &sb, &title);
        win.set_titlebar(Some(&header));
        // ⚠️ **窗口得有个内容区**（Xvfb 没 WM ⇒ 空窗口的 HeaderBar 会被
        //   压扁 ⇒ 截出来的字竖着；真机上有 WM 不存在这个问题）。
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        win.set_child(Some(&body));
        win.present();
        pump(40);

        let mut btns = Vec::new();
        all_buttons(header.upcast_ref::<gtk::Widget>(), &mut btns);

        // ---- ① 三颗直出按钮都在 ----
        let tips: Vec<&str> = topbar::SESSION_ACTIONS.iter().map(|(_, t, _)| *t).collect();
        let mut found = Vec::new();
        for t in &tips {
            found.push(find_by_tip(&btns, t));
        }
        check!(
            "① 顶栏右端直出了「新建终端 / 新建数据库 / 容器」三颗",
            found.iter().all(|f| f.is_some()),
            format!("按 tooltip 找：{:?}", found.iter().map(|f| f.is_some()).collect::<Vec<_>>())
        );

        // ---- ② 没会话时那三颗是**灰的**（P1-2：亮了点了没反应 ⇒ 用户以为卡了）----
        let grayed = found
            .iter()
            .all(|f| f.as_ref().map(|b| !b.is_sensitive()).unwrap_or(false));
        check!(
            "② 没有会话时那三颗是灰的（点不动，不会「点了没反应」）",
            grayed,
            format!(
                "sensitive={:?}",
                found
                    .iter()
                    .map(|f| f.as_ref().map(|b| b.is_sensitive()))
                    .collect::<Vec<_>>()
            )
        );

        // ---- ③ 侧栏开关在**顶栏最右**（不再是左上角那个「像 logo」的位置）----
        let b_side = find_by_tip(&btns, "侧边栏");
        check!(
            "③ 顶栏里有那颗「显示/隐藏侧边栏」按钮",
            b_side.is_some(),
            "按 tooltip 找不到"
        );
        // 菜单栏（顶栏**最左**的那一组）⇒ 拿它当「左边」的参照。
        let menu_bar = find_menu_bar(header.upcast_ref::<gtk::Widget>());

        // 菜单栏的**第一个按钮**（"文件"）⇒ 拿它当「左边」的参照点。
        let menu_first_btn = menu_bar.as_ref().and_then(|b| {
            let mut v = Vec::new();
            all_buttons(b.upcast_ref::<gtk::Widget>(), &mut v);
            v.first().cloned()
        });

        if let Some(side) = b_side.as_ref() {
            // **同组**：侧栏开关与三颗会话按钮在**同一个** end 盒子里
            // ⇒ 说明它们是**并排的一组**，不是分散在两头。
            let side_parent = side.parent();
            let same_group = found
                .iter()
                .filter_map(|f| f.as_ref())
                .all(|b| b.parent() == side_parent);
            check!(
                "③ 侧栏开关与三颗会话按钮在**同一组**（右端并排）",
                same_group,
                "它们的父容器不一样 ⇒ 不是一组"
            );

            // **在右边**：用**真实坐标**判定（窗口已 present ⇒ 坐标有效）。
            //
            // ⚠️ **为什么不用「HeaderBar 内部盒子的索引」**：GTK4 的 start /
            //   center / end 三个盒子的**子控件顺序不是文档承诺的**（实测
            //   end 盒排在最前）⇒ 按索引判会判反 ⇒ 改比坐标。
            //
            // ① 侧栏开关在**三颗会话按钮的右边**（同一父容器 ⇒ 坐标可比）。
            let last_session_x = found
                .iter()
                .filter_map(|f| f.as_ref())
                .map(|b| b.allocation().x())
                .max();
            let side_x = side.allocation().x();
            check!(
                "③ 侧栏开关在**三颗会话按钮的右边**（同一组里排最后）",
                last_session_x.map(|x| side_x > x).unwrap_or(false),
                format!("侧栏 x={side_x} 三颗里最右 x={last_session_x:?}")
            );

            // ② 整组在**菜单栏的右边**（不同父容器 ⇒ 换算到顶栏坐标再比）。
            let to_header = |w: &gtk::Widget| -> Option<f64> {
                w.translate_coordinates(header.upcast_ref::<gtk::Widget>(), 0.0, 0.0)
                    .map(|(x, _)| x)
            };
            let side_hx = to_header(side.upcast_ref::<gtk::Widget>());
            let menu_hx = menu_first_btn
                .as_ref()
                .and_then(|b| to_header(b.upcast_ref::<gtk::Widget>()));
            // ⚠️ **判「绝对位置」而不是「谁在谁右边」**：两个都在左边时，
            //   「谁在谁右边」照样成立（`pack_start` 的顺序决定）⇒ 抓不住
            //   「被挪回左上角」这种改坏 ⇒ 必须跟**顶栏中线**比。
            let half = header.width() as f64 / 2.0;
            check!(
                "③ 侧栏开关在**右半边**（不再挤在左上角那个位置）",
                side_hx.map(|x| x > half).unwrap_or(false),
                format!("侧栏 x={side_hx:?} 顶栏中线={half}")
            );
            check!(
                "③ 菜单栏在**左半边**（两边各归各的，不打架）",
                menu_hx.map(|x| x < half).unwrap_or(false),
                format!("菜单栏 x={menu_hx:?} 顶栏中线={half}")
            );
        }

        // ---- ④ 图标都在 Adwaita 里（不然是「缺图占位符」）----
        let theme = gtk::IconTheme::for_display(&gtk::gdk::Display::default().unwrap());
        let missing: Vec<&str> = topbar::SESSION_ACTIONS
            .iter()
            .map(|(i, _, _)| *i)
            .chain(std::iter::once(topbar::SIDEBAR_TOGGLE_ICON))
            .filter(|i| !theme.has_icon(i))
            .collect();
        check!(
            "④ 顶栏这几颗的图标在主题里都存在（不是缺图占位符）",
            missing.is_empty(),
            format!("缺：{missing:?}")
        );

        // ---- ⑤ 点侧栏开关 ⇒ 回调真的被调用 ----
        if let Some(side) = b_side.as_ref() {
            side.emit_clicked();
            pump(10);
            check!(
                "⑤ 点侧栏开关 ⇒ 回调被调用了（不是死按钮）",
                hit.get() == 1,
                format!("回调次数={}", hit.get())
            );
        }

        // ---- 截图（**可选**）：`PP_TOPBAR_SHOT=/tmp/topbar` ⇒ 截一张新顶栏 ----
        //
        // ⚠️ `PP_TOPBAR_LIT=1` ⇒ 把那三颗**点亮**（截图用：正常情况下没会话
        //   时它们是灰的 ⇒ 截出来是灰的，看不出「亮着」什么样）。
        //   ⚠️ **必须放在断言之后**（提前点亮 ⇒ 断言②「没会话时是灰的」会红）。
        if let Ok(base) = std::env::var("PP_TOPBAR_SHOT") {
            if std::env::var("PP_TOPBAR_LIT").is_ok() {
                for b in all_buttons_of(&header) {
                    if b.tooltip_text()
                        .map(|t| {
                            topbar::SESSION_ACTIONS
                                .iter()
                                .any(|(_, tip, _)| t.contains(tip))
                        })
                        .unwrap_or(false)
                    {
                        b.set_sensitive(true);
                    }
                }
            }
            pump(20);
            shot(&format!("{base}.png"));
        }

        if fails.get() == 0 {
            println!("topbar_actions_smoke: 全部通过");
            std::process::exit(0);
        } else {
            println!("topbar_actions_smoke: {} 条失败", fails.get());
            std::process::exit(1);
        }
    });
    app.run();
}
