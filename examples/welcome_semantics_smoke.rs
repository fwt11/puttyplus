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
        win.close();
        pump(200);

        // ---------- ④ 垂直居中（上下留白均衡）----------
        //
        // ⚠️ **这条改写了阶段 175 的「重心稳定」**：那时要的是"窗口加高
        //   200px，内容列的 **y 不变**"（内容贴顶）—— 但那**正是**用户
        //   2026-11-27 报的现象：「上面太挤，下面太空」（用户截图：860 高的
        //   窗口里内容只占上面 380px，下面 480px 全空）⇒ 改成**垂直居中**：
        //   内容列要**跟着窗口一起往下移**，上下留白一样多。
        //
        // ⚠️⚠️ **必须用两个不同高度的窗口来量**（2026-11-27 实测）：Xvfb 里
        //   **没有窗口管理器** ⇒ 已经显示出来的窗口 `set_default_size` 不会
        //   真的改变大小 ⇒ 老写法「同一个窗口改高度」量到的其实是**同一个
        //   高度** ⇒ 那条「重心稳定」断言是**空转的**（恒真，挡不住任何
        //   回归）⇒ 这里改成"开两个窗口分别量"。
        let measure = |h: i32| -> (i32, i32, i32) {
            // ⚠️ **每个窗口要自己的页面**：一个 widget 只能有一个父级。
            let page = puttyplus::tabs::welcome_page();
            let w = gtk::Window::new();
            w.set_default_size(1920, h);
            w.set_child(Some(&page));
            w.present();
            pump(500);
            let cp = tiles_parent(&page);
            let r = cp
                .map(|w| {
                    let a = w.allocation();
                    // ⚠️ **上下留白怎么量**：内容列的 `y` 是**相对父级**
                    //   （Viewport）的 ⇒ 上留白 = y；下留白 = 视口高 - y - 高。
                    let vp = w.parent().map(|p| p.allocation().height()).unwrap_or(0);
                    (a.y(), a.height(), vp)
                })
                .unwrap_or((0, 0, 0));
            w.close();
            pump(200);
            r
        };
        let (y900, h900, vp900) = measure(900);
        let (y1100, h1100, vp1100) = measure(1100);
        let gap_top = y900;
        let gap_bottom = vp900.saturating_sub(y900 + h900);
        println!("[实测] 900 高窗口 ⇒ 内容列 y={y900} h={h900} 视口高={vp900}（上留白 {gap_top} / 下留白 {gap_bottom}）");
        check!(
            "内容列**垂直居中**（上留白 ≈ 下留白，不再「上面挤、下面空」）",
            gap_top > 0 && (gap_top as i32 - gap_bottom as i32).abs() <= 48,
            format!("上留白 {gap_top} / 下留白 {gap_bottom}")
        );
        println!("[实测] 1100 高窗口 ⇒ 内容列 y={y1100} h={h1100} 视口高={vp1100}");
        check!(
            "窗口更高 ⇒ 内容列**跟着往下移**（垂直居中；旧规则是\"不动\"）",
            y1100 > y900,
            format!("900 高 y = {y900} / 1100 高 y = {y1100}")
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
