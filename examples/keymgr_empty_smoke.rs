//! 私钥管理器：空态独立 + 生成后能拿到私钥路径（阶段 78 / 审计 P1-16）。
//!
//! 审计 P1-16 原话：「~/.ssh 不存在」是**一行**（还被选中了），
//! 用户看着像"有一项被选中了"，其实那不是一项。
//!
//! 覆盖：
//! - 空态**不是 ListBox 的一行**（行数必须是 0）；
//! - 空态有自己的标题、说明和**主 CTA**（生成第一对密钥）；
//! - 生成后能拿到**私钥路径**，且落在 `~/.ssh` 里、权限 0600。
//!
//! ⚠ HOME 必须在**任何 glib 调用之前**换掉（glib 第一次调用就把 home
//! 目录缓存了，之后再改 HOME 不生效）—— 这里换成一个**没有 ~/.ssh**
//! 的假 HOME，正好是空态最典型的场景。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example keymgr_empty_smoke

use gtk::prelude::*;
use puttyplus::connmgr;
use puttyplus::keyui;
use puttyplus::windows::ToolKind;

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

fn main() {
    // ⚠ HOME 必须在任何 glib 调用之前改：glib 的 g_get_home_dir()
    // 第一次调用就把结果缓存了，之后再改 HOME 不生效。
    let home = std::env::temp_dir().join(format!("pp-keymgr-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let settings_ini = home.join("settings.ini");
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini);
    }

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    // ---- ① 空态：独立区域，不是列表的一行 ----
    keyui::open(None);
    pump(80);

    let p = keyui::probe().expect("私钥管理器窗口开了，探针却没了");
    assert_eq!(
        p.list_rows, 0,
        "空态被塞进 ListBox 当成一行了（还会被选中）——行数必须是 0"
    );
    assert!(p.empty_visible, "没有密钥时该显示空态");
    assert_eq!(p.empty_title, "还没有密钥", "空态要有标题");
    assert!(
        p.empty_desc.contains("~/.ssh"),
        "空态要说清楚去哪儿找：{}",
        p.empty_desc
    );
    assert_eq!(
        p.cta_label.as_deref(),
        Some("生成第一对密钥"),
        "空态要有一个主 CTA（阶段 80 的空态规范：标题+解释+一个主 CTA）"
    );
    println!(
        "[1] 空态：标题={:?} 说明={:?} CTA={:?} 列表行数={}",
        p.empty_title, p.empty_desc, p.cta_label, p.list_rows
    );

    // 空态截图（阶段 78 的正脸）
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_keymgr_empty.png")
        .status();
    println!("截图：/tmp/pp_keymgr_empty.png");

    // ---- ② 生成后能拿到私钥路径 ----
    let (priv_p, pub_p) = keyui::generate_for_test("id_smoke").expect("生成失败");
    assert!(priv_p.exists(), "私钥没写出来：{}", priv_p.display());
    assert!(pub_p.exists(), "公钥没写出来：{}", pub_p.display());
    // ⚠️ 必须落在 ~/.ssh 里：以前 ~/.ssh 不存在时会退回 home 根目录，
    // 密钥就掉到 ~ 里去了。
    assert_eq!(
        priv_p.parent().unwrap(),
        home.join(".ssh").as_path(),
        "密钥没落在 ~/.ssh 里：{}",
        priv_p.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(&priv_p).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600, "私钥权限必须是 0600，实际 {m:o}");
    }

    keyui::refresh();
    pump(50);
    let p2 = keyui::probe().expect("探针丢了");
    assert_eq!(p2.list_rows, 1, "生成后列表里应该有一行");
    assert!(!p2.empty_visible, "有密钥了就不该再显示空态");
    let want = priv_p.to_string_lossy().into_owned();
    assert!(
        p2.key_paths.iter().any(|x| *x == want),
        "列表里拿到的路径要和生成出来的一致：{:?}",
        p2.key_paths
    );
    println!("[2] 生成后：{} 行，路径 {:?}", p2.list_rows, p2.key_paths);

    // ---- ③ 「在连接中使用」：新建连接窗口里预选了这把私钥 ----
    {
        let key = priv_p.to_string_lossy().into_owned();
        let parent = puttyplus::windows::tool_window(ToolKind::Key);
        connmgr::ui_new_connection_with_key(parent.as_ref(), &key, |_| {});
        pump(120);
        let dlg = gtk::Window::list_toplevels()
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
            .find(|w| w.title().map(|t| t == "新建连接").unwrap_or(false))
            .expect("「在连接中使用」没打开新建连接窗口");
        let combos = all_of::<gtk::ComboBoxText>(&dlg);
        assert!(
            combos
                .iter()
                .any(|c| c.active_id().as_deref() == Some(key.as_str())),
            "新建连接窗口的私钥下拉没选中刚生成的那把"
        );
        assert!(
            combos
                .iter()
                .any(|c| c.active_id().as_deref() == Some("keyfile")),
            "认证方式没落在「只用指定的私钥文件」上"
        );
        println!("[3] 「在连接中使用」⇒ 新建连接窗口里私钥已预选");
    }

    // ---- ④ 截图（空态那张更说明问题，另截一张有密钥的）----
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_keymgr_keys.png")
        .status();
    println!("截图：/tmp/pp_keymgr_keys.png");

    // 窗口还开着吗（工具窗单例）
    assert!(
        puttyplus::windows::is_open(ToolKind::Key),
        "私钥管理器是工具窗：开着就该登记着"
    );

    println!("keymgr_empty_smoke: OK");
}
