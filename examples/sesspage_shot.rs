//! 连接编辑器「会话」页的实机截图（阶段 69 之二）。
//!
//! ```text
//! xvfb-run -a cargo run --example sesspage_shot
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

    // ⚠️ 别往用户真实配置里写：ini 指到临时文件
    let ini = std::env::temp_dir().join(format!("pp_sesspage_shot_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &ini) };

    puttyplus::connmgr::ui_new_connection(None, |_| {});
    pump(600);

    // 切到「会话」页
    for w in gtk::Window::list_toplevels() {
        let Some(win) = w.downcast::<gtk::Window>().ok() else {
            continue;
        };
        if win.title().as_deref() != Some("新建连接") {
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
            nb.set_current_page(Some(1));
            println!("已切到「会话」页");
        }
    }
    pump(300);

    gtk::glib::MainLoop::new(None, false).run();
}
