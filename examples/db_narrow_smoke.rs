//! 数据库工作台在**窄容器**里不被裁的冒烟（审计 P0-2，2026-10-30）。
//!
//! **问题**：DB 工作台是**连接页的子标签**。阶段 97 之前，连接页是横向
//! Paned —— 左边 SFTP 文件面板**常驻**，右边才是子标签区。默认窗口 1200 宽：
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
//! **阶段 97 之后**：文件面板进了**终端子页内部**，连接页只剩子标签区 ⇒
//! 子标签区拿到「窗口宽 - 侧栏宽」整块（1200 宽 ⇒ 970px），DB 页不再被
//! 文件面板挤窄。这个冒烟守住的是「窄窗口里 CTA 依然不被裁」。
//!
//! **断言**：在按真实宽度搭出来的场景里，
//! - 空态卡与卡里那句提示的**右边界不许超出窗口**（不许被裁）；
//! - 空态卡的宽度不许超出子标签区；
//! - 子标签区确实是「窗口宽 - 侧栏宽」左右（防止场景本身搭歪了，断言空转）。
//!
//! ⚠️ **断言对象换过**（2026-11-05）：原先盯的是「主 CTA」按钮
//! （`pp-empty-primary`）——但 DB 页的空态阶段 83 起改成了**结构化骨架**
//! （品牌 + 提示），那个类只有侧栏在用 ⇒ 断言早就空了（这个例子一直
//! 没进 CI 的冒烟清单，所以没人发现）。现在盯的是**空态卡本身**。
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

    // ---- 按真机结构搭：窗口 = 侧栏 | 子标签区；终端子页 = SFTP | 终端 ----
    //
    // ⚠️ 阶段 97：连接页不再是「SFTP｜子标签区」的 Paned 了——文件面板进了
    // **终端子页内部**，连接页只剩子标签区 ⇒ 子标签区拿到的是
    // 「窗口宽 - 侧栏宽」整块，DB 页自然宽裕。
    let sub = subtabs::SubTabs::new();
    *panel.st.sub_tabs.borrow_mut() = Some(sub.clone());

    {
        // 终端子页 = 横向 Paned：左＝文件面板，右＝终端占位
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        term_page.set_start_child(Some(panel.widget()));
        let term = gtk::Label::new(Some("终端（narrow smoke 占位）"));
        term.set_margin_top(20);
        term_page.set_end_child(Some(term.upcast_ref::<gtk::Widget>()));
        term_page.set_position(SFTP_W);
        sub.add_page("终端", term_page.upcast::<gtk::Widget>(), None);
    }

    let fake_side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    fake_side.set_size_request(SIDEBAR_W, -1);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&fake_side));
    outer.set_end_child(Some(sub.root()));
    outer.set_position(SIDEBAR_W);

    win.set_child(Some(&outer));
    win.present();
    pump(200);

    // 打开数据库工作台（没连库 ⇒ 走空态）
    panel::on_open_db(&panel.st);
    pump(500);

    // ---- 场景本身要是对的：子标签区应当是「窗口宽 - 侧栏宽」整块 ----
    let nb_w = sub.nb.allocation().width();
    eprintln!("子标签区宽度 = {nb_w}（预期 ~{}）", WIN_W - SIDEBAR_W);
    assert!(
        nb_w > 700 && nb_w < 1000,
        "场景宽度不对（子标签区 {nb_w}px），断言会空转"
    );

    // ---- 空态不许被裁 ----
    let page = {
        let slot = panel.st.db_ui.borrow();
        let Some(ui) = slot.as_ref() else {
            panic!("数据库工作台没打开");
        };
        ui.page.clone()
    };
    //
    // ⚠️ **DB 页只有连上库才会挂进子标签**（阶段 98 的规矩：连不上就不摆
    // 一个空工作台给用户看）⇒ 这里**手动挂上去**，跟 `dbui::mount` 连上后
    // 做的事一样（冒烟量的是**布局**，不是连接流程）。
    //
    // 不挂的话 `ui.page` 是个**游离控件** ⇒ `translate_coordinates` 换算
    // 窗口坐标会直接失败（"换算窗口坐标失败"）。
    let db_tab = sub.add_page_icon_kind(
        Some("drive-harddisk-symbolic"),
        "数据库",
        subtabs::PageKind::Db,
        page.clone(),
        None,
    );
    // ⚠️ **必须切过去**：notebook 只映射当前页 ⇒ 不切的话 DB 页整棵控件树
    // 都没被布局（控件宽 2px、坐标全是 0）⇒ 断言会**空转**（阶段 66 的坑）。
    sub.switch_to(db_tab);
    pump(300);
    //
    // ⚠️ **断言对象换过了**（2026-11-05 修）：DB 页的空态**早就不再用**
    // `emptystate::EmptyState`（那个构件强制"必须有一个主 CTA"）——
    // 阶段 83 起改成了**结构化骨架**：品牌 + 提示（连上后才有动作行）。
    // ⇒ 这里盯的是**空态卡 `pp-empty`** 与卡里的**提示 Label**，
    //  原来的「主 CTA（`pp-empty-primary`）」在 DB 页里**根本不存在**
    //  （那个类只有侧栏「新建连接」与密钥管理在用）⇒ 断言会永远失败。
    let card = find_by_class(&page, "pp-empty").expect("找不到空态卡（pp-empty）");
    let (cx, _) = card
        .translate_coordinates(&win, 0.0, 0.0)
        .expect("换算窗口坐标失败");
    let cw = card.allocation().width();
    eprintln!("空态卡：x={cx} 宽={cw}（子标签区 {nb_w}）");
    assert!(cw > 0, "空态卡宽度为 0（没布局出来）");
    // ⚠️ **不许塌成一条缝**：没被布局的控件宽是 2px（GTK 的最小分配）⇒
    // 只断言「宽 > 0」会被这种废值蒙混过关（阶段 66 的坑：断言空转）。
    assert!(
        cw >= 300,
        "空态卡宽只有 {cw}px ⇒ 这一页根本没被布局（断言会空转）"
    );
    assert!(
        cx as f64 + cw as f64 <= (WIN_W - 1) as f64,
        "空态卡被裁了：右边界 {:.0} 超出窗口宽 {WIN_W}（x={cx} 宽={cw}）",
        cx as f64 + cw as f64
    );
    assert!(
        cw <= nb_w,
        "空态卡宽 {cw} 超出子标签区 {nb_w}（还是撑破了）"
    );

    // ---- 卡里那句提示也不能超出容器 ----
    //
    // ⚠️ 防御性断言：这行文案本来就放得下，断言防的是"文案变长 ⇒ 把容器
    // 撑破 ⇒ 被裁"。审计 P0-2 里"提示被截断"指的是**空态卡里那行不换行
    // 的快捷键提示**（"Ctrl+Enter 执行全部…"），阶段 96 已把它从卡片里
    // 挪到工具栏，卡片里现在只剩这一句。
    let hint = find_label_by_prefix(&page, "先在左侧打开一个数据库连接")
        .expect("找不到空态提示「先在左侧打开一个数据库连接」");
    let (hx, hy) = hint.translate_coordinates(&win, 0.0, 0.0).expect("换算窗口坐标失败");
    let hw = hint.allocation().width();
    eprintln!("顶部提示：x={hx} y={hy} 宽={hw}");
    assert!(
        hx + hw as f64 <= (WIN_W - 1) as f64,
        "顶部提示被裁了：右边界 {:.0} 超出窗口宽 {WIN_W}",
        hx + hw as f64
    );

    eprintln!("✅ 窄容器里空态卡与提示都没被裁");
    if hold {
        pump(20_000);
    }
    let _ = std::fs::remove_file(&db_ini);
}
