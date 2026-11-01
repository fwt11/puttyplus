//! 数据库工作台 GUI 冒烟（8.x）：不连真库，验证**界面与纯逻辑**。
//!
//! 覆盖：
//! - 数据库窗口能建出来（左对象树 / 右结果页签 / 下 SQL 编辑器）；
//! - 对象树的懒加载：展开根 → 填模式 → 填表 → 填列，节点名与层级对；
//! - 结果表：排序、筛选、CSV 导出；
//! - 数据编辑：改一个单元格 → 生成带主键定位的 UPDATE；
//! - SQL 编辑器的语法高亮真的上了色（关键字有前景色）。
//!
//! 真库链路（认证、分页游标、取消、真实 UPDATE）由 `db_smoke` 对真库验证，
//! 这里只测不依赖网络的部分。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_ui_smoke
//!   xvfb-run -a cargo run --example db_ui_smoke -- --hold   # 停留 20 秒供截图

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbmeta::{self, ColType};
use puttyplus::panel::worker::{DbMetaPayload, DbMetaReq};
use puttyplus::panel::{self, SftpPanel, dbgrid, dbtree};
use puttyplus::theme::{self, Theme};

/// 递归遍历树（TreeStore 的 `iter_next` 只走同级，子节点要 `iter_children`）。
fn walk(
    model: &gtk::TreeModel,
    iter: gtk::TreeIter,
    names: &mut Vec<String>,
    details: &mut Vec<String>,
) {
    names.push(model.get::<String>(&iter, 1));
    details.push(model.get::<String>(&iter, 2));
    // 注意：`while let Some(mut c) = child` 每轮都会把 c 重置回**第一个**子节点
    // （child 本身没被更新），于是永远转圈——树里有多少子节点就转多少遍。
    // 正确写法：把 iter_next 作用在同一个 iter 上。
    if let Some(mut c) = model.iter_children(Some(&iter)) {
        loop {
            walk(model, c.clone(), names, details);
            if !model.iter_next(&mut c) {
                break;
            }
        }
    }
}

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

/// 退出时把临时 ini 删掉（别留垃圾）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");
    let th = if args.iter().any(|a| a == "light") {
        Theme::Light
    } else {
        Theme::Dark
    };

    gtk::init().expect("gtk init");
    theme::apply(th);

    // **别碰用户的真实配置**：数据库连接配置存在 `databases.ini`，冒烟会
    // 触发保存（persist），不隔离的话会往用户配置里塞一堆 `user@host:port`
    // 的假连接（之前就塞出过 `@:0` 这种空会话键的组）。
    let db_ini = std::env::temp_dir().join(format!(
        "pp-db-ui-smoke-{}.ini",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_ini);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑
    unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
    let _guard = IniGuard(db_ini.clone());

    eprintln!("P1 tree begin");
    /* ---- 对象浏览器：懒加载填充 ---- */
    let tree = dbtree::DbTree::new();
    // 展开根节点应立刻触发第一次元数据请求。
    // 回归点：GTK4 里**没有子节点的行 expand_row 不发 row-expanded**，
    // 所以根节点必须先挂一个占位子节点，否则懒加载永远触发不了
    // （症状就是"连上了但树里什么都没有"）。
    let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    {
        let reqs = requests.clone();
        tree.set_request_handler(move |req| {
            reqs.borrow_mut().push(format!("{req:?}"));
        });
    }
    tree.reset("postgres@127.0.0.1:5432", "PostgreSQL");
    pump(120);
    assert!(
        requests.borrow().iter().any(|r| r.contains("Schemas")),
        "连接成功后没有发出模式列表请求（懒加载没触发）：{:?}",
        requests.borrow()
    );
    tree.fill(
        &DbMetaReq::Schemas,
        &DbMetaPayload::Schemas(vec!["public".to_string()]),
    );
    tree.fill(
        &DbMetaReq::Tables {
            schema: "public".to_string(),
        },
        &DbMetaPayload::Tables(vec![
            puttyplus::backend::db::DbTable {
                schema: "public".into(),
                name: "users".into(),
                is_view: false,
            },
            puttyplus::backend::db::DbTable {
                schema: "public".into(),
                name: "v_active".into(),
                is_view: true,
            },
        ]),
    );
    tree.fill(
        &DbMetaReq::Columns {
            schema: "public".to_string(),
            table: "users".to_string(),
        },
        &DbMetaPayload::Columns(vec![
            puttyplus::backend::db::DbColumn {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                is_pk: true,
                ..Default::default()
            },
            puttyplus::backend::db::DbColumn {
                name: "name".into(),
                data_type: "text".into(),
                nullable: true,
                is_pk: false,
                ..Default::default()
            },
        ]),
    );
    pump(120);

    let model = tree.view.model().expect("树没有 model");
    // 根 → 模式 → 表 → 列，一共 6 行（树是分层的，要递归走）
    let mut names = Vec::new();
    let mut details = Vec::new();
    if let Some(mut i) = model.iter_first() {
        loop {
            walk(&model, i.clone(), &mut names, &mut details);
            if !model.iter_next(&mut i) {
                break;
            }
        }
    }
    println!("tree rows: {names:?}");
    // 根 → 模式 → 表 → 列，另外每个"还没查子节点"的节点下挂一个占位子节点
    // （v_active 还没查列 → 它的占位子节点还在；users 的占位已被真列替换）
    assert_eq!(names.len(), 7, "对象树的行数不对：{names:?}");
    assert_eq!(names[0], "postgres@127.0.0.1:5432");
    assert_eq!(names[1], "public");
    assert_eq!(names[2], "users");
    // 深度优先：users 的子节点（列）排在兄弟节点 v_active 前面
    assert_eq!(names[3], "id");
    assert_eq!(names[4], "name");
    assert_eq!(names[5], "v_active");
    // v_active 还没查列 → 占位子节点"载入中…"留在它下面
    assert_eq!(names[6], "载入中…");
    // 列的说明里要带上主键标记
    assert!(
        details[3].contains("主键"),
        "列说明里没有主键标记：{}",
        details[3]
    );
    assert!(
        details[4].contains("text"),
        "列说明里没有类型：{}",
        details[4]
    );

    eprintln!("P2 grid begin");
    /* ---- 结果表：排序 / 筛选 / 导出 ---- */
    let grid = dbgrid::DbGrid::new();
    grid.set_data(dbgrid::GridData {
        columns: vec!["id".into(), "name".into()],
        types: vec![ColType::Int, ColType::Text],
        rows: vec![
            vec![Some("2".into()), Some("乙".into())],
            vec![Some("10".into()), Some("甲".into())],
            vec![None, Some("丙".into())],
        ],
        pk: vec![0],
    });
    pump(50);
    assert_eq!(grid.row_count(), 3);
    // 默认顺序 = 原始顺序
    assert_eq!(grid.visible_values()[0][1], "乙");

    // 点列头排序：数字列按数值比（2 < 10），NULL 当最大值 → 升序在最后
    grid.toggle_sort(0);
    let sorted = grid.visible_values();
    assert_eq!(sorted[0][0], "2", "排序后第一行不是 2：{sorted:?}");
    assert_eq!(sorted[1][0], "10");
    assert_eq!(sorted[2][0], "NULL", "NULL 没有排在最后：{sorted:?}");

    // 再点一次换方向：NULL 跟着变成最大 → 到最前（跟 SQL 一个语义）
    grid.toggle_sort(0);
    let desc = grid.visible_values();
    assert_eq!(desc[0][0], "NULL", "降序不对：{desc:?}");
    assert_eq!(desc[1][0], "10", "降序不对：{desc:?}");
    assert_eq!(desc[2][0], "2", "降序不对：{desc:?}");

    // 筛选：包含关键字
    grid.set_filter("甲");
    assert_eq!(grid.visible_count(), 1);
    assert_eq!(grid.visible_values()[0][1], "甲");
    grid.set_filter("");
    assert_eq!(grid.visible_count(), 3);

    // CSV 导出：BOM + 表头 + 转义
    let csv = grid.to_csv(true);
    assert!(csv.starts_with('\u{feff}'), "CSV 少了 BOM");
    assert!(csv.contains("id,name"), "CSV 表头不对：{csv}");
    assert!(csv.contains("NULL"), "CSV 里 NULL 没写出来：{csv}");

    eprintln!("P2.5 阶段 46：列头 + 多选");
    /* ---- 阶段 46：dbx 风格列头（列名 + 类型小字）+ 多选 ---- */
    let cols = grid.view.columns();
    assert_eq!(cols.len(), 2, "列数不对：{}", cols.len());
    // ⚠️ **`title` 属性还得留着**：导出、过滤条件编辑器、列类型映射都靠
    // `col.title()` 拿列名。列头换成 widget 之后这里要是空了，那些地方就全瞎。
    assert_eq!(cols[0].title(), "id", "列标题丢了：{}", cols[0].title());
    assert_eq!(cols[1].title(), "name");
    // 列头换成了自定义 widget（dbx 的「列名 + 类型小字 + ⋮」）
    let Some(header) = cols[0].widget() else {
        panic!("列头没有自定义 widget");
    };
    let hb = header
        .downcast_ref::<gtk::Box>()
        .expect("列头不是 Box")
        .clone();
    // 列头里：竖向 Box（列名 + 类型小字）+ ⋮ 按钮
    let vbox = hb
        .first_child()
        .and_then(|c| c.downcast_ref::<gtk::Box>().cloned())
        .expect("列头里没有竖向 Box");
    let mut labels: Vec<String> = Vec::new();
    let mut child = vbox.first_child();
    while let Some(c) = child {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            labels.push(l.text().to_string());
        }
        child = c.next_sibling();
    }
    assert_eq!(
        labels,
        vec!["id".to_string(), "整数".to_string()],
        "列头内容不对（要「列名 + 类型小字」）：{labels:?}"
    );
    // 类型小字是 dim-label（小字弱化）
    let type_lbl = vbox
        .last_child()
        .and_then(|c| c.downcast_ref::<gtk::Label>().cloned())
        .expect("没有类型小字");
    assert!(
        type_lbl.css_classes().contains(&gtk::glib::GString::from("dim-label")),
        "类型小字没有用 dim-label 弱化"
    );

    // 多选：框选两行 → 选中的是**两行**（不是一行）
    grid.set_multi_select(true);
    assert_eq!(
        grid.view.selection().mode(),
        gtk::SelectionMode::Multiple,
        "结果网格没开多选"
    );
    let sel = grid.view.selection();
    sel.select_path(&gtk::TreePath::from_indices(&[0]));
    sel.select_path(&gtk::TreePath::from_indices(&[1]));
    pump(20);
    assert_eq!(
        grid.selected_display_rows().len(),
        2,
        "多选没生效：{:?}",
        grid.selected_display_rows()
    );

    eprintln!("P3 edit begin");
    /* ---- 数据编辑：改单元格 → UPDATE ---- */
    // 切回升序：显示行 ↔ 原始行的对应关系随排序变，
    // 编辑"第 0 个显示行"之前得先知道它到底是哪一行
    grid.toggle_sort(0);
    assert_eq!(grid.visible_values()[0][0], "2", "没切回升序");
    grid.set_editable(true);
    // 升序下第 0 个显示行就是原始第 0 行（id=2）
    grid.set_edit(0, 1, "乙改");
    assert!(grid.has_edits());
    let edits = grid.pending_edits();
    assert_eq!(edits.len(), 1);

    let sqls = dbmeta::build_updates(
        puttyplus::backend::db::DbProto::Postgres,
        "public",
        "users",
        &grid.columns(),
        &grid.types(),
        &grid.pk(),
        &grid.original_rows(),
        &edits,
    )
    .expect("拼 UPDATE");
    assert_eq!(sqls.len(), 1, "{sqls:?}");
    assert!(
        sqls[0].contains("WHERE \"id\" = '2'"),
        "UPDATE 没有用主键定位：{}",
        sqls[0]
    );
    assert!(sqls[0].contains("'乙改'"), "{}", sqls[0]);

    // 放弃改动 → 表格回到原值
    grid.discard_edits();
    assert!(!grid.has_edits());
    assert_eq!(grid.visible_values()[0][1], "乙");

    eprintln!("P4 window begin");
    /* ---- 数据库窗口整体能建出来 ---- */
    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("db ui smoke"));
    win.set_default_size(1100, 720);
    win.set_child(Some(panel.widget()));
    win.present();
    // 阶段 12：连接页的子标签（真实运行时由 tabs.rs 建）
    *panel.st.sub_tabs.borrow_mut() = Some(panel::subtabs::SubTabs::new());
    pump(200);

    panel::on_open_db(&panel.st);
    pump(400);
    let db_open = panel.st.db_ui.borrow().is_some();
    println!("db window opened: {db_open}");
    assert!(db_open, "数据库窗口没打开");
    // 阶段 12：数据库工作台应该在连接页的子标签里（不是独立弹窗）
    let in_tab = panel
        .st
        .sub_tabs()
        .map(|s| s.has(panel.st.db_ui.borrow().as_ref().unwrap().tab_id.get()))
        .unwrap_or(false);
    assert!(in_tab, "数据库工作台没挂到子标签上");
    println!("db in sub tab: {in_tab}");

    eprintln!("P4.0 阶段 47：网格右键菜单（详情 / 转置 / 批量修改）");
    /* ---- 阶段 47：结果网格右键菜单（**离线**：灌假数据，不连库）---- */
    {
        let tidx = puttyplus::panel::dbui::make_test_result_tab(
            &panel.st,
            "离线测试",
            &["id", "name", "age"],
            &[ColType::Int, ColType::Text, ColType::Int],
            vec![
                vec![Some("1".into()), Some("张三".into()), Some("31".into())],
                vec![Some("2".into()), Some("李四".into()), Some("25".into())],
                vec![Some("3".into()), Some("王五".into()), Some("41".into())],
            ],
        )
        .expect("应当能离线造一个结果页签");
        pump(200);

        // 单元格详情
        assert!(
            puttyplus::panel::dbui::grid_menu_action_now(&panel.st, tidx, "cell"),
            "单元格详情没触发"
        );
        let d = puttyplus::panel::dbui::grid_detail_now(&panel.st);
        println!("单元格详情：{d:?}");
        assert!(!d.is_empty(), "单元格详情是空的");
        assert!(d.iter().any(|(k, _)| k == "列名"), "{d:?}");
        assert!(d.iter().any(|(k, _)| k == "当前值"), "{d:?}");
        // 值对上：钩子查的是第 0 行第 0 列（id 列）→ 值是 "1"
        assert!(
            d.iter().any(|(k, v)| k == "当前值" && v == "1"),
            "单元格详情里的值不对：{d:?}"
        );
        assert!(
            d.iter().any(|(k, v)| k == "列名" && v == "id"),
            "单元格详情里的列名不对：{d:?}"
        );

        // 行详情：每列一行（3 列 → 3 行）
        assert!(
            puttyplus::panel::dbui::grid_menu_action_now(&panel.st, tidx, "row"),
            "行详情没触发"
        );
        let d = puttyplus::panel::dbui::grid_detail_now(&panel.st);
        println!("行详情：{} 项", d.len());
        assert_eq!(d.len(), 3, "行详情应当每列一行：{d:?}");
        assert!(d.iter().any(|(k, _)| k == "name"), "{d:?}");

        // 列详情：列名 / 类型 / 行数 / NULL 数 …
        assert!(
            puttyplus::panel::dbui::grid_menu_action_now(&panel.st, tidx, "col"),
            "列详情没触发"
        );
        let d = puttyplus::panel::dbui::grid_detail_now(&panel.st);
        println!("列详情：{d:?}");
        assert!(d.iter().any(|(k, _)| k == "类型"), "{d:?}");
        assert!(d.iter().any(|(k, v)| k == "行数" && v == "3"), "{d:?}");

        // 转置查看：开独立窗口（不崩就成）
        assert!(
            puttyplus::panel::dbui::grid_menu_action_now(&panel.st, tidx, "transpose"),
            "转置查看没触发"
        );

        // 批量修改对话框能打开
        assert!(
            puttyplus::panel::dbui::grid_menu_action_now(&panel.st, tidx, "batch"),
            "批量修改没触发"
        );
    }

    eprintln!("P4.1 sub tab begin");
    /* ---- 阶段 12：子标签的关 / 重开 ---- */
    {
        let sub = panel.st.sub_tabs().expect("没有子标签容器");
        let tab_id = panel.st.db_ui.borrow().as_ref().unwrap().tab_id.get();

        // 用 × 关掉数据库那一页
        assert!(sub.close_page(tab_id), "关数据库页签失败");
        assert!(!sub.has(tab_id), "页签还在 notebook 里");
        // 再点「数据库」要能重新挂回来（否则用户看到的是"点了没反应"）
        panel::on_open_db(&panel.st);
        pump(200);
        let new_id = panel.st.db_ui.borrow().as_ref().unwrap().tab_id.get();
        assert!(sub.has(new_id), "重新打开后数据库页签没挂回来");
        println!("db tab re-added: {new_id}");

        // 文件编辑器也应该是子标签
        let f = std::env::temp_dir().join(format!("pp-smoke-{}.txt", std::process::id()));
        std::fs::write(&f, "hello\n").unwrap();
        let path = f.to_string_lossy().to_string();
        puttyplus::panel::fileedit::open_local(&panel.st, &path, false);
        pump(300);
        let edits = panel.st.file_edits.borrow();
        assert_eq!(edits.len(), 1, "编辑器没建起来");
        assert!(
            sub.has(edits[0].tab_id.get()),
            "文件编辑器没挂到子标签上"
        );
        println!("file editor in sub tab: {}", edits[0].tab_id.get());
        drop(edits);
        let _ = std::fs::remove_file(&f);
    }

    eprintln!("P5 highlight begin");
    // SQL 编辑器的高亮：写一句 SELECT，关键字应该上了色
    if let Some(ui) = panel.st.db_ui.borrow().as_ref() {
        let buf = ui.sql.buffer();
        buf.set_text("SELECT id FROM users WHERE name = '张三'");
        pump(500); // 等防抖 + 重画
        let start = buf.start_iter();
        let end = buf.end_iter();
        let text = buf.text(&start, &end, false).to_string();
        assert_eq!(text, "SELECT id FROM users WHERE name = '张三'");
        let tag_table = buf.tag_table();
        println!("sql tag table size: {}", tag_table.size());
        assert!(tag_table.size() > 1, "SQL 高亮一个 tag 都没建");
        // "SELECT" 是关键字：颜色应该读得回来（跟文件编辑器的回归一个套路：
        // 「语言识别出来了」测不出来，「颜色真的贴上去了」才测得出来）
        let it = buf.iter_at_offset(0);
        let colors_at: Vec<Option<String>> = it
            .tags()
            .into_iter()
            .map(|t| t.foreground_rgba().map(|c| format!("{:?}", c)))
            .collect();
        println!("colors at 0: {colors_at:?}");
        assert!(
            colors_at.iter().any(|c| c.is_some()),
            "SQL 关键字没上色：{colors_at:?}"
        );
    }

    /* ---- 走 app 的真实路径：on_connected → 对象树 ---- */
    {
        let st = Rc::clone(&panel.st);
        let rows = |tag: &str| {
            let tv = puttyplus::panel::dbui::tree_view(&st).expect("没有对象树");
            let model = tv.model().expect("树没有 model");
            let model: gtk::TreeModel = model;
            let mut names = Vec::new();
            let mut details = Vec::new();
            // 树是分层的：只看顶层会把子节点（模式/表）漏掉
            if let Some(i) = model.iter_first() {
                walk(&model, i, &mut names, &mut details);
            }
            println!("[{tag}] 树里的行: {names:?} / {details:?}");
            names
        };
        // 模拟 worker 回包：连接成功
        puttyplus::panel::dbui::on_connected(&st, Ok("postgres@127.0.0.1:5432".to_string()));
        pump(200);
        let n1 = rows("连接后");
        assert!(!n1.is_empty(), "连接成功后对象树是空的（连根节点都没有）");

        // 模拟 worker 回包：模式列表（pending 里没有这个 id，fill 不该发生，
        // 但也不能把已有的行弄没）
        puttyplus::panel::dbui::on_meta(
            &st,
            4242,
            DbMetaReq::Schemas,
            Ok(DbMetaPayload::Schemas(vec!["public".to_string()])),
        );
        pump(200);
        let n2 = rows("元数据回包后");
        assert!(!n2.is_empty(), "元数据回包后树被清空了");

        // 模拟 worker 回包：模式列表（**真实 pending id**——on_connected 里
        // 自动展开根节点时已经把 Schemas 请求登记进 pending，id 是第一个）
        puttyplus::panel::dbui::on_meta(
            &st,
            1,
            DbMetaReq::Schemas,
            Ok(DbMetaPayload::Schemas(vec!["public".to_string()])),
        );
        pump(200);
        let n3 = rows("模式回包后");
        assert!(!n3.is_empty(), "模式回包后树被清空了");
        assert!(
            n3.len() >= 2,
            "模式回包后树里只有根节点（子节点没挂上）：{n3:?}"
        );
    }

    println!("db_ui_smoke: OK");
    if hold {
        pump(20_000);
    }
}
