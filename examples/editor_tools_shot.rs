//! 文件编辑器工具栏截图（阶段 84）。
//!
//! 截两张供人眼核对：
//! - 编辑器常态：一行工具栏（撤销/重做/复制/粘贴/查找/替换/跳转到行/
//!   自动换行/字号/保存/重载）+ 行号 + 正文；
//! - 打开「替换」之后：查找行 + 替换行。
//!
//! ```text
//! xvfb-run -a cargo run --example editor_tools_shot
//! ```

#![allow(deprecated)]

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, fileedit, SftpPanel};
use puttyplus::theme::{self, Theme};

const SAMPLE: &str = "\
# 配置文件示例
listen = 0.0.0.0
port = 22

[认证]
method = publickey
user = deploy

# 中文注释：这一行里也有 auth 字样
# auth = required
";

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("截图：{path}");
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Dark);

    let dir = std::env::temp_dir().join(format!("pp_editor_shot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("config.toml");
    std::fs::write(&file, SAMPLE).expect("sample");
    let path = file.to_string_lossy().to_string();

    let panel = SftpPanel::new(&ConnOptions::default());
    let path = file.to_string_lossy().to_string();

    // ---- 子标签模式（用户实际看到的那一屏）：图标在左、文件名在右 ----
    //
    // 复刻 `tabs.rs` 的接线（阶段 97）：连接页 = 子标签区；文件面板在
    // **终端子页内部**（左半边）。
    {
        let sub = puttyplus::panel::subtabs::SubTabs::new();
        *panel.st.sub_tabs.borrow_mut() = Some(sub.clone());
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        term_page.set_start_child(Some(panel.widget()));
        let term = gtk::Label::new(Some("终端（shot 占位）"));
        term_page.set_end_child(Some(term.upcast_ref::<gtk::Widget>()));
        term_page.set_position(340);
        sub.add_page("终端", term_page.upcast::<gtk::Widget>(), None);
        let win = gtk::Window::new();
        win.set_title(Some("editor tools shot"));
        win.set_default_size(1280, 760);
        win.set_child(Some(sub.root()));
        win.present();
        pump(400);
        panel::fileedit::open_local(&panel.st, &path, false);
        pump(800);
        shot("/tmp/pp_editor_subtab.png");
    }

    // ---- 独立窗口模式：标题在窗口标题栏，工具栏只有按钮 ----
    {
        *panel.st.sub_tabs.borrow_mut() = None;
        panel::fileedit::open_local(&panel.st, &path, false);
        pump(800);
    }

    // 常态：工具栏
    shot("/tmp/pp_editor_toolbar.png");

    // 打开替换模式
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        let btns = fileedit::toolbar_buttons(e);
        let replace = btns.iter().find(|(n, _)| *n == "替换").expect("替换按钮");
        replace.1.emit_clicked();
        pump(200);
        fileedit::set_find_query(e, "auth");
        fileedit::set_replace_text(e, "认证方式");
        pump(400);
    }
    shot("/tmp/pp_editor_findbar.png");

    panel::shutdown(&panel.st);
    let _ = std::fs::remove_dir_all(&dir);
}
