//! 可访问性（无障碍）辅助。
//!
//! 审计 P2-7：纯图标按钮只有 tooltip，**读屏软件只会念"按钮"**——
//! 用户知道有个按钮，却不知道是哪个（"三个长得一样的方块"）。
//!
//! GTK4 里读屏念的是 **accessible label**（`gtk::accessible::Property::Label`），
//! tooltip 只是"描述"，两者不是一回事 ⇒ 光有 tooltip 不算数。
//!
//! ⚠ GTK4 **没有"读回 accessible label"的接口**（`update_property` 只写不读，
//! 读屏那边是 GTK/AT-SPI 的事）。所以这里顺手给控件挂一个
//! `pp-a11y-labelled` CSS 类作为**自己人的标记**：冒烟要断言
//! 「图标按钮都有可访问名称」只能靠它。

use gtk::prelude::*;

/// 控件已经设过可访问名称时挂的 CSS 类（冒烟与排查用）。
pub const LABELLED_CLASS: &str = "pp-a11y-labelled";

/// 给控件设可访问名称（读屏念出来的那句）。
///
/// 空名字直接跳过：GTK 收到空 label 反而会念成空。
pub fn set_label(w: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>), name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    w.update_property(&[gtk::accessible::Property::Label(name)]);
    w.add_css_class(LABELLED_CLASS);
}

/// 让控件的**可访问名称跟着 tooltip 走**。
///
/// 做法：盯住 `tooltip-text` 属性，一变就把同样的文字写进 accessible label。
/// 这样调用方**照旧**写 `set_tooltip_text(...)`，不用两处写同一个字符串——
/// 两处写，迟早有一处忘（界面不会报错，只会悄悄读不出来）。
///
/// 同步是异步的（notify 信号在设完 tooltip 之后才发），所以刚建好还没设
/// tooltip 的按钮暂时没有可访问名称——正常，tooltip 一到就有。
///
/// 凡是**纯图标**的按钮/开关/菜单按钮都要调一次（不管图标是
/// `from_icon_name` 来的还是 `set_child` 塞进去的）。
pub fn watch_tooltip(w: &impl IsA<gtk::Widget>) {
    let w: &gtk::Widget = w.upcast_ref();
    w.connect_notify_local(Some("tooltip-text"), move |w, _| {
        match w.tooltip_text() {
            Some(t) if !t.trim().is_empty() => set_label(w, &t),
            // tooltip 被清掉了 ⇒ 可访问名称也该跟着没
            _ => w.remove_css_class(LABELLED_CLASS),
        }
    });
}

/// 图标按钮：自带 tooltip↔可访问名称同步。
///
/// 剩下要做的照旧：`set_tooltip_text` 写清这个按钮是干什么的。
pub fn icon_button(icon: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    watch_tooltip(&b);
    b
}

/// 这个控件有没有设过可访问名称（冒烟用；GTK 那边读不回来）。
pub fn has_label(w: &impl IsA<gtk::Widget>) -> bool {
    w.has_css_class(LABELLED_CLASS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 标记类名不与既有类冲突() {
        assert!(LABELLED_CLASS.starts_with("pp-"));
        assert!(!LABELLED_CLASS.contains(' '));
    }
}
