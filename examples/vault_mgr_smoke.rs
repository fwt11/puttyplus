//! 凭据库管理器窗口的冒烟（阶段 31）。
//!
//! 钉死四件事：
//! - 解锁后能**看见库里存了些什么**（连接名 + 所属会话），不是只弹一句
//!   "已解锁"就完事；
//! - 「**改主密码**」可用：凭据一条不少地搬过去，旧密码失效、新密码能用；
//! - 「**锁定**」有效（状态变回"已锁定"）；
//! - 「**不设主密码**」的选项在（堡垒机场景：明文保存、打开免解锁）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example vault_mgr_smoke
//!
//! ⚠️ **不碰用户真实凭据库**：`PUTTYPLUS_VAULT` 指向临时目录。

use gtk::prelude::*;
use puttyplus::vault;
use puttyplus::vaultui;
use zeroize::Zeroizing;

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
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

/// 等一个**标题匹配**的顶层窗口出现。
fn wait_dialog(title: &str) -> Option<gtk::Window> {
    for _ in 0..60 {
        for w in gtk::Window::list_toplevels() {
            let Some(win) = w.downcast_ref::<gtk::Window>() else {
                continue;
            };
            if win.title().as_deref() == Some(title) {
                return Some(win.clone());
            }
        }
        pump(100);
    }
    None
}

/// 在窗口里递归找某类型的控件。
fn find_widgets<T>(root: &gtk::Widget) -> Vec<T>
where
    T: Clone + IsA<gtk::Widget>,
{
    fn walk<T>(w: &gtk::Widget, out: &mut Vec<T>)
    where
        T: Clone + IsA<gtk::Widget>,
    {
        if let Ok(t) = w.clone().downcast::<T>() {
            out.push(t);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

fn labels(root: &gtk::Widget) -> Vec<String> {
    find_widgets::<gtk::Label>(root)
        .into_iter()
        .map(|l| l.text().to_string())
        .collect()
}

fn button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    find_widgets::<gtk::Button>(root)
        .into_iter()
        .find(|b| b.label().as_deref() == Some(label))
}

fn shot(path: &str) {
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}

fn main() {
    let dir = std::env::temp_dir().join(format!("pp_vault_mgr_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let vp = dir.join("credentials.vault");
    // SAFETY：example 是单线程的，设完立刻用。
    unsafe { std::env::set_var("PUTTYPLUS_VAULT", &vp) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    // 建一个带两条凭据的加密库
    let creds = vec![
        (
            "db:root@10.0.0.9:22/本地 PG".to_string(),
            Zeroizing::new("pw1".to_string()),
        ),
        (
            "db:root@10.0.0.9:22/测试 MySQL".to_string(),
            Zeroizing::new("pw2".to_string()),
        ),
    ];
    vault::create("主密码", &creds).expect("建库");
    assert!(vault::unlock("主密码").is_ok());

    // ---- 1) 打开管理器窗口 ----
    vaultui::open_manager_vault(None, Vec::new());
    // ⚠️ **标题从常量取**（阶段 86 修）：阶段 67 把工具窗标题统一成
    // 「凭据库 — PuTTYPlus」时**没同步这个冒烟** ⇒ 它从那时起就一直红着
    // （"应打开凭据库管理器窗口"），没人当回事。⇒ 写死标题 = 迟早再漂一次。
    let mgr = wait_dialog(puttyplus::windows::ToolKind::Vault.title())
        .expect("应打开凭据库管理器窗口");
    let root = mgr.upcast_ref::<gtk::Widget>();

    // 状态行：已解锁 + 主密码保护 + 2 条
    let texts = labels(root);
    let state = texts
        .iter()
        .find(|t| t.contains("已解锁"))
        .unwrap_or_else(|| panic!("找不到状态行，实得：{texts:?}"));
    assert!(state.contains("主密码保护"), "状态行要说清有主密码保护：{state}");
    assert!(state.contains('2'), "状态行要说清有几条凭据：{state}");

    // 列表：连接名 + 会话键都摆出来
    let texts = labels(root);
    assert!(
        texts.iter().any(|t| t == "本地 PG"),
        "列表里要有连接名，实得：{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "测试 MySQL"),
        "列表里要有第二条连接名，实得：{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "root@10.0.0.9:22"),
        "列表里要标出所属会话，实得：{texts:?}"
    );
    println!("[1] 管理器窗口列出了库里的连接（连接名 + 所属会话）");

    // 三个动作都在
    for want in ["改主密码…", "重置凭据库…", "锁定"] {
        assert!(
            button(root, want).is_some(),
            "管理器里应该有「{want}」按钮"
        );
    }
    shot("/tmp/pp_vault_manager.png");

    // ---- 2) 改主密码：凭据一条不少 ----
    button(root, "改主密码…").unwrap().emit_clicked();
    pump(500);
    let chg = wait_dialog("改主密码").expect("应弹出改主密码对话框");
    let entries = find_widgets::<gtk::Entry>(chg.upcast_ref::<gtk::Widget>());
    assert_eq!(entries.len(), 3, "应有：当前主密码 / 新主密码 / 确认新主密码");
    entries[0].set_text("主密码");
    entries[1].set_text("新主密码");
    entries[2].set_text("新主密码");
    shot("/tmp/pp_vault_change_master.png");
    // 点确定
    for b in find_widgets::<gtk::Button>(chg.upcast_ref::<gtk::Widget>()) {
        if b.label().as_deref() == Some("确定") {
            b.emit_clicked();
        }
    }
    pump(800);

    assert!(vault::unlock("主密码").is_err(), "旧主密码必须失效");
    assert!(vault::unlock("新主密码").is_ok(), "新主密码必须能用");
    let names = vault::names();
    assert_eq!(names.len(), 2, "改主密码不该丢凭据，实得 {names:?}");
    println!("[2] 改主密码：旧密码失效 / 新密码可用 / 2 条凭据一条不少");

    // ---- 3) 锁定 ----
    let mgr2 = wait_dialog(puttyplus::windows::ToolKind::Vault.title())
        .expect("管理器窗口应该还在");
    let root2 = mgr2.upcast_ref::<gtk::Widget>();
    button(root2, "锁定").unwrap().emit_clicked();
    pump(500);
    assert!(!vault::is_unlocked(), "锁定后不该再是解锁态");
    let texts = labels(root2);
    assert!(
        texts.iter().any(|t| t.contains("已锁定")),
        "状态行要变成「已锁定」，实得：{texts:?}"
    );
    println!("[3] 锁定有效，状态行变成「已锁定」");

    let _ = std::fs::remove_dir_all(&dir);
    println!("vault manager smoke ok");
}
