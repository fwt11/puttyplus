//! 顶栏菜单的**弹层正文**（阶段 74 / 审计 P1-13）。
//!
//! **为什么从 `main.rs` 里抽出来**：这段原来在 bin 里 ⇒ example 冒烟
//! **测不到**（例子链不到 bin）⇒ 「快捷键列有没有渲染」「没有终端时菜单项
//! 是不是灰的」只能靠人眼看。现在搬到 lib，冒烟能直接验这两件事。
//!
//! **菜单表**在 [`crate::topmenus`]；这里只管"怎么把它画出来"。

use std::rc::Rc;

use gtk::prelude::*;
use gtk::gio;

use crate::i18n::tr;
use crate::trf;
use crate::menu;
use crate::topmenus::MenuEntry;
use crate::topmenus::TOP_MENUS;

/// 动作表：action 名 → action 本体。
///
/// 激活与**禁用**都用它：禁用要 `SimpleAction::set_enabled`——`gio::Action`
/// 这个**接口**没有这个方法（只有 `is_enabled`）。
pub type ActionMap = std::collections::HashMap<&'static str, gtk::gio::SimpleAction>;

/// 一个顶级菜单的弹层：正文 + 刷新闭包。
pub struct MenuBody {
    /// 弹层正文（塞进 Popover 的那个）
    pub body: gtk::Box,
    /// 按当前状态刷新：开关项的勾、菜单项的灰、动作的禁用。
    ///
    /// 弹层每次弹出都会自动调一次；**测试也能手动调**——不用去凑真指针
    /// 事件（GTK4 的弹层只有真实指针才会 map）。
    pub refresh: Rc<dyn Fn()>,
}

/// 按菜单表建一个顶级菜单的弹层正文。
///
/// `acts` 是动作表，`pop` 是这个菜单自己的弹层，`win` 用来注销凭据库的
/// 状态钩子（窗口关了钩子得撤）。
pub fn build_body(
    entries: &[MenuEntry],
    acts: &Rc<ActionMap>,
    pop: &gtk::Popover,
    win: &gtk::Window,
) -> MenuBody {
    let vb = crate::menu::body();
    // 菜单项 + 它的可用条件（阶段 74）：弹出前按当前状态刷灰。
    let mut gated: Vec<(gtk::Widget, &'static str, Option<fn() -> bool>)> = Vec::new();
    // 开关项的当前状态（勾 = 开）：菜单项只建一次 ⇒ 每次弹出前问一次。
    let mut toggles: Vec<(gtk::ToggleButton, fn() -> bool)> = Vec::new();
    for e in entries.iter().copied() {
        // ⚠️ **平台可见性**（阶段 C5）：Windows 上数据库工作台的入口先藏
        //    起来（首版范围是终端 + SFTP + 命令行契约；DB 只保证能编译）。
        if !crate::topmenus::entry_visible(&e) {
            continue;
        }
        match e {
            MenuEntry::Sep => {
                vb.append(&crate::menu::sep());
            }
            MenuEntry::Item {
                icon,
                label,
                action,
                danger,
                shortcut,
                enabled,
            } => {
                // ⚠️ 快捷键写在文字右边（阶段 74 / 审计 P1-13）：
                // 以前顶栏菜单一个快捷键都不显示，用户只能去查一览表。
                // ⚠️ 菜单项文字在 const 表里 ⇒ 显示时翻译（见 `main.rs` 同理）。
                let b = crate::menu::item(icon, tr(label), shortcut);
                if danger {
                    crate::menu::danger(&b);
                }
                let acts2 = acts.clone();
                let pop2 = pop.clone();
                b.connect_clicked(move |_| {
                    if let Some(a) = acts2.get(action) {
                        a.activate(None);
                    }
                    pop2.popdown();
                });
                gated.push((b.clone().upcast::<gtk::Widget>(), action, enabled));
                vb.append(&b);
            }
            MenuEntry::Toggle {
                icon,
                label,
                action,
                state,
                shortcut,
                enabled,
            } => {
                // 开关项（阶段 59）：右侧勾选标记显示当前状态（勾 = 开）。
                // 菜单项只建一次 → 每次弹出前把状态刷成最新的。
                let b = crate::menu::toggle(icon, tr(label), shortcut, state());
                toggles.push((b.clone(), state));
                let acts2 = acts.clone();
                let pop2 = pop.clone();
                // ⚠️ 动作挂 `clicked`（不是 `toggled`）：`set_active()` 是程序
                // 改状态，也会发 `toggled` —— 挂错了等于「刷新菜单就触发一次
                // 动作」。
                b.connect_clicked(move |_| {
                    if let Some(a) = acts2.get(action) {
                        a.activate(None);
                    }
                    pop2.popdown();
                });
                gated.push((b.clone().upcast::<gtk::Widget>(), action, enabled));
                vb.append(&b);
            }
            MenuEntry::Vault => {
                // 凭据库菜单项**带状态**：菜单是"低频入口"，但状态还是要
                // 看得见（用户最容易忘的就是"我到底开没开凭据库"）。
                let b = crate::menu::item(
                    "dialog-password-symbolic",
                    &trf!("凭据库…（{v0}）", v0 = crate::vaultui::state_short(crate::vault::state())),
                    None,
                );
                let acts2 = acts.clone();
                let pop2 = pop.clone();
                b.connect_clicked(move |_| {
                    if let Some(a) = acts2.get("vault") {
                        a.activate(None);
                    }
                    pop2.popdown();
                });
                vb.append(&b);
                // ⚠️ 弱引用：钩子是进程级的，窗口关了钩子还在——抓强引用会漏。
                let w = b.downgrade();
                let alive = crate::vaultui::add_state_hook(move |st| {
                    let Some(b) = w.upgrade() else { return };
                    crate::menu::set_item_label(
                        &b,
                        &trf!("凭据库…（{v0}）", v0 = crate::vaultui::state_short(st)),
                    );
                });
                // 窗口关了就把这条回调注销（否则死回调一直留在列表里）
                let win2 = win.clone();
                win2.connect_destroy(move |_| alive.set(false));
            }
        }
    }
    // ⚠️ **弹出前**按当前状态刷灰（阶段 74 / 审计 P1-13）：
    // 欢迎页上「复制/粘贴/查找/分屏/关闭当前终端」以前全是亮着的，
    // 点了没反应。现在没有终端/会话时这些项**灰掉**，动作也禁用。
    //
    // ⚠️ 状态**只在弹出时**问一次：菜单项只建一次，平时不刷新 ⇒
    // 切了标签再开菜单，状态就是最新的（弹出那一刻问的）。
    let acts2 = acts.clone();
    let gated2 = gated.clone();
    let toggles2 = toggles.clone();
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        for (b, state) in &toggles2 {
            b.set_active(state());
        }
        for (b, action, enabled) in &gated2 {
            let on = enabled.map(|f| f()).unwrap_or(true);
            b.set_sensitive(on);
            // 动作也禁用 ⇒ 即便有人绕开按钮直接激活动作也没用
            if let Some(a) = acts2.get(action) {
                a.set_enabled(on);
            }
        }
    });
    pop.connect_map({
        let refresh = refresh.clone();
        move |_| refresh()
    });
    MenuBody { body: vb, refresh }
}

/// 顶栏菜单栏（文件 / 连接 / 终端 / 视图 / 工具）。
///
/// 全部动作注册在窗口的 `app` action group 上；菜单项只引用 action 名，
/// 所以"菜单 / 快捷键 / 欢迎页提示"三处不会各自维护一套文案。
///
/// **为什么不用 `gtk::PopoverMenuBar`**：原生菜单栏的下拉是 `gio::Menu`
/// 模型渲染的——没有图标、没有快捷键列、hover 样式也跟终端右键菜单对不上。
/// 用户要求"所有菜单风格都要像终端右键菜单看齐"，所以这里是一排
/// `MenuButton`，点开的是 `crate::menu` 建的弹层。
pub fn app_menubar(
    win: &gtk::ApplicationWindow,
    tabs: &Rc<crate::tabs::Tabs>,
    toggle_sidebar: &Rc<dyn Fn()>,
    sb: &Rc<crate::sidebar::Sidebar>,
) -> gtk::Box {
    let actions = gio::SimpleActionGroup::new();
    // 注册过的 action 名（启动时跟菜单表对账用，见下）
    let mut registered: Vec<&'static str> = Vec::new();
    // ⚠️ action 本体也留一份：菜单项要按当前状态**禁用**动作
    // （`gio::Action` 接口没有 `set_enabled`，只有 `SimpleAction` 有）
    let mut simple_actions: std::collections::HashMap<&'static str, gio::SimpleAction> =
        std::collections::HashMap::new();

    macro_rules! act {
        ($name:literal, $body:expr) => {{
            let a = gio::SimpleAction::new($name, None);
            let f: Rc<dyn Fn()> = Rc::new($body);
            a.connect_activate({
                let f = f.clone();
                move |_, _| f()
            });
            registered.push($name);
            simple_actions.insert($name, a.clone());
            actions.add_action(&a);
        }};
    }

    // ---- 文件 ----
    //
    // ⚠️ 「新建」的动作**只有一份实现**（`crate::newmenu::run_in`）：顶栏的「＋」、
    // 侧栏的「＋」、子标签的「＋」、欢迎页的按钮、文件菜单、快捷键
    // 全都走它（阶段 68 / 审计 P1-2、P1-6）。
    {
        let t = Rc::clone(tabs);
        act!("new", move || crate::newmenu::run_in(&t, "new"));
    }
    {
        let t = Rc::clone(tabs);
        act!("new_term", move || crate::newmenu::run_in(&t, "new_term"));
    }
    {
        let t = Rc::clone(tabs);
        act!("new_db", move || crate::newmenu::run_in(&t, "new_db"));
    }
    {
        let t = Rc::clone(tabs);
        act!("close", move || {
            t.close_current();
        });
    }
    {
        let t = Rc::clone(tabs);
        act!("import", move || crate::newmenu::run_in(&t, "import"));
    }
    {
        let w = win.clone();
        act!("export", move || {
            let names: Vec<String> = crate::connmgr::load_all()
                .into_iter()
                .filter(|e| e.custom)
                .map(|e| e.name)
                .collect();
            crate::connmgr::ui_export_putty(Some(w.upcast_ref::<gtk::Window>()), names);
        });
    }
    {
        let w = win.clone();
        let sb = Rc::clone(sb);
        act!("clear_recent", move || {
            // ⚠️ 外层是 `Fn` 闭包 ⇒ 不能把捕获的 Rc **移**进内层闭包
            //    （那是 FnOnce 才干的事）⇒ 这里克隆一份。
            let sb2 = Rc::clone(&sb);
            // ⚠️ **先问一句**：最近连接删了就没了（不像连接配置还能重建）
            crate::panel::dialogs::confirm_with(
                Some(w.upcast_ref::<gtk::Window>()),
                tr("清空最近连接？\n\n\
                 「最近连接」里的记录会被全部删掉（已保存的连接配置不受影响）。"),
                tr("清空"),
                move |ok| {
                    if ok {
                        crate::recent::clear();
                        sb2.reload();
                    }
                },
            );
        });
    }
    {
        let w = win.clone();
        act!("quit", move || {
            w.close();
        });
    }

    // ---- 编辑 ----
    {
        let t = Rc::clone(tabs);
        act!("copy", move || {
            if let Some(term) = t.current_terminal() {
                term.copy_selection();
            }
        });
    }
    {
        let t = Rc::clone(tabs);
        act!("paste", move || {
            if let Some(term) = t.current_terminal() {
                term.paste_clipboard();
            }
        });
    }
    act!("find", || {
        if let Some(term) = crate::tabs::current_terminal() {
            toggle_search_for(&term);
        }
    });
    act!("keys", || crate::shortcutsui::open());
    // 通知中心：可回看程序发过的每一条通知（阶段 77c-3 / 审计 P1-9）
    {
        let w = win.clone();
        act!("notify_center", move || {
            crate::notify::open_center(Some(w.upcast_ref::<gtk::Window>()));
        });
    }

    // ---- 连接 / 终端 ----
    // 按作用域拆开：端口转发/数据库/会话选项都挂在当前 SSH 连接上，
    // 归「连接」；分屏/广播/日志/录制/快速命令是终端行为，归「终端」。
    {
        let t = Rc::clone(tabs);
        act!("reconnect", move || {
            if let Some(page) = t.current_page_widget() {
                t.reconnect_page(&page);
            }
        });
    }
    {
        // 终端是另起的 ssh 子进程，与 SFTP 通道不是一回事：
        // 会话结束后只有它能把终端救回来。
        let t = Rc::clone(tabs);
        act!("reconnect_term", move || {
            if let Some(page) = t.current_page_widget() {
                t.reconnect_term_page(&page);
            }
        });
    }
    // 端口转发 / 会话选项是**会话级**动作，不属于某个目录面板：
    // 菜单按当前标签解析作用对象（分屏时取活动面板的 panel state）
    act!("fwd", || {
        if let Some(st) = crate::tabs::current_panel_state() {
            crate::panel::on_open_forwards(&st);
        }
    });
    act!("db", || {
        // ⚠️ **Windows 上这个入口是关着的**（阶段 C5）：首版范围是
        //    「终端 + SFTP + 命令行契约」，DB 工作台在 Windows 上只保证
        //    能编译、没实测 ⇒ 菜单里不显示（见 `crate::topmenus::entry_visible`），
        //    快捷键这条路也在这里拦住，免得「菜单里没有、快捷键却能进」。
        #[cfg(windows)]
        {
            crate::panel::dialogs::info(
                None,
                "数据库工作台",
                "数据库工作台的 Windows 版尚未验证，首版暂不开放。",
            );
        }
        #[cfg(not(windows))]
        if let Some(st) = crate::tabs::current_panel_state() {
            crate::panel::on_open_db(&st);
        }
    });
    act!("sessopt", || {
        if let Some(st) = crate::tabs::current_panel_state() {
            crate::panel::on_open_session_opts(&st);
        }
    });
    // 传输队列是**会话级**窗口（每个会话一个队列）：以前只有 SFTP 文件
    // 面板工具栏上那个图标能进 ⇒ 错过通知就找不到门（审计 P1-9 / P2-18）。
    act!("queue", || {
        if let Some(st) = crate::tabs::current_panel_state() {
            crate::panel::on_open_queue(&st);
        }
    });
    act!("split_lr", || {
        if let Some(page) = crate::tabs::current_page() {
            crate::tabs::split_page(&page, false);
        }
    });
    act!("split_ud", || {
        if let Some(page) = crate::tabs::current_page() {
            crate::tabs::split_page(&page, true);
        }
    });
    act!("close_pane", || {
        if let Some(page) = crate::tabs::current_page() {
            crate::tabs::close_pane_page(&page);
        }
    });
    act!("broadcast", || crate::tabs::toggle_broadcast());
    act!("log", || {
        if let Some(page) = crate::tabs::current_page() {
            crate::tabs::toggle_logging(&page);
        }
    });
    act!("record", || {
        if let Some(page) = crate::tabs::current_page() {
            crate::tabs::toggle_recording(&page);
        }
    });
    act!("replay", || crate::replayui::pick_and_open());
    {
        let t = Rc::clone(tabs);
        act!("quickcmds", move || {
            if let (Some(st), Some(term)) = (t.current_panel_state(), t.current_terminal()) {
                // 等菜单栏弹层关掉再弹快速命令，避免两个 Popover 抢同一个 grab
                glib::idle_add_local_once(move || {
                    crate::panel::on_open_quickcmds(&st, &term, term.widget());
                });
            }
        });
    }

    // ---- 工具 ----
    {
        let w = win.clone();
        act!("keymgr", move || {
            crate::keyui::open(Some(w.upcast_ref::<gtk::Window>()));
        });
    }
    {
        let w = win.clone();
        act!("vault", move || {
            crate::vaultui::open_manager_vault(
                Some(w.upcast_ref::<gtk::Window>()),
                crate::connmgr::load_all(),
            );
        });
    }

    // ---- 视图 / 帮助 ----
    {
        let f = Rc::clone(toggle_sidebar);
        act!("sidebar", move || f());
    }
    // ⚠️ 阶段 69：「外观…」不再开那个独立的小窗口 ⇒ 打开**首选项中心**
    // 的「外观」页（审计 P1-4：设置项散在四个地方，没有首选项）。
    act!("theme", || crate::settingsui::open(crate::settingsui::Page::Appearance));
    act!("prefs", || crate::settingsui::open(crate::settingsui::Page::Appearance));
    act!("about", || crate::about::open());

    // 注册完了 ⇒ 冻成 Rc 给菜单那边查（禁用动作要用）
    let simple_actions = Rc::new(simple_actions);

    win.insert_action_group("app", Some(&actions));

    // ⚠️ **菜单项与 action 对账**：菜单会并来并去（阶段 41 把「编辑」并进
    // 「终端」、「帮助」并进「工具」），最容易出的错是"菜单项还在、action
    // 没了"——点了没反应，界面还不报错。这里启动时对一遍，缺了就打 stderr。
    for (title, label, action) in crate::topmenus::missing_actions(&registered) {
        eprintln!("{}", trf!("[菜单] 顶级菜单「{title}」的「{label}」引用了未注册的 action：{action}", title = title, label = label, action = action));
    }
    // ⚠️ **反向**对账（阶段 74）：注册了却没有任何入口的 action = 孤儿
    // （快捷键还能用，但菜单里找不到它，用户不知道它存在）。
    for action in crate::topmenus::orphan_actions(&registered) {
        eprintln!("{}", trf!("[菜单] action「{action}」注册了，但没有任何菜单入口能点到它", action = action));
    }

    // 一排 Button，点开的是统一构件的弹层
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("pp-menubar");
    let ag = actions.clone().upcast::<gio::ActionGroup>();
    // ⚠️ 顶栏菜单栏用 `crate::menu::MenuBar`：它比"每个按钮自己 popup"多一样东西
    // ——**悬停切换**（鼠标移到另一个标题上，那个菜单自动打开，不用点第二下）。
    // 弹层是**非模态**的（模态会 grab 指针，motion 到不了别的按钮）。
    let menubar = crate::menu::MenuBar::new();
    for (title, entries) in TOP_MENUS {
        // ⚠️ 用**普通 Button**而不是 `MenuButton`：MenuButton 自带一个小三角
        // （GTK 4.18 里那不是普通 image 节点，CSS 压不掉），菜单栏里显得多余。
        // 普通 Button 配一个挂在它上面的 Popover，观感与经典菜单栏一致。
        // ⚠️ 菜单标题在 const 表里（编译期求值 ⇒ 不能调 tr()）⇒ 显示时翻译。
        let (btn, pop) = menubar.item(tr(*title));
        // ⚠️ 正文交给 `crate::topbar::build_body`（阶段 74）：这段原来在 main.rs 里，
        // 冒烟**测不到**（例子链不到 bin）⇒ 「快捷键列」「没有终端时灰掉」
        // 只能靠人眼看。搬到 lib 后冒烟能直接验。
        let m = crate::topbar::build_body(
            entries,
            &simple_actions,
            &pop,
            win.upcast_ref::<gtk::Window>(),
        );
        pop.set_child(Some(&m.body));
        bar.append(&btn);
    }
    bar
}

/// **顶栏**（侧栏开关 + 应用名 + 菜单栏）。
///
/// ⚠️ **为什么是独立函数**：界面语言要能**实时切换**（用户 2026-11-09：
/// 「切换语言也应该实时生效」）⇒ 语言一变就把顶栏（连菜单栏）**重建
/// 一遍** ⇒ 这段必须能单独再跑一次。
pub fn build_header(
    win: &gtk::ApplicationWindow,
    tabs: &Rc<crate::tabs::Tabs>,
    toggle_sidebar: &Rc<dyn Fn()>,
    sb: &Rc<crate::sidebar::Sidebar>,
    header_title: &gtk::Label,
) -> gtk::HeaderBar {
    let header = gtk::HeaderBar::new();
    header.add_css_class("pp-header");

    let b_side = crate::a11y::icon_button("sidebar-show-symbolic");
    b_side.add_css_class("flat");
    b_side.set_tooltip_text(Some(tr("显示/隐藏 会话列表 (Ctrl+Shift+B)")));
    {
        let t = Rc::clone(toggle_sidebar);
        b_side.connect_clicked(move |_| t());
    }
    header.pack_start(&b_side);

    header.set_title_widget(Some(header_title));

    // ⚠️ 顶栏**不再有「＋新建」按钮**（阶段 92c，用户 2026-11-01：
    // 「顶栏的加号也可以去掉了」）。
    //
    // 理由跟凭据库徽标一样——**同一个入口出现两遍**：侧栏标题栏已经有一
    // 个「＋」（阶段 68 起它就是「新建」菜单的入口，菜单项与顶栏那份
    // **完全相同**：`NewEntry::items()` 对 TopBar / Sidebar 都给全集），
    // 顶栏再放一个就是重复 ⇒ 删掉，功能一点不丢。
    //
    // 顶栏右端现在只剩：窗口按钮。

    // ⚠️ 这里**不再放凭据库状态标志**（阶段 92）。
    //
    // 以前顶栏与**侧栏标题栏**各有一个 `crate::vaultui::badge_button()` ⇒ 同一
    // 个状态在屏幕上出现**两次**（用户 2026-11-01 报「界面上有两处『凭证库
    // 已锁定』是否重复」）。
    //
    // 留的是**侧栏那个**：阶段 77a/77b 的既定设计——连接管理器删掉后，
    // "我到底开没开凭据库"就挪到侧栏标题栏（那儿是配连接、勾「记住密码」
    // 的地方），两个冒烟也一直盯着它。顶栏这个是更早的遗留，阶段 77a
    // 挪位时忘了删 ⇒ 这次补上。
    //
    // 代价：侧栏收起时看不到凭据库状态。但侧栏收起时用户在看终端/
    // 工作台，不是配连接的时候；真要查，工具 → 凭据库管理器里也有。

    // ⚠️ **这里原来有一个「全局状态区」chip**（阶段 77c-2 / 审计 P1-9），
    // 正常时只显示「已连接」三个字。
    //
    // **用户 2026-11-07 明确要求删掉**：它**不指示是哪一条连接**——
    // 连着多台服务器时，"已连接"说的是哪台根本说不清；而"连上了"这件事
    // 本身也不是什么需要一直挂在顶栏的消息。
    //
    // 删了之后信息**没丢**：每个标签头上本来就有**聚合状态点**
    // （`crate::tabs::update_dot`：● 已连 / ◐ 连接中 / ○ 还没连，tooltip 说清
    // 是文件面板、终端还是数据库出的问题）⇒ "哪台离线了"在**对应的标签
    // 上**看得见，正是用户要的位置。

    // 菜单栏（文件/编辑/会话/视图/帮助）：入口不再只有图标，
    // 与 PuTTY 用户的菜单使用习惯也对得上
    let menubar = app_menubar(win, tabs, toggle_sidebar, sb);
    header.pack_start(&menubar);

    header
}

/// 从终端 widget 向上找同级 SearchBar 并切换（避免 Tab 持 TermPane 的循环依赖）。
pub fn toggle_search_for(term: &crate::termview::TermView) {
    let mut w: Option<gtk::Widget> = term.widget().parent();
    while let Some(p) = w {
        if let Some(vbox) = p.downcast_ref::<gtk::Box>() {
            if vbox.orientation() == gtk::Orientation::Vertical {
                let mut c = vbox.first_child();
                while let Some(child) = c {
                    if let Some(bar) = child.downcast_ref::<gtk::SearchBar>() {
                        if bar.is_search_mode() {
                            bar.set_search_mode(false);
                            term.grab_focus();
                        } else {
                            bar.set_search_mode(true);
                            if let Some(e) = bar.child() {
                                if let Some(entry) = e.downcast_ref::<gtk::Entry>() {
                                    entry.grab_focus();
                                }
                            }
                        }
                        return;
                    }
                    c = child.next_sibling();
                }
            }
        }
        w = p.parent();
    }
}
