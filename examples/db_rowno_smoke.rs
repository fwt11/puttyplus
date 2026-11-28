//! 阶段 187：数据库工作台的三处改造（行号列 / SQL 区可收起 / 底栏自解释）。
//!
//! **用户 2026-11-27 报的三条**：
//! ① 「默认的 SQL 命令编辑区域太大 ⇒ 要一种能隐藏它的办法」；
//! ② 「数据库表显示部分要有**序号**（像 Excel），让用户知道这是第几行」；
//! ③ 「『总行数』是什么意思、后面的下拉框又是什么意思」。
//!
//! 盯的断言：
//! - 网格最左边是**行号列**：1、2、3…；分页时**接着页码走**（跳到第 3 页
//!   ⇒ 行号从 401 开始）；
//! - 行号列**不可编辑、不参与排序**（它是算出来的，不是数据）；
//! - SQL 编辑区能**收起**（编辑区不见了 ⇒ 空间真的还给结果区）、
//!   也能**展开**回来；
//! - 底栏的每页行数下拉**前面有「每页」标签**（自解释）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_rowno_smoke

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::{self, SftpPanel, dbgrid};
use puttyplus::theme::{self, Theme};

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

/// 在控件树里找第一个符合条件的控件。
fn find<T: IsA<gtk::Widget>>(root: &gtk::Widget, pred: impl Fn(&gtk::Widget) -> bool) -> Option<T> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if pred(&w) {
            if let Some(t) = w.downcast_ref::<T>() {
                return Some(t.clone());
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

/// `target` 在不在 `root` 的子树里。
fn contains(root: &gtk::Widget, target: &gtk::Widget) -> bool {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if &w == target {
            return true;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    false
}

/// 找那个**装着 SQL 编辑器**的竖直 Paned（工作台右半：结果区 / SQL 区）。
fn find_sql_paned(root: &gtk::Widget, editor: &gtk::Widget) -> Option<gtk::Paned> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if let Some(p) = w.downcast_ref::<gtk::Paned>() {
            if p.orientation() == gtk::Orientation::Vertical {
                if let Some(end) = p.end_child() {
                    if contains(&end, editor) {
                        return Some(p.clone());
                    }
                }
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

/// 控件树里带这个 CSS 类的节点有几个。
fn count_class(root: &gtk::Widget, cls: &str) -> usize {
    let mut n = 0;
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if w.css_classes().iter().any(|c| c == cls) {
            n += 1;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    n
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");
    let th = if args.iter().any(|a| a == "light") {
        Theme::Light
    } else {
        Theme::Dark
    };

    // ⚠️ **隔离配置**：不许碰用户的真实设置
    for (k, f) in [
        ("PUTTYPLUS_DB_INI", "pp-rowno-db.ini"),
        ("PUTTYPLUS_COLS_INI", "pp-rowno-cols.ini"),
        ("PUTTYPLUS_CONN_INI", "pp-rowno-conn.ini"),
        ("PUTTYPLUS_SETTINGS_INI", "pp-rowno-settings.ini"),
        ("PUTTYPLUS_RECENT_INI", "pp-rowno-recent.ini"),
    ] {
        let p = std::env::temp_dir().join(format!("{}-{}", f, std::process::id()));
        let _ = std::fs::remove_file(&p);
        unsafe { std::env::set_var(k, &p) };
    }
    unsafe { std::env::set_var("GTK_A11Y", "none") };
    gtk::init().expect("gtk init");
    puttyplus::theme::apply(th);

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

    /* ================= ① 行号列（Excel 式） ================= */

    let grid = dbgrid::DbGrid::new();
    grid.set_data(dbgrid::GridData {
        columns: vec!["id".into(), "name".into()],
        types: vec![ColType::Int, ColType::Text],
        rows: vec![
            vec![Some("1".into()), Some("甲".into())],
            vec![Some("2".into()), Some("乙".into())],
            vec![Some("3".into()), Some("丙".into())],
        ],
        pk: vec![0],
    });
    pump(150);

    let nums: Vec<String> = (0..3).map(|r| grid.row_number_text(r)).collect();
    check!(
        "网格最左边有**行号列**（1、2、3）",
        nums == vec!["1", "2", "3"],
        format!("行号是 {nums:?}")
    );

    // ⚠️ **行号列不许编辑、不许排序**：它是算出来的
    let cols = grid.view.columns();
    check!(
        "行号列在最左边（第 0 列）",
        cols.len() == 3 && cols[0].title().is_empty(),
        format!(
            "列数 = {}，第 0 列标题 = {:?}（行号列的标题应当是空的）",
            cols.len(),
            cols[0].title()
        )
    );
    check!(
        "数据列的标题还在（第 1、2 列 = id、name）",
        cols.len() >= 3 && cols[1].title() == "id" && cols[2].title() == "name",
        format!("列标题 = {:?}", cols.iter().map(|c| c.title().to_string()).collect::<Vec<_>>())
    );

    // 分页：跳到第 3 页 ⇒ 行号从 401 开始（接真实行号）
    grid.set_row_number_offset(400);
    pump(120);
    let nums2: Vec<String> = (0..3).map(|r| grid.row_number_text(r)).collect();
    check!(
        "分页后行号**接着页码走**（401、402、403）",
        nums2 == vec!["401", "402", "403"],
        format!("行号是 {nums2:?}")
    );
    grid.set_row_number_offset(0);
    pump(120);

    /* ================= ② SQL 区可收起 ================= */

    // ---- 建出数据库工作台（不连真库）----
    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("db rowno smoke"));
    win.set_default_size(1100, 720);
    win.set_child(Some(panel.widget()));
    win.present();
    // 连接页的子标签（真实运行时由 tabs.rs 建）
    let sub = panel::subtabs::SubTabs::new();
    *panel.st.sub_tabs.borrow_mut() = Some(sub.clone());
    // ⚠️⚠️ **子标签那一块得真的在屏幕上**：工作台是它的**子页签**，
    //   不把它放进窗口 ⇒ 工作台不会被布局（高度恒为 0 ⇒ 量不出
    //   「SQL 区占了多高」）⇒ 底栏/分隔条的断言就成了空断言。
    let sub_win = gtk::Window::new();
    sub_win.set_title(Some("db rowno smoke · 子标签"));
    sub_win.set_default_size(1200, 780);
    sub_win.set_child(Some(sub.root()));
    sub_win.present();
    pump(200);

    panel::on_open_db(&panel.st);
    puttyplus::panel::dbui::mount_for_test(&panel.st);
    pump(400);
    // ⚠️ **必须切到数据库那一页**：工作台是连接页的**子标签**，不切过去
    //   它就没被布局（分隔条的高度是 0 ⇒ 量不出「SQL 区占了多高」）。
    if let Some(sub) = panel.st.sub_tabs() {
        let db_id = panel
            .st
            .db_ui
            .borrow()
            .as_ref()
            .map(|u| u.tab_id.get())
            .unwrap_or(0);
        sub.switch_to(db_id);
    }
    pump(500);
    let db_open = panel.st.db_ui.borrow().is_some();
    check!("数据库工作台能建出来", db_open, "工作台没打开".to_string());
    if !db_open {
        eprintln!("工作台没开起来，后面没法测");
        std::process::exit(1);
    }

    let (sql_editor, ui_page) = {
        let slot = panel.st.db_ui.borrow();
        let ui = slot.as_ref().expect("工作台");
        (ui.sql.clone(), ui.page.clone())
    };

    // 找「收起 SQL 编辑器」那颗按钮（tooltip 认它 ⇒ 不受图标名影响）
    let toggle = find::<gtk::Button>(&ui_page, |w| {
        w.downcast_ref::<gtk::Button>()
            .and_then(|b| b.tooltip_text())
            .map(|t| t.contains("SQL 编辑器"))
            .unwrap_or(false)
    });
    check!(
        "工具条上有「收起 SQL 编辑器」按钮",
        toggle.is_some(),
        "没找到那颗按钮（tooltip 里含「SQL 编辑器」）".to_string()
    );
    let Some(toggle) = toggle else {
        eprintln!("找不到收起按钮，后面没法测");
        std::process::exit(1);
    };

    // ⚠️ **默认就是「收着的」**（用户 2026-11-28：「sql 编辑的区域还是很大
    //   ⇒ 应该是尽量显示数据，用户不一定需要编辑 sql」）⇒ 启动时编辑区
    //   **藏起来**，下半区让给结果网格。
    check!(
        "**默认**SQL 编辑区就是**收着的**（空间让给结果网格）",
        !sql_editor.is_visible(),
        "编辑区默认可见 ⇒ 下半区被占了一半".to_string()
    );

    // ⚠️ **分隔条的位置**＝「SQL 区占了多高」⇒ 收着时它必须**贴到最下边**
    //   （只把编辑区藏起来而不管分隔条 ⇒ 空间白留着，用户看到的还是
    //   "SQL 区太大" ⇒ 这条断言才是「真的收起」的证据）。
    let editor_w: gtk::Widget = sql_editor.clone().upcast();
    let paned = find_sql_paned(&ui_page, &editor_w);
    check!(
        "找得到装着 SQL 编辑器的那条分隔",
        paned.is_some(),
        "没找到竖直 Paned".to_string()
    );
    let pos_collapsed = paned.as_ref().map(|p| p.position()).unwrap_or(-1);

    // 点一下 ⇒ 展开（编辑区回来 + 给一块够写 SQL 的地方）
    toggle.emit_clicked();
    pump(300);
    check!(
        "点一下 ⇒ SQL 编辑区**展开**回来",
        sql_editor.is_visible(),
        "编辑区没回来".to_string()
    );
    let pos_expanded = paned.as_ref().map(|p| p.position()).unwrap_or(-1);
    check!(
        "展开后**分隔条上移**（编辑区拿到了空间）",
        pos_expanded < pos_collapsed - 50,
        format!("分隔条位置 {pos_collapsed} → {pos_expanded}（没上移 ⇒ 编辑区还是没地方写）")
    );

    // 再点一下 ⇒ 又收起来
    // ⚠️⚠️ **必须同步读**：`emit_clicked()` 是**同步**的（处理器当场跑完）⇒
    //   紧接着读 position 读到的就是「**刚写进去的那个值**」⇒ 才能抓住
    //   `i32::MAX`（写成它 ⇒ 结果区拿到 21 亿像素 ⇒ 布局崩 ⇒ 卡死/控件消失）。
    //   ⚠️ **先 pump 再读就抓不到了**：主循环里的分配会把位置**修回来**
    //   （notify 处理器会重新贴底）⇒ 断言就成了永远为真的假阳性。
    toggle.emit_clicked();
    let pos_sync = paned.as_ref().map(|p| p.position()).unwrap_or(-1);
    let max_sync = paned.as_ref().map(|p| p.max_position()).unwrap_or(-1);
    check!(
        "收起时 position **不超过合法上限**（不能写 i32::MAX ⇒ 结果区会被撑爆）",
        pos_sync <= max_sync,
        format!("刚写进去的 position = {pos_sync}，合法上限 = {max_sync}")
    );
    pump(300);
    check!(
        "再点一下 ⇒ SQL 编辑区**收起来**",
        !sql_editor.is_visible(),
        "编辑区没收起来".to_string()
    );
    let pos_again = paned.as_ref().map(|p| p.position()).unwrap_or(-1);
    check!(
        "收起后**分隔条回到最下边**",
        pos_again >= pos_collapsed - 2,
        format!("分隔条位置 {pos_collapsed} → {pos_again}（没贴底 ⇒ 空间白留着）")
    );

    // ⚠️⚠️ **回归护栏：结果区拿到的高度必须是「正常值」**（2026-11-28 修的
    //   一个真 bug）：收起时把 Paned 的 position 写成 `i32::MAX` ⇒ GTK 把它
    //   **当成上半区（结果网格）的高度** ⇒ 结果区拿到 **21 亿像素** ⇒ 整个
    //   工作台的尺寸协商崩掉 ⇒ **界面卡死、控件整个消失**，切页面才恢复
    //   （用户原话：「点几次就卡死了，然后整个控件都不见了」）。
    // ⇒ 现在写的是 `max_position`（GTK 自己算的合法上限）⇒ 高度必须是 sane 的。
    let start_h = paned
        .as_ref()
        .and_then(|p| p.start_child())
        .map(|c| c.height())
        .unwrap_or(-1);
    let paned_h = paned.as_ref().map(|p| p.height()).unwrap_or(-1);
    check!(
        "结果区拿到的**高度是正常值**（不是 21 亿像素 ⇒ 布局不会崩）",
        start_h > 0 && start_h < 100_000,
        format!("结果区高度 = {start_h}px（Paned 总高 {paned_h}px）")
    );
    let pos_vs_max = paned
        .as_ref()
        .map(|p| (p.position(), p.max_position()))
        .unwrap_or((-1, -1));
    check!(
        "分隔条位置**不超过合法上限**（不会再越界 ⇒ 不会把结果区撑爆）",
        pos_vs_max.0 <= pos_vs_max.1,
        format!("position = {}，max_position = {}", pos_vs_max.0, pos_vs_max.1)
    );

    /* ================= ③ 底栏自解释 ================= */

    // ⚠️ **底栏属于「结果页签」**（不是工作台本身）⇒ 得先有一个结果页签
    //   （真库里是跑一条查询才有）。
    let tab_idx = puttyplus::panel::dbui::make_test_result_tab(
        &panel.st,
        "行号冒烟",
        &["id", "name"],
        &[ColType::Int, ColType::Text],
        vec![
            vec![Some("1".into()), Some("甲".into())],
            vec![Some("2".into()), Some("乙".into())],
        ],
    );
    check!(
        "能造出一个结果页签（底栏在那儿）",
        tab_idx.is_some(),
        "结果页签造不出来".to_string()
    );
    pump(300);

    // 每页行数下拉**前面有「每页」标签**
    let lbl_every = find::<gtk::Label>(&ui_page, |w| {
        w.downcast_ref::<gtk::Label>()
            .map(|l| l.text() == "每页")
            .unwrap_or(false)
    });
    check!(
        "底栏的每页行数下拉**前面有「每页」标签**",
        lbl_every.is_some(),
        "没找到「每页」标签".to_string()
    );

    // ⚠️⚠️ **下拉框必须真的有文字**（用户 2026-11-28：「后面的下拉框又是
    //   什么意思」—— 因为 `ComboBoxText::append` 的参数顺序在 GTK4 里是
    //   **`(id, text)`**（GTK3 是 `(text, id)`）⇒ 传反了 `set_active_id`
    //   永远选不中 ⇒ **下拉框一个字都不显示**）。
    let combo = find::<gtk::ComboBoxText>(&ui_page, |_| true);
    check!(
        "「每页行数」下拉框**有当前值**（不是空的）",
        combo
            .as_ref()
            .and_then(|c| c.active_text())
            .map(|t| t == "200 行")
            .unwrap_or(false),
        format!(
            "下拉框当前值 = {:?}",
            combo.as_ref().and_then(|c| c.active_text())
        )
    );

    // ⚠️ **底栏要分段**（用户：「这行显示的有些混乱」）⇒ 几段之间有竖线
    let seps = count_class(&ui_page, "pp-foot-sep");
    check!(
        "底栏**分了段**（段与段之间有竖线）",
        seps >= 4,
        format!("只找到 {seps} 条分隔线（至少 4 段）")
    );

    // ⚠️ **「本页 200 行」**：光写「200 行」会被当成**总数**
    //   （旁边就摆着「总行数 1989」）⇒ 得分清。
    // ⚠️ **要 `starts_with("本页 ")`**：光 `contains("本页")` 会匹配到
    //   「**筛本页**」那个标签 ⇒ 断言就成了永远为真的假阳性。
    let info = find::<gtk::Label>(&ui_page, |w| {
        w.downcast_ref::<gtk::Label>()
            .map(|l| l.text().starts_with("本页 ") && l.text().contains("行"))
            .unwrap_or(false)
    });
    check!(
        "底栏左侧写的是「**本页** N 行」（跟「总行数」分得清）",
        info.is_some(),
        "没找到「本页 N 行」".to_string()
    );

    // 「总行数」按钮：没统计过时是「总行数」
    let btn_total = find::<gtk::Button>(&ui_page, |w| {
        w.downcast_ref::<gtk::Button>()
            .and_then(|b| b.label())
            .map(|l| l == "总行数")
            .unwrap_or(false)
    });
    check!(
        "底栏有「总行数」按钮",
        btn_total.is_some(),
        "没找到「总行数」按钮".to_string()
    );

    // ⚠️ **按钮文字的两种形态**（纯函数，逻辑跟真代码共用）
    let t_none = puttyplus::panel::dbui::total_button_text(None);
    let t_some = puttyplus::panel::dbui::total_button_text(Some(117_837));
    check!(
        "「总行数」按钮统计后**显示结果**（总行数 117837）",
        t_none == "总行数" && t_some == "总行数 117837",
        format!("未统计 = {t_none:?}，统计后 = {t_some:?}")
    );

    /* ================= 截图窗口 ================= */
    //
    // ⚠️ **切到数据库那一页** ⇒ 面板窗口里显示的就是工作台
    //   （工作台是连接页的**子标签**，不切过去看不到）。
    // 另开一个窗口给「行号列」的特写（行号列在最左边 ⇒ 看得最清楚）
    {
        let win = gtk::ApplicationWindow::builder()
            .title("行号列（Excel 式）")
            .default_width(520)
            .default_height(260)
            .build();
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);
        sw.set_child(Some(&grid.view));
        win.set_child(Some(&sw));
        win.present();
        pump(500);
        if hold {
            println!("（--hold：停住等截图）");
            pump(20_000);
        }
    }

    if fails.is_empty() {
        println!("db_rowno_smoke: OK");
    } else {
        println!("db_rowno_smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
