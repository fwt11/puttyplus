//! 统一的面板头协议（阶段 143）。
//!
//! **协议长这样**：
//!
//! ```text
//! ┌ 标题（可省略）                  [动作键…] [⋯] ┐
//! └────────────────────────────────────────────────┘
//! ```
//!
//! - 头部行统一挂 [`HEAD_CLASS`]（`pp-panel-head`）⇒ **留白与间距全由 CSS 管**
//!   ⇒ 切密度**立即生效**（不用重建控件）。
//! - 动作键统一用 [`icon_action`]（flat ＋ tooltip↔可访问名称同步）。
//! - 低频动作统一收进 [`Overflow`] 的 `⋯` 弹层（自绘，跟项目其它菜单同款）。
//!
//! **为什么要统一**：现在各面板的头部各写各的（文件面板一行、编辑器一行、
//! 容器页又不一样）⇒ 按钮位置 / 间距 / 可访问名全不一致 ⇒
//! 「占地方」＋「记不住」双重来源。
//!
//! ⚠️ **阶段 144 的格子头直接复用本模块** ⇒ 协议只写一遍。

use gtk::prelude::*;

/// 所有面板头共用的 CSS 类（**留白与间距由 CSS 一处管**）。
pub const HEAD_CLASS: &str = "pp-panel-head";

/// `⋯` 溢出按钮用的图标（Adwaita 自带）。
pub const OVERFLOW_ICON: &str = "view-more-symbolic";

/// 一条标准头部行。
pub struct HeadRow {
    /// 行本身（挂 [`HEAD_CLASS`]）。
    pub bar: gtk::Box,
    /// 标题（`None` = 不要标题）。
    pub title: Option<gtk::Label>,
    /// 右侧动作组（新按钮往这里加）。
    pub actions: gtk::Box,
}

/// 建一条标准头部行。
///
/// ⚠️ **间距不在这里定**（不要 `set_margin_*` / `set_spacing`）：
/// 全交给 CSS 的 [`HEAD_CLASS`] ⇒ 切密度立即生效。
pub fn head_row(title: Option<&str>) -> HeadRow {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class(HEAD_CLASS);
    let title = title.map(|t| {
        let l = gtk::Label::new(Some(t));
        l.set_halign(gtk::Align::Start);
        l.set_xalign(0.0);
        l
    });
    if let Some(l) = &title {
        bar.append(l);
    }
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.append(&actions);
    HeadRow {
        bar,
        title,
        actions,
    }
}

/// 一个动作键（纯图标）。
///
/// ⚠️ **可访问名称靠 tooltip**（[`crate::a11y::watch_tooltip`]）——
/// 纯图标按钮没有文字，屏幕阅读器得靠这个才知道它是干什么的。
pub fn icon_action(icon: &str, tip: &str) -> gtk::Button {
    // ⚠️ **必须走 `a11y::icon_button`**（项目 lint 钉死）：纯图标按钮
    //    没有文字 ⇒ 可访问名称只能靠 tooltip 同步过去。
    let b = crate::a11y::icon_button(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b
}

/// `⋯` 溢出菜单（低频动作的去处）。
///
/// ⚠️ **用普通 Button ＋ 自绘弹层**（不是 `MenuButton`）：
/// `MenuButton` 自带的小三角压不掉，而且它的弹层走 `set_popover()`
/// ⇒ 内容一次性建好、上下文变了菜单不跟着变。
/// ⇒ 这里跟侧栏的「＋」菜单同款做法（见 `src/sidebar.rs`）。
pub struct Overflow {
    /// 按钮本身（放进工具栏）。
    pub button: gtk::Button,
    /// 菜单体：往这里加 [`crate::menu::item`] / [`crate::menu::toggle`]。
    pub body: gtk::Box,
}

impl Overflow {
    /// 建一个 `⋯` 按钮（**不挂任何菜单项**）。
    pub fn new(tip: &str) -> Overflow {
        // ⚠️ 同样走 `a11y::icon_button`（可访问名称靠 tooltip 同步）。
        let button = crate::a11y::icon_button(OVERFLOW_ICON);
        button.add_css_class("flat");
        button.set_tooltip_text(Some(tip));
        let body = crate::menu::body();
        let pop = crate::menu::popover(&button);
        pop.set_child(Some(&body));
        button.connect_clicked(move |_| pop.popup());
        Overflow { button, body }
    }

    /// 加一行**普通动作**。
    pub fn action(&self, icon: &str, label: &str, f: impl Fn() + 'static) {
        let b = self.action_button(icon, label);
        b.connect_clicked(move |_| f());
    }

    /// 建一行**普通动作**（**返回那个菜单项本身**）。
    ///
    /// ⚠️ 返回它是为了让调用方**存起来**：面板状态里常存着这些按钮
    /// （灵敏度、勾选态都由业务代码改）⇒ 存的是**同一个控件**，
    /// 不会出现「工具栏上那个」与「菜单里那个」各说各话。
    pub fn action_button(&self, icon: &str, label: &str) -> gtk::Button {
        let b = crate::menu::item(icon, label, None);
        self.body.append(&b);
        b
    }

    /// 加一行**开关**（带勾选态）。
    ///
    /// ⚠️ **返回那个开关本身**：调用方常常要把它存起来（面板状态里
    /// 存着「跟随」开关 ⇒ 别的代码改状态时，菜单里的勾也跟着变）。
    pub fn toggle(
        &self,
        icon: &str,
        label: &str,
        active: bool,
        f: impl Fn(bool) + 'static,
    ) -> gtk::ToggleButton {
        let b = self.toggle_button(icon, label, active);
        b.connect_toggled(move |b| f(b.is_active()));
        b
    }

    /// 建一行**开关**（**返回那个开关本身**，理由同 [`action_button`]）。
    pub fn toggle_button(&self, icon: &str, label: &str, active: bool) -> gtk::ToggleButton {
        let b = crate::menu::toggle(icon, label, None, active);
        self.body.append(&b);
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 头部类名合法() {
        assert!(HEAD_CLASS.starts_with("pp-"));
        assert!(!HEAD_CLASS.contains(' '));
    }

    #[test]
    fn 溢出图标是_adwaita_真有的() {
        // ⚠️ 写错图标名 ⇒ 界面上是个**空白按钮**（不报错）⇒ 钉死这个名字。
        assert_eq!(OVERFLOW_ICON, "view-more-symbolic");
    }
}
