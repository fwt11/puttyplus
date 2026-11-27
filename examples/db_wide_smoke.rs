//! 子标签栏**不跳位** + 文件面板**跟着终端页签走**的冒烟（阶段 97）。
//!
//! **问题**（用户 2026-11-05 截图报）：连接页原来是横向 Paned
//! （左＝常驻文件面板 / 右＝子标签区），切到数据库页时把左边的文件面板
//! 藏起来 ⇒ GTK4 的 Paned 把空间全给右边 ⇒ **子标签栏整体左移**（页签头
//! 从 x≈840 跳到 x≈330）。
//!
//! **改法**：文件面板进了**终端子页内部**（终端子页 = 横向 Paned：左＝文件
//! 面板 / 右＝终端），连接页只剩子标签区 ⇒ 切页 = 整页替换 ⇒ 子标签栏不动。
//! 文件面板仍是**会话级的一个 widget**，切到别的终端页签时会**搬**过去。
//!
//! **断言**：
//! 1. 终端页：文件面板可见、占着宽度；**记下子标签栏的位置**；
//! 2. 切到 DB 页 ⇒ **子标签栏位置一模一样**（核心：不再跳位），
//!    DB 页占满整个连接页，文件面板此刻不可见（它在终端页里），宽度没丢；
//! 3. 切回终端页 ⇒ 文件面板又回来了；
//! 4. **不搬家**：用「+」新建一个终端页签 ⇒ 切过去 ⇒ 文件面板**原地不动**
//!    （阶段 178：它在侧边栏里，跟终端页签彻底无关）；
//!    切回来 ⇒ 又跟着回来（宽度始终一致）；
//! 5. **寄存/恢复**：关掉主终端页签 ⇒ 文件面板没被销毁；
//!    点「重新连接终端」重建 ⇒ 文件面板回到新终端页旁边；
//! 6. 对象树可折叠：收起后树那一列不可见、Paned 位置归零；展开后复原。
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

/// 两个 Paned 是不是同一个（GTK 控件按 GObject 指针比）。
/// 两个「父级」是不是同一个（阶段 178：文件面板的父级是**侧边栏的槽位**，
/// 不再是 Paned ⇒ 比的是 `Option<&Widget>`）。
fn same_opt(a: &Option<gtk::Widget>, b: &Option<gtk::Widget>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => *x == *y,
        (None, None) => true,
        _ => false,
    }
}

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

fn main() {
    // ⚠️ **别碰用户真实的设置文件**：这个冒烟会拖分割条、翻页签。
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
    // ⚠️ **阶段 178**：文件面板住在**侧边栏**里 ⇒ 窗口里必须有侧边栏。
    let sb = puttyplus::sidebar::Sidebar::new(std::rc::Rc::clone(&tabs));
    sb.setup();
    let host = puttyplus::sidebarhost::SidebarHost::new(&sb);
    tabs.set_sidebar_host(&host);
    host.switch_to(puttyplus::sidebarhost::PAGE_FILES);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&host.root));
    outer.set_end_child(Some(&tabs.notebook));
    // ⚠️ 主窗口那一栏的宽度 = 侧边栏宽度（真实界面里也是这个值）。
    outer.set_position(puttyplus::settings::sidebar_width());
    win.set_child(Some(&outer));
    win.present();
    pump(600);

    let st = tabs.current_panel_state().expect("应有当前面板");
    let sub = st.sub_tabs().expect("应有子标签容器");
    let page = tabs.current_page_widget().expect("应有当前页");

    // ---- 1) 终端页：记下子标签栏的位置 ----
    assert_eq!(sub.current_kind(), PageKind::Terminal, "第一页应当是终端");
    // ⚠️ **阶段 178**：文件面板**不在连接页里**了（它在**侧边栏**里）⇒
    //   用 `is_mapped()` 验它「真的在屏幕上」时，得先切到侧边栏的文件页。
    assert!(st.root.is_mapped(), "文件面板应当显示在屏幕上（它在侧边栏里）");
    let sftp_w_term = st.root.width();
    assert!(
        sftp_w_term > 100,
        "文件面板宽度只有 {sftp_w_term}px（它应当占着位置）"
    );
    // ⚠️ **核心量的位置**：子标签栏（notebook）在连接页里的左边界。
    // 以前连接页是「文件面板｜子标签区」的 Paned ⇒ 切到数据库页文件面板
    // 一藏，子标签栏的左边界就从 340 跳到 0 ⇒ 页签头整体位移。
    // ⇒ 阶段 178 之后文件面板**根本不在连接页里** ⇒ 跳位**不可能发生**。
    let x_term = sub.nb.allocation().x();
    let w_term = sub.nb.allocation().width();
    println!("[1] 终端页：文件面板 {sftp_w_term}px（侧边栏里），子标签栏 x={x_term} 宽 {w_term}px ✓");

    // ---- 2) 打开 DB 页 ⇒ 子标签栏**不许动** ----
    puttyplus::panel::on_open_db_page(&st);
    puttyplus::panel::dbui::mount_for_test(&st);
    pump(600);
    assert_eq!(
        sub.current_kind(),
        PageKind::Db,
        "DB 页应当是当前页（种类没标对 ⇒ 上层不知道要给它腾地方）"
    );
    let x_db = sub.nb.allocation().x();
    assert_eq!(
        x_db, x_term,
        "子标签栏跳位了：切到数据库页后左边界从 {x_term} 变成了 {x_db}（文件面板被藏起来 ⇒ 整栏左移）"
    );
    // DB 页要占满整个连接页（文件面板在终端页里，DB 页不用跟它抢宽度）
    //
    // ⚠️ 工作台自己带一点内边距（22px 上下），所以按比例断言：DB 页至少要
    // 拿到连接页的九成——以前它被文件面板挤到 630/1198 ≈ 53%。
    let db_page_w = {
        let slot = st.db_ui.borrow();
        let ui = slot.as_ref().expect("数据库工作台应当挂出来了");
        ui.page.width()
    };
    let page_w = page.width();
    assert!(
        db_page_w * 100 >= page_w * 90,
        "DB 页没占满连接页：{db_page_w} vs {page_w}（文件面板还在跟它抢宽度？）"
    );
    // ⚠️ **阶段 178**：文件面板在**侧边栏**里 ⇒ 切到 DB 页**不影响**它
    //   （它压根不在连接页里）⇒ 它照样显示着。
    assert!(
        st.root.is_mapped(),
        "文件面板在侧边栏里 ⇒ 切到 DB 页也不该被藏起来"
    );
    println!("[2] DB 页：子标签栏 x 仍是 {x_db}（不跳位 ✓），DB 页 {db_page_w}px 占满连接页 ✓");

    // ---- 3) 切回终端页 ⇒ 文件面板又回来了 ----
    // 主终端页是**第一个**「终端」子页（后面新建的终端页签也叫「终端」）
    let primary_term_id = sub
        .id_by_title("终端")
        .expect("应当有终端子页");
    let db_id = sub.id_by_title("数据库").expect("应有数据库子页");
    assert_eq!(
        sub.current_id(),
        Some(db_id),
        "此刻应当停在数据库页上"
    );
    sub.switch_to(primary_term_id);
    pump(400);
    assert!(st.root.width() > 100, "文件面板应当占着宽度");
    assert_eq!(
        sub.nb.allocation().x(),
        x_term,
        "切回终端页后子标签栏的位置又变了"
    );
    println!("[3] 切回终端页：文件面板还在（{}px），子标签栏 x 仍是 {x_term} ✓", st.root.width());

    // ---- 4) 新建终端页签 ⇒ **文件面板不受影响**（它在侧边栏里）----
    //
    // ⚠️ **阶段 178 改了这件事**：以前文件面板**跟着当前终端页签搬家**
    //   （在终端子页的 Paned 左半）⇒ 切页签它就搬 ⇒ 阶段 97 的「子标签栏
    //   跳位」就是这么来的。
    //   ⇒ 现在它在**侧边栏**里 ⇒ **跟终端页签彻底无关** ⇒ 切页签时它
    //   **原地不动**（这才是"不跳位"的根治）。
    {
        let before = st.root.parent();
        // 「+」→ 新建终端 ⇒ 会切到那个新页签
        tabs.new_term_page(&page);
        pump(500);
        assert!(
            same_opt(&before, &st.root.parent()),
            "切到新终端页签后文件面板**不该搬家**（它在侧边栏里）"
        );
        assert!(st.root.is_mapped(), "切页签后文件面板应当还在屏幕上");
        // 切回第一个终端页签 ⇒ 文件面板依然原地不动
        sub.switch_to(primary_term_id);
        pump(400);
        assert!(
            same_opt(&before, &st.root.parent()),
            "切回原来的终端页签后文件面板也不该搬家"
        );
        println!("[4] 切终端页签：文件面板**原地不动**（它在侧边栏里）✓");
    }

    // ---- 5) 关掉主终端页签 ⇒ 文件面板不丢；重建 ⇒ 搬回来 ----
    {
        assert!(
            sub.close_page(primary_term_id),
            "关不掉主终端页签"
        );
        pump(400);
        // 文件面板**没被销毁**（PanelState 一直持有它）
        assert!(
            st.root.parent().is_some(),
            "文件面板被销毁了（关个终端页签不该把文件面板弄没）"
        );
        // 点「重新连接终端」⇒ 重建终端页 ⇒ 文件面板**在侧边栏里，不受影响**
        tabs.reconnect_term_page(&page);
        pump(800);
        assert!(
            st.root.is_mapped(),
            "重建后文件面板应当还在侧边栏里显示着"
        );
        assert!(
            st.root.parent().is_some(),
            "重建后文件面板不该被销毁"
        );
        println!("[5] 关掉终端页签 → 文件面板没丢；重建终端 → 还在侧边栏里 ✓");
    }

    // ---- 6) 对象树可折叠 ----
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
        println!("[6] 对象树可收起 / 可展开（几何也对）✓");
    }

    // ---- 7) 关掉 DB 页签 ⇒ 回到终端页，文件面板可见 ----
    {
        sub.switch_to(db_id);
        pump(300);
        assert!(sub.close_page(db_id), "关不掉 DB 页签");
        pump(400);
        assert!(
            st.root.is_mapped(),
            "关掉 DB 页签后文件面板没回来（用户左边就永远空着了）"
        );
        println!("[7] 关掉 DB 页签 ⇒ 文件面板回来了 ✓");
    }

    // ---- 截图模式（--hold）：摆出要拍的样子停住 ----
    if std::env::args().any(|a| a == "--hold") {
        // 重新开一个数据库页签，摆出「DB 页占满整页」的样子
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
