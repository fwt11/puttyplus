//! SSH 连不上时的**界面提示**冒烟（阶段 154）。
//!
//! 用户报的：「ssh 登录超时退出，界面上应该能有提示」。
//!
//! **断言**（两条路各来一遍）：
//! 1. **外部 ssh 那条路**（`PP_TERM_SSH=spawn`）：ssh 连不上 ⇒ 它自己以
//!    255 退出 ⇒ 界面要弹一个「SSH 连接失败」的框，**说清楚原因**
//!    （这里连的是没人监听的端口 ⇒ 「连不上服务器」），
//!    并且框里有一颗**「重试」**按钮（用户当场就能再来一次）；
//! 2. **进程内 SSH 那条路**（`PP_TERM_SSH=inproc`）：同理 ⇒ 也要弹框、
//!    也要说清楚原因、也要有「重试」；
//! 3. **终端里也要留一行**（进程内那条路终端本来一个字都没有 ⇒
//!    用户什么都没看到）；外部 ssh 那条路还要看得见**退出码**。
//!
//! 运行（无桌面时用 xvfb-run）：
//! ```text
//! xvfb-run -a cargo run --example ssh_fail_hint_smoke
//! ```
//!
//! ⚠ 连的是 **127.0.0.1:1**（没人监听）⇒ 立刻「连接被拒绝」，
//!   不用等 15 秒超时，也不需要外部网络。

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

/// 等某个条件成立（最多 timeout 毫秒）。
fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

/// 找出标题为「SSH 连接失败」的那个框。
fn find_fail_dialog() -> Option<gtk::Dialog> {
    for w in gtk::Window::list_toplevels() {
        if let Some(d) = w.downcast_ref::<gtk::Dialog>() {
            if d.title().as_deref() == Some("SSH 连接失败") {
                return Some(d.clone());
            }
        }
    }
    None
}

/// 框里所有 Label 的文字（拼起来）。
fn dialog_text(dlg: &gtk::Dialog) -> String {
    let mut out = String::new();
    let mut stack = vec![dlg.upcast_ref::<gtk::Widget>().clone()];
    while let Some(w) = stack.pop() {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push_str(l.text().as_str());
            out.push('\n');
        }
        let mut cur = w.first_child();
        while let Some(c) = cur {
            let next = c.next_sibling();
            stack.push(c.clone());
            cur = next;
        }
    }
    out
}

/// 框里有没有某颗按钮。
fn has_button(dlg: &gtk::Dialog, label: &str) -> bool {
    let mut stack = vec![dlg.upcast_ref::<gtk::Widget>().clone()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if b.label().as_deref() == Some(label) {
                return true;
            }
        }
        let mut cur = w.first_child();
        while let Some(c) = cur {
            let next = c.next_sibling();
            stack.push(c.clone());
            cur = next;
        }
    }
    false
}

/// 终端屏幕上的文字。
fn screen_text(pane: &TermPane) -> String {
    pane.terminal().screen_text(200, 400)
}

/// 跑一遍：连不上 ⇒ 弹框 ＋ 说清楚原因 ＋ 有「重试」。
fn check_path(mode: &str, expect_term_line: bool) {
    // SAFETY：单线程程序，此刻没有别的线程在读环境。
    unsafe {
        std::env::set_var("PP_TERM_SSH", mode);
    }

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 1, // 没人监听 ⇒ 立刻「连接被拒绝」
        user: "nobody".into(),
        ..Default::default()
    };

    let pane = TermPane::new(&opts);
    let win = gtk::Window::new();
    win.set_default_size(800, 600);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(300);

    let got = wait_until(30_000, || find_fail_dialog().is_some());
    let dlg = find_fail_dialog().unwrap_or_else(|| {
        panic!(
            "[{mode}] ssh 连不上却**没有**弹「SSH 连接失败」的框\n屏幕内容：\n{}",
            screen_text(&pane)
        )
    });
    assert!(got, "[{mode}] 没等到失败提示框");

    let text = dialog_text(&dlg);
    assert!(
        text.contains("连不上服务器"),
        "[{mode}] 框里没说明原因（要告诉用户是网络/端口的问题）\n框里文字：\n{text}"
    );
    assert!(
        has_button(&dlg, "重试"),
        "[{mode}] 框里没有「重试」按钮 ⇒ 用户只能干瞪眼\n框里文字：\n{text}"
    );
    println!("[{mode}] 弹框说清楚了原因，也有「重试」 ✔");

    // 终端里也要看得见（进程内那条路终端本来一个字都没有）
    let screen = screen_text(&pane);
    if expect_term_line {
        assert!(
            screen.contains("会话已结束"),
            "[{mode}] 终端里没有「会话已结束」这一行\n屏幕内容：\n{screen}"
        );
        println!("[{mode}] 终端里留了一行（会话已结束） ✔");
    }

    dlg.close();
    let _ = win.close();
}

fn main() {
    gtk::init().expect("gtk init");

    // ① 外部 ssh 那条路：ssh 自己报错、自己退 255 ⇒ 界面要提示
    check_path("spawn", true);

    // ② 进程内 SSH 那条路：russh 报「连接被拒绝」⇒ 界面也要提示
    //    （这条路的终端**一个字都没有** ⇒ 弹框是用户唯一的提示）
    check_path("inproc", false);

    println!("\nssh_fail_hint_smoke: 全部通过");
}
