//! 凭据库「忘了主密码 → 重置」冒烟（阶段 30）。
//!
//! **用户场景**：主密码是解锁凭据库的**唯一**钥匙。忘了它（或者文件
//! 损坏），里面保存的密码就再也解不开——Argon2id + AES-GCM 没有后门。
//! 原先的界面只有一条路：弹一个「解锁失败」的报错框，然后**死路一条**。
//! 用户从此再也存不了密码。
//!
//! 本冒烟钉死四件事：
//! - 解锁框里**有**「忘记主密码？重置凭据库」的出路；
//! - 密码输错时**对话框不关**（当场就能改密码重试，也能就地转重置）；
//! - 点「忘记主密码？」→ 弹出**说清后果**的确认框 → 能设新主密码；
//! - 重置之后：旧主密码失效、新主密码可用、旧凭据**清空**。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example vault_reset_smoke
//!
//! ⚠️ **不碰用户真实凭据库**：全程用 `PUTTYPLUS_VAULT` 指向临时目录。

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

/// 截图（同一个 X display 里调 scrot）。
fn shot(path: &str) {
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
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

/// 按标签找一颗按钮。
fn find_button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    find_widgets::<gtk::Button>(root)
        .into_iter()
        .find(|b| b.label().as_deref() == Some(label))
}

/// 窗口里所有可见 Label 的文本（用来找错误提示）。
fn visible_labels(root: &gtk::Widget) -> Vec<String> {
    find_widgets::<gtk::Label>(root)
        .into_iter()
        .filter(|l| l.is_visible())
        .map(|l| l.text().to_string())
        .collect()
}

/// 等一个**标题匹配**的顶层窗口出现。
fn wait_dialog(title: &str) -> Option<gtk::Dialog> {
    for _ in 0..60 {
        for w in gtk::Window::list_toplevels() {
            let Some(win) = w.downcast_ref::<gtk::Window>() else {
                continue;
            };
            if win.title().as_deref() != Some(title) {
                continue;
            }
            if let Ok(d) = win.clone().downcast::<gtk::Dialog>() {
                return Some(d);
            }
        }
        pump(100);
    }
    None
}

fn main() {
    // 临时凭据库：**绝不碰用户真实的** ~/.config/puttyplus/credentials.vault
    let dir = std::env::temp_dir().join("pp_vault_smoke");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let vp = dir.join("credentials.vault");
    // SAFETY：example 是单线程的，设完立刻用。
    unsafe { std::env::set_var("PUTTYPLUS_VAULT", &vp) };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

    // 造一个带一条凭据的库（主密码 = 旧主密码）
    let creds = vec![(
        "堡垒机".to_string(),
        Zeroizing::new("s3cret".to_string()),
    )];
    vault::create("旧主密码", &creds).expect("建库");
    // 未解锁时 `get` 本来就取不到（这是设计），所以先解锁再验凭据在。
    assert!(
        vault::unlock("旧主密码").is_ok(),
        "建库后旧主密码应该能解锁"
    );
    assert!(
        vault::get("堡垒机").is_some(),
        "解锁后应该能取到凭据"
    );

    // ---- 1) 打开解锁框 ----
    let unlocked = std::rc::Rc::new(std::cell::Cell::new(false));
    let u2 = unlocked.clone();
    let dlg = vaultui::ask_master(
        None,
        vaultui::MasterOpts {
            title: "解锁凭据库".into(),
            hint: "输入主密码以解密本机保存的连接密码。".into(),
            twice: false,
            allow_empty: false,
            unlock: true,
        },
        move |pw| {
            if pw.is_some() {
                u2.set(true);
            }
        },
    );
    pump(400);

    // ---- 2) 出路必须在 ----
    let forgot = find_button(dlg.upcast_ref::<gtk::Widget>(), "忘记主密码？重置凭据库");
    assert!(
        forgot.is_some(),
        "解锁框里必须有「忘记主密码？重置凭据库」——不然忘了主密码就是死路"
    );
    println!("[1] 解锁框里有「忘记主密码？重置凭据库」");
    shot("/tmp/pp_vault_unlock.png");

    // ---- 3) 密码输错：对话框**不关**，就地提示 + 指路 ----
    let entries = find_widgets::<gtk::Entry>(dlg.upcast_ref::<gtk::Widget>());
    assert!(!entries.is_empty(), "解锁框里得有密码输入框");
    entries[0].set_text("错的主密码");
    dlg.response(gtk::ResponseType::Ok);
    pump(500);

    let texts = visible_labels(dlg.upcast_ref::<gtk::Widget>());
    let hint = texts.iter().any(|t| t.contains("忘了主密码"));
    assert!(hint, "解锁失败要就地提示并指路重置，实得：{texts:?}");
    assert!(
        dlg.is_visible(),
        "解锁失败时对话框**不能关**——用户得当场能改密码或转重置"
    );
    println!("[2] 密码输错 → 对话框没关，就地提示了重置出路");
    shot("/tmp/pp_vault_unlock_fail.png");

    // ---- 4) 点「忘记主密码？」→ 弹出说清后果的确认框 ----
    forgot.unwrap().emit_clicked();
    pump(500);
    let confirm = wait_dialog("请确认").expect("应弹出重置确认框");
    let ctexts = visible_labels(confirm.upcast_ref::<gtk::Widget>());
    let says_loss = ctexts.iter().any(|t| t.contains("永久丢失"));
    let says_keep = ctexts.iter().any(|t| t.contains("不会丢的"));
    assert!(says_loss, "确认框要说清「会永久丢失」，实得：{ctexts:?}");
    assert!(
        says_keep,
        "确认框要说清「什么不会丢」（用户敢点的前提），实得：{ctexts:?}"
    );
    println!("[3] 重置确认框说清了「永久丢失」与「不会丢的」");
    shot("/tmp/pp_vault_reset_confirm.png");

    // ---- 5) 确认 → 设新主密码 ----
    confirm.response(gtk::ResponseType::Ok);
    pump(500);
    let setup = wait_dialog("设置新主密码").expect("应弹出设置新主密码框");
    let setup_entries = find_widgets::<gtk::Entry>(setup.upcast_ref::<gtk::Widget>());
    assert!(
        setup_entries.len() >= 2,
        "设置主密码要输两遍，实得 {} 个输入框",
        setup_entries.len()
    );
    setup_entries[0].set_text("新主密码");
    setup_entries[1].set_text("新主密码");
    shot("/tmp/pp_vault_new_master.png");
    setup.response(gtk::ResponseType::Ok);
    pump(800);

    // ---- 6) 重置生效 ----
    // ⚠️ 别拿 `unlocked` 判：它跟的是**本 example 自己那个**解锁框，
    // 而重置走的是 `reset_flow` 内部的另一个框。直接验库的状态才靠谱。
    assert!(
        vault::unlock("旧主密码").is_err(),
        "旧主密码必须失效（这才叫重置）"
    );
    assert!(vault::unlock("新主密码").is_ok(), "新主密码必须能用");
    assert!(
        vault::get("堡垒机").is_none(),
        "旧凭据必须清空——用户要重新输一遍，UI 提前说清了"
    );
    println!("[4] 重置生效：旧密码失效 / 新密码可用 / 旧凭据已清空");

    let _ = std::fs::remove_dir_all(&dir);
    println!("vault reset smoke ok");
}
