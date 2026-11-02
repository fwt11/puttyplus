//! 顶栏菜单的冒烟（阶段 74，审计 P1-13）。
//!
//! **问题**（审计原话）：
//! - 顶栏菜单**一个快捷键都不显示**（用户只能去查「快捷键一览」）；
//! - 欢迎页（没有终端）时打开「终端」菜单，「复制/粘贴/查找/分屏/关闭当前
//!   终端」**全是可点状态**，点了没反应；
//! - 「显示侧边栏」是带勾的开关项，「按键广播（开关）」「记录终端日志
//!   （开关）」「录制会话（开关）」却是普通项 ⇒ 同一类状态三种表现。
//!
//! **断言**（走真实菜单表 + 真实弹层正文）：
//! 1. 每个带快捷键的菜单项，右边都渲染出快捷键（`pp-menu-accel`）；
//! 2. 没有终端时，终端相关的项**灰掉**（insensitive），动作也禁用；
//! 3. 与终端无关的项（「新建连接…」「快捷键一览」「退出」）**照常可点**
//!    （别把整张菜单都灰了）；
//! 4. 「按键广播」「记录终端日志」「录制会话」「显示侧边栏」都是**开关项**
//!    （ToggleButton，带勾选标记），文字里不再写「（开关）」。
//!
//! ```text
//! xvfb-run -a cargo run --example topmenu_smoke
//! ```

use gtk::prelude::*;
use puttyplus::topbar;
use puttyplus::topmenus::{MenuEntry, TOP_MENUS};

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

/// 菜单项按钮 + 它的文字（菜单项按钮的 child 是一层 Box：图标 + 文字 + …）。
fn items_of(body: &gtk::Widget) -> Vec<(String, gtk::Widget)> {
    let mut out: Vec<(String, gtk::Widget)> = Vec::new();
    walk(body, &mut |w| {
        if !w.is::<gtk::Button>() {
            return;
        }
        // 找带 pp-menu-label 类的那个 Label
        let mut text: Option<String> = None;
        walk(w, &mut |x| {
            if text.is_none() && x.has_css_class("pp-menu-label") {
                if let Some(l) = x.downcast_ref::<gtk::Label>() {
                    text = Some(l.text().to_string());
                }
            }
        });
        if let Some(t) = text {
            out.push((t, w.clone()));
        }
    });
    out
}

/// 某个菜单项里的快捷键文字（`pp-menu-accel` 那个 Label）。
fn accel_of(item: &gtk::Widget) -> Option<String> {
    let mut found: Option<String> = None;
    walk(item, &mut |x| {
        if found.is_none() && x.has_css_class("pp-menu-accel") {
            if let Some(l) = x.downcast_ref::<gtk::Label>() {
                found = Some(l.text().to_string());
            }
        }
    });
    found
}

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ⚠️ 「清空最近连接」的可点性取决于**有没有最近连接** ⇒ 隔离一份空文件，
    //    免得断言随用户机器上的真实记录变（那样这条断言在别人机器上会挂）。
    let recent = std::env::temp_dir().join(format!("pp-topmenu-recent-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&recent);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑，不存在并发改环境变量
    unsafe {
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
    }

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

    let win = gtk::Window::new();
    win.set_default_size(600, 200);

    // 动作表：菜单表里引用到的每个 action 都建一个
    let mut acts: std::collections::HashMap<&'static str, gtk::gio::SimpleAction> =
        std::collections::HashMap::new();
    for a in puttyplus::topmenus::all_referenced_actions() {
        acts.insert(a, gtk::gio::SimpleAction::new(a, None));
    }
    let acts = std::rc::Rc::new(acts);

    // 按菜单表把 5 个顶级菜单的正文都建出来
    let mut menus: Vec<(String, Vec<(String, gtk::Widget)>)> = Vec::new();
    let mut popovers: Vec<gtk::Popover> = Vec::new();
    for (title, entries) in TOP_MENUS {
        let pop = gtk::Popover::new();
        let m = topbar::build_body(entries, &acts, &pop, win.upcast_ref::<gtk::Window>());
        pop.set_child(Some(&m.body));
        // ⚠️ 刷新**手动调**：GTK4 的弹层只有真实指针事件才会 map，
        // 冒烟不去凑那个（`refresh` 就是给这种场合用的）。
        (m.refresh)();
        pump(80);
        menus.push((
            (*title).to_string(),
            items_of(m.body.upcast_ref::<gtk::Widget>()),
        ));
        popovers.push(pop);
    }
    // 别让弹层被回收
    let _ = &popovers;

    let item = |menu: &str, label: &str| -> Option<gtk::Widget> {
        menus
            .iter()
            .find(|(t, _)| t == menu)
            .and_then(|(_, items)| {
                items
                    .iter()
                    .find(|(l, _)| l == label)
                    .map(|(_, w)| w.clone())
            })
    };

    // ---------- 1. 快捷键列渲染出来了 ----------
    let mut n_accel = 0;
    let mut missing: Vec<String> = Vec::new();
    for (title, entries) in TOP_MENUS {
        for e in entries.iter().copied() {
            let (label, sc) = match e {
                MenuEntry::Item {
                    label, shortcut, ..
                } => (label, shortcut),
                MenuEntry::Toggle {
                    label, shortcut, ..
                } => (label, shortcut),
                _ => continue,
            };
            let Some(sc) = sc else { continue };
            match item(title, label) {
                Some(w) => match accel_of(&w) {
                    Some(t) if t == sc => n_accel += 1,
                    other => missing.push(format!("{title}·{label} 期望「{sc}」，实得 {other:?}")),
                },
                None => missing.push(format!("{title}·{label} 找不到菜单项")),
            }
        }
    }
    check!(
        "每个带快捷键的菜单项右边都渲染出快捷键",
        missing.is_empty(),
        missing.join("；")
    );
    check!(
        "快捷键列不是摆设（有 10 处以上）",
        n_accel >= 10,
        format!("{n_accel} 处")
    );

    // ---------- 2. 没有终端 ⇒ 终端相关的项灰掉 ----------
    for label in ["复制", "粘贴", "终端查找…", "左右分屏", "关闭当前终端"] {
        let w = item("终端", label).unwrap_or_else(|| panic!("找不到「{label}」"));
        check!(
            format!("没有终端时「{label}」灰掉"),
            !w.is_sensitive(),
            "还是可点的"
        );
    }
    for label in ["数据库…", "端口转发…", "会话选项…"] {
        let w = item("连接", label).unwrap_or_else(|| panic!("找不到「{label}」"));
        check!(
            format!("没有会话时「{label}」灰掉"),
            !w.is_sensitive(),
            "还是可点的"
        );
    }

    // ---------- 3. 与终端无关的项照常可点 ----------
    for (menu, label) in [
        ("文件", "新建连接…"),
        ("文件", "退出"),
        ("工具", "快捷键一览"),
        ("工具", "关于 PuTTYPlus"),
        ("视图", "外观…"),
    ] {
        let w = item(menu, label).unwrap_or_else(|| panic!("找不到「{label}」"));
        check!(
            format!("「{menu}·{label}」照常可点"),
            w.is_sensitive(),
            "被误灰了"
        );
    }

    // ---------- 4. 动作也跟着禁用 ----------
    check!(
        "没有终端时「复制」动作也禁用",
        !acts.get("copy").map(|a| a.is_enabled()).unwrap_or(true),
        "动作还是启用的"
    );
    check!(
        "「新建连接」动作不受影响",
        acts.get("new").map(|a| a.is_enabled()).unwrap_or(false),
        "动作被误禁了"
    );

    // ---------- 4c. 「清空最近连接」：没有最近连接时灰掉 ----------
    //
    // 堡垒机场景：最近连接会被一次性连接冲垮 ⇒ 用户得能清。
    // 没有记录时清不了 ⇒ 灰掉（别给一个点了没反应的项）。
    {
        let w = item("文件", "清空最近连接")
            .unwrap_or_else(|| panic!("找不到「清空最近连接」"));
        check!(
            "没有最近连接时「清空最近连接」灰掉",
            !w.is_sensitive(),
            "还是可点的"
        );
    }

    // ---------- 4d. 动作也跟着禁用 ----------
    check!(
        "「清空最近连接」动作也禁用",
        !acts
            .get("clear_recent")
            .map(|a| a.is_enabled())
            .unwrap_or(true),
        "动作还是启用的"
    );

    // ---------- 4b. 传输队列有主菜单入口（阶段 77c-1）----------
    //
    // 以前传输队列**只有 SFTP 文件面板工具栏上的一个图标**能进 ⇒ 错过一次
    // 桌面通知、想回看传过什么，就找不到门了（审计 P1-9 / P2-18）。
    {
        let w = item("连接", "传输队列…").expect("「连接」菜单里找不到「传输队列…」");
        check!(
            "「连接·传输队列…」在菜单里",
            true,
            ""
        );
        check!(
            "没有会话时「传输队列…」不可点",
            !w.is_sensitive(),
            "没有会话也亮着（点了没反应）"
        );
        check!(
            "没有会话时「queue」动作也禁用",
            !acts.get("queue").map(|a| a.is_enabled()).unwrap_or(true),
            "动作还是启用的"
        );
    }

    // ---------- 5. 开关项都是 Toggle ----------
    for (menu, label) in [
        ("终端", "按键广播"),
        ("终端", "记录终端日志"),
        ("终端", "录制会话"),
        ("视图", "显示侧边栏"),
    ] {
        let w = item(menu, label).unwrap_or_else(|| panic!("找不到「{label}」"));
        let is_toggle = w.is::<gtk::ToggleButton>();
        check!(
            format!("「{menu}·{label}」是开关项"),
            is_toggle,
            "不是 ToggleButton"
        );
        check!(
            format!("「{label}」文字里不再写「（开关）」"),
            !label.contains("（开关）"),
            label
        );
    }

    if fails.is_empty() {
        println!("topmenu smoke ok");
    } else {
        println!("topmenu smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
