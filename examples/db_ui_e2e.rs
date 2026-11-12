//! 数据库工作台**端到端**冒烟：真 SSH + 真数据库 + 真 worker + 真事件分发。
//!
//! `db_ui_smoke` 只测纯 UI 路径（直接调 `on_connected` / `on_meta`），
//! 测不到「worker 回包 → 事件分发 → 对象树」这条链。这个例子把整条链跑通：
//!
//!   1. 用 app 自己的 `SftpPanel`（含真 worker 线程）连上跳板机；
//!   2. 打开数据库窗口，填连接表单，点「连接」（走 `do_connect` 真路径）；
//!   3. 断言对象树逐层展开（连接 → 模式 → 表）；
//!   4. 双击一张表，断言数据页签里**真的有值**。
//!
//! 它守的是三个曾经把界面坑成"连上了却什么都没有"的 bug：
//! - TreeView 的列没绑 `text` 属性 → 单元格全是空白；
//! - GTK4 里没有子节点的行 `expand_row` 不发信号 → 懒加载永远不触发；
//! - `open_table` 登记 pending 用的 id 与发给 worker 的 id 不一致 →
//!   表的列信息对不上，数据页签永远是空的。
//!
//! 用法（srv-01 上的 PG/MySQL 容器）：
//!   xvfb-run -a cargo run --example db_ui_e2e -- \
//!       10.0.0.9 22 root 127.0.0.1 5432 postgres postgres db-password pg
//!
//! 形参：<ssh_host> <ssh_port> <ssh_user> <db_host> <db_port> <db_user> <dbname> <db_pass> [pg|mysql]

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::dbexport::ExportFmt;
use puttyplus::panel::{self, PanelState, SftpPanel, dbui, dbtransfer};

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

/// 递归打印树（**按路径逐层走**）。
///
/// **为什么不能用 `iter_next` 深遍历**：在子行上调 `iter_children` 会让**上层**
/// 的 iter 失效，递归两层后顶层 `iter_next` 直接返回 false，树只走了一半
/// （看着像"数据没加载出来"，其实是遍历器坏了）。
/// 可靠写法是按路径走：`TreePath::from_indices` + `append_index`，
/// 每步从根重新定位。
fn walk(model: &gtk::TreeModel, path: &gtk::TreePath, depth: usize, out: &mut Vec<String>) {
    let Some(iter) = model.iter(path) else {
        return;
    };
    let kind = model.get::<String>(&iter, 0);
    let name = model.get::<String>(&iter, 1);
    let detail = model.get::<String>(&iter, 2);
    out.push(format!("{}{} | {} | {}", "  ".repeat(depth), kind, name, detail));
    let mut j: i32 = 0;
    loop {
        let mut child = path.clone();
        child.append_index(j);
        if model.iter(&child).is_none() {
            break;
        }
        walk(model, &child, depth + 1, out);
        j += 1;
    }
}

/// 树里的所有行（每行一行文本）。
fn tree_rows(st: &Rc<PanelState>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(tv) = dbui::tree_view(st) {
        if let Some(model) = tv.model() {
            let mut i: i32 = 0;
            loop {
                let path = gtk::TreePath::from_indices(&[i]);
                if model.iter(&path).is_none() {
                    break;
                }
                walk(&model, &path, 0, &mut out);
                i += 1;
            }
        }
    }
    out
}

/// 展开第一个模式（模拟用户点展开箭头）→ 把表、例程、触发器都查出来。
///
/// 树是懒加载的：展开模式会同时发「表」「例程」「触发器」三个请求，
/// 回来的顺序就是它们在树上的顺序。
fn expand_first_schema(st: &Rc<PanelState>) {
    if let Some(tv) = dbui::tree_view(st) {
        if let Some(model) = tv.model() {
            if let Some(root) = model.iter_first() {
                if let Some(child) = model.iter_children(Some(&root)) {
                    let path = model.path(&child);
                    tv.expand_row(&path, false);
                    pump(3000);
                }
            }
        }
    }
}

/// 在树里找第一行指定"类"的节点，返回它的路径。
fn find_node(st: &Rc<PanelState>, kinds: &[&str]) -> Option<gtk::TreePath> {
    let tv = dbui::tree_view(st)?;
    let model = tv.model()?;
    let root = model.iter_first()?;
    let mut found = None;
    for depth in 0..3 {
        let mut iters: Vec<gtk::TreeIter> = Vec::new();
        if depth == 0 {
            iters.push(root.clone());
        } else {
            // 收集上一层的所有孩子
            let mut level = vec![root.clone()];
            for _ in 1..depth {
                let mut next = Vec::new();
                for p in &level {
                    if let Some(mut c) = model.iter_children(Some(p)) {
                        loop {
                            next.push(c.clone());
                            if !model.iter_next(&mut c) {
                                break;
                            }
                        }
                    }
                }
                level = next;
            }
            for p in &level {
                if let Some(mut c) = model.iter_children(Some(p)) {
                    loop {
                        iters.push(c.clone());
                        if !model.iter_next(&mut c) {
                            break;
                        }
                    }
                }
            }
        }
        for it in &iters {
            let kind = model.get::<String>(it, 0);
            if kinds.contains(&kind.as_str()) {
                found = Some(model.path(it));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    found
}

/// 双击树里的某一行（等价于用户双击）。
fn activate(st: &Rc<PanelState>, path: &gtk::TreePath) {
    if let Some(tv) = dbui::tree_view(st) {
        tv.emit_row_activated(path, None);
        pump(2500);
    }
}

/// 等一次查询回来（状态栏出现「查询完成」）。
fn wait_query_done(st: &Rc<PanelState>) -> String {
    for _ in 0..50 {
        pump(300);
        let s = dbui::status_text(st);
        if s.contains("查询完成") {
            return s;
        }
    }
    dbui::status_text(st)
}

/// 等一次查询完成，**且状态跟上次不一样**。
///
/// ⚠️ **为什么不能只看"有没有查询完成"**：上一次查询留下的
/// 「查询完成：3 行」还挂在状态栏上，光看关键词会**立刻返回**，
/// 拿到的是**上一次的旧结果**——网络慢的时候必踩（看着像"过滤没生效"，
/// 其实是没等到新回包）。所以要跟发查询**之前**的状态比一比。
fn wait_query_done_after(st: &Rc<PanelState>, prev: &str) -> String {
    for _ in 0..150 {
        pump(300);
        let s = dbui::status_text(st);
        if s.contains("查询完成") && s != prev {
            return s;
        }
    }
    dbui::status_text(st)
}

/// 探针例程：建 / 删（PG 用函数，MySQL 用存储过程——MySQL 建函数要 SUPER
/// 权限或 `log_bin_trust_function_creators`，存储过程没这个限制）。
fn probe_routine(proto: &str, create: bool) -> String {
    if proto == "mysql" {
        if create {
            "CREATE PROCEDURE pp_probe_proc() BEGIN SELECT 1; END".to_string()
        } else {
            "DROP PROCEDURE IF EXISTS pp_probe_proc".to_string()
        }
    } else if create {
        "CREATE OR REPLACE FUNCTION pp_probe_fn() RETURNS integer AS $$ SELECT 1 $$ LANGUAGE sql"
            .to_string()
    } else {
        "DROP FUNCTION IF EXISTS pp_probe_fn()".to_string()
    }
}

/// 截图（在同一个 X display 里调 scrot，不用另开一个显示）。
fn shot(path: &str) {
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}

/// 等"提交后刷新表格"的查询回来（状态栏出现「查询完成：N 行」）。
///
/// **为什么不能只看网格的行数**：标记删除是**本地**操作——网格里立刻就
/// 少了一行，但数据库还没删。只看行数会以为"删成功了"，其实语句还没发。
/// 状态栏那行文案是刷新查询回来时才写的，才是"库里的真实行数"。
fn wait_query_rows(st: &Rc<PanelState>, want: usize) -> String {
    let needle = format!("查询完成：{want} 行");
    // ⚠️ **上限 150 次（45 秒）**：远端网络慢的时候一次查询要十几秒
    for _ in 0..150 {
        pump(300);
        let s = dbui::status_text(st);
        if s.contains(&needle) {
            return s;
        }
    }
    dbui::status_text(st)
}

/// 等一个**标题匹配**的顶层窗口出现（返回 `None` = 超时没出现）。
///
/// **用来验证弹框**：GTK 的对话框是非阻塞的（`present()` 不是 `run()`），
/// 所以主循环照跑，测试能在 pump 里把它捞出来。
fn wait_dialog(title: &str) -> Option<gtk::Dialog> {
    for _ in 0..50 {
        for w in gtk::Window::list_toplevels() {
            let Some(win) = w.downcast_ref::<gtk::Window>() else {
                continue;
            };
            if win.title().as_deref() != Some(title) {
                continue;
            }
            // downcast 成 Dialog（`response()` 是 Dialog 才有的）
            if let Ok(d) = win.clone().downcast::<gtk::Dialog>() {
                return Some(d);
            }
        }
        pump(100);
    }
    None
}

/// 把窗口里的第一个输入框填上字（找不到就返回 false）。
fn set_first_entry_text(win: &gtk::Window, text: &str) -> bool {
    fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Entry>) {
        if let Ok(e) = w.clone().downcast::<gtk::Entry>() {
            out.push(e);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    let mut entries = Vec::new();
    walk(win.upcast_ref::<gtk::Widget>(), &mut entries);
    match entries.first() {
        Some(e) => {
            e.set_text(text);
            true
        }
        None => false,
    }
}

/// 在窗口里递归找第一个某类型的控件（找不到返回 None）。
fn find_widgets<T>(win: &gtk::Window) -> Vec<T>
where
    T: Clone + IsA<gtk::Widget>,
{
    fn walk<T>(w: &gtk::Widget, out: &mut Vec<T>)
    where
        T: Clone + IsA<gtk::Widget>,
    {
        if let Ok(t) = w.clone().downcast::<T>() {
            out.push(t);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    let mut out = Vec::new();
    walk(win.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 对话框里的列清单。
fn find_listbox(win: &gtk::Window) -> Option<gtk::ListBox> {
    find_widgets::<gtk::ListBox>(win).into_iter().next()
}

/// 对话框里的勾选框（「显示这一列」）。
fn find_checkbutton(win: &gtk::Window) -> Option<gtk::CheckButton> {
    find_widgets::<gtk::CheckButton>(win).into_iter().next()
}

/// 对话框里文案匹配的按钮。
///
/// **注意**：菜单项是 `crate::menu::item` 建的——按钮的 child 是一层
/// Box（图标 + 文字 Label），**没有**设 button 的 `label` 属性，
/// 所以光看 `b.label()` 会找不到（阶段 35 把菜单改成自绘后踩过）。
/// 这里两种都查：`label` 属性，或者后代里的那个文字 Label。
fn find_button_by_label(win: &gtk::Window, label: &str) -> Option<gtk::Button> {
    find_widgets::<gtk::Button>(win)
        .into_iter()
        .find(|b| button_text(b).as_deref() == Some(label))
}

/// 按钮上的文字（`label` 属性，或后代 Label 的文字）。
fn button_text(b: &gtk::Button) -> Option<String> {
    if let Some(l) = b.label() {
        return Some(l.to_string());
    }
    let mut found = None;
    fn walk(w: &gtk::Widget, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            *out = Some(l.text().to_string());
            return;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            if out.is_some() {
                return;
            }
            c = next;
        }
    }
    walk(b.upcast_ref::<gtk::Widget>(), &mut found);
    found
}

/// 最后一个结果页签第一行第一列的值（用来读 `SELECT count(*)` 的结果）。
fn count_of_last_grid(st: &Rc<PanelState>) -> String {
    let grids = dbui::grid_rows(st);
    grids
        .last()
        .and_then(|(_, rows)| rows.first())
        .and_then(|r| r.first())
        .cloned()
        .unwrap_or_default()
}

/// 退出时把临时 ini 删掉（别留垃圾）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |i: usize, d: &str| -> String { args.get(i).cloned().unwrap_or_else(|| d.to_string()) };
    let ssh_host = get(0, "127.0.0.1");
    let ssh_port: i32 = get(1, "22").parse().unwrap_or(22);
    let ssh_user = get(2, "root");
    let db_host = get(3, "127.0.0.1");
    let db_port = get(4, "5432");
    let db_user = get(5, "postgres");
    let dbname = get(6, "postgres");
    let db_pass = get(7, "");
    let proto = get(8, "pg");
    let hold = args.iter().any(|a| a == "--hold");

    gtk::init().expect("gtk init");
    // 装上主题（跟真实程序一样）：不然 ER 图的表框没有边框和表头底色，
    // 截图看不出真实效果
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    // **别碰用户的真实配置**：连上库会 persist（写 `databases.ini`），
    // 不隔离的话每次跑 e2e 都在用户配置里塞一条连接。
    let db_ini = std::env::temp_dir().join(format!(
        "pp-db-ui-e2e-{}-{}.ini",
        std::process::id(),
        proto
    ));
    let _ = std::fs::remove_file(&db_ini);
    // set_var 在新 Rust 里是 unsafe：这里只有一个线程在跑
    unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
    let _guard = IniGuard(db_ini.clone());

    // 设置也隔离（阶段 22）：工作台会记「上次用的数据库连接」，
    // 不隔离的话用户设置里会多一条 e2e 的连接名
    let set_ini = std::env::temp_dir().join(format!(
        "pp-db-ui-e2e-settings-{}-{}.ini",
        std::process::id(),
        proto
    ));
    let _ = std::fs::remove_file(&set_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &set_ini) };
    let _set_guard = IniGuard(set_ini.clone());

    // 执行历史也隔离（11.2）：跑 SQL 会写 `history.ini`，不隔离的话用户
    // 的历史里会塞满 e2e 的探针语句
    let hist_ini = std::env::temp_dir().join(format!(
        "pp-db-ui-e2e-hist-{}-{}.ini",
        std::process::id(),
        proto
    ));
    let _ = std::fs::remove_file(&hist_ini);
    unsafe { std::env::set_var("PUTTYPLUS_HIST_INI", &hist_ini) };
    let _hist_guard = IniGuard(hist_ini.clone());

    // 列显示设置也隔离（阶段 20）：改列显示会写 `columns.ini`，
    // 不隔离的话用户的配置里会留下一条 e2e 的列设置
    let cols_ini = std::env::temp_dir().join(format!(
        "pp-db-ui-e2e-cols-{}-{}.ini",
        std::process::id(),
        proto
    ));
    let _ = std::fs::remove_file(&cols_ini);
    unsafe { std::env::set_var("PUTTYPLUS_COLS_INI", &cols_ini) };
    let _cols_guard = IniGuard(cols_ini.clone());

    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = ssh_host.clone();
    opts.port = ssh_port;
    opts.user = ssh_user.clone();
    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);

    // ---- 阶段 97 的真实布局：连接页 = 子标签区；终端子页 = 横向 Paned
    //      （左文件面板 / 右终端）----
    //
    // 真实运行时这段在 `tabs.rs::open` 里。这里照抄一份，e2e 的截图才跟
    // 用户实际看到的形状一致（数据库工作台是**子标签**，不是弹窗）。
    let sub = puttyplus::panel::subtabs::SubTabs::new();
    {
        // 第一个子页是终端（这里没有真终端，用占位页占住位置）
        let term = gtk::Label::new(Some("终端（e2e 占位）"));
        term.set_margin_top(20);
        // 终端子页 = 横向 Paned：左＝文件面板，右＝终端占位
        let term_page = gtk::Paned::new(gtk::Orientation::Horizontal);
        term_page.set_start_child(Some(panel.widget()));
        term_page.set_end_child(Some(term.upcast_ref::<gtk::Widget>()));
        term_page.set_position(340);
        sub.add_page(
            "终端",
            term_page.upcast::<gtk::Widget>(),
            None,
        );
    }
    *st.sub_tabs.borrow_mut() = Some(sub.clone());
    // 用 `root()`（notebook + 尾部的「+」按钮），不是裸的 notebook：
    // notebook 现在挂在 root 里，直接塞进别的容器会因为"已有父级"被拒绝，
    // 结果整个内容区高度为 0（对象树一行的可见范围都查不到）。
    let win = gtk::Window::new();
    win.set_title(Some("db ui e2e"));
    win.set_default_size(1500, 900);
    win.set_child(Some(sub.root()));
    win.present();
    pump(400);

    // 等 SSH 连上
    // ⚠️ **等 60 秒**：远端 SSH + 真库，网络抖的时候 20 秒不够
    let mut waited = 0;
    while !st.conn_ok.get() && waited < 60_000 {
        pump(200);
        waited += 200;
    }
    assert!(st.conn_ok.get(), "SSH 没连上 {ssh_user}@{ssh_host}:{ssh_port}");
    println!("[1] SSH 已连接");

    // 打开数据库窗口
    panel::on_open_db(&st);
    pump(300);

    // 阶段 13：工作台里没有连接表单了，连接是"当前连接"这一条。
    // 测试直接给一条临时连接（**不落盘**，测试不该往用户配置里塞连接）。
    let p = if proto == "mysql" { "mysql" } else { "pg" };

    // ---- 「服务器要密码」这一段单独走一遍（2026-10 修的真坑）----
    //
    // **为什么非验不可**：用户实际遇到的报错是 PG 驱动那句看不懂的
    // "invalid configuration"。修完必须是"弹个框问密码"，光有后端单测
    // 不够——弹框、填密码、自动重连这一整条链路都得在 GUI 里真走一遍。
    if !db_pass.is_empty() {
        // **故意不带密码**连一次
        dbui::set_conn_now(&st, p, &db_host, &db_port, &db_user, &dbname, "");
        pump(200);
        dbui::connect_now(&st);
        let dlg = wait_dialog("数据库需要密码")
            .expect("服务器要密码时必须弹密码框（不能只甩一句 invalid configuration）");
        set_first_entry_text(dlg.upcast_ref::<gtk::Window>(), &db_pass);
        shot("/tmp/db_need_password.png");
        dlg.response(gtk::ResponseType::Ok);
        // 填完密码应当自动重连并连上
        let mut waited = 0;
        while waited < 20_000 {
            pump(300);
            waited += 300;
            if dbui::status_text(&st).starts_with("已连接") {
                break;
            }
        }
        let s = dbui::status_text(&st);
        println!("[1.2] 要密码 → 弹框 → 填密码 → 自动重连：{s}");
        assert!(s.starts_with("已连接"), "填了密码还没连上：{s}");
        // 这条路径（要密码 → 弹框 → 填完自动重连）也必须把对象树填上
        println!("[1.3] 要密码路径下的对象树：{}", dbui::tree_state(&st).join(" / "));
    }

    dbui::set_conn_now(&st, p, &db_host, &db_port, &db_user, &dbname, &db_pass);
    pump(200);

    let want_name = format!("{p} {db_host}:{db_port}/{dbname}");
    let got_name = dbui::current_conn_name(&st);
    println!("[1.5] 当前连接：{got_name}（应为 {want_name}）");
    assert_eq!(got_name, want_name, "「当前连接」不对");
    dbui::connect_now(&st);

    // 等连接 + 元数据回包
    let mut waited = 0;
    while waited < 20_000 {
        pump(300);
        waited += 300;
        if dbui::status_text(&st).starts_with("已连接") {
            // 连上以后再多等一会儿，等模式列表回来
            pump(1500);
            break;
        }
    }
    let status = dbui::status_text(&st);
    println!("[2] 状态栏：{status}");
    assert!(status.starts_with("已连接"), "数据库没连上：{status}");

    // ---- 阶段 22：双击即连 ----
    //
    // 验：双击一条数据库连接（= use_profile）之后**真的连上了**，
    // 不是"打开页面还得再点一次「连接」"。
    {
        let key = dbui::session_key_now(&st);
        let name = dbui::current_conn_name(&st);
        // 真实流程里这条连接是**建好的**（在连接管理器里建），
        // 双击时按名字从 databases.ini 里取 —— 这里把那条记录补上。
        let prof = puttyplus::backend::dbstore::DbProfile {
            name: name.clone(),
            proto: if proto == "mysql" {
                puttyplus::backend::db::DbProto::MySql
            } else {
                puttyplus::backend::db::DbProto::Postgres
            },
            host: db_host.clone(),
            port: db_port.parse().unwrap_or(0),
            user: db_user.clone(),
            dbname: dbname.clone(),
            remember: false,
        };
        puttyplus::backend::dbstore::save(&key, &prof);
        // 先断开，模拟"重新双击"
        dbui::disconnect_now(&st);
        pump(1500);
        assert!(!dbui::is_connected_now(&st), "应当已经断开");
        // 双击 → 直接连上，不用再点「连接」
        dbui::use_profile(&st, &key, &name);
        let mut waited = 0;
        while waited < 20_000 {
            pump(300);
            waited += 300;
            if dbui::is_connected_now(&st) {
                break;
            }
        }
        assert!(
            dbui::is_connected_now(&st),
            "双击之后应当已经连上（不用再点「连接」）：{}",
            dbui::status_text(&st)
        );
        println!("[2.1] 双击即连：断开 → 双击 → 已连接");
        // 截图：工作台（阶段 22：没有连接信息那一行了，页签头写着连的是哪条）
        shot("/tmp/db_workbench.png");

        // ---- 2.2 关掉数据库页签 → **自动断开**（工作台里不再有「断开」按钮）----
        {
            let sub = st.sub_tabs.borrow().clone().expect("应有子标签容器");
            let tab_id = dbui::workbench_tab_id(&st);
            assert!(sub.has(tab_id), "数据库页签不在子标签里");
            sub.close_page(tab_id);
            pump(1500);
            assert!(
                !dbui::is_connected_now(&st),
                "关掉数据库页签后应当自动断开"
            );
            println!("[2.2] 关掉数据库页签 → 自动断开");
            // 重新打开工作台并连上（后面的测试还要用）
            puttyplus::panel::on_open_db_page(&st);
            pump(800);
            dbui::use_profile(&st, &key, &name);
            // ⚠️ **等 60 秒**：同上，网络抖的时候 20 秒不够
            let mut waited2 = 0;
            while waited2 < 60_000 {
                pump(300);
                waited2 += 300;
                if dbui::is_connected_now(&st) {
                    break;
                }
            }
            assert!(
                dbui::is_connected_now(&st),
                "重新打开工作台后应当能再连上：{}",
                dbui::status_text(&st)
            );
            println!("[2.3] 重新打开工作台 → 再连上");
        }
    }

    // 连接成功 → 根节点下面应该已经有模式了（懒加载的第一层）
    let rows = tree_rows(&st);
    println!("[3] 对象树共 {} 行：", rows.len());
    for l in &rows {
        println!("    {l}");
    }
    assert!(rows.len() >= 2, "连上了但对象树是空的：{rows:?}");

    // ---- 对象树**屏幕上**要真的有子节点（不只是模型里有）----
    //
    // **为什么必须查视图**：2026-10 用户报「连上数据库后左侧只显示一行」，
    // 根因是树那个 ScrolledWindow 没撑满高度 → 控件只有一行高 → 子节点
    // 全被裁掉。**模型里照样有「public」这一行**，所以只查模型行数永远
    // 查不出来（之前 e2e 就是这么漏掉的）。这里查 `visible_range`。
    {
        let depth = dbui::tree_visible_depth(&st);
        println!("[3.05] 对象树屏幕上最深的可见行：{depth:?}（至少要 1 = 有子节点画出来了）");
        assert!(
            depth.unwrap_or(0) >= 1,
            "对象树只显示了根节点一行（子节点没画出来）：{depth:?}"
        );
    }

    // ---- 连上以后**立刻**截一张对象树（用户报的就是这个画面）----
    //
    // **为什么不能等到最后再截**：后面会开一堆对话框（对象属性 / 导入结果…），
    // 截图就被盖住了，看不出树到底显示没显示。
    if args.iter().any(|a| a == "--shot-tree") {
        if let Some(ui) = st.db_ui.borrow().as_ref() {
            if let Some(sub) = st.sub_tabs() {
                sub.switch_to(ui.tab_id.get());
            }
        }
        pump(800);
        println!("[3.1] 对象树状态：{}", dbui::tree_state(&st).join(" / "));
        shot("/tmp/db_tree.png");
    }

    // 建一个探针例程（真库上准备数据，后面要在对象树里看到它）
    {
        dbui::exec_sql_now(&st, &probe_routine(&proto, true));
        // ⚠️ **要等回包**：远端 SSH + 真库，网络慢的时候 2.5 秒不够——
        // 状态栏还停在"已连接…"就断言 → **假失败**（跟功能没关系）。
        let mut status = dbui::status_text(&st);
        for _ in 0..120 {
            if status.contains("执行完成") || status.contains("失败") {
                break;
            }
            pump(300);
            status = dbui::status_text(&st);
        }
        println!("[3.2] 建探针例程：{status}");
        assert!(status.contains("执行完成"), "建探针例程失败：{status}");
    }

    // ---- 建完探针例程，刷新一次树 ----
    //
    // ⚠️ **树是懒加载的**：连上时那次展开查的是"建探针**之前**"的库 ⇒ 例程节点
    // 压根不会出现在树上。上一轮跑剩的例程会让它"碰巧"出现 ⇒ 掩盖了这个坑
    // （e2e 自己清干净后，例程定义页签那段就会挂在"树里没有例程节点"）。
    // 建完得刷新一次，例程才看得到。
    {
        dbui::refresh_tree(&st);
        pump(2000);
        expand_first_schema(&st);
    }

    // ---- 清理上一轮留下的探针行 ----
    //
    // e2e 会在 demo_users 里插一行 id >= 900000 的探针行。中途崩掉的话这行
    // 就留在真库里了，下一轮"行数回到原样"的断言就会挂（看着像功能坏了，
    // 其实是上一轮的脏数据）。
    //
    // **只删探针行**（id >= 900000），不碰用户自己的数据。
    {
        let sql = if proto == "mysql" {
            "DELETE FROM demo.demo_users WHERE id >= 900000"
        } else {
            "DELETE FROM public.demo_users WHERE id >= 900000"
        };
        dbui::exec_sql_now(&st, sql);
        let mut status = dbui::status_text(&st);
        for _ in 0..120 {
            if status.contains("执行完成") || status.contains("失败") {
                break;
            }
            pump(300);
            status = dbui::status_text(&st);
        }
        println!("[3.3] 清理探针行：{status}");
    }

    // 展开第一个模式（模拟用户点展开箭头）→ 应该把表、例程、触发器都查出来
    expand_first_schema(&st);
    // ⚠️ **同样要等**：展开是异步的（查元数据），网络慢时立刻读只有占位节点
    let mut rows = tree_rows(&st);
    for _ in 0..150 {
        let has_table = rows.iter().any(|l| l.starts_with("表 |") && !l.contains("载入中"));
        if has_table {
            break;
        }
        pump(500);
        rows = tree_rows(&st);
    }
    println!("[3.6] 展开模式后，共 {} 行：", rows.len());
    for l in &rows {
        println!("    {l}");
    }
    assert!(rows.len() >= 3, "展开模式后没查出表：{rows:?}");

    // 双击第一张表 → 打开数据页签 → 结果表里应该有值
    //
    // ⚠️ **记住真库里查到的行**：后面导出断言要按它算行数。
    // 不同库的 demo_users 行数不一样（开发机那套 25 行，srv-01 上 3 行）⇒
    // 写死行数会让 e2e **只在一个库上能跑**。
    let mut demo_cols: Vec<String> = Vec::new();
    let mut demo_rows: Vec<Vec<String>> = Vec::new();
    {
        let mut table_path = None;
        if let Some(tv) = dbui::tree_view(&st) {
            if let Some(model) = tv.model() {
                if let Some(root) = model.iter_first() {
                    if let Some(schema) = model.iter_children(Some(&root)) {
                        if let Some(mut t) = model.iter_children(Some(&schema)) {
                            loop {
                                let kind = model.get::<String>(&t, 0);
                                if kind == "表" || kind == "视图" {
                                    table_path = Some(model.path(&t));
                                    break;
                                }
                                if !model.iter_next(&mut t) {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(table_path.is_some(), "树里没有表可选");
        if let Some(p) = table_path {
            activate(&st, &p);
        }
        // ⚠️ **必须等数据回包**：双击表是异步的（先取列元信息 → 再查数据），
        // 网络慢的时候立刻读会读到**空网格**——那不是 bug，是 e2e 没等。
        let mut grids = dbui::grid_rows(&st);
        // ⚠️ **等 75 秒**：远端 SSH + 真库，慢的时候一次查询要十几秒
        for _ in 0..150 {
            let ready = !grids.is_empty() && !grids[0].0.is_empty() && !grids[0].1.is_empty();
            if ready {
                break;
            }
            pump(500);
            grids = dbui::grid_rows(&st);
        }
        println!("[3.7] 结果表 {} 个：", grids.len());
        for (cols, rows) in &grids {
            println!("    列：{cols:?}");
            for r in rows.iter().take(5) {
                println!("    行：{r:?}");
            }
        }
        assert_eq!(grids.len(), 1, "数据页签没打开");
        assert!(!grids[0].0.is_empty(), "结果表没有列");
        assert!(!grids[0].1.is_empty(), "结果表没有数据行");
        demo_cols = grids[0].0.clone();
        demo_rows = grids[0].1.clone();
    }

    // 过滤条件（9.5）：填 Where 条件 → **服务端重查** → 行数变少
    //
    // 断言要落到**真库返回的行**上：条件是 `id > 1`，回来的每一行 id 都得
    // 大于 1；只看"行数变少"不够（可能只是分页没刷新）。
    {
        let before = dbui::grid_rows(&st);
        let n0 = before[0].1.len();
        println!("[3.7.1] 过滤前 {n0} 行");

        // ⚠️ **先记住旧状态**：等回包时要跟它比，否则会拿到上一次的旧状态
        let prev_status = dbui::status_text(&st);
        let w = dbui::apply_filter_now(
            &st,
            0,
            &[("id".to_string(), ">".to_string(), "1".to_string())],
        )
        .expect("应用过滤条件失败");
        println!("[3.7.2] WHERE：{w}");
        assert!(
            w.contains("id") && w.contains("> 1"),
            "WHERE 拼得不对：{w}"
        );

        let status = wait_query_done_after(&st, &prev_status);
        println!("[3.7.3] 过滤后状态：{status}");
        let rows = dbui::grid_rows(&st);
        let n1 = rows[0].1.len();
        println!("[3.7.4] 过滤后 {n1} 行");
        assert!(n1 < n0, "过滤后行数没变（{n0} → {n1}）");
        for r in rows[0].1.iter() {
            let id: i64 = r.first().map(|c| c.parse().unwrap_or(-1)).unwrap_or(-1);
            assert!(id > 1, "过滤后还有 id <= 1 的行：{r:?}");
        }

        // 清除条件 → 又回到全量（后面的测试要看到完整数据）
        let prev_status = dbui::status_text(&st);
        dbui::apply_filter_now(&st, 0, &[]).expect("清除过滤条件失败");
        wait_query_done_after(&st, &prev_status);
        let rows = dbui::grid_rows(&st);
        assert_eq!(rows[0].1.len(), n0, "清除条件后行数没回来");
        println!("[3.7.5] 清除条件后 {} 行", rows[0].1.len());

        // WHERE 框长什么样（截图）
        //
        // ⚠️ **阶段 84**：条件编辑器弹窗退场了（改成常驻的 WHERE 输入框），
        // 这里改成把 WHERE 框填上内容再截图。
        if args.iter().any(|a| a == "--shot-filter") {
            assert!(
                dbui::where_enter_now(&st, 0, "id > 1"),
                "WHERE 框回车没反应"
            );
            pump(600);
            shot("/tmp/where_box.png");
        }
    }

    // 导出 INSERT SQL（9.2）
    {
        let sql = dbui::export_sql_preview(&st, 0);
        println!("[3.8] 导出 SQL 预览：\n{sql}");
        assert!(sql.contains("INSERT INTO"), "导出 SQL 里没有 INSERT：{sql}");
        assert!(sql.contains("张三"), "导出 SQL 里没有数据：{sql}");
    }

    // 网格插入 / 删除行（9.4）：插一行 → 填值 → 保存 → 再删掉 → 保存
    //
    // **只动自己插进去的那一行**：最后状态回到原样，不污染真库。
    {
        // 主键用**每次跑都不同**的值：万一上一轮中途失败留了脏数据，
        // 这一轮也不会撞主键（撞了就 INSERT 失败 → 整个事务回滚）。
        let probe_id = format!(
            "{}",
            900_000 + (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() % 100_000)
                .unwrap_or(0))
        );
        let before = dbui::grid_rows(&st);
        let n0 = before[0].1.len();
        println!("[3.9] 插入前行数：{n0}（探针 id = {probe_id}）");

        // 插入一行（会自动开事务，事务开好后这一行才出现）
        //
        // ⚠️ **轮询等，不要固定 sleep**：远端 SSH + 真库，回包慢的时候
        // 固定等待会读到"还没插进去"的网格 → 假失败。
        dbui::grid_insert_row(&st, 0);
        let mut rows = dbui::grid_rows(&st);
        for _ in 0..120 {
            if rows[0].1.len() == n0 + 1 {
                break;
            }
            pump(300);
            rows = dbui::grid_rows(&st);
        }
        assert_eq!(rows[0].1.len(), n0 + 1, "插入后没多出一行");
        // 新行在最后，各列是空白
        let last = rows[0].1.last().expect("新行");
        assert!(
            last.iter().all(|c| c.is_empty()),
            "新行应该都是空的：{last:?}"
        );
        let (ins, del, cells) = dbui::grid_pending(&st, 0);
        println!("[3.9.1] 待保存：插入 {ins} 行 / 删除 {del} 行 / 改动 {cells} 格");
        assert_eq!((ins, del), (1, 0));

        // ⚠️ **「保存修改 / 放弃修改」必须可点**（用户 2026-10-28：「提交和
        // 回滚按钮都是禁用的，改了数据之后也点不了」）。
        //
        // **为什么现在才钉住**：这段流程以前全走 `grid_save` 这个**测试钩子
        // 直接调内部函数**，**从来不点真实按钮**——按钮一直是灰的，钩子照样
        // 跑通，所以这个 bug 一直没被发现。
        let Some((btn_edit, btn_save, btn_discard)) = dbui::edit_toolbar_buttons(&st, 0) else {
            panic!("拿不到编辑工具栏的按钮");
        };
        assert!(
            btn_save.is_sensitive(),
            "「保存修改」按钮是**禁用**的 —— 改了数据点不动（用户 2026-10-28 报的）"
        );
        assert!(
            btn_discard.is_sensitive(),
            "「放弃修改」按钮是**禁用**的 —— 改了数据点不动（用户 2026-10-28 报的）"
        );
        assert!(
            !btn_edit.is_visible(),
            "进了编辑模式就不该再显示「编辑」按钮"
        );
        println!("[3.9.1.1] 保存/放弃按钮可点：save={} discard={}",
            btn_save.is_sensitive(), btn_discard.is_sensitive());

        // 填两列（id 与 name：NOT NULL 的列必须给值；其余列用数据库默认值）
        dbui::grid_set_cell(&st, 0, n0, 0, &probe_id);
        dbui::grid_set_cell(&st, 0, n0, 1, "插入测试");
        pump(500);
        // 新行的改动记在"待插入行"里，不在"已有行的改动"里
        let (ins, del, cells) = dbui::grid_pending(&st, 0);
        assert_eq!((ins, del, cells), (1, 0, 0), "待保存的改动不对");
        let rows = dbui::grid_rows(&st);
        let last = rows[0].1.last().expect("新行");
        assert_eq!(last[0], probe_id, "新行的 id 没显示出来");
        assert_eq!(last[1], "插入测试", "新行的 name 没显示出来");
        if args.iter().any(|a| a == "--shot-insert") {
            shot("/tmp/insert_row.png");
        }

        // 保存 → 提交 → 刷新
        dbui::grid_save(&st, 0);
        let status = wait_query_rows(&st, n0 + 1);
        println!("[3.9.2] 保存后状态：{status}");
        assert!(!status.contains("失败"), "保存没成功：{status}");
        let rows = dbui::grid_rows(&st);
        // 新行确实在库里（id = 探针 id）
        let found = rows[0]
            .1
            .iter()
            .any(|r| r.first().map(|c| c.as_str()) == Some(probe_id.as_str()));
        assert!(found, "新行没进库：{:?}", rows[0].1);

        // 再把刚插的那行删掉 → 保存 → 行数回到原样
        let pos = rows[0]
            .1
            .iter()
            .position(|r| r.first().map(|c| c.as_str()) == Some(probe_id.as_str()))
            .expect("找不到刚插的行");
        dbui::grid_delete_row(&st, 0, pos);
        // ⚠️ **轮询等删除生效**：删除要先 BEGIN（异步），事务没开好之前
        // 那行还在网格里 —— 固定 sleep 会读到"还没删"的状态。
        let mut rows = dbui::grid_rows(&st);
        for _ in 0..120 {
            if rows[0].1.len() == n0 {
                break;
            }
            pump(300);
            rows = dbui::grid_rows(&st);
        }
        assert_eq!(rows[0].1.len(), n0, "标记删除后行数不对");
        let (_, del, _) = dbui::grid_pending(&st, 0);
        assert_eq!(del, 1, "应该标记了 1 行删除");
        if args.iter().any(|a| a == "--shot-del") {
            shot("/tmp/delete_row.png");
        }
        dbui::grid_save(&st, 0);
        let status = wait_query_rows(&st, n0);
        println!("[3.9.3] 删除保存后状态：{status}");
        assert!(!status.contains("失败"), "删除没保存成功：{status}");
        let rows = dbui::grid_rows(&st);
        assert_eq!(rows[0].1.len(), n0, "删除后行数没恢复");
        assert!(
            !rows[0]
                .1
                .iter()
                .any(|r| r.first().map(|c| c.as_str()) == Some(probe_id.as_str())),
            "探针那行还在库里"
        );
        println!("[3.9.4] 插入 / 删除行验证通过（行数回到 {n0}）");
    }

    // ---- 阶段 46：网格多选 + 列头增强 ----
    //
    // 验：① 列头是 dbx 风格「列名 + 下面一行小字类型」；
    //      ② 结果网格能多选（框两行）；
    //      ③ 点**真实的**「删除行」按钮 → **两行**都被标记删除。
    //
    // ⚠️ ③ 必须点**真实按钮**：钩子直接调内部函数的话，按钮灰着也照样"通过"
    // （阶段 42 的教训）。
    {
        // ① 列头
        let hdr = dbui::grid_header_info(&st, 0);
        println!("[3.11] 列头（列名, 类型小字）：{hdr:?}");
        assert!(hdr.len() >= 2, "结果表至少要有两列：{hdr:?}");
        assert!(!hdr[0].0.is_empty(), "列名不该是空的：{hdr:?}");
        assert!(
            hdr.iter().all(|(_, ty)| !ty.is_empty()),
            "每一列都该有类型小字（dbx 的 TEXT 那种）：{hdr:?}"
        );
        assert!(
            hdr.iter()
                .all(|(_, ty)| ["整数", "数字", "布尔", "文本"].contains(&ty.as_str())),
            "类型小字得是认识的中文类型：{hdr:?}"
        );

        // ② ③ 多选两行 → 点「删除行」
        let n_before = dbui::grid_rows(&st)[0].1.len();
        dbui::grid_select_rows(&st, 0, &[0, 1]);
        pump(300);
        assert!(
            dbui::grid_click_delete(&st, 0),
            "「删除行」按钮应当可点（表可编辑时它一直可点）"
        );
        // ⚠️ **轮询等事务开好**：删除要先 BEGIN，回包慢的时候不能固定 sleep
        let mut del = 0usize;
        for _ in 0..120 {
            pump(300);
            let (_, d, _) = dbui::grid_pending(&st, 0);
            del = d;
            if del > 0 {
                break;
            }
        }
        println!("[3.11.1] 选中两行按删除 → 待删除 {del} 行");
        assert_eq!(del, 2, "应当把**选中的两行**都标记删除（不是只删第一行）");
        let n_after = dbui::grid_rows(&st)[0].1.len();
        assert_eq!(n_after, n_before - 2, "网格里应当少了两行");
        if args.iter().any(|a| a == "--shot-header") {
            shot("/tmp/db_header.png");
        }

        // 放弃修改 → 恢复现场（别把真库改脏）
        dbui::click_discard(&st, 0);
        let mut n_back = 0usize;
        for _ in 0..120 {
            pump(300);
            n_back = dbui::grid_rows(&st)[0].1.len();
            if n_back == n_before {
                break;
            }
        }
        println!("[3.11.2] 放弃修改后行数：{n_back}");
        assert_eq!(n_back, n_before, "放弃修改后行数应当恢复");

        // ---- 阶段 47：网格右键菜单（详情 / 转置 / 批量修改）----
        //
        // ⚠️ 走的是**真实菜单回调**（网格 → hook → dbui）。
        {
            // 单元格详情
            assert!(dbui::grid_menu_action_now(&st, 0, "cell"), "单元格详情没触发");
            let d = dbui::grid_detail_now(&st);
            println!("[3.12] 单元格详情：{d:?}");
            assert!(!d.is_empty(), "单元格详情是空的");
            assert!(
                d.iter().any(|(k, _)| k == "列名"),
                "单元格详情里应当有「列名」：{d:?}"
            );
            assert!(
                d.iter().any(|(k, _)| k == "当前值"),
                "单元格详情里应当有「当前值」：{d:?}"
            );

            // 行详情：每一列一行
            assert!(dbui::grid_menu_action_now(&st, 0, "row"), "行详情没触发");
            let d = dbui::grid_detail_now(&st);
            println!("[3.12.1] 行详情：{} 项", d.len());
            assert!(d.len() >= 3, "行详情应当每列一行：{d:?}");

            // 列详情：列名 / 类型 / 行数 / NULL 数 …
            assert!(dbui::grid_menu_action_now(&st, 0, "col"), "列详情没触发");
            let d = dbui::grid_detail_now(&st);
            println!("[3.12.2] 列详情：{d:?}");
            assert!(
                d.iter().any(|(k, _)| k == "类型"),
                "列详情里应当有「类型」：{d:?}"
            );
            assert!(
                d.iter().any(|(k, _)| k == "行数"),
                "列详情里应当有「行数」：{d:?}"
            );

            // 转置查看：开一个独立窗口（不报错就成）
            assert!(
                dbui::grid_menu_action_now(&st, 0, "transpose"),
                "转置查看没触发"
            );
        }
    }

    // ---- 阶段 84：常驻 WHERE 框 + 可视化 ORDER BY ----
    //
    // 验：① WHERE 条件能生效（**按列类型转义**：整数列不加引号）；
    //      ② 带条件真的**重查了库**（行数变少，且剩下的行都满足条件）；
    //      ③ 排序列能生成 ORDER BY；④ 「清除」恢复整张表。
    {
        // 拿第一列（主键，通常是整数）和它的第一个值
        let (cols, rows) = dbui::grid_rows(&st)[0].clone();
        assert!(!cols.is_empty() && !rows.is_empty(), "表里没有数据可测");
        let col0 = cols[0].clone();
        let val0 = rows[0][0].clone();
        let n_before = rows.len();
        println!("[3.13] 用 {col0} = {val0} 过滤（原来 {n_before} 行）");

        // ① 下条件（**走 `apply_filter_now`**：它按列类型转义，整数列不加引号）
        assert!(
            dbui::apply_filter_now(&st, 0, &[(col0.clone(), "=".to_string(), val0.clone())])
                .is_ok(),
            "WHERE 条件下不了"
        );
        // ⚠️ **轮询等结果**：条件是服务端过滤，要等 worker 回包
        let mut n_after = n_before;
        for _ in 0..120 {
            pump(300);
            let (_, r) = dbui::grid_rows(&st)[0].clone();
            n_after = r.len();
            if n_after != n_before {
                break;
            }
        }
        let (w, ob, ocol) = dbui::cond_bar_state(&st, 0);
        println!("[3.13.1] WHERE = {w}（行数 {n_before} → {n_after}）");
        assert!(!w.is_empty(), "WHERE 条件没生成");
        assert!(w.contains(&col0), "WHERE 里没有那一列：{w}");
        // ⚠️ **WHERE 框里的文字要跟生效的条件一致**：用户看得见自己下发了什么
        assert_eq!(
            dbui::where_text_now(&st, 0),
            w,
            "WHERE 框里的文字与生效的条件不一致"
        );
        assert!(
            n_after < n_before,
            "带条件重查后行数没变（{n_before} → {n_after}）：条件没真的生效"
        );
        // ⚠️ **剩下的行都得满足条件**（这才是"服务端过滤"的证据）
        let (_, rows2) = dbui::grid_rows(&st)[0].clone();
        assert!(
            rows2.iter().all(|r| r[0] == val0),
            "过滤后还有不满足条件的行：{rows2:?}"
        );

        // ③ 排序：按同一列降序
        assert!(
            dbui::cond_bar_order_now(&st, 0, &col0, false),
            "条件条挑不了排序列"
        );
        pump(500);
        let (w2, ob2, ocol2) = dbui::cond_bar_state(&st, 0);
        println!("[3.13.2] ORDER BY = {ob2}");
        assert!(ob2.contains("DESC"), "排序方向没切成降序：{ob2}");
        assert_eq!(ocol2, col0, "排序列不对：{ocol2}");
        // ⚠️ **加了排序不能把条件弄丢**
        assert_eq!(w2, w, "加排序后条件变了：{w2} != {w}");

        // ④ 清除 → 恢复整张表
        assert!(dbui::cond_bar_clear_now(&st, 0), "清除点不动");
        let mut n_back = n_after;
        for _ in 0..120 {
            pump(300);
            let (_, r) = dbui::grid_rows(&st)[0].clone();
            n_back = r.len();
            if n_back == n_before {
                break;
            }
        }
        let (w3, ob3, _) = dbui::cond_bar_state(&st, 0);
        println!("[3.13.3] 清除后行数 {n_back}，WHERE = {w3:?} ORDER = {ob3:?}");
        assert!(w3.is_empty(), "清除后还有条件：{w3}");
        assert!(ob3.is_empty(), "清除后还有排序：{ob3}");
        // ⚠️ **框里的文字也要清**：只清生效状态不清框 ⇒ 用户看着框里的条件
        // 以为还在筛，其实没筛。
        assert!(
            dbui::where_text_now(&st, 0).is_empty(),
            "清除后 WHERE 框里还有字"
        );
        assert_eq!(n_back, n_before, "清除后行数没恢复：{n_back} != {n_before}");
        if args.iter().any(|a| a == "--shot-header") {
            shot("/tmp/db_condbar.png");
        }
    }

    // ---- 真库验「数字列按数值排序」（阶段 64 的核心目标）----
    //
    // 选 `SELECT 2 AS n UNION ALL SELECT 10`：服务端回来的顺序是 2、10，
    // 而**字典序**排出来是 10、2 ⇒ 只要排完还是「2 在 10 前面」，
    // 就说明排序用的是**数值**而不是字符串。
    //
    // ⚠️ 这条以前只做了离线断言（阶段 64 标"真库未验证"），现在真库验。
    {
        dbui::run_sql_now(&st, "SELECT 2 AS n UNION ALL SELECT 10");
        // 等回包（远端 SSH + 真库，慢的时候要等一会儿）
        let mut idx = None;
        for _ in 0..150 {
            pump(300);
            let grids = dbui::grid_rows(&st);
            if let Some(last) = grids.len().checked_sub(1) {
                if grids[last].1.len() == 2 {
                    idx = Some(last);
                    break;
                }
            }
        }
        let idx = idx.expect("没等到 SELECT 2 UNION ALL SELECT 10 的结果");
        assert!(
            dbui::grid_sort_now(&st, idx, 0, true),
            "给数字列排序没反应"
        );
        pump(400);
        let grids = dbui::grid_rows(&st);
        let vals: Vec<String> = grids[idx].1.iter().map(|r| r[0].clone()).collect();
        println!("[3.14] 数字列排序后：{vals:?}");
        assert_eq!(
            vals,
            vec!["2".to_string(), "10".to_string()],
            "数字列应当按**数值**排序（2 在 10 前）；字典序会排成 10、2"
        );
    }

    // 例程定义页签（9.3）：双击函数 / 存储过程 → 看到定义文本
    {
        let kinds: &[&str] = if proto == "mysql" {
            &["存储过程", "函数"]
        } else {
            &["函数", "存储过程"]
        };
        let path = find_node(&st, kinds);
        assert!(path.is_some(), "树里没有例程节点：{rows:?}");
        activate(&st, path.as_ref().unwrap());
        let defs = dbui::def_tabs_info(&st);
        println!("[3.9] 定义页签：{}", defs.len());
        for (title, text) in &defs {
            println!("    {title}：\n{text}");
        }
        assert_eq!(defs.len(), 1, "定义页签没打开");
        let (title, text) = &defs[0];
        assert!(
            title.contains("pp_probe"),
            "定义页签的标题不对（拿到的是别的例程）：{title}"
        );
        assert!(!text.trim().is_empty(), "定义文本是空的");
        assert!(
            text.to_uppercase().contains("CREATE"),
            "定义文本不像建例程语句：{text}"
        );

        // 「执行定义」：PG 是 CREATE OR REPLACE（幂等），MySQL 是先 DROP 再建
        dbui::def_exec_now(&st, 0);
        pump(3000);
        let status = dbui::status_text(&st);
        println!("[3.10] 执行定义后状态：{status}");
        assert!(
            status.contains("定义已执行"),
            "执行定义失败：{status}"
        );
    }

    // 模式名 / 表名从树里取（PG 是 public，MySQL 是库名，不能写死）
    let mut schema_name = String::new();
    let mut table_name = String::new();
    if let Some(tv) = dbui::tree_view(&st) {
        if let Some(model) = tv.model() {
            if let Some(root) = model.iter_first() {
                if let Some(schema) = model.iter_children(Some(&root)) {
                    schema_name = model.get::<String>(&schema, 1);
                    if let Some(mut t) = model.iter_children(Some(&schema)) {
                        loop {
                            let kind = model.get::<String>(&t, 0);
                            if kind == "表" {
                                table_name = model.get::<String>(&t, 1);
                                break;
                            }
                            if !model.iter_next(&mut t) {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(!table_name.is_empty(), "树里没有表");

    // 表结构页签（9.1）：字段 / 索引 / 外键 + DDL
    {
        dbui::open_structure_now(&st, &schema_name, &table_name);
        // ⚠️ **等 DDL 生成**：表结构要三次元数据回包（字段 / 索引 / 外键），
        // 远端网络慢的时候固定 3 秒不够 → "DDL 没生成"。
        let mut info = dbui::struct_tabs_info(&st);
        for _ in 0..120 {
            let ready = info
                .first()
                .map(|(_, ddl, _, _, _)| ddl.contains("CREATE TABLE"))
                .unwrap_or(false);
            if ready {
                break;
            }
            pump(500);
            info = dbui::struct_tabs_info(&st);
        }
        println!("[4] 表结构页签：{info:?}");
        println!("[4.0] 状态栏：{}", dbui::status_text(&st));
        assert_eq!(info.len(), 1, "表结构页签没打开");
        let (name, ddl, fields, indexes, fks) = &info[0];
        assert!(name.contains(&table_name), "表结构页签的表名不对：{name}");
        assert!(
            ddl.contains("CREATE TABLE"),
            "DDL 没生成：{ddl}"
        );
        assert!(ddl.contains("PRIMARY KEY"), "DDL 里没有主键：{ddl}");
        assert!(*fields > 0, "字段网格是空的");
        println!("[4.1] DDL：\n{ddl}");

        // 执行一条**无害**的 DDL（临时表：会话级，断开就没了）
        let probe = if proto == "mysql" {
            "CREATE TEMPORARY TABLE pp_ddl_probe (id int);"
        } else {
            "CREATE TEMP TABLE pp_ddl_probe (id int);"
        };
        dbui::struct_set_ddl(&st, 0, probe);
        dbui::struct_exec_ddl(&st, 0);
        pump(2500);
        let status = dbui::status_text(&st);
        println!("[4.2] 执行 DDL 后状态：{status}");
        assert!(
            status.contains("DDL 已执行"),
            "DDL 没执行成功：{status}"
        );
        // 把 DDL 文本还原成生成的建表语句（截图时看的是真东西）
        let ddl_text = ddl.clone();
        dbui::struct_set_ddl(&st, 0, &ddl_text);

        // 触发器定义页签（9.3）：建一个探针触发器 → 刷新树 → 双击看定义
        {
            let trg = if proto == "mysql" {
                format!(
                    "CREATE TRIGGER pp_probe_trg BEFORE INSERT ON `{schema_name}`.`{table_name}` FOR EACH ROW SET @pp_probe = 1"
                )
            } else {
                // PG 的触发器体必须是一个返回 trigger 的函数，先把它建出来
                // ⚠️ **等状态变化**，别固定 sleep：网络慢的时候 2 秒不够，
                // 状态栏还停在上一条（"DDL 已执行"）→ 假失败。
                let prev = dbui::status_text(&st);
                dbui::exec_sql_now(
                    &st,
                    "CREATE OR REPLACE FUNCTION pp_probe_trg_fn() RETURNS trigger AS $$ BEGIN RETURN NEW; END $$ LANGUAGE plpgsql",
                );
                let mut st0 = dbui::status_text(&st);
                for _ in 0..100 {
                    if st0 != prev {
                        break;
                    }
                    pump(300);
                    st0 = dbui::status_text(&st);
                }
                println!("[4.2.5] 建触发器函数：{st0}");
                assert!(st0.contains("执行完成"), "建触发器函数失败：{st0}");
                // EXECUTE PROCEDURE 在 PG 11+ 仍然可用（向后兼容别名）
                format!(
                    "CREATE TRIGGER pp_probe_trg BEFORE INSERT ON \"{schema_name}\".\"{table_name}\" FOR EACH ROW EXECUTE PROCEDURE pp_probe_trg_fn()"
                )
            };
            dbui::exec_sql_now(&st, &trg);
            pump(2500);
            let status = dbui::status_text(&st);
            println!("[4.3] 建探针触发器：{status}");
            assert!(status.contains("执行完成"), "建探针触发器失败：{status}");

            // 树是懒加载的，建完要刷新才看得到
            dbui::refresh_tree(&st);
            pump(2000);
            expand_first_schema(&st);
            let rows = tree_rows(&st);
            println!("[4.4] 刷新后树里有触发器：{}", rows.iter().filter(|l| l.contains("触发器")).count());
            let path = find_node(&st, &["触发器"]);
            assert!(path.is_some(), "树里没有触发器节点：{rows:?}");
            activate(&st, path.as_ref().unwrap());
            let defs = dbui::def_tabs_info(&st);
            println!("[4.5] 定义页签 {} 个", defs.len());
            let last = defs.last().expect("触发器定义页签没打开");
            println!("    {}：\n{}", last.0, last.1);
            assert!(
                last.0.contains("pp_probe_trg"),
                "触发器定义页签的标题不对：{}",
                last.0
            );
            assert!(!last.1.trim().is_empty(), "触发器定义是空的");
            assert!(
                last.1.to_uppercase().contains("TRIGGER"),
                "触发器定义不像建触发器语句：{}",
                last.1
            );
        }
    }

    // 清理探针对象（不留垃圾在真库上）
    {
        if proto == "mysql" {
            dbui::exec_sql_now(&st, "DROP TRIGGER IF EXISTS pp_probe_trg");
        } else {
            dbui::exec_sql_now(
                &st,
                &format!(
                    "DROP TRIGGER IF EXISTS pp_probe_trg ON \"{schema_name}\".\"{table_name}\""
                ),
            );
            pump(1500);
            dbui::exec_sql_now(&st, "DROP FUNCTION IF EXISTS pp_probe_trg_fn()");
            pump(1500);
        }
        dbui::exec_sql_now(&st, &probe_routine(&proto, false));
        pump(1500);
        println!("[5] 探针对象已清理：{}", dbui::status_text(&st));
    }

    // 对象树上的 DDL（9.6）：建探针表 → 树里出现 → 改名 → 删掉
    //
    // 走的是**产品代码那条路**（worker 的写通道 + 成功后刷新模式那一层），
    // 所以这里验的是"点了右键菜单之后树真的会变"。
    {
        let t1 = "pp_probe_tbl";
        let t2 = "pp_probe_tbl2";
        // 先幂等清一遍：上一轮 e2e 中途失败会在真库上留残表，
        // 下一轮就"表已存在"建不出来了（真库状态回不到原样）。
        let q0 = |s: &str| -> String {
            if proto == "mysql" {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        dbui::exec_sql_now(
            &st,
            &format!(
                "DROP TABLE IF EXISTS {}.{}",
                q0(&schema_name),
                q0(t1)
            ),
        );
        pump(1200);
        dbui::exec_sql_now(
            &st,
            &format!(
                "DROP TABLE IF EXISTS {}.{}",
                q0(&schema_name),
                q0(t2)
            ),
        );
        pump(1200);
        let q = |s: &str| -> String {
            if proto == "mysql" {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        // 建表
        dbui::exec_tree_ddl_now(
            &st,
            &schema_name,
            &format!(
                "CREATE TABLE {}.{} ({} integer, {} text)",
                q(&schema_name),
                q(t1),
                q("id"),
                q("name")
            ),
            "建探针表",
        );
        pump(2500);
        let status = dbui::status_text(&st);
        println!("[5.5] 建探针表：{status}");
        assert!(
            status.contains("建探针表") && !status.contains("失败"),
            "建表失败：{status}"
        );
        let rows = tree_rows(&st);
        assert!(
            rows.iter().any(|l| l.contains(t1)),
            "树里没有新建的表 {t1}：{rows:?}"
        );

        // 改名
        dbui::exec_tree_ddl_now(
            &st,
            &schema_name,
            &if proto == "mysql" {
                format!(
                    "RENAME TABLE {}.{} TO {}.{}",
                    q(&schema_name),
                    q(t1),
                    q(&schema_name),
                    q(t2)
                )
            } else {
                format!(
                    "ALTER TABLE {}.{} RENAME TO {}",
                    q(&schema_name),
                    q(t1),
                    q(t2)
                )
            },
            "改表名",
        );
        pump(2500);
        let status = dbui::status_text(&st);
        println!("[5.6] 改表名：{status}");
        assert!(
            status.contains("改表名") && !status.contains("失败"),
            "改表名失败：{status}"
        );
        let rows = tree_rows(&st);
        assert!(
            rows.iter().any(|l| l.contains(t2)),
            "树里没有改名后的表 {t2}：{rows:?}"
        );

        // 删表（真删，跑完不留垃圾）
        dbui::exec_tree_ddl_now(
            &st,
            &schema_name,
            &format!("DROP TABLE {}.{}", q(&schema_name), q(t2)),
            "删探针表",
        );
        pump(2500);
        let status = dbui::status_text(&st);
        println!("[5.7] 删探针表：{status}");
        assert!(
            status.contains("删探针表") && !status.contains("失败"),
            "删表失败：{status}"
        );
        let rows = tree_rows(&st);
        assert!(
            !rows.iter().any(|l| l.contains(t2)),
            "树里还有删掉的表 {t2}：{rows:?}"
        );
    }

    // 事件（9.3 的最后一类对象）
    //
    // **定时事件只有 MySQL 有**（Event Scheduler）。PostgreSQL 压根没有这个
    // 功能——它只有 `pg_event_trigger`（DDL 钩子），不是定时任务；查过 PG
    // 9.6/10/12/17/18 的系统目录清单，从来没有 `pg_event` 系统表。所以 PG 侧
    // 只能验"**不报错、树上没有事件节点**"；MySQL 侧是真功能，全流程测。
    {
        let ev = "pp_probe_ev";
        if proto == "mysql" {
            // MySQL：事件挂在库（= schema）下，跟表平铺
            // 先幂等清一遍（上一轮中断会留下定时任务）
            dbui::exec_sql_now(&st, &format!("DROP EVENT IF EXISTS `{ev}`"));
            pump(1200);
            dbui::exec_sql_now(
                &st,
                &format!("CREATE EVENT `{ev}` ON SCHEDULE EVERY 1 DAY DO SELECT 1"),
            );
            pump(2500);
            let status = dbui::status_text(&st);
            println!("[5.8] 建探针事件：{status}");
            assert!(status.contains("执行完成"), "建事件失败：{status}");

            // 树是懒加载的 → 刷新才看得到
            dbui::refresh_tree(&st);
            pump(2500);
            expand_first_schema(&st);
            let rows = tree_rows(&st);
            println!(
                "[5.9] 树里有事件：{}",
                rows.iter().filter(|l| l.contains(ev)).count()
            );
            assert!(
                rows.iter().any(|l| l.contains(ev) && l.contains("事件")),
                "树里没有事件节点 {ev}：{rows:?}"
            );

            // 截图：树里的事件节点（挂在库下面，跟表平铺）
            if args.iter().any(|a| a == "--shot-event") {
                shot("/tmp/event_tree.png");
            }

            // 双击事件 → 定义页签
            let path = find_node(&st, &["事件"]);
            assert!(path.is_some(), "树里没有事件节点（双击用）：{rows:?}");
            activate(&st, path.as_ref().unwrap());
            pump(1500);
            let defs = dbui::def_tabs_info(&st);
            let last = defs.last().expect("事件定义页签没打开");
            println!("[5.10] 事件定义：\n{}", last.1);
            assert!(last.0.contains(ev), "事件定义页签的标题不对：{}", last.0);
            assert!(
                last.1.to_uppercase().contains("EVENT"),
                "事件定义不像建事件语句：{}",
                last.1
            );

            // 截图：事件定义页签
            if args.iter().any(|a| a == "--shot-event") {
                shot("/tmp/event_def.png");
            }

            // 「执行定义」：先 DROP 再 CREATE（同名事件不能直接建）
            dbui::def_exec_now(&st, defs.len() - 1);
            pump(3000);
            let status = dbui::status_text(&st);
            println!("[5.10.5] 执行事件定义：{status}");
            assert!(
                !status.contains("失败"),
                "执行事件定义失败：{status}"
            );

            // 清理（不留定时任务在真库上）
            dbui::exec_sql_now(&st, &format!("DROP EVENT IF EXISTS `{ev}`"));
            pump(2000);
            println!("[5.11] 清理探针事件：{}", dbui::status_text(&st));
        } else {
            // PG：压根没有定时事件 → 查事件不能报错（否则整棵树都读不出来），
            // 树上也不该出现事件节点
            dbui::refresh_tree(&st);
            pump(2500);
            expand_first_schema(&st);
            let status = dbui::status_text(&st);
            println!("[5.8] PG 侧刷新后状态：{status}");
            assert!(
                !status.contains("读元数据失败"),
                "PG 上查事件报错了（PG 没有这功能，不该发请求）：{status}"
            );
            let rows = tree_rows(&st);
            assert!(
                !rows.iter().any(|l| l.contains("事件")),
                "PG 没有定时事件，树上不该有事件节点：{rows:?}"
            );
            println!("[5.9] PG 侧无事件节点（符合 PG 没有定时事件的事实）");
        }
    }

    // 对象全局搜索（11.1）
    //
    // 搜的是**整个库**（跨模式），不是已经展开的那几层树节点。
    // 关键词用 `demo_users`（真库里的探针表）：表本身、它的字段都该命中。
    {
        let kw = "demo_users";
        dbui::search_now(&st, kw);
        pump(3000);
        let status = dbui::status_text(&st);
        println!("[6] 搜索「{kw}」：{status}");
        assert!(
            !status.contains("失败"),
            "搜索报错：{status}"
        );
        let rows = dbui::search_results(&st);
        println!("[6.1] 搜索结果 {} 行：", rows.len());
        for r in rows.iter().take(12) {
            println!("     {}", r.join(" | "));
        }
        // 表本身要能搜到
        assert!(
            rows.iter().any(|r| r[0] == "表" && r[2] == kw),
            "搜索结果里没有表 {kw}：{rows:?}"
        );
        // 字段也要能搜到（列名 demo_users 不会命中，得用表名的一部分搜字段）
        // ——这里再搜一次字段名，确认「搜列」这条路通
        dbui::search_now(&st, "email");
        pump(3000);
        let rows2 = dbui::search_results(&st);
        println!("[6.2] 搜字段 email：{} 行", rows2.len());
        assert!(
            rows2.iter().any(|r| r[0] == "字段" && r[2] == "email"),
            "搜索结果里没有字段 email：{rows2:?}"
        );
        assert!(
            rows2.iter().any(|r| r[0] == "字段" && r[3] == kw),
            "字段 email 没带上它所在的表：{rows2:?}"
        );

        // 截图：搜索结果窗口
        if args.iter().any(|a| a == "--shot-search") {
            shot("/tmp/search_results.png");
        }

        // 双击第一条命中 → 打开/切到对应对象（表 → 表文档）
        //
        // ⚠️ **阶段 72e 之后不是"新开一个页签"**：打开一张表 = 打开/切到它的
        // **表文档**（数据 / 结构 / 属性 / ER 四视图合一），同一张表再打开
        // 还是同一个文档 ⇒ **外层页数不会变**。光数页数分不清"切过去了"和
        // "啥也没发生"，所以这里断言的是"当前停在**那张表**的文档上"。
        let before_pages = dbui::outer_page_count(&st);
        dbui::search_open_now(&st, 0);
        pump(2500);
        let after_pages = dbui::outer_page_count(&st);
        let names = dbui::doc_names(&st);
        let cur = dbui::current_doc_index(&st);
        println!(
            "[6.3] 打开命中：外层页 {before_pages} → {after_pages}，当前文档 {cur:?} / {names:?}"
        );
        assert!(
            after_pages >= before_pages,
            "打开命中后外层页数不该变少：{before_pages} → {after_pages}"
        );
        let cur_doc = cur.and_then(|i| names.get(i).cloned());
        assert_eq!(
            cur_doc,
            Some(("public".to_string(), kw.to_string())),
            "双击搜索结果没切到那张表的文档：当前 {cur:?} / {names:?}"
        );
    }

    // 执行历史（11.2）
    //
    // 跑过的 SQL 要能翻回去：先跑一条查询，再开历史窗口看它在不在，
    // 双击/「填入编辑器」后编辑器里应该是那条 SQL。
    {
        let sql = "SELECT 1 AS pp_probe_hist";
        dbui::run_sql_now(&st, sql);
        pump(2500);
        let n = dbui::open_history_now(&st);
        let list = dbui::history_info(&st);
        println!("[7] 历史 {n} 条：");
        for (t, s) in list.iter().take(6) {
            println!("     {t} | {s}");
        }
        assert!(n > 0, "历史是空的（跑过 SQL 应该有记录）");
        assert!(
            list.iter().any(|(_, s)| s.contains("pp_probe_hist")),
            "历史里没有刚跑的 SQL：{list:?}"
        );
        // 填回编辑器（不自动执行）
        let text = dbui::history_fill_now(&st, 0);
        println!("[7.1] 填回编辑器：{text}");
        assert!(
            text.contains("pp_probe_hist"),
            "历史没填回编辑器：{text}"
        );
        // 截图：历史窗口
        if args.iter().any(|a| a == "--shot-hist") {
            shot("/tmp/history_window.png");
        }
    }

    // 截图用：--shot-newtable 看「新建表」对话框
    if args.iter().any(|a| a == "--shot-newtable") {
        assert!(
            dbui::open_create_table_dialog_now(&st, &schema_name),
            "新建表对话框没打开"
        );
        shot("/tmp/new_table_dialog.png");
    }

    // ---- 阶段 13.5：对象属性（Navicat 的 Details 面板）----
    {
        // 用真库里那张探针表（demo_users）
        let schema = if proto == "mysql" { "demo" } else { "public" };
        dbui::open_object_stats_now(&st, schema, "demo_users");
        // 等回包（属性是异步查的）
        let mut rows = Vec::new();
        for _ in 0..40 {
            pump(300);
            rows = dbui::object_stats_rows(&st);
            if !rows.is_empty() {
                break;
            }
        }
        println!("[6.9] 对象属性：");
        for (k, v) in &rows {
            println!("    {k}：{v}");
        }
        assert!(!rows.is_empty(), "对象属性没读出来");
        // 行数 / 列数 / 大小这三项是 Navicat 属性面板的骨架
        let find = |k: &str| rows.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        assert!(find("行数（估算）").is_some(), "没有行数：{rows:?}");
        assert!(find("列数").is_some(), "没有列数：{rows:?}");
        assert!(find("占用空间").is_some(), "没有占用空间：{rows:?}");
        // 类型：PG 是 BASE TABLE，MySQL 是 InnoDB
        let engine = find("类型").unwrap_or_default();
        assert!(
            engine.contains("BASE TABLE") || engine.contains("InnoDB") || engine.contains("VIEW"),
            "类型不对：{engine}"
        );
        if args.iter().any(|a| a == "--shot-stats") {
            // 先关掉两个浮窗：历史和搜索结果都是独立窗口，在 xvfb（没有窗口
            // 管理器）里它们跟属性窗口一起贴在屏幕左上角，正好把属性窗口
            // 盖住，截图就看不出属性面板长什么样了。
            dbui::close_history_now(&st);
            dbui::close_search_now(&st);
            pump(500);
            shot("/tmp/object_stats.png");
        }
    }

    // ---- 阶段 14：表设计器改结构（可视化改表）----
    if args.iter().any(|a| a == "--shot-designer") {
        let q = |s: &str| -> String {
            if proto == "mysql" {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        let t = "pp_design_t";
        // 先幂等清一遍：上一轮 e2e 中途失败会在真库上留残表
        dbui::exec_sql_now(
            &st,
            &format!("DROP TABLE IF EXISTS {}.{}", q(&schema_name), q(t)),
        );
        pump(1200);
        dbui::exec_sql_now(
            &st,
            &format!(
                "CREATE TABLE {}.{} ({} integer NOT NULL, {} text)",
                q(&schema_name),
                q(t),
                q("id"),
                q("note")
            ),
        );
        pump(2000);

        // 打开表结构页签（设计器）
        // 先关掉浮窗（属性 / 历史 / 搜索）：它们是独立窗口，在 xvfb（没有窗口
        // 管理器）里全贴在屏幕左上角，正好盖住设计器，截图就看不清了。
        dbui::close_stats_now(&st);
        dbui::close_history_now(&st);
        dbui::close_search_now(&st);
        pump(400);
        dbui::open_structure_now(&st, &schema_name, t);
        pump(2500);
        let sidx = dbui::struct_count(&st).saturating_sub(1);
        assert!(dbui::struct_count(&st) > 0, "表结构页签没打开");

        let mut rows = Vec::new();
        for _ in 0..40 {
            pump(300);
            rows = dbui::struct_field_rows(&st, sidx);
            if rows.len() == 2 {
                break;
            }
        }
        println!("[6.10] 字段表：{rows:?}");
        assert_eq!(rows.len(), 2, "字段表应该两行：{rows:?}");

        // ⚠️ **「应用变更」主按钮**（阶段 44，对标 dbx 右下角那颗实心按钮）：
        // 主操作按钮"不突出"不会报错，只是界面退化回"一排一样的图标"——
        // 这类问题只能靠断言钉住。
        let apply = dbui::struct_apply_button_now(&st, sidx)
            .expect("表结构页签右下角应当有「应用变更」按钮");
        assert!(
            apply.has_css_class("pp-primary"),
            "「应用变更」应当是主按钮（带 pp-primary 类）"
        );
        println!("[6.10.0] 「应用变更」是主按钮 ✓");
        assert_eq!(rows[0][0], "id", "第一列不叫 id：{rows:?}");
        // 类型：PG 的 information_schema 写 "integer"，MySQL 写 "int"
        // （MySQL 内部就把 INTEGER 存成 INT，两边都算对）
        assert!(
            rows[0][1] == "integer" || rows[0][1] == "int",
            "类型不对：{rows:?}"
        );
        shot("/tmp/designer_fields.png");

        // ---- 改类型：id integer → bigint ----
        dbui::struct_set_cell(&st, sidx, 0, 1, "bigint");
        dbui::struct_save_now(&st, sidx);
        let mut r2 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r2 = dbui::struct_field_rows(&st, sidx);
            if !r2.is_empty() && r2[0][1] == "bigint" {
                break;
            }
        }
        println!("[6.10.1] 改类型后：{r2:?}");
        println!("[6.10.1] 状态：{}", dbui::struct_status(&st, sidx));
        assert_eq!(r2[0][1], "bigint", "类型没改成 bigint：{r2:?}");
        shot("/tmp/designer_saved.png");

        // ---- 加一个字段 ----
        dbui::struct_add_field(&st, sidx);
        dbui::struct_set_cell(&st, sidx, 2, 0, "extra");
        dbui::struct_set_cell(&st, sidx, 2, 1, "text");
        dbui::struct_save_now(&st, sidx);
        let mut r3 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r3 = dbui::struct_field_rows(&st, sidx);
            if r3.len() == 3 {
                break;
            }
        }
        println!("[6.10.2] 加字段后：{r3:?}");
        assert_eq!(r3.len(), 3, "加字段没生效：{r3:?}");
        assert_eq!(r3[2][0], "extra", "新字段名字不对：{r3:?}");

        // ---- 删掉刚加的字段 ----
        dbui::struct_del_field(&st, sidx, 2);
        dbui::struct_save_now(&st, sidx);
        let mut r4 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r4 = dbui::struct_field_rows(&st, sidx);
            if r4.len() == 2 {
                break;
            }
        }
        println!("[6.10.3] 删字段后：{r4:?}");
        assert_eq!(r4.len(), 2, "删字段没生效：{r4:?}");

        // ---- 改回来（bigint → integer），别把真库留成改过的样子 ----
        dbui::struct_set_cell(&st, sidx, 0, 1, "integer");
        dbui::struct_save_now(&st, sidx);
        let mut r5 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r5 = dbui::struct_field_rows(&st, sidx);
            if !r5.is_empty() && r5[0][1] == "integer" {
                break;
            }
        }
        println!("[6.10.4] 改回来后：{r5:?}");
        // 改回来后 MySQL 会把 INTEGER 规范化成 INT（内部存法），两边都算对
        assert!(
            r5[0][1] == "integer" || r5[0][1] == "int",
            "类型没改回来：{r5:?}"
        );

        // 清理探针表
        dbui::exec_sql_now(
            &st,
            &format!("DROP TABLE {}.{}", q(&schema_name), q(t)),
        );
        pump(1500);
    }

    // ---- 阶段 15：表设计器改索引 ----
    if args.iter().any(|a| a == "--shot-index") {
        let q = |s: &str| -> String {
            if proto == "mysql" {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        let t = "pp_idx_t";
        // **MySQL 的 TEXT 列建索引必须给前缀长度**（不给就报
        // ERROR 1170 BLOB/TEXT column used in key specification without a
        // key length）；PG 的 text 列可以直接建索引，但不支持前缀。
        //
        // ⚠️ 两个写法别混：`note_col` 是**填进网格**的（用户视角），
        // `note_idx_sql` 是**建索引语句**里的（列名加引号、长度不加）。
        let note_col = if proto == "mysql" { "note(10)" } else { "note" };
        let note_idx_sql = if proto == "mysql" {
            "`note`(10)"
        } else {
            "\"note\""
        };
        dbui::exec_sql_now(
            &st,
            &format!("DROP TABLE IF EXISTS {}.{}", q(&schema_name), q(t)),
        );
        pump(1200);
        // 带主键 + 一个普通索引
        dbui::exec_sql_now(
            &st,
            &format!(
                "CREATE TABLE {}.{} ({} integer NOT NULL, {} text, PRIMARY KEY ({}))",
                q(&schema_name),
                q(t),
                q("id"),
                q("note"),
                q("id")
            ),
        );
        pump(2000);
        dbui::exec_sql_now(
            &st,
            &format!(
                "CREATE INDEX idx_note ON {}.{} ({})",
                q(&schema_name),
                q(t),
                note_idx_sql
            ),
        );
        pump(1500);

        dbui::close_stats_now(&st);
        dbui::close_history_now(&st);
        dbui::close_search_now(&st);
        pump(400);
        dbui::open_structure_now(&st, &schema_name, t);
        pump(2500);
        let sidx = dbui::struct_count(&st).saturating_sub(1);
        assert!(dbui::struct_count(&st) > 0, "表结构页签没打开");

        let mut rows = Vec::new();
        for _ in 0..40 {
            pump(300);
            rows = dbui::struct_index_rows(&st, sidx);
            if !rows.is_empty() {
                break;
            }
        }
        println!("[6.11] 索引表：{rows:?}");
        // **主键索引不显示**（主键由字段页管），所以只有普通索引那一条
        assert_eq!(rows.len(), 1, "索引表应该只有一条：{rows:?}");
        assert_eq!(rows[0][0], "idx_note");
        assert_eq!(rows[0][1], "note");
        assert_eq!(rows[0][2], "否", "普通索引不是唯一索引");

        // ---- 加一个唯一索引 ----
        dbui::struct_add_index(&st, sidx);
        dbui::struct_set_index_cell(&st, sidx, 1, 0, "idx_uniq");
        dbui::struct_set_index_cell(&st, sidx, 1, 1, note_col);
        dbui::struct_set_index_cell(&st, sidx, 1, 2, "是");
        dbui::struct_save_now(&st, sidx);
        let mut r2 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r2 = dbui::struct_index_rows(&st, sidx);
            if r2.len() == 2 {
                break;
            }
        }
        println!("[6.11.1] 加索引后：{r2:?}");
        assert_eq!(r2.len(), 2, "加索引没生效：{r2:?}");
        let uniq = r2.iter().find(|r| r[0] == "idx_uniq");
        assert!(uniq.is_some(), "新索引不在列表里：{r2:?}");
        assert_eq!(uniq.map(|r| r[2].clone()), Some("是".to_string()), "唯一标记不对");
        // 切到索引页再截图（默认停在字段页）
        dbui::struct_show_page(&st, sidx, 1);
        pump(600);
        shot("/tmp/designer_index.png");

        // ---- 删掉刚加的那个索引 ----
        let pos = r2.iter().position(|r| r[0] == "idx_uniq").unwrap();
        dbui::struct_del_index(&st, sidx, pos);
        dbui::struct_save_now(&st, sidx);
        let mut r3 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r3 = dbui::struct_index_rows(&st, sidx);
            if r3.len() == 1 {
                break;
            }
        }
        println!("[6.11.2] 删索引后：{r3:?}");
        assert_eq!(r3.len(), 1, "删索引没生效：{r3:?}");

        // 清理探针表
        dbui::exec_sql_now(
            &st,
            &format!("DROP TABLE {}.{}", q(&schema_name), q(t)),
        );
        pump(1500);
    }

    // ---- 阶段 16：表设计器改外键 ----
    if args.iter().any(|a| a == "--shot-fk") {
        let q = |s: &str| -> String {
            if proto == "mysql" {
                format!("`{s}`")
            } else {
                format!("\"{s}\"")
            }
        };
        let parent = "pp_fk_parent";
        let child = "pp_fk_child";
        // 引用表只写表名（同库 / 同 schema 内）
        let parent_ref = q(parent);
        for t in [parent, child] {
            dbui::exec_sql_now(
                &st,
                &format!("DROP TABLE IF EXISTS {}.{}", q(&schema_name), q(t)),
            );
            pump(1200);
        }
        dbui::exec_sql_now(
            &st,
            &format!(
                "CREATE TABLE {}.{} ({} integer NOT NULL PRIMARY KEY, {} text)",
                q(&schema_name),
                q(parent),
                q("id"),
                q("code")
            ),
        );
        pump(1500);
        dbui::exec_sql_now(
            &st,
            &format!(
                "CREATE TABLE {}.{} ({} integer NOT NULL PRIMARY KEY, {} integer)",
                q(&schema_name),
                q(child),
                q("id"),
                q("parent_id")
            ),
        );
        pump(1500);
        dbui::exec_sql_now(
            &st,
            &format!(
                "ALTER TABLE {}.{} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
                q(&schema_name),
                q(child),
                q("fk_child_parent"),
                q("parent_id"),
                parent_ref,
                q("id")
            ),
        );
        pump(2000);

        dbui::close_stats_now(&st);
        dbui::close_history_now(&st);
        dbui::close_search_now(&st);
        pump(400);
        dbui::open_structure_now(&st, &schema_name, child);
        pump(2500);
        let sidx = dbui::struct_count(&st).saturating_sub(1);
        assert!(dbui::struct_count(&st) > 0, "表结构页签没打开");

        let mut rows = Vec::new();
        for _ in 0..40 {
            pump(300);
            rows = dbui::struct_fk_rows(&st, sidx);
            if !rows.is_empty() {
                break;
            }
        }
        println!("[6.12] 外键表：{rows:?}");
        assert_eq!(rows.len(), 1, "外键表应该只有一条：{rows:?}");
        assert_eq!(rows[0][0], "fk_child_parent");
        assert_eq!(rows[0][1], "parent_id");
        assert_eq!(rows[0][2], parent);
        assert_eq!(rows[0][3], "id");

        // ---- 改约束名（PG 直接 RENAME，MySQL 删了重建）----
        dbui::struct_set_fk_cell(&st, sidx, 0, 0, "fk_child_p2");
        dbui::struct_save_now(&st, sidx);
        let mut r2 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r2 = dbui::struct_fk_rows(&st, sidx);
            if !r2.is_empty() && r2[0][0] == "fk_child_p2" {
                break;
            }
        }
        println!("[6.12.1] 改名后：{r2:?}");
        assert_eq!(r2[0][0], "fk_child_p2", "改名没生效：{r2:?}");
        // 引用关系要保住
        assert_eq!(r2[0][1], "parent_id", "引用列被改坏了：{r2:?}");
        assert_eq!(r2[0][2], parent, "引用表被改坏了：{r2:?}");
        dbui::struct_show_page(&st, sidx, 2);
        pump(600);
        shot("/tmp/designer_fk.png");

        // ---- 删掉外键 ----
        dbui::struct_del_fk(&st, sidx, 0);
        dbui::struct_save_now(&st, sidx);
        let mut r3 = Vec::new();
        for _ in 0..40 {
            pump(300);
            r3 = dbui::struct_fk_rows(&st, sidx);
            if r3.is_empty() {
                break;
            }
        }
        println!("[6.12.2] 删外键后：{r3:?}");
        assert!(r3.is_empty(), "删外键没生效：{r3:?}");

        // 清理探针表
        for t in [child, parent] {
            dbui::exec_sql_now(
                &st,
                &format!("DROP TABLE {}.{}", q(&schema_name), q(t)),
            );
            pump(1200);
        }
    }

    // ---- 阶段 17：结果网格的列显示设置（Navicat 的字段显示设置）----
    //
    // 验三件事：① 工具栏按钮能把对话框打开；② 在对话框里藏掉一列，
    // 确定后网格里真的少了一列；③ **隐藏只影响显示**——导出 SQL 仍按
    // 全部列算（藏一列不该让那一列从 INSERT 里消失）。
    {
        // 找一个列数 >= 2 的结果页签
        let tabs = dbui::grid_rows(&st);
        // ⚠️ **别 `return`**：后面还有别的段落（截图等），提前退出会把它们
        // 全跳掉。找不到多列页签就整段跳过。
        if let Some(tidx) = tabs.iter().position(|(cols, _)| cols.len() >= 2) {
            let before = tabs[tidx].0.clone();
            println!("[6.13] 原来的列：{before:?}");
            assert!(
                dbui::open_column_dialog_now(&st, tidx),
                "列显示设置应当能打开"
            );
            let dlg = wait_dialog("列显示设置").expect("「列显示设置」对话框应当出现");
            // 选中第 2 列，把它的「显示这一列」勾去掉
            let list =
                find_listbox(dlg.upcast_ref::<gtk::Window>()).expect("对话框里应当有列清单");
            let row = list.row_at_index(1).expect("至少要有第 2 列");
            list.select_row(Some(&row));
            pump(300);
            let chk = find_checkbutton(dlg.upcast_ref::<gtk::Window>())
                .expect("应当有「显示这一列」勾选框");
            assert!(chk.is_active(), "选中的列默认应当是显示的");
            chk.set_active(false);
            pump(300);
            shot("/tmp/column_dialog.png");
            dlg.response(gtk::ResponseType::Ok);
            pump(500);

            let after = dbui::grid_visible_columns(&st, tidx);
            println!("[6.13.1] 藏掉一列后：{after:?}");
            assert_eq!(
                after.len(),
                before.len() - 1,
                "藏掉一列后网格应当少一列：{before:?} → {after:?}"
            );
            assert!(
                !after.contains(&before[1]),
                "被藏掉的列不该还在显示：{after:?}"
            );
            // **隐藏只影响显示**：可见列的**原始下标**仍指向原来的列
            let idxs = dbui::grid_visible_col_indexes(&st, tidx);
            println!("[6.13.2] 可见列的原始下标：{idxs:?}");
            assert_eq!(idxs.len(), after.len());

            // 导出 SQL 仍按**全部列**算（藏一列不该让那一列的数据消失）
            let sql = dbui::export_sql_preview(&st, tidx);
            println!("[6.13.3] 导出 SQL（前两行）：");
            for l in sql.lines().take(2) {
                println!("    {l}");
            }
            assert!(
                sql.contains(&before[1]),
                "藏起来的列也必须在导出的 SQL 里（隐藏只影响显示）：{sql}"
            );
            // 恢复：全部列显示回来，别把后面的截图留在缺列状态
            assert!(dbui::open_column_dialog_now(&st, tidx));
            let dlg2 = wait_dialog("列显示设置").expect("「列显示设置」对话框应当能再打开");
            let all = find_button_by_label(dlg2.upcast_ref::<gtk::Window>(), "全显示")
                .expect("应当有「全显示」按钮");
            all.emit_clicked();
            pump(300);
            dlg2.response(gtk::ResponseType::Ok);
            pump(500);
            let restored = dbui::grid_visible_columns(&st, tidx);
            println!("[6.13.4] 全显示后：{restored:?}");
            assert_eq!(restored.len(), before.len(), "「全显示」应当恢复所有列");
            // ⚠️ **顺序也得回来**：藏过的列不能因为「全显示」就跑到末尾去
            assert_eq!(restored, before, "「全显示」后列顺序应当恢复原样");
        } else {
            println!("[6.13] 跳过：没有多列的结果页签");
        }
    }

    // ---- 阶段 20：列显示设置的持久化 ----
    //
    // 验：① 在**表浏览**页签里藏掉一列 → 重开这张表 → 那列还是藏着的；
    //      ② 设置存在**配置文件**里（不是内存里的临时状态）。
    {
        let schema = if proto == "mysql" { "demo" } else { "public" };
        // 表浏览页签才有持久化（普通查询结果的列每次都不一样，不存）
        let tidx = dbui::open_table_now(&st, schema, "demo_users")
            .expect("应当能打开 demo_users 的表浏览页签");
        let mut cols = Vec::new();
        for _ in 0..40 {
            pump(300);
            cols = dbui::grid_visible_columns(&st, tidx);
            if cols.len() >= 2 {
                break;
            }
        }
        assert!(cols.len() >= 2, "表浏览页签应当有列：{cols:?}");
        let hidden = cols[1].clone();

        // ⚠️ **面包屑（阶段 43）**：表浏览页签顶部那排芯片（对标 dbx 的
        // `库 / 表@模式 / N 字段`）——打开一张表后看不出自己在哪个库哪张表，
        // 是 dbx 那种"专业感"的重要来源。
        let mut crumbs = Vec::new();
        for _ in 0..40 {
            pump(300);
            crumbs = dbui::result_crumbs_now(&st, tidx);
            if !crumbs.is_empty() {
                break;
            }
        }
        println!("[6.16.0] 面包屑：{crumbs:?}");
        assert!(
            crumbs.iter().any(|c| c.contains("demo_users")),
            "面包屑里应当有表名 demo_users：{crumbs:?}"
        );
        assert!(
            crumbs.iter().any(|c| c.contains(schema)),
            "面包屑里应当有模式 {schema}：{crumbs:?}"
        );
        assert!(
            crumbs.iter().any(|c| c.ends_with("字段")),
            "面包屑里应当有「N 字段」：{crumbs:?}"
        );
        if args.iter().any(|a| a == "--shot-crumb") {
            shot("/tmp/db_crumb.png");
        }

        // ⚠️ **底栏（阶段 45）**：dbx 底部那一长条信息量很大
        // （行数·耗时·待保存项数 | 每页行数 | 分页控件）。
        let (foot_info, foot_page) = dbui::result_foot_now(&st, tidx);
        println!("[6.16.1] 底栏：{foot_info:?} / {foot_page:?}");
        assert!(
            foot_info.contains("行"),
            "底栏应当有行数信息：{foot_info:?}"
        );
        assert!(
            foot_page.contains("第"),
            "底栏应当有页码：{foot_page:?}"
        );
        // 第一页时「首页 / 上一页」不可点
        assert!(
            foot_page.contains("1"),
            "刚打开应当在第一页：{foot_page:?}"
        );

        // 打开列显示设置，把第 2 列藏掉
        assert!(
            dbui::open_column_dialog_now(&st, tidx),
            "列显示设置应当能打开"
        );
        let dlg = wait_dialog("列显示设置").expect("「列显示设置」对话框应当出现");
        let list = find_listbox(dlg.upcast_ref::<gtk::Window>()).expect("对话框里应当有列清单");
        let row = list.row_at_index(1).expect("至少要有第 2 列");
        list.select_row(Some(&row));
        pump(300);
        let chk = find_checkbutton(dlg.upcast_ref::<gtk::Window>())
            .expect("应当有「显示这一列」勾选框");
        assert!(chk.is_active(), "选中的列默认应当是显示的");
        chk.set_active(false);
        pump(300);
        dlg.response(gtk::ResponseType::Ok);
        pump(800);
        let after = dbui::grid_visible_columns(&st, tidx);
        println!("[6.16] 藏掉「{hidden}」后：{after:?}");
        assert!(!after.contains(&hidden), "藏掉的列不该还在显示：{after:?}");

        // **重开这张表** → 设置还在（这才是持久化的意义）
        let tidx2 =
            dbui::open_table_now(&st, schema, "demo_users").expect("应当能再打开一次这张表");
        let mut cols2 = Vec::new();
        for _ in 0..40 {
            pump(300);
            cols2 = dbui::grid_visible_columns(&st, tidx2);
            if !cols2.is_empty() {
                break;
            }
        }
        println!("[6.16.1] 重开表后可见列：{cols2:?}");
        assert!(
            !cols2.contains(&hidden),
            "重开表后那一列应当还是藏着的：{cols2:?}"
        );
        assert_eq!(
            cols2.len(),
            after.len(),
            "重开表后的可见列应当跟之前一致：{cols2:?} vs {after:?}"
        );
    }

    // ---- 阶段 21：导出格式（JSON / XML / HTML）----
    //
    // 验：① 「导出…」菜单里五种格式都在；② 每种格式都能生成内容；
    //      ③ JSON 里的 NULL 是 null（不是字符串 "NULL"）；④ 老的两样
    //      （CSV / INSERT SQL）没被弄坏。
    {
        let tabs = dbui::grid_rows(&st);
        if let Some(tidx) = tabs.iter().position(|(cols, _)| cols.len() >= 2) {
            // 菜单里五种格式都在
            assert!(
                dbui::open_export_menu_now(&st, tidx),
                "「导出…」菜单应当能弹出来"
            );
            pump(400);
            for label in [
                "导出为 CSV…",
                "导出为 INSERT SQL 脚本…",
                "导出为 JSON…",
                "导出为 XML…",
                "导出为 HTML 表格…",
            ] {
                assert!(
                    find_button_by_label(&win, label).is_some(),
                    "导出菜单里应当有「{label}」"
                );
            }
            // 截图：导出菜单长什么样
            shot("/tmp/export_menu.png");
            // 关掉菜单（别在后面的截图里留个浮层）
            dbui::close_export_menu_now(&st, tidx);
            pump(300);

            let json = dbui::export_preview(&st, tidx, "json");
            println!(
                "[6.17] JSON 导出（前 160 字）：{}",
                json.chars().take(160).collect::<String>()
            );
            assert!(json.trim_start().starts_with('['), "JSON 应当是数组");
            assert!(json.contains("\"id\""), "JSON 应当带列名：{json}");

            let xml = dbui::export_preview(&st, tidx, "xml");
            assert!(xml.starts_with("<?xml"), "XML 应当有声明头");
            assert!(
                xml.contains("<col name=\""),
                "XML 应当用属性放列名（列名可能不是合法元素名）：{xml}"
            );

            let html = dbui::export_preview(&st, tidx, "html");
            assert!(html.contains("<table>"), "HTML 应当是表格");
            assert!(html.contains("<th>"), "HTML 应当有表头");

            // 老的两样没被弄坏
            let csv = dbui::export_preview(&st, tidx, "csv");
            assert!(csv.contains(','), "CSV 应当还是逗号分隔");
            let sql = dbui::export_preview(&st, tidx, "sql");
            assert!(sql.contains("INSERT INTO"), "SQL 导出应当还是 INSERT 语句");
        }
    }

    // ---- 阶段 18：CSV 导入向导（Navicat 的 Import Wizard）----
    //
    // 验三件事：① 正常导入能进库；② **失败要整体回滚**（一行都不能留）；
    // ③ 导入失败时报的是**中文且带行号**的原因。
    {
        let schema = if proto == "mysql" { "demo" } else { "public" };
        // 建探针表（id 主键 —— 后面用它验证回滚）
        dbui::exec_sql_now(&st, "DROP TABLE IF EXISTS pp_import_t");
        pump(1000);
        dbui::exec_sql_now(
            &st,
            "CREATE TABLE pp_import_t (id integer PRIMARY KEY, name varchar(50), age integer)",
        );
        pump(1500);
        assert!(
            dbui::status_text(&st).contains("执行完成"),
            "建探针表失败：{}",
            dbui::status_text(&st)
        );

        // 导入 3 行（列名自动匹配：id / name / age）
        let csv = "id,name,age\n1,张三,30\n2,李四,25\n3,王五,41\n";

        // 截图用：走完整向导（弹字段映射对话框），看看界面长什么样
        if args.iter().any(|a| a == "--shot-import") {
            assert!(dbui::import_csv_dialog_now(&st, schema, "pp_import_t", csv));
            let dlg = wait_dialog("导入数据到 public.pp_import_t")
                .or_else(|| wait_dialog("导入数据到 demo.pp_import_t"))
                .expect("字段映射对话框应当打开");
            pump(800);
            shot("/tmp/import_dialog.png");
            dlg.response(gtk::ResponseType::Cancel);
            pump(600);
        }
        assert!(
            dbui::import_csv_now(&st, schema, "pp_import_t", csv),
            "导入应当能发起"
        );
        let mut got = None;
        for _ in 0..120 {
            pump(300);
            if let Some(r) = dbui::last_import(&st) {
                got = Some(r);
                break;
            }
        }
        let (ok, msg) = got.expect("导入应当有结果");
        println!("[6.14] 导入 3 行：成功={ok} {msg}");
        assert!(ok, "导入失败：{msg}");

        // 查行数：应当是 3
        // ⚠️ 用 `run_sql_now`（走查询路径、会显示结果），不是 `exec_sql_now`
        // （那是执行 DDL 的，只回影响行数，不开结果页签）
        dbui::run_sql_now(&st, &format!("SELECT count(*) FROM {schema}.pp_import_t"));
        pump(1800);
        let cnt = count_of_last_grid(&st);
        println!("[6.14.1] 表里的行数：{cnt}");
        assert_eq!(cnt, "3", "导入后应当有 3 行");

        // **重复主键** → 失败，而且**整体回滚**（表里仍然是 3 行）
        let csv2 = "id,name,age\n1,重复,1\n";
        assert!(dbui::import_csv_now(&st, schema, "pp_import_t", csv2));
        let mut got2 = None;
        for _ in 0..120 {
            pump(300);
            if let Some(r) = dbui::last_import(&st) {
                got2 = Some(r);
                break;
            }
        }
        let (ok2, msg2) = got2.expect("第二次导入应当有结果");
        println!("[6.14.2] 重复主键：成功={ok2} {msg2}");
        assert!(!ok2, "重复主键应当失败");
        assert!(!msg2.is_empty(), "失败原因不能是空的");

        // 回滚验证：行数没变
        dbui::run_sql_now(&st, &format!("SELECT count(*) FROM {schema}.pp_import_t"));
        pump(1800);
        let cnt2 = count_of_last_grid(&st);
        println!("[6.14.3] 回滚后行数：{cnt2}");
        assert_eq!(cnt2, "3", "导入失败必须整体回滚，表里不该多出那一行");

        // 清理探针表
        dbui::exec_sql_now(&st, "DROP TABLE pp_import_t");
        pump(1200);
        // 关掉「导入失败」的错误框：它会挡住后面的布局截图
        dbui::close_import_error_now(&st);
        pump(400);
    }

    // ---- 阶段 19：ER 图（Navicat 的 Database Model）----
    //
    // **为什么要在真库上建带外键的表**：ER 图画的是"表和表的关系"，
    // 没有外键就画不出线来。而"整个模式的外键"这条 SQL 是新写的
    // （PG 走 referential_constraints 按位置配对列，MySQL 走
    // key_column_usage），得在真库上验——单测里只有造出来的假数据。
    {
        let schema = if proto == "mysql" { "demo" } else { "public" };
        // 先删一遍残留：上一轮跑挂了会留下探针表，这一轮就建不出来
        // （PG 报 "relation already exists"，MySQL 报 "table already exists"）。
        // 探针表名字固定，先 DROP 一下就自愈了。
        for sql in ["DROP TABLE IF EXISTS pp_er_child", "DROP TABLE IF EXISTS pp_er_parent"] {
            dbui::exec_sql_now(&st, sql);
            pump(800);
        }
        // 建一对父子表（子表有外键指向父表）
        let ddl = if proto == "mysql" {
            vec![
                "CREATE TABLE pp_er_parent (id INT NOT NULL, name VARCHAR(32), \
                 PRIMARY KEY (id)) ENGINE=InnoDB",
                "CREATE TABLE pp_er_child (id INT NOT NULL, parent_id INT, \
                 PRIMARY KEY (id), CONSTRAINT pp_er_child_fk FOREIGN KEY (parent_id) \
                 REFERENCES pp_er_parent (id)) ENGINE=InnoDB",
            ]
        } else {
            vec![
                "CREATE TABLE pp_er_parent (id INTEGER NOT NULL, name TEXT, \
                 PRIMARY KEY (id))",
                "CREATE TABLE pp_er_child (id INTEGER NOT NULL, parent_id INTEGER, \
                 PRIMARY KEY (id), CONSTRAINT pp_er_child_fk FOREIGN KEY (parent_id) \
                 REFERENCES pp_er_parent (id))",
            ]
        };
        for sql in &ddl {
            dbui::exec_sql_now(&st, sql);
            pump(1500);
        }
        assert!(
            dbui::status_text(&st).contains("执行完成"),
            "建 ER 探针表失败：{}",
            dbui::status_text(&st)
        );

        // 打开子表的 ER 图 → 应当带出父表 + 一条关系
        assert!(
            dbui::open_er_now(&st, schema, Some("pp_er_child")),
            "ER 图应当能打开"
        );
        let mut graph = None;
        for _ in 0..120 {
            pump(300);
            if let Some(g) = dbui::er_graph_now(&st) {
                if !g.tables.is_empty() {
                    graph = Some(g);
                    break;
                }
            }
        }
        let g = graph.expect("ER 图应当有数据（超时没回来）");
        let mut names: Vec<&str> = g.tables.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        println!("[6.15] ER 图里的表：{names:?}，关系 {} 条", g.edges.len());
        assert_eq!(
            names,
            vec!["pp_er_child", "pp_er_parent"],
            "ER 图应当带上直接相关的两张表"
        );
        assert_eq!(g.edges.len(), 1, "应当有一条外键关系");
        assert_eq!(g.edges[0].from_table, "pp_er_child");
        assert_eq!(g.edges[0].to_table, "pp_er_parent");
        assert_eq!(g.edges[0].from_col, "parent_id");
        // 外键列要标出来，主键也要
        let child = g.table("pp_er_child").unwrap();
        let pid = child.columns.iter().find(|c| c.name == "parent_id").unwrap();
        assert!(pid.is_fk, "parent_id 应当标成外键列");
        let cid = child.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(cid.is_pk, "id 应当标成主键列");

        // 聚焦图的截图（ER 图最常看的用法：一张表跟别人的关系）
        if args.iter().any(|a| a == "--shot-er") {
            // 先关掉别的浮窗——对象属性 / 历史 / 搜索结果会盖住画布
            dbui::close_stats_now(&st);
            dbui::close_history_now(&st);
            dbui::close_search_now(&st);
            // ⚠️ 阶段 97：不用再收左侧文件面板了——它已经进了**终端子页内部**，
            // 数据库页本来就占满整页宽（画布自然占满宽度）。
            pump(800);
            shot("/tmp/er_diagram.png");
        }

        // 整个模式的 ER 图（也能画出来）
        assert!(
            dbui::open_er_now(&st, schema, None),
            "整个模式的 ER 图应当能打开"
        );
        let mut all = None;
        for _ in 0..120 {
            pump(300);
            if let Some(g) = dbui::er_graph_now(&st) {
                if g.tables.len() > 2 {
                    all = Some(g);
                    break;
                }
            }
        }
        let g2 = all.expect("整个模式的 ER 图应当能画出来");
        println!(
            "[6.15.1] 整个模式：{} 张表 · {} 条关系",
            g2.tables.len(),
            g2.edges.len()
        );
        assert!(g2.tables.len() > 2, "整个模式应当有多张表");
        assert!(g2.edges.len() >= 1, "整个模式应当有外键关系");

        // 截图：ER 图长什么样
        // （先关掉别的浮窗——对象属性 / 历史 / 搜索结果会盖住画布）
        if args.iter().any(|a| a == "--shot-er") {
            pump(600);
            shot("/tmp/er_schema.png");
        }

        // ---- 走右键菜单那条路（用户真正点的那条）----
        //
        // 前面用的是 `open_er_now`（直接调），**测不到右键菜单这一段**：
        // 菜单项建没建、敏感性对不对、点了会不会真开图。
        //
        // 树是懒加载的：刚用 SQL 建的探针表要刷新 + 展开模式才看得到。
        dbui::refresh_tree(&st);
        pump(2500);
        expand_first_schema(&st);
        let rows = tree_rows(&st);
        assert!(
            rows.iter().any(|l| l.contains("pp_er_child")),
            "树里应当能看到探针表 pp_er_child：{rows:?}"
        );

        let tabs_before = dbui::er_tab_count(&st);
        assert!(
            dbui::tree_er_menu_now(&st, "pp_er_child"),
            "对象树里应当能弹出 pp_er_child 的右键菜单"
        );
        pump(400);
        let er_btn = find_button_by_label(&win, "ER 图…")
            .expect("右键菜单里应当有「ER 图…」这一项");
        assert!(er_btn.is_sensitive(), "表节点上「ER 图…」应当可点");
        er_btn.emit_clicked();
        pump(2500);
        let tabs_after = dbui::er_tab_count(&st);
        println!("[6.15.3] 右键菜单开 ER 图：页签 {tabs_before} → {tabs_after}");
        assert_eq!(
            tabs_after,
            tabs_before + 1,
            "点右键菜单里的「ER 图…」应当真开一个 ER 图页签"
        );
        // 菜单里点开的跟直接调的是同一张图（聚焦这张表的关系）
        let g3 = dbui::er_graph_now(&st).expect("菜单开出来的 ER 图应当有数据");
        assert_eq!(
            g3.tables.len(),
            2,
            "应当还是聚焦图：这张表 + 直接相关的表（{:?}）",
            g3.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        );

        // 模式节点上：ER 图按钮也该可点（画的是整个模式）
        assert!(
            dbui::tree_er_menu_now(&st, schema),
            "对象树里应当能弹出模式 {schema} 的右键菜单"
        );
        pump(400);
        let er_btn2 = find_button_by_label(&win, "ER 图…")
            .expect("模式节点的菜单里也应当有「ER 图…」");
        assert!(er_btn2.is_sensitive(), "模式节点上「ER 图…」也应当可点");
        er_btn2.emit_clicked();
        pump(2500);
        assert_eq!(
            dbui::er_tab_count(&st),
            tabs_after + 1,
            "模式节点上点「ER 图…」也应当开一张图"
        );
        let g4 = dbui::er_graph_now(&st).expect("模式开出来的 ER 图应当有数据");
        assert!(
            g4.tables.len() > 2,
            "模式节点开出来的应当是整张图（{} 张表）",
            g4.tables.len()
        );

        // 清理探针表（先删子表：它有外键）
        dbui::exec_sql_now(&st, "DROP TABLE pp_er_child");
        pump(1200);
        dbui::exec_sql_now(&st, "DROP TABLE pp_er_parent");
        pump(1200);
        println!("[6.15.2] ER 探针表已清理");
    }

    // ---- 阶段 52：数据传输（dbx 顶栏的「数据传输」）----
    //
    // 验三件事：
    // ① **向导的「对象」下拉里有那张表**（跟着对象树走，导哪张表先展开它）；
    // ② **导出的 CSV 是整张表**（表头 + 25 行，不止当前页）；
    // ③ **导出的 SQL 带建表语句**（搬到空库也能跑——只有 INSERT 是搬不动的）。
    {
        let schema = if proto == "mysql" { "demo" } else { "public" };

        // ① 向导
        dbui::open_transfer_now(&st);
        pump(400);
        let objs = dbtransfer::last_objects();
        println!("[9] 数据传输向导里的对象：{objs:?}");
        assert!(
            objs.iter().any(|(_, t)| t.contains("demo_users")),
            "向导里应当能选到 demo_users：{objs:?}"
        );
        let fmts = dbtransfer::last_formats();
        assert!(fmts.len() == 3, "格式下拉该有 CSV / SQL / JSON 三项：{fmts:?}");
        assert!(fmts.iter().any(|(id, _)| id == "csv"), "{fmts:?}");
        assert!(fmts.iter().any(|(id, _)| id == "sql"), "{fmts:?}");
        assert!(fmts.iter().any(|(id, _)| id == "json"), "{fmts:?}");
        dbtransfer::close_last();
        pump(300);

        // ② 导出 CSV
        let csv_path = "/tmp/pp_export_demo_users.csv";
        let _ = std::fs::remove_file(csv_path);
        dbui::start_export(&st, schema, "demo_users", csv_path, ExportFmt::Csv);
        let mut done = false;
        for _ in 0..150 {
            pump(200);
            if dbui::status_text(&st).contains("已导出") {
                done = true;
                break;
            }
        }
        assert!(done, "CSV 导出没完成：{}", dbui::status_text(&st));
        let text = std::fs::read_to_string(csv_path).expect("导出的 CSV 应当存在");
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let expect_rows = demo_rows.len();
        println!("[9.1] 导出的 CSV：{} 行（表头 1 + 数据 {expect_rows}）", lines.len());
        assert!(
            lines[0].contains("id") && lines[0].contains("name") && lines[0].contains("email"),
            "CSV 表头不对：{}",
            lines[0]
        );
        assert_eq!(
            lines.len(),
            expect_rows + 1,
            "CSV 应当是表头 1 行 + 数据 {expect_rows} 行（按真库里查到的行数算）"
        );
        // 数据内容也按真库里查到的值验（**不写死"张三"**：不同库的数据不一样）
        let name_idx = demo_cols.iter().position(|c| c == "name").unwrap_or(0);
        let first_name = demo_rows
            .first()
            .and_then(|r| r.get(name_idx))
            .cloned()
            .unwrap_or_default();
        assert!(
            !first_name.is_empty() && text.contains(&first_name),
            "CSV 里应当有数据（{first_name}）：{text}"
        );
        let _ = std::fs::remove_file(csv_path);

        // ③ 导出 SQL（带建表语句）
        let sql_path = "/tmp/pp_export_demo_users.sql";
        let _ = std::fs::remove_file(sql_path);
        dbui::start_export(&st, schema, "demo_users", sql_path, ExportFmt::Sql);
        let mut done = false;
        for _ in 0..150 {
            pump(200);
            if dbui::status_text(&st).contains("已导出") {
                done = true;
                break;
            }
        }
        assert!(done, "SQL 导出没完成：{}", dbui::status_text(&st));
        let text = std::fs::read_to_string(sql_path).expect("导出的 SQL 应当存在");
        println!("[9.2] 导出的 SQL 前 160 字：{:?}", &text.chars().take(160).collect::<String>());
        assert!(text.contains("CREATE TABLE"), "SQL 导出要带建表语句：{text}");
        assert_eq!(
            text.matches("INSERT INTO").count(),
            demo_rows.len(),
            "SQL 导出的 INSERT 条数应当等于表里的行数（{}）",
            demo_rows.len()
        );
        let _ = std::fs::remove_file(sql_path);
    }

    // ---- 阶段 13：新建数据库连接对话框（截图用）----
    if args.iter().any(|a| a == "--shot-dbconn") {
        // 阶段 29 起对话框**没有**「在哪个会话上连」的下拉框了：
        // 会话是**定死**的（从哪儿来就属于哪儿），所以这里传当前会话的键。
        let parent = win.clone();
        let sess = puttyplus::panel::dbconn::session_choices();
        println!("[7.5] 可选会话：{:?}", sess.iter().map(|(_, l)| l).collect::<Vec<_>>());
        let cur_key = puttyplus::backend::dbstore::session_key(&st.opts);
        puttyplus::panel::dbconn::ui_new(Some(&parent), &cur_key, Some(Rc::clone(&st)), |_k, _n| {});
        pump(800);
        shot("/tmp/stage13_dbconn.png");
    }

    // ---- 阶段 12：文件编辑器也是子标签（不是弹窗）----
    {
        let f = std::env::temp_dir().join(format!("pp-e2e-{}.sh", std::process::id()));
        std::fs::write(&f, "#!/bin/sh\necho hello from puttyplus\n").unwrap();
        let path = f.to_string_lossy().to_string();
        puttyplus::panel::fileedit::open_local(&st, &path, false);
        pump(800);
        let pages = st.sub_tabs().map(|s| s.nb.n_pages()).unwrap_or(0);
        println!("[8] 子标签页数：{pages}（终端 / 数据库 / 文件编辑器）");
        assert!(
            pages >= 3,
            "文件编辑器没挂到子标签上（子标签只有 {pages} 页）"
        );
        let _ = std::fs::remove_file(&f);
    }

    // 截图用：--data 切回数据页签（看「导出 SQL」按钮），默认留在结构页签
    if args.iter().any(|a| a == "--data") {
        dbui::show_tab(&st, 0);
        pump(500);
    }

    // 布局截图前：把浮窗关掉、切回数据库子标签。
    //
    // **为什么**：历史 / 搜索结果都是浮在主窗口上的独立窗口，会把左下角
    // 的子标签栏盖住——而那正是要验证的地方。截图要能看清「连接页 =
    // 左文件面板 + 右子标签（终端 / 数据库 / 文件）」这个形状。
    if args.iter().any(|a| a == "--shot-layout") {
        dbui::close_history_now(&st);
        dbui::close_search_now(&st);
        pump(400);
        // 切回数据库工作台那一页
        if let Some(ui) = st.db_ui.borrow().as_ref() {
            if let Some(sub) = st.sub_tabs() {
                sub.switch_to(ui.tab_id.get());
            }
        }
        pump(600);
        shot("/tmp/stage12_db_tab.png");
        // 切到文件编辑器那一页（编辑器也是子标签，不是弹窗）
        let file_tab = st.file_edits.borrow().first().map(|e| e.tab_id.get());
        if let (Some(sub), Some(tid)) = (st.sub_tabs(), file_tab) {
            sub.switch_to(tid);
            pump(600);
            shot("/tmp/stage12_file_tab.png");
            // 再切回数据库，方便看最终状态
            if let Some(ui) = st.db_ui.borrow().as_ref() {
                sub.switch_to(ui.tab_id.get());
            }
            pump(400);
        }
    }

    // 截图用：--shot <路径>（在同一个 X display 里调 scrot，不用另开一个显示）
    if let Some(i) = args.iter().position(|a| a == "--shot") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| "/tmp/pp.png".into());
        shot(&path);
    }

    println!("db_ui_e2e: OK");
    if hold {
        pump(30_000);
    }
}
