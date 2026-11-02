//! 「执行历史」窗口关不掉（用户 2026-11-02 报的）—— 复现 + 定位。
//!
//! 用法：xvfb-run -a cargo run --example hist_close_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel};

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

/// 在控件树里找文字等于 `want` 的按钮。
fn find_button(w: &gtk::Widget, want: &str) -> Option<gtk::Button> {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        let mut found = None;
        let mut c = b.first_child();
        while let Some(x) = c {
            if let Some(l) = x.downcast_ref::<gtk::Label>() {
                if l.text() == want {
                    found = Some(b.clone());
                    break;
                }
            }
            c = x.next_sibling();
        }
        if found.is_some() {
            return found;
        }
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        if let Some(b) = find_button(&x, want) {
            return Some(b);
        }
        c = x.next_sibling();
    }
    None
}

/// 控件树里第一个 Label 的文字（自绘按钮：文字在按钮里的 Box 深处）。
fn first_label(w: &gtk::Widget) -> String {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return l.text().to_string();
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        let t = first_label(&x);
        if !t.is_empty() {
            return t;
        }
        c = x.next_sibling();
    }
    String::new()
}

/// 在工作台页面里找首页那颗动作按钮（按文字）。
fn find_home_action(page: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if page.css_classes().iter().any(|x| x == "pp-empty-btn") {
        if first_label(page) == label {
            return page.downcast_ref::<gtk::Button>().cloned();
        }
    }
    let mut c = page.first_child();
    while let Some(x) = c {
        if let Some(b) = find_home_action(&x, label) {
            return Some(b);
        }
        c = x.next_sibling();
    }
    None
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpHistClose")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        let db_ini = std::env::temp_dir().join(format!("pp-hist-close-{}.ini", std::process::id()));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

        let panel = SftpPanel::new(&ConnOptions::default());
        panel::on_open_db(&panel.st);
        pump(400);

        // 存一条连接配置 + 切到「已连接」状态（走**首页那颗「执行历史」**那条路）
        let opts = ConnOptions::default();
        let key = puttyplus::backend::dbstore::session_key(&opts);
        puttyplus::backend::dbstore::save(
            &key,
            &puttyplus::backend::dbstore::DbProfile {
                name: "本地 PG".into(),
                proto: puttyplus::backend::db::DbProto::Postgres,
                host: "127.0.0.1".into(),
                port: 5432,
                user: "postgres".into(),
                dbname: "postgres".into(),
                remember: false,
            },
        );
        let ok = puttyplus::panel::dbui::empty_state_set_connected(&panel.st, "本地 PG");
        println!("切到已连接：{ok}，会话键 {:?}", puttyplus::panel::dbui::session_key_now(&panel.st));
        pump(200);
        println!("空态文字：{:?}", puttyplus::panel::dbui::empty_state_text(&panel.st));
        // 首页那颗「执行历史」按钮
        let page = {
            let slot = panel.st.db_ui.borrow();
            slot.as_ref().map(|u| u.page.clone()).unwrap()
        };
        let mut clicked = false;
        let mut stack: Vec<gtk::Widget> = vec![page.clone()];
        while let Some(w) = stack.pop() {
            let mut c = w.first_child();
            while let Some(x) = c {
                stack.push(x.clone());
                c = x.next_sibling();
            }
            if w.css_classes().iter().any(|x| x == "pp-empty-btn") {
                if first_label(&w) == "执行历史" {
                    if let Some(b) = w.downcast_ref::<gtk::Button>() {
                        b.emit_clicked();
                        clicked = true;
                    }
                }
            }
        }
        pump(500);
        println!("首页那颗「执行历史」点了：{clicked}");
        pump(400);
        let win = puttyplus::panel::dbui::history_window(&panel.st).expect("历史窗口没建起来");
        println!("窗口可见（打开后）：{}", win.is_visible());
        println!("窗口 realized（打开后）：{}", win.is_realized());
        win.connect_close_request(move |_| {
            eprintln!("[dbg] close-request 信号来了");
            gtk::glib::Propagation::Proceed
        });
        println!("窗口 title：{:?}", win.title());
        println!("是否 modal：{}", win.is_modal());

        // ---- 第一轮：点「关闭」 ----
        let btn = find_button(win.upcast_ref::<gtk::Widget>(), "关闭");
        match &btn {
            Some(b) => {
                println!("找到「关闭」按钮：{}", b.is_sensitive());
                b.emit_clicked();
            }
            None => println!("⚠️ 没找到「关闭」按钮"),
        }
        pump(500);
        println!("窗口可见（第一轮点关闭之后）：{}", win.is_visible());
        println!("窗口 realized（第一轮点关闭之后）：{}", win.is_realized());

        // ---- 模态？开着的时候主窗口不该还能点 ----
        println!("窗口是 modal：{}", win.is_modal());
        assert!(win.is_modal(), "「执行历史」应该是 modal 对话框");

        // ---- 第二轮：**再打开一次**（点首页那颗「执行历史」），然后再关 ----
        println!("---- 第二轮：再打开一次 ----");
        let home_btn = find_home_action(&page, "执行历史");
        println!("首页那颗按钮还在吗：{}", home_btn.is_some());
        if let Some(b) = &home_btn {
            b.emit_clicked();
        }
        pump(600);
        println!("窗口可见（第二轮打开之后）：{}", win.is_visible());
        println!("窗口 realized（第二轮打开之后）：{}", win.is_realized());
        let btn2 = find_button(win.upcast_ref::<gtk::Widget>(), "关闭");
        if let Some(b) = &btn2 {
            println!("第二轮「关闭」按钮 sensitive：{}", b.is_sensitive());
            b.emit_clicked();
        }
        pump(600);
        println!("窗口可见（第二轮点关闭之后）：{}", win.is_visible());
        assert!(
            !win.is_visible(),
            "第二次关不掉「执行历史」窗口（GTK 4.18 的 close() 只有第一次有效）"
        );
        // 再试窗口自带的 X（close-request）
        win.close();
        pump(400);
        println!("窗口可见（第二轮 close() 之后）：{}", win.is_visible());
        println!("窗口 mapped（第二轮 close() 之后）：{}", win.is_mapped());

        // 换一种关法：直接 set_visible(false)
        win.set_visible(false);
        pump(300);
        println!("窗口可见（set_visible(false) 之后）：{}", win.is_visible());

        // 第三轮：再打开，再 set_visible(false)
        if let Some(b) = &home_btn {
            b.emit_clicked();
        }
        pump(400);
        println!("窗口可见（第三轮打开之后）：{}", win.is_visible());
        win.set_visible(false);
        pump(300);
        println!("窗口可见（第三轮 set_visible(false) 之后）：{}", win.is_visible());
        // 第四轮：再打开，再 close()
        if let Some(b) = &home_btn {
            b.emit_clicked();
        }
        pump(400);
        println!("窗口可见（第四轮打开之后）：{}", win.is_visible());
        win.close();
        pump(300);
        println!("窗口可见（第四轮 close() 之后）：{}", win.is_visible());

        let _ = std::fs::remove_file(&db_ini);
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
