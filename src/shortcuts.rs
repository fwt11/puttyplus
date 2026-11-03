//! 快捷键集中定义表 + cheatsheet（4.4）。
//!
//! **表是唯一事实来源**：窗口级按键由本表驱动（`main.rs` 遍历匹配），
//! 终端内的局部按键（查找条里的 Shift+Enter 等）从本表取说明，
//! cheatsheet 对话框也直接渲染本表。
//!
//! 以前按键散在三处（窗口 handler、`term.rs` 的查找条、若干 tooltip），
//! 改一处忘另一处是必然的——所以让它们都指回这张表。
//!
//! 匹配用 GDK keysym 名而不是 `gtk::AccelLabel`/`AccelGroup`：
//! 我们要的是"按下就执行"，不是"菜单项上显示一行加速键文字"，
//! 引入加速键组反而绕。显示串自己拼，省一层依赖。

/// 快捷键的作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 窗口级：由 `main.rs` 的捕获阶段按键控制器分发
    Window,
    /// 终端内局部：由对应控件自己的按键控制器处理（这里只登记说明）
    Terminal,
}

/// 窗口级动作。分发时按这个走，别再写第二份 if-else。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    NewTab,
    CloseTab,
    ToggleSidebar,
    ToggleSearch,
    Copy,
    Paste,
    ToggleBroadcast,
    ShowCheatsheet,
    OpenReplay,
    QuickCommands,
    OpenDatabase,
}

#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub id: Id,
    /// 作用域
    pub scope: Scope,
    /// 显示用（"Ctrl+Shift+T"）
    pub display: &'static str,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// GDK keysym 名（小写，如 "t" / "Return" / "ISO_Left_Tab"）
    pub key: &'static str,
    pub group: &'static str,
    pub desc: &'static str,
}

impl Binding {
    /// 按下事件是否命中这条绑定。
    ///
    /// 修饰键要求**精确匹配**：Ctrl+Shift+T 不该被光按了 Ctrl+T 的手
    /// 顺带触发，否则两个键位含义就重叠了。
    pub fn matches(&self, keyname: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        self.ctrl == ctrl && self.shift == shift && self.alt == alt && self.key.eq_ignore_ascii_case(keyname)
    }
}

/// 快捷键的分组名与说明**都要有英文译文**。
///
/// ⚠️ 同 `topmenus`：它们在 `const` 表里 ⇒ 显示时才翻译 ⇒ 漏一条
///    英文界面上那一条就是中文。
#[test]
fn 快捷键文案都有英文译文() {
    for b in ALL.iter() {
        assert!(
            crate::i18n::has_translation(b.group),
            "快捷键分组「{}」没有英文译文",
            b.group
        );
        assert!(
            crate::i18n::has_translation(b.desc),
            "快捷键说明「{}」没有英文译文",
            b.desc
        );
    }
}

const WIN: Scope = Scope::Window;
const TERM: Scope = Scope::Terminal;

/// 全部快捷键。**新增按键只改这一处**。
pub const ALL: &[Binding] = &[
    Binding {
        id: Id::NewTab,
        scope: WIN,
        display: "Ctrl+Shift+T",
        ctrl: true,
        shift: true,
        alt: false,
        key: "t",
        group: "标签",
        desc: "新建连接（在当前窗口开新标签）",
    },
    Binding {
        id: Id::CloseTab,
        scope: WIN,
        display: "Ctrl+Shift+W",
        ctrl: true,
        shift: true,
        alt: false,
        key: "w",
        group: "标签",
        desc: "关闭当前标签",
    },
    Binding {
        id: Id::ShowCheatsheet,
        scope: WIN,
        display: "F1",
        ctrl: false,
        shift: false,
        alt: false,
        key: "F1",
        group: "标签",
        desc: "打开本快捷键一览",
    },
    Binding {
        id: Id::ToggleSidebar,
        scope: WIN,
        display: "Ctrl+Shift+B",
        ctrl: true,
        shift: true,
        alt: false,
        key: "b",
        group: "界面",
        desc: "显示/隐藏 会话侧边栏",
    },
    Binding {
        id: Id::ToggleSearch,
        scope: WIN,
        display: "Ctrl+F",
        ctrl: true,
        shift: false,
        alt: false,
        key: "f",
        group: "界面",
        desc: "在当前终端中查找（再按一次收起）",
    },
    Binding {
        id: Id::Copy,
        scope: WIN,
        display: "Ctrl+Shift+C",
        ctrl: true,
        shift: true,
        alt: false,
        key: "c",
        group: "终端",
        desc: "复制选中内容",
    },
    Binding {
        id: Id::Paste,
        scope: WIN,
        display: "Ctrl+Shift+V",
        ctrl: true,
        shift: true,
        alt: false,
        key: "v",
        group: "终端",
        desc: "粘贴剪贴板",
    },
    Binding {
        id: Id::ToggleBroadcast,
        scope: WIN,
        display: "Alt+Shift+B",
        ctrl: false,
        shift: true,
        alt: true,
        key: "b",
        group: "输入广播",
        desc: "开/关按键广播（发往其它标签的终端）",
    },
    Binding {
        id: Id::OpenReplay,
        scope: WIN,
        display: "Ctrl+Shift+O",
        ctrl: true,
        shift: true,
        alt: false,
        key: "o",
        group: "会话录制",
        desc: "打开 .cast 录制文件回放",
    },
    Binding {
        id: Id::QuickCommands,
        scope: WIN,
        display: "Ctrl+Shift+K",
        ctrl: true,
        shift: true,
        alt: false,
        key: "k",
        group: "快速命令",
        desc: "打开快速命令面板（把预设命令敲进当前终端）",
    },
    Binding {
        id: Id::OpenDatabase,
        scope: WIN,
        display: "Ctrl+Shift+D",
        ctrl: true,
        shift: true,
        alt: false,
        key: "d",
        group: "会话",
        desc: "打开数据库窗口（复用当前 SSH 连接）",
    },
    Binding {
        id: Id::ToggleSearch,
        scope: TERM,
        display: "Shift+Enter",
        ctrl: false,
        shift: true,
        alt: false,
        key: "Return",
        group: "终端查找",
        desc: "查找上一个匹配项",
    },
    Binding {
        id: Id::ToggleSearch,
        scope: TERM,
        display: "Esc",
        ctrl: false,
        shift: false,
        alt: false,
        key: "Escape",
        group: "终端查找",
        desc: "关闭查找条，焦点回终端",
    },
    Binding {
        id: Id::OpenDatabase,
        scope: TERM,
        display: "Ctrl+Enter",
        ctrl: true,
        shift: false,
        alt: false,
        key: "Return",
        group: "数据库窗口",
        desc: "在 SQL 编辑器里执行（执行整个编辑框的内容）",
    },
    Binding {
        id: Id::OpenDatabase,
        scope: TERM,
        display: "Ctrl+Shift+Enter",
        ctrl: true,
        shift: true,
        alt: false,
        key: "Return",
        group: "数据库窗口",
        desc: "在 SQL 编辑器里只执行选中的那段",
    },
];

/// 窗口级按键（供分发遍历）。
pub fn window_bindings() -> impl Iterator<Item = &'static Binding> {
    ALL.iter().filter(|b| b.scope == Scope::Window)
}

/// 终端内登记的按键说明（终端查找的 Shift+Enter 由 term.rs 消费，
/// 说明文字从表里取，避免两处各写一遍）。
pub fn by_display(display: &str) -> Option<&'static Binding> {
    ALL.iter().find(|b| b.display == display)
}

/// 分组后的清单，供 cheatsheet 渲染。
pub fn grouped() -> Vec<(&'static str, Vec<&'static Binding>)> {
    let mut out: Vec<(&'static str, Vec<&'static Binding>)> = Vec::new();
    for b in ALL {
        match out.iter_mut().find(|(g, _)| *g == b.group) {
            Some((_, v)) => v.push(b),
            None => out.push((b.group, vec![b])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 显示串与修饰键标记一致() {
        // 表里的 display 是给人看的，标记是给匹配用的；两者对不上就成了
        // "文档说 Ctrl+Shift+T、实际按 Ctrl+T 才有反应"
        for b in ALL {
            let d = b.display;
            assert_eq!(d.contains("Ctrl"), b.ctrl, "{}: Ctrl 标记不符", d);
            assert_eq!(d.contains("Shift"), b.shift, "{}: Shift 标记不符", d);
            assert_eq!(d.contains("Alt"), b.alt, "{}: Alt 标记不符", d);
            // 单字符键名必须出现在显示串里（不分大小写：显示是 "T"、键名是 "t"）。
            // 特殊键不查——显示用"人话"（Enter / Escape / F1），键名用 GDK
            // keysym（Return / Escape / F1），两者本来就不一定同形
            if b.key.len() == 1 {
                assert!(
                    d.to_lowercase().contains(&b.key.to_lowercase()),
                    "{}: 键名没出现在显示串里",
                    d
                );
            }
        }
    }

    #[test]
    fn 没有重复绑定() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                if a.scope == b.scope && a.ctrl == b.ctrl && a.shift == b.shift && a.alt == b.alt && a.key == b.key
                {
                    panic!("重复绑定：{} 与 {}", a.display, b.display);
                }
            }
        }
    }

    #[test]
    fn 修饰键要精确匹配() {
        let t = ALL.iter().find(|b| b.id == Id::NewTab).unwrap();
        assert!(t.matches("t", true, true, false));
        // 只按 Ctrl 不算命中——否则 Ctrl+T 会被吞掉
        assert!(!t.matches("t", true, false, false));
        assert!(!t.matches("T", true, false, false));
        assert!(!t.matches("w", true, true, false));
    }

    #[test]
    fn 键名不分大小写() {
        let t = ALL.iter().find(|b| b.id == Id::NewTab).unwrap();
        assert!(t.matches("T", true, true, false));
    }

    #[test]
    fn 窗口级键都能被查到() {
        for id in [
            Id::NewTab,
            Id::CloseTab,
            Id::ToggleSidebar,
            Id::ToggleSearch,
            Id::Copy,
            Id::Paste,
            Id::ToggleBroadcast,
            Id::ShowCheatsheet,
            Id::OpenReplay,
            Id::QuickCommands,
            Id::OpenDatabase,
        ] {
            assert!(
                window_bindings().any(|b| b.id == id),
                "{id:?} 没有登记窗口级绑定"
            );
        }
    }

    #[test]
    fn cheatsheet_能按显示串反查() {
        assert_eq!(
            by_display("Alt+Shift+B").map(|b| b.id),
            Some(Id::ToggleBroadcast)
        );
        assert!(by_display("不存在的键").is_none());
    }

    #[test]
    fn 分组后条目一条不丢() {
        let g = grouped();
        assert_eq!(g.iter().map(|(_, v)| v.len()).sum::<usize>(), ALL.len());
        assert!(g.iter().any(|(name, _)| *name == "输入广播"));
    }
}
