//! **界面基线的公共测试口径**（阶段 176 / UI-00，治 P2-3 的验收基础）。
//!
//! ⚠️ **为什么要有这个模块**：审核 P2-3 说「已有许多冒烟，但覆盖的是单点
//!   行为；缺少统一的窗口几何、主题、键盘和 GTK 健康检查」。
//!   ⇒ **每个冒烟各量各的** ⇒ 一个量法变了、另一个没变 ⇒ 结论对不上。
//!   ⇒ 这里把**量法**统一成一份：例子都调它 ⇒ 量出来的数可比。
//!
//! ⚠️ **口径三条**（审核实施步骤第 5 条）：
//!   ① 控件**必须已映射**（`allocation` 有值）——没映射的控件量出来是
//!      0×0，看着像"布局坏了"；
//!   ② 窗口**必须 `present()` 过**；
//!   ③ 量之前**主循环必须泵送过**（GTK 的分配是异步的）。
//!
//! ⚠️ **截图只是人工证据**（实施步骤第 6 条）：不做像素 diff —— 字体、
//!   窗口管理器、Xvfb 尺寸都会让像素 diff 误报 ⇒ 自动化**只量几何**。

use gtk::prelude::*;

use crate::theme::Theme;

/// 三个**参考尺寸**（审核实施步骤第 2 条）。
///
/// ⚠️ **为什么是这三个**：
///   - `1200×780` = 主窗默认尺寸（最常见）；
///   - `1024×700` = 窄屏下限（审这个尺寸下有没有横向滚动/按钮截断）；
///   - `1366×768` = 老笔记本（竖向最紧）。
pub const SIZES: [(i32, i32); 3] = [(1200, 780), (1024, 700), (1366, 768)];

/// 三套主题（审核实施步骤第 3 条）。
pub const THEMES: [Theme; 3] = [Theme::Light, Theme::Dark, Theme::HighContrast];

/// 泵送主循环 `ms` 毫秒。
///
/// ⚠️ **为什么要泵**：GTK 的分配与绘制都是**异步**的 ⇒ 量几何之前必须让
///   主循环转起来，否则量到的是还没分配的尺寸。
pub fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 把窗口摆到 `w × h` 并等它**映射完**（口径 ①②③）。
///
/// ⚠️ **返回窗口的实际尺寸**：窗口管理器可能改尺寸（最小尺寸约束）⇒
///   量的时候要拿**实际**的，不是请求的。
pub fn present(win: &gtk::Window, w: i32, h: i32) -> (i32, i32) {
    win.set_default_size(w, h);
    win.present();
    // ⚠️ **多泵一会儿**：第一次 present 之后 GTK 才做第一次分配 ⇒
    //   立即量会量到 0×0。
    for _ in 0..12 {
        pump(30);
        let a = win.allocation();
        if a.width() > 0 && a.height() > 0 {
            break;
        }
    }
    let a = win.allocation();
    (a.width(), a.height())
}

/// 控件**是否已经映射**（口径 ①）。
pub fn is_mapped(w: &impl IsA<gtk::Widget>) -> bool {
    let a = w.allocation();
    a.width() > 0 && a.height() > 0
}

/// 抓 GTK 的 **critical / warning**（GTK 健康检查）。
///
/// ⚠️ **为什么要有**：GTK 遇到不合法的参数（Adjustment 的上下限反了、
///   CSS 里写了不存在的属性…）**只打 critical、不会崩** ⇒ 没有这道
///   检查，冒烟全绿而界面其实已经坏了。
///
/// ⚠️ **用法**：先 [`watch_criticals`]，跑界面，再 [`Criticals::take`]。
pub struct Criticals {
    /// ⚠️ **必须 `Send + Sync`**：GLib 的日志写手是**跨线程**回调
    ///   （`log_set_writer_func` 要求 `Send + Sync`）⇒ 这里用
    ///   `Arc<Mutex<…>>`，不能用 `Rc<RefCell<…>>`。
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Criticals {
    /// 收下到目前为止的 critical（并清空）。
    pub fn take(&self) -> Vec<String> {
        let mut v = self.seen.lock().unwrap();
        std::mem::take(&mut *v)
    }

    /// **过滤掉已知的环境噪声**后的真问题。
    ///
    /// ⚠️ **为什么要有白名单**：CI 上没有 a11y 总线、没有 GLX ⇒ GTK 自己
    ///   会打一串 critical ⇒ 不滤掉的话基线永远是红的 ⇒ 基线一红就没人
    ///   看 ⇒ 等于没有基线。
    ///
    /// ⚠️ **白名单必须逐条写明理由**（别写成「忽略所有 Gtk」）⇒ 新出现的
    ///   critical 仍然会被抓到。
    pub fn real_problems(&self) -> Vec<String> {
        self.take()
            .into_iter()
            .filter(|c| !ENV_NOISE.iter().any(|n| c.contains(n)))
            .collect()
    }
}

/// **已知的环境噪声**（测试环境问题，**不是产品缺陷**）。
///
/// | 片段 | 为什么是噪声 |
/// |---|---|
/// | `Unable to acquire the address of the accessibility bus` | CI/容器里**没有 a11y 总线** ⇒ GTK 自己打的一条警告（解法：`GTK_A11Y=none`） |
/// | `gdk_cairo_format_for_content: should not be reached` | Xvfb **没有 GLX** ⇒ 画布格式探测走不通（真实桌面上不出现） |
/// | `gtk_css_node_insert_after` | GTK 4 的 CSS 节点插入断言（审核文档 §9.2 已记为**测试侧**问题） |
pub const ENV_NOISE: [&str; 3] = [
    "Unable to acquire the address of the accessibility bus",
    "gdk_cairo_format_for_content",
    "gtk_css_node_insert_after",
];

/// 装上 GTK 日志收集器。
///
/// ⚠️ **只能装一次**：GTK 的 log writer 是**全局**的 ⇒ 第二次装会把第一
///   次的替换掉（之前那次的记录就丢了）⇒ 这里用 `Once` 守住。
pub fn watch_criticals() -> Criticals {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    ONCE.call_once(|| {
        gtk::glib::log_set_writer_func(move |level, fields| {
            // ⚠️ 只收 critical 与 warning：info/debug 是噪声。
            if matches!(
                level,
                gtk::glib::LogLevel::Critical | gtk::glib::LogLevel::Warning
            ) {
                let msg = fields
                    .iter()
                    .filter_map(|f| f.value_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
                    .join(" | ");
                sink.lock().unwrap().push(msg);
            }
            gtk::glib::LogWriterOutput::Handled
        });
    });
    Criticals { seen }
}

/// 参考环境记录（审核实施步骤第 1 条：记录 GTK、字体与显示缩放）。
///
/// ⚠️ **为什么记**：换一台机器（字体不同 ⇒ 文字宽度不同 ⇒ 几何断言的
///   阈值会变）⇒ 光有断言没有环境记录 ⇒ 复现不了。
pub fn env_report(win: &gtk::Window) -> String {
    // ⚠️ **版本号从 gtk4 crate 拿**（`gtk::major_version()` 等）——
    //   glib 里没有这几个函数。
    let gtk_ver =
        gtk::major_version() * 10000 + gtk::minor_version() * 100 + gtk::micro_version();
    // ⚠️ `pango::FontDescription::from_string` **不返回 Result**（这一版
    //   的 gtk4-rs）⇒ 直接取 `to_string()`。
    let font = gtk::pango::FontDescription::from_string("Sans 10").to_string();
    // ⚠️ **缩放从控件拿**（`Widget::scale_factor()`）：gdk4 0.11 的
    //   `Display` 上**没有** `scale_factor()`（那是更新的 API）⇒
    //   从窗口这个控件拿，一样准。
    let scale = win.scale_factor() as f32;
    let a = win.allocation();
    let screen = gtk::gdk::Display::default()
        .map(|d| {
            let n = d.monitors().n_items();
            format!("{n} 个显示器")
        })
        .unwrap_or_else(|| "（无显示）".to_string());
    format!(
        "GTK {gtk_ver}（{}.{}.{}）\n字体：{font}\n缩放：{scale}\n窗口实测：{}×{}\n屏幕：{screen}",
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        a.width(),
        a.height(),
    )
}

/// 清理测试用的临时配置（审核实施步骤第 7 条）。
///
/// ⚠️ **为什么**：example 会往 ini 里写东西 ⇒ 上一次跑失败留下的配置会
///   **污染下一次**（比如上次留了三条最近连接 ⇒ 这次测「空记录态」直接
///   失败，而且失败原因看着像代码坏了）。
///
/// ⚠️ **只删 env 里指明的那几个** ⇒ 绝不碰用户真实的配置。
pub fn cleanup_temp_configs() {
    for var in [
        "PUTTYPLUS_SETTINGS_INI",
        "PUTTYPLUS_DB_INI",
        "PUTTYPLUS_CONN_INI",
        "PUTTYPLUS_HIST_INI",
        "PUTTYPLUS_COLS_INI",
        "PUTTYPLUS_RECENT_INI",
        "PUTTYPLUS_VAULT",
    ] {
        if let Some(p) = std::env::var_os(var) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 把所有测试配置**指到临时目录**（隔离用户配置）。
///
/// ⚠️ **必须在任何 glib 调用之前调**（设置写盘的地方会读这些变量）。
pub fn isolate_configs(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("pp-uitest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("建临时目录");
    let map = [
        ("PUTTYPLUS_SETTINGS_INI", "settings.ini"),
        ("PUTTYPLUS_DB_INI", "databases.ini"),
        ("PUTTYPLUS_CONN_INI", "connections.ini"),
        ("PUTTYPLUS_HIST_INI", "history.ini"),
        ("PUTTYPLUS_COLS_INI", "columns.ini"),
        ("PUTTYPLUS_RECENT_INI", "recent.ini"),
        ("PUTTYPLUS_VAULT", "credentials.vault"),
    ];
    for (var, file) in map {
        let p = home.join(file);
        unsafe { std::env::set_var(var, &p) };
    }
    home
}
