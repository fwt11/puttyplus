//! 空态规范落地 + 欢迎页定位（阶段 80 / 审计 P2-1、P2-2、P2-9）的冒烟。
//!
//! 覆盖：
//! - 侧栏一条连接都没有时：**不再摆三个空组**，改显示**空态构件**
//!   （标题 + 解释 + 一个主 CTA「新建连接」），树藏起来；
//! - 欢迎页：**不再重复品牌**（大图标 + 大字「PuTTYPlus」都去掉），
//!   快捷键**收成一行**（原来是一张跟 F1 重复的六行速查表），
//!   并且有「**最近连接**」块（侧栏有、欢迎页以前没有）。
//!
//! ⚠️ 不碰用户的真实配置：ini 都指到临时文件（`glib::user_config_dir()`
//! 在进程内缓存，换 HOME 不生效 ⇒ 最近连接必须用 `PUTTYPLUS_RECENT_INI`）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example empty_state_smoke

use gtk::prelude::*;
use puttyplus::sidebar::Sidebar;
use puttyplus::tabs::Tabs;

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

fn texts_of(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out = Vec::new();
    walk(&root, &mut |w| {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
    });
    out
}

fn buttons_of(root: &impl IsA<gtk::Widget>) -> Vec<(String, bool)> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out = Vec::new();
    walk(&root, &mut |w| {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            out.push((button_text(b), b.is_sensitive()));
        }
    });
    out
}

/// 按钮的文字：普通按钮 `label()` 就有；**自绘按钮**（图标 + 文字）
/// 的文字在子 Label 里（可能隔了两层）⇒ 递归找。
fn button_text(b: &gtk::Button) -> String {
    if let Some(t) = b.label() {
        return t.to_string();
    }
    let mut found = String::new();
    walk(b.upcast_ref::<gtk::Widget>(), &mut |w| {
        if found.is_empty() {
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                found = l.text().to_string();
            }
        }
    });
    found
}

fn main() {
    let tag = std::process::id();
    // ⚠️ HOME 必须隔离：连接清单里的 **ssh config 别名**是从 `~/.ssh/config`
    // 读的（不隔离的话用户机器上的别名会冒出来 ⇒ 侧栏不为空 ⇒ 测不到空态）。
    // HOME 要在**任何 glib 调用之前**改（glib 第一次调用就把 home 缓存了）。
    let home = std::env::temp_dir().join(format!("pp-empty-home-{tag}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let conn_ini = std::env::temp_dir().join(format!("pp-empty-conn-{tag}.ini"));
    let recent = std::env::temp_dir().join(format!("pp-empty-recent-{tag}.ini"));
    let vault = std::env::temp_dir().join(format!("pp-empty-vault-{tag}.bin"));
    let db_ini = std::env::temp_dir().join(format!("pp-empty-db-{tag}.ini"));
    for p in [&conn_ini, &recent, &vault, &db_ini] {
        let _ = std::fs::remove_file(p);
    }
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
        std::env::set_var("PUTTYPLUS_RECENT_INI", &recent);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
    }

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

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

    // ---- ① 侧栏：一条连接都没有 ⇒ 空态 ----
    let tabs = Tabs::new();
    let sb = Sidebar::new(tabs.clone());
    sb.setup();
    pump(100);

    let et = texts_of(sb.empty_box());
    println!("侧栏空态文字：{et:?}");
    check!(
        "侧栏空态有标题",
        et.iter().any(|t| t == "还没有连接"),
        format!("{et:?}")
    );
    check!(
        "侧栏空态有解释",
        et.iter().any(|t| t.contains("连接") && t.len() > 6),
        format!("{et:?}")
    );
    let ebtns = buttons_of(sb.empty_box());
    println!("侧栏空态按钮：{ebtns:?}");
    check!(
        "侧栏空态有一个主 CTA「新建连接」",
        ebtns.iter().any(|(t, s)| t == "新建连接" && *s),
        format!("{ebtns:?}")
    );
    check!(
        "空态时树藏起来了（不再摆三个空组）",
        !sb.tree_sw().is_visible(),
        "树还露着"
    );

    // ---- ② 欢迎页：不再重复品牌 + 快捷键只留一行 ----
    let welcome = puttyplus::tabs::welcome_page();
    let wt = texts_of(&welcome);
    println!("欢迎页文字：{wt:?}");
    check!(
        "欢迎页不再摆大字品牌",
        !wt.iter().any(|t| t == "PuTTYPlus"),
        format!("{wt:?}")
    );
    check!(
        "欢迎页有定位说明",
        wt.iter().any(|t| t.contains("SSH") && t.contains("SFTP")),
        format!("{wt:?}")
    );
    check!(
        "欢迎页的快捷键收成了一行",
        wt.iter()
            .any(|t| t.contains("常用键位") && t.contains("F1")),
        format!("{wt:?}")
    );
    check!(
        "欢迎页没有那张两列六行的速查表",
        !wt.iter().any(|t| t == "快捷键一览"),
        format!("{wt:?}")
    );
    // 「新建连接」的主 CTA 还在
    let wbtns = buttons_of(&welcome);
    check!(
        "欢迎页有「新建连接」主 CTA",
        wbtns.iter().any(|(t, s)| t.contains("新建连接") && *s),
        format!("{wbtns:?}")
    );
    // 最近连接块：先是没有最近记录 ⇒ 不摆这块（摆一个空块更没用）
    check!(
        "没有最近记录时不摆「最近连接」块",
        !wt.iter().any(|t| t == "最近连接"),
        format!("{wt:?}")
    );

    // ---- ③ 写了条最近记录 ⇒ 欢迎页出现「最近连接」块 ----
    {
        // ⚠️ 最近记录的 ini 格式：段 `[recent]`，键 `0`/`1`/…，
        // 值是 `标签\x1f主机\x1f端口\x1f用户\x1f条目名`（\x1f = 0x1f）。
        let ini = glib::KeyFile::new();
        ini.set_string("recent", "0", "测试机\u{1f}10.0.0.9\u{1f}22\u{1f}ops");
        ini.save_to_file(&recent).unwrap();
    }
    let welcome2 = puttyplus::tabs::welcome_page();
    let wt2 = texts_of(&welcome2);
    println!("有最近记录时的欢迎页：{wt2:?}");
    check!(
        "有最近记录时欢迎页摆「最近连接」块",
        wt2.iter().any(|t| t == "最近连接"),
        format!("{wt2:?}")
    );
    check!(
        "最近连接块里列出那一条",
        wt2.iter().any(|t| t == "测试机"),
        format!("{wt2:?}")
    );

    // ---- ④ 截图（侧栏空态 + 欢迎页，摆进一个窗口里再截）----
    let win = gtk::Window::new();
    win.set_title(Some("空态与欢迎页 — 阶段 80"));
    win.set_default_size(1100, 620);
    let hb = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    hb.append(&sb.widget);
    let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
    right.append(&welcome2);
    hb.append(&right);
    win.set_child(Some(&hb));
    win.present();
    pump(250);
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_empty_state.png")
        .status();
    println!("截图：/tmp/pp_empty_state.png");

    if fails.is_empty() {
        println!("empty_state_smoke: OK");
    } else {
        println!("empty_state_smoke: {} 条断言没过", fails.len());
        std::process::exit(1);
    }
}
