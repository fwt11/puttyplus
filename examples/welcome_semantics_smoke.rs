//! 阶段 175 / UI-09 冒烟：**欢迎页的层级与交互语义**（治 P2-1）。
//!
//! ⚠️ **背景**（UI 审核 P2-1）：
//!   能力卡以前是普通 Box，但用的是 `.pp-card`（**有底色 ＋ 边框**）⇒
//!   一看着就是能点的（真能点的「最近连接」就是那个样子）⇒ 用户点了没
//!   反应 ⇒「**看起来能点但点不了**」。
//!
//! **断言**：
//! 1. 能力区**明确是说明**：有「这个应用能干什么」的小标题；
//! 2. 能力块**不是按钮**（不是 Button、祖先里也没有 Button）⇒ 不响应点击；
//! 3. 能力块**没有 `.pp-card`**（那套有底色 ＋ 边框的"卡片"外观）⇒
//!    不会被误认成可点；
//! 4. **真能点的仍然是按钮**：最近连接卡片是 Button、有焦点环样式；
//! 5. **欢迎页上没有「新建连接」按钮**（用户 2026-11-02 的硬决定，
//!    入口只留侧栏那一个）；
//! 6. **空记录态不出现空标题区**（没有「最近连接」四个字）；
//! 7. **宽屏下内容列不会无限拉大**（1920 宽的窗口里量内容列宽）；
//! 8. **垂直重心稳定**：窗口加高 200px，内容列的**上边距不变**。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example welcome_semantics_smoke

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

/// 按 CSS 类找控件。
fn all_with_class<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, cls: &str) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if w.css_classes().iter().any(|c| c == cls) {
            if let Some(t) = w.downcast_ref::<T>() {
                out.push(t.clone());
            }
        }
    });
    out
}

fn texts_of(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out = Vec::new();
    walk(&root, &mut |w| {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
    });
    out
}

/// 祖先里有没有 Button（能力块自己不是按钮、但被塞在按钮里也不行）。
fn has_button_ancestor(w: &gtk::Widget) -> bool {
    let mut p = w.parent();
    while let Some(pp) = p {
        if pp.downcast_ref::<gtk::Button>().is_some() {
            return true;
        }
        p = pp.parent();
    }
    false
}

/// 找到「能力块」那一列（三个等宽的说明块）。
fn cap_tiles(welcome: &gtk::Widget) -> Vec<gtk::Widget> {
    all_with_class::<gtk::Widget>(welcome, "pp-cap")
}

fn main() {
    // ⚠️ HOME / ini 必须在任何 glib 调用之前改（最近连接写在 HOME 底下）。
    let home = std::env::temp_dir().join(format!("pp_welcome2_home_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    }
    let recent = home.join("recent.ini");
    unsafe { std::env::set_var("PUTTYPLUS_RECENT_INI", &recent) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

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

    // ---------- ① 空记录态 ----------
    {
        let welcome = puttyplus::tabs::welcome_page();
        let wt = texts_of(&welcome);
        println!("[实测] 空欢迎页文字：{wt:?}");

        check!(
            "空记录态**不出现**空的「最近连接」标题区",
            !wt.iter().any(|t| t == "最近连接"),
            format!("{wt:?}")
        );
        check!(
            "空记录态留了一条清晰指引",
            wt.iter().any(|t| t.contains("从左侧建一个连接")),
            format!("{wt:?}")
        );
        check!(
            "欢迎页上没有「新建连接」按钮（入口只留侧栏那一个）",
            !wt.iter().any(|t| t == "新建连接"),
            format!("{wt:?}")
        );

        // ---- 能力区是「说明」 ----
        check!(
            "能力区有「这个应用能干什么」的小标题（明确是说明）",
            wt.iter().any(|t| t.contains("这个应用能干什么")),
            format!("{wt:?}")
        );
        let tiles = cap_tiles(&welcome);
        println!("[实测] 能力块数 = {}", tiles.len());
        check!(
            "有三个能力块",
            tiles.len() == 3,
            format!("{} 个", tiles.len())
        );
        check!(
            "能力块**自己不是按钮**",
            tiles.iter().all(|t| t.downcast_ref::<gtk::Button>().is_none()),
            "有能力块是 Button".to_string()
        );
        check!(
            "能力块**祖先里也没有按钮**（不会被点错）",
            tiles.iter().all(|t| !has_button_ancestor(t)),
            "有能力块在按钮里".to_string()
        );
        check!(
            "能力块**没有 `.pp-card`**（那套有底色＋边框的「卡片」外观）",
            tiles
                .iter()
                .all(|t| !t.css_classes().iter().any(|c| c == "pp-card")),
            "还有能力块带 pp-card".to_string()
        );
        // ⚠️ **能力块不可聚焦**：说明不是控件 ⇒ 不该进 Tab 序列
        //   （审核验收：「Tab 顺序按视觉顺序」⇒ 说明块占位是噪声）。
        check!(
            "能力块**不可聚焦**（不进 Tab 序列）",
            tiles.iter().all(|t| !t.is_focusable()),
            "有能力块能聚焦".to_string()
        );
    }

    // ---------- ② 有记录：真能点的仍然是按钮 ----------
    {
        let ini = gtk::glib::KeyFile::new();
        ini.set_string("recent", "0", "测试机\u{1f}10.0.0.9\u{1f}22\u{1f}ops");
        ini.set_string("recent", "1", "srv-01\u{1f}10.0.0.9\u{1f}22\u{1f}root");
        ini.save_to_file(&recent).unwrap();

        let welcome = puttyplus::tabs::welcome_page();
        let wt = texts_of(&welcome);
        let cards = all_with_class::<gtk::Button>(&welcome, "pp-recent-card");
        check!(
            "最近连接仍是**真按钮**（能点）",
            cards.len() == 2,
            format!("{} 张卡片", cards.len())
        );
        check!(
            "卡片上写着 user@host:port",
            wt.iter().any(|t| t.contains("10.0.0.9:22")),
            format!("{wt:?}")
        );

        // ---- 主题 CSS 里真的有焦点环（键盘用户看得见）----
        let css = puttyplus::theme::css_text(puttyplus::theme::Theme::Light).unwrap_or_default();
        check!(
            "最近连接卡片有**可见焦点环**（:focus-visible）",
            css.contains("button.pp-recent-card:focus-visible"),
            "主题 CSS 里没有焦点环".to_string()
        );
        check!(
            "说明卡**没有** hover 样式（不会看着能点）",
            !css.contains(".pp-cap:hover"),
            "`.pp-cap:hover` 不该存在".to_string()
        );

        // ---------- ③ 宽屏：内容列不会无限拉大 ----------
        let win = gtk::Window::new();
        win.set_default_size(1920, 900);
        win.set_child(Some(&welcome));
        win.present();
        pump(500);

        // 内容列 = 装 `.pp-cap` 的那一列的父级（Box）。
        let cap_parent = tiles_parent(&welcome);
        let w_wide = cap_parent.as_ref().map(|w| w.allocation().width()).unwrap_or(0);
        println!("[实测] 1920 宽窗口 ⇒ 内容列宽 = {w_wide}");
        check!(
            "宽屏下内容列**不超过 900px**（不会无限拉大）",
            w_wide > 0 && w_wide <= 900,
            format!("内容列宽 = {w_wide}")
        );

        // ---------- ④ 垂直重心稳定 ----------
        let y_wide = cap_parent.as_ref().map(|w| w.allocation().y()).unwrap_or(-1);
        win.set_default_size(1920, 1100);
        pump(500);
        let y_tall = cap_parent.as_ref().map(|w| w.allocation().y()).unwrap_or(-1);
        println!("[实测] 窗口 900 高 ⇒ 内容列 y = {y_wide}；1100 高 ⇒ y = {y_tall}");
        check!(
            "窗口加高 200px，**内容列的 y 不变**（重心稳定，不上下浮）",
            y_wide >= 0 && y_wide == y_tall,
            format!("900 高 y = {y_wide} / 1100 高 y = {y_tall}")
        );
        win.close();
        pump(200);
    }

    let _ = std::fs::remove_dir_all(&home);

    if fails.is_empty() {
        println!("welcome_semantics smoke ok");
    } else {
        println!("welcome_semantics smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}

/// 能力块所在的那**一列**（内容列 = 能力块的**爷爷**）。
fn tiles_parent(welcome: &gtk::Widget) -> Option<gtk::Widget> {
    let tiles = cap_tiles(welcome);
    let first = tiles.first()?;
    // 能力块 → 能力那一行 → **内容列**
    let row = first.parent()?;
    let col = row.parent()?;
    Some(col)
}
