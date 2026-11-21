//! 阶段 143 冒烟：**统一面板头协议 ＋ 工具栏瘦身**。
//!
//! **为什么必须数真实控件**：瘦身的价值全在"少占地方"上 ⇒
//! 光看代码里的 `toolbar.append` 数不出**界面上真有几个按钮**
//! ⇒ 这里**真的建一个文件面板**（`panel::pane::build_pane`），
//! 数它工具栏里的交互控件。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example pane_header_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::density::{self, Density};
use puttyplus::panel;

/// 工具栏上**交互控件**的上限（阶段 143 的瘦身指标）。
///
/// ⚠️ **为什么是 6**：远程面板 = 路径框 + 重连 + 主目录 + 刷新 + 队列
/// ＋ `⋯` ⇒ 6 个；计划要求「≤ 5 ＋ ⋯」⇒ 6。
const MAX_PANE_TOOLBAR_CONTROLS: usize = 6;

/// 泵事件，**等帧**（不跑帧 ⇒ 量到的还是旧值）。
fn settle(ms: u64) {
    let ml = glib::MainLoop::new(None, false);
    let q = ml.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || q.quit());
    ml.run();
}

/// 一个按钮的**图标名**（工具栏的按钮都是纯图标 ⇒ 靠图标认人）。
fn icon_of(w: &gtk::Widget) -> Option<String> {
    if let Some(img) = w.downcast_ref::<gtk::Image>() {
        return img.icon_name().map(|s| s.to_string());
    }
    // 队列按钮那种：Box 里套着 Image ＋ 角标
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(n) = icon_of(&c) {
            return Some(n);
        }
        child = c.next_sibling();
    }
    None
}

/// 工具栏里的**交互控件**数（按钮 / 输入框 / 开关）。
fn interactive_count(bar: &gtk::Box) -> usize {
    let mut n = 0;
    let mut c = bar.first_child();
    while let Some(w) = c {
        if w.is::<gtk::Button>()
            || w.is::<gtk::ToggleButton>()
            || w.is::<gtk::Entry>()
            || w.is::<gtk::MenuButton>()
        {
            n += 1;
        }
        c = w.next_sibling();
    }
    n
}

/// 工具栏里出现的图标名（**用来验"低频项已经不在工具栏上"**）。
fn icons_in(bar: &gtk::Box) -> Vec<String> {
    let mut out = Vec::new();
    let mut c = bar.first_child();
    while let Some(w) = c {
        if let Some(i) = icon_of(&w) {
            out.push(i);
        }
        c = w.next_sibling();
    }
    out
}

/// 树里有没有挂了某个 CSS 类的控件。
fn has_class_under(w: &gtk::Widget, cls: &str) -> bool {
    if w.has_css_class(cls) {
        return true;
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        if has_class_under(&child, cls) {
            return true;
        }
        c = child.next_sibling();
    }
    false
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpPaneHeaderSmoke")
        .build();
    app.connect_activate(|app| {
        check(app);
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}

fn check(app: &gtk::Application) {
    density::install();

    let win = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(900)
        .default_height(600)
        .build();

    // ⚠️ **真的建一个远程文件面板**（不是拼一个像的）⇒ 数出来的
    //    控件数就是界面上的真实控件数。
    let sp = panel::SftpPanel::new(&ConnOptions::default());
    let pane = panel::pane::build_pane(&sp.st, true);
    // ⚠️ 面板的根容器**已经在** `PanelState.root`（那个 Paned）里了
    //    ⇒ 不能再塞进窗口（GTK 会报 "already has a parent"）
    //    ⇒ 窗口装的是那个 Paned。
    let root: gtk::Widget = sp.st.root.clone().upcast();
    win.set_child(Some(&root));
    win.present();
    settle(600);

    // ---- 1. 头行挂的是**统一协议**的类 ----
    assert!(
        has_class_under(&root, puttyplus::layout::header::HEAD_CLASS),
        "面板头行没挂统一的 `{}` 类 ⇒ 各面板的头还是各写各的",
        puttyplus::layout::header::HEAD_CLASS
    );
    println!("[1] 面板头行挂了 `{}` ✔", puttyplus::layout::header::HEAD_CLASS);

    // ---- 2. 工具栏控件数 ≤ 上限 ----
    let n = interactive_count(&pane.toolbar);
    println!("[2] 远程面板工具栏的交互控件数 = {n}（上限 {MAX_PANE_TOOLBAR_CONTROLS}）");
    assert!(
        n <= MAX_PANE_TOOLBAR_CONTROLS,
        "工具栏控件还是 {n} 个 ⇒ 没瘦下来（上限 {MAX_PANE_TOOLBAR_CONTROLS}）"
    );

    // ---- 3. 低频项**不在工具栏上**（收进 ⋯ 了） ----
    let icons = icons_in(&pane.toolbar);
    println!("[3] 工具栏上的图标 = {icons:?}");
    for low in ["view-conceal-symbolic", "emblem-synchronizing-symbolic"] {
        assert!(
            !icons.iter().any(|i| i == low),
            "「{low}」还留在工具栏上 ⇒ 没收进 ⋯ 菜单"
        );
    }
    // 但 `⋯` 必须在（不然低频项就**没地方去了**）
    assert!(
        icons.iter().any(|i| i == puttyplus::layout::header::OVERFLOW_ICON),
        "工具栏上没有 `⋯` ⇒ 低频项被收了却没地方放"
    );

    // ---- 4. 两档密度下头部高度递减 ----
    let h_normal = {
        density::apply_density(Density::Normal);
        settle(500);
        natural_h(&root)
    };
    let h_compact = {
        density::apply_density(Density::Compact);
        settle(500);
        natural_h(&root)
    };
    let h_xcompact = {
        density::apply_density(Density::ExtraCompact);
        settle(500);
        natural_h(&root)
    };
    println!("[4] 面板自然高：标准 {h_normal} / 紧凑 {h_compact} / 超紧凑 {h_xcompact}");
    assert!(
        h_compact < h_normal,
        "紧凑档的面板应变矮：标准 {h_normal} vs 紧凑 {h_compact}"
    );
    assert!(
        h_xcompact < h_compact,
        "超紧凑档的面板应更矮：紧凑 {h_compact} vs 超紧凑 {h_xcompact}"
    );

    println!("pane_header_smoke: OK");
}

/// 自然高度（第二个返回值 = natural）。
fn natural_h(w: &impl IsA<gtk::Widget>) -> i32 {
    let (_, nat, _, _) = w.measure(gtk::Orientation::Vertical, -1);
    nat
}
