//! PuTTYPlus — PuTTY 兼容的 SSH 终端 + SFTP 图形客户端（Rust 版入口）。
//! 主窗 = 会话侧边栏 + 多标签（每标签一套 SFTP 面板 + 终端）。

use std::rc::Rc;

use puttyplus::{
    about, appicon, args, backend, bridge, connmgr, density, keyui, menu, notify, panel, replayui,
    settings, settingsui, sidebar, shortcuts, shortcutsui, statusbar, tabs, term, theme, topbar,
    topmenus, util, vault, vaultui,
};
use topmenus::{MenuEntry, TOP_MENUS};

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use vte4::prelude::*;

const APP_TITLE: &str = "PuTTYPlus";

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    // askpass 模式：被 ssh 调用输出密码，不做任何 GUI 初始化
    if term::run_askpass(&argv[1..]) {
        return; // run_askpass 内部已 exit(0)；此处仅为可读性
    }

    // 终端桥模式：telnet/串口会话的 VTE 子进程就是本程序
    match bridge::run_if_bridge(&argv[1..]) {
        Ok(true) => return, // 桥退出（PTY 关闭）即会话结束
        Ok(false) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }

    // 与 C 版一致：setlocale(LC_ALL, "")，使 strerror 等按 locale 本地化
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }

    /* 参数解析必须在 GUI 初始化之前：堡垒机生成的临时会话/密码文件
     * 会在客户端启动后立即被删除，GTK 初始化有显示服务器往返耗时。
     * 分支顺序与 C 版 main 一致：parse → --print-config → --sftp-selftest */
    let mut warnings: Vec<String> = Vec::new();
    let mut opts = args::ConnOptions::parse(&argv[1..], &mut warnings);
    // spawn 出来的子进程：密码经环境变量传递（不进 argv）
    if opts.password.is_none() {
        opts.password = connmgr::password_from_env();
    }
    // ⚠️ **命令行启动的连接要不要记进「最近连接」**：
    //    堡垒机的 launcher 每次拉起都是一条一次性的设备连接 ⇒ 默认**不记**
    //    （否则「最近」列表全是一次性连接，自己常用的那条得翻很久）。
    //    「在新窗口打开」是用户在界面里点的 ⇒ 照记（它带内部标记）。
    let internal_spawn = std::env::var_os(args::INTERNAL_SPAWN_ENV).is_some();
    opts.record_recent = internal_spawn || settings::record_cli_recent();

    if argv.get(1).map(String::as_str) == Some("--print-config") {
        // 与 C 版一致：调试输出，警告不打印
        opts.print();
        return;
    }

    if argv.get(1).map(String::as_str) == Some("--sftp-selftest") {
        if argv.len() < 5 {
            // i18n:keep
            // ⚠️ 这句是**命令行契约**（C 版逐字相同，compat_check 要对照）
            // ⇒ 保持中文，不翻译。
            eprintln!(
                "用法: {} --sftp-selftest <host> <port> <user> [password]",
                argv[0]
            );
            // i18n:end
            std::process::exit(2);
        }
        let pw = argv.get(5).map(String::as_str);
        // 与 C 版一致：atoi 语义（非法端口得 0，连接失败）
        let port = util::atoi_like(&argv[3]) as i32;
        // argv[6] 可选：ProxyCommand（Rust 扩展，SFTP 代理验证用）
        let proxycmd = argv.get(6).map(String::as_str);
        let code = backend::selftest::run(&argv[2], port, &argv[4], pw, proxycmd);
        std::process::exit(code.clamp(0, 255) as i32);
    }

    // 界面语言：必须在**建任何界面之前**读（菜单表、页签标题都是建的时候
    // 就把文案定下来的 ⇒ 之后改语言要重启，见 settingsui 的「界面语言」）
    puttyplus::i18n::set_lang(settings::language());

    let app = gtk::Application::builder()
        .application_id("org.puttyplus.PuttyPlus")
        // C 版（GTK3 gtk_init）没有单实例语义：堡垒机每次拉起
        // 都是新进程新窗口。必须 NON_UNIQUE，否则二次启动会被转发给
        // 已有实例（哪怕主窗已关、只剩队列窗的僵尸实例）然后静默退出。
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let has_host = !opts.need_dialog();
    let opts = std::sync::Arc::new(opts);
    let warnings = std::sync::Arc::new(warnings);
    app.connect_activate(move |app| {
        let (tabs, win) = build_main_window(app);
        // 主题/密度要在建界面之前套好，否则首帧先画成默认样式再跳一次
        theme::load_saved();
        density::load_saved();
        if has_host {
            tabs.open(&opts);
            // 首个标签 append 不触发 switch_page，这里手动同步窗口标题
            sync_window_title(&win, &tabs);
        }
        // 无参启动：直接显示主界面（欢迎页）。侧栏由
        // 标签「+」/侧边栏「管理」按需打开，不再自动弹出
        if !warnings.is_empty() {
            let msg = format!(
                "以下 PuTTY 参数被忽略或需要留意：\n\n{}",
                warnings.join("\n")
            );
            term::alert(None::<&gtk::Window>, &msg);
        }
    });
    // 不把 PuTTY 风格的命令行参数转交给 GTK 自行解析
    std::process::exit(i32::from(app.run_with_args::<&str>(&[])));
}

fn build_main_window(app: &gtk::Application) -> (Rc<tabs::Tabs>, gtk::ApplicationWindow) {
    // 窗口尺寸记忆：上次关窗时的宽高（最大化状态下不记，见 save_window_size）
    let (w0, h0) = settings::window_size("main").unwrap_or((1200, 780));
    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .default_width(w0)
        .default_height(h0)
        .build();

    let tabs = tabs::Tabs::new();
    let sb = sidebar::Sidebar::new(Rc::clone(&tabs));
    sb.setup();

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    let sb_wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // 只给一个最小宽度；实际宽度由分割位置（可拖、可记忆）决定
    sb_wrap.set_size_request(180, -1);
    sb_wrap.append(&sb.widget);
    paned.set_start_child(Some(&sb_wrap));
    paned.set_end_child(Some(&tabs.notebook));
    paned.set_position(settings::sidebar_width());
    // 会话列表的宽度上限：给终端留出大部分窗口
    settings::clamp_paned_start(&paned, 0.28, 320);
    win.set_child(Some(&paned));

    // 侧边栏显隐开关（顶栏按钮 + Ctrl+Shift+B；状态持久化）
    let toggle_sidebar: Rc<dyn Fn()> = {
        let sb = sb_wrap.clone();
        Rc::new(move || {
            let next = !sb.is_visible();
            sb.set_visible(next);
            settings::set_show_sidebar(next);
        })
    };
    if !settings::show_sidebar() {
        sb_wrap.set_visible(false);
    }

    // ---- 顶栏：侧边栏开关 + 应用名 + 新建连接 + 应用菜单 ----
    // 原来这些入口散在"标签栏两端的图标 + 侧边栏底部四个图标"里，
    // 现在收成一条 header bar：主操作在右、低频入口进菜单。
    //
    // ⚠️ 顶栏标题**跟着当前标签走**（阶段 75 / 审计 P1-14）：以前这里
    // 固定写死 "PuTTYPlus"，多窗口 / 多会话时从窗口顶部分不出是哪台机器。
    let header_title = gtk::Label::new(Some(APP_TITLE));
    header_title.add_css_class("pp-app-title");
    header_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header_title.set_max_width_chars(30);
    header_title.set_tooltip_text(Some(APP_TITLE));
    {
        let header = gtk::HeaderBar::new();
        header.add_css_class("pp-header");

        let b_side = puttyplus::a11y::icon_button("sidebar-show-symbolic");
        b_side.add_css_class("flat");
        b_side.set_tooltip_text(Some("显示/隐藏 会话列表 (Ctrl+Shift+B)"));
        {
            let t = toggle_sidebar.clone();
            b_side.connect_clicked(move |_| t());
        }
        header.pack_start(&b_side);

        header.set_title_widget(Some(&header_title));

        // ⚠️ 顶栏**不再有「＋新建」按钮**（阶段 92c，用户 2026-11-01：
        // 「顶栏的加号也可以去掉了」）。
        //
        // 理由跟凭据库徽标一样——**同一个入口出现两遍**：侧栏标题栏已经有一
        // 个「＋」（阶段 68 起它就是「新建」菜单的入口，菜单项与顶栏那份
        // **完全相同**：`NewEntry::items()` 对 TopBar / Sidebar 都给全集），
        // 顶栏再放一个就是重复 ⇒ 删掉，功能一点不丢。
        //
        // 顶栏右端现在只剩：全局状态 chip + 窗口按钮。

        // ⚠️ 这里**不再放凭据库状态标志**（阶段 92）。
        //
        // 以前顶栏与**侧栏标题栏**各有一个 `vaultui::badge_button()` ⇒ 同一
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

        // **全局状态区**（阶段 77c-2 / 审计 P1-9）：顶栏常驻一个 chip，
        // 显示**当前窗口当前会话**的聚合状态，tooltip 里把四个维度
        // （文件面板 / 终端 / 数据库 / 传输）逐条说清。
        //
        // ⚠️ 以前这四个状态**散在四个地方**（标签点 / 终端 banner /
        // DB 状态行 / 传输队列窗口），用户想知道"现在到底怎么了"得
        // 一个一个去找。
        {
            let b_status = statusbar::build(Rc::clone(&tabs));
            header.pack_end(&b_status);
        }

        // 菜单栏（文件/编辑/会话/视图/帮助）：入口不再只有图标，
        // 与 PuTTY 用户的菜单使用习惯也对得上
        let menubar = app_menubar(&win, &tabs, &toggle_sidebar, &sb);
        header.pack_start(&menubar);

        win.set_titlebar(Some(&header));
    }

    // 窗口标题跟随当前标签（远程 OSC 标题 → 标签头 → 此处）
    {
        let tabs = Rc::clone(&tabs);
        let win2 = win.clone();
        let ht = header_title.clone();
        let nb = tabs.notebook.clone();
        nb.connect_switch_page(move |_nb, page, _num| {
            let title = tabs
                .tabs
                .borrow()
                .iter()
                .find(|t| t.page == *page)
                .map(|t| t.label.text().to_string())
                .unwrap_or_else(|| APP_TITLE.to_string()); // 欢迎页
            win2.set_title(Some(&format!("{title} — {APP_TITLE}")));
            // ⚠️ 阶段 75：顶栏里的标题也跟着走（欢迎页回落应用名）
            ht.set_text(&title);
            ht.set_tooltip_text(Some(&title));
            // ⚠️ 状态区跟着当前标签走（阶段 77c-2）：状态区显示的是
            // **当前会话**的状态 ⇒ 切了标签就得刷新。
            statusbar::refresh_all();
        });
    }

    // 关窗 = 关闭所有标签（取消传输/停 worker/杀 ssh）
    {
        let tabs = Rc::clone(&tabs);
        let paned = paned.clone();
        win.connect_close_request(move |w| {
            // 关窗前记住用户拉过的尺寸与侧栏宽度
            settings::save_window_size("main", w);
            settings::set_sidebar_width(paned.position());
            // 「上次关闭时的本地目录」= 当前标签的本地目录。先取出来：
            // 下面 close_page 会逐个释放面板状态。
            let last_local = tabs
                .current_panel_state()
                .map(|st| st.local_cwd.borrow().clone());
            let pages: Vec<gtk::Widget> =
                tabs.tabs.borrow().iter().map(|t| t.page.clone()).collect();
            let panels: Vec<Rc<panel::PanelState>> = tabs
                .tabs
                .borrow()
                .iter()
                .map(|t| Rc::clone(&t.panel_st))
                .collect();
            let tabs2 = Rc::clone(&tabs);
            let w2 = w.clone();
            // ⚠️ 一次问完所有标签（阶段 63 / P0-1）：以前是直接关，
            // 用户改了没保存的文件、没提交的数据库改动，说没就没。
            //
            // 选「保存并关闭」时保存还没完 ⇒ 返回 Stop（窗口先别关，
            // 关了 worker 就死了、存的全丢）；等保存的回包齐了再关。
            let pages = Rc::new(pages);
            let last_local = Rc::new(last_local);
            let close_pages = move || {
                for p in pages.iter() {
                    tabs2.close_page_forced(p);
                }
                if let Some(dir) = last_local.as_ref() {
                    panel::remember_local_dir(dir);
                }
            };
            let close_pages2 = close_pages.clone();
            match panel::closeguard::request_close_window(
                &panels,
                close_pages,
                move || {
                    close_pages2();
                    // 存完了 ⇒ 这一趟主动关窗（再进来时已经干净了，
                    // 走"立刻关"那条路，不会递归）
                    w2.close();
                },
            ) {
                panel::closeguard::CloseFlow::Proceed => glib::Propagation::Proceed,
                panel::closeguard::CloseFlow::Stop => glib::Propagation::Stop,
                // 还在保存 ⇒ 窗口先别关
                panel::closeguard::CloseFlow::Saving => glib::Propagation::Stop,
            }
        });
    }

    // 全局快捷键（捕获阶段）：**由 shortcuts::ALL 驱动**，表是唯一事实来源。
    // 以前这里是手写的一串 if，改键位要同时改这里 + 各处 tooltip + 文档，
    // 漏一处就是"文档说 A、实际要按 B"。现在表、行为、cheatsheet 同源。
    {
        let tabs = Rc::clone(&tabs);
        let toggle_sidebar = toggle_sidebar.clone();
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _code, mods| {
            let name = keyval.name().unwrap_or_default();
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = mods.contains(gdk::ModifierType::ALT_MASK);
            for b in shortcuts::window_bindings() {
                if !b.matches(&name, ctrl, shift, alt) {
                    continue;
                }
                match b.id {
                    shortcuts::Id::NewTab => {
                        tabs::TabsHandle(Rc::clone(&tabs)).prompt_new();
                    }
                    shortcuts::Id::CloseTab => {
                        tabs.close_current();
                    }
                    shortcuts::Id::ToggleSidebar => toggle_sidebar(),
                    shortcuts::Id::ToggleSearch => {
                        // ⚠️ 编辑器页签里的 Ctrl+F 是**编辑器自己的查找**
                        // （阶段 84），不是终端查找——用户正停在编辑器上时
                        // 弹出终端的查找条会让人一头雾水（而且终端查找条
                        // 根本搜不到编辑器里的字）。
                        if let Some(st) = tabs::current_panel_state() {
                            if panel::fileedit::handle_find_key(&st) {
                                return glib::Propagation::Stop;
                            }
                        }
                        if let Some(term) = tabs.current_terminal() {
                            toggle_search_for(&term);
                            return glib::Propagation::Stop;
                        }
                    }
                    shortcuts::Id::Copy => {
                        if let Some(term) = tabs.current_terminal() {
                            term.copy_clipboard_format(vte4::Format::Text);
                        }
                    }
                    shortcuts::Id::Paste => {
                        if let Some(term) = tabs.current_terminal() {
                            <vte4::Terminal as vte4::TerminalExt>::paste_clipboard(&term);
                        }
                    }
                    shortcuts::Id::ToggleBroadcast => tabs::toggle_broadcast(),
                    shortcuts::Id::ShowCheatsheet => shortcutsui::open(),
                    shortcuts::Id::OpenReplay => replayui::pick_and_open(),
                    shortcuts::Id::OpenDatabase => {
                        if let Some(st) = tabs::current_panel_state() {
                            panel::on_open_db(&st);
                        }
                    }
                    shortcuts::Id::QuickCommands => {
                        // 快速命令是"往终端里敲"，作用对象必须是**当前活动终端**；
                        // 分屏后 tab 里有多个终端，不能拿标签里第一个凑数
                        if let (Some(st), Some(term)) =
                            (tabs::current_panel_state(), tabs.current_terminal())
                        {
                            panel::on_open_quickcmds(
                                &st,
                                &term,
                                term.upcast_ref::<gtk::Widget>(),
                            );
                        }
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win.add_controller(key);
    }

    // 关于 / 快捷键一览对话框要挂在主窗口上：侧边栏拿不到它，
    // 登记成全局（F1 的 cheatsheet 也走这里）
    //
    // ⚠️ 阶段 69：「外观」窗口升级成**首选项中心**（六页）⇒ 不再是模态
    // 对话框，而是工具窗（走 `windows::open_or_focus`）⇒ 不需要父窗口。
    // 新建/管理对话框的默认父窗口（按钮、菜单、欢迎页都能拉起它们）
    about::set_parent(win.clone().upcast());
    shortcutsui::set_parent(win.clone().upcast());
    // 6.1 回放窗与文件选择器同样要挂在主窗口上
    replayui::set_parent(win.clone().upcast());
    // 4.5 界面密度/字号：类挂主窗口上，启动时按已保存的设置套用
    density::set_window(win.clone().upcast());
    // 4.2 应用图标：内嵌 PNG 落到缓存目录并挂进图标主题（gtk4-rs 没有
    // set_default_icon(Texture) 绑定，只能走 IconTheme 搜索路径）
    if let Some(name) = appicon::install() {
        win.set_icon_name(Some(name));
    }

    win.present();
    (tabs, win)
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
fn app_menubar(
    win: &gtk::ApplicationWindow,
    tabs: &Rc<tabs::Tabs>,
    toggle_sidebar: &Rc<dyn Fn()>,
    sb: &Rc<sidebar::Sidebar>,
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
    // ⚠️ 「新建」的动作**只有一份实现**（`newmenu::run_in`）：顶栏的「＋」、
    // 侧栏的「＋」、子标签的「＋」、欢迎页的按钮、文件菜单、快捷键
    // 全都走它（阶段 68 / 审计 P1-2、P1-6）。
    {
        let t = Rc::clone(tabs);
        act!("new", move || puttyplus::newmenu::run_in(&t, "new"));
    }
    {
        let t = Rc::clone(tabs);
        act!("new_term", move || puttyplus::newmenu::run_in(&t, "new_term"));
    }
    {
        let t = Rc::clone(tabs);
        act!("new_db", move || puttyplus::newmenu::run_in(&t, "new_db"));
    }
    {
        let t = Rc::clone(tabs);
        act!("close", move || {
            t.close_current();
        });
    }
    {
        let t = Rc::clone(tabs);
        act!("import", move || puttyplus::newmenu::run_in(&t, "import"));
    }
    {
        let w = win.clone();
        act!("export", move || {
            let names: Vec<String> = connmgr::load_all()
                .into_iter()
                .filter(|e| e.custom)
                .map(|e| e.name)
                .collect();
            connmgr::ui_export_putty(Some(w.upcast_ref::<gtk::Window>()), names);
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
                "清空最近连接？\n\n\
                 「最近连接」里的记录会被全部删掉（已保存的连接配置不受影响）。",
                "清空",
                move |ok| {
                    if ok {
                        puttyplus::recent::clear();
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
                term.copy_clipboard_format(vte4::Format::Text);
            }
        });
    }
    {
        let t = Rc::clone(tabs);
        act!("paste", move || {
            if let Some(term) = t.current_terminal() {
                <vte4::Terminal as vte4::TerminalExt>::paste_clipboard(&term);
            }
        });
    }
    act!("find", || {
        if let Some(term) = tabs::current_terminal() {
            toggle_search_for(&term);
        }
    });
    act!("keys", || shortcutsui::open());
    // 通知中心：可回看程序发过的每一条通知（阶段 77c-3 / 审计 P1-9）
    {
        let w = win.clone();
        act!("notify_center", move || {
            notify::open_center(Some(w.upcast_ref::<gtk::Window>()));
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
        if let Some(st) = tabs::current_panel_state() {
            panel::on_open_forwards(&st);
        }
    });
    act!("db", || {
        if let Some(st) = tabs::current_panel_state() {
            panel::on_open_db(&st);
        }
    });
    act!("sessopt", || {
        if let Some(st) = tabs::current_panel_state() {
            panel::on_open_session_opts(&st);
        }
    });
    // 传输队列是**会话级**窗口（每个会话一个队列）：以前只有 SFTP 文件
    // 面板工具栏上那个图标能进 ⇒ 错过通知就找不到门（审计 P1-9 / P2-18）。
    act!("queue", || {
        if let Some(st) = tabs::current_panel_state() {
            panel::on_open_queue(&st);
        }
    });
    act!("split_lr", || {
        if let Some(page) = tabs::current_page() {
            tabs::split_page(&page, false);
        }
    });
    act!("split_ud", || {
        if let Some(page) = tabs::current_page() {
            tabs::split_page(&page, true);
        }
    });
    act!("close_pane", || {
        if let Some(page) = tabs::current_page() {
            tabs::close_pane_page(&page);
        }
    });
    act!("broadcast", || tabs::toggle_broadcast());
    act!("log", || {
        if let Some(page) = tabs::current_page() {
            tabs::toggle_logging(&page);
        }
    });
    act!("record", || {
        if let Some(page) = tabs::current_page() {
            tabs::toggle_recording(&page);
        }
    });
    act!("replay", || replayui::pick_and_open());
    {
        let t = Rc::clone(tabs);
        act!("quickcmds", move || {
            if let (Some(st), Some(term)) = (t.current_panel_state(), t.current_terminal()) {
                // 等菜单栏弹层关掉再弹快速命令，避免两个 Popover 抢同一个 grab
                glib::idle_add_local_once(move || {
                    panel::on_open_quickcmds(&st, &term, term.upcast_ref::<gtk::Widget>());
                });
            }
        });
    }

    // ---- 工具 ----
    {
        let w = win.clone();
        act!("keymgr", move || {
            keyui::open(Some(w.upcast_ref::<gtk::Window>()));
        });
    }
    {
        let w = win.clone();
        act!("vault", move || {
            vaultui::open_manager_vault(
                Some(w.upcast_ref::<gtk::Window>()),
                connmgr::load_all(),
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
    act!("theme", || settingsui::open(settingsui::Page::Appearance));
    act!("prefs", || settingsui::open(settingsui::Page::Appearance));
    act!("about", || about::open());

    // 注册完了 ⇒ 冻成 Rc 给菜单那边查（禁用动作要用）
    let simple_actions = Rc::new(simple_actions);

    win.insert_action_group("app", Some(&actions));

    // ⚠️ **菜单项与 action 对账**：菜单会并来并去（阶段 41 把「编辑」并进
    // 「终端」、「帮助」并进「工具」），最容易出的错是"菜单项还在、action
    // 没了"——点了没反应，界面还不报错。这里启动时对一遍，缺了就打 stderr。
    for (title, label, action) in topmenus::missing_actions(&registered) {
        eprintln!("[菜单] 顶级菜单「{title}」的「{label}」引用了未注册的 action：{action}");
    }
    // ⚠️ **反向**对账（阶段 74）：注册了却没有任何入口的 action = 孤儿
    // （快捷键还能用，但菜单里找不到它，用户不知道它存在）。
    for action in topmenus::orphan_actions(&registered) {
        eprintln!("[菜单] action「{action}」注册了，但没有任何菜单入口能点到它");
    }

    // 一排 Button，点开的是统一构件的弹层
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("pp-menubar");
    let ag = actions.clone().upcast::<gio::ActionGroup>();
    // ⚠️ 顶栏菜单栏用 `menu::MenuBar`：它比"每个按钮自己 popup"多一样东西
    // ——**悬停切换**（鼠标移到另一个标题上，那个菜单自动打开，不用点第二下）。
    // 弹层是**非模态**的（模态会 grab 指针，motion 到不了别的按钮）。
    let menubar = menu::MenuBar::new();
    for (title, entries) in TOP_MENUS {
        // ⚠️ 用**普通 Button**而不是 `MenuButton`：MenuButton 自带一个小三角
        // （GTK 4.18 里那不是普通 image 节点，CSS 压不掉），菜单栏里显得多余。
        // 普通 Button 配一个挂在它上面的 Popover，观感与经典菜单栏一致。
        let (btn, pop) = menubar.item(title);
        // ⚠️ 正文交给 `topbar::build_body`（阶段 74）：这段原来在 main.rs 里，
        // 冒烟**测不到**（例子链不到 bin）⇒ 「快捷键列」「没有终端时灰掉」
        // 只能靠人眼看。搬到 lib 后冒烟能直接验。
        let m = topbar::build_body(
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

/// 窗口标题 = 当前标签标题（欢迎页回落 APP_TITLE）。/// 窗口标题 = 当前标签标题（欢迎页回落 APP_TITLE）。
fn sync_window_title(win: &gtk::ApplicationWindow, tabs: &Rc<tabs::Tabs>) {
    let title = tabs
        .current_title()
        .unwrap_or_else(|| APP_TITLE.to_string());
    win.set_title(Some(&format!("{title} — {APP_TITLE}")));
}

/// 从终端 widget 向上找同级 SearchBar 并切换（避免 Tab 持 TermPane 的循环依赖）。
fn toggle_search_for(term: &vte4::Terminal) {
    let mut w: Option<gtk::Widget> = term.upcast_ref::<gtk::Widget>().parent();
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
