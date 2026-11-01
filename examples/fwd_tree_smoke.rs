//! 阶段 56/57 冒烟：端口转发作为**叶子**挂在连接下面（两棵树）+ 转发窗口按钮位置。
//!
//! 用户诉求（2026-10-29）：
//! 1. 「保存到连接配置」按钮贴着**下面的表单**，但它保存的是**上面的列表**——
//!    位置本身就是误导；而且这个行为到底合不合理，值得商榷。
//! 2. 连接管理器树与会话管理器树都**不显示端口转发** → 应该像数据库连接一样，
//!    作为叶子挂在连接下面。
//! 3. 正在连着的连接 / 数据库 / 转发，要在树上标出来。
//!
//! 本冒烟查：
//! - 自定义条目的**进程内**与**终端**转发规则，在两棵树里都是叶子（挂在连接下）；
//! - ssh config 别名的转发规则同样挂上；
//! - 转发窗口的保存按钮**贴着列表**（`pp-fwd-list-head`），不在表单旁边；
//! - 按钮文字说清「保存的是列表」。
//!
//! **不碰用户真实配置**：HOME 与所有 ini 都指到临时目录，退出时删掉。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fwd_tree_smoke

use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel};

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

/// 退出时把临时文件删掉（别往用户配置里塞东西）。
struct Cleanup(Vec<std::path::PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 假的别名（名字取得很偏，避免跟用户真配置里的别名撞上；
/// IP 用 TEST-NET-3 保留段 203.0.113.0/24，不指向任何真机器）。
const ALIAS: &str = "pp-smoke-alias";
const ALIAS_IP: &str = "203.0.113.9";

fn main() {
    // ⚠️ **HOME 必须在任何 glib 调用之前改**：glib 的 g_get_home_dir()
    // 第一次调用就把结果缓存了，之后再改 HOME 不生效。
    // 别名清单从 HOME/.ssh/config 读——所以这里换成一个假 HOME。
    let home = std::env::temp_dir().join(format!("pp-fwd-tree-smoke-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(
        home.join(".ssh").join("config"),
        format!("Host {ALIAS}\n  HostName {ALIAS_IP}\n  User tester\n"),
    )
    .unwrap();
    unsafe { std::env::set_var("HOME", &home) };

    let conn_ini =
        std::env::temp_dir().join(format!("pp-fwd-tree-smoke-{}.ini", std::process::id()));
    let db_ini =
        std::env::temp_dir().join(format!("pp-fwd-tree-smoke-db-{}.ini", std::process::id()));
    let settings_ini =
        std::env::temp_dir().join(format!("pp-fwd-tree-smoke-set-{}.ini", std::process::id()));
    for p in [&conn_ini, &db_ini, &settings_ini] {
        let _ = std::fs::remove_file(p);
    }
    unsafe {
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini);
    }
    let _cleanup = Cleanup(vec![conn_ini.clone(), db_ini.clone(), settings_ini.clone()]);

    // 一条自定义连接（带两套转发规则）+ 别名的进程内规则
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", "测试跳板机");
        kf.set_string("conn1", "host", "10.0.0.1");
        kf.set_uint64("conn1", "port", 22);
        kf.set_string("conn1", "user", "root");
        // 终端承接的一套
        kf.set_string("conn1", "forwardings", "L:9000:10.0.0.1:80");
        // 进程内的一套（两条：一条启用一条停用）
        kf.set_string("conn1", "inprocess_forwardings", "D:1080,!R:9200:10.0.0.2:22");
        // 别名的进程内规则（阶段 55 的落点）
        kf.set_string("ssh_alias_forwards", ALIAS, "L:7000:10.0.0.3:80");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }

    gtk::init().expect("gtk init");

    // ⚠️ 阶段 77b：连接管理器删了（连接操作全收敛到侧栏）⇒ 转发规则的
    // **唯一**的树就是侧栏这棵。规则键的断言挪到下面（用侧栏的钩子）。


    // ---- 2. 会话管理器树：转发规则也是叶子 ----
    let tabs = puttyplus::tabs::Tabs::new();
    let sb = puttyplus::sidebar::Sidebar::new(tabs.clone());
    sb.reload();
    let rows = sb.fwd_rows();
    println!("[2] 会话管理器树里的转发叶子：");
    for (text, tip) in &rows {
        println!("    {text}  |  {tip}");
    }
    assert_eq!(rows.len(), 4, "会话树里的转发叶子数不对：{rows:?}");
    assert!(
        rows.iter().any(|(t, _)| t.contains("SOCKS")),
        "会话树里没有 SOCKS 规则：{rows:?}"
    );
    assert!(
        rows.iter().any(|(t, _)| t.contains("7000")),
        "会话树里没有别名的规则：{rows:?}"
    );
    // 停用的规则要标出来
    assert!(
        rows.iter().any(|(t, _)| t.contains("已停用")),
        "停用的规则没标出来：{rows:?}"
    );
    println!("✓ 会话管理器树：转发规则挂在连接下面");

    // 规则键要挂在对的行上（右键菜单靠它找规则）
    let keys = sb.fwd_rule_keys();
    println!("[2.5] 会话树里的转发规则键：");
    for (ssh, key, text) in &keys {
        println!("    {ssh}  →  {key}  ({text})");
    }
    assert_eq!(keys.len(), 4, "规则键数不对：{keys:?}");
    assert!(
        keys.iter()
            .any(|(s, k, _)| s.contains("测试跳板机") && k == "D:1080"),
        "SOCKS 规则的键挂错了：{keys:?}"
    );
    assert!(
        keys.iter()
            .any(|(s, k, _)| s.contains("测试跳板机") && k == "L:9000:10.0.0.1:80"),
        "终端规则的键挂错了：{keys:?}"
    );
    assert!(
        keys.iter()
            .any(|(s, k, _)| s.contains(ALIAS) && k == "L:7000:10.0.0.3:80"),
        "别名规则的键挂错了：{keys:?}"
    );
    println!("✓ 规则键挂在对的行上");

    // ---- 3. 转发窗口：保存按钮贴着列表（不再在表单旁边）----
    let mut opts = ConnOptions::default();
    opts.host = "10.0.0.1".into();
    opts.user = "root".into();
    opts.port = 22;
    opts.title = Some("测试跳板机".into());
    let panel = SftpPanel::new(&opts);
    let st = &panel.st;
    panel::on_open_forwards(st);
    pump(400);

    assert!(
        puttyplus::panel::fwdmgr::save_button_in_list_area(st),
        "「保存」按钮要贴着规则列表（它保存的是列表，不是下面表单里的内容）"
    );
    let label = puttyplus::panel::fwdmgr::save_button_label(st);
    assert_eq!(label, "保存列表到连接配置", "按钮文字要说清保存的是列表");
    println!("✓ 转发窗口：保存按钮贴着列表，文字是「{label}」");

    // ---- 4. 活跃标记：连上的连接 / 数据库 / 转发在树上点亮 ----
    //
    // 模拟一个"正连着"的会话：真连服务器不现实（也没必要），这里直接把
    // 面板状态改成「已连接」，再挂一条在跑的转发和一个连着库的数据库连接。
    let key = "s:测试跳板机";
    puttyplus::backend::dbstore::save(
        key,
        &puttyplus::backend::dbstore::DbProfile {
            name: "本地 PG".into(),
            proto: puttyplus::backend::db::DbProto::Postgres,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "root".into(),
            dbname: "postgres".into(),
            remember: false,
        },
    );
    let mut opts2 = ConnOptions::default();
    opts2.host = "127.0.0.1".into();
    opts2.user = "root".into();
    opts2.port = 1; // 故意连不上：我们要的是"面板状态"，不是真的连上
    opts2.title = Some("测试跳板机".into());
    // ⚠️ 自定义条目的会话键是 `s:<名字>`——面板这边也得带上 session_name，
    // 否则两边算出的键不一样，树上的点就不会亮（这正是阶段 13 踩过的坑）。
    opts2.session_name = Some("测试跳板机".into());
    tabs.open(&opts2);
    pump(300);
    let live = tabs.current_panel_state().expect("要开出一个面板");
    *live.status.borrow_mut() = Some((
        puttyplus::theme::State::Ok,
        "已连接".into(),
        "已连接".into(),
    ));
    *live.fwd_views.borrow_mut() = vec![puttyplus::panel::worker::FwdView {
        key: "D:1080".into(),
        text: "SOCKS 127.0.0.1:1080".into(),
        enabled: true,
        active: true,
        err: None,
    }];
    *live.db_active.borrow_mut() = Some("本地 PG".into());

    sb.reload();
    let entry_rows = sb.entry_rows();
    println!("[4] 会话树里的 SSH 连接行：");
    for (text, tip) in &entry_rows {
        println!("    {text}  |  {tip}");
    }
    let ssh = entry_rows
        .iter()
        .find(|(t, _)| t.contains("测试跳板机"))
        .expect("要有「测试跳板机」这一行");
    // ⚠️ 阶段 82（P2-6）：状态点是**形状 + 颜色**双通道 —— 已连接 `●` /
    // 警告 `◐` ⇒ 这里**不能写死「●」**：这个会话的聚合状态是 Warn
    // （终端已结束）⇒ 形状就是 ◐。以前形状恒是 ●、只靠颜色说话，断言才
    // 能写死；现在形状本身带状态 ⇒ 断言跟着状态走。
    assert!(
        ['●', '◐'].iter().any(|g| ssh.0.contains(*g)),
        "连上了的行首要有状态点（● 已连接 / ◐ 警告）：{:?}",
        ssh.0
    );
    assert!(ssh.1.contains("已连接"), "悬停提示要写明状态：{:?}", ssh.1);
    // 数据库连着 → 点亮
    let db_rows = sb.db_rows();
    let db = db_rows
        .iter()
        .find(|(t, _)| t.contains("本地 PG"))
        .expect("要有「本地 PG」这一行");
    assert!(db.0.contains('●'), "连着库的数据库要点亮：{:?}", db.0);
    assert!(db.0.contains("已连接"), "摘要要写明已连接：{:?}", db.0);
    // 转发在跑 → 标「运行中」
    let fwd = sb
        .fwd_rows()
        .into_iter()
        .find(|(t, _)| t.contains("SOCKS"))
        .expect("要有 SOCKS 规则这一行");
    assert!(fwd.0.contains('●'), "在跑的转发要点亮：{:?}", fwd.0);
    assert!(fwd.0.contains("运行中"), "在跑的转发要标「运行中」：{:?}", fwd.0);
    // 停用的规则不许冒充在跑
    let off = sb
        .fwd_rows()
        .into_iter()
        .find(|(t, _)| t.contains("已停用"))
        .expect("要有停用规则这一行");
    assert!(!off.0.contains('●'), "停用的规则不该点亮：{:?}", off.0);
    assert!(!off.0.contains("运行中"), "停用的规则不该标运行中：{:?}", off.0);
    println!("✓ 活跃标记：连接 / 数据库 / 转发都按状态点亮了");

    println!("[OK] 转发叶子 + 活跃标记 + 保存按钮位置都对");
}
