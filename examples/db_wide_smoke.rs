//! DB 页签**独占宽度** + 对象树可折叠的冒烟（审计 P0-2，阶段 66）。
//!
//! **问题**：数据库工作台是连接页的**子标签**，而连接页左边是常驻的
//! SFTP 文件面板（340px）。默认窗口 1200 宽：侧栏 230 + SFTP 340 ⇒
//! 子标签区只剩 630，工作台再扣掉对象树 330 ⇒ **主区只剩 300px**——
//! 结果网格和对象树全挤在一起（审计 P0-2 的现场截图就是这个）。
//!
//! **断言**：
//! 1. 终端页激活时：SFTP 面板**可见**，且占着宽度；
//! 2. DB 页激活时：SFTP 面板**不可见**（宽度为 0），子标签区**占满**
//!    连接页的整个宽度（= 窗口宽 - 侧栏宽）；
//! 3. 切回终端页 ⇒ SFTP 又**可见**了（不能把用户永久关在门外）；
//! 4. 「文件面板」开关能把它**拉回来**（用户自己说了算）；
//! 5. 对象树可折叠：收起后树那一列不可见、Paned 位置归零；展开后复原。
//!
//! ```text
//! xvfb-run -a cargo run --example db_wide_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::subtabs::PageKind;

/// 退出时把临时 ini 删掉（别留垃圾）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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

fn main() {
    // ⚠️ **别碰用户真实的设置文件**：这个冒烟会点「文件面板」开关，
    // 以前那个选择是记在 `settings.ini` 里的 ⇒ 跑一次冒烟就改了用户
    // 的设置（而且**跑挂了还留在那儿**：上一次点击已经写进去了）。
    // 现在那个选择是**会话内**的状态，但设置文件本身照旧隔离。
    let settings_ini = std::env::temp_dir()
        .join(format!("pp-db-wide-smoke-set-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };
    let _settings_guard = IniGuard(settings_ini.clone());

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let tabs = puttyplus::tabs::Tabs::new();
    // 连一个不存在的地址：这里只关心**布局**，不关心连上没连上。
    // （截图时用环境变量指向本地测试 sshd，免得弹出「连接失败」挡镜头）
    let host = std::env::var("PP_SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: i32 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9);
    let user = std::env::var("PP_SSH_USER").unwrap_or_else(|_| "nobody".into());
    let key = std::env::var("PP_SSH_KEY").ok().filter(|k| !k.is_empty());
    let opts = ConnOptions {
        proto: "ssh".into(),
        host,
        port,
        user,
        keyfile: key,
        ..Default::default()
    };
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_title(Some("db wide smoke"));
    win.set_default_size(1200, 800);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(600);

    let st = tabs.current_panel_state().expect("应有当前面板");
    let sub = st.sub_tabs().expect("应有子标签容器");
    let page = tabs.current_page_widget().expect("应有当前页");

    // ---- 1) 终端页：SFTP 可见、占着宽度 ----
    assert_eq!(sub.current_kind(), PageKind::Terminal, "第一页应当是终端");
    assert!(st.root.is_visible(), "终端页时文件面板应当可见");
    let sftp_w_term = st.root.width();
    let sub_w_term = sub.root().width();
    assert!(
        sftp_w_term > 100,
        "终端页时文件面板宽度只有 {sftp_w_term}px（它应当占着位置）"
    );
    assert!(
        sub_w_term < page.width(),
        "终端页时子标签区不该占满（文件面板还在旁边）：{} vs {}",
        sub_w_term,
        page.width()
    );
    println!("[1] 终端页：文件面板 {sftp_w_term}px，子标签区 {sub_w_term}px ✓");

    // ---- 2) 打开 DB 页 ⇒ 文件面板让位 ----
    puttyplus::panel::on_open_db_page(&st);
    puttyplus::panel::dbui::mount_for_test(&st);
    pump(600);
    assert_eq!(
        sub.current_kind(),
        PageKind::Db,
        "DB 页应当是当前页（种类没标对 ⇒ 上层不知道要给它腾地方）"
    );
    assert!(
        !st.root.is_visible(),
        "DB 页激活时文件面板应当隐藏（不然工作台主区只剩 300px）"
    );
    assert_eq!(st.root.width(), 0, "隐藏了还占着宽度");
    let sub_w_db = sub.root().width();
    assert!(
        sub_w_db > sub_w_term,
        "DB 页时子标签区没有变宽：{sub_w_db} vs {sub_w_term}"
    );
    // 子标签区应当**占满**连接页（= 侧栏以外的全部）
    assert!(
        sub_w_db >= page.width() - 2,
        "DB 页时子标签区没占满连接页：{sub_w_db} vs {}",
        page.width()
    );
    println!("[2] DB 页：文件面板 0px，子标签区 {sub_w_db}px（连接页 {}px）✓", page.width());

    // ---- 3) 切回终端页 ⇒ 文件面板又回来了 ----
    let term_id = sub.id_by_title("终端").expect("应有终端子页");
    sub.switch_to(term_id);
    pump(400);
    assert!(
        st.root.is_visible(),
        "切回终端页后文件面板必须回来（不能把用户永久关在门外）"
    );
    assert!(st.root.width() > 100, "切回终端页后文件面板应当占着宽度");
    println!("[3] 切回终端页：文件面板又可见了（{}px）✓", st.root.width());

    // ---- 4) 再进 DB 页，用「文件面板」开关把它拉回来 ----
    sub.switch_to(sub.id_by_title("数据库").expect("应有数据库子页"));
    pump(400);
    assert!(!st.root.is_visible(), "DB 页默认应当隐藏文件面板");
    // **真的点那个按钮**（走按钮的 toggled 回调 → 上层的钩子 → 切可见性）
    sub.click_toggle();
    pump(300);
    assert!(
        st.root.is_visible(),
        "点了「文件面板」开关，文件面板应当回来"
    );
    assert!(sub.toggle_state(), "开关的状态应当是按下");
    // ⚠️ **这个选择只在本次会话里有效**（2026-11-05）：以前它记在
    // `settings.ini` 里 ⇒ 同一套代码在两台机器上表现不一样（办公机器
    // 记住了"文件面板可见"，家里机器是藏起来的）——用户报的就是这个。
    // 现在它挂在面板状态上：关掉这个连接就没了。
    assert!(
        st.db_sftp_shown.get(),
        "「文件面板」被拉回来这件事应当在本会话里被记住"
    );
    // 再点一下 ⇒ 又藏起来
    sub.click_toggle();
    pump(300);
    assert!(!st.root.is_visible(), "再点一次应当又藏起来");
    assert!(!st.db_sftp_shown.get(), "再点一次应当又记成「藏起来」");
    println!("[4] 「文件面板」开关：拉回来 / 再藏起（本会话内记住）✓");

    // ---- 5) 对象树可折叠 ----
    {
        assert!(
            !puttyplus::panel::dbui::tree_collapsed(&st),
            "对象树一开始不该是收着的"
        );
        assert!(
            puttyplus::panel::dbui::tree_col_visible(&st),
            "对象树一开始应当是可见的"
        );
        puttyplus::panel::dbui::set_tree_collapsed(&st, true);
        pump(300);
        assert!(
            puttyplus::panel::dbui::tree_collapsed(&st),
            "收起对象树没有生效"
        );
        // ⚠️ **几何断言**：标志位对了不算数——那一列必须真的藏起来、
        // Paned 的位置必须真的归零（不然主区照样被挤）。
        assert!(
            !puttyplus::panel::dbui::tree_col_visible(&st),
            "收起后对象树那一列还占着地方"
        );
        assert_eq!(
            puttyplus::panel::dbui::tree_paned_pos(&st),
            0,
            "收起后 Paned 位置没有归零"
        );
        puttyplus::panel::dbui::set_tree_collapsed(&st, false);
        pump(300);
        assert!(
            !puttyplus::panel::dbui::tree_collapsed(&st),
            "展开对象树没有生效"
        );
        assert!(
            puttyplus::panel::dbui::tree_col_visible(&st),
            "展开后对象树那一列没回来"
        );
        assert_eq!(
            puttyplus::panel::dbui::tree_paned_pos(&st),
            240,
            "展开后 Paned 位置没有回到对象树宽度"
        );
        println!("[5] 对象树可收起 / 可展开（几何也对）✓");
    }

    // ---- 6) 关掉 DB 页签 ⇒ 文件面板必须回来 ----
    //
    // ⚠️ GTK 移除"当前页"时**不一定**发 `switch-page` ⇒ 要是这里没人
    // 把文件面板放回来，用户关掉 DB 页签后终端页左边就**永远空着**。
    {
        let db_id = sub.id_by_title("数据库").expect("应有数据库子页");
        sub.switch_to(db_id);
        pump(300);
        assert!(!st.root.is_visible(), "DB 页时文件面板应当是藏着的");
        assert!(sub.close_page(db_id), "关不掉 DB 页签");
        pump(400);
        assert!(
            st.root.is_visible(),
            "关掉 DB 页签后文件面板没回来（用户左边就永远空着了）"
        );
        println!("[6] 关掉 DB 页签 ⇒ 文件面板回来了 ✓");
    }

    // ---- 截图模式（--hold）：摆出要拍的样子停住 ----
    if std::env::args().any(|a| a == "--hold") {
        // 步骤 6 把 DB 页签关掉了 ⇒ 这里重新开一个
        puttyplus::panel::on_open_db_page(&st);
        puttyplus::panel::dbui::mount_for_test(&st);
        pump(700);
        // --collapsed：把对象树收起来（另一张截图）
        let collapsed = std::env::args().any(|a| a == "--collapsed");
        puttyplus::panel::dbui::set_tree_collapsed(&st, collapsed);
        pump(400);
        println!(
            "（--hold：停住等截图；对象树{}）",
            if collapsed { "已收起" } else { "展开" }
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            pump(200);
        }
    }

    println!("db_wide_smoke: OK");
}
