//! 连接编辑器「转发」页的实机截图（阶段 71）。
//!
//! ```text
//! xvfb-run -a cargo run --example fwdpage_shot
//! ```
//!
//! ⚠️ 自己不会退出（要留着窗口给截图脚本），用 `timeout 30` 跑。

use gtk::prelude::*;

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

fn main() {
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ⚠️ 别往用户真实配置里写：ini 指到临时文件，并预先放两条规则
    let ini = std::env::temp_dir().join(format!("pp_fwdpage_shot_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    std::fs::write(
        &ini,
        "[conn1]\nname=堡垒机\nhost=10.0.0.9\nport=2222\nuser=ops\n\
         forwardings=L:9000:10.0.0.1:80,!R:9200:10.0.0.1:22,D:1080\n\
         inprocess_forwardings=L:9100:10.0.0.1:81\n",
    )
    .unwrap();
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &ini) };

    // 打开这条连接 ⇒ 编辑器里带着两条规则
    let entries = puttyplus::connmgr::load_all();
    if let Some(e) = entries.first() {
        let e = e.clone();
        puttyplus::connmgr::ui_edit_connection(None, e, || {});
    }
    pump(700);

    // 切到「转发」页
    for w in gtk::Window::list_toplevels() {
        let Some(win) = w.downcast::<gtk::Window>().ok() else {
            continue;
        };
        if win.title().as_deref() != Some("编辑连接") {
            continue;
        }
        let mut nb: Option<gtk::Notebook> = None;
        let mut stack = vec![win.clone().upcast::<gtk::Widget>()];
        while let Some(cur) = stack.pop() {
            if let Some(n) = cur.downcast_ref::<gtk::Notebook>() {
                nb = Some(n.clone());
                break;
            }
            let mut c = cur.first_child();
            while let Some(ch) = c {
                stack.push(ch.clone());
                c = ch.next_sibling();
            }
        }
        if let Some(nb) = nb {
            nb.set_current_page(Some(2));
            println!("已切到「转发」页");
        }
    }
    pump(300);

    gtk::glib::MainLoop::new(None, false).run();
}
