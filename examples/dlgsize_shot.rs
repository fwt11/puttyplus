//! 对话框版式截图（阶段 73b，审计 P1-11）。
//!
//! 把「新建连接」「新建数据库连接」两个对话框各截一张，供人眼核对：
//! - 新建连接：高度被限住了（≤520），内容能滚，动作区只剩「取消」「连接」，
//   旁边一个「保存到连接列表」勾选；
//! - 新建数据库连接：宽度被限在 560，说明文字折行了，「记住密码」只出现一次。
//!
//! ```text
//! xvfb-run -a cargo run --example dlgsize_shot
//! ```

use gtk::prelude::*;
use puttyplus::connmgr;
use puttyplus::panel::dbconn;

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
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let ini = std::env::temp_dir().join(format!("pp_dlgsize_shot_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_CONN_INI", &ini);
        std::env::set_var("PUTTYPLUS_DB_INI", &ini);
    }

    connmgr::ui_new_connection(None, |_| {});
    pump(800);
    shot("/tmp/pp_newconn.png");

    dbconn::ui_new(None, "10.0.0.1:22:root", |_, _| {});
    pump(800);
    shot("/tmp/pp_dbconn_dlg.png");

    let _ = std::fs::remove_file(&ini);
}
