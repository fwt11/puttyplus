//! 快捷键一览（cheatsheet，4.4）。
//!
//! 直接渲染 `shortcuts::ALL`：表改了这里自动跟着变，不存在"文档里的
//! 快捷键和实际对不上"这种经典问题。

use std::cell::RefCell;

use gtk::prelude::*;

use crate::shortcuts;
use crate::i18n::{tr, tr_dyn};

pub fn set_parent(w: gtk::Window) {
    // ⚠️ 父窗口统一由 `windows` 管（阶段 67）
    crate::windows::set_parent(w);
}

/// 打开快捷键一览窗口。
pub fn open() {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        tr("快捷键一览"),
        None,
        &[(tr("关闭"), gtk::ResponseType::Close)],
    );
    dlg.set_default_size(540, 640);
    dlg.connect_response(|dlg, _| dlg.close());

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let mut first = true;
    for (group, list) in shortcuts::grouped() {
        if !first {
            let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
            sep.add_css_class("pp-menu-sep");
            root.append(&sep);
        }
        first = false;
        // ⚠️ 分组名在 const 表里 ⇒ 显示时翻译。
        let gname = tr_dyn(group);
        let head = gtk::Label::new(Some(&gname));
        head.set_xalign(0.0);
        head.set_margin_top(4);
        head.add_css_class("pp-section");
        root.append(&head);

        for b in list {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.set_margin_start(8);
            // 加速键做成等宽的一列，描述对齐，读起来像张表
            let key = gtk::Label::new(Some(b.display));
            key.set_xalign(0.0);
            key.set_width_chars(16);
            key.add_css_class("monospace");
            key.add_css_class("pp-key");
            // ⚠️ 说明在 const 表里 ⇒ 显示时翻译。
            let dtext = tr_dyn(b.desc);
            let desc = gtk::Label::new(Some(&dtext));
            desc.set_xalign(0.0);
            desc.set_hexpand(true);
            desc.set_wrap(true);
            desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&key);
            row.append(&desc);
            root.append(&row);
        }
    }

    // 说明为什么 Alt+Shift 而不是 Ctrl+Shift：用户第一个疑问一定是这个
    let tip = gtk::Label::new(Some(
        tr("Ctrl+Shift 系列基本被终端占用（复制/粘贴/新标签），所以广播开关用了\n\
         Alt+Shift+B；单按 Alt 会被终端当成 ESC 前缀，也不敢占。"),
    ));
    tip.add_css_class("pp-hint");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    root.append(&tip);

    let sw = gtk::ScrolledWindow::new();
    // 不设最小高度时，ScrolledWindow 的自然高度是 0：对话框只剩标题栏与动作区。
    sw.set_min_content_height(420);
    sw.set_vexpand(true);
    sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    sw.set_child(Some(&root));
    dlg.content_area().append(&sw);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
}
