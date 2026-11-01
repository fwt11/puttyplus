//! 侧栏版式冒烟（阶段 93）。
//!
//! 背景：用户 2026-11-01 看侧栏截图说「有点丑」。这一版把侧栏重新排了一遍：
//! - **组头** = 粗体标题 + 计数徽章（以前跟条目长得一样）；
//! - **条目** = 名字（正文色）+ 副标题（小号淡色），中间有分隔点
//!   （以前只隔两个空格，挤成一坨）；
//! - 图标与文字之间留了空隙（以前图标直接贴在字上）。
//!
//! 版式是**看得见**的东西，单测（`--lib`）里 GTK 用例会静默跳过 ⇒ 这些断言
//! 必须放**冒烟**（跑在主线程，`gtk::init()` 成功）。
//!
//! **不碰用户的真实配置**：ini 都指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sidebar_design_smoke

use gtk::prelude::*;

use puttyplus::connmgr::ConnEntry;
use puttyplus::recent::RecentConn;
use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

/// 在控件树里找第一个 `gtk::Entry`（侧栏的过滤框）。
fn find_entry(root: &impl IsA<gtk::Widget>) -> Option<gtk::Entry> {
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Some(e) = w.clone().downcast::<gtk::Entry>().ok() {
            return Some(e);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    None
}

fn entry(name: &str, host: &str, user: &str, port: i32, custom: bool) -> ConnEntry {
    ConnEntry {
        name: name.into(),
        host: host.into(),
        user: Some(user.into()),
        password: None,
        port,
        custom,
        proto: "ssh".into(),
        sercfg: None,
        forwards: Vec::new(),
        inproc_forwards: Vec::new(),
        sess: Default::default(),
        keyfile: None,
        auth_method: Default::default(),
    }
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 临时 ini：绝不碰用户真实配置 ----
    let conn_ini = std::env::temp_dir().join(format!("pp-sbdesign-conn-{}.ini", std::process::id()));
    let recent = std::env::temp_dir().join(format!("pp-sbdesign-recent-{}.ini", std::process::id()));
    let vault = std::env::temp_dir().join(format!("pp-sbdesign-vault-{}", std::process::id()));
    for f in [&conn_ini, &recent, &vault] {
        let _ = std::fs::remove_file(f);
    }
    unsafe {
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
    }
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", "测试跳板机");
        kf.set_string("conn1", "host", "10.0.0.1");
        kf.set_uint64("conn1", "port", 22);
        kf.set_string("conn1", "user", "root");
        kf.set_string("conn1", "custom", "true");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }
    puttyplus::recent::push(RecentConn {
        label: "srv-01".into(),
        host: "10.0.0.9".into(),
        port: 22,
        user: "root".into(),
        ref_name: None,
    });

    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs);
    sb.reload();
    pump(80);

    let rows = sb.tree_rows();
    println!("[0] 树上的行：");
    for (kind, name, sub, text) in &rows {
        println!("    kind={kind}  {name}  |  {sub}  |  {text}");
    }
    assert!(!rows.is_empty(), "树上没有行");

    // ---- ① 组头：粗体标题 + 计数徽章 ----
    let groups: Vec<_> = rows.iter().filter(|(k, ..)| *k == 0).collect();
    assert!(!groups.is_empty(), "树上没有组头行");
    for (_, name, sub, text) in &groups {
        assert!(
            text.starts_with("<b>"),
            "组头标题要粗体（以前跟条目长得一样）：{text}"
        );
        assert!(
            text.contains("background='"),
            "组头的计数要有徽章底色：{text}"
        );
        assert!(
            text.contains(sub.as_str()),
            "徽章里要有计数：{text}"
        );
        assert!(!name.is_empty(), "组头要有主标题");
    }
    println!("[1] {} 个组头都是「粗体标题 + 计数徽章」", groups.len());

    // ---- ② 条目：名字显式上正文色 + 分隔点 + 副标题小号淡色 ----
    let fg = puttyplus::theme::fg_hex();
    let items: Vec<_> = rows.iter().filter(|(k, ..)| *k > 0).collect();
    assert!(!items.is_empty(), "树上没有条目行");
    for (_, _, _, text) in &items {
        assert!(
            text.contains(&format!("foreground='{fg}'")),
            "名字要显式上正文色（阶段 91a 的教训：不能靠 GTK 默认色）：{text}"
        );
        assert!(text.contains('·'), "名字和副标题之间要有分隔点：{text}");
        assert!(
            text.contains("size='small'"),
            "副标题要小一号（跟名字分出层次）：{text}"
        );
    }
    println!("[2] {} 个条目都是「名字 + 分隔点 + 小号副标题」", items.len());

    // ---- ③ 主标题 / 副标题分开存（过滤搜的是纯文本）----
    let conn = rows
        .iter()
        .find(|(k, n, ..)| *k == 2 && n == "测试跳板机")
        .expect("树上没有「测试跳板机」");
    assert_eq!(conn.2, "root@10.0.0.1:22", "副标题应该是地址：{:?}", conn);
    assert!(
        !conn.1.contains('@'),
        "主标题里不该混进地址：{:?}",
        conn
    );
    println!("[3] 主标题/副标题分开存：{:?}", conn);

    // ---- ④ 过滤按**纯文本**搜，不搜 markup ----
    //
    // ⚠️ 这是本阶段最容易回归的一条：以前过滤搜的是 COL_TEXT（markup）
    // ⇒ 搜 "span" 会命中每一行（markup 里到处是标签），搜 "<" 永远搜不到。
    let filter = find_entry(&sb.widget).expect("侧栏里没有过滤框");
    filter.set_text("span");
    pump(30);
    let hit_span = sb.visible_rows();
    assert!(
        hit_span.iter().all(|(_, _, _, t)| !t.contains("span")),
        "不该有行命中"
    );
    let n_span = hit_span.len();
    filter.set_text("10.0.0.1");
    pump(30);
    let hit_addr = sb.visible_rows();
    assert!(
        hit_addr.iter().any(|(_, _, sub, _)| sub.contains("10.0.0.1")),
        "按地址搜要能搜到：{hit_addr:?}"
    );
    filter.set_text("");
    pump(30);
    let all = sb.visible_rows();
    println!("[4] 搜 \"span\" 剩 {n_span} 行（应接近全量），搜 \"10.0.0.1\" 剩 {} 行", hit_addr.len());
    assert!(
        n_span * 2 < all.len(),
        "搜 markup 标签名（span）命中了太多行：{n_span} / {}——过滤搜的是 markup 而不是纯文本",
        all.len()
    );

    // ---- ⑤ 图标与文字之间有间距 ----
    //
    // 以前 CellRendererPixbuf 的 padding 是 0 ⇒ 图标直接贴在字上。
    // 这里量的是渲染器属性（改回 0 就会挂）。
    {
        let pad = sb.icon_renderer().padding();
        assert!(
            pad.0 >= 4,
            "图标渲染器的水平内边距太小（图标会贴在字上）：{pad:?}"
        );
        assert!(
            pad.1 >= 2,
            "图标渲染器的垂直内边距太小（行与行贴在一起）：{pad:?}"
        );
        println!("[5] 图标渲染器内边距 {pad:?}（图标不贴字、行不贴行）");
    }

    for f in [&conn_ini, &recent, &vault] {
        let _ = std::fs::remove_file(f);
    }
    println!("sidebar_design_smoke: OK");
}
