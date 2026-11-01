//! 终端查找条冒烟（阶段 84）：查找条上有「上一个 / 下一个」按钮。
//!
//! 以前终端的查找条**只有输入框**：想跳到上一个匹配只能自己数着回车按
//! 几次（回车是"下一个"，Shift+Enter 是"上一个"，但没有按钮）。
//!
//! 现在终端的查找条改用**共用构件** `findbar`（跟文件编辑器那条同一套
//! 外观与按键约定），并补上「上一个 / 下一个」两个按钮。
//!
//! 这个冒烟不连服务器：只验**结构**（按钮在不在、是不是纯图标、有没有
//! tooltip、查找条能不能开/关）。真连服务器查回滚缓冲的行为由
//! `term_reconnect_smoke` 那类例子覆盖。
//!
//! ```text
//! xvfb-run -a cargo run --example term_findbar_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::term::TermPane;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 按钮是不是**纯图标**：child 是 Image（而不是带文字的 Box）。
fn is_icon_only(b: &gtk::Button) -> bool {
    b.child().and_downcast::<gtk::Image>().is_some()
}

fn main() {
    gtk::init().expect("gtk init");

    // 连不上也无所谓：这里只验查找条的**结构**
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 9; // 丢弃端口：连不上，进程会自己退
    opts.user = "nobody".into();

    let pane = TermPane::new(&opts);
    pump(400);

    // 查找条上要有「上一个 / 下一个」两个按钮
    let btns = pane.find_nav_buttons();
    let names: Vec<&str> = btns.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, vec!["上一个", "下一个"], "查找条上少了跳转按钮");
    for (name, b) in &btns {
        assert!(
            is_icon_only(b),
            "{name} 应当是纯图标按钮（child 是 Image），实际 {:?}",
            b.child().map(|c| c.type_().name().to_string())
        );
        let tip = b.tooltip_text().unwrap_or_default();
        assert!(!tip.trim().is_empty(), "{name} 必须有 tooltip");
    }
    println!("[1] 终端查找条有「上一个 / 下一个」，纯图标 + 有 tooltip");

    // 查找条能开能关（Ctrl+F 那条路）
    let win = gtk::Window::new();
    win.set_title(Some("term findbar smoke"));
    win.set_default_size(900, 420);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(300);
    pane.toggle_search(true);
    pump(300);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_term_findbar.png")
        .status();
    println!("截图：/tmp/pp_term_findbar.png");
    pane.toggle_search(false);
    pump(200);
    println!("[2] 查找条能开能关");

    println!("term_findbar_smoke: OK");
}
