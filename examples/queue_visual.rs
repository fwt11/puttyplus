//! 传输队列窗口视觉验证：与 panel::queue::ensure 相同的列构建 + 假数据，
//! 1.2 秒后截图到 /tmp/queue_shot.png 并退出。
//! 运行：cargo run --example queue_visual

use gtk::glib;
use gtk::prelude::*;

const QCOL_DIR: i32 = 0;
const QCOL_FILE: i32 = 1;
const QCOL_PCT: i32 = 2;
const QCOL_SIZE: i32 = 3;
const QCOL_SPEED: i32 = 4;
const QCOL_STATE: i32 = 5;
const QCOL_ID: i32 = 6;

fn main() {
    let app = gtk::Application::builder()
        .application_id("org.puttyplus.QueueVisualTest")
        .build();
    app.connect_activate(|app| {
        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .title("传输队列 — PuTTYPlus")
            .default_width(720)
            .default_height(260)
            .build();

        let model = gtk::ListStore::new(&[
            glib::Type::STRING,
            glib::Type::STRING,
            glib::Type::I32,
            glib::Type::STRING,
            glib::Type::STRING,
            glib::Type::STRING,
            glib::Type::U64,
        ]);
        let view = gtk::TreeView::with_model(&model);

        // —— 与 panel/queue.rs ensure() 保持一致的列构建（已修复版）——
        let cols: [(&str, bool, bool); 5] = [
            ("方向", false, false),
            ("文件", true, false),
            ("进度", false, true),
            ("大小", false, false),
            ("速度", false, false),
        ];
        for (i, (title, expand, progress)) in cols.iter().enumerate() {
            let col = gtk::TreeViewColumn::new();
            col.set_title(title);
            col.set_resizable(true);
            if *expand {
                col.set_expand(true);
                col.set_min_width(120);
            }
            if i == 0 {
                col.set_min_width(84);
            }
            let r: gtk::CellRenderer = if *progress {
                gtk::CellRendererProgress::new().upcast()
            } else {
                gtk::CellRendererText::new().upcast()
            };
            col.pack_start(&r, true);
            if *progress {
                col.add_attribute(&r, "value", QCOL_PCT);
            } else {
                col.add_attribute(&r, "text", i as i32);
            }
            view.append_column(&col);
        }
        let col_state = gtk::TreeViewColumn::new();
        col_state.set_title("状态");
        let rs = gtk::CellRendererText::new();
        col_state.pack_start(&rs, true);
        col_state.add_attribute(&rs, "text", QCOL_STATE);
        view.append_column(&col_state);

        // 假数据：一行传输中、一行已完成
        {
            let it = model.append();
            let (dir, file, pct): (&str, &str, i32) = ("⬆ 上传", "/home/user/大文件.iso → /data", 37);
            let (sz, sp, st, id): (&str, &str, &str, u64) =
                ("1.2 GB / 3.1 GB", "18.4 MB", "传输中", 1);
            model.set(
                &it,
                &[
                    (QCOL_DIR as u32, &dir),
                    (QCOL_FILE as u32, &file),
                    (QCOL_PCT as u32, &pct),
                    (QCOL_SIZE as u32, &sz),
                    (QCOL_SPEED as u32, &sp),
                    (QCOL_STATE as u32, &st),
                    (QCOL_ID as u32, &id),
                ],
            );
        }
        {
            let it = model.append();
            let (dir, file, pct): (&str, &str, i32) = ("⬇ 下载", "/remote/配置.tar.gz → ~/Downloads", 100);
            let (sz, sp, st, id): (&str, &str, &str, u64) = ("45 KB / 45 KB", "-", "完成", 2);
            model.set(
                &it,
                &[
                    (QCOL_DIR as u32, &dir),
                    (QCOL_FILE as u32, &file),
                    (QCOL_PCT as u32, &pct),
                    (QCOL_SIZE as u32, &sz),
                    (QCOL_SPEED as u32, &sp),
                    (QCOL_STATE as u32, &st),
                    (QCOL_ID as u32, &id),
                ],
            );
        }

        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_child(Some(&view));
        sw.set_hexpand(true);
        sw.set_vexpand(true);
        win.set_child(Some(&sw));
        win.present();

        // —— 右键菜单（与 panel/queue.rs 一致的构建，静态模拟）——
        let actions = gtk::gio::SimpleActionGroup::new();
        let act_cancel = gtk::gio::SimpleAction::new("cancel", None);
        let a_remove = gtk::gio::SimpleAction::new("remove", None);
        let a_clear = gtk::gio::SimpleAction::new("clear", None);
        actions.add_action(&act_cancel);
        actions.add_action(&a_remove);
        actions.add_action(&a_clear);
        view.insert_action_group("queue", Some(&actions));

        let menu = gtk::gio::Menu::new();
        menu.append(Some("取消"), Some("queue.cancel"));
        menu.append(Some("删除"), Some("queue.remove"));
        menu.append_section(None, &gtk::gio::Menu::new());
        menu.append(Some("清除已完成"), Some("queue.clear"));

        // 1.0 秒后选中第一行（传输中→取消可用/删除禁用）并弹出菜单
        {
            let view = view.clone();
            let menu = menu.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
                let path = gtk::TreePath::new_first();
                view.selection().select_path(&path);
                act_cancel.set_enabled(true);
                a_remove.set_enabled(false); // 选中行是“传输中”
                let popover = gtk::PopoverMenu::from_model(Some(&menu));
                popover.set_autohide(false);
                popover.set_has_arrow(false);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(40, 40, 1, 1)));
                popover.set_parent(&view);
                popover.popup();
            });
        }

        // 2.0 秒后截图退出
        glib::timeout_add_local_once(std::time::Duration::from_millis(2000), move || {
            let _ = std::process::Command::new("/usr/bin/scrot")
                .args(["-u", "/tmp/queue_shot.png"]) // -u = 当前聚焦窗口
                .status();
            std::process::exit(0);
        });
    });
    std::process::exit(i32::from(app.run_with_args::<&str>(&[])));
}
