//! 「记住密码」的凭据库状态 + 冲突对话框的层级（阶段 79 / 审计 P1-17、P1-18、P2-12）。
//!
//! 覆盖：
//! - 凭据库**没启用**时，DB 密码提示里的「记住密码」**不可勾**，并说清原因
//!   （以前能勾，勾了还照样不存，事后才弹「密码没有保存」）；
//! - 传输冲突对话框：「覆盖全部」是**危险**动作（红色），默认响应是
//!   「跳过已存在」，正文列出冲突数量与示例。
//!
//! ⚠ HOME 与凭据库路径都要在**任何 glib 调用之前**隔离：
//! `glib::user_config_dir()` 在进程内缓存，换 HOME 不生效 ⇒ 凭据库用
//! `PUTTYPLUS_VAULT` 单独指到一个临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example remember_vault_smoke

use gtk::prelude::*;
use puttyplus::panel::dialogs;

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

fn all_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if let Some(t) = w.downcast_ref::<T>() {
            out.push(t.clone());
        }
    });
    out
}

fn toplevel(title: &str) -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
        .find(|w| w.title().map(|t| t == title).unwrap_or(false))
}

fn main() {
    let home = std::env::temp_dir().join(format!("pp-remember-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let vault = home.join("vault.bin");
    let _ = std::fs::remove_file(&vault);
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", home.join("settings.ini"));
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

    // ---- ① DB 密码提示：凭据库没启用 ⇒「记住密码」不可勾 ----
    dialogs::ask_password_remember(
        None,
        "数据库需要密码",
        "测试用的提示正文。",
        false,
        |_pw, _k| {},
    );
    pump(200);
    let dlg = toplevel("数据库需要密码").expect("密码提示框没开起来");
    let cbs = all_of::<gtk::CheckButton>(&dlg);
    // ⚠️ GtkCheckButton 的文字是**内部 label**（不是子控件 Label）⇒
    // 用 label() 取，别去 child() 里找。
    let remember = cbs
        .iter()
        .find(|c| c.label().map(|l| l.contains("记住密码")).unwrap_or(false))
        .expect("密码提示框里没有「记住密码」勾选框");
    check!(
        "凭据库未启用时「记住密码」不可勾",
        !remember.is_sensitive(),
        format!("勾选框仍然可点：sensitive={}", remember.is_sensitive())
    );
    let texts: Vec<String> = all_of::<gtk::Label>(&dlg)
        .iter()
        .map(|l| l.text().to_string())
        .collect();
    let has_reason = texts
        .iter()
        .any(|t| t.contains("凭据库") && t.contains("没启用"));
    check!(
        "不可勾时要说清原因（凭据库没启用）",
        has_reason,
        format!("提示框里的文字：{texts:?}")
    );
    let unlocks = all_of::<gtk::Button>(&dlg)
        .iter()
        .filter(|b| b.label().map(|l| l.contains("解锁")).unwrap_or(false))
        .count();
    check!(
        "没启用时不给「解锁」入口（解锁也没用）",
        unlocks == 0,
        format!("出现了 {unlocks} 个解锁按钮")
    );
    dlg.close();
    pump(100);

    // ---- ② 冲突对话框：危险层级 + 默认响应 + 正文 ----
    let conflicts = vec!["/远端/a.txt".to_string(), "/远端/b.txt".to_string()];
    dialogs::ask_conflict(None, &conflicts, |_c| {});
    pump(200);
    let cdlg = toplevel("目标已存在").expect("冲突对话框没开起来");
    let btns = all_of::<gtk::Button>(&cdlg);
    let over = btns
        .iter()
        .find(|b| b.label().map(|l| l == "覆盖全部").unwrap_or(false))
        .expect("冲突对话框里没有「覆盖全部」");
    check!(
        "「覆盖全部」是危险动作（红色）",
        over.has_css_class("destructive-action"),
        format!("css classes={:?}", over.css_classes())
    );
    let skip = btns
        .iter()
        .find(|b| b.label().map(|l| l == "跳过已存在").unwrap_or(false))
        .expect("冲突对话框里没有「跳过已存在」");
    let default_is_skip = cdlg
        .default_widget()
        .and_then(|w| w.downcast_ref::<gtk::Button>().cloned())
        .map(|b| b.label().map(|l| l == "跳过已存在").unwrap_or(false))
        .unwrap_or(false);
    check!(
        "默认响应是「跳过已存在」（回车不覆盖）",
        default_is_skip,
        format!(
            "默认控件={:?}；「跳过已存在」敏感={}",
            cdlg.default_widget().map(|w| format!("{w:?}")),
            skip.is_sensitive()
        )
    );
    let ctexts: Vec<String> = all_of::<gtk::Label>(&cdlg)
        .iter()
        .map(|l| l.text().to_string())
        .collect();
    let body = ctexts.join("\n");
    check!(
        "正文列出冲突数量",
        body.contains("2 个目标已存在"),
        format!("正文：{body}")
    );
    check!(
        "正文列出冲突示例",
        body.contains("/远端/a.txt") && body.contains("/远端/b.txt"),
        format!("正文：{body}")
    );
    cdlg.close();
    pump(100);

    if fails.is_empty() {
        println!("remember_vault_smoke: OK");
    } else {
        println!("remember_vault_smoke: {} 条断言没过", fails.len());
        std::process::exit(1);
    }
}
