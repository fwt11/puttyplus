//! 数据库工作台在**窄容器**里不被裁的冒烟（审计 P0-2，2026-10-30）。
//!
//! **问题**：DB 工作台是**连接页的子标签**，而连接页是横向 Paned ——
//! 左边 SFTP 文件面板**常驻**，右边才是子标签区。默认窗口 1200 宽：
//!
//! ```text
//! 1200 = 侧栏 230 + SFTP 340 + 子标签区 630
//!                                 └─ 工作台再扣掉对象树 330 → 主区只剩 300px
//! ```
//!
//! 空态卡原来 padding 左右各 28px + min-width 260px（卡片自身就 316px），
//! 里面那行快捷键提示又不换行（自然宽 ~450px）⇒ 整张卡撑出容器，
//! **主 CTA「新建数据库连接…」被裁到看不见** —— 用户以为功能坏了。
//!
//! **断言**：在按真实宽度搭出来的场景里，
//! - 主 CTA 的**右边界不许超出窗口**（不许被裁）；
//! - 空态卡的宽度不许超出子标签区；
//! - 子标签区确实是 630px 左右（防止场景本身搭歪了，断言空转）。
//!
//! ```text
//! xvfb-run -a cargo run --example db_narrow_smoke
//! xvfb-run -a cargo run --example db_narrow_smoke -- --hold   # 停留截图
//! ```

#![allow(deprecated)] // 用 allocation() 取几何（4.12 起标记废弃但仍可用）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel, subtabs};
use puttyplus::theme::{self, Theme};

/// 侧栏宽度（会话树）与 SFTP 面板宽度——跟真机一致。
const SIDEBAR_W: i32 = 230;
const SFTP_W: i32 = 340;
const WIN_W: i32 = 1200;

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

/// 在控件树里找带指定 CSS 类的第一个 widget。
fn find_by_class(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if w.css_classes().iter().any(|x| x == class) {
        return Some(w.clone());
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if let Some(f) = find_by_class(&c, class) {
            return Some(f);
        }
        cur = c.next_sibling();
    }
    None
}

/// 找文本以指定前缀开头的 Label。
fn find_label_by_prefix(w: &gtk::Widget, prefix: &str) -> Option<gtk::Label> {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        if l.text().starts_with(prefix) {
            return Some(l.clone());
        }
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if let Some(f) = find_label_by_prefix(&c, prefix) {
            return Some(f);
        }
        cur = c.next_sibling();
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");

    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);

    // ⚠️ **别碰用户的真实配置**（AGENTS.md 规则 6）：DB 工作台会往
    // databases.ini 写连接，隔离到临时文件。
    let db_ini = std::env::temp_dir().join(format!(
        "pp-db-narrow-smoke-{}.ini",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_ini);
    unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };

    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("db narrow smoke"));
    win.set_default_size(WIN_W, 780);

    // ---- 按真机结构搭：窗口 = 侧栏 | (SFTP | 子标签区) ----
    let sub = subtabs::SubTabs::new();
    *panel.st.sub_tabs.borrow_mut() = Some(sub.clone());

    let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
    inner.set_start_child(Some(panel.widget()));
    inner.set_end_child(Some(sub.root()));
    inner.set_position(SFTP_W);

    let fake_side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    fake_side.set_size_request(SIDEBAR_W, -1);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&fake_side));
    outer.set_end_child(Some(&inner));
    outer.set_position(SIDEBAR_W);

    win.set_child(Some(&outer));
    win.present();
    pump(200);

    // 打开数据库工作台（没连库 ⇒ 走空态）
    panel::on_open_db(&panel.st);
    pump(500);

    // ---- 场景本身要是对的：子标签区应只剩 ~630px ----
    let nb_w = sub.nb.allocation().width();
    eprintln!("子标签区宽度 = {nb_w}（预期 ~{}）", WIN_W - SIDEBAR_W - SFTP_W);
    assert!(
        nb_w > 400 && nb_w < 700,
        "场景宽度不对（子标签区 {nb_w}px），断言会空转"
    );

    // ---- 主 CTA 必须在窗口内（不许被裁）----
    let page = {
        let slot = panel.st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else {
            panic!("数据库工作台没打开");
        };
        ui.page.clone()
    };
    let cta = find_by_class(&page, "pp-empty-primary").expect("找不到主 CTA「新建数据库连接…」");
    let (x, y) = cta
        .translate_coordinates(&win, 0.0, 0.0)
        .expect("换算窗口坐标失败");
    let w = cta.allocation().width();
    eprintln!("主 CTA：窗口坐标 x={x} y={y} 宽={w}");
    assert!(w > 0, "主 CTA 宽度为 0（没布局出来）");
    assert!(
        x + w as f64 <= (WIN_W - 1) as f64,
        "主 CTA「新建数据库连接…」被裁了：右边界 {:.0} 超出窗口宽 {WIN_W}（x={x} 宽={w}）",
        x + w as f64
    );

    // ---- 空态卡也不能超出子标签区 ----
    if let Some(card) = find_by_class(&page, "pp-empty") {
        let (cx, _) = card.translate_coordinates(&win, 0.0, 0.0).expect("换算窗口坐标失败");
        let cw = card.allocation().width();
        eprintln!("空态卡：x={cx} 宽={cw}（子标签区 {nb_w}）");
        assert!(
            cw <= nb_w,
            "空态卡宽 {cw} 超出子标签区 {nb_w}（还是撑破了）"
        );
    }

    // ---- 顶部那句提示也不能超出容器 ----
    // ⚠️ 这条是**防御性**断言：现在这行文案（~500px）本来就放得下
    // （主区 626px），断言防的是"文案变长 ⇒ 把容器撑破 ⇒ 被裁"。
    // 审计 P0-2 里"提示被截断"指的是**空态卡里那行快捷键提示**
    // （"Ctrl+Enter 执行全部…"），那个由 hint 的 wrap + 限宽解决。
    let hint = find_label_by_prefix(&page, "这个会话下还没有");
    if let Some(h) = hint {
        let (hx, hy) = h.translate_coordinates(&win, 0.0, 0.0).expect("换算窗口坐标失败");
        let hw = h.allocation().width();
        let hh = h.allocation().height();
        eprintln!("顶部提示：x={hx} y={hy} 宽={hw} 高={hh}");
        assert!(
            hx + hw as f64 <= (WIN_W - 1) as f64,
            "顶部提示被裁了：右边界 {:.0} 超出窗口宽 {WIN_W}",
            hx + hw as f64
        );
    }

    eprintln!("✅ 窄容器里主 CTA、空态卡与顶部提示都没被裁");
    if hold {
        pump(20_000);
    }
    let _ = std::fs::remove_file(&db_ini);
}
