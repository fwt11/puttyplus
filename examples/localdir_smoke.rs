//! 本地目录记忆冒烟（不连接远端，不弹窗口）。
//!
//! 用隔离的 `XDG_CONFIG_HOME` 验证五件事：
//!   1. 本地面板导航到新目录后，`settings.ini` 立刻出现 `[navigation] local_dir`；
//!   2. 新建面板时自动恢复到该目录；
//!   3. 记忆目录被删除后，新面板不会恢复到不存在的路径；
//!   4. **每条连接各自记自己的目录**（阶段 154）：连接 A 记 first、连接 B 记
//!      second ⇒ 两个连接**互不干扰**；
//!   5. 设置里多出 `[conn_local_dirs]` 段（每条连接一把键）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example localdir_smoke

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
    //
    // ⚠️ **还要清掉 PUTTYPLUS_SETTINGS_INI**（2026-11-25 修 CI 红灯）：
    //   本冒烟靠 `XDG_CONFIG_HOME` 隔离（断言文件在 `$cfg/puttyplus/`），
    //   而 **`PUTTYPLUS_SETTINGS_INI` 的优先级比 `XDG_CONFIG_HOME` 高**
    //   （`settings::path()` 先看它）⇒ CI 里导出了
    //   `PUTTYPLUS_SETTINGS_INI=$HOME/settings.ini` ⇒ 设置被写到**别处**
    //   ⇒ 本冒烟读 `$cfg/puttyplus/settings.ini` 时文件**根本不存在**
    //   ⇒ `expect("settings.ini 没生成")` 直接 panic（exit 134）。
    //   ⇒ 自己把变量清掉：**隔离以本冒烟的 XDG 为准**，不靠 CI 怎么设环境。
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
        std::env::remove_var("PUTTYPLUS_LOCALDIR");
        std::env::remove_var("PUTTYPLUS_SETTINGS_INI");
    }

    gtk::init().expect("gtk init");

    let second_abs = std::fs::canonicalize(&second)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // ⚠️ Windows 上 `canonicalize` 返回的是**逐字路径**（`\\?\C:\…`）⇒
    //    面板里存/显示的是**去掉前缀**的那个（阶段 134）⇒ 断言要比这个。
    let second_pretty = puttyplus::platform::localpath::pretty(&second_abs);

    // 第一次启动：导航到 second，应立即落盘
    let p1 = SftpPanel::new(&ConnOptions::default());
    panel::on_entry_activated(&p1.st, false, &second_abs);
    assert_eq!(*p1.st.local_cwd.borrow(), second_pretty, "导航后目录没变");
    let settings = cfg.join("puttyplus").join("settings.ini");
    let text = std::fs::read_to_string(&settings).expect("settings.ini 没生成");
    assert!(
        text.contains("[navigation]") && text.contains("local_dir="),
        "settings.ini 里没有本地目录记忆:\n{text}"
    );
    // ⚠️ 设置里**不该**留着 Windows 的逐字路径前缀（界面显示的就是它）
    assert!(
        !text.contains("\\\\?\\"),
        "设置里不该有逐字路径前缀:\n{text}"
    );
    panel::shutdown(&p1.st);
    drop(p1);

    // 第二次启动：恢复 second
    let p2 = SftpPanel::new(&ConnOptions::default());
    assert_eq!(
        *p2.st.local_cwd.borrow(),
        second_pretty,
        "重启后没有恢复到上次的本地目录"
    );
    panel::shutdown(&p2.st);
    drop(p2);

    // ---------- 阶段 154：每条连接各自的本地目录 ----------
    //
    // 连接 A 记 first、连接 B 记 second ⇒ 再开时**各回各家**，不串。
    let mut oa = ConnOptions::default();
    oa.session_name = Some("smoke-server-a".into());
    let mut ob = ConnOptions::default();
    ob.session_name = Some("smoke-server-b".into());
    assert_ne!(
        oa.session_key(),
        ob.session_key(),
        "两条连接要有不同的会话键，否则测的不是「各自」"
    );

    let first_abs = std::fs::canonicalize(&first)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let first_pretty = puttyplus::platform::localpath::pretty(&first_abs);

    let pa = SftpPanel::new(&oa);
    panel::on_entry_activated(&pa.st, false, &first_abs);
    assert_eq!(
        *pa.st.local_cwd.borrow(),
        first_pretty,
        "连接 A 导航后目录没变"
    );
    panel::shutdown(&pa.st);
    drop(pa);

    let pb = SftpPanel::new(&ob);
    panel::on_entry_activated(&pb.st, false, &second_abs);
    assert_eq!(
        *pb.st.local_cwd.borrow(),
        second_pretty,
        "连接 B 导航后目录没变"
    );
    panel::shutdown(&pb.st);
    drop(pb);

    // 设置里多出「每条连接一份」的那一段
    let text = std::fs::read_to_string(&settings).expect("settings.ini 没生成");
    assert!(
        text.contains("[conn_local_dirs]"),
        "settings.ini 里没有每条连接各自的本地目录:\n{text}"
    );

    // 再开一次：连接 A 回到 first、连接 B 回到 second（**互不干扰**）
    let pa2 = SftpPanel::new(&oa);
    assert_eq!(
        *pa2.st.local_cwd.borrow(),
        first_pretty,
        "连接 A 应当回到自己记住的目录，而不是别的连接的"
    );
    panel::shutdown(&pa2.st);
    drop(pa2);

    let pb2 = SftpPanel::new(&ob);
    assert_eq!(
        *pb2.st.local_cwd.borrow(),
        second_pretty,
        "连接 B 应当回到自己记住的目录，而不是别的连接的"
    );
    panel::shutdown(&pb2.st);
    drop(pb2);

    // 记忆目录消失：不应恢复到不存在/不可读的路径
    std::fs::remove_dir_all(&second).unwrap();
    let p3 = SftpPanel::new(&ConnOptions::default());
    let restored = p3.st.local_cwd.borrow().clone();
    assert_ne!(restored, second_pretty, "恢复到已被删除的目录");
    assert!(
        std::path::Path::new(&restored).is_dir(),
        "回退目录也不可用: {restored}"
    );
    panel::shutdown(&p3.st);
    drop(p3);

    let _ = std::fs::remove_dir_all(&root);
    println!("localdir smoke ok");
}
