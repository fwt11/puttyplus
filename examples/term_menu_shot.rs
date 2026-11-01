//! 终端右键菜单 + 说明气泡截图（阶段 94）。
//!
//! 用途：右键菜单里的**说明（tooltip）**是看得见的东西——单测只能验文字，
//! 验不了「弹在哪、排版挤不挤」。这个例子把**真菜单**弹出来，再用真指针
//! 悬停到某一项上截下气泡。
//!
//! ⚠️ **必须用真指针事件**：`emit_clicked()` / 直接 `popup()` 弹不出菜单
//! （GTK4 的 Popover 只认真指针）⇒ 用 `xdotool` 发真的右键与移动。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example term_menu_shot
//!
//! 截图：`/tmp/pp_term_menu.png`、`/tmp/pp_term_tip.png`

use gtk::prelude::*;

use puttyplus::args::ConnOptions;
use puttyplus::term::TermPane;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let step = 20u64;
    for _ in 0..(ms / step) {
        while ctx.iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(step));
    }
}

/// 在控件树里按**文字**找按钮（菜单项的按钮里没有 label 属性，文字在
/// 后代的 Label 里）。
fn find_button_by_text(root: &gtk::Widget, want: &str) -> Option<gtk::Button> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.clone().downcast::<gtk::Button>().ok() {
            let mut hit = false;
            let mut inner = vec![b.clone().upcast::<gtk::Widget>()];
            while let Some(x) = inner.pop() {
                if let Some(l) = x.clone().downcast::<gtk::Label>().ok() {
                    if l.text() == want {
                        hit = true;
                    }
                }
                let mut c = x.first_child();
                while let Some(ch) = c {
                    inner.push(ch.clone());
                    c = ch.next_sibling();
                }
            }
            if hit {
                return Some(b);
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    None
}

fn xdo(args: &[&str]) {
    let _ = std::process::Command::new("xdotool").args(args).status();
}

fn main() {
    gtk::init().expect("gtk init");

    // 连不上的主机没关系：要的是菜单，不是会话
    let mut o = ConnOptions::default();
    o.host = "127.0.0.1".into();
    o.port = 1;
    o.user = "nobody".into();
    o.title = Some("示例终端".into());

    let pane = TermPane::new(&o);
    // 右键菜单的扩展区（分屏 / 广播 / 日志 / 录制）由 tabs 注册；
    // 这里注册一个假的 page —— 只是为了让菜单长全，不触发分屏。
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    puttyplus::term::attach_split_menu(&pane, page.upcast_ref::<gtk::Widget>());

    let win = gtk::Window::new();
    win.set_title(Some("终端右键菜单 — 阶段 94"));
    win.set_default_size(760, 520);
    win.set_child(Some(pane.widget()));
    win.present();
    pump(900);

    // ---- 真右键 ⇒ 菜单弹出来（扩展区也在里面）----
    xdo(&["mousemove", "380", "260", "click", "3"]);
    pump(1200);

    let root = win.clone().upcast::<gtk::Widget>();
    let item = find_button_by_text(&root, "录制会话到 .cast");
    let Some(item) = item else {
        println!("没找到「录制会话到 .cast」那一项");
        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg("/tmp/pp_term_menu.png")
            .status();
        return;
    };
    println!("找到了录制项；tooltip = {:?}", item.tooltip_text());

    // 菜单本体截一张
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_term_menu.png")
        .status();
    println!("截图：/tmp/pp_term_menu.png");

    // ---- 悬停 ⇒ 说明气泡 ----
    let root_win = win.clone();
    if let Some((x, y)) = item.translate_coordinates(&root_win, 0.0, 0.0) {
        println!("录制项在窗口里的位置：{x},{y}");
        xdo(&[
            "mousemove",
            &format!("{}", x as i32 + 30),
            &format!("{}", y as i32 + 8),
        ]);
        // 等 tooltip 自己冒出来（GTK 默认 500ms 延时）
        pump(2500);
        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg("/tmp/pp_term_tip.png")
            .status();
        println!("截图：/tmp/pp_term_tip.png");
    } else {
        println!("translate_coordinates 拿不到位置");
    }

    println!("term_menu_shot: OK");
}
