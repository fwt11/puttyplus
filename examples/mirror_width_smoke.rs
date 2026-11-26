//! 阶段 174 / UI-12 冒烟：**目录镜像的表头在默认宽度下要完整**。
//!
//! ⚠️ **背景**（UI 审核 P2-5，高置信）：目录镜像默认 860px，五列
//!   （动作 / 相对路径 / 大小 / 本地修改时间 / 远程修改时间）都**没有**
//!   最小宽度 ⇒ GTK 按内容分配 ⇒ **时间列表头被截断**（看着像
//!   「本地修改时…」）。
//!
//! **断言**（**不连远端**）：
//! 1. 五列都有**语义最小宽度**（不是 0）；
//! 2. 默认宽度下，**每个表头都放得下**（列的实际宽度 ≥ 表头文字的
//!    自然宽度）—— 这才是「表头完整」的判据；
//! 3. 两个时间列的宽度**一样**（两列并排看，不能一宽一窄）；
//! 4. 窄窗口（860px）下时间列**仍然放得下表头**；
//! 5. 窗口**不出屏**（默认宽度不许超过屏幕）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example mirror_width_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::mirrorui;
use puttyplus::panel::SftpPanel;

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

fn all_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if let Some(t) = w.downcast_ref::<T>() {
            out.push(t.clone());
        }
    });
    out
}

/// 一段文字在这个主题下要多宽（用同样字号的 Label 量）。
fn text_width(s: &str) -> i32 {
    let l = gtk::Label::new(Some(s));
    l.measure(gtk::Orientation::Horizontal, -1).1
}

fn main() {
    gtk::init().expect("gtk init");
    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ⚠️ **隔离配置**：不写用户真实的窗口尺寸记忆。
    let ini = std::env::temp_dir().join(format!("pp_mirrorw_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &ini);
    }

    let panel = SftpPanel::new(&ConnOptions::default());
    let st = std::rc::Rc::clone(&panel.st);
    // ⚠️ 目录镜像窗口**要求已连接**（要扫远程目录）⇒ 这里只测**界面**，
    //   不连真服务器 ⇒ 把「已连接」这个前置条件摆平。
    st.conn_ok.set(true);
    mirrorui::open(&st);
    pump(800);

    let win = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.title().unwrap_or_default().contains("目录镜像"))
        .expect("目录镜像窗口应当开着");
    let view = all_of::<gtk::TreeView>(&win)
        .into_iter()
        .next()
        .expect("应当有差异列表");

    let titles: Vec<String> = (0..view.n_columns())
        .filter_map(|i| view.column(i as i32))
        .map(|c| c.title().to_string())
        .collect();
    println!("[实测] 列 = {titles:?}，窗口 = {}×{}", win.width(), win.height());

    // ---- ① 每列都有语义最小宽度 ----
    for i in 0..view.n_columns() {
        let c = view.column(i as i32).expect("列");
        check!(
            &format!("第 {i} 列（{}）有最小宽度", c.title()),
            c.min_width() > 0,
            format!("min_width = {}", c.min_width())
        );
    }

    // ---- ② 默认宽度下每个表头都放得下 ----
    for i in 0..view.n_columns() {
        let c = view.column(i as i32).expect("列");
        let need = text_width(&c.title());
        let got = c.width();
        println!("[实测] 「{}」需要 {need}px，实际 {got}px", c.title());
        check!(
            &format!("表头「{}」放得下", c.title()),
            got >= need,
            format!("需要 {need}px，实际 {got}px（被截断了）")
        );
    }

    // ---- ③ 两个时间列一样宽 ----
    let widths: Vec<i32> = (0..view.n_columns())
        .filter_map(|i| view.column(i as i32))
        .map(|c| c.width())
        .collect();
    let lm = widths.iter().position(|_| true).unwrap_or(0);
    let _ = lm;
    let lm_w = view.column(3).map(|c| c.width()).unwrap_or(0);
    let rm_w = view.column(4).map(|c| c.width()).unwrap_or(0);
    check!(
        "两个时间列一样宽",
        (lm_w - rm_w).abs() <= 2,
        format!("本地 {lm_w}px / 远程 {rm_w}px")
    );

    // ---- ④ 窄窗口下时间列仍然放得下表头 ----
    win.set_default_size(860, 560);
    pump(500);
    for i in [3usize, 4] {
        let c = view.column(i as i32).expect("列");
        let need = text_width(&c.title());
        check!(
            &format!("860px 窗口下表头「{}」仍放得下", c.title()),
            c.width() >= need,
            format!("需要 {need}px，实际 {}px", c.width())
        );
    }

    // ---- ⑤ 窗口不出屏 ----
    let screen_w = gtk::gdk::Display::default()
        .and_then(|d| {
            let mons = d.monitors();
            let first = mons.item(0)?;
            first.downcast::<gtk::gdk::Monitor>().ok()
        })
        .map(|m| m.geometry().width())
        .unwrap_or(1280);
    check!(
        "窗口不出屏",
        win.width() <= screen_w,
        format!("窗口 {}px / 屏幕 {screen_w}px", win.width())
    );

    let _ = std::fs::remove_file(&ini);

    if fails.is_empty() {
        println!("mirror_width smoke ok");
    } else {
        println!("mirror_width smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
