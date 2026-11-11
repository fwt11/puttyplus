//! **断线自愈**的冒烟（阶段 99）：失败 ⇒ 重连一次 ⇒ 重试原来的操作。
//!
//! 用户原话（2026-11-06）：
//! 「手动点击重连终端的时候，也触发 sftp 重连……现在 sftp 报了断开，显示
//! 断开了，但我重连了终端，sftp 没有重连，需要手动点一下重连。我希望改成
//! 自动的……就是在异常处理程序中重连一次，如果还是失败就报错，否则就继续
//! 执行原来的操作。总之就是凡是 sftp 因为没连接的失败都要进行一次重连。」
//!
//! 覆盖四件事：
//! 1. **SFTP 失败 ⇒ 自愈**：连接断了之后点目录 ⇒ 那条操作失败 ⇒ **不弹错**，
//!    而是自动重连（并记住"要重试哪一条"）；
//! 2. **连上 ⇒ 重试**：重连成功之后把**原来那条操作**再发一遍；
//! 3. **只重试一次**：重试又失败 ⇒ 这次才报错（不再无限重连）；
//! 4. **数据库同理**：连接断了 ⇒ 重连 ⇒ 重发那条查询。
//!
//! ⚠️ **不连真服务器**：事件是直接喂给面板的（`handle_event` 是公开的）。
//! 也**不泵主循环**——真连不上时 worker 会回一个「连接失败」，主循环一跑
//! 就会弹对话框（冒烟不需要那个）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::db::DbRows;
use puttyplus::panel::worker::PanelEvent;
use puttyplus::panel::{self, SftpPanel, dbui};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn main() {
    gtk::init().expect("gtk init");

    // 别碰用户真实配置
    let home = std::env::temp_dir().join(format!("pp-selfheal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", home.join("settings.ini"));
        std::env::set_var("PUTTYPLUS_DB_INI", home.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", home.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_HIST_INI", home.join("history.ini"));
        std::env::set_var("PUTTYPLUS_COLS_INI", home.join("columns.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", home.join("recent.ini"));
        std::env::set_var("PUTTYPLUS_VAULT", home.join("vault.bin"));
        std::env::set_var("PUTTYPLUS_CAPTURE_DIR", home.join("capture"));
    }

    // 故意指一个没人监听的端口：worker 连不上，也不会连上
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 9;
    opts.user = "nobody".into();
    let panel = SftpPanel::new(&opts);
    let st = panel.st.clone();

    /* ================= SFTP ================= */

    // ---- 1) 先假装连上了 ----
    panel::handle_event(&st, PanelEvent::Connected(Ok("/tmp".into())));
    assert!(st.conn_ok.get(), "喂了 Connected 之后应当是连着的");
    println!("[1] 连上了 ✓");

    // ---- 2) 会话断了 ----
    panel::handle_event(&st, PanelEvent::Disconnected);
    assert!(!st.conn_ok.get(), "断了之后 conn_ok 应当是 false");
    println!("[2] 断开 ✓");

    // ---- 3) 断了之后点目录 ⇒ 那条操作失败 ⇒ **不弹错，自动重连** ----
    //
    // ⚠️ 这一步模拟的是「点目录」：命令发出去了（重试的"怎么重发"也就
    // 记下来了），随后 worker 回了一个失败（连接断了）。
    panel::on_row_activated(&st, true, "sub", true, false);
    let list_id = st.last_list.get();
    panel::handle_event(
        &st,
        PanelEvent::Listed {
            id: list_id,
            path: "/tmp/sub".into(),
            silent: false,
            result: Err("连接已断开".into()),
            dir_mtime: 0,
        },
    );
    assert!(
        st.connecting.get(),
        "连接断了导致的失败应当**自动重连**（connecting 应当是 true），而不是弹个错"
    );
    let (_, text, _) = st.status.borrow().clone().expect("状态栏该有内容");
    assert!(
        text.contains("重新连接"),
        "状态栏没写「正在重新连接」：{text}"
    );
    println!("[3] SFTP 失败 ⇒ 自动重连（状态栏：{text}）✓");

    // ---- 3b) **重连本身失败** ⇒ 下一次操作仍然会再发起一次重连 ----
    //
    // ⚠️ 「只重试一次」那道闸门是防「重连成功 ⇒ 重试又失败 ⇒ 又重连」的
    // 无限循环的。**重连自己都没成功**就没进那个循环 ⇒ 不该占着用户的
    // 额度（否则用户再点目录只会看到一个错、连重连都不发起）。
    panel::handle_event(
        &st,
        PanelEvent::Connected(Err("连不上".into())),
    );
    assert!(!st.connecting.get(), "重连失败之后不该还挂着「正在连接」");
    panel::handle_event(
        &st,
        PanelEvent::Listed {
            id: st.last_list.get(),
            path: "/tmp/sub".into(),
            silent: false,
            result: Err("连接已断开".into()),
            dir_mtime: 0,
        },
    );
    assert!(
        st.connecting.get(),
        "上一次重连失败了，这一次**仍然该发起重连**（不能被闸门挡住）"
    );
    println!("[3b] 上次重连失败 ⇒ 下一次操作照样发起重连 ✓");

    // ---- 4) 连上 ⇒ **重试原来那条操作** ----
    panel::handle_event(&st, PanelEvent::Connected(Ok("/tmp".into())));
    assert!(!st.connecting.get(), "连上了就不该还在「正在连接」");
    assert!(
        st.remote_list_pending.get(),
        "重连成功之后应当把**原来那条操作**再发一遍（列表重发）"
    );
    println!("[4] 重连成功 ⇒ 原来那条操作被重发 ✓");

    // ---- 5) 重试又失败 ⇒ **这次才报错**（不再无限重连）----
    let list_id = st.last_list.get();
    panel::handle_event(
        &st,
        PanelEvent::Listed {
            id: list_id,
            path: "/tmp/sub".into(),
            silent: false,
            result: Err("连接已断开".into()),
            dir_mtime: 0,
        },
    );
    assert!(
        !st.connecting.get(),
        "已经重试过一次了，不该再发起重连（否则就是无限重连）"
    );
    println!("[5] 重试又失败 ⇒ 不再重连（这次才报错）✓");

    /* ================= 数据库 ================= */

    // ---- 6) 数据库连接断了 ⇒ 重连 + 重发那条查询 ----
    // ⚠️ **先把工作台建出来**（`on_open_db_page` 只建控件，不连库）
    panel::on_open_db_page(&st);
    dbui::mount_for_test(&st);
    // ⚠️ **得先选中一条连接**：重连用的是「当前这条连接」的配置
    dbui::set_conn_now(&st, "pg", "127.0.0.1", "5432", "postgres", "postgres", "");
    dbui::on_connected(&st, Ok("postgres@127.0.0.1:5432".into()));
    assert!(dbui::is_connected_now(&st), "数据库应当是连着的");
    // 跑一条查询（发出去 ⇒ 待办表里有了它）
    dbui::run_sql_now(&st, "select 1");
    let qid = dbui::last_pending_id(&st).expect("应当有待办请求");
    // 连接断了 ⇒ worker 发 DbLost
    panel::handle_event(&st, PanelEvent::DbLost { id: qid });
    let text = dbui::status_text(&st);
    assert!(
        text.contains("重新连接"),
        "数据库断线后状态栏没写「正在重新连接」：{text}"
    );
    assert!(
        !dbui::is_connected_now(&st),
        "断线之后数据库不该还显示连着"
    );
    println!("[6] 数据库断线 ⇒ 自动重连（状态栏：{text}）✓");

    // ---- 7) 连上 ⇒ **重发那条查询** ----
    dbui::on_connected(&st, Ok("postgres@127.0.0.1:5432".into()));
    assert!(
        dbui::busy_now(&st),
        "重连成功之后应当把**原来那条查询**再发一遍（busy = true）"
    );
    // ⚠️ **重发要把待办补登记**：重连时待办表被清过 ⇒ 不补的话，回包按
    // 待办表找落点会找不到 ⇒ **结果被丢掉**（用户看着就是"重连了，但那条
    // 查询没结果"）。
    assert!(
        dbui::pending_ids(&st).contains(&qid),
        "重发那条查询之后，待办表里应当还有它（回包要靠它找落点）：{:?}",
        dbui::pending_ids(&st)
    );
    println!("[7] 数据库重连成功 ⇒ 原来那条查询被重发（待办已补登记）✓");

    // ---- 7b) **自愈成功之后 ⇒ 闸门清掉 ⇒ 下一次断线照样自愈** ----
    //
    // ⚠️ 这条钉的是「只重试一次」那道闸门的**生命周期**：
    // 闸门如果只加不清 ⇒ 自愈成功一次之后它一直挂着 ⇒ 之后**每一次**断线
    // 都会被当成「重试后又失败」⇒ 直接报错、**永远不再重连**（用户会看到
    // 「数据库连接已断开，重连后仍然失败」却没人去重连）。
    dbui::on_rows(
        &st,
        qid,
        DbRows {
            page: 1,
            done: true,
            ..Default::default()
        },
    );
    // 再断一次
    panel::handle_event(&st, PanelEvent::DbLost { id: qid });
    let text = dbui::status_text(&st);
    assert!(
        text.contains("正在重新连接"),
        "自愈成功之后闸门应当清掉：下一次断线仍要自愈，而不是报「重连后仍然失败」：{text}"
    );
    println!("[7b] 自愈成功 ⇒ 闸门清掉 ⇒ 下次断线照样自愈 ✓");

    /* ================= 重连终端 ⇒ 顺带救回 SFTP ================= */

    // ---- 8) 点「重新连接终端」⇒ 断了的 SFTP 也自动重连 ----
    //
    // 用户原话：「我记得之前实现过，手动点击重连终端的时候，也触发 sftp
    // 重连……但我重连了终端，sftp 没有重连」。
    {
        let tabs = puttyplus::tabs::Tabs::new();
        let mut o = ConnOptions::default();
        o.proto = "ssh".into();
        o.host = "127.0.0.1".into();
        o.port = 9;
        o.user = "nobody".into();
        tabs.open(&o);
        let win = gtk::Window::new();
        win.set_child(Some(&tabs.notebook));
        win.present();
        pump(300);
        let tst = tabs.current_panel_state().expect("应有面板");
        let page = tabs.current_page_widget().expect("应有当前页");
        // SFTP 断了
        panel::handle_event(&tst, PanelEvent::Disconnected);
        assert!(!tst.conn_ok.get(), "SFTP 应当是断着的");
        // 点「重新连接终端」
        tabs.reconnect_term_page(&page);
        assert!(
            tst.connecting.get(),
            "重连终端 ⇒ 断了的 SFTP 也该自动重连（connecting 应当是 true）"
        );
        println!("[8] 重连终端 ⇒ 断了的 SFTP 也自动重连 ✓");
    }

    let _ = std::fs::remove_dir_all(&home);
    println!("selfheal_smoke: OK");
}
