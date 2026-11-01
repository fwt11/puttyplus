//! 界面视觉自查工具：把单个对话框/控件拉起来，方便截图比对。
//!
//! 用法（需要显示环境；无桌面时用 `xvfb-run -a`）：
//!
//! ```text
//! cargo run --release --example ui_shot -- <mode> [dark|light|contrast]
//! ```
//!
//! mode:
//! - `key`      私钥管理器（含长提示换行、生成区布局）
//! - `info`     结果提示（成功不再用错误弹窗）
//! - `confirm`  危险操作确认（中文按钮）
//! - `listbox`  单选列表的选中态对比度（浅色/深色各看一遍）
//! - `chips`    终端状态条的三个 chip（广播/录制/日志）
//! - `icons`    文件列表的图标列（空模型时列宽塌成 0 的回归检查）
//! - `menu`     自绘右键菜单（图标/快捷键/勾选/危险项）
//! - `menu-native` 原生 `PopoverMenu`（顶栏/⋯ 菜单的卡片样式）
//! - `shortcuts` 快捷键一览（分组标题/等宽键位列）
//! - `probe`    打印当前主题的几个颜色令牌解析结果（不弹窗，纯检查）

use gtk::prelude::*;
use puttyplus::keyui;
use puttyplus::menu;
use puttyplus::panel::dialogs;
use puttyplus::shortcutsui;
use puttyplus::theme::{self, Theme};

fn option_row(title: &str, hint: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(true);
    let h = gtk::Box::new(gtk::Orientation::Vertical, 0);
    h.set_margin_top(6);
    h.set_margin_bottom(6);
    h.set_margin_start(6);
    h.set_margin_end(6);
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    h.append(&t);
    let d = gtk::Label::new(Some(hint));
    d.add_css_class("dim-label");
    d.set_xalign(0.0);
    h.append(&d);
    row.set_child(Some(&h));
    row
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "key".into());
    let t = match args.next().as_deref() {
        Some("light") => Theme::Light,
        Some("contrast") => Theme::HighContrast,
        _ => Theme::Dark,
    };
    gtk::init().expect("gtk init");
    theme::apply(t);

    match mode.as_str() {
        "key" => keyui::open(None),
        "info" => dialogs::info(
            None,
            "已生成",
            "私钥：/home/user/.ssh/id_rsa\n公钥：/home/user/.ssh/id_rsa.pub\n\n把公钥内容追加到目标机的 authorized_keys 即可用。",
        ),
        "confirm" => dialogs::confirm_with(
            None,
            "删除连接「生产跳板」？\n\n此操作不可撤销；该条目在 connections.ini 里的配置（含端口转发规则）会一并删除。",
            "删除",
            |_| {},
        ),
        "listbox" => {
            let win = gtk::Window::new();
            win.set_title(Some("选中态检查"));
            win.set_default_size(420, 260);
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            for (t, h) in [
                ("跟随系统", "不覆盖系统 GTK 主题"),
                ("深色", "暗色底，夜间/暗光环境"),
                ("浅色", "亮色底，白天/投影"),
            ] {
                list.append(&option_row(t, h));
            }
            win.set_child(Some(&list));
            win.present();
            if let Some(r) = list.row_at_index(1) {
                list.select_row(Some(&r));
            }
        }
        "icons" => {
            let win = gtk::Window::new();
            win.set_default_size(360, 220);
            let model = gtk::ListStore::new(&[gtk::glib::Type::STRING, gtk::glib::Type::STRING]);
            for (icon, name) in [
                ("folder-symbolic", "目录"),
                ("text-x-generic-symbolic", "文件.txt"),
            ] {
                let it = model.append();
                model.set(&it, &[(0u32, &icon), (1u32, &name)]);
            }
            let view = gtk::TreeView::with_model(&model);
            let c = gtk::TreeViewColumn::new();
            let p = gtk::CellRendererPixbuf::new();
            c.pack_start(&p, false);
            c.add_attribute(&p, "icon-name", 0);
            view.append_column(&c);
            let c2 = gtk::TreeViewColumn::new();
            c2.set_title("名称");
            let t = gtk::CellRendererText::new();
            c2.pack_start(&t, true);
            c2.add_attribute(&t, "text", 1);
            view.append_column(&c2);
            win.set_child(Some(&view));
            win.present();
        }
        "shortcuts" => shortcutsui::open(),
        "menu" => {
            let win = gtk::Window::new();
            win.set_default_size(420, 300);
            let btn = gtk::Button::with_label("菜单锚点");
            btn.set_margin_top(20);
            btn.set_margin_start(20);
            btn.set_halign(gtk::Align::Start);
            win.set_child(Some(&btn));
            win.present();

            let pop = menu::popover(&btn);
            let vb = menu::body();
            vb.append(&menu::section("示例分组"));
            vb.append(&menu::item("edit-copy-symbolic", "复制", Some("Ctrl+Shift+C")));
            vb.append(&menu::item("edit-paste-symbolic", "粘贴", Some("Ctrl+Shift+V")));
            vb.append(&menu::sep());
            vb.append(&menu::toggle(
                "network-transmit-receive-symbolic",
                "按键广播到其它终端",
                Some("Alt+Shift+B"),
                true,
            ));
            vb.append(&menu::toggle(
                "document-save-symbolic",
                "记录终端到日志文件",
                None,
                false,
            ));
            vb.append(&menu::sep());
            let del = menu::item("window-close-symbolic", "关闭当前终端", None);
            menu::danger(&del);
            vb.append(&del);
            pop.set_child(Some(&vb));
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                pop.popup();
            });
        }
        "menu-native" => {
            let win = gtk::Window::new();
            win.set_default_size(420, 300);
            let btn = gtk::Button::with_label("原生菜单锚点");
            btn.set_margin_top(20);
            btn.set_margin_start(20);
            btn.set_halign(gtk::Align::Start);
            win.set_child(Some(&btn));
            win.present();

            let actions = gtk::gio::SimpleActionGroup::new();
            for name in ["new", "other", "quit"] {
                actions.add_action(&gtk::gio::SimpleAction::new(name, None));
            }
            btn.insert_action_group("shot", Some(&actions));
            let m = gtk::gio::Menu::new();
            m.append(Some("新建连接"), Some("shot.new"));
            m.append(Some("管理连接…"), Some("shot.other"));
            m.append_section(None, &gtk::gio::Menu::new());
            m.append(Some("退出"), Some("shot.quit"));
            let pop = gtk::PopoverMenu::from_model(Some(&m));
            pop.add_css_class("pp-menu");
            pop.set_parent(&btn);
            pop.set_has_arrow(false);
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                pop.popup();
            });
        }
        "chips" => {
            let win = gtk::Window::new();
            win.set_default_size(560, 120);
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
            bx.set_margin_top(14);
            bx.set_margin_bottom(14);
            bx.set_margin_start(14);
            bx.set_margin_end(14);

            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let bc = gtk::Button::new();
            bc.add_css_class("pp-chip");
            bc.add_css_class("pp-chip-warn");
            bc.set_label("广播中 · 按键同时发往其它 2 个终端（点击停用）");
            let log = gtk::Label::new(Some("记录中 → ~/.local/share/puttyplus/logs/fwt.log"));
            log.add_css_class("pp-chip");
            let rec = gtk::Button::new();
            rec.add_css_class("pp-chip");
            rec.add_css_class("pp-chip-err");
            rec.set_label("● 录制中 → fwt.cast（点此停止）");
            row.append(&bc);
            row.append(&rec);
            bx.append(&row);
            bx.append(&log);

            let sec = gtk::Label::new(Some("界面主题"));
            sec.add_css_class("pp-section");
            sec.set_xalign(0.0);
            bx.append(&sec);
            win.set_child(Some(&bx));
            win.present();
        }
        "paned" => {
            // 查 GTK4 的 Paned 两个行为（阶段 37 修分屏要用）：
            // ① 只有一个子控件时，它是不是占满整个区域？
            // ② position=0 且有两个子控件时，空间怎么分？
            let win = gtk::Window::new();
            win.set_default_size(800, 240);
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
            bx.set_margin_top(10);
            bx.set_margin_bottom(10);
            bx.set_margin_start(10);
            bx.set_margin_end(10);

            // ① 单子控件
            let p1 = gtk::Paned::new(gtk::Orientation::Horizontal);
            let b1 = gtk::Button::with_label("只有一个子控件");
            b1.set_hexpand(true);
            b1.set_vexpand(true);
            p1.set_start_child(Some(&b1));
            p1.set_vexpand(true);
            bx.append(&p1);

            // ② position=0 的双子控件
            let p2 = gtk::Paned::new(gtk::Orientation::Horizontal);
            let b2 = gtk::Button::with_label("左");
            let b3 = gtk::Button::with_label("右");
            b2.set_hexpand(true);
            b3.set_hexpand(true);
            p2.set_start_child(Some(&b2));
            p2.set_end_child(Some(&b3));
            p2.set_vexpand(true);
            bx.append(&p2);

            win.set_child(Some(&bx));
            win.present();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let a1 = p1.allocation();
                let a2 = p2.allocation();
                let s1 = p2.start_child().map(|c| c.allocation().width()).unwrap_or(0);
                let s2 = p2.end_child().map(|c| c.allocation().width()).unwrap_or(0);
                eprintln!("单子控件 Paned 宽={} 高={}", a1.width(), a1.height());
                eprintln!(
                    "position=0 的 Paned：总宽={} 左={} 右={}",
                    a2.width(),
                    s1,
                    s2
                );
            });
        }
        "menubar" => {
            // 量一量顶栏菜单栏到底多宽（用户 2026-10-28："主菜单宽度有些太大"）。
            //
            // **为什么要量**：菜单栏的宽度 = 标题的宽 + 各自的 padding。
            // padding 差 2px，5 项就是 10px；字号差 1px，10 个汉字就是 10px——
            // 都是"看不见但占地方"的部分。光凭感觉调容易过头（挤成一团）。
            //
            // ⚠️ **标题要从 `topmenus::top_menu_titles()` 取，别手抄**：
            // 手抄的那份会随菜单增删而漂移（阶段 41 菜单从 7 个减到 5 个时，
            // 抄本还是 7 项，量出来的数就不是当前菜单栏的数了）。
            let bar = menu::MenuBar::new();
            let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bx.add_css_class("pp-menubar");
            let mut btns = Vec::new();
            for t in puttyplus::topmenus::top_menu_titles() {
                let (b, _p) = bar.item(t);
                btns.push((t.to_string(), b.clone()));
                bx.append(&b);
            }
            let win = gtk::Window::new();
            win.set_default_size(1200, 160);
            win.set_child(Some(&bx));
            win.present();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                let mut total = 0;
                for (t, b) in &btns {
                    let a = b.allocation();
                    eprintln!("{t}：宽 {} 高 {}", a.width(), a.height());
                    total += a.width();
                }
                eprintln!("菜单栏总宽 = {total}");
            });
        }
        "probe" => {
            eprintln!("display_default = {}", gtk::gdk::Display::default().is_some());
            eprintln!("gtk_initialized = {}", gtk::is_initialized_main_thread());
            let win = gtk::Window::new();
            win.present();
            let sc = win.style_context();
            for name in ["pp_bg", "pp_fg", "pp_accent_soft"] {
                let v = sc.lookup_color(name).map(|c| {
                    format!("({:.3},{:.3},{:.3})", c.red(), c.green(), c.blue())
                });
                eprintln!("PROBE {name} => {v:?}");
            }
        }
        other => eprintln!("未知模式：{other}"),
    }

    gtk::glib::MainLoop::new(None, false).run();
}
