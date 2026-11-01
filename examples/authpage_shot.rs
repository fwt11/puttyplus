//! 连接编辑器「认证」页的实机截图（阶段 70）。
//!
//! ```text
//! xvfb-run -a cargo run --example authpage_shot
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
    // ⚠️ HOME 必须在任何 glib 调用之前改
    let home = std::env::temp_dir().join(format!("pp_authpage_shot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(
        home.join(".ssh/id_ed25519"),
        "-----BEGIN OPENSSH PRIVATE KEY-----\n",
    )
    .unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    }
    let ini = home.join("connections.ini");
    unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &ini) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    puttyplus::connmgr::ui_new_connection(None, |_| {});
    pump(600);

    // 切到「认证」页
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
            println!("已切到「认证」页");
        }
    }
    pump(300);

    gtk::glib::MainLoop::new(None, false).run();
}
