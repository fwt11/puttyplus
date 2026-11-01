//! 阶段 72 冒烟：**连接后对象树默认展开 schema**（审计 P2-11）。
//!
//! **不连真库**——纯界面验证。
//!
//! 验四件事：
//! 1. 连上以后树只展开到模式这一层、一张表都看不见 ⇒ 用户第一眼以为"库里
//!    没表 / 加载失败"。现在**默认模式自动展开**（PG `public`、MySQL 当前库）；
//! 2. 自动展开的**效果**是发出了那个模式的表查询（表真的会显示出来）；
//! 3. **只**展开默认模式——系统模式（`pg_catalog` / `information_schema`）
//!    不展开（展开就是一屏系统表，比不展开还糟）；
//! 4. 默认模式不在列表里时（PG 删过 `public`）⇒ 展开**第一个非系统模式**，
//!    而不是对着空树发愣。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbschema_smoke

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::backend::db::DbTable;
use puttyplus::panel::dbtree::DbTree;
use puttyplus::panel::worker::{DbMetaPayload, DbMetaReq};

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

/// 造一棵树 + 一个记录请求的钩子。
fn make_tree(default_schema: Option<&str>) -> (Rc<DbTree>, Rc<std::cell::RefCell<Vec<String>>>) {
    let tree = DbTree::new();
    let reqs = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    {
        let reqs = reqs.clone();
        tree.set_request_handler(move |req| {
            reqs.borrow_mut().push(format!("{req:?}"));
        });
    }
    tree.set_default_schema(default_schema.map(|s| s.to_string()));
    (tree, reqs)
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpDbSchema")
        .build();
    app.connect_activate(|app| {
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // ---- 场景 1：默认模式是 `public` ----
        {
            let (tree, reqs) = make_tree(Some("public"));
            tree.reset("pg 127.0.0.1:5432/postgres", "PostgreSQL");
            pump(200);
            assert!(
                reqs.borrow().iter().any(|r| r.contains("Schemas")),
                "连接后没有发模式列表请求：{:?}",
                reqs.borrow()
            );
            // PG 的模式列表里 `public` 通常**排在最后**（按名字排）
            tree.fill(
                &DbMetaReq::Schemas,
                &DbMetaPayload::Schemas(vec![
                    "information_schema".to_string(),
                    "pg_catalog".to_string(),
                    "public".to_string(),
                ]),
            );
            pump(300);
            println!("请求：{:?}", reqs.borrow());
            // ② 效果：那个模式的表查询真的发出去了
            assert!(
                reqs.borrow()
                    .iter()
                    .any(|r| r.contains("Tables") && r.contains("public")),
                "默认模式 `public` 没被展开（表查询没发出去）：{:?}",
                reqs.borrow()
            );
            // ① 视图层：这一行确实展开着
            assert!(
                tree.is_expanded_by_name("public"),
                "`public` 这一行没展开（展开过没记上）"
            );
            // ③ 系统模式不展开
            assert!(
                !tree.is_expanded_by_name("information_schema"),
                "`information_schema` 不该被展开（展开就是一屏系统表）"
            );
            assert!(
                !tree.is_expanded_by_name("pg_catalog"),
                "`pg_catalog` 不该被展开"
            );
            // 展开之后表也挂上来了
            tree.fill(
                &DbMetaReq::Tables {
                    schema: "public".to_string(),
                },
                &DbMetaPayload::Tables(vec![DbTable {
                    schema: "public".to_string(),
                    name: "demo_users".to_string(),
                    is_view: false,
                }]),
            );
            pump(200);
            assert!(
                tree.is_expanded_by_name("public"),
                "表挂上来以后 `public` 仍然该是展开的"
            );
            println!("场景 1（默认模式自动展开）✓");
        }

        // ---- 场景 2：默认模式不在列表里 ⇒ 展开第一个非系统模式 ----
        {
            let (tree, reqs) = make_tree(Some("public"));
            tree.reset("pg 127.0.0.1:5432/postgres", "PostgreSQL");
            pump(200);
            reqs.borrow_mut().clear();
            tree.fill(
                &DbMetaReq::Schemas,
                &DbMetaPayload::Schemas(vec![
                    "information_schema".to_string(),
                    "pg_catalog".to_string(),
                    "sales".to_string(),
                ]),
            );
            pump(300);
            println!("场景 2 的请求：{:?}", reqs.borrow());
            assert!(
                reqs.borrow()
                    .iter()
                    .any(|r| r.contains("Tables") && r.contains("sales")),
                "默认模式不在列表里时该展开第一个非系统模式（sales）：{:?}",
                reqs.borrow()
            );
            assert!(
                !tree.is_expanded_by_name("information_schema"),
                "系统模式仍然不该被展开"
            );
            println!("场景 2（兜底展开）✓");
        }

        // ---- 场景 3：MySQL 的默认模式是当前库 ----
        {
            let (tree, reqs) = make_tree(Some("demo"));
            tree.reset("mysql 127.0.0.1:3306/demo", "MySQL");
            pump(200);
            reqs.borrow_mut().clear();
            tree.fill(
                &DbMetaReq::Schemas,
                &DbMetaPayload::Schemas(vec![
                    "information_schema".to_string(),
                    "mysql".to_string(),
                    "demo".to_string(),
                ]),
            );
            pump(300);
            println!("场景 3 的请求：{:?}", reqs.borrow());
            assert!(
                reqs.borrow()
                    .iter()
                    .any(|r| r.contains("Tables") && r.contains("demo")),
                "MySQL 该展开当前库（demo）：{:?}",
                reqs.borrow()
            );
            assert!(
                !tree.is_expanded_by_name("mysql"),
                "MySQL 的系统库（mysql）不该被展开"
            );
            println!("场景 3（MySQL 当前库）✓");
        }

        println!("dbschema_smoke: OK");
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}
