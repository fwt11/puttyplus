//! 阶段 51 冒烟：**空态结构化 + 对象树面板增强**（对标 dbx 的对象树面板）。
//!
//! **不连真库**——纯界面验证，所以远端数据库连不上也能跑。
//!
//! 验六件事：
//! 1. 空态是**品牌页**（阶段 97 重做）：应用图标 + 产品名「PuTTYPlus」+
//!    「数据库工作台」；没连上时只有主 CTA「新建连接」，**不再摆**
//!    「新建查询」「搜索对象」（用户 2026-11-02：那两个点了跟没点一样）；
//!    连上之后只剩三个**看得见效果**的动作：浏览对象 / 写一条查询 / 执行历史；
//! 2. 树面板工具行在：「显示」下拉（6 项）+「详细」开关；
//! 3. 「详细」开关走**真实控件**（`set_active` → `toggled` 回调），
//!    树那边的状态跟着变；
//! 4. 详细模式把统计填进**说明列**：`表 · ≈1.2 万 行 · 1.0 MB · 2026-10-20`；
//! 5. 关掉详细模式，说明列**还原**成「表」——不然用户以为开关没生效；
//! 6. 类型过滤走**真实下拉**：选「视图」之后，表节点没了；选回「全部」又回来。
//!
//! ⚠️ 树上灌的是**假数据**（直接调 `DbTree::fill`），但走的是树自己的
//! 过滤 / 填统计逻辑，不是把模型改了了事。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example tree_panel_smoke

#![allow(deprecated)] // 结果表沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::db::DbTable;
use puttyplus::backend::dbmeta::TableStat;
use puttyplus::panel::dbtree::{DbTree, TreeFilter};
use puttyplus::panel::worker::{DbMetaPayload, DbMetaReq};
use puttyplus::panel::{self, SftpPanel};

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

/// 在控件树里找带指定 CSS 类的**所有** widget（阶段 96：量一列行是否等宽）。
fn find_all_by_class(w: &gtk::Widget, class: &str) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    if w.css_classes().iter().any(|x| x == class) {
        out.push(w.clone());
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        out.extend(find_all_by_class(&c, class));
        cur = c.next_sibling();
    }
    out
}

/// 控件树里**第一个** Label 的文字（自绘按钮：文字在按钮里的 Box 深处）。
fn first_label(w: &gtk::Widget) -> String {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return l.text().to_string();
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        let t = first_label(&x);
        if !t.is_empty() {
            return t;
        }
        c = x.next_sibling();
    }
    String::new()
}

/// 在空态那几行里按文字点一下（阶段 97：验"点了真的有效果"）。
fn click_empty_action(page: &gtk::Widget, label: &str) -> bool {
    for b in find_all_by_class(page, "pp-empty-btn") {
        if first_label(&b) != label {
            continue;
        }
        let Some(btn) = b.downcast_ref::<gtk::Button>() else {
            continue;
        };
        btn.emit_clicked();
        return true;
    }
    false
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

/// 退出时把临时 ini 删掉（别往用户配置里塞假连接）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 往树上灌一份假数据（一个模式 + 一张表 + 一个视图）。
fn fill_tree(tree: &DbTree) {
    tree.fill(
        &DbMetaReq::Schemas,
        &DbMetaPayload::Schemas(vec!["public".to_string()]),
    );
    tree.fill(
        &DbMetaReq::Tables {
            schema: "public".to_string(),
        },
        &DbMetaPayload::Tables(vec![
            DbTable {
                schema: "public".to_string(),
                name: "users".to_string(),
                is_view: false,
            },
            DbTable {
                schema: "public".to_string(),
                name: "v_users".to_string(),
                is_view: true,
            },
        ]),
    );
}

/// 灌一份统计（详细模式应答）。
fn fill_stats(tree: &DbTree) {
    tree.fill(
        &DbMetaReq::TableStats {
            schema: "public".to_string(),
        },
        &DbMetaPayload::TableStats(vec![
            TableStat {
                name: "users".to_string(),
                rows: 12345,
                size_bytes: 1048576,
                updated: "2026-10-20 14:33:12".to_string(),
            },
            TableStat {
                name: "v_users".to_string(),
                rows: 12345,
                size_bytes: 0,
                updated: String::new(),
            },
        ]),
    );
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpTreePanel")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类（.pp-empty…）只有 theme::apply()
        // 之后才生效——不装主题的话截图里那张卡片是"没上妆"的默认样式。
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // 别碰用户的真实配置
        let db_ini = std::env::temp_dir().join(format!(
            "pp-tree-panel-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&db_ini);
        unsafe { std::env::set_var("PUTTYPLUS_DB_INI", &db_ini) };
        let _guard = IniGuard(db_ini.clone());

        // ⚠️ **先存一条连接配置**：空态在"没连上"时要列出这个会话下的连接
        // （阶段 97：连上了才不摆那一列）。
        let opts = ConnOptions::default();
        let key = puttyplus::backend::dbstore::session_key(&opts);
        puttyplus::backend::dbstore::save(
            &key,
            &puttyplus::backend::dbstore::DbProfile {
                name: "本地 PG".into(),
                proto: puttyplus::backend::db::DbProto::Postgres,
                host: "127.0.0.1".into(),
                port: 5432,
                user: "postgres".into(),
                dbname: "postgres".into(),
                remember: false,
            },
        );

        let panel = SftpPanel::new(&opts);
        // ⚠️ **不给面板另开窗口**：工作台在"没有子标签容器"时会**自己开一个
        // 窗口**（`dbui::ensure` 的兜底路径），那才是要截图的东西。
        panel::on_open_db(&panel.st);
        assert!(panel.st.db_ui.borrow().is_some(), "数据库工作台没建起来");
        pump(400);

        // ---- 断言 1：空态是**品牌页**（阶段 97 重做）----
        //
        // ⚠️ 用户 2026-11-02：「新建查询」「搜索对象」点了没有任何作用，
        // 不知道他们的目的是什么」「这个会话的数据库连接放在这里也没什么用」
        // 「不知道该显示啥，也许显示个漂亮的 logo？图片？艺术字？」
        // ⇒ 现在：招牌 = **应用图标 + 产品名 + 这一屏是什么**；
        //   下面的动作**每一个都得看得见效果**。
        let texts = puttyplus::panel::dbui::empty_state_text(&panel.st);
        println!("空态文字：{texts:?}");
        assert!(
            texts.iter().any(|t| t == "PuTTYPlus"),
            "空态没有产品名（品牌页的招牌）：{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "数据库工作台"),
            "空态没有「数据库工作台」：{texts:?}"
        );
        // ⚠️ **「新建查询」「搜索对象」删掉了**：搜索框就在对象树顶上
        // （树一摆出来就看见了），再摆一个"聚焦那个框"的按钮毫无意义。
        assert!(
            !texts.iter().any(|t| t == "新建查询" || t == "搜索对象"),
            "空态不该再有「新建查询」「搜索对象」（点了跟没点一样）：{texts:?}"
        );
        // 没连上 ⇒ 首页**只有招牌**（用户 2026-11-02）：
        //   「这个会话的数据库连接」那一列删了（进到这一屏说明已经选好了），
        //   「新建连接」按钮也删了（"首页上没必要有这个按钮"）。
        let btns = puttyplus::panel::dbui::empty_state_buttons(&panel.st);
        println!("空态按钮：{btns:?}");
        assert!(
            btns.is_empty(),
            "没连上时首页不该有任何按钮：{btns:?}"
        );
        for gone in ["新建连接", "浏览对象", "写一条查询", "执行历史"] {
            assert!(
                !texts.iter().any(|t| t == gone),
                "首页不该出现「{gone}」：{texts:?}"
            );
        }
        // 没连上 ⇒ 也不该有那一列连接
        assert!(
            !texts.iter().any(|t| t == "这个会话下的数据库连接"),
            "首页不该再摆「这个会话的数据库连接」那一列：{texts:?}"
        );
        println!("空态（没连上）✓ 只有品牌页");

        // ---- 断言 1b：切到"连上了"：三个动作都在，且**都能点** ----
        //
        // 阶段 97：连上之后空态只剩「浏览对象 / 写一条查询 / 执行历史」，
        // 三个都是**看得见效果**的（用户说之前的「新建查询」「搜索对象」
        // 点了跟没点一样）。
        assert!(
            puttyplus::panel::dbui::empty_state_set_connected(&panel.st, "本地 PG"),
            "切到「已连接」状态失败"
        );
        pump(200);
        let texts = puttyplus::panel::dbui::empty_state_text(&panel.st);
        println!("连上后的空态文字：{texts:?}");
        let btns = puttyplus::panel::dbui::empty_state_buttons(&panel.st);
        println!("连上后的空态按钮：{btns:?}");
        for want in ["浏览对象", "写一条查询", "执行历史"] {
            let b = btns.iter().find(|(t, _)| t == want);
            assert!(b.is_some(), "连上后空态没有「{want}」：{btns:?}");
            assert!(
                b.map(|(_, s)| *s).unwrap_or(false),
                "「{want}」不该是灰的（点了要有效果）：{btns:?}"
            );
        }
        // ⚠️ 连上了就**不该**再摆那一列连接（用户说没用：能进到这一屏
        // 说明他已经选好了 ⇒ 一列选择器是多余的）。
        assert!(
            !btns.iter().any(|(t, _)| t == "本地 PG"),
            "连上后不该再摆「这个会话的数据库连接」那一列：{btns:?}"
        );
        // 也不该有「新建连接」（用户 2026-11-02：首页上没必要有它）
        assert!(
            !btns.iter().any(|(t, _)| t == "新建连接"),
            "首页不该有「新建连接」：{btns:?}"
        );
        // 当前连的是哪个库 ⇒ 一个 chip（只表示状态）
        assert!(
            texts.iter().any(|t| t.contains("本地 PG")),
            "空态没说当前连的是哪个库：{texts:?}"
        );
        println!("空态（连上了）✓ 三个动作 + 当前库 chip");

        // ---- 断言 1c：一列行**等宽**（阶段 96）----
        //
        // 以前每颗按钮按自己的文字宽度排 ⇒ 一列参差的锯齿 ⇒ "看着丑"。
        let page = {
            let slot = panel.st.db_ui.borrow();
            let Some(ui) = slot.as_ref() else {
                panic!("数据库工作台没建起来");
            };
            ui.page.clone()
        };
        let rows = find_all_by_class(&page, "pp-empty-btn");
        assert!(rows.len() >= 3, "空态行太少：{}", rows.len());
        let w0 = rows[0].allocation().width();
        for r in &rows {
            let w = r.allocation().width();
            assert_eq!(w, w0, "空态各行宽度不一致（{w} vs {w0}）⇒ 一列参差");
        }
        println!("空态行等宽 ✓（{} 行）", rows.len());

        // ---- 断言 1d：卡片**真的居中**（阶段 96）----
        //
        // ⚠️ 这条是被用户的截图逼出来的：以前卡片一直**贴着主区左边**，
        // 因为 GtkBox 给非 FILL 的子控件按**左边缘**摆（halign=CENTER 在
        // Box 里根本不居中）⇒ 宽屏下看着像"贴边的小纸条"。
        let card = find_by_class(&page, "pp-empty").expect("找不到空态卡片");
        let parent = card.parent().expect("卡片没有父容器");
        let (cx, _) = card
            .translate_coordinates(&parent, 0.0, 0.0)
            .expect("换算坐标失败");
        let pw = parent.allocation().width();
        let cw = card.allocation().width();
        let want = (pw as f64 - cw as f64) / 2.0;
        println!("空态卡：左 {cx:.0} 宽 {cw}（父容器宽 {pw}，居中应为 {want:.0}）");
        assert!(
            (cx - want).abs() <= 2.0,
            "空态卡没居中：左 {cx:.0}，应为 {want:.0}（父宽 {pw} 卡宽 {cw}）"
        );

        // ---- 断言 1e：三个动作**点了真的有效果**（阶段 97 的核心）----
        //
        // 用户 2026-11-02：「新建查询」「搜索对象」点了没有任何作用，
        // 不知道他们的目的是什么」⇒ 留下的动作必须**一个一个验过**：
        // 点下去之后，界面上必须**看得见**变化。
        //
        // · 「浏览对象」→ 对象树**摆出来**（它可能正收着！）
        puttyplus::panel::dbui::set_tree_collapsed(&panel.st, true);
        pump(100);
        assert!(
            puttyplus::panel::dbui::tree_collapsed(&panel.st),
            "对象树没收起来（后面那步就验不出来了）"
        );
        assert!(
            click_empty_action(&page, "浏览对象"),
            "空态里找不到「浏览对象」那颗按钮"
        );
        pump(200);
        assert!(
            !puttyplus::panel::dbui::tree_collapsed(&panel.st),
            "点了「浏览对象」对象树还是收着的 ⇒ 又是「点了没反应」"
        );
        assert!(
            puttyplus::panel::dbui::tree_col_visible(&panel.st),
            "点了「浏览对象」对象树那一列没露出来"
        );

        // · 「写一条查询」→ 下面那个编辑器**清空**（它是**一直可见**的）
        puttyplus::panel::dbui::sql_set_text(&panel.st, "select 1");
        assert_eq!(
            puttyplus::panel::dbui::sql_text(&panel.st),
            "select 1",
            "往编辑器里写字没写上"
        );
        assert!(
            click_empty_action(&page, "写一条查询"),
            "空态里找不到「写一条查询」那颗按钮"
        );
        pump(200);
        assert_eq!(
            puttyplus::panel::dbui::sql_text(&panel.st),
            "",
            "点了「写一条查询」编辑器没清空 ⇒ 点了跟没点一样"
        );

        // · 「执行历史」→ **弹出历史窗口**
        assert!(
            !puttyplus::panel::dbui::history_window_open(&panel.st),
            "历史窗口一开始就是开着的（后面那步就验不出来了）"
        );
        assert!(
            click_empty_action(&page, "执行历史"),
            "空态里找不到「执行历史」那颗按钮"
        );
        pump(400);
        assert!(
            puttyplus::panel::dbui::history_window_open(&panel.st),
            "点了「执行历史」没弹出历史窗口"
        );
        println!("三个动作都真的有效果 ✓");

        // ---- 断言 2：树面板的工具行 ----
        assert_eq!(
            puttyplus::panel::dbui::tree_filter_id(&panel.st),
            "all",
            "类型过滤一开始该是「全部」"
        );
        assert!(
            !puttyplus::panel::dbui::tree_detail_mode(&panel.st),
            "「详细」一开始不该是开的"
        );
        println!("树面板工具行 ✓");

        // ---- 灌假数据 ----
        let tree = puttyplus::panel::dbui::tree(&panel.st).expect("应当能拿到对象树");
        tree.reset("pg 127.0.0.1:5432/postgres", "PostgreSQL");
        pump(200);
        fill_tree(&tree);
        pump(200);
        let rows = puttyplus::panel::dbui::tree_rows(&panel.st);
        println!("树上的行：{rows:?}");
        assert!(
            rows.iter().any(|(n, _)| n == "users"),
            "表节点没挂上：{rows:?}"
        );
        assert!(
            rows.iter().any(|(n, _)| n == "v_users"),
            "视图节点没挂上：{rows:?}"
        );
        // 没开详细模式时，说明列就是「表」/「视图」
        let users_detail = rows
            .iter()
            .find(|(n, _)| n == "users")
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        assert_eq!(users_detail, "表", "没开详细模式时说明列该是「表」：{users_detail}");

        // ---- 断言 3：详细开关走真实控件 ----
        puttyplus::panel::dbui::tree_set_detail_now(&panel.st, true);
        pump(200);
        assert!(
            puttyplus::panel::dbui::tree_detail_mode(&panel.st),
            "「详细」开关点了没生效"
        );
        assert!(
            tree.detail_mode(),
            "树那边的详细模式状态没跟着变"
        );
        println!("详细开关 ✓");

        // ---- 断言 4：统计填进说明列 ----
        fill_stats(&tree);
        pump(200);
        let rows = puttyplus::panel::dbui::tree_rows(&panel.st);
        println!("详细模式下的行：{rows:?}");
        let users_detail = rows
            .iter()
            .find(|(n, _)| n == "users")
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        assert_eq!(
            users_detail, "表 · ≈1.2 万 行 · 1.0 MB · 2026-10-20",
            "说明列没填上统计：{users_detail}"
        );
        // 视图没有大小也没有更新时间 → 那两段不写
        let view_detail = rows
            .iter()
            .find(|(n, _)| n == "v_users")
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        assert_eq!(view_detail, "视图 · ≈1.2 万 行", "视图说明列不对：{view_detail}");
        println!("统计填进说明列 ✓");

        // ---- 断言 5：关掉详细模式要还原 ----
        puttyplus::panel::dbui::tree_set_detail_now(&panel.st, false);
        pump(200);
        let rows = puttyplus::panel::dbui::tree_rows(&panel.st);
        let users_detail = rows
            .iter()
            .find(|(n, _)| n == "users")
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        assert_eq!(
            users_detail, "表",
            "关掉详细模式后说明列没还原：{users_detail}"
        );
        println!("关掉详细模式会还原 ✓");

        // ---- 断言 6：类型过滤走真实下拉 ----
        puttyplus::panel::dbui::tree_set_filter_now(&panel.st, "views");
        pump(200);
        assert_eq!(
            puttyplus::panel::dbui::tree_filter_id(&panel.st),
            "views",
            "下拉没切过去"
        );
        assert_eq!(tree.filter(), TreeFilter::Views, "树那边的过滤没跟着变");
        // 过滤变了 → 树要重建 → 重新灌一遍数据
        fill_tree(&tree);
        pump(200);
        let rows = puttyplus::panel::dbui::tree_rows(&panel.st);
        println!("只看视图时的行：{rows:?}");
        assert!(
            rows.iter().any(|(n, _)| n == "v_users"),
            "过滤成「视图」后视图没了：{rows:?}"
        );
        assert!(
            !rows.iter().any(|(n, _)| n == "users"),
            "过滤成「视图」后表还在：{rows:?}"
        );

        // 切回「全部」→ 两个都在
        puttyplus::panel::dbui::tree_set_filter_now(&panel.st, "all");
        pump(200);
        fill_tree(&tree);
        pump(200);
        let rows = puttyplus::panel::dbui::tree_rows(&panel.st);
        assert!(
            rows.iter().any(|(n, _)| n == "users") && rows.iter().any(|(n, _)| n == "v_users"),
            "切回「全部」后对象没回来：{rows:?}"
        );
        println!("类型过滤 ✓");

        // ---- 截图：把空态 + 树面板的样子留下来 ----
        {
            // 空态要看得到：把树清空、回到「没有页签」的状态
            tree.clear();
            pump(400);
            // ⚠️ **截图要等主循环转起来**：窗口刚 present 时还没画过，
            // 截出来是一片黑（看着像"界面没出来"）。
            pump(800);
            // ⚠️ **先把旧图删掉**：scrot 不覆盖同名文件，会自动加 _000/_001
            // 的后缀——下次再看 /tmp/tree_panel.png 看到的是**上一次**的图。
            let _ = std::fs::remove_file("/tmp/tree_panel.png");
            let st = std::process::Command::new("/usr/bin/scrot")
                .arg("/tmp/tree_panel.png")
                .status();
            println!("截图 /tmp/tree_panel.png：{st:?}");
        }

        println!("tree_panel_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
