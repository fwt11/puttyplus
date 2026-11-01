//! 空态规范（阶段 80 / 审计 P2-1）：**标题 + 一句解释 + 一个主 CTA**（可选一个次 CTA）。
//!
//! 以前每个地方的空态各写各的：有的是一行字，有的是一行**会被选中的**
//! Label（私钥管理器），有的干脆一片空白 ⇒ 用户看着像"功能坏了"。
//!
//! 现在所有空态都从 [`EmptyState`] 走：
//! - `primary` **不是 Option** ⇒ 没有主 CTA 的空态**构造不出来**
//!   （编译期就挡住"忘了放主按钮"这种事）；
//! - 文案与图标只有一份（[`all_empty_states`]），单元测试照它逐条断言。
//!
//! ⚠️ 图标名必须是**当前主题里真的存在**的：Adwaita 没有 `emblem-ok-symbolic`
//! （只有 Papirus 有）⇒ GTK 会静默回退成一个默认图标，看着完全不是那个意思。

use gtk::prelude::*;

/// 一个 CTA（按钮）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cta {
    /// 按钮文字。
    pub label: &'static str,
    /// 图标名（Adwaita 里真的存在的）。
    pub icon: &'static str,
}

/// 空态的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyState {
    /// 标题：一句话说这是什么。
    pub title: String,
    /// 解释：为什么是空的 / 下一步能干什么。
    pub desc: String,
    /// **主 CTA**（必填）。
    pub primary: Cta,
    /// 次 CTA（可选）。
    pub secondary: Option<Cta>,
}

impl EmptyState {
    /// 造一个空态（主 CTA 必填）。
    pub fn new(title: &str, desc: &str, primary: Cta) -> Self {
        Self {
            title: title.to_string(),
            desc: desc.to_string(),
            primary,
            secondary: None,
        }
    }

    /// 加一个次 CTA。
    pub fn with_secondary(mut self, c: Cta) -> Self {
        self.secondary = Some(c);
        self
    }
}

/* ---------------- 全应用用到的空态 ---------------- */

/// 侧栏：一条连接都没有。
pub fn sidebar() -> EmptyState {
    EmptyState::new(
        "还没有连接",
        "连一台机器、传个文件、跑个数据库查询 —— 先建一个连接。",
        Cta {
            label: "新建连接",
            icon: "list-add-symbolic",
        },
    )
}

/// 私钥管理器：~/.ssh 里一把钥匙都没有。
///
/// `has_ssh_dir` = ~/.ssh 目录在不在（说明文字不一样：
/// 「没找到私钥」与「目录还不存在」是两回事）。
pub fn keys(has_ssh_dir: bool) -> EmptyState {
    let desc = if has_ssh_dir {
        "~/.ssh 下没有找到私钥。生成一对就能用了。"
    } else {
        "~/.ssh 目录还不存在，先生成一对密钥（会一并建好目录）。"
    };
    EmptyState::new(
        "还没有密钥",
        desc,
        Cta {
            label: "生成第一对密钥",
            icon: "document-new-symbolic",
        },
    )
}

/// 传输队列：一批都没传。
pub fn queue() -> EmptyState {
    EmptyState::new(
        "还没有传输",
        "从文件面板拖一个文件过去，或在文件菜单里选「传输…」。",
        Cta {
            label: "打开传输队列",
            icon: "folder-download-symbolic",
        },
    )
}

/// 通知中心：一条通知都没有。
pub fn notifications() -> EmptyState {
    EmptyState::new(
        "还没有通知",
        "传输完成、连接失败这些事会记在这里，错过桌面通知也还能回看。",
        Cta {
            label: "知道了",
            icon: "dialog-information-symbolic",
        },
    )
}

/// 全应用声明过的空态（单元测试照这份清单断言）。
pub fn all_empty_states() -> Vec<EmptyState> {
    vec![sidebar(), keys(true), queue(), notifications()]
}

/* ---------------- 构件 ---------------- */

/// 按规范建一个空态：标题 + 解释 + 主 CTA（+ 次 CTA）。
///
/// 返回 (容器, 主 CTA 按钮) —— 主按钮的句柄给调用方接点击回调
/// （也带 CSS 类 `pp-empty-primary`，冒烟能定位）。
pub fn build(s: &EmptyState) -> (gtk::Box, gtk::Button) {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 8);
    box_.set_halign(gtk::Align::Center);
    box_.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(Some(&s.title));
    title.add_css_class("pp-empty-title");
    box_.append(&title);

    let desc = gtk::Label::new(Some(&s.desc));
    desc.add_css_class("pp-empty-sub");
    desc.set_xalign(0.5);
    desc.set_wrap(true);
    // ⚠️ 必须限宽 + 开 wrap：不限宽的话这一行文字的自然宽能把整个容器撑破
    // （窄面板里最典型的"卡片被裁"）。
    desc.set_max_width_chars(40);
    box_.append(&desc);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Center);
    let primary = cta_button(&s.primary, true);
    row.append(&primary);
    if let Some(c) = &s.secondary {
        row.append(&cta_button(c, false));
    }
    box_.append(&row);

    (box_, primary)
}

/// CTA 按钮（图标 + 文字）。
fn cta_button(c: &Cta, primary: bool) -> gtk::Button {
    let b = gtk::Button::new();
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if !c.icon.is_empty() {
        h.append(&gtk::Image::from_icon_name(c.icon));
    }
    h.append(&gtk::Label::new(Some(c.label)));
    b.set_child(Some(&h));
    b.add_css_class("pp-empty-btn");
    if primary {
        b.add_css_class("pp-empty-primary");
        // ⚠️ 主 CTA 要**一眼看出来**：它是这块地方唯一该点的按钮
        b.add_css_class("suggested-action");
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 每个空态都有主CTA() {
        for s in all_empty_states() {
            assert!(
                !s.primary.label.is_empty(),
                "空态「{}」没有主 CTA 的文字",
                s.title
            );
            assert!(
                !s.primary.icon.is_empty(),
                "空态「{}」的主 CTA 没有图标",
                s.title
            );
            assert!(!s.title.is_empty(), "空态必须有标题（说明这是什么）");
            assert!(
                !s.desc.is_empty(),
                "空态必须有一句解释（为什么空 / 下一步能干什么）"
            );
        }
    }

    #[test]
    fn 空态清单不是空的() {
        // 清单空了就等于没人管空态了 ⇒ 断言它至少有几条
        assert!(
            all_empty_states().len() >= 3,
            "空态清单太短：{}",
            all_empty_states().len()
        );
    }
}
