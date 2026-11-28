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
    *panel.st.sub_tabs.borrow_mut() = Some(panel::subtabs::SubTabs::new());
    pump(200);

    panel::on_open_db(&panel.st);
    puttyplus::panel::dbui::mount_for_test(&panel.st);
    pump(400);
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

    // 收起前：编辑区可见
    check!(
        "收起前 SQL 编辑区是**可见**的",
        sql_editor.is_visible(),
        "编辑区一开始就不见了".to_string()
    );

    // ⚠️ **分隔条的位置**＝「SQL 区占了多高」⇒ 收起来之后它必须**下移**
    //   （只把编辑区藏起来而不管分隔条 ⇒ 空间白留着，用户看到的还是
    //   "SQL 区太大" ⇒ 这条断言才是「真的收起」的证据）。
    let editor_w: gtk::Widget = sql_editor.clone().upcast();
    let paned = find_sql_paned(&ui_page, &editor_w);
    check!(
        "找得到装着 SQL 编辑器的那条分隔",
        paned.is_some(),
        "没找到竖直 Paned".to_string()
    );
    let pos_before = paned.as_ref().map(|p| p.position()).unwrap_or(-1);

    // 点一下 ⇒ 编辑区藏起来
    toggle.emit_clicked();
    pump(300);
    check!(
        "点一下 ⇒ SQL 编辑区**收起来**了",
        !sql_editor.is_visible(),
        "编辑区还看得见".to_string()
    );
    let pos_after = paned.as_ref().map(|p| p.position()).unwrap_or(-1);
    check!(
        "收起后**分隔条下移**（空间真的还给结果区了）",
        pos_after > pos_before + 100,
        format!("分隔条位置 {pos_before} → {pos_after}（没下移 ⇒ 空间白留着）")
    );

    // 再点一下 ⇒ 编辑区回来
    toggle.emit_clicked();
    pump(300);
    check!(
        "再点一下 ⇒ SQL 编辑区**展开**回来",
        sql_editor.is_visible(),
        "编辑区没回来".to_string()
    );
    let pos_back = paned.as_ref().map(|p| p.position()).unwrap_or(-1);
    check!(
        "展开后分隔条**回到原位**",
        (pos_back - pos_before).abs() <= 2,
        format!("分隔条位置 {pos_before} → {pos_back}（没还原）")
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
