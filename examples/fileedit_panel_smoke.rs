//! **文件编辑器在格子里的页签 + 可拖动停靠**的冒烟（阶段 149/151）。
//!
//! **背景**：
//! - 用户 2026-11-14：「终端和文件编辑可以放在同一个窗口中，同时显示」；
//! - 用户 2026-11-15：「我是希望文本编辑器和 term 的宽度一样，就是不要
//!   占用文件列表的空间。而且我希望也可以拖动它的 tab，把它拖动到原来
//!   修改之前的位置，这样更灵活些。」
//!
//! 阶段 107/108 的「编辑区」（编辑器住在**终端列下面**）在**阶段 151 退役**
//! （用户 2026-11-22：「中间那一块是什么」⇒「它没有用就不应该存在」）⇒
//! 编辑器**恒为格子里的一个页签**（与终端并列）⇒「终端与编辑器同屏上下」
//! 改由**拆格子**实现。
//!
//! **断言**：
//! 1. 布局：文件面板**独立成格**（阶段 146）⇒ 宽度 = 格子树根分割条的位置；
//! 2. 打开文件 ⇒ 编辑器是**主栏里的一个页签**（与终端并列）；
//! 3. 拖放零件真的装上了（编辑器**页签头**有拖拽源、页签栏有接收器）；
//! 4. 挪到**侧栏** ⇒ 再挪回**主栏**（两个停靠位都通）；
//! 5. 改内容 ⇒ 标题带「●」脏标记；全关 ⇒ 编辑器没了、终端还在。
//!
//! ```text
//! xvfb-run -a cargo run --example fileedit_panel_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::subtabs::{EditorPlace, PageKind};

/// 退出时把临时 ini 与临时文件删掉（别留垃圾）。
struct TempFiles(Vec<std::path::PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
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

/// 文件面板现在的宿主（**阶段 146：它自己一格** ⇒ 父级是格子的容器）。
///
/// ⚠️ **不再是 Paned 的左格**：文件面板独立成格了（默认布局仍是"文件在左、
///   终端在右"，但那是**格子树的分割条**，不是终端页里的 Paned）。
fn host_of(st: &puttyplus::panel::PanelState) -> gtk::Widget {
    st.root.parent().expect("文件面板应当挂在某个格子里")
}

/// 终端控件（阶段 107 的核心断言：编辑器开着的时候它**仍然可见**）。
fn term_widget(st: &puttyplus::panel::PanelState) -> gtk::Widget {
    st.term
        .borrow()
        .as_ref()
        .expect("面板应当记着终端")
        .widget()
        .clone()
}

/// 这个控件上有没有装某类事件控制器（拖拽源 / 拖放接收器）。
///
/// ⚠️ GTK4 **没有公开**「某个控件挂了哪些控制器」⇒ 用
/// `gtk_widget_observe_controllers`（gtk4-rs 绑成了 `observe_controllers`）。
fn controllers(w: &impl IsA<gtk::Widget>) -> Vec<glib::Object> {
    let m = w.as_ref().observe_controllers();
    (0..m.n_items()).filter_map(|i| m.item(i)).collect()
}

/// 控件上有没有装拖拽源。
fn has_drag_source(w: &impl IsA<gtk::Widget>) -> bool {
    controllers(w)
        .iter()
        .any(|o| o.downcast_ref::<gtk::DragSource>().is_some())
}

/// 控件上有没有装拖放接收器。
fn has_drop_target(w: &impl IsA<gtk::Widget>) -> bool {
    controllers(w)
        .iter()
        .any(|o| o.downcast_ref::<gtk::DropTargetAsync>().is_some())
}

/// 这棵控件树里有没有装拖拽源（编辑器的小标签上的那个）。
fn tree_has_drag_source(w: &impl IsA<gtk::Widget>) -> bool {
    if has_drag_source(w.as_ref()) {
        return true;
    }
    let mut c = w.as_ref().first_child();
    while let Some(x) = c {
        if tree_has_drag_source(&x) {
            return true;
        }
        c = x.next_sibling();
    }
    false
}

/// 我们自己那个窗口的 X id。
fn window_wid() -> String {
    std::process::Command::new("xdotool")
        .args(["search", "--name", "fileedit panel smoke"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

/// 窗口在屏幕上的绝对位置（**没有窗口管理器**时是 (0,0)）。
///
/// ⚠️ Xvfb 下**没有 WM** ⇒ 窗口没有标题栏、没有边框 ⇒ 控件在窗口里的
/// 坐标就是屏幕坐标。
fn window_origin(title: &str) -> (i32, i32) {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", title])
        .output();
    let Ok(out) = out else { return (0, 0) };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(wid) = text.lines().next() else {
        return (0, 0);
    };
    let geo = std::process::Command::new("xdotool")
        .args(["getwindowgeometry", "--shell", wid])
        .output();
    let Ok(geo) = geo else { return (0, 0) };
    let text = String::from_utf8_lossy(&geo.stdout);
    let mut x = 0;
    let mut y = 0;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("X=") {
            x = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = line.strip_prefix("Y=") {
            y = v.trim().parse().unwrap_or(0);
        }
    }
    (x, y)
}

/// 屏幕上**除了我们这个窗口**、还有哪些「像窗口」的窗口。
///
/// ⚠️ 连接失败时面板会弹一个模态对话框（「SFTP 连接失败」）⇒ 它
/// **抓走输入** ⇒ 父窗口一个事件都收不到 ⇒ 真拖拽按不动。
///
/// ⇒ 拖之前先看一眼有没有它，有就**跳过真拖拽**（别去关它：
/// `xdotool windowclose` 会让 GDK 在销毁 EGL surface 时断言崩掉）。
fn extra_windows(keep: &str) -> Vec<String> {
    let Ok(all) = std::process::Command::new("xdotool")
        .args(["search", "--onlyvisible", "--name", ".*"])
        .output()
    else {
        return Vec::new();
    };
    // 根窗口不算（它跟屏幕一样大）
    //
    // ⚠️ `xdotool getwindowroot` 在老版本 xdotool 里**没有**这个命令
    // ⇒ 改成按**尺寸**判断：跟屏幕一样大的就是根窗口。
    let disp = std::process::Command::new("xdotool")
        .arg("getdisplaygeometry")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().map(|v| v.to_string()).collect::<Vec<_>>())
        .unwrap_or_default();
    let screen_w: i32 = disp.first().and_then(|v| v.parse().ok()).unwrap_or(0);
    let screen_h: i32 = disp.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut names: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&all.stdout).lines() {
        let wid = line.trim();
        if wid.is_empty() || wid == keep {
            continue;
        }
        // 只关「像对话框」的那些（1x1 的辅助窗口不算）
        let Ok(geo) = std::process::Command::new("xdotool")
            .args(["getwindowgeometry", "--shell", wid])
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&geo.stdout);
        let num = |k: &str| {
            text.lines()
                .find(|l| l.starts_with(k))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(0)
        };
        if num("WIDTH") < 50 || num("HEIGHT") < 50 {
            continue;
        }
        // 根窗口（跟屏幕一样大）不算
        if screen_w > 0 && num("WIDTH") >= screen_w && num("HEIGHT") >= screen_h {
            continue;
        }
        let nm = std::process::Command::new("xdotool")
            .args(["getwindowname", wid])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        names.push(format!("{wid}「{nm}」"));
    }
    names
}

/// 树里第一个按钮（编辑器小标签里那个「文件名」按钮）。
fn find_button(w: &impl IsA<gtk::Widget>) -> Option<gtk::Button> {
    let mut c = w.as_ref().first_child();
    while let Some(x) = c {
        if let Some(b) = x.downcast_ref::<gtk::Button>() {
            return Some(b.clone());
        }
        if let Some(b) = find_button(&x) {
            return Some(b);
        }
        c = x.next_sibling();
    }
    None
}

/// **真的**拖一次（xdotool 驱动 X 指针，GTK 那边照常收到拖拽事件）。
///
/// ⚠️ **必须放到另一个线程里跑**：拖拽期间主线程要继续转 GTK 主循环，
/// 否则事件没人处理 ⇒ 拖不动。
fn real_drag(from: (i32, i32), to: (i32, i32)) {
    let xdo = |args: &[&str]| {
        let _ = std::process::Command::new("xdotool").args(args).status();
    };
    xdo(&["mousemove", &from.0.to_string(), &from.1.to_string()]);
    std::thread::sleep(std::time::Duration::from_millis(200));
    xdo(&["mousedown", "1"]);
    std::thread::sleep(std::time::Duration::from_millis(200));
    // 分好几步移动：GTK 要越过**拖拽阈值**才起拖。
    for i in 1..=12 {
        let x = from.0 + (to.0 - from.0) * i / 12;
        let y = from.1 + (to.1 - from.1) * i / 12;
        xdo(&["mousemove", &x.to_string(), &y.to_string()]);
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    xdo(&["mouseup", "1"]);
}

/// 截图（同一个 X display 里调 scrot，不用另开一个显示）。
///
/// ⚠️ **先把本窗口置顶**（阶段 137 补）：连接失败会弹一个模态框（「SFTP
///   连接失败」）⇒ 它**盖住页签栏** ⇒ 截图里页签头看着像「没文件名」
///   （其实是控件树是对的，被挡了）⇒ 置顶后再拍。
fn shot(path: &str) {
    if let Ok(out) = std::process::Command::new("xdotool")
        .args(["search", "--name", "fileedit panel smoke"])
        .output()
    {
        if let Some(wid) = String::from_utf8_lossy(&out.stdout).lines().next() {
            let _ = std::process::Command::new("xdotool")
                .args(["windowraise", wid.trim()])
                .status();
        }
    }
    pump(800);
    let st = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("[截图] {path}：{:?}", st.map(|s| s.success()));
}
/// 页签头里的**文件名标签**（阶段 117 回归断言用）。
///
/// 干什么用：用户报「编辑器拖到子标签栏后，**页签头上不显示文件名了**」
/// ⇒ 冒烟必须能问出「页签头上那个标签还在不在、文字对不对」。
fn head_file_label(head: &gtk::Widget) -> Option<gtk::Label> {
    let mut c = head.first_child();
    while let Some(x) = c {
        if let Some(l) = x.downcast_ref::<gtk::Label>() {
            return Some(l.clone());
        }
        c = x.next_sibling();
    }
    None
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
    std::fs::write(&tmp_txt, "hello\nPuTTYPlus 阶段 108：编辑器与终端同宽\n").expect("写临时文件");
    let tmp_two = std::env::temp_dir().join(format!(
        "pp_fileedit_panel_smoke2_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp_two, "second\n").expect("写临时文件");
    let _guard = TempFiles(vec![settings_ini.clone(), tmp_txt.clone(), tmp_two.clone()]);

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

    // ---- 1) 布局：文件面板**独立成格**（阶段 146）＋ 终端格子 ----
    assert_eq!(sub.current_kind(), PageKind::Terminal, "第一页应当是终端");
    assert!(st.root.is_mapped(), "终端页时文件面板应当显示在屏幕上");
    let _file_host = host_of(&st);
    // ⚠️ **阶段 146**：文件面板的宽度 = **格子树根分割条**的位置。
    let tree = tabs
        .cell_tree_for(&tabs.current_page_widget().expect("应当有当前页"))
        .expect("应当有格子树");
    let root_paned = tree.root_paned().expect("两个格子 ⇒ 树根是分割条");
    assert_eq!(
        root_paned.position(),
        340,
        "文件格子的宽度不对：{}",
        root_paned.position()
    );
    // 阶段 108：终端子页的内容**就是终端列**（竖向 Paned）
    let term_col = sub
        .paned_by_page_num(0)
        .and_downcast::<gtk::Paned>()
        .expect("阶段 108：终端子页右半应当是终端列（竖向 Paned）");
    let pages_before = sub.page_count();
    println!(
        "[1] 布局：文件面板 {w}px｜终端列（竖向）✓",
        w = st.root.width()
    );

    // ---- 2) 打开一个文件 ⇒ **默认与终端并列**（阶段 137 的新默认）----
    puttyplus::panel::fileedit::open_local(&st, tmp_txt.to_str().unwrap(), false);
    pump(700);
    let eid0 = puttyplus::panel::fileedit::editor_id_for(&st, tmp_txt.to_str().unwrap());
    // ⚠️ **阶段 137**：默认是**主栏页签**（与终端并列）——
    // 用户 2026-11-19：「默认打开还是显示为和 term 并列的 tab」。
    assert_eq!(
        sub.page_count(),
        pages_before + 1,
        "编辑器应当与终端**并列为一个页签**（阶段 137 的默认）"
    );
    assert_eq!(sub.editor_count(), 1, "应当有一个编辑器");
    // ⚠️ **阶段 151**：编辑器**恒为格子里的页签**（主栏）⇒ 判据是
    //    「停在主栏」，不再是「不在底部编辑区」（那块已经退役了）。
    assert_eq!(
        sub.editor_place(eid0),
        Some(EditorPlace::Main),
        "默认应当停在主栏（与终端并列的页签）"
    );
    // 终端**还在**（只是切到了编辑器那一页 ⇒ 终端页的内容暂时不在屏幕上）。
    let term = term_widget(&st);
    assert!(
        term.parent().is_some(),
        "终端被连带销毁了（切页不该动它）"
    );
    // ⚠️ **阶段 137 的默认形态**拍一张（用户要看的「与 term 并列的 tab」）。
    shot("/tmp/fileedit_main_tab.png");

    // ---- 3) 拖放零件真的装上了（阶段 149：编辑器是页签 ⇒ 拖拽源在**页签头**上）----
    let host0 = sub
        .editor_host(eid0)
        .expect("编辑器作为页签 ⇒ 应当有自己的宿主 Paned");
    let head0 = sub
        .nb
        .tab_label(&host0.clone().upcast::<gtk::Widget>())
        .expect("编辑器应当有页签头");
    assert!(
        tree_has_drag_source(&head0),
        "编辑器页签头上没装拖拽源（编辑器拖不动 = 功能等于没有）"
    );
    assert!(
        has_drop_target(&sub.nb),
        "页签栏上没装拖放接收器（编辑器拖不进格子 = 功能等于没有）"
    );
    println!("[3] 拖放零件齐了：页签头拖拽源 + 页签栏接收器 ✓");

    // ---- 3b) 页签上要标出「这个文件在哪台机器上」（用户 2026-11-19）----
    //
    // ⚠️ **为什么重要**：页签上只有**文件名** ⇒ 「ntluser.ini」是这台
    //   服务器上的还是**本机**的，光看名字分不出来 ⇒ 必须有个小图标。
    let icons = sub.editor_tab_icon_names_for_test(eid0);
    assert!(
        icons.iter().any(|n| n == "computer-symbolic"),
        "本地文件的小标签上应当有「电脑」图标（现在有：{icons:?}）"
    );
    assert!(
        icons.iter().any(|n| n == "document-edit-symbolic"),
        "可编辑的文件还应当保留「铅笔」图标（现在有：{icons:?}）"
    );
    println!("[3b] 页签图标齐了：位置（电脑）+ 模式（铅笔）✓");
    // ---- 4) 挪到**侧栏** ⇒ 再挪回**主栏**（阶段 151：只剩这两个停靠位）----
    let eid = puttyplus::panel::fileedit::editor_id_for(&st, tmp_txt.to_str().unwrap());
    // ⚠️ **走跟拖放落点一样的路**：先解析负载，再调用同一个转换函数。
    //
    // ⚠️ 阶段 145：负载统一成 `<kind>:<id>`（旧的 `pp-editor:<id>` 仍认）。
    let payload = format!("pp-editor:{eid}");
    let (kind, pid) = puttyplus::layout::dnd::parse_page_payload(&payload)
        .expect("拖放负载应当能解析出编辑器 id");
    assert_eq!(
        kind,
        puttyplus::layout::dnd::PageKindTag::Editor,
        "负载里的种类应当是编辑器"
    );
    assert_eq!(pid, eid, "负载里的 id 跟编辑器对不上");
    // ⚠️ **先挪到侧栏**（阶段 137 的另一个停靠位）⇒ 再挪回主栏。
    assert!(sub.move_editor_to_side(pid), "编辑器没能挪到侧栏");
    pump(400);
    assert_eq!(
        sub.editor_place(pid),
        Some(EditorPlace::Side),
        "编辑器应当停在侧栏"
    );
    assert_eq!(sub.side_page_count(), 1, "侧栏应当有一个页签");
    assert!(sub.move_editor_to_main(pid), "编辑器没能挪回主栏页签");
    pump(400);
    assert_eq!(
        sub.editor_place(pid),
        Some(EditorPlace::Main),
        "编辑器应当回到主栏（与终端并列的页签）"
    );
    assert_eq!(
        sub.page_count(),
        pages_before + 1,
        "挪回主栏后应当多一个子页签"
    );

    // ⚠️ **拖出去后，那一页的左边就是 SFTP 文件列表**（阶段 121，用户
    // 2026-11-15：「**编辑器旁边要有 sftp**，就像我们修改将 term 和编辑器能
    // 同时显示之前那样」）。
    //
    // **为什么不是「别切页」**：阶段 119 试过「拖完留在终端页」⇒ SFTP 列表
    // 确实还在终端页，但**编辑器自己那一页旁边没有列表** ⇒ 用户要的没解决。
    // ⇒ 现在那一页的内容是「**文件面板 ｜ 编辑器**」的 Paned（阶段 98 的
    // 形态）⇒ 切过去就是「左边列表、右边编辑器」。
    let ewidget = sub
        .editor_widget(pid)
        .expect("编辑器应当还有正文");
    let _ = &ewidget; // 编辑器正文本身（页签头挂在宿主上，见下）
    let host = sub
        .editor_host(pid)
        .expect("编辑器应当有自己的宿主（文件面板｜编辑器）");
    let enum_page = sub
        .nb
        .page_num(&host.clone().upcast::<gtk::Widget>())
        .expect("编辑器应当是一个子页签");
    // ⚠️ **阶段 146**：编辑器这一页的宿主**左边不再摆文件面板**了 ——
    //    文件面板**独立成格**（在终端格子左边那一格）⇒ 它一直可见，
    //    「编辑器旁边有 sftp」照样成立（而且更稳：切页签时它不闪）。
    //
    // ⇒ 断言改成：**文件面板仍在格子里、且没被搬走**。
    assert!(
        st.root.parent().is_some(),
        "文件面板被连带销毁了（编辑器挪来挪去不该动它）"
    );
    let file_cell_tree = tabs
        .cell_tree_for(&tabs.current_page_widget().expect("应当有当前页"))
        .expect("应当有格子树");
    let file_cell = file_cell_tree
        .cell_of_page(st.file_page_id.get())
        .expect("文件面板那一页应当还在某个格子里");
    assert!(
        file_cell_tree.cell(file_cell).is_some(),
        "文件面板那一页应当还在树上"
    );
    assert_eq!(
        sub.nb.current_page(),
        Some(enum_page),
        "挪回主栏后应当切到编辑器那一页"
    );
    assert!(
        st.root.is_mapped(),
        "挪来挪去之后 **SFTP 文件列表应当还在屏幕上**（编辑器旁边那一列）"
    );
    // ⚠️ **页签头上必须有文件名**（阶段 117 回归：挪来挪去后文件名不见了）。
    //
    // 根因：GTK 的 `GtkButton` 私下记着 `priv->child` ⇒ 把 Label 从按钮里
    // 搬走之后，按钮销毁时还会**再摘一次** ⇒ 文件名凭空消失。
    let head = sub
        .nb
        .tab_label(&host.clone().upcast::<gtk::Widget>())
        .expect("应当有页签头");
    let fname = head_file_label(&head).expect(
        "页签头上必须有文件名标签（文件名不见了 = 回归）",
    );
    assert!(
        fname.text().contains("pp_fileedit_panel_smoke"),
        "页签头上的文件名不对：{:?}",
        fname.text()
    );
    assert!(
        fname.parent().is_some(),
        "文件名标签被从页签头上摘掉了（GTK 的 GtkButton 会把搬走的子控件再摘一次）"
    );
    assert!(
        fname.is_mapped(),
        "文件名标签没被画出来（在页签头上却看不见 = 还是丢了）"
    );
    println!(
        "[4] 挪到侧栏再挪回主栏：还是子页签（{n} 个）、**SFTP 列表在旁边的格子里**（阶段 146）、文件名「{t}」还在 ✓",
        n = pages_before + 1,
        t = fname.text()
    );
    shot("/tmp/fileedit-side-roundtrip.png");

    // ---- 5) 再开一个 ⇒ 两个编辑器；关掉 ⇒ 1 个；全关 ⇒ 0 个 ----
    puttyplus::panel::fileedit::open_local(&st, tmp_two.to_str().unwrap(), false);
    pump(700);
    assert_eq!(sub.editor_count(), 2, "应当有两个编辑器");
    // ⚠️ **阶段 137/149**：新开的文件**与终端并列为一个页签** ⇒ 子页签 +1。
    //
    // ⚠️ **阶段 149**：编辑器**恒为格子里的页签** ⇒ 两个文件 = 两个页签。
    assert_eq!(
        sub.page_count(),
        pages_before + 2,
        "两个文件应当各占一个页签（阶段 149）：现在 {n} 个，之前 {b} 个",
        n = sub.page_count(),
        b = pages_before
    );
    println!(
        "[5] 两个文件：2 个编辑器、子页签 {n} 个（终端 + 两个文件）✓",
        n = pages_before + 2
    );


    // 改内容 ⇒ 标题带「●」脏标记（阶段 107）
    assert!(
        puttyplus::panel::fileedit::type_into(&st, tmp_txt.to_str().unwrap(), "dirty\n"),
        "往编辑器里插字失败"
    );
    pump(300);
    let dirty_title = puttyplus::panel::fileedit::tab_title_for(&st, tmp_txt.to_str().unwrap());
    assert!(
        dirty_title.contains('●'),
        "脏标记没进小标签标题：{dirty_title}"
    );
    println!("[6] 脏标记进了页签标题：{dirty_title} ✓");

    // 关掉一个 ⇒ 1 个
    let e1 = puttyplus::panel::fileedit::editor_id_for(&st, tmp_txt.to_str().unwrap());
    assert!(
        puttyplus::panel::fileedit::close_editor_by_id(&st, e1),
        "关不掉编辑器"
    );
    pump(400);
    assert_eq!(sub.editor_count(), 1, "应当剩一个编辑器");

    // 全关 ⇒ 编辑器没了（界面跟没开过编辑器一样）
    let e2 = puttyplus::panel::fileedit::editor_id_for(&st, tmp_two.to_str().unwrap());
    assert!(
        puttyplus::panel::fileedit::close_editor_by_id(&st, e2),
        "关不掉第二个编辑器"
    );
    pump(400);
    assert_eq!(sub.editor_count(), 0, "应当没有编辑器了");
    assert!(term.is_mapped(), "终端还在（编辑器全关了也不该影响它）");
    println!("[7] 全部关掉：编辑器没了、终端还在 ✓");

    // ---- 8) **真的**拖一次（xdotool 驱动 X 指针）⇒ 拖到**文件面板那一格** ----
    //
    // ⚠️ 这一段验的是「拖放接线真的通」：拖拽源 → 负载 → 接收器 → 转换。
    //    前面那几步是直接调转换函数，**没经过 GTK 的拖放管线**。
    //
    // ⚠️ **为什么拖到「文件面板那一格」**：编辑器现在是**格子里的页签**
    //    （阶段 149）⇒ 拖到**同一格**的页签栏等于原地不动 ⇒ 拖到
    //    **另一格**才看得出「真搬走了」（阶段 146 起文件面板自己一格）。
    let has_xdo = std::process::Command::new("xdotool")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_xdo {
        println!("[8] ⚠️ 没有 xdotool ⇒ 真拖拽这一段没验（转换逻辑前面验过）");
    } else {
        // ⚠️ **有模态对话框就不能拖**：它抓走输入 ⇒ 父窗口收不到事件。
        //    （连接失败时面板弹的那个「SFTP 连接失败」就是它。）
        let extra = extra_windows(&window_wid());
        if !extra.is_empty() {
            println!(
                "[8] ⚠️ 有模态对话框挡着（{}）⇒ 真拖拽跳过（转换逻辑前面已验）",
                extra.join("、")
            );
        } else {
            // ⚠️ **另开一个文件**来拖（别把正在用的那个拖走 ⇒ 后面的断言会乱）。
            let tmp_drag = std::env::temp_dir().join(format!(
                "pp_fileedit_panel_drag_{}.txt",
                std::process::id()
            ));
            std::fs::write(&tmp_drag, "drag\n").expect("写临时文件");
            puttyplus::panel::fileedit::open_local(&st, tmp_drag.to_str().unwrap(), false);
            pump(700);
            let did =
                puttyplus::panel::fileedit::editor_id_for(&st, tmp_drag.to_str().unwrap());
            let tree2 = tabs
                .cell_tree_for(&tabs.current_page_widget().expect("应当有当前页"))
                .expect("应当有格子树");
            let term_cell = tree2
                .cell_of_page(did)
                .expect("编辑器应当在终端那一格");
            // 起点：编辑器**页签头**（用户就是这么拖的）。
            let host = sub
                .editor_host(did)
                .expect("编辑器在主栏 ⇒ 应当有宿主（文件面板｜编辑器）");
            let head = sub.nb.tab_label(&host).expect("编辑器的页签头");
            let p = head
                .compute_point(&win, &gtk::graphene::Point::new(6.0, 6.0))
                .expect("要能算出页签头的位置");
            // 终点：**文件面板那一格**的页签栏（窗口左上方那条）。
            let file_cell_id = tree2
                .cell_of_page(st.file_page_id.get())
                .expect("文件面板那一页应当还在某个格子里");
            let file_cell = tree2.cell(file_cell_id).expect("文件面板那一格");
            let file_nb = file_cell.tabs.nb.clone();
            let q = file_nb
                .compute_point(&win, &gtk::graphene::Point::new(40.0, 6.0))
                .expect("要能算出文件面板那格页签栏的位置");
            let (ox, oy) = window_origin("fileedit panel smoke");
            let from = (ox + p.x() as i32, oy + p.y() as i32);
            let to = (ox + q.x() as i32, oy + q.y() as i32);
            println!("[8] 真拖拽：从 ({}, {}) 拖到 ({}, {})", from.0, from.1, to.0, to.1);
            let h = std::thread::spawn(move || real_drag(from, to));
            // ⚠️ **主线程要继续转主循环**：拖拽事件得有人处理。
            while !h.is_finished() {
                pump(30);
            }
            let _ = h.join();
            pump(800);
            // ⚠️ **断言「真搬走了」**：编辑器现在在**文件面板那一格**里。
            let now_cell = tree2
                .cell_of_page(did)
                .expect("编辑器应当还在某个格子里");
            assert_eq!(
                now_cell, file_cell_id,
                "真拖拽没把编辑器搬到文件面板那一格（拖放管线没通）"
            );
            assert_ne!(now_cell, term_cell, "编辑器还在原来那一格");
            println!("[8] 真拖拽成功：编辑器搬到了文件面板那一格 ✓");
            let _ = std::fs::remove_file(&tmp_drag);
        }
    }


    println!("fileedit_panel_smoke: OK");
}
