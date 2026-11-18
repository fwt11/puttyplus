//! 「编辑这个数据库连接」的真回归（用户 2026-11-18 报：右键 → 编辑 ⇒ 没用）。
//!
//! **背景**：编辑对话框里有个「记住密码（存进凭据库）」的勾选框。以前它
//! 的**显示状态**被当成**要写回 ini 的值**：
//!
//! ```text
//! remember.set_active(p.remember && pol.can_remember && !bastion);
//! ```
//!
//! 凭据库**只是暂时存不了密码**时（`can_remember == false`：锁着 / 没启用）
//! ⇒ 勾选框被强制取消 ⇒ 保存时把 ini 里的 `remember=true` **抹掉**
//! ⇒ 库里那份密码变成**孤儿**（没人再会去用它）
//! ⇒ 这条连接从此每次都要重输密码 ⇒ 用户看着就是「编辑没用」。
//!
//! 而且保存后**一点回音都没有**（只有"密码没存上"才弹提示）⇒ 对话框一关
//! 就没动静了 ⇒ 同样像"点了没用"。
//!
//! 本冒烟走**真实对话框**（`ui_edit` → 填 → 点「保存」）：
//!
//! - **① 存不了密码时**：ini 里的 `remember=true` **不许**被抹掉；保存后
//!   **必须有回音**，且回音里要说清"密码这次没存住"；
//! - **② 存得了密码时**：勾选框要摆出来；勾上 + 填密码 + 保存 ⇒ 密码
//!   **真的进凭据库**、ini 的 `remember=true` 也在。
//!
//! ⚠️ **不碰用户的真实配置**：ini / 凭据库 / 连接列表全指到临时文件。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example dbedit_remember_smoke

use gtk::prelude::*;

use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};

fn pump(n: usize) {
    let ctx = gtk::glib::MainContext::default();
    for _ in 0..n {
        while ctx.iteration(false) {}
    }
}

/// 标题对上的那个对话框。
fn find_dialog(title: &str) -> Option<gtk::Dialog> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some(title))
        .find_map(|w| w.downcast::<gtk::Dialog>().ok())
}

/// 在控件树里找文字对上的按钮（对话框的动作按钮是带 label 的）。
fn find_button(root: &impl IsA<gtk::Widget>, label: &str) -> Option<gtk::Button> {
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if b.label().as_deref() == Some(label) {
                return Some(b.clone());
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    None
}

/// 对话框里**密码框**（靠 placeholder 认：gtk4 0.11 的 Entry 没有
/// `visibility()` 的读取口，只有写口）。
fn find_password_entry(root: &impl IsA<gtk::Widget>) -> Option<gtk::Entry> {
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Some(e) = w.downcast_ref::<gtk::Entry>() {
            if e.placeholder_text().as_deref() == Some("密码（留空表示不改）") {
                return Some(e.clone());
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    None
}

/// 在控件树里找文字对上的**勾选框**（`gtk::CheckButton` 在 gtk4 里**不是**
/// `gtk::Button` 的子类 ⇒ 得单独找）。
///
/// ⚠️ 只认**看得见**的：存不了密码时那个勾选框是被 `set_visible(false)`
/// 藏起来的，但它仍在控件树里 ⇒ 不看可见性就会把"藏起来了"当成"摆出来了"。
fn find_check_button(root: &impl IsA<gtk::Widget>, label: &str) -> Option<gtk::CheckButton> {
    let mut stack = vec![root.as_ref().clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Some(cb) = w.downcast_ref::<gtk::CheckButton>() {
            if cb.label().as_deref() == Some(label) && cb.is_visible() {
                return Some(cb.clone());
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    None
}

/// 对话框正文里有没有某句话（对话框里的 Label 都算）。
fn dialog_says(dlg: &gtk::Dialog, needle: &str) -> bool {
    let mut stack = vec![dlg.content_area().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            if l.text().contains(needle) {
                return true;
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
    false
}

fn profile(name: &str, remember: bool) -> DbProfile {
    DbProfile {
        name: name.to_string(),
        proto: DbProto::Postgres,
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        dbname: "postgres".into(),
        remember,
    }
}

/// 开「编辑数据库连接」→ 填密码 → 点保存 → 返回「连接已保存」那个回音。
///
/// `remember_box`：
/// - `None` ⇒ 不管那个勾选框；
/// - `Some(None)` ⇒ 断言**压根没摆**「记住密码」（存不了的时候）；
/// - `Some(Some(tick))` ⇒ 断言摆出来了，并把它设成 `tick`。
fn edit_and_save(
    key: &str,
    name: &str,
    password: Option<&str>,
    remember_box: Option<Option<bool>>,
) -> gtk::Dialog {
    puttyplus::panel::dbconn::ui_edit(None, key, name);
    pump(30);
    let dlg = find_dialog("编辑数据库连接").expect("编辑对话框要开着");
    if let Some(pw) = password {
        let pass = find_password_entry(&dlg).expect("对话框里要有密码框");
        pass.set_text(pw);
    }
    match remember_box {
        None => {}
        Some(None) => {
            assert!(
                find_check_button(&dlg, "记住密码（存进凭据库）").is_none(),
                "凭据库存不住时不该摆「记住密码」"
            );
        }
        Some(Some(tick)) => {
            let cb = find_check_button(&dlg, "记住密码（存进凭据库）")
                .expect("存得了密码时要有「记住密码」勾选框");
            cb.set_active(tick);
        }
    }
    let save_btn = find_button(&dlg, "保存").expect("对话框要有「保存」按钮");
    save_btn.emit_clicked();
    pump(30);
    find_dialog("连接已保存").expect("保存后要弹「连接已保存」")
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 临时文件：绝不碰用户真实配置 ----
    let tag = std::process::id();
    let ini = std::env::temp_dir().join(format!("pp-dbedit-db-{tag}.ini"));
    let vault = std::env::temp_dir().join(format!("pp-dbedit-vault-{tag}"));
    let conn_ini = std::env::temp_dir().join(format!("pp-dbedit-conn-{tag}.ini"));
    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&vault);
    let _ = std::fs::remove_file(&conn_ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &ini);
        std::env::set_var("PUTTYPLUS_VAULT", &vault);
        std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini);
    }

    // ⚠️ **这条会话得在连接列表里**（不然 `is_bastion_session` 判成堡垒机
    // ⇒ 「记住密码」永远不摆出来 ⇒ 测的就不是"凭据库存不住"那条路）。
    // 自建条目的会话键是 `s:<名字>`。
    {
        let kf = glib::KeyFile::new();
        kf.set_string("conn1", "name", "测试主机");
        kf.set_string("conn1", "host", "10.0.0.1");
        kf.set_uint64("conn1", "port", 22);
        kf.set_string("conn1", "user", "root");
        std::fs::write(&conn_ini, kf.to_data().as_bytes()).unwrap();
    }
    let key = "s:测试主机";
    assert!(
        !puttyplus::panel::dbconn::is_bastion_session(key),
        "前置条件：这条会话不是堡垒机会话"
    );

    /* ---------------- ① 凭据库存不了密码（没启用 / 锁着）---------------- */
    assert!(dbstore::save(key, &profile("生产库", true)));
    assert!(
        dbstore::get(key, "生产库").unwrap().remember,
        "前置条件：ini 里本来就有 remember=true"
    );

    let info = edit_and_save(key, "生产库", Some("NewSecret123"), Some(None));
    println!("[1] 编辑对话框已打开 → 填了密码 → 点了保存");

    // ini 里的 remember=true **不许**被抹掉
    let after = dbstore::get(key, "生产库").expect("连接还在");
    assert!(
        after.remember,
        "凭据库只是暂时存不了密码，ini 里的 remember=true 被抹掉了！\
         （库里那份密码变成孤儿 ⇒ 这条连接从此每次都要重输）"
    );
    println!("[2] ini 里的 remember=true 保住了");

    // 回音里要说清"密码这次没存住"
    assert!(
        dialog_says(&info, "密码") && (dialog_says(&info, "没保存") || dialog_says(&info, "没有存起来")),
        "填了密码却没存住，回音里必须说清楚"
    );
    println!("[3] 回音里说清了密码没存住");
    info.response(gtk::ResponseType::Ok);
    pump(10);

    // 不填密码时，保存也要有回音（普通编辑也不能"点了没反应"）
    let info2 = edit_and_save(key, "生产库", None, Some(None));
    assert!(dialog_says(&info2, "生产库"), "普通保存也要有回音");
    info2.response(gtk::ResponseType::Ok);
    pump(10);
    println!("[4] 不填密码的保存也有回音");

    /* ---------------- ② 凭据库能用（明文库 + 已解锁）---------------- */
    assert!(puttyplus::vault::create("", &[]).is_ok(), "建个明文凭据库");
    assert!(puttyplus::vault::unlock("").is_ok(), "解锁明文库");
    assert!(
        puttyplus::vaultui::remember_policy(puttyplus::vault::state()).can_remember,
        "明文库 + 已解锁 ⇒ 存得了密码"
    );

    assert!(dbstore::save(key, &profile("另一个库", false)));
    let info3 = edit_and_save(key, "另一个库", Some("Secret456"), Some(Some(true)));
    let after3 = dbstore::get(key, "另一个库").expect("连接还在");
    assert!(after3.remember, "勾了「记住密码」⇒ ini 里要有 remember=true");
    let stored = puttyplus::vault::get(&dbstore::vault_name(key, "另一个库"))
        .expect("密码要真的进凭据库");
    assert_eq!(
        stored.as_str(),
        "Secret456",
        "凭据库里存的密码不对：{stored:?}"
    );
    assert!(
        !dialog_says(&info3, "没有存起来"),
        "密码存住了就不该说没存住"
    );
    println!("[5] 库可用时：勾选框摆出来了，密码真的进了凭据库");
    info3.response(gtk::ResponseType::Ok);
    pump(10);

    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&vault);
    let _ = std::fs::remove_file(&conn_ini);
    println!("dbedit_remember_smoke: OK");
}
