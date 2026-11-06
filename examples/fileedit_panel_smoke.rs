//! **文本编辑页也带上文件面板**的冒烟（阶段 98）。
//!
//! **背景**：阶段 97 把文件面板搬进了终端子页签（切页签时页签头不再跳位）。
//! 阶段 98 让**文本编辑页**也变成「文件面板｜编辑器」的结构 —— 用户编辑
//! 远程文件时，左边照样能浏览目录、点别的文件换着编辑，不用来回切页签。
//!
//! **文件面板是会话级的一个 widget**，跟着「当前页」走：切到编辑页就搬进
//! 编辑页，切回终端页就搬回终端页。**数据库工作台页不托管它**（那一页
//! 要整页宽度）。
//!
//! **断言**：
//! 1. 终端页：文件面板在终端页的 Paned 里（340px）；
//! 2. 打开一个本地文件 ⇒ 编辑器页出现 ⇒ 文件面板**搬进编辑页**（宽度仍是 340）；
//! 3. 切回终端页 ⇒ 文件面板搬回终端页（编辑页的左格空着）；
//! 4. 切回编辑页 ⇒ 又搬进去；
//! 5. 切到数据库页 ⇒ 文件面板**留在编辑页里**（数据库页不抢它的位置）；
//! 6. **关掉编辑页签**（此刻文件面板正托管在里面）⇒ 文件面板**没被销毁**
//!    （GTK4 里容器被销毁会把它子控件一起 destroy ⇒ 宿主 Paned 必须有人
//!    强引用护着，见 `PanelState::file_host`）；
//! 7. 切到终端页 ⇒ 文件面板回来了、可见、宽度还是 340。
//!
//! ```text
//! xvfb-run -a cargo run --example fileedit_panel_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::subtabs::PageKind;

/// 退出时把临时 ini 与临时文件删掉（别留垃圾）。
struct TempFiles(Vec<std::path::PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 两个 Paned 是不是同一个（GTK 控件按 GObject 指针比）。
fn same(a: &gtk::Paned, b: &gtk::Paned) -> bool {
    // ⚠️ 用 `==`（gtk::Widget 的 PartialEq 比的是 GObject 指针），
    // 别用 `std::ptr::eq` 比 upcast 出来的引用——那比的是引用本身的地址。
    a.upcast_ref::<gtk::Widget>() == b.upcast_ref::<gtk::Widget>()
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

/// 文件面板现在的宿主（它左边那个 Paned）。
fn host_of(st: &puttyplus::panel::PanelState) -> gtk::Paned {
    st.root
        .parent()
        .and_downcast::<gtk::Paned>()
        .expect("文件面板应当挂在某个 Paned 的左格里")
}

/// 截图（同一个 X display 里调 scrot，不用另开一个显示）。
fn shot(path: &str) {
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}

fn main() {
    // ⚠️ **别碰用户真实的设置文件**：这个冒烟会拖分割条、翻页签。
    let settings_ini = std::env::temp_dir()
        .join(format!("pp-fileedit-panel-smoke-set-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    // 待编辑的文件（本地文件 ⇒ 不用连 SFTP 也能开编辑器）
    let tmp_txt = std::env::temp_dir().join(format!(
        "pp_fileedit_panel_smoke_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp_txt, "hello\nPuTTYPlus 阶段 98：编辑页也带文件面板\n").expect("写临时文件");
    let _guard = TempFiles(vec![settings_ini.clone(), tmp_txt.clone()]);

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let tabs = puttyplus::tabs::Tabs::new();
    // 连一个不存在的地址：这里只关心**布局**，不关心连上没连上。
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
    win.set_title(Some("fileedit panel smoke"));
    win.set_default_size(1200, 800);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(600);

    let st = tabs.current_panel_state().expect("应有当前面板");
    let sub = st.sub_tabs().expect("应有子标签容器");

    // ---- 1) 终端页：文件面板在终端页的 Paned 里 ----
    assert_eq!(sub.current_kind(), PageKind::Terminal, "第一页应当是终端");
    assert!(st.root.is_mapped(), "终端页时文件面板应当显示在屏幕上");
    let term_host = host_of(&st);
    assert_eq!(
        term_host.position(),
        340,
        "终端页里文件面板的宽度不对：{}",
        term_host.position()
    );
    let term_id = sub.id_by_title("终端").expect("应当有终端子页");
    println!("[1] 终端页：文件面板 {w}px 在终端页的 Paned 里 ✓", w = st.root.width());

    // ---- 2) 打开一个文件 ⇒ 编辑器页 ⇒ 文件面板搬进去 ----
    puttyplus::panel::fileedit::open_local(&st, tmp_txt.to_str().unwrap(), false);
    pump(700);
    assert_eq!(
        sub.current_kind(),
        PageKind::FileEdit,
        "新开的编辑器应当是当前页（种类没标对 ⇒ 上层不知道它能托管文件面板）"
    );
    let edit_title = tmp_txt
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .expect("临时文件应当有文件名");
    let edit_id = sub
        .id_by_title(&edit_title)
        .expect("应当能找到编辑器的子页");
    let edit_host = host_of(&st);
    assert!(
        !same(&term_host, &edit_host),
        "文件面板没搬进编辑页（还待在终端页里）"
    );
    assert_eq!(
        edit_host.position(),
        340,
        "搬进编辑页后宽度应当按记忆值复位：{}",
        edit_host.position()
    );
    assert!(
        st.root.is_mapped(),
        "编辑页时文件面板应当显示在屏幕上"
    );
    println!("[2] 编辑器页：文件面板搬进来了（宽度仍是 340px）✓");
    shot("/tmp/fileedit_panel.png");

    // ---- 3) 切回终端页 ⇒ 文件面板搬回去 ----
    sub.switch_to(term_id);
    pump(400);
    let back = host_of(&st);
    assert!(same(&back, &term_host), "切回终端页后文件面板没搬回终端页");
    assert_eq!(back.position(), 340, "搬回来以后宽度变了");
    assert!(
        edit_host.start_child().is_none(),
        "编辑页的左格应当空了（文件面板搬走了）"
    );
    assert!(st.root.is_mapped(), "切回终端页后文件面板应当可见");
    println!("[3] 切回终端页：文件面板搬回去了 ✓");

    // ---- 4) 再切回编辑页 ⇒ 又搬进去 ----
    sub.switch_to(edit_id);
    pump(400);
    let again = host_of(&st);
    assert!(same(&again, &edit_host), "切回编辑页后文件面板没搬回去");
    assert_eq!(again.position(), 340, "搬进去以后宽度变了");
    println!("[4] 切回编辑页：文件面板又搬进去了 ✓");

    // ---- 5) 切到数据库页 ⇒ 文件面板留在编辑页里 ----
    puttyplus::panel::on_open_db_page(&st);
    puttyplus::panel::dbui::mount_for_test(&st);
    pump(600);
    assert_eq!(sub.current_kind(), PageKind::Db, "应当切到数据库页");
    let in_db = host_of(&st);
    assert!(
        same(&in_db, &edit_host),
        "文件面板不该被搬到数据库页（那一页要整页宽度，不托管文件面板）"
    );
    assert!(
        !st.root.is_mapped(),
        "数据库页激活时文件面板不该露出来（它留在编辑页里）"
    );
    println!("[5] 数据库页：文件面板留在编辑页里（数据库页要整页宽度）✓");

    // ---- 6) 关掉编辑页签（此刻文件面板正托管在里面）⇒ 不能被连带销毁 ----
    assert!(
        sub.close_page(edit_id),
        "关不掉编辑器的子页签"
    );
    pump(400);
    assert!(
        st.root.parent().is_some(),
        "文件面板被连带销毁了（关个编辑页签不该把文件面板弄没）"
    );
    println!("[6] 关掉编辑页签：文件面板没被连带销毁 ✓");

    // ---- 7) 切回终端页 ⇒ 文件面板回来了 ----
    sub.switch_to(term_id);
    pump(500);
    let home = host_of(&st);
    assert!(same(&home, &term_host), "文件面板没回到终端页");
    assert_eq!(home.position(), 340, "回来以后宽度变了");
    assert!(
        st.root.is_mapped(),
        "文件面板回来以后应当可见（不能把用户永久关在门外）"
    );
    assert!(st.root.width() > 100, "文件面板应当占着宽度");
    println!("[7] 切回终端页：文件面板回来了（{}px）✓", st.root.width());

    println!("fileedit_panel_smoke: OK");
}
