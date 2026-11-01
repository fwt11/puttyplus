//! 顶栏菜单的**弹层正文**（阶段 74 / 审计 P1-13）。
//!
//! **为什么从 `main.rs` 里抽出来**：这段原来在 bin 里 ⇒ example 冒烟
//! **测不到**（例子链不到 bin）⇒ 「快捷键列有没有渲染」「没有终端时菜单项
//! 是不是灰的」只能靠人眼看。现在搬到 lib，冒烟能直接验这两件事。
//!
//! **菜单表**在 [`crate::topmenus`]；这里只管"怎么把它画出来"。

use std::rc::Rc;

use gtk::prelude::*;

use crate::menu;
use crate::topmenus::MenuEntry;

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
    let vb = menu::body();
    // 菜单项 + 它的可用条件（阶段 74）：弹出前按当前状态刷灰。
    let mut gated: Vec<(gtk::Widget, &'static str, Option<fn() -> bool>)> = Vec::new();
    // 开关项的当前状态（勾 = 开）：菜单项只建一次 ⇒ 每次弹出前问一次。
    let mut toggles: Vec<(gtk::ToggleButton, fn() -> bool)> = Vec::new();
    for e in entries.iter().copied() {
        match e {
            MenuEntry::Sep => {
                vb.append(&menu::sep());
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
                let b = menu::item(icon, label, shortcut);
                if danger {
                    menu::danger(&b);
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
                let b = menu::toggle(icon, label, shortcut, state());
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
                let b = menu::item(
                    "dialog-password-symbolic",
                    &format!(
                        "凭据库…（{}）",
                        crate::vaultui::state_short(crate::vault::state())
                    ),
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
                    menu::set_item_label(
                        &b,
                        &format!("凭据库…（{}）", crate::vaultui::state_short(st)),
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
