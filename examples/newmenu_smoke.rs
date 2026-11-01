//! 「新建」入口统一的冒烟（审计 P1-2、P1-6，阶段 68）。
//!
//! **问题**：同一个"新建"以前有 5 个入口（顶栏 `+` / 侧栏 `+` / 子标签 `+`
//! / 欢迎页按钮 / 文件菜单），**各写各的** ⇒ 从哪个入口进来看到的东西不一样
//! （子标签 `+` 能建数据库连接，顶栏 `+` 不能；文件菜单叫「新建连接…」，
//! 标签头右键叫「增加数据库连接…」）⇒ 用户以为功能不一样，得多试几个地方。
//!
//! **断言**：
//! 1. 顶栏入口的菜单项 = 动作表**全集**，且**逐项**对得上（文字 + 图标）；
//! 2. 子标签入口只给"会话内能做"的（新建终端 / 新建数据库连接）；
//! 3. 没有会话时「新建终端 / 新建数据库连接」**置灰**（点了没反应最让人
//!    以为程序卡了），有会话时全部可用；
//! 4. 两个 `+` 长得**不一样**（子标签是 `tab-new-symbolic`），顶栏与侧栏
//!    是同一件事 ⇒ 图标一样；
//! 5. 侧栏的 `+` 点开后，弹层里就是动作表那几项（**走真按钮**）。
//!
//! ```text
//! xvfb-run -a cargo run --example newmenu_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::newmenu::{self, NewCtx};
use puttyplus::topmenus::{NewEntry, NEW_ITEMS};

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

/// 盒子里的一排菜单项按钮。
fn buttons_in(b: &gtk::Box) -> Vec<gtk::Button> {
    let mut out = Vec::new();
    let mut cur = b.first_child();
    while let Some(c) = cur {
        if let Some(btn) = c.downcast_ref::<gtk::Button>() {
            out.push(btn.clone());
        }
        cur = c.next_sibling();
    }
    out
}

fn main() {
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

    // ---------- 1. 顶栏入口 = 动作表全集，逐项对得上 ----------
    {
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);
        newmenu::build_menu(NewEntry::TopBar, &vb, NewCtx::with_session(), |_| {});
        let items = NewEntry::TopBar.items();
        let btns = buttons_in(&vb);
        check!(
            "顶栏菜单项数 = 动作表全集",
            btns.len() == NEW_ITEMS.len() && items.len() == NEW_ITEMS.len(),
            format!("菜单 {} 项 / 表 {} 项", btns.len(), NEW_ITEMS.len())
        );
        for (b, it) in btns.iter().zip(items.iter()) {
            check!(
                &format!("顶栏「{}」文字", it.label),
                puttyplus::menu::item_label(b).as_deref() == Some(it.label),
                format!("{:?}", puttyplus::menu::item_label(b))
            );
            check!(
                &format!("顶栏「{}」图标", it.label),
                puttyplus::menu::item_icon(b).as_deref() == Some(it.icon),
                format!("{:?}", puttyplus::menu::item_icon(b))
            );
        }
    }

    // ---------- 2. 子标签入口只给会话内的 ----------
    {
        let items = NewEntry::SubTab.items();
        let actions: Vec<&str> = items.iter().map(|i| i.action).collect();
        check!(
            "子标签只给「新建终端 / 新建数据库连接」",
            actions == vec!["new_term", "new_db"],
            format!("{actions:?}")
        );
        // ⚠️ **不许**出现全局动作（建连接 / 导入 PuTTY 会话）——那不属于这个会话
        check!(
            "子标签不给全局动作",
            !actions.contains(&"new") && !actions.contains(&"import"),
            format!("{actions:?}")
        );
    }

    // ---------- 3. 没有会话 ⇒ 会话内的项置灰 ----------
    {
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);
        newmenu::build_menu(NewEntry::TopBar, &vb, NewCtx::empty(), |_| {});
        let items = NewEntry::TopBar.items();
        for (b, it) in buttons_in(&vb).iter().zip(items.iter()) {
            let want = it.scope != puttyplus::topmenus::NewScope::Session;
            check!(
                &format!(
                    "没有会话时「{}」{}",
                    it.label,
                    if want { "可用" } else { "置灰" }
                ),
                b.is_sensitive() == want,
                format!("is_sensitive = {}", b.is_sensitive())
            );
        }
    }

    // ---------- 4. 两个 + 长得不一样 ----------
    {
        check!(
            "顶栏 + 与子标签 + 图标不同",
            newmenu::entry_icon(NewEntry::TopBar) != newmenu::entry_icon(NewEntry::SubTab),
            format!(
                "{} vs {}",
                newmenu::entry_icon(NewEntry::TopBar),
                newmenu::entry_icon(NewEntry::SubTab)
            )
        );
        check!(
            "顶栏 + 与侧栏 + 图标相同（同一件事）",
            newmenu::entry_icon(NewEntry::TopBar) == newmenu::entry_icon(NewEntry::Sidebar),
            format!(
                "{} vs {}",
                newmenu::entry_icon(NewEntry::TopBar),
                newmenu::entry_icon(NewEntry::Sidebar)
            )
        );
        check!(
            "子标签 + 的 tooltip 写清作用域",
            newmenu::entry_tooltip(NewEntry::SubTab).contains("当前会话"),
            newmenu::entry_tooltip(NewEntry::SubTab).to_string()
        );
    }

    // ---------- 5. 侧栏的 +：点开就是动作表那几项（走真按钮） ----------
    {
        let tabs = puttyplus::tabs::Tabs::new();
        let sb = puttyplus::sidebar::Sidebar::new(std::rc::Rc::clone(&tabs));
        // ⚠️ **必须 setup**：接线（点「＋」的回调）在 `setup()` 里，不在
        // `new()` 里 ⇒ 不调它，按钮点了没反应（跟用户看到的不是一回事）。
        sb.setup();
        let plus = sb.plus_button();
        // ⚠️ 弹层要能 popup，控件必须**在一个真实窗口里**（没有 toplevel 时
        // `pop.popup()` 会 GTK-CRITICAL 直接段错误）。
        let win = gtk::Window::new();
        win.set_default_size(420, 640);
        win.set_title(Some("newmenu smoke"));
        win.set_child(Some(&sb.widget));
        win.present();
        pump(400);
        // ⚠️ 阶段 92b：侧栏的 ＋ 改成**文字加号**（symbolic 图标是细线条，
        // 浅色底上几乎看不见——用户 2026-11-01：「『＋』号是否可以搞得粗一点、
        // 明显一点」）⇒ 它不再有 icon_name，而是 child 是个写着「＋」的 Label。
        // 阶段 92c：顶栏那个 ＋ 已删 ⇒ "两个入口图标一致"不再是不变量。
        {
            let lbl = plus
                .child()
                .and_downcast::<gtk::Label>()
                .expect("侧栏 ＋ 应当是文字加号（Label）");
            check!(
                "侧栏 + 是文字加号（不是 symbolic 图标）",
                lbl.text() == "＋",
                format!("{:?}", lbl.text())
            );
            check!(
                "侧栏 + 不再是图标按钮（没有 icon_name）",
                plus.icon_name().is_none(),
                format!("{:?}", plus.icon_name())
            );
        }
        let tip = plus.tooltip_text().unwrap_or_default();
        check!("侧栏 + 有 tooltip", !tip.is_empty(), tip.clone());

        // ⚠️ 点**真按钮**（跟用户点的是同一条路径）
        plus.emit_clicked();
        pump(200);
        // ⚠️ 弹层是 `set_parent` 挂到按钮上的，**不进 children 链** ⇒
        // 只能从侧栏的测试钩子里拿。
        let pop = sb.plus_popover().expect("侧栏 + 点开后应当有弹层");
        let body = pop.child().expect("弹层里应当有内容");
        let body = body
            .downcast_ref::<gtk::Box>()
            .expect("弹层内容应当是菜单主体 Box")
            .clone();
        let btns = buttons_in(&body);
        let labels: Vec<String> = btns
            .iter()
            .filter_map(|b| puttyplus::menu::item_label(b))
            .collect();
        let want: Vec<String> = NewEntry::Sidebar
            .items()
            .iter()
            .map(|i| i.label.to_string())
            .collect();
        check!(
            "侧栏菜单 = 动作表全集",
            labels == want,
            format!("{labels:?} vs {want:?}")
        );
        // 没有会话 ⇒ 会话内的项应当是灰的
        for (b, it) in btns.iter().zip(NewEntry::Sidebar.items().iter()) {
            if it.scope == puttyplus::topmenus::NewScope::Session {
                check!(
                    &format!("没有会话时侧栏「{}」置灰", it.label),
                    !b.is_sensitive(),
                    "它还能点"
                );
            }
        }
    }

    // ---------- 6. 有会话 ⇒ 上下文跟着变（不再置灰） ----------
    {
        let tabs = puttyplus::tabs::Tabs::new();
        check!(
            "空窗口没有会话",
            !NewCtx::of(&tabs).has_session,
            "刚建好就有会话了"
        );
        // 连一个不存在的地址：这里只关心"有没有会话"这件事
        let opts = ConnOptions {
            proto: "ssh".into(),
            host: "127.0.0.1".into(),
            port: 9,
            user: "nobody".into(),
            ..Default::default()
        };
        tabs.open(&opts);
        pump(300);
        check!(
            "打开连接后有会话",
            NewCtx::of(&tabs).has_session,
            "连接页开着却说有会话"
        );

        // ---- 子标签的「＋」：走真按钮，看它弹出来的菜单 ----
        //
        // ⚠️ 弹层要能 popup，控件必须在一个**真实窗口**里。
        let win = gtk::Window::new();
        win.set_default_size(900, 600);
        win.set_title(Some("newmenu smoke sub"));
        win.set_child(Some(&tabs.notebook));
        win.present();
        pump(400);

        let st = tabs.current_panel_state().expect("应当有会话状态");
        let sub = st.sub_tabs.borrow().clone().expect("连接页应当有子标签栏");
        sub.new_button().emit_clicked();
        pump(300);
        let pop = sub.new_popover().expect("点「＋」应当弹出菜单");
        let body = pop.child().expect("弹层里应当有内容");
        let body = body
            .downcast_ref::<gtk::Box>()
            .expect("弹层内容应当是菜单主体 Box")
            .clone();
        let btns = buttons_in(&body);
        let labels: Vec<String> = btns
            .iter()
            .filter_map(|b| puttyplus::menu::item_label(b))
            .collect();
        check!(
            "子标签菜单 = 会话内的两项",
            labels == vec!["新建终端", "新建数据库连接…"],
            format!("{labels:?}")
        );
        // 有会话 ⇒ 两项都该能点
        for b in btns.iter() {
            check!(
                &format!(
                    "有会话时子标签「{}」可用",
                    puttyplus::menu::item_label(b).unwrap_or_default()
                ),
                b.is_sensitive(),
                "它被置灰了"
            );
        }
        // 子标签的「＋」图标跟顶栏**不一样**（长得一样分不清）
        check!(
            "子标签 + 的图标与顶栏不同",
            sub.new_button().icon_name().as_deref()
                != Some(newmenu::entry_icon(NewEntry::TopBar)),
            format!("{:?}", sub.new_button().icon_name())
        );
    }

    if fails.is_empty() {
        println!("newmenu smoke ok");
    } else {
        println!("newmenu smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
