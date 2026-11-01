//! 查询结果的**真实列类型**冒烟（审计 P0-3，阶段 64）。
//!
//! **问题**：查询结果的列头一律写"文本"，于是
//! - 数字列点列头排序按**字典序**走（10 排在 2 前面）；
//! - 布尔列显示 `t` / `f`，用户得自己翻译；
//! - 类型真的不知道时也写"文本"——**在骗人**（用户会以为这一列真是字符串，
//!   改的时候按字符串的规矩加引号）。
//!
//! **断言**（**不连真库**，纯界面 + 排序逻辑）：
//! 1. 列头的小字显示**真实类型**（整数 / 布尔），未知类型显示「—」；
//! 2. 数字列按**数值**排序（2 在 10 前面，不是字典序）；
//! 3. 布尔列按**真假**排序（false 在 true 前面）；
//! 4. 布尔单元格显示「是」/「否」。
//!
//! ```text
//! xvfb-run -a cargo run --example coltype_smoke
//! ```

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::backend::dbmeta::ColType;
use puttyplus::panel::dbgrid;

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

/// 列头里的类型小字（第 2 行那个）。
fn header_type_texts(grid: &dbgrid::DbGrid) -> Vec<String> {
    let mut out = Vec::new();
    for c in grid.view.columns().iter() {
        let Some(w) = c.widget() else {
            panic!("列头没有自定义 widget");
        };
        let hb = w.downcast_ref::<gtk::Box>().expect("列头不是 Box").clone();
        let vb = hb
            .first_child()
            .and_then(|x| x.downcast_ref::<gtk::Box>().cloned())
            .expect("列头里没有竖向 Box");
        let mut labels = Vec::new();
        let mut child = vb.first_child();
        while let Some(c) = child {
            if let Some(l) = c.downcast_ref::<gtk::Label>() {
                labels.push(l.text().to_string());
            }
            child = c.next_sibling();
        }
        assert_eq!(labels.len(), 2, "列头应当是「列名 + 类型小字」两行：{labels:?}");
        out.push(labels[1].clone());
    }
    out
}

/// 某一列在当前排序下的显示顺序（取值）。
fn column_values(grid: &dbgrid::DbGrid, col: usize, n: usize) -> Vec<String> {
    (0..n).map(|r| grid.cell_text(r, col)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");

    gtk::init().expect("gtk init");
    // ⚠️ **别碰用户的真实配置**（AGENTS.md 规则 6）
    for (k, f) in [
        ("PUTTYPLUS_DB_INI", "pp-coltype-db.ini"),
        ("PUTTYPLUS_COLS_INI", "pp-coltype-cols.ini"),
        ("PUTTYPLUS_CONN_INI", "pp-coltype-conn.ini"),
    ] {
        let p = std::env::temp_dir().join(format!("{}-{}", f, std::process::id()));
        let _ = std::fs::remove_file(&p);
        unsafe { std::env::set_var(k, &p) };
    }
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let grid = dbgrid::DbGrid::new();
    grid.set_data(dbgrid::GridData {
        columns: vec!["n".into(), "flag".into(), "raw".into()],
        types: vec![ColType::Int, ColType::Bool, ColType::Other],
        rows: vec![
            vec![Some("10".into()), Some("t".into()), Some("x".into())],
            vec![Some("2".into()), Some("f".into()), Some("y".into())],
            vec![Some("33".into()), Some("t".into()), Some("z".into())],
        ],
        pk: vec![0],
    });
    pump(150);

    // ---- 断言 1：列头显示真实类型，未知显示「—」 ----
    let types = header_type_texts(&grid);
    assert_eq!(
        types,
        vec!["整数", "布尔", "—"],
        "列头的类型小字不对：{types:?}（未知类型要显示「—」，不能写「文本」）"
    );
    println!("列头类型小字：{types:?} ✓");

    // ---- 断言 4：布尔显示「是/否」 ----
    // ⚠️ 读**渲染模型**（屏幕上显示的那串字），不是原始数据（原始值是 "t"/"f"）。
    assert_eq!(grid.model_text(0, 1), "是", "布尔值没显示成「是」");
    assert_eq!(grid.model_text(1, 1), "否", "布尔值没显示成「否」");
    // 原始数据不受影响（排序 / 提交用的都是它）
    assert_eq!(grid.cell_text(0, 1), "t", "原始值被改动了（提交会出错）");
    println!(
        "布尔显示：{} / {} ✓",
        grid.model_text(0, 1),
        grid.model_text(1, 1)
    );

    // ---- 断言 2：数字列按数值排序 ----
    grid.sort_by_column(0, true);
    pump(80);
    let nums = column_values(&grid, 0, 3);
    assert_eq!(
        nums,
        vec!["2", "10", "33"],
        "数字列没按数值排序：{nums:?}（10 排在 2 前面就是按字典序了）"
    );
    println!("数字排序：{nums:?} ✓");

    // ---- 断言 3：布尔列按真假排序 ----
    grid.sort_by_column(1, true);
    pump(80);
    // 原始值顺序（排序用的是原始值）
    let flags = column_values(&grid, 1, 3);
    assert_eq!(
        flags,
        vec!["f", "t", "t"],
        "布尔列没按真假排序：{flags:?}（false 应当排在 true 前面）"
    );
    // 屏幕上显示的是「是/否」
    let shown: Vec<String> = (0..3).map(|r| grid.model_text(r, 1)).collect();
    assert_eq!(
        shown,
        vec!["否", "是", "是"],
        "布尔列排序后显示不对：{shown:?}"
    );
    println!("布尔排序：{flags:?} → {shown:?} ✓");

    // ---- 截图用的窗口 ----
    {
        let win = gtk::ApplicationWindow::builder()
            .title("阶段 64 列类型冒烟")
            .default_width(720)
            .default_height(260)
            .build();
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        // ⚠️ **必须 vexpand**：不撑的话滚动窗口只按内容高度占位，
        // 里面的行会被裁掉（模型里有、屏幕上没有）。
        sw.set_vexpand(true);
        sw.set_child(Some(&grid.view));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_start(10);
        root.set_margin_end(10);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        let hint = gtk::Label::new(Some(
            "列头 = 列名 + 类型小字（整数 / 布尔 / —= 类型不知道）；布尔显示「是/否」",
        ));
        hint.add_css_class("dim-label");
        hint.set_xalign(0.0);
        root.append(&hint);
        root.append(&sw);
        win.set_child(Some(&root));
        win.present();
        pump(500);
        if hold {
            println!("（--hold：停住等截图）");
            gtk::glib::MainLoop::new(None, false).run();
        }
    }

    println!("coltype_smoke: OK");
}
