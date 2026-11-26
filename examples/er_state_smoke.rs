//! 阶段 174 / UI-10 冒烟：**ER 图「没有关系」时给出有意义的状态**。
//!
//! ⚠️ **背景**（UI 审核 P2-3）：单表或无关系时，用户只看到**一大片空白
//!   画布** ⇒ 分不清是「没有关系」还是「图没加载」。
//!
//! **断言**：
//! 1. **0 张表** ⇒ 说明板亮着，标题说「这个 schema 里还没有表」；
//! 2. **1 张表** ⇒ 说明板亮着，标题里**带表名**，且与 0 表时**不同**；
//!    并且解释里**带字段摘要**（计划验收：「单表仍显示真实表名/字段摘要」）；
//! 3. **多张表 0 条边** ⇒ 说明板亮着，标题说「没有找到表间关系」，
//!    且与前两种**都不同**（三种状态标题与解释各自不同）；
//! 4. **有边** ⇒ 说明板**藏起来**（画布本身就是内容，不要多嘴）；
//! 5. **主 CTA 有可见结果**（计划验收：「主 CTA 产生可见结果，
//!    不出现『点了没反应』」）：点「打开画布」⇒ 状态行变了 ＋ 画布拿到焦点；
//! 6. **「查看表数据」只在单表时出现**，点了会**真的回调**打开那张表
//!    （走双击表框的同一条路径 ⇒ 不新增写库行为）；
//! 7. **不生成假边**：0 边就是 0 边，画布上的连线数 = 图里的边数。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example er_state_smoke

use gtk::prelude::*;
use puttyplus::backend::ermodel::{ErColumn, ErEdge, ErGraph, ErTable};
use puttyplus::panel::erview::ErView;

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

fn table(name: &str) -> ErTable {
    ErTable {
        schema: "public".to_string(),
        name: name.to_string(),
        columns: vec![ErColumn {
            name: "id".to_string(),
            data_type: "int".to_string(),
            is_pk: true,
            is_fk: false,
        }],
        x: 0,
        y: 0,
        w: 240,
        h: 58,
        level: 0,
    }
}

/// 带两个字段的表（字段摘要要能看到名字）。
fn table2(name: &str) -> ErTable {
    let mut t = table(name);
    t.columns.push(ErColumn {
        name: "title".to_string(),
        data_type: "text".to_string(),
        is_pk: false,
        is_fk: false,
    });
    t
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

    let view = ErView::new();
    // ⚠️ **要挂进真的窗口**：GTK 的 `grab_focus()` 要求控件在 toplevel 里
    //   （否则焦点根本无处安放）⇒ 不挂窗口就验不了「点了以后画布拿到焦点」。
    let win = gtk::Window::new();
    win.set_default_size(900, 700);
    win.set_child(Some(&view.root));
    win.present();
    pump(200);
    // 记录「打开表」的回调（验证「查看表数据」真的有事发生）。
    let opened: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    {
        let opened = std::rc::Rc::clone(&opened);
        view.set_on_open(move |t| opened.borrow_mut().push(t.to_string()));
    }

    // ---------- ① 0 张表 ----------
    view.set_graph(ErGraph::default());
    pump(120);
    check!(
        "0 张表时说明板亮着",
        view.info_visible_for_test(),
        "说明板还藏着"
    );
    let (t0, d0) = view.info_texts_for_test();
    println!("[实测] 0 表：标题 = {t0}\n[实测] 0 表：解释 = {d0}");
    check!(
        "0 张表时标题说「这个 schema 里还没有表」",
        t0.contains("还没有表"),
        format!("标题 = {t0}")
    );
    check!(
        "0 张表时解释告诉用户下一步做什么",
        !d0.is_empty() && d0.contains("表"),
        format!("解释 = {d0}")
    );

    // ---------- ② 1 张表 ----------
    view.set_graph(ErGraph {
        tables: vec![table2("demo_users")],
        edges: vec![],
    });
    pump(120);
    check!(
        "1 张表时说明板亮着",
        view.info_visible_for_test(),
        "说明板还藏着"
    );
    let (t1, d1) = view.info_texts_for_test();
    println!("[实测] 1 表：标题 = {t1}\n[实测] 1 表：解释 = {d1}");
    check!(
        "1 张表时标题里带表名",
        t1.contains("demo_users"),
        format!("标题 = {t1}")
    );
    check!(
        "1 张表的标题与 0 表时不同",
        t1 != t0,
        format!("两者都是 {t1}")
    );
    check!(
        "1 张表的解释与 0 表时不同",
        d1 != d0,
        format!("两者都是 {d1}")
    );
    check!(
        "1 张表时解释提到「没有表间关系」",
        d1.contains("关系") || t1.contains("关系"),
        format!("标题 = {t1} / 解释 = {d1}")
    );
    check!(
        "1 张表时解释**带字段摘要**（字段名 ＋ 类型 ＋ 字段数）",
        d1.contains("id") && d1.contains("int") && d1.contains("title") && d1.contains("2"),
        format!("解释 = {d1}")
    );
    check!(
        "字段摘要标出主键",
        d1.contains("主键"),
        format!("解释 = {d1}")
    );

    // ---- 单表时的两个 CTA ----
    let (cta1, cta1_vis, cta2, cta2_vis) = view.info_ctas_for_test();
    println!("[实测] 1 表：主 CTA = {cta1}（可见 {cta1_vis}） / 次 CTA = {cta2}（可见 {cta2_vis}）");
    check!(
        "有主 CTA（计划要求「没有主 CTA 的空态构造不出来」）",
        cta1_vis && !cta1.is_empty(),
        format!("主 CTA = {cta1}（可见 {cta1_vis}）")
    );
    check!(
        "单表时「查看表数据」出现",
        cta2_vis && cta2.contains("表"),
        format!("次 CTA = {cta2}（可见 {cta2_vis}）")
    );

    // ---- 主 CTA：**点了有看得见的结果** ----
    {
        let before = view.status.text().to_string();
        view.info_cta_for_test().emit_clicked();
        pump(150);
        let after = view.status.text().to_string();
        println!("[实测] 点主 CTA 前状态行 = {before}\n[实测] 点主 CTA 后状态行 = {after}");
        check!(
            "点主 CTA 后状态行**变了**（不是「点了没反应」）",
            after != before && after.contains("画布"),
            format!("前 = {before} / 后 = {after}")
        );
        check!(
            "点主 CTA 后画布拿到焦点",
            view.canvas.has_focus(),
            "焦点不在画布上"
        );
    }

    // ---- 次 CTA：**真的回调**打开那张表 ----
    {
        opened.borrow_mut().clear();
        view.info_cta2_for_test().emit_clicked();
        pump(150);
        let got = opened.borrow().clone();
        println!("[实测] 点「查看表数据」后回调收到 = {got:?}");
        check!(
            "点「查看表数据」真的打开了那张表",
            got.len() == 1 && got[0] == "demo_users",
            format!("回调收到 {got:?}")
        );
    }

    // ---------- ③ 多张表 0 条边 ----------
    view.set_graph(ErGraph {
        tables: vec![table("demo_users"), table("orders"), table("items")],
        edges: vec![],
    });
    pump(120);
    check!(
        "多表 0 边时说明板亮着",
        view.info_visible_for_test(),
        "说明板还藏着"
    );
    let (t2, d2) = view.info_texts_for_test();
    println!("[实测] 多表 0 边：标题 = {t2}\n[实测] 多表 0 边：解释 = {d2}");
    check!(
        "多表 0 边时标题说「没有找到表间关系」",
        t2.contains("关系"),
        format!("标题 = {t2}")
    );
    check!(
        "多表 0 边的标题与 0 表时不同",
        t2 != t0,
        format!("两者都是 {t2}")
    );
    check!(
        "多表 0 边的标题与 1 表时不同",
        t2 != t1,
        format!("两者都是 {t2}")
    );
    check!(
        "多表 0 边的解释与 0 表时不同",
        d2 != d0,
        format!("两者都是 {d2}")
    );
    check!(
        "多表 0 边的解释与 1 表时不同",
        d2 != d1,
        format!("两者都是 {d2}")
    );

    // ---------- ④ 有边 ⇒ 说明板藏起来 ----------
    view.set_graph(ErGraph {
        tables: vec![table("demo_users"), table("orders")],
        edges: vec![ErEdge {
            name: "orders_user_id_fkey".to_string(),
            from_table: "orders".to_string(),
            from_col: "user_id".to_string(),
            to_table: "demo_users".to_string(),
            to_col: "id".to_string(),
        }],
    });
    pump(120);
    check!(
        "有边时说明板藏起来（画布本身就是内容）",
        !view.info_visible_for_test(),
        "说明板还亮着"
    );

    // ---------- ⑤ 不生成假边 ----------
    // ⚠️ **审核验收点**：「不生成假边」⇒ 0 边时画布上就是 0 根线。
    //   回到 0 边状态，确认状态行里的边数也是 0。
    view.set_graph(ErGraph {
        tables: vec![table("a"), table("b")],
        edges: vec![],
    });
    pump(120);
    let status = view.status.text().to_string();
    println!("[实测] 0 边时的状态行 = {status}");
    check!(
        "0 边时状态行说「0 条关系」（没造假边）",
        status.contains("0 条关系"),
        format!("状态行 = {status}")
    );

    if fails.is_empty() {
        println!("er_state smoke ok");
    } else {
        println!("er_state smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
