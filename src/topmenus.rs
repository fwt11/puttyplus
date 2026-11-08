//! 顶栏菜单栏的**内容表**（菜单标题 + 菜单项）。
//!
//! **为什么单独抽一个模块**：这张表原先放在 `main.rs` 里，于是
//! `examples/ui_shot.rs` 的 menubar 模式只能**手抄一份标题列表**来量宽度
//! ——抄了一份就会漂移（阶段 41 菜单从 7 个减到 5 个时，那份抄本就过时了，
//! 量出来的数还是 7 项的数，量了个寂寞）。
//!
//! 现在表在 lib 里，`ui_shot` 直接问 `top_menu_titles()` 要真实标题，
//! 量到的永远是**当前**菜单栏的宽度；`tests` 里也钉死了"顶级菜单是 5 个"。
//!
//! 菜单项的**观感**（图标 / hover 高亮 / 弹层）统一由 [`crate::menu`] 提供，
//! 顶栏只是用它搭出来的标题行 + 弹层。
use crate::i18n::tr;

/// 顶栏菜单栏里的一项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    /// 普通项：图标 + 文字 + action 名（`danger` = 不可逆动作，标红）
    Item {
        icon: &'static str,
        label: &'static str,
        action: &'static str,
        danger: bool,
        /// 快捷键提示（显示在文字右边，如 "Ctrl+Shift+T"）。
        ///
        /// ⚠️ **必须**能在 [`crate::shortcuts`] 的表里查到（单测钉死）——
        /// 菜单上写着一个表里没有的快捷键，用户按了没反应。
        shortcut: Option<&'static str>,
        /// 可用条件：返回 false 时菜单项**灰掉**、动作也禁用
        /// （阶段 74 / 审计 P1-13：欢迎页上「复制/粘贴/查找/分屏」全是亮着的，
        /// 点了没反应）。
        enabled: Option<fn() -> bool>,
    },
    /// **开关项**：右侧勾选标记显示当前状态（勾 = 开）。
    ///
    /// 文字是**设置名**（不是动作）——开还是关由勾选标记说，用户不用去猜。
    /// `state` = 取当前状态的函数（菜单项只建一次，每次弹出前问一次）。
    Toggle {
        icon: &'static str,
        label: &'static str,
        action: &'static str,
        state: fn() -> bool,
        /// 快捷键提示（同 [`MenuEntry::Item`]）。
        shortcut: Option<&'static str>,
        /// 可用条件（同 [`MenuEntry::Item`]）。
        enabled: Option<fn() -> bool>,
    },
    /// 分组线
    Sep,
    /// 「凭据库…」：标题要跟着凭据库状态变
    Vault,
}

/// 顶栏菜单栏的内容（5 个顶级菜单：文件 / 连接 / 终端 / 视图 / 工具）。
///
/// **图标与终端右键菜单用同一套**：用户 2026-10-28 要求"所有菜单风格都要
/// 像终端右键菜单看齐"，菜单项的观感由 `crate::menu` 统一提供。
///
/// **为什么只剩 5 个（阶段 41）**：用户嫌顶栏"宽度有些太大"。阶段 40 只把
/// 字号/padding 收了一轮（301→245px，-19%），用户反馈"看不出变化"——19%
/// 的差别肉眼看不出来。**要让人一眼看出差别，只能减顶级菜单的数量**：
/// 「编辑」（复制/粘贴/终端查找）并进「终端」，「帮助」（快捷键一览/关于）
/// 并进「工具」，7 个顶级菜单减为 5 个。菜单项本身一个没少（action 与
/// 快捷键都还在），只是标题行变短了。
pub const TOP_MENUS: &[(&str, &[MenuEntry])] = &[
    (
        "文件",
        &[
            MenuEntry::Item {
                icon: "list-add-symbolic",
                label: "新建连接…",
                action: "new",
                danger: false,
                shortcut: Some("Ctrl+Shift+T"),
                enabled: None,
            },
            MenuEntry::Item {
                icon: "window-close-symbolic",
                label: "关闭标签",
                action: "close",
                danger: true,
                shortcut: Some("Ctrl+Shift+W"),
                // 没有标签（欢迎页）时没什么可关的
                enabled: Some(crate::tabs::has_tab),
            },
            MenuEntry::Sep,
            MenuEntry::Item {
                icon: "document-open-symbolic",
                label: "导入 PuTTY 会话…",
                action: "import",
                danger: false,
                shortcut: None,
                enabled: None,
            },
            MenuEntry::Item {
                icon: "document-save-symbolic",
                label: "导出为 PuTTY 会话…",
                action: "export",
                danger: false,
                shortcut: None,
                enabled: None,
            },
            MenuEntry::Item {
                icon: "edit-clear-all-symbolic",
                label: "清空最近连接",
                action: "clear_recent",
                danger: false,
                shortcut: None,
                // 没有最近连接时没什么可清 ⇒ 置灰（别给一个点了没反应的项）
                enabled: Some(crate::recent::has_any),
            },
            MenuEntry::Sep,
            MenuEntry::Item {
                icon: "application-exit-symbolic",
                label: "退出",
                action: "quit",
                danger: true,
                shortcut: None,
                enabled: None,
            },
        ],
    ),
    // 连接：都作用于"当前这条 SSH 连接"（端口转发、数据库通道、会话选项等）。
    (
        "连接",
        &[
            MenuEntry::Item {
                icon: "view-refresh-symbolic",
                label: "重新连接 SFTP",
                action: "reconnect",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_session),
            },
            MenuEntry::Item {
                icon: "utilities-terminal-symbolic",
                label: "重新连接终端",
                action: "reconnect_term",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_session),
            },
            MenuEntry::Sep,
            MenuEntry::Item {
                icon: "network-transmit-receive-symbolic",
                label: "端口转发…",
                action: "fwd",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_session),
            },
            MenuEntry::Item {
                icon: "drive-harddisk-symbolic",
                label: "数据库…",
                action: "db",
                danger: false,
                shortcut: Some("Ctrl+Shift+D"),
                enabled: Some(crate::tabs::has_session),
            },
            MenuEntry::Item {
                // ⚠️ 传输队列以前**只有 SFTP 文件面板工具栏上的一个图标**
                // 能进（`pane.rs`）⇒ 错过一次桌面通知、想回看传过什么，
                // 就找不到门了（审计 P1-9 / P2-18）。现在它也在主菜单里。
                icon: "folder-download-symbolic",
                label: "传输队列…",
                action: "queue",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_session),
            },
            MenuEntry::Sep,
            MenuEntry::Item {
                icon: "preferences-system-symbolic",
                label: "会话选项…",
                action: "sessopt",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_session),
            },
        ],
    ),
    // 终端：作用于当前终端/分屏，不改连接本身。
    //
    // 复制/粘贴/查找原属独立的「编辑」顶级菜单（阶段 41 并入这里）：
    // 顶级菜单从 7 个减到 5 个，顶栏宽度才真正看得见地变窄。
    // 复制/粘贴在终端右键菜单里本来就有，查找有 Ctrl+F 快捷键，
    // 所以并进来不会让功能"消失"，只是少一个顶级标题。
    (
        "终端",
        &[
            MenuEntry::Item {
                icon: "edit-copy-symbolic",
                label: "复制",
                action: "copy",
                danger: false,
                shortcut: Some("Ctrl+Shift+C"),
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "edit-paste-symbolic",
                label: "粘贴",
                action: "paste",
                danger: false,
                shortcut: Some("Ctrl+Shift+V"),
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "edit-find-symbolic",
                label: "终端查找…",
                action: "find",
                danger: false,
                shortcut: Some("Ctrl+F"),
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Sep,
            MenuEntry::Item {
                icon: "view-dual-symbolic",
                label: "左右分屏",
                action: "split_lr",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "view-continuous-symbolic",
                label: "上下分屏",
                action: "split_ud",
                danger: false,
                shortcut: None,
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "window-close-symbolic",
                label: "关闭当前终端",
                action: "close_pane",
                danger: true,
                shortcut: None,
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Sep,
            // ⚠️ 阶段 74：广播 / 日志 / 录制以前是普通项（只在文字里写「开关」），
            // 和「显示侧边栏」（带勾）**长得不一样** ⇒ 同一类状态三种表现。
            // 现在统一成 **Toggle**：右侧勾选标记直接说开还是关。
            MenuEntry::Toggle {
                icon: "network-transmit-receive-symbolic",
                label: "按键广播",
                action: "broadcast",
                state: crate::tabs::broadcast_on,
                shortcut: Some("Alt+Shift+B"),
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "utilities-terminal-symbolic",
                label: "快速命令…",
                action: "quickcmds",
                danger: false,
                shortcut: Some("Ctrl+Shift+K"),
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Sep,
            MenuEntry::Toggle {
                icon: "document-save-symbolic",
                label: "记录终端日志",
                action: "log",
                state: crate::tabs::logging_on,
                shortcut: None,
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Toggle {
                icon: "media-record-symbolic",
                label: "录制会话",
                action: "record",
                state: crate::tabs::recording_on,
                shortcut: None,
                enabled: Some(crate::tabs::has_terminal),
            },
            MenuEntry::Item {
                icon: "media-playback-start-symbolic",
                label: "打开录制回放…",
                action: "replay",
                danger: false,
                shortcut: Some("Ctrl+Shift+O"),
                enabled: None,
            },
        ],
    ),
    (
        "视图",
        &[
            // 开关项（阶段 59）：勾 = 侧边栏显示中。
            // 原来写「显示/隐藏侧边栏」，看不出当前是开是关。
            MenuEntry::Toggle {
                icon: "view-list-symbolic",
                label: "显示侧边栏",
                action: "sidebar",
                state: crate::settings::show_sidebar,
                shortcut: Some("Ctrl+Shift+B"),
                enabled: None,
            },
            MenuEntry::Item {
                icon: "preferences-desktop-appearance-symbolic",
                label: "外观…",
                action: "theme",
                danger: false,
                shortcut: None,
                enabled: None,
            },
        ],
    ),
    (
        "工具",
        &[
            MenuEntry::Item {
                icon: "channel-secure-symbolic",
                label: "私钥管理器…",
                action: "keymgr",
                danger: false,
                shortcut: None,
                enabled: None,
            },
            MenuEntry::Item {
                // ⚠️ 通知中心是**可回看**的通知列表（阶段 77c-3）：
                // 桌面通知发不出去（没有通知守护进程）时，它是用户唯一
                // 能回看"刚才发生了什么"的地方 ⇒ 必须有入口。
                icon: "preferences-system-notifications-symbolic",
                label: "通知中心…",
                action: "notify_center",
                danger: false,
                shortcut: None,
                enabled: None,
            },
            MenuEntry::Vault,
            MenuEntry::Sep,
            // 「快捷键一览」「关于」原属独立的「帮助」顶级菜单（阶段 41 并入这里）。
            // ⚠️ 阶段 69：首选项中心（六页）——「外观…」那个入口只是它的
            // 外观页，所以另给一个总入口，用户找得到"设置"在哪儿。
            MenuEntry::Item {
                icon: "preferences-system-symbolic",
                label: "首选项…",
                action: "prefs",
                danger: false,
                shortcut: None,
                enabled: None,
            },
            MenuEntry::Item {
                icon: "preferences-desktop-keyboard-symbolic",
                label: "快捷键一览",
                action: "keys",
                danger: false,
                shortcut: Some("F1"),
                enabled: None,
            },
            MenuEntry::Item {
                icon: "dialog-information-symbolic",
                label: "关于 PuTTYPlus",
                action: "about",
                danger: false,
                shortcut: None,
                enabled: None,
            },
        ],
    ),
];

/// 顶级菜单的标题（按显示顺序）。
///
/// 给 `examples/ui_shot.rs` 量宽度用——**别再手抄一份标题列表**：
/// 抄本会漂移，量出来的数就不是当前菜单栏的数了。
pub fn top_menu_titles() -> Vec<&'static str> {
    TOP_MENUS.iter().map(|(t, _)| *t).collect()
}

/// 某个菜单项在**当前平台**上要不要显示。
///
/// ⚠️ **Windows 上数据库工作台的入口先藏起来**（跨平台版 阶段 C5）：
///
/// - 首版承诺的范围是「**终端 + SFTP + 命令行契约**」，DB 工作台在
///   Windows 上只保证**能编译**（说明它可移植），**没在 Windows 上实测**；
/// - 菜单里摆一个点进去会出问题的入口，比不摆更糟 ⇒ Windows 上不显示；
/// - 代码原样保留（`panel::dbui` 那套还在编）⇒ 第二版放开入口即可。
///
/// ⚠️ 只管**显示**；快捷键那条路在 action 分发处另外拦（见 `main.rs`）。
pub fn entry_visible(entry: &MenuEntry) -> bool {
    #[cfg(windows)]
    {
        if let MenuEntry::Item { action: "db", .. } = entry {
            return false;
        }
    }
    let _ = entry;
    true
}

/// 表里引用到的 action（去重、保序）。
pub fn referenced_actions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, entries) in TOP_MENUS {
        for e in entries.iter().copied() {
            let action = match e {
                MenuEntry::Item { action, .. } => action,
                MenuEntry::Toggle { action, .. } => action,
                _ => continue,
            };
            if !out.contains(&action) {
                out.push(action);
            }
        }
    }
    out
}

/// **全部入口**引用到的 action：顶级菜单 + 「新建」弹层 + 凭据库项。
///
/// 顶级菜单不是唯一的入口：「新建」弹层（阶段 68）与凭据库项也有 action，
/// 对账时得把它们算进来，否则会误报"孤儿动作"。
pub fn all_referenced_actions() -> Vec<&'static str> {
    let mut out = referenced_actions();
    for i in NEW_ITEMS {
        if !out.contains(&i.action) {
            out.push(i.action);
        }
    }
    if !out.contains(&"vault") {
        out.push("vault");
    }
    out
}

/// 注册了、却**没有任何入口**的 action（启动时对账用）。
///
/// **为什么要有**：菜单项还在、action 没了 ⇒ 点了没反应（`missing_actions`
/// 抓这一头）；反过来 action 还在、菜单项没了 ⇒ 快捷键还能用，但菜单里
/// 找不到它，用户不知道它存在。两头都要对账。
pub fn orphan_actions(registered: &[&'static str]) -> Vec<&'static str> {
    let refs = all_referenced_actions();
    registered
        .iter()
        .copied()
        .filter(|a| !refs.contains(a))
        .collect()
}

/// 表里引用了、但 `registered` 里没有的 action（返回 顶级菜单 / 菜单项 / action）。
///
/// 「新建」动作的**作用域**（阶段 68 / 审计 P1-2、P1-6）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewScope {
    /// 全局：跟哪个会话无关（新建连接、导入 PuTTY 会话）
    Global,
    /// 会话内：在**当前会话**里新建（新建终端、新建数据库连接）
    Session,
}

/// 「新建」菜单里的一项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewItem {
    /// 图标（统一取自主题的 symbolic 图标）
    pub icon: &'static str,
    /// 菜单上的文字
    pub label: &'static str,
    /// **action 名**（顶层 action 注册表里的名字）
    pub action: &'static str,
    /// 作用域（会话内的入口只给会话内的项）
    pub scope: NewScope,
}

/// **唯一的**「新建」动作表（阶段 68 / 审计 P1-2、P1-6）。
///
/// ⚠️ 以前"新建"有 **5 个入口**（顶栏 `+` / 侧栏 `+` / 子标签 `+` /
/// 欢迎页按钮 / 文件菜单），**各写各的** ⇒ 从哪个入口进来看到的东西
/// 不一样（子标签 `+` 能建数据库连接，顶栏 `+` 不能；文件菜单叫
/// 「新建连接…」，标签头右键叫「增加数据库连接…」）⇒ 用户以为功能
/// 不一样，得多试几个地方。
///
/// ⇒ 现在所有入口都从这张表取（会话内的入口按 [`NewScope`] 过滤），
/// 动作的**实现**也只有一份（`newmenu::run`）。
pub const NEW_ITEMS: &[NewItem] = &[
    NewItem {
        icon: "list-add-symbolic",
        label: "新建连接…",
        action: "new",
        scope: NewScope::Global,
    },
    NewItem {
        icon: "utilities-terminal-symbolic",
        label: "新建终端",
        action: "new_term",
        scope: NewScope::Session,
    },
    NewItem {
        icon: "drive-harddisk-symbolic",
        label: "新建数据库连接…",
        action: "new_db",
        scope: NewScope::Session,
    },
    NewItem {
        icon: "document-open-symbolic",
        label: "导入 PuTTY 会话…",
        action: "import",
        scope: NewScope::Global,
    },
];

/// 「新建」的**入口**（阶段 68）。
///
/// 入口只是"从表的哪个角度看"，**动作本身只有一份**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewEntry {
    /// 顶栏的「＋新建」
    TopBar,
    /// 侧栏的「＋」
    Sidebar,
    /// 子标签栏的「＋」（**会话内**）
    SubTab,
    /// 欢迎页的主按钮
    Welcome,
    /// 文件菜单的「新建连接…」
    FileMenu,
}

impl NewEntry {
    /// 入口的名字（报错 / 测试里用）。
    pub fn name(self) -> &'static str {
        match self {
            NewEntry::TopBar => tr("顶栏"),
            NewEntry::Sidebar => tr("侧栏"),
            NewEntry::SubTab => tr("子标签栏"),
            NewEntry::Welcome => tr("欢迎页"),
            NewEntry::FileMenu => tr("文件菜单"),
        }
    }

    /// 这个入口提供哪些项（**从 [`NEW_ITEMS`] 里筛**，不另写一份）。
    pub fn items(self) -> Vec<&'static NewItem> {
        match self {
            // 会话内的入口：只给"会话内能做"的（新建终端 / 新建数据库连接）
            NewEntry::SubTab => NEW_ITEMS
                .iter()
                .filter(|i| i.scope == NewScope::Session)
                .collect(),
            // 全局入口：全都给（含会话内的——当前会话里也能建）
            _ => NEW_ITEMS.iter().collect(),
        }
    }

    /// 这个入口的图标。
    ///
    /// ⚠️ **两个 `+` 要换不同图标**（审计 P1-6）：顶栏/侧栏的 `+` 是
    /// 「新建连接」，子标签的 `+` 是「新建**页**」——长得一样、干的活
    /// 不一样，用户分不清。
    pub fn icon(self) -> &'static str {
        match self {
            // 子标签 = 新建**页**（终端 / 数据库），不是新建连接
            NewEntry::SubTab => "tab-new-symbolic",
            _ => "list-add-symbolic",
        }
    }

    /// 这个入口的 tooltip（**写清作用域**）。
    pub fn tooltip(self) -> &'static str {
        match self {
            NewEntry::TopBar | NewEntry::Sidebar => {
                tr("新建…（连接 / 终端 / 数据库连接 / 导入 PuTTY 会话）")
            }
            // 会话内的：说清"只作用于当前会话"
            NewEntry::SubTab => tr("新建页（终端 / 数据库连接）——只作用于当前会话"),
            NewEntry::Welcome => tr("新建连接…（侧栏的「＋」里还能建终端 / 数据库连接）"),
            NewEntry::FileMenu => tr("新建连接…"),
        }
    }
}

/// 列出菜单里引用了、但**没注册**的 action（启动时对账用）。
///
/// **为什么要有这个对账**：菜单并来并去（阶段 41 把「编辑」并进「终端」、
/// 「帮助」并进「工具」），最容易出的错就是**菜单项还在、action 没了**——
/// 点了没反应，界面还不报错，排查起来能磨一天。
///
/// `main.rs` 启动时拿真实的 action group 对一遍，缺了就往 stderr 打一行。
pub fn missing_actions(registered: &[&str]) -> Vec<(&'static str, &'static str, &'static str)> {
    let mut out = Vec::new();
    for (title, entries) in TOP_MENUS {
        for e in entries.iter().copied() {
            if let MenuEntry::Item {
                label, action, ..
            } = e
            {
                if !registered.contains(&action) {
                    out.push((*title, label, action));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文件菜单里得有「清空最近连接」（最近连接太多时的清理入口）。
    #[test]
    fn 文件菜单能清空最近连接() {
        let file = TOP_MENUS
            .iter()
            .find(|(t, _)| *t == "文件")
            .expect("得有「文件」菜单");
        let acts: Vec<&str> = file
            .1
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
        assert!(
            acts.contains(&"clear_recent"),
            "文件菜单里没有清空最近连接：{acts:?}"
        );
    }

    /// **所有「新建」入口都指向同一组 action**（阶段 68 / 审计 P1-2、P1-6）。
    ///
    /// ⚠️ 这是本阶段的核心断言：以前 5 个入口**各写各的** ⇒ 从哪个入口
    /// 进来看到的东西不一样（子标签 `+` 能建数据库连接，顶栏 `+` 不能）。
    /// 现在动作表只有一份，各入口只准从表里取。
    #[test]
    fn 所有新建入口都指向同一组动作() {
        let all: Vec<&str> = NEW_ITEMS.iter().map(|i| i.action).collect();
        assert!(!all.is_empty(), "动作表空了");
        // 动作名不许重名（重名会让两个入口指向同一个动作，用户看不出来）
        let mut uniq = all.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), all.len(), "动作表里有重名：{all:?}");

        let entries = [
            NewEntry::TopBar,
            NewEntry::Sidebar,
            NewEntry::SubTab,
            NewEntry::Welcome,
            NewEntry::FileMenu,
        ];
        for e in entries {
            let items = e.items();
            assert!(!items.is_empty(), "{} 一个动作都没给", e.name());
            for it in items {
                assert!(
                    all.contains(&it.action),
                    "{} 给了不在表里的动作：{}",
                    e.name(),
                    it.action
                );
            }
        }
        // 全局入口给**全集**
        assert_eq!(NewEntry::TopBar.items().len(), NEW_ITEMS.len());
        assert_eq!(NewEntry::Sidebar.items().len(), NEW_ITEMS.len());
        // 会话内的入口只给会话内的（不能在里面建连接——那不属于这个会话）
        for it in NewEntry::SubTab.items() {
            assert_eq!(it.scope, NewScope::Session, "子标签给了全局动作");
        }
        assert!(NewEntry::SubTab.items().len() < NEW_ITEMS.len());
    }

    /// 两个 `+` 换不同图标 + tooltip 写清作用域（审计 P1-6）。
    #[test]
    fn 两个新建按钮长得不一样() {
        assert_ne!(
            NewEntry::TopBar.icon(),
            NewEntry::SubTab.icon(),
            "顶栏的「＋」和子标签的「＋」长得一样，用户分不清"
        );
        // 顶栏与侧栏都是**全局**入口（同一件事）⇒ 图标必须一致，
        // 跟子标签的「新建**页**」区分开。
        assert_eq!(
            NewEntry::TopBar.icon(),
            NewEntry::Sidebar.icon(),
            "两个全局入口的图标应当一致"
        );
        // 会话内的入口要说清"只作用于当前会话"
        assert!(NewEntry::SubTab.tooltip().contains("当前会话"));
        // 每个入口的 tooltip 都不能是空的
        for e in [
            NewEntry::TopBar,
            NewEntry::Sidebar,
            NewEntry::SubTab,
            NewEntry::Welcome,
            NewEntry::FileMenu,
        ] {
            assert!(!e.tooltip().is_empty(), "{} 没有 tooltip", e.name());
        }
    }

    #[test]
    fn 顶级菜单是五个() {
        assert_eq!(top_menu_titles(), vec!["文件", "连接", "终端", "视图", "工具"]);
    }

    #[test]
    fn 编辑与帮助已并入别处() {
        // 阶段 41：这两个顶级菜单被拆掉，但功能项一个没少。
        let titles = top_menu_titles();
        assert!(!titles.contains(&"编辑"));
        assert!(!titles.contains(&"帮助"));
        // 复制/粘贴/查找进了「终端」，快捷键/关于进了「工具」。
        let actions = |name: &str| -> Vec<&'static str> {
            TOP_MENUS
                .iter()
                .find(|(t, _)| *t == name)
                .map(|(_, items)| {
                    items
                        .iter()
                        .filter_map(|e| match e {
                            MenuEntry::Item { action, .. } => Some(*action),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(&actions("终端")[..3], &["copy", "paste", "find"]);
        // ⚠️ 这里断言的是「编辑 / 帮助**没有**变成独立顶级菜单」，
        // **不是**「工具菜单里恰好有哪几项」——后者是抄本，菜单一增删
        // 就得跟着改（阶段 77c-3 加了「通知中心…」就挂过一次）。
        let tools = actions("工具");
        assert!(
            tools.contains(&"keys") && tools.contains(&"about"),
            "「帮助」的内容（快捷键一览 / 关于）应当并进「工具」：{tools:?}"
        );
        assert!(
            !tools.contains(&"edit") && !tools.contains(&"help"),
            "「编辑 / 帮助」不该是独立入口：{tools:?}"
        );
        // 复制/粘贴/查找归「终端」，不在「工具」里重复一份
        assert!(
            !tools.contains(&"copy") && !tools.contains(&"paste"),
            "复制/粘贴在「终端」菜单里，别在「工具」里重复：{tools:?}"
        );
    }

    #[test]
    fn 标题不重复() {
        let mut seen = Vec::new();
        for (t, _) in TOP_MENUS {
            assert!(!seen.contains(t), "顶级菜单标题重复：{t}");
            seen.push(*t);
        }
    }

    #[test]
    fn 每个菜单都有内容() {
        for (t, items) in TOP_MENUS {
            assert!(!items.is_empty(), "顶级菜单「{t}」是空的");
        }
    }

    #[test]
    fn action_都在表里注册过() {
        // 「凭据库」是 MenuEntry::Vault（不走 action 表），其余都必须是 Item。
        let refs = referenced_actions();
        assert!(refs.contains(&"copy"));
        assert!(refs.contains(&"paste"));
        assert!(refs.contains(&"find"));
        assert!(refs.contains(&"keys"));
        assert!(refs.contains(&"about"));
    }

    #[test]
    fn 缺的_action_会被揪出来() {
        // 全注册 → 没有缺口
        let all = referenced_actions();
        assert!(missing_actions(&all).is_empty());
        // 少注册一个 → 揪出来（连同它属于哪个菜单、菜单项叫什么）
        let less: Vec<&str> = all.iter().copied().filter(|a| *a != "copy").collect();
        let miss = missing_actions(&less);
        assert_eq!(miss, vec![("终端", "复制", "copy")]);
    }

    /// 菜单里写的快捷键**都能在快捷键表里查到**（阶段 74 / 审计 P1-13）。
    ///
    /// ⚠️ 菜单上写着一个表里没有的快捷键 ⇒ 用户按了没反应，还以为坏了。
    #[test]
    fn 菜单里的快捷键都能在快捷键表里查到() {
        let mut n = 0;
        for (title, entries) in TOP_MENUS {
            for e in entries.iter().copied() {
                let (label, sc) = match e {
                    MenuEntry::Item { label, shortcut, .. } => (label, shortcut),
                    MenuEntry::Toggle { label, shortcut, .. } => (label, shortcut),
                    _ => continue,
                };
                if let Some(s) = sc {
                    assert!(
                        crate::shortcuts::by_display(s).is_some(),
                        "顶级菜单「{title}」的「{label}」写了快捷键「{s}」，快捷键表里查不到"
                    );
                    n += 1;
                }
            }
        }
        assert!(n >= 10, "菜单里几乎没写快捷键（{n} 个）");
    }

    /// **窗口级快捷键都在顶栏菜单里露出来了**（阶段 74）。
    ///
    /// 快捷键表是唯一事实来源，用户按 F1 / Ctrl+Shift+T 能用，但菜单里
    /// 一个字都看不到 ⇒ 用户根本不知道它们存在。
    #[test]
    fn 窗口级快捷键都在顶栏菜单里() {
        let mut in_menu: Vec<&'static str> = Vec::new();
        for (_, entries) in TOP_MENUS {
            for e in entries.iter().copied() {
                match e {
                    MenuEntry::Item {
                        shortcut: Some(s), ..
                    } => in_menu.push(s),
                    MenuEntry::Toggle {
                        shortcut: Some(s), ..
                    } => in_menu.push(s),
                    _ => {}
                }
            }
        }
        for b in crate::shortcuts::window_bindings() {
            assert!(
                in_menu.contains(&b.display),
                "窗口级快捷键「{}」（{}）在顶栏菜单里没有露出来",
                b.display,
                b.desc
            );
        }
    }

    /// 菜单标题与菜单项的文案**都要有英文译文**。
    ///
    /// ⚠️ 这些文字在 `const` 表里（编译期求值 ⇒ 不能调 `tr()`）⇒ 显示时
    ///    才翻译。表里漏了一条 ⇒ 英文界面上那一条还是中文。
    #[test]
    fn 菜单文案都有英文译文() {
        for (title, entries) in TOP_MENUS {
            assert!(
                crate::i18n::has_translation(title),
                "顶级菜单「{title}」没有英文译文"
            );
            for e in entries.iter() {
                let label = match e {
                    MenuEntry::Item { label, .. } | MenuEntry::Toggle { label, .. } => Some(*label),
                    _ => None,
                };
                if let Some(l) = label {
                    assert!(crate::i18n::has_translation(l), "菜单项「{l}」没有英文译文");
                }
            }
        }
    }

    /// 开关项**统一成 Toggle**：勾选标记说开还是关（阶段 74 / 审计 P1-13）。
    ///
    /// ⚠️ 以前「显示侧边栏」是 Toggle（有勾），「按键广播（开关）」「记录
    /// 终端日志（开关）」「录制会话（开关）」是普通项（没勾，只在文字里
    /// 写"开关"）⇒ 同一类状态三种表现。
    #[test]
    fn 开关项都用同一种表现() {
        for name in ["按键广播", "记录终端日志", "录制会话"] {
            let e = TOP_MENUS
                .iter()
                .flat_map(|(_, es)| es.iter())
                .find(|e| matches!(e, MenuEntry::Toggle { label, .. } if *label == name))
                .unwrap_or_else(|| panic!("「{name}」没有做成 Toggle（勾选标记）"));
            assert!(
                !matches!(e, MenuEntry::Toggle { label, .. } if label.contains("（开关）")),
                "「{name}」的文字里不该再写「（开关）」——勾选标记已经说明了"
            );
        }
        // 菜单里不许再有靠文字说明的「（开关）」项
        for (_, entries) in TOP_MENUS {
            for e in entries.iter().copied() {
                if let MenuEntry::Item { label, .. } = e {
                    assert!(!label.contains("（开关）"), "「{label}」还是普通项");
                }
            }
        }
    }

    /// **每个 action 都有对应菜单或快捷键**（阶段 74）。
    ///
    /// 反向对账：注册了却没有任何入口的 action = 孤儿（菜单里找不到它）。
    #[test]
    fn 孤儿动作会被揪出来() {
        let refs = all_referenced_actions();
        assert!(orphan_actions(&refs).is_empty(), "菜单表自己就不自洽");
        let mut reg = refs.clone();
        reg.push("没人点的动作");
        assert_eq!(orphan_actions(&reg), vec!["没人点的动作"]);
    }
}
