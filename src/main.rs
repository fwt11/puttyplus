//! PuTTYPlus — PuTTY 兼容的 SSH 终端 + SFTP 图形客户端（Rust 版入口）。
//! 主窗 = 会话侧边栏 + 多标签（每标签一套 SFTP 面板 + 终端）。

use std::rc::Rc;

use puttyplus::{
    about, appicon, args, backend, bridge, connmgr, density, keyui, menu, notify, panel, platform,
    replayui, settings, settingsui, sidebar, shortcuts, shortcutsui, tabs, term, theme, topbar,
    topmenus, util, vault, vaultui,
};
use topmenus::{MenuEntry, TOP_MENUS};

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::i18n::tr;
use puttyplus::trf;

const APP_TITLE: &str = "PuTTYPlus";

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    // ⚠️ 程序名必须设（`g_set_prgname`）—— 不设的话**每开一个终端都会往
    // stderr 喷一条 GLib-CRITICAL**（VTE 0.80 的 systemd-scope 那条路要
    // `g_get_prgname()`，没设过就是 NULL）。详见 util::init_prgname。
    puttyplus::util::init_prgname();

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
    // （非 Unix 平台不走这条路，见 platform::locale 的说明）
    crate::platform::locale::init();

    // ⚠️ 阶段 C8：清掉**陈旧**的会话级密码文件（上次崩了留下的）
    // —— 里面是明文密码，不能一直躺在盘上。
    puttyplus::pwcache::cleanup_stale_session_files();

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
            eprintln!("用法: {} --sftp-selftest <host> <port> <user> [password]",
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
            let msg = trf!("以下 PuTTY 参数被忽略或需要留意：\n\n{v0}", v0 = warnings.join("\n"));
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
    // 原来这些入口散在"标签栏两端的图标 + 侧栏底部四个图标"里，
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
        let header = topbar::build_header(&win, &tabs, &toggle_sidebar, &sb, &header_title);
        win.set_titlebar(Some(&header));
    }

    // ---- 语言变了 ⇒ 重建「界面外壳」（顶栏 + 菜单 + 侧栏）----
    //
    // ⚠️ **终端标签页不动**：终端在跑（SSH 会话、传输队列都在里面），
    // 重建会把它们全弄死 ⇒ 只换「外壳」（顶栏、菜单栏、侧栏）。
    // 标签页里的零散文案（状态行 / 查找条）保持旧语言，新开的终端就是
    // 新语言。
    {
        let win2 = win.clone();
        let tabs2 = Rc::clone(&tabs);
        let ts2 = Rc::clone(&toggle_sidebar);
        let sb_wrap2 = sb_wrap.clone();
        let alive = puttyplus::i18n::add_lang_hook(move || {
            // 侧栏：换一个新的（旧的还挂在 sb_wrap 里 ⇒ 先摘掉）
            let sb_new = sidebar::Sidebar::new(Rc::clone(&tabs2));
            sb_new.setup();
            while let Some(c) = sb_wrap2.first_child() {
                sb_wrap2.remove(&c);
            }
            sb_wrap2.append(&sb_new.widget);
            // 顶栏 + 菜单栏（菜单文案也是新的语言）
            let title = gtk::Label::new(Some(APP_TITLE));
            title.add_css_class("pp-app-title");
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(30);
            title.set_tooltip_text(Some(APP_TITLE));
            let header = topbar::build_header(&win2, &tabs2, &ts2, &sb_new, &title);
            win2.set_titlebar(Some(&header));
        });
        win.connect_destroy(move |_| alive.set(false));
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
                            topbar::toggle_search_for(&term);
                            return glib::Propagation::Stop;
                        }
                    }
                    shortcuts::Id::Copy => {
                        if let Some(term) = tabs.current_terminal() {
                            term.copy_selection();
                        }
                    }
                    shortcuts::Id::Paste => {
                        if let Some(term) = tabs.current_terminal() {
                            term.paste_clipboard();
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
                                term.widget(),
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



/// 窗口标题 = 当前标签标题（欢迎页回落 APP_TITLE）。/// 窗口标题 = 当前标签标题（欢迎页回落 APP_TITLE）。
fn sync_window_title(win: &gtk::ApplicationWindow, tabs: &Rc<tabs::Tabs>) {
    let title = tabs
        .current_title()
        .unwrap_or_else(|| APP_TITLE.to_string());
    win.set_title(Some(&format!("{title} — {APP_TITLE}")));
}

