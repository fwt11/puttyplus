//! 本地目录记忆冒烟（不连接远端，不弹窗口）。
//!
//! 用隔离的 `XDG_CONFIG_HOME` 验证三件事：
//!   1. 本地面板导航到新目录后，`settings.ini` 立刻出现 `[navigation] local_dir`；
//!   2. 新建面板时自动恢复到该目录；
//!   3. 记忆目录被删除后，新面板不会恢复到不存在的路径。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example localdir_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel};

fn main() {
    let root = std::env::temp_dir().join(format!("pp_localdir_smoke_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cfg = root.join("config");
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();

    // 必须在任何 glib/gtk 读用户目录之前隔离配置，也不能让测试用的
    // PUTTYPLUS_LOCALDIR 抢在记忆前面。
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
        std::env::remove_var("PUTTYPLUS_LOCALDIR");
    }

    gtk::init().expect("gtk init");

    let second_abs = std::fs::canonicalize(&second)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // 第一次启动：导航到 second，应立即落盘
    let p1 = SftpPanel::new(&ConnOptions::default());
    panel::on_entry_activated(&p1.st, false, &second_abs);
    assert_eq!(*p1.st.local_cwd.borrow(), second_abs, "导航后目录没变");
    let settings = cfg.join("puttyplus").join("settings.ini");
    let text = std::fs::read_to_string(&settings).expect("settings.ini 没生成");
    assert!(
        text.contains("[navigation]") && text.contains("local_dir="),
        "settings.ini 里没有本地目录记忆:\n{text}"
    );
    panel::shutdown(&p1.st);
    drop(p1);

    // 第二次启动：恢复 second
    let p2 = SftpPanel::new(&ConnOptions::default());
    assert_eq!(
        *p2.st.local_cwd.borrow(),
        second_abs,
        "重启后没有恢复到上次的本地目录"
    );
    panel::shutdown(&p2.st);
    drop(p2);

    // 记忆目录消失：不应恢复到不存在/不可读的路径
    std::fs::remove_dir_all(&second).unwrap();
    let p3 = SftpPanel::new(&ConnOptions::default());
    let restored = p3.st.local_cwd.borrow().clone();
    assert_ne!(restored, second_abs, "恢复到已被删除的目录");
    assert!(
        std::path::Path::new(&restored).is_dir(),
        "回退目录也不可用: {restored}"
    );
    panel::shutdown(&p3.st);
    drop(p3);

    let _ = std::fs::remove_dir_all(&root);
    println!("localdir smoke ok");
}
