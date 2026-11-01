//! 欢迎页「最近连接卡片」的冒烟（阶段 91）。
//!
//! **用户 2026-11-01 报**：欢迎页"不是很漂亮"，而且"右边的最近连接，首次打开
//! 软件的人可能看不出来这是最近的连接、能点进去"。
//!
//! ⇒ 改法：最近连接从"一列只写名字的按钮"改成**卡片**（图标 + 名字 +
//! `user@host:port` + 右侧箭头 + 边框），三列网格摆开；标题区加一句
//! "选一个最近连接就能开工"。
//!
//! **断言**：
//! 1. 有「最近连接」这一块（标题在）；
//! 2. **每条记录一张卡片**，卡片是**按钮**（能点，不是纯文字）；
//! 3. 卡片上写着 `user@host:port` ⇒ 一眼知道那是哪台机器；
//! 4. 卡片有 tooltip 说明"点一下就开"；
//! 5. 卡片里有图标（不是光秃秃一行字）；
//! 6. 没有最近记录时**不摆**「最近连接」块（摆个空块更没用）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example welcome_cards_smoke

use gtk::prelude::*;

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

/// 按 CSS 类找控件（项目自己的类才认，别靠"第一个按钮恰好是它"）。
fn all_with_class<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, cls: &str) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if w.css_classes().iter().any(|c| c == cls) {
            if let Some(t) = w.downcast_ref::<T>() {
                out.push(t.clone());
            }
        }
    });
    out
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

/// 卡片里的文字（标题 + 副标题）。
fn card_texts(card: &impl IsA<gtk::Widget>) -> Vec<String> {
    texts_of(card.as_ref())
}

/// 卡片里有没有图标（Image）。
fn card_has_icon(card: &impl IsA<gtk::Widget>) -> bool {
    let mut found = false;
    walk(card.as_ref(), &mut |w| {
        if w.downcast_ref::<gtk::Image>().is_some() {
            found = true;
        }
    });
    found
}

fn main() {
    // ⚠️ HOME / ini 必须在任何 glib 调用之前改（最近连接写在 HOME 底下）
    let home = std::env::temp_dir().join(format!("pp_welcome_home_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    }
    let recent = home.join("recent.ini");
    unsafe { std::env::set_var("PUTTYPLUS_RECENT_INI", &recent) };

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

    // ---------- 1. 没有最近记录 ⇒ 不摆「最近连接」块 ----------
    {
        let welcome = puttyplus::tabs::welcome_page();
        let wt = texts_of(&welcome);
        println!("空欢迎页文字：{wt:?}");
        check!(
            "没有最近记录时不摆「最近连接」块",
            !wt.iter().any(|t| t == "最近连接"),
            format!("{wt:?}")
        );
        check!(
            "空欢迎页也提示了怎么开始",
            wt.iter().any(|t| t.contains("新建连接")),
            format!("{wt:?}")
        );
    }

    // ---------- 2. 写几条最近记录 ----------
    {
        // ⚠️ 最近记录的 ini 格式：段 `[recent]`，键 `0`/`1`/…，
        // 值是 `标签\x1f主机\x1f端口\x1f用户\x1f条目名`（\x1f = 0x1f）。
        let ini = gtk::glib::KeyFile::new();
        ini.set_string("recent", "0", "测试机\u{1f}10.0.0.9\u{1f}22\u{1f}ops");
        ini.set_string("recent", "1", "srv-01\u{1f}10.0.0.9\u{1f}22\u{1f}root");
        ini.set_string("recent", "2", "本机\u{1f}127.0.0.1\u{1f}2222\u{1f}fwt");
        ini.save_to_file(&recent).unwrap();
    }

    let welcome = puttyplus::tabs::welcome_page();
    let wt = texts_of(&welcome);
    println!("欢迎页文字：{wt:?}");

    // ---------- 3. 有「最近连接」这一块 ----------
    check!(
        "有「最近连接」块",
        wt.iter().any(|t| t == "最近连接"),
        format!("{wt:?}")
    );
    check!(
        "标题区说了下一步干什么",
        wt.iter().any(|t| t.contains("选一个最近连接")),
        format!("{wt:?}")
    );

    // ---------- 4. 每条记录一张卡片 ----------
    let cards = all_with_class::<gtk::Button>(&welcome, "pp-recent-card");
    println!("卡片数：{}", cards.len());
    check!(
        "三条记录 ⇒ 三张卡片",
        cards.len() == 3,
        format!("{} 张", cards.len())
    );

    // ---------- 5. 卡片能点、看得懂 ----------
    for (i, c) in cards.iter().enumerate() {
        let tip = c.tooltip_text().unwrap_or_default().to_string();
        let ts = card_texts(c);
        check!(
            &format!("卡片 {i} 的 tooltip 说明「点一下就开」"),
            tip.contains("点一下就开"),
            format!("{tip:?}")
        );
        check!(
            &format!("卡片 {i} 写着 user@host:port"),
            ts.iter().any(|t| t.contains('@') && t.contains(':')),
            format!("{ts:?}")
        );
        check!(
            &format!("卡片 {i} 有图标（不是光秃秃一行字）"),
            card_has_icon(c),
            format!("{ts:?}")
        );
        // 卡片本身是 Button ⇒ 天然可点、可 Tab 聚焦、可回车
        check!(
            &format!("卡片 {i} 是按钮（能点）"),
            c.is_sensitive(),
            "按钮不该是灰的".to_string()
        );
    }
    check!(
        "卡片上写着那台机器的地址",
        wt.iter().any(|t| t.contains("10.0.0.9:22")),
        format!("{wt:?}")
    );

    // ---------- 6. 截图 ----------
    {
        let win = gtk::Window::new();
        win.set_title(Some("欢迎页 — 阶段 91"));
        win.set_default_size(1100, 620);
        win.set_child(Some(&welcome));
        win.present();
        pump(900);
        let path = "/tmp/welcome_cards.png";
        let _ = std::fs::remove_file(path);
        let st = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg(path)
            .status();
        println!("截图 {path}：{st:?}");
    }

    let _ = std::fs::remove_dir_all(&home);

    if fails.is_empty() {
        println!("welcome_cards smoke ok");
    } else {
        println!("welcome_cards smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        std::process::exit(1);
    }
}
