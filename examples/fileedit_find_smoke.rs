//! 编辑器查找条的两件事（用户 2026-11-17 报的）。
//!
//! 1. **搜索时不要左右移动**：用户原话「被搜索的内容都跑到屏幕最左边了，
//!    希望不要左右移动」——那是 `scroll_to_iter` 的 `use_align=true` +
//!    `x_offset=0` 干的（把匹配**对齐到视口左边** ⇒ 行首被顶出屏幕）。
//! 2. **查找框里按回车 ⇒ 跳到下一个匹配**。
//!
//! **断言**（都落在「动作的结果」上，不是控件属性上）：
//! 1. 点「下一个」⇒ 匹配序号往前走、选中的就是那一段匹配；
//! 2. 点「下一个」⇒ **横向滚动位置一点没变**（老代码会把它顶到最左边）；
//! 3. **真回车**（xdotool 发真实按键）⇒ 匹配序号照样往前走。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fileedit_find_smoke
//!
//! 需要一个本地 sshd（公钥认证），默认 127.0.0.1:2233（面板状态从它来，
//! 免得连接失败弹模态框把输入抓走）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::fileedit;

/// 退出时把临时文件删掉（别留垃圾）。
struct TempFiles(Vec<std::path::PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
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

fn wait_until(timeout_ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        pump(50);
    }
    f()
}

/// 本地测试 sshd 的连接参数。
fn test_opts() -> ConnOptions {
    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2233);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    });
    let key = std::env::var("PP_SSH_KEY")
        .unwrap_or_else(|_| format!("{}/.ssh/id_rsa", std::env::var("HOME").unwrap_or_default()));
    ConnOptions {
        proto: "ssh".into(),
        host,
        port,
        user,
        keyfile: Some(key),
        ..Default::default()
    }
}

/// 窗口 id（xdotool 的 `key --window` 要用）。
fn win_id(title: &str) -> Option<String> {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// 发一个**真的**按键（XTEST ⇒ GTK 那边照常收到 key-press）。
fn send_key(wid: &str, key: &str) {
    let _ = std::process::Command::new("xdotool")
        .args(["windowfocus", "--sync", wid])
        .status();
    let _ = std::process::Command::new("xdotool")
        .args(["key", "--clearmodifiers", key])
        .status();
}

fn main() {
    gtk::init().expect("gtk init");
    let opts = test_opts();

    let tabs = puttyplus::tabs::Tabs::new();
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_title(Some("fileedit find smoke"));
    win.set_default_size(900, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(400);

    let st = tabs.current_panel_state().expect("应有当前面板");
    assert!(
        wait_until(30_000, || st.conn_ok.get()),
        "SFTP 没连上（本地 sshd 在 {}:{} 吗？）",
        opts.host,
        opts.port
    );

    // ---- 准备一个「长行 + 已知匹配」的文件 ----
    //
    // ⚠️ 行要**够长**（横向能滚），匹配要**不在行首**——
    //    老代码（对齐到左边）会把横向位置从 0 拽到匹配那一列 ⇒ 断言抓得住。
    let dir = std::env::temp_dir().join(format!("pp-find-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("a.txt");
    let mut txt = String::new();
    for i in 0..120 {
        txt.push_str(&format!(
            "{i:04} ALLOWED_ORIGINS=[\"http://127.0.0.1:9100\",\"http://a.example.com:900/x\"] tail\n"
        ));
    }
    std::fs::write(&path, txt).expect("写文件");
    let _tmp = TempFiles(vec![path.clone()]);

    fileedit::open_local(&st, path.to_str().unwrap(), false);
    assert!(
        wait_until(15_000, || fileedit::active_editor(&st).is_some()),
        "编辑器没打开"
    );
    let e = fileedit::active_editor(&st).expect("编辑器");
    pump(300);

    // ---- 打开查找条、输词 ----
    assert!(fileedit::handle_find_key(&st), "Ctrl+F 应当交给编辑器");
    pump(200);
    assert!(fileedit::find_open(&e), "查找条应当开着");
    fileedit::set_find_query(&e, "900");
    pump(300);

    let n = fileedit::match_count(&e);
    assert!(n >= 100, "匹配数不对：{n}");
    println!("[1] 匹配数 = {n}");

    // 「下一个」按钮（**点真按钮**，别直接调内部函数）。
    let btns = fileedit::findbar_buttons(&e);
    let next = btns
        .iter()
        .find(|(name, _)| *name == "下一个")
        .map(|(_, b)| b.clone())
        .expect("查找条上应当有「下一个」");

    // ---- 2) 跳匹配 ⇒ 横向**一点不动** ----
    let h0 = fileedit::find_hscroll(&e);
    println!("[2] 起始横向 = {h0}");

    next.emit_clicked();
    pump(250);
    assert_eq!(fileedit::find_match_index(&e), 0, "第一次应当选中第 0 个匹配");
    assert_eq!(
        fileedit::selected_text(&e),
        "900",
        "选中的应当就是那一段匹配"
    );
    let h1 = fileedit::find_hscroll(&e);
    assert!(
        (h1 - h0).abs() < 0.5,
        "跳匹配时横向滚动动了：{h0} ⇒ {h1}（用户 2026-11-17：不要左右移动）"
    );
    println!("[2] 跳第一个匹配后横向 = {h1}（没动 ✔）");

    next.emit_clicked();
    pump(250);
    assert_eq!(fileedit::find_match_index(&e), 1, "第二次应当选中第 1 个匹配");
    let h2 = fileedit::find_hscroll(&e);
    assert!(
        (h2 - h0).abs() < 0.5,
        "跳匹配时横向滚动动了：{h0} ⇒ {h2}"
    );
    println!("[2] 跳第二个匹配后横向 = {h2}（没动 ✔）");

    // ---- 2b) 用户自己横向滚过去了 ⇒ 跳匹配**别把他拽回去** ----
    //
    // 老代码（把匹配对齐到左边）会把视图拽到匹配那一列 ⇒ 用户自己滚到的
    // 位置就丢了 ⇒ 这条断言抓的就是它。
    fileedit::set_hscroll(&e, 50.0);
    pump(150);
    let h_user = fileedit::find_hscroll(&e);
    next.emit_clicked();
    pump(250);
    let h_after = fileedit::find_hscroll(&e);
    assert!(
        h_after <= h_user + 1.0,
        "跳匹配把视图横向拽到 {h_after} 了（用户自己滚到的是 {h_user}）"
    );
    println!("[2b] 用户横向在 {h_user} ⇒ 跳匹配后还是 {h_after}（没被拽走 ✔）");

    // ---- 3) **真回车** ⇒ 跳下一个匹配 ----
    let has_xdo = std::process::Command::new("xdotool")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_xdo {
        println!("[3] ⚠️ 没有 xdotool ⇒ 真回车这一段没验");
    } else {
        let Some(wid) = win_id("fileedit find smoke") else {
            panic!("找不到窗口（标题「fileedit find smoke」）");
        };
        let idx0 = fileedit::find_match_index(&e);
        send_key(&wid, "Return");
        pump(400);
        assert_eq!(
            fileedit::find_match_index(&e),
            idx0 + 1,
            "查找框里按回车没有跳到下一个匹配（用户 2026-11-17 要求的）"
        );
        println!("[3] 真回车后匹配序号 = {} ✔", idx0 + 1);

        // ---- 4) Shift+回车 ⇒ 上一个 ----
        let idx1 = fileedit::find_match_index(&e);
        send_key(&wid, "shift+Return");
        pump(400);
        assert_eq!(
            fileedit::find_match_index(&e),
            idx1 - 1,
            "查找框里按 Shift+回车没有跳到上一个匹配"
        );
        println!("[4] Shift+回车后匹配序号 = {} ✔", idx1 - 1);

        // ---- 5) Esc ⇒ 关掉查找条 ----
        send_key(&wid, "Escape");
        pump(400);
        assert!(!fileedit::find_open(&e), "查找框里按 Esc 没有关掉查找条");
        println!("[5] Esc 之后查找条关掉了 ✔");
    }

    println!("fileedit find smoke ok");
}
