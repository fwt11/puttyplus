//! 首选项中心（阶段 69 / 审计 P1-4）。
//!
//! **问题**：设置项散在**四个地方**——「外观」窗口（界面主题/密度/字号/
//! 终端配色）、「会话选项」（保活/TERM/认证顺序/终端字号/响铃）、连接编辑
//! 器、传输队列窗口顶部（断点续传/SHA-1/并发数），再加上文件列表列设置、
//! 窗口尺寸、侧栏宽度各在自己入口 ⇒ 用户想调一个设置得先知道它在哪。
//!
//! 现在：**一个窗口六个页**（外观 / 终端 / 传输 / 安全 / 快捷键 / 高级），
//! 外加一张**注册表** [`SETTINGS`]——每个设置项**有且只有一个归属页**
//! （单测 + 冒烟都查这一点）。
//!
//! ⚠️ **页的归属是"规定"，不是"碰巧"**：注册表里的 `page` 是唯一的真相，
//! 改设置项落在哪一页 ⇒ 改注册表 + 改那一页的界面，两处一起改。

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::density::{self, Density, UiFont};
use crate::i18n::{tr, tr_dyn, Lang};
use crate::theme::{self, Palette, Theme};
use crate::windows::ToolKind;
use crate::trf;

/// 首选项的**六个页**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// 外观：界面主题 / 密度 / 界面字号
    Appearance,
    /// 终端：终端配色 / 终端字号
    Terminal,
    /// 传输：断点续传 / 校验 / 并发数
    Transfer,
    /// 安全：凭据库 / 私钥
    Security,
    /// 快捷键：一览
    Shortcuts,
    /// 高级：布局尺寸 / 列显示 / 镜像规则
    Advanced,
}

impl Page {
    /// 页签上的名字。
    pub fn title(self) -> &'static str {
        match self {
            Page::Appearance => tr("外观"),
            Page::Terminal => tr("终端"),
            Page::Transfer => tr("传输"),
            Page::Security => tr("安全"),
            Page::Shortcuts => tr("快捷键"),
            Page::Advanced => tr("高级"),
        }
    }

    /// 全部六页（顺序 = 页签顺序）。
    pub fn all() -> [Page; 6] {
        [
            Page::Appearance,
            Page::Terminal,
            Page::Transfer,
            Page::Security,
            Page::Shortcuts,
            Page::Advanced,
        ]
    }

    /// 页序号（Notebook 用）。
    pub fn index(self) -> u32 {
        match self {
            Page::Appearance => 0,
            Page::Terminal => 1,
            Page::Transfer => 2,
            Page::Security => 3,
            Page::Shortcuts => 4,
            Page::Advanced => 5,
        }
    }

    /// 按页序号取页。
    pub fn from_index(i: u32) -> Page {
        Page::all()
            .into_iter()
            .find(|p| p.index() == i)
            .unwrap_or(Page::Appearance)
    }
}

/// 一个设置项（**注册表**的一行）。
///
/// **为什么要注册表**：设置项散在各处时，"这个设置归哪儿"没人说得清
/// ⇒ 只能靠记忆。现在每个设置项在这里登记**一行**：哪一项、叫什么、
/// 归哪一页、原来住在哪儿。
pub struct SettingItem {
    /// 稳定 id（测试 / 文档用）。
    pub id: &'static str,
    /// 界面上的名字（**必须与页面上显示的文字一致**——冒烟按它核对）。
    pub label: &'static str,
    /// 归属页（**有且只有一个**）。
    pub page: Page,
    /// 原来住在哪儿（迁移前的落点）——写给文档看，也提醒"这儿曾经有第二处"。
    pub from: &'static str,
}

impl SettingItem {
    /// 界面上显示的名字（**按当前语言翻译**）。
    ///
    /// 注册表里的 `label` 是**中文原文**（const 上下文里不能调函数）⇒
    /// 显示时统一走这里翻译，冒烟按它找控件才不会在英文下找不到。
    pub fn label_text(&self) -> &'static str {
        tr(self.label)
    }
}

/// **全部设置项的注册表**（唯一来源）。
///
/// ⚠️ 增删设置项时**这里也要改**——单测会查"每个 id 只出现一次"与
/// "每页都有项"，冒烟会查"每项的标题只出现在它归属的那一页里"。
pub const SETTINGS: &[SettingItem] = &[
    SettingItem {
        id: "theme",
        label: "界面主题",
        page: Page::Appearance,
        from: "外观窗口",
    },
    SettingItem {
        id: "density",
        label: "界面密度",
        page: Page::Appearance,
        from: "外观窗口",
    },
    SettingItem {
        id: "ui_font",
        label: "界面字号",
        page: Page::Appearance,
        from: "外观窗口",
    },
    SettingItem {
        id: "palette",
        label: "终端配色",
        page: Page::Terminal,
        from: "外观窗口",
    },
    SettingItem {
        id: "term_font",
        label: "终端字体",
        page: Page::Terminal,
        from: "会话选项",
    },
    SettingItem {
        id: "term_backend",
        label: "终端引擎",
        page: Page::Terminal,
        from: "（新）首选项中心「终端」页",
    },
    SettingItem {
        id: "term_ssh",
        label: "终端连接方式",
        page: Page::Terminal,
        from: "（新）首选项中心「终端」页",
    },
    SettingItem {
        id: "xfer_resume",
        label: "断点续传",
        page: Page::Transfer,
        from: "传输队列窗口顶部",
    },
    SettingItem {
        id: "xfer_checksum",
        label: "完成后校验 SHA-1",
        page: Page::Transfer,
        from: "传输队列窗口顶部",
    },
    SettingItem {
        id: "xfer_parallel",
        label: "并发传输数",
        page: Page::Transfer,
        from: "传输队列窗口顶部",
    },
    SettingItem {
        id: "vault",
        label: "凭据库",
        page: Page::Security,
        from: "顶栏标志 / 凭据库管理器",
    },
    SettingItem {
        id: "keys",
        label: "私钥",
        page: Page::Security,
        from: "私钥管理器",
    },
    SettingItem {
        id: "shortcuts",
        label: "快捷键一览",
        page: Page::Shortcuts,
        from: "快捷键说明窗口",
    },
    SettingItem {
        id: "sidebar_width",
        label: "侧栏宽度",
        page: Page::Advanced,
        from: "拖侧栏边缘",
    },
    SettingItem {
        id: "sftp_pane_pos",
        label: "文件面板宽度",
        page: Page::Advanced,
        from: "拖终端页中间的分割条",
    },
    SettingItem {
        id: "columns",
        label: "文件列表列显示",
        page: Page::Advanced,
        from: "文件面板的列标题右键菜单",
    },
    SettingItem {
        id: "mirror_ignore",
        label: "目录镜像忽略规则",
        page: Page::Advanced,
        from: "目录镜像窗口",
    },
    SettingItem {
        id: "recent_max",
        label: "最近连接条数",
        page: Page::Advanced,
        from: "（新）侧栏「最近」分组",
    },
    SettingItem {
        id: "record_cli_recent",
        label: "命令行启动的连接记入最近连接",
        page: Page::Advanced,
        from: "（新）命令行启动路径",
    },
];

/// 某一页上的设置项。
pub fn items_of(page: Page) -> Vec<&'static SettingItem> {
    SETTINGS.iter().filter(|i| i.page == page).collect()
}

/* ---------------- 窗口 ---------------- */

/// 打开首选项中心（**停在某一页**）。
///
/// 同类只开一个（走 [`crate::windows::open_or_focus`]）：已经开着就聚焦
/// 并**切到那一页**——用户从「视图 → 外观…」进来，要看到的是外观页。
pub fn open(page: Page) {
    let win = crate::windows::open_or_focus(ToolKind::Settings, build_window);
    if let Some(nb) = find_pages(&win) {
        nb.set_current_page(Some(page.index()));
    }
    win.present();
    register_retranslate_hook();
}

/// 注册「语言变了 ⇒ 把首选项窗口的页签重建一遍」的回调（**只注册一次**）。
///
/// ⚠️ **窗口不关**，只把六个页签的内容换掉 ⇒ 用户在「外观」页改语言，
/// 窗口当场变成新语言，还停在原来那一页。
///
/// ⚠️ 用 `once` 保证只注册一次：`open()` 每次开窗口都会调 ⇒ 不去重会
/// 攒下一堆重复回调（每改一次语言就重建 N 遍）。
fn register_retranslate_hook() {
    use std::cell::Cell;
    thread_local! {
        static REGISTERED: Cell<bool> = const { Cell::new(false) };
    }
    if REGISTERED.with(|r| r.replace(true)) {
        return;
    }
    crate::i18n::add_lang_hook(|| {
        let Some(win) = crate::windows::tool_window(ToolKind::Settings) else {
            return;
        };
        let Some(nb) = find_pages(&win) else { return };
        let cur = nb.current_page();
        // ⚠️ **先把旧页全摘掉，再按顺序挂新的**：边摘边挂的话，`remove_page`
        // 会让后面的页**往前挪**，挂上去的顺序全乱了（页签名会跟页面
        // 对不上——实测「外观」页签变成「终端」）。
        while nb.n_pages() > 0 {
            nb.remove_page(Some(0));
        }
        for p in Page::all() {
            let body = build_page(p);
            let lbl = gtk::Label::new(Some(p.title()));
            lbl.set_width_chars(crate::panel::subtabs::display_width(p.title()) as i32 + 1);
            nb.append_page(&body, Some(&lbl));
        }
        if let Some(c) = cur {
            nb.set_current_page(Some(c));
        }
    });
}

/// 当前开着的首选项窗口的**页签容器**（测试钩子）。
///
/// ⚠️ 弹层/子控件不进 children 链这类坑之前吃过亏（见 AGENTS.md），
/// 这里用**自己的 CSS 类**认，不靠"第一个 Notebook 恰好是它"。
pub fn pages_notebook() -> Option<gtk::Notebook> {
    let win = crate::windows::tool_window(ToolKind::Settings)?;
    find_pages(&win)
}

/// 打开首选项中心（默认停在「外观」页）。
pub fn open_default() {
    open(Page::Appearance);
}

/// 首选项窗口是不是开着（冒烟 / 调试用）。
pub fn is_open() -> bool {
    crate::windows::is_open(ToolKind::Settings)
}

/// 建窗口（六个页签）。
fn build_window() -> gtk::Window {
    let win = gtk::Window::new();
    win.set_title(Some(ToolKind::Settings.title()));
    // 尺寸：记住上次开的（阶段 67 的规矩：工具窗记尺寸）
    let (w, h) = crate::settings::window_size(ToolKind::Settings.size_key()).unwrap_or((780, 640));
    win.set_default_size(w, h);

    let nb = gtk::Notebook::new();
    nb.set_vexpand(true);
    nb.set_hexpand(true);
    nb.add_css_class("pp-settings-pages");
    for p in Page::all() {
        let body = build_page(p);
        let lbl = gtk::Label::new(Some(p.title()));
        // ⚠️ **页签里的汉字会被压成「…」**：一个汉字占两列，宽度要按
        // 显示列算（见 `panel::subtabs::display_width`），而且要留一点余量。
        lbl.set_width_chars(crate::panel::subtabs::display_width(p.title()) as i32 + 1);
        nb.append_page(&body, Some(&lbl));
    }
    win.set_child(Some(&nb));
    win
}

/// 在窗口里找页签容器。
fn find_pages(win: &gtk::Window) -> Option<gtk::Notebook> {
    let root = win.child()?;
    find_notebook_cls(root.upcast_ref::<gtk::Widget>())
}

fn find_notebook_cls(w: &gtk::Widget) -> Option<gtk::Notebook> {
    if let Some(nb) = w.downcast_ref::<gtk::Notebook>() {
        if nb.css_classes().iter().any(|c| c == "pp-settings-pages") {
            return Some(nb.clone());
        }
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if let Some(nb) = find_notebook_cls(&c) {
            return Some(nb);
        }
        cur = c.next_sibling();
    }
    None
}

/* ---------------- 各页 ---------------- */

/// 建某一页的内容。
pub fn build_page(page: Page) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(18);
    root.set_margin_end(18);
    match page {
        Page::Appearance => appearance_page(&root),
        Page::Terminal => terminal_page(&root),
        Page::Transfer => transfer_page(&root),
        Page::Security => security_page(&root),
        Page::Shortcuts => shortcuts_page(&root),
        Page::Advanced => advanced_page(&root),
    }
    root
}

/// 外观页：界面主题 / 界面密度 / 界面字号。
fn appearance_page(root: &gtk::Box) {
    /* ---- 界面语言 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("界面语言")));

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(7);
        row.set_margin_bottom(7);
        row.set_margin_start(12);
        row.set_margin_end(12);
        let lbl = gtk::Label::new(Some(tr("界面语言")));
        lbl.set_xalign(0.0);
        lbl.set_hexpand(true);
        let combo = gtk::ComboBoxText::new();
        for l in [Lang::Zh, Lang::En] {
            combo.append(Some(l.id()), &l.self_name());
        }
        // ⚠️ **先设当前值、再接回调**：否则 `set_active_id` 自己就会触发
        //    `changed` ⇒ 一开窗口就把语言"改"一遍。
        combo.set_active_id(Some(crate::i18n::lang().id()));
        combo.connect_changed(move |c| {
            let Some(id) = c.active_id() else { return };
            let l = Lang::from_code(&id);
            if l == crate::i18n::lang() {
                return; // 没变 ⇒ 什么都不用做
            }
            crate::settings::set_language(l);
            crate::i18n::set_lang(l);
            // ⚠️ **实时生效**（用户 2026-11-09：「切换语言也应该实时生效」）
            // ⇒ 通知界面把要重建的部分重建一遍（顶栏/菜单/侧栏 + 本窗口
            // 的页签）。
            //
            // ⚠️ **必须延到 idle**：现在正在这个下拉框的 `changed` 回调里，
            // 而重建会**销毁这个下拉框本身**（它就在要重建的页签里）⇒
            // 在回调里销毁 ⇒ use-after-free ⇒ 必须等这一趟回调走完。
            gtk::glib::idle_add_local_once(|| {
                crate::i18n::notify_lang_changed();
            });
        });
        row.append(&lbl);
        row.append(&combo);
        sec.append(&row);
        sec.append(&hint_label(tr("改完立刻生效（不用重启）。")));
        root.append(&sec);
    }

    /* ---- 界面主题 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("界面主题")));

        let list = gtk::ListBox::new();
        // ⚠️ **不能用 GTK 的"选中行"当事实来源**（阶段 89）：
        // GTK4 的 ListBox 在**页签重新显示**时会把选中行自己往后挪一行，
        // 并为此发一次 row-selected ⇒ 用户只是切个页签，主题就被悄悄换掉。
        // （现场：外观选浅色 → 终端页选深色 → 回外观页，整个软件变成高对比。）
        // ⇒ 选中状态一律用**自己的勾选标记**表示；值的变化只在**用户真点了
        // 那一行**（row-activated：鼠标点击 / 回车）时才生效。
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_show_separators(true);
        // 每行带一个**勾选标记**（默认隐藏）：选中不只靠底色（审计 P2-6）
        let mut checks: Vec<gtk::Image> = Vec::new();
        for t in Theme::ALL {
            let (row, check) = option_row(t.label(), t.hint());
            list.append(&row);
            if t == theme::theme() {
                check.set_visible(true);
            }
            checks.push(check);
        }
        sec.append(&list);
        root.append(&sec);

        let cur = Rc::new(Cell::new(theme::theme()));
        let checks2 = checks.clone();
        list.connect_row_activated(move |_, row| {
            let Some(t) = Theme::ALL.get(row.index().max(0) as usize) else {
                return;
            };
            // 勾选标记跟着**当前值**走（不是跟着 GTK 的选中走）
            for (i, c) in checks2.iter().enumerate() {
                c.set_visible(Theme::ALL.get(i) == Some(t));
            }
            if cur.get() != *t {
                cur.set(*t);
                theme::set_theme(*t);
            }
        });
    }

    /* ---- 界面密度 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("界面密度")));

        let cur = density::density();
        let items = [
            (Density::Normal.label(), Density::Normal.hint()),
            (Density::Compact.label(), Density::Compact.hint()),
        ];
        let cur_idx = if cur == Density::Compact { 1 } else { 0 };
        let (seg, hint, btns) = segmented(&items, cur_idx);
        sec.append(&seg);
        sec.append(&hint);
        root.append(&sec);

        for (i, b) in btns.into_iter().enumerate() {
            let hint = hint.clone();
            b.connect_toggled(move |b| {
                if !b.is_active() {
                    return;
                }
                let d = if i == 0 {
                    Density::Normal
                } else {
                    Density::Compact
                };
                if density::density() != d {
                    density::set_density(d);
                }
                hint.set_text(d.hint());
            });
        }
    }

    /* ---- 界面字号 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("界面字号")));

        let fonts = UiFont::ALL;
        // ⚠️ FONT_HINT 是 const（编译期求值 ⇒ 不能调 tr()）⇒ 显示时翻译。
        let items: [(&str, &str); 3] = [
            (fonts[0].label(), tr(FONT_HINT)),
            (fonts[1].label(), tr(FONT_HINT)),
            (fonts[2].label(), tr(FONT_HINT)),
        ];
        let cur = density::ui_font();
        let cur_idx = fonts.iter().position(|f| *f == cur).unwrap_or(1);
        let (seg, hint, btns) = segmented(&items, cur_idx);
        sec.append(&seg);
        sec.append(&hint);
        root.append(&sec);

        for (i, b) in btns.into_iter().enumerate() {
            b.connect_toggled(move |b| {
                if !b.is_active() {
                    return;
                }
                let f = fonts[i];
                if density::ui_font() != f {
                    density::set_ui_font(f);
                }
            });
        }
    }

    root.append(&hint_label(
        tr("界面主题与终端配色各管各的：界面选浅色不代表终端也要浅色。"),
    ));
}

/// 终端页：终端配色 / 终端字号。
fn terminal_page(root: &gtk::Box) {
    /* ---- 终端引擎（2026-11-10，用户拍板） ---- */
    //
    // ⚠️ **两个后端都编进了同一个二进制** ⇒ 用户能选用 libvte 还是
    //    ghostty ⇒ 跨平台开发时不用改环境变量就能在 Linux 上跑 Windows
    //    那条渲染路径。
    //
    // ⚠️ **只在两个后端都可用时才显示**：Windows 上没有 VTE
    //    （`vte4-sys` 在 Windows 上编不过）⇒ 那一项恒 ghostty ⇒ 不给
    //    用户一个「选了也没用」的下拉框。
    if crate::settings::backend_usable(crate::settings::BACKEND_VTE)
        && crate::settings::backend_usable(crate::settings::BACKEND_GHOSTTY)
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("终端引擎")));

        let combo = gtk::ComboBoxText::new();
        combo.append(Some(crate::settings::BACKEND_VTE), "libvte");
        combo.append(Some(crate::settings::BACKEND_GHOSTTY), "ghostty");
        // ⚠️ **先设当前值、再接回调**（同上面的语言下拉：否则
        //    `set_active_id` 自己就触发 `changed` ⇒ 一开窗口就把值"改"一遍）。
        combo.set_active_id(Some(crate::settings::effective_term_backend()));

        let tip = hint_label(
            tr("libvte 是 Linux 的 GTK 终端控件，ghostty 是 Ghostty 的终端内核。两个后端都编进了这个程序，切换只对新开的终端生效。"),
        );
        let tip2 = hint_label(
            tr("ghostty 引擎还在完善中：自定义配色、鼠标拖选复制、录制回放暂不可用。"),
        );
        combo.connect_changed(move |c| {
            let Some(id) = c.active_id() else { return };
            crate::settings::set_term_backend(Some(&id));
        });
        sec.append(&combo);
        sec.append(&tip);
        sec.append(&tip2);
        root.append(&sec);
    }

    /* ---- 终端连接方式（2026-11-11，用户拍板） ---- */
    //
    // ⚠️ **默认 = 并入共享连接**：同一台服务器只开一条 SSH 连接，文件面板、
    //    端口映射、数据库、终端都走它 ⇒ 密码只问一次。
    //    ⚠️ **代价**：并入之后终端**不走用户自己的 ssh** ⇒
    //    `~/.ssh/config`（ProxyJump 等）不生效 ⇒ 需要的话切回 spawn。
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("终端连接方式")));

        let combo = gtk::ComboBoxText::new();
        combo.append(Some(crate::settings::TERM_SSH_SHARED), tr("并入共享连接"));
        combo.append(Some(crate::settings::TERM_SSH_SPAWN), tr("spawn 自己的 ssh"));
        // ⚠️ **先设当前值、再接回调**（同上面的终端引擎下拉：否则
        //    `set_active_id` 自己就触发 `changed` ⇒ 一开窗口就把值"改"一遍）。
        let cur = crate::settings::term_ssh()
            .unwrap_or_else(|| crate::settings::TERM_SSH_SHARED.to_string());
        combo.set_active_id(Some(cur.as_str()));

        let tip = hint_label(tr(
            "并入共享连接：同一台服务器只开一条 SSH 连接，文件面板、端口映射、数据库、终端共用它（密码只问一次）。",
        ));
        let tip2 = hint_label(tr(
            "spawn 自己的 ssh：沿用 ~/.ssh/config、ssh-agent、ProxyJump（但每台服务器会多一条连接）。",
        ));
        combo.connect_changed(move |c| {
            let Some(id) = c.active_id() else { return };
            crate::settings::set_term_ssh(Some(&id));
        });
        sec.append(&combo);
        sec.append(&tip);
        sec.append(&tip2);
        // ⚠️ **当前后端不支持 ⇒ 明确告诉用户「已经退回 spawn 了」**
        //    （否则他选了「并入共享连接」却发现还是多开连接 ⇒ 以为设置没生效）
        if !crate::term::backend_supports_inproc() {
            sec.append(&hint_label(tr(
                "当前终端引擎还不支持进程内 SSH，这个选择暂时退回 spawn。",
            )));
        }
        root.append(&sec);
    }

    /* ---- 终端配色 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("终端配色")));

        let list = gtk::ListBox::new();
        // ⚠️ 同上：不用 GTK 的"选中行"当事实来源（阶段 89 的同一个坑）
        list.set_selection_mode(gtk::SelectionMode::None);
        list.set_show_separators(true);
        // 配色行也带**勾选标记**：既然不用 GTK 的高亮，当前是哪套配色
        // 就得自己标出来（否则切页签后看不出当前值）。
        let mut checks: Vec<gtk::Image> = Vec::new();
        for p in Palette::ALL {
            let (row, check) = palette_row(p);
            list.append(&row);
            if p == theme::palette() {
                check.set_visible(true);
            }
            checks.push(check);
        }
        sec.append(&list);
        root.append(&sec);

        let cur = Rc::new(Cell::new(theme::palette()));
        let checks2 = checks.clone();
        list.connect_row_activated(move |_, row| {
            let Some(p) = Palette::ALL.get(row.index().max(0) as usize) else {
                return;
            };
            for (i, c) in checks2.iter().enumerate() {
                c.set_visible(Palette::ALL.get(i) == Some(p));
            }
            if cur.get() != *p {
                cur.set(*p);
                theme::set_palette(*p);
            }
        });
    }

    /* ---- 终端字体 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("终端字体")));

        // ---- 字体名：本机装了的**等宽字体**（终端用等宽，比例字体没法对齐）----
        let families = monospace_families();
        let mut names: Vec<String> = vec!["Monospace".to_string()];
        for f in &families {
            if f != "Monospace" && !names.contains(f) {
                names.push(f.clone());
            }
        }
        let cur_family = crate::settings::term_font_family().unwrap_or_else(|| "Monospace".into());
        let cur_idx = names
            .iter()
            .position(|n| n == &cur_family)
            .unwrap_or(0) as u32;

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(7);
        row.set_margin_bottom(7);
        row.set_margin_start(12);
        row.set_margin_end(12);
        let lbl = gtk::Label::new(Some(tr("字体")));
        lbl.set_xalign(0.0);
        row.append(&lbl);
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dd = gtk::DropDown::from_strings(&refs);
        dd.set_selected(cur_idx);
        dd.set_hexpand(true);
        dd.set_tooltip_text(Some(tr("终端用哪种字体（只列本机的等宽字体）")));
        row.append(&dd);
        sec.append(&row);

        // ---- 字号 ----
        let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row2.set_margin_top(7);
        row2.set_margin_bottom(7);
        row2.set_margin_start(12);
        row2.set_margin_end(12);
        let lbl2 = gtk::Label::new(Some(tr("字号")));
        lbl2.set_xalign(0.0);
        let spin = gtk::SpinButton::with_range(9.0, 40.0, 0.5);
        spin.set_value(crate::settings::term_font_size().unwrap_or(10.5));
        spin.set_tooltip_text(Some(tr("终端字号（点）")));
        let tip = gtk::Label::new(Some(tr("改完立刻生效")));
        tip.add_css_class("dim-label");
        tip.add_css_class("pp-hint");
        tip.set_xalign(0.0);
        row2.append(&lbl2);
        row2.append(&spin);
        row2.append(&tip);
        sec.append(&row2);
        root.append(&sec);

        // ⚠️ **实时生效**（用户 2026-11-09：「字体设置和字号修改后应该实时
        // 生效，这样用户可以马上在终端上看出改变的效果」）⇒ 改完立刻刷
        // 所有已开的终端，不用重开。
        //
        // ⚠️ **先设当前值、再接回调**（跟上面语言那个下拉一个道理）：否则
        // 一开窗口就把自己"改"了一遍。
        let names2 = names.clone();
        dd.connect_selected_notify(move |d| {
            let i = d.selected();
            let Some(name) = names2.get(i as usize) else {
                return;
            };
            crate::settings::set_term_font_family(Some(name));
            crate::tabs::sync_term_fonts();
        });
        spin.connect_value_changed(move |s| {
            let v = s.value();
            crate::settings::set_term_font_size(Some(v));
            crate::tabs::sync_term_fonts();
        });
    }

    root.append(&hint_label(
        tr("终端的「终端类型（TERM）/ 响铃 / 保活」是每条会话自己的设置，\
         在连接编辑器的「会话」页里改；这里管的是所有终端共用的默认。"),
    ));
}

/// 本机装了的**等宽字体**（终端只能用等宽字体）。
///
/// ⚠️ 拿不到（没有显示服务 / pango 还没初始化）⇒ 空列表 ⇒ 设置页里
/// 只留默认的 `Monospace`。
fn monospace_families() -> Vec<String> {
    if !gtk::is_initialized_main_thread() {
        return Vec::new();
    }
    let probe = gtk::Label::new(None);
    let ctx = probe.create_pango_context();
    let mut out: Vec<String> = ctx
        .list_families()
        .iter()
        .filter(|f| f.is_monospace())
        .map(|f| f.name().to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 传输页：断点续传 / 校验 / 并发数（原来在队列窗口顶部）。
fn transfer_page(root: &gtk::Box) {
    let b_resume = gtk::CheckButton::with_label(tr("断点续传"));
    b_resume.set_active(crate::settings::xfer_resume());
    b_resume.set_tooltip_text(Some(tr("中断后接着传，不从头再来")));
    b_resume.connect_toggled(|c| {
        crate::settings::set_xfer_resume(c.is_active());
    });
    root.append(&b_resume);

    let b_sum = gtk::CheckButton::with_label(tr("完成后校验 SHA-1"));
    b_sum.set_active(crate::settings::xfer_checksum());
    b_sum.set_tooltip_text(Some(tr("传完比一遍远程文件的摘要；没校验过就显示「未校验」")));
    b_sum.connect_toggled(|c| {
        crate::settings::set_xfer_checksum(c.is_active());
    });
    root.append(&b_sum);

    let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bbox.append(&gtk::Label::new(Some(tr("并发传输数"))));
    let parallel = gtk::SpinButton::with_range(1.0, 16.0, 1.0);
    parallel.set_value(crate::settings::xfer_parallel() as f64);
    parallel.set_tooltip_text(Some(tr("同一条连接上同时传几个文件")));
    bbox.append(&parallel);
    root.append(&bbox);
    parallel.connect_value_changed(|s| {
        crate::settings::set_xfer_parallel(s.value() as usize);
    });

    root.append(&hint_label(
        tr("这些是全局设置，对之后新建的传输任务生效。\
         传输队列窗口顶部只显示摘要，点「去设置」回到这里。"),
    ));
}

/// 安全页：凭据库 / 私钥。
fn security_page(root: &gtk::Box) {
    /* ---- 凭据库 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("凭据库")));

        let st = crate::vault::state();
        let line = gtk::Label::new(Some(&trf!("状态：{v0}", v0 = crate::vaultui::state_short(st))));
        line.set_xalign(0.0);
        line.add_css_class("pp-hint");
        sec.append(&line);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let b_open = gtk::Button::with_label(tr("打开凭据库…"));
        b_open.connect_clicked(|_| {
            crate::vaultui::open_manager_vault(None, crate::connmgr::load_all());
        });
        row.append(&b_open);
        let b_lock = gtk::Button::with_label(tr("立即锁定"));
        b_lock.set_sensitive(crate::vault::is_unlocked());
        let b_lock2 = b_lock.clone();
        b_lock.connect_clicked(move |_| {
            crate::vault::lock();
            // 状态就地改写（不用弹窗——改个状态不值得打断用户）
            let txt = trf!("状态：{v0}", v0 = crate::vaultui::state_short(crate::vault::state()));
            line.set_text(&txt);
            b_lock2.set_sensitive(false);
        });
        row.append(&b_lock);
        sec.append(&row);
        root.append(&sec);
    }

    /* ---- 私钥 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("私钥")));

        let n = crate::keymgr::ssh_dir()
            .map(|d| crate::keymgr::list_dir(&d).len())
            .unwrap_or(0);
        let line = gtk::Label::new(Some(&trf!("~/.ssh 里找到 {n} 个私钥文件", n = n)));
        line.set_xalign(0.0);
        line.add_css_class("pp-hint");
        sec.append(&line);

        let b = gtk::Button::with_label(tr("打开私钥管理器…"));
        b.connect_clicked(|_| crate::keyui::open(None));
        sec.append(&b);
        root.append(&sec);
    }

    root.append(&hint_label(
        tr("连接用哪把私钥、认证方式选哪种，在连接编辑器的「认证」页里设；\
         凭据库管的是「记住的密码」要不要加密存着。"),
    ));
}

/// 快捷键页：一览。
fn shortcuts_page(root: &gtk::Box) {
    let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
    // ⚠️ 标题写「快捷键一览」——注册表里登记的就是这个名字（冒烟按它核对）
    sec.append(&section(tr("快捷键一览")));

    let groups = crate::shortcuts::grouped();
    for (title, items) in groups {
        let g = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let t = gtk::Label::new(Some(title));
        t.set_xalign(0.0);
        t.add_css_class("pp-section");
        g.append(&t);
        for b in items {
            // ⚠️ 快捷键说明在 const 表里 ⇒ 用 tr_dyn 翻（trf! 的占位符
            //    走的是 to_string()，传 Cow 进去也对）。
            let line = gtk::Label::new(Some(&trf!("{v0}　{v1}", v0 = b.display, v1 = tr_dyn(b.desc))));
            line.set_xalign(0.0);
            line.add_css_class("pp-hint");
            g.append(&line);
        }
        sec.append(&g);
    }
    root.append(&sec);

    let b = gtk::Button::with_label(tr("打开快捷键说明窗口…"));
    b.connect_clicked(|_| crate::shortcutsui::open());
    root.append(&b);
}

/// 高级页：布局尺寸 / 列显示 / 镜像规则。
fn advanced_page(root: &gtk::Box) {
    /* ---- 侧栏宽度 ---- */
    {
        let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bbox.append(&gtk::Label::new(Some(tr("侧栏宽度"))));
        let spin = gtk::SpinButton::with_range(160.0, 520.0, 10.0);
        spin.set_value(f64::from(crate::settings::sidebar_width()));
        spin.set_tooltip_text(Some(tr("会话列表那一列的宽度")));
        bbox.append(&spin);
        root.append(&bbox);
        spin.connect_value_changed(|s| {
            crate::settings::set_sidebar_width(s.value() as i32);
        });
    }

    /* ---- 文件面板宽度 ---- */
    {
        let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bbox.append(&gtk::Label::new(Some(tr("文件面板宽度"))));
        let spin = gtk::SpinButton::with_range(180.0, 900.0, 10.0);
        let cur = crate::settings::sftp_pane_pos();
        spin.set_value(if cur > 120 { f64::from(cur) } else { 340.0 });
        spin.set_tooltip_text(Some(tr("终端页左边文件面板的宽度")));
        bbox.append(&spin);
        root.append(&bbox);
        spin.connect_value_changed(|s| {
            crate::settings::set_sftp_pane_pos(s.value() as i32);
        });
    }

    /* ---- 文件列表列显示 ---- */
    {
        let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bbox.append(&gtk::Label::new(Some(tr("文件列表列显示"))));
        let b = gtk::Button::with_label(tr("选择要显示的列…"));
        b.connect_clicked(|_| columns_dialog());
        bbox.append(&b);
        root.append(&bbox);
    }

    /* ---- 目录镜像忽略规则 ---- */
    {
        let bbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bbox.append(&gtk::Label::new(Some(tr("目录镜像忽略规则"))));
        let e = gtk::Entry::new();
        e.set_text(&crate::settings::mirror_ignore());
        e.set_placeholder_text(Some(tr("留空 = 什么都不忽略")));
        e.set_tooltip_text(Some(tr("一行一条，目录镜像时跳过这些名字/后缀")));
        e.set_hexpand(true);
        bbox.append(&e);
        root.append(&bbox);
        e.connect_changed(|e| {
            crate::settings::set_mirror_ignore(e.text().as_str());
        });
    }

    /* ---- 最近连接 ---- */
    {
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sec.append(&section(tr("最近连接")));

        // ---- 条数上限 ----
        let cur_n = crate::settings::recent_max();
        let mut list: Vec<usize> = vec![3, 5, 10, 20];
        if !list.contains(&cur_n) {
            list.push(cur_n);
        }
        list.push(0); // 「不限」放最后
        let combo = gtk::ComboBoxText::new();
        for n in &list {
            let id = n.to_string();
            let text = if *n == 0 {
                tr("不限").to_string()
            } else {
                trf!("{n} 条", n = n)
            };
            combo.append(Some(&id), &text);
        }
        let cur_id = cur_n.to_string();
        combo.set_active_id(Some(cur_id.as_str()));
        combo.set_tooltip_text(Some(tr("「最近连接」最多留几条（0 = 不限）")));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let lbl = gtk::Label::new(Some(tr("最近连接条数")));
        lbl.set_xalign(0.0);
        lbl.set_hexpand(true);
        row.append(&lbl);
        row.append(&combo);
        sec.append(&row);
        combo.connect_changed(move |c| {
            let Some(id) = c.active_id() else { return };
            let Ok(n) = id.parse::<usize>() else { return };
            crate::settings::set_recent_max(n);
        });

        // ---- 命令行启动的连接要不要记 ----
        let sw = gtk::Switch::new();
        sw.set_active(crate::settings::record_cli_recent());
        sw.set_tooltip_text(Some(
            tr("堡垒机每次拉起都是一条一次性的设备连接 ⇒ 默认不记，\
             省得把「最近连接」冲垮、自己常用的那条反而找不到。"),
        ));
        let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let lbl2 = gtk::Label::new(Some(tr("命令行启动的连接记入最近连接")));
        lbl2.set_xalign(0.0);
        lbl2.set_hexpand(true);
        row2.append(&lbl2);
        row2.append(&sw);
        sec.append(&row2);
        sw.connect_state_set(|_s, state| {
            crate::settings::set_record_cli_recent(state);
            gtk::glib::Propagation::Proceed
        });

        // ---- 清空 ----
        let row3 = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let lbl3 = gtk::Label::new(Some(tr("清空最近连接")));
        lbl3.set_xalign(0.0);
        lbl3.set_hexpand(true);
        let b = gtk::Button::with_label(tr("清空"));
        b.set_tooltip_text(Some(tr("把「最近连接」里的记录全删掉（连接配置不受影响）")));
        row3.append(&lbl3);
        row3.append(&b);
        sec.append(&row3);
        b.connect_clicked(|_| {
            crate::panel::dialogs::confirm_with(
                None,
                tr("清空最近连接？\n\n\
                 「最近连接」里的记录会被全部删掉（已保存的连接配置不受影响）。"),
                tr("清空"),
                |ok| {
                    if ok {
                        crate::recent::clear();
                    }
                },
            );
        });
        root.append(&sec);
    }

    root.append(&hint_label(
        tr("这一页放的是「零散但确实得调」的设置：宽度、列、镜像规则。\
         改完多半要新开窗口才看得出效果。"),
    ));
}

/// 文件列表的列显示对话框（远程 / 本地各一份）。
fn columns_dialog() {
    let dlg = crate::windows::dialog(
        tr("文件列表列显示"),
        None,
        &[(tr("关闭"), gtk::ResponseType::Close)],
    );
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(14);
    root.set_margin_end(14);

    for remote in [true, false] {
        let title = if remote {
            tr("远程文件")
        } else {
            tr("本地文件")
        };
        let sec = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let t = gtk::Label::new(Some(title));
        t.set_xalign(0.0);
        t.add_css_class("pp-section");
        sec.append(&t);
        let cfg = crate::settings::columns(remote);
        for (key, label, on) in [
            ("size", tr("大小"), cfg.size),
            ("perm", tr("权限"), cfg.perm),
            ("mtime", tr("修改时间"), cfg.mtime),
        ] {
            let cb = gtk::CheckButton::with_label(label);
            cb.set_active(on);
            cb.connect_toggled(move |c| {
                crate::settings::set_column(remote, key, c.is_active());
            });
            sec.append(&cb);
        }
        root.append(&sec);
    }

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
}

/* ---------------- 构件 ---------------- */

const FONT_HINT: &str = "只影响界面控件；终端字号在「终端」页里调";

fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("pp-section");
    l
}

/// 页面底部的一句说明。
fn hint_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.add_css_class("dim-label");
    l.add_css_class("pp-hint");
    l
}

/// 单行选项：左边标题，右边灰字说明（说明过长时省略号收尾）。
fn option_row(title: &str, hint: &str) -> (gtk::ListBoxRow, gtk::Image) {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    h.set_margin_top(7);
    h.set_margin_bottom(7);
    h.set_margin_start(12);
    h.set_margin_end(12);

    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.set_hexpand(true);
    h.append(&t);

    if !hint.is_empty() {
        let d = gtk::Label::new(Some(hint));
        d.add_css_class("dim-label");
        d.add_css_class("pp-hint");
        d.set_xalign(1.0);
        d.set_ellipsize(gtk::pango::EllipsizeMode::End);
        d.set_max_width_chars(32);
        h.append(&d);
    }

    // 勾选标记：选中**不只靠底色**（审计 P2-6）——
    // 色弱用户、灰度截图、高对比主题里，光看底色认不出选的是哪个。
    // 图标用 `object-select-symbolic`（项目里已在用，Adwaita 确实有这个图标）。
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_pixel_size(16);
    check.set_visible(false);
    h.append(&check);
    row.set_child(Some(&h));
    (row, check)
}

/// 互斥的分段控件（`linked` 按钮组）+ 下方一行说明。
///
/// 返回（容器, 说明标签, 按钮列表）；说明文字由调用方在选中变化时更新。
fn segmented(
    items: &[(&str, &str)],
    current: usize,
) -> (gtk::Box, gtk::Label, Vec<gtk::ToggleButton>) {
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hb.add_css_class("linked");

    let mut btns = Vec::new();
    let mut first: Option<gtk::ToggleButton> = None;
    for (i, (label, _)) in items.iter().enumerate() {
        let b = gtk::ToggleButton::with_label(label);
        b.set_hexpand(true);
        match &first {
            Some(f) => b.set_group(Some(f)),
            None => first = Some(b.clone()),
        }
        if i == current {
            b.set_active(true);
        }
        hb.append(&b);
        btns.push(b);
    }

    let hint = gtk::Label::new(Some(items.get(current).map(|(_, h)| *h).unwrap_or("")));
    hint.add_css_class("dim-label");
    hint.add_css_class("pp-hint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);

    (hb, hint, btns)
}

/// 配色行：名称 + 16 色色板条（第一格是前景/背景示例）。
fn palette_row(p: Palette) -> (gtk::ListBoxRow, gtk::Image) {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    h.set_margin_top(6);
    h.set_margin_bottom(6);
    h.set_margin_start(12);
    h.set_margin_end(12);

    let name = gtk::Label::new(Some(p.label()));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    h.append(&name);
    h.append(&ansi_strip(p));

    // 勾选标记（与主题列表一致）：当前是哪套配色一眼看得见，
    // 不靠 GTK 的高亮（高亮在阶段 89 之后不再用了）。
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_pixel_size(16);
    check.set_visible(false);
    h.append(&check);

    row.set_child(Some(&h));
    (row, check)
}

/// 色板条：底色块 + `Aa` 前景示例 + 16 个 ANSI 小方块。
fn ansi_strip(p: Palette) -> gtk::DrawingArea {
    const W: i32 = 148;
    const H: i32 = 20;

    let (fg, bg, ansi) = p.colors();
    let (f, b) = (
        gtk::gdk::RGBA::parse(fg).ok(),
        gtk::gdk::RGBA::parse(bg).ok(),
    );
    let ansi: Vec<Option<gtk::gdk::RGBA>> = ansi
        .iter()
        .map(|c| gtk::gdk::RGBA::parse(*c).ok())
        .collect();

    let d = gtk::DrawingArea::new();
    d.set_content_width(W);
    d.set_content_height(H);
    d.set_draw_func(move |_a, cr, w, h| {
        let set = |c: Option<gtk::gdk::RGBA>, fallback: (f64, f64, f64)| match c {
            Some(c) => cr.set_source_rgba(
                f64::from(c.red()),
                f64::from(c.green()),
                f64::from(c.blue()),
                f64::from(c.alpha()),
            ),
            None => cr.set_source_rgb(fallback.0, fallback.1, fallback.2),
        };

        // 背景（圆角矩形）+ 一层中性描边：浅色配色在白底行上才有个边界
        let (ww, hh) = (f64::from(w), f64::from(h));
        set(b, (0.2, 0.2, 0.2));
        rounded_rect(cr, 0.5, 0.5, ww - 1.0, hh - 1.0, 4.0);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.45);
        let _ = cr.set_line_width(1.0);
        let _ = cr.stroke();

        // 前景示例
        set(f, (0.9, 0.9, 0.9));
        let _ = cr.set_font_size(11.0);
        let _ = cr.move_to(6.0, hh - 6.0);
        let _ = cr.show_text("Aa");

        // 16 色：8 + 8 两排，每格 8×4
        let (cell_w, cell_h) = (9.0, 5.0);
        let x0 = 32.0;
        for (i, c) in ansi.iter().enumerate() {
            let col = i % 8;
            let row = i / 8;
            let x = x0 + col as f64 * (cell_w + 1.0);
            let y = 4.0 + row as f64 * (cell_h + 2.0);
            set(*c, (0.5, 0.5, 0.5));
            let _ = cr.rectangle(x, y, cell_w, cell_h);
            let _ = cr.fill();
        }
    });
    d
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// 页里出现的所有文字（冒烟用来核对"设置项只出现在它归属的那一页"）。
pub fn page_labels(page: Page) -> Vec<String> {
    let body = build_page(page);
    let mut out = Vec::new();
    collect_labels(body.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 递归收集控件树里的 Label 文字。
fn collect_labels(w: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push(l.text().to_string());
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        collect_labels(&c, out);
        cur = c.next_sibling();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **每个设置项有且只有一个归属页**（审计 P1-4 的验收点）。
    ///
    /// ⚠️ 这一层只查**注册表**本身（纯逻辑，不碰 GTK）——"页面上是不是
    /// 真有这一项"要建控件才能查，放在冒烟里（`examples/settingsui_smoke.rs`）。
    #[test]
    fn 每个设置项有且只有一个归属页() {
        // id 不许重名（重名 = 同一个设置项登记了两次）
        let mut ids: Vec<&str> = SETTINGS.iter().map(|i| i.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SETTINGS.len(), "注册表里有重名的 id");

        // 每页都得有项（空页 = 用户点进去啥也没有）
        for p in Page::all() {
            assert!(!items_of(p).is_empty(), "「{}」页一项都没有", p.title());
        }

        // 名字不许为空（空名字在界面上就是一行没字）
        for it in SETTINGS {
            assert!(!it.label.is_empty(), "{} 没有名字", it.id);
            assert!(!it.from.is_empty(), "{} 没写原来住在哪儿", it.id);
        }

        // 页的名字不许重复（页签上两个同名 ⇒ 用户分不清）
        let mut titles: Vec<&str> = Page::all().iter().map(|p| p.title()).collect();
        titles.sort();
        titles.dedup();
        assert_eq!(titles.len(), Page::all().len(), "有两个页同名");
    }

    /// 页序号与页要对得上（Notebook 靠序号切页）。
    #[test]
    fn 页序号连续且能反查() {
        for (i, p) in Page::all().into_iter().enumerate() {
            assert_eq!(p.index() as usize, i, "页序号不连续");
            assert_eq!(Page::from_index(p.index()), p, "反查不回来");
        }
    }
}
