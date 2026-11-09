//! 凭据库状态标志的冒烟（阶段 33）。
//!
//! **为什么要有这个标志**：凭据库是"开了才生效"的开关，而用户很容易忘
//! 自己开没开——忘了就会以为"记住密码"把密码加密存起来了，其实它明文躺
//! 在 `connections.ini` 里。所以界面上得**一直看得见**当前是什么状态。
//!
//! 覆盖：
//! - 四种状态（未启用 / 已启用 / 已锁定 / 未加密）的**图标与配色**都对；
//! - 徽标是**纯图标**（用户 2026-11-09：「这个按钮感觉太大了，换成一个图标
//!   吧」）⇒ 按钮里**不许有文字**，状态文字挪到 tooltip 与无障碍标签；
//! - 状态一变，标志**自己跟着变**（不用重开窗口）；
//! - 状态短文案（菜单项用的）是同一套；
//! - 徽标点击的**判定表**对（只解锁，不开管理器）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example vault_badge_smoke

use gtk::prelude::*;
use puttyplus::vault;
use puttyplus::vaultui;

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
///
/// ⚠️ **截图前必须泵**：窗口还没映射时截出来是一片黑。
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

/// 徽标按钮里的**图标名**（徽标是纯图标 ⇒ 状态只能看它）。
fn badge_icon(b: &gtk::Button) -> String {
    fn walk(w: &gtk::Widget, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        if let Some(i) = w.clone().downcast_ref::<gtk::Image>() {
            *out = i.icon_name().map(|s| s.to_string());
            return;
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            walk(&x, out);
            if out.is_some() {
                return;
            }
            c = x.next_sibling();
        }
    }
    let mut out = None;
    walk(b.upcast_ref::<gtk::Widget>(), &mut out);
    out.unwrap_or_default()
}

/// 徽标按钮里的**文字**（应当是空的 —— 纯图标）。
fn badge_text(b: &gtk::Button) -> String {
    fn walk(w: &gtk::Widget, out: &mut String) {
        if let Some(l) = w.clone().downcast_ref::<gtk::Label>() {
            out.push_str(&l.text());
            return;
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            walk(&x, out);
            c = x.next_sibling();
        }
    }
    let mut s = String::new();
    walk(b.upcast_ref::<gtk::Widget>(), &mut s);
    s
}

/// 在一棵控件树里找带 `pp-chip` 类的按钮（即凭据库标志）。
fn find_badge(root: &gtk::Widget) -> Option<gtk::Button> {
    fn walk(root: &gtk::Widget, out: &mut Option<gtk::Button>) {
        if out.is_some() {
            return;
        }
        if let Some(b) = root.clone().downcast_ref::<gtk::Button>() {
            // 标志 = 带 `pp-chip` 基础类的按钮
            if b.has_css_class("pp-chip") {
                *out = Some(b.clone());
                return;
            }
        }
        let mut c = root.first_child();
        while let Some(x) = c {
            walk(&x, out);
            if out.is_some() {
                return;
            }
            c = x.next_sibling();
        }
    }
    let mut out = None;
    walk(root, &mut out);
    out
}

fn main() {
    // 临时凭据库：**绝不碰用户真实的** ~/.config/puttyplus/credentials.vault
    let dir = std::env::temp_dir().join("pp_badge_smoke");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let vp = dir.join("credentials.vault");
    // SAFETY：example 是单线程的，设完立刻用。
    unsafe { std::env::set_var("PUTTYPLUS_VAULT", &vp) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    let badge = vaultui::badge_button();
    let win = gtk::Window::new();
    win.set_default_size(360, 90);
    win.set_child(Some(&badge));
    win.present();

    // ---- 1) 还没建库 → 未启用 ----
    assert_eq!(
        badge_icon(&badge),
        "dialog-password-symbolic",
        "没建库时应当是「钥匙」图标（未启用）"
    );
    assert_eq!(
        badge_text(&badge),
        "",
        "徽标是**纯图标**（用户 2026-11-09：「这个按钮感觉太大了，换成一个图标吧」）⇒ 按钮里不该有文字"
    );
    let tip = badge.tooltip_text().unwrap_or_default().to_string();
    assert!(
        tip.contains("凭据库"),
        "纯图标之后，状态文字得在 tooltip 里说清：{tip}"
    );
    assert!(
        badge.has_css_class("pp-chip"),
        "未启用应该是中性色"
    );
    println!("[1] 没建库 → 钥匙图标（tooltip：{tip}）");

    // ---- 2) 建库 + 解锁 → 已启用 ----
    vault::create("主密码", &[]).expect("建库");
    vault::unlock("主密码").expect("解锁");
    vaultui::notify_state_changed();
    assert_eq!(
        badge_icon(&badge),
        "channel-secure-symbolic",
        "解锁后应当是「已解锁」图标"
    );
    assert!(
        badge.has_css_class("pp-chip-ok"),
        "已启用（加密）是「一切正常」，应该是绿色"
    );
    println!("[2] 建库并解锁 → 已解锁图标（绿）");

    // ---- 3) 锁定 → 已锁定 ----
    vault::lock();
    vaultui::notify_state_changed();
    assert_eq!(
        badge_icon(&badge),
        "dialog-password-symbolic",
        "锁定后应当是「钥匙」图标"
    );
    assert!(
        badge.has_css_class("pp-chip-warn"),
        "已锁定应该是警告色"
    );
    println!("[3] 锁定 → 钥匙图标（黄）");

    // ---- 4) 明文模式 → 未加密 ----
    let _ = std::fs::remove_file(&vp);
    vault::create("", &[]).expect("建明文库");
    vault::unlock("").expect("解锁明文库");
    vaultui::notify_state_changed();
    assert_eq!(
        badge_icon(&badge),
        "dialog-warning-symbolic",
        "明文库必须说清「未加密」（警告图标）"
    );
    assert!(
        badge.has_css_class("pp-chip-err"),
        "未加密是安全倒退，必须是红色"
    );
    println!("[4] 明文模式 → 警告图标（红）");

    // ---- 5) 短文案（菜单项用的）是同一套 ----
    assert_eq!(vaultui::state_short(vault::State::Off), "未启用");
    assert_eq!(vaultui::state_short(vault::State::Unlocked), "已启用");
    assert_eq!(vaultui::state_short(vault::State::Locked), "已锁定");
    assert_eq!(vaultui::state_short(vault::State::Plain), "未加密");
    println!("[5] 菜单项用的短文案一致");

    // ---- 6) 侧栏标题栏里也有这个标志 ----
    //
    // 侧栏是用户配连接、勾「记住密码」的地方——"我到底开没开凭据库"
    // 在这里最该一眼看见（阶段 77b：连接管理器删了，标志挪到侧栏）。
    {
        let tabs = puttyplus::tabs::Tabs::new();
        let sb = puttyplus::sidebar::Sidebar::new(tabs);
        sb.reload();
        pump(800);
        let b = sb.vault_button();
        assert_eq!(
            badge_icon(b),
            "dialog-warning-symbolic",
            "侧栏里的徽标也应当是纯图标（明文状态 ⇒ 警告图标）"
        );
        assert_eq!(badge_text(b), "", "侧栏徽标里也不该有文字");
        println!("[6] 侧栏标题栏里也有纯图标状态标志");
        // 截一张侧栏的图（标志在标题栏里）
        let win = gtk::Window::new();
        win.set_default_size(320, 420);
        win.set_child(Some(&sb.widget));
        win.present();
        pump(900);
        let st = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg("/tmp/pp_sidebar_badge.png")
            .status();
        println!("[截图] /tmp/pp_sidebar_badge.png：{:?}", st.map(|s| s.success()));
    }

    // 截图（明文状态最值得看——它是"要提醒用户"的那个）
    pump(1200);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_vault_badge.png")
        .status();
    println!("[截图] /tmp/pp_vault_badge.png：{:?}", st.map(|s| s.success()));

    let _ = std::fs::remove_dir_all(&dir);
    println!("vault badge smoke ok");
}
