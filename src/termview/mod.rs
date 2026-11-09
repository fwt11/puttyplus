//! 终端后端抽象（跨平台版 阶段 C1，阶段 C4 起接上 ghostty 后端）。
//!
//! ## 为什么要有这一层
//!
//! VTE（libvte 2.91）**只有 Linux 有**。要把 PuTTYPlus 搬到 Windows，
//! 终端这一层必须能换成别的引擎（跨平台版计划里是 libghostty-vt）。
//!
//! ## 为什么用**枚举**而不是 trait object
//!
//! `vte::Terminal` 是**具体类型**。枚举能单态化（零动态派发），
//! 而且**编译期就会把每个后端都检查一遍**——加了新后端却忘了实现某个
//! 方法 ⇒ 编译不过，而不是运行到一半才炸。
//!
//! ## 约定
//!
//! - 应用层（`term.rs` / `tabs.rs` / `panel/` / `main.rs`）**不许**出现
//!   `vte::` 字样；VTE 特有的东西（HTML 导出、正则搜索、信号名）全关在
//!   后端里。
//! - 跨后端语义**统一**的接口放在这一层：光标行号、带样式的行文本、
//!   查找、复制粘贴、标题/目录回调、子进程退出回调。
//! - 录制（asciicast）要的是**带样式的行文本**，两个后端的取法不同
//!   （VTE 给 HTML，ghostty 给 VT 序列）⇒ 这一层统一**产出 ANSI 行**，
//!   上层拿到的永远是 ANSI（见 [`TermView::ansi_rows`]）。
//!
//! ## 两个后端各在哪（阶段 C4）
//!
//! | 平台 | 后端 | 依赖 |
//! |---|---|---|
//! | Linux | **VTE**（主线，行为零变化） | 系统装 `libvte-2.91-dev` |
//! | Windows | **ghostty**（自绘） | 编译期要 **Zig 0.17** |
//!
//! Linux 上也可以用 `ghostty` 特性**试跑 ghostty 后端**（自测用，
//! 见 `examples/ghostty_view_smoke.rs`）。

#[cfg(target_os = "linux")]
pub mod vte;

/// ghostty 终端内核（跨平台版 阶段 C2 起）。
///
/// ⚠️ Linux 上需要 `ghostty` 特性（会编译 libghostty-vt ⇒ 要 Zig 0.17）；
/// Windows 上**常驻**（Windows 没有 VTE，终端后端只能是它）。
#[cfg(any(feature = "ghostty", target_os = "windows"))]
pub mod ghostty;

/// ghostty 自绘终端控件（跨平台版 阶段 C3 起）。
#[cfg(any(feature = "ghostty", target_os = "windows"))]
pub mod ghostty_view;

/// 本程序目前只做了 Linux 与 Windows。
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!("目前只支持 Linux 与 Windows（macOS 待在后续版本做）");

#[cfg(target_os = "linux")]
use gtk::prelude::*;
#[cfg(target_os = "linux")]
use vte4::prelude::*;
#[cfg(any(feature = "ghostty", target_os = "windows"))]
use gtk::prelude::*;
#[cfg(any(feature = "ghostty", target_os = "windows"))]
use gtk::glib::prelude::*;

#[cfg(any(feature = "ghostty", target_os = "windows"))]
use ghostty_view::GhosttyView;
#[cfg(any(feature = "ghostty", target_os = "windows"))]
use ghostty::GhosttyTerm;

/// 终端视图：后端无关的句柄。
///
/// ⚠️ 克隆是**浅拷贝**（两个句柄指着同一个终端控件）——与改造前
/// `vte::Terminal` 的克隆语义一致。
#[derive(Clone)]
pub enum TermView {
    /// Linux：VTE 终端控件。
    #[cfg(target_os = "linux")]
    Vte(vte4::Terminal),
    /// Windows（以及 Linux 上的自测）：ghostty 自绘终端。
    #[cfg(any(feature = "ghostty", target_os = "windows"))]
    Ghostty(GhosttyView),
}

/// 终端字体（**字体名 + 字号**）⇒ Pango 字体描述。
///
/// ## 为什么放在这一层
///
/// 两个后端（VTE / ghostty）都要用同一套字体规则，**各写一份就会漂**
/// （以前就漂过：两边的 `format!("Monospace {size}")` 是抄的）。
///
/// ## 优先级
///
/// - **字号**：会话/连接里设的 > 首选项中心的全局字号 > 内置 10.5；
/// - **字体名**：目前只有全局一档（首选项中心 →「终端」页），默认
///   `Monospace`（系统等宽字体）。
///
/// ⚠️ `PUTTYPLUS_FONT` 环境变量仍然可以**整个覆盖**（调试用）。
pub fn font_description(font_size: Option<f64>) -> gtk::pango::FontDescription {
    let font_env = std::env::var("PUTTYPLUS_FONT").unwrap_or_default();
    if !font_env.is_empty() {
        return gtk::pango::FontDescription::from_string(&font_env);
    }
    let size = font_size
        .or_else(crate::settings::term_font_size)
        .unwrap_or(10.5);
    let family = crate::settings::term_font_family()
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "Monospace".to_string());
    // ⚠️ 字体名里**可能有空格**（"Noto Sans Mono CJK SC"）⇒ 拼的时候
    // 字体名在前、字号在后（pango 按「最后一个是数字的词」认字号）。
    gtk::pango::FontDescription::from_string(&format!("{family} {size}"))
}

impl TermView {
    /// 新建一个终端控件（还没接 PTY、没设外观）。
    pub fn new() -> Self {
        // Linux 走 VTE（主线零回归）；其它平台走 ghostty 自绘。
        #[cfg(target_os = "linux")]
        {
            TermView::Vte(vte4::Terminal::new())
        }
        #[cfg(not(target_os = "linux"))]
        {
            TermView::Ghostty(GhosttyView::new())
        }
    }

    /// **新建一个 ghostty 后端的终端**（Windows 默认走这个）。
    ///
    /// ⚠️ Linux 上只是给自测用（主线仍然是 VTE）。
    #[cfg(any(feature = "ghostty", target_os = "windows"))]
    pub fn new_ghostty() -> Self {
        TermView::Ghostty(GhosttyView::new())
    }

    /// 用**现成的 VTE 终端控件**包一层。
    ///
    /// ⚠️ **Linux 专用**：给那些直接操作 VTE 控件的工具用
    /// （录制比对的 example 等）。跨平台的后端应当用 [`TermView::new`]。
    #[cfg(target_os = "linux")]
    pub fn from_vte(t: vte4::Terminal) -> Self {
        TermView::Vte(t)
    }

    /* ---------------- GTK 侧 ---------------- */

    /// 终端控件本体（塞进容器 / 挂控制器都用它）。
    pub fn widget(&self) -> &gtk::Widget {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.upcast_ref::<gtk::Widget>(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.widget(),
        }
    }

    /// 这个控件**是不是一个终端**（快速命令弹层据此决定弹层指向哪）。
    pub fn is_terminal_widget(w: &gtk::Widget) -> bool {
        #[cfg(target_os = "linux")]
        {
            if w.is::<vte4::Terminal>() {
                return true;
            }
        }
        #[cfg(any(feature = "ghostty", target_os = "windows"))]
        {
            if GhosttyView::is_terminal_widget(w) {
                return true;
            }
        }
        false
    }

    /// 从控件反查它所属的终端。
    ///
    /// ⚠️ **别在闭包里长期持有返回值**（控件 → 控制器 → 闭包 → 控件
    /// 的强引用环，关掉标签也回收不掉）。用法是「当场用一下就放」：
    /// ```ignore
    /// gesture.connect_pressed(|g, _, _, _| {
    ///     if let Some(t) = TermView::from_widget(g.widget().unwrap()) {
    ///         t.paste_clipboard();
    ///     }
    /// });
    /// ```
    pub fn from_widget(w: &gtk::Widget) -> Option<TermView> {
        #[cfg(target_os = "linux")]
        {
            if let Some(t) = w.downcast_ref::<vte4::Terminal>() {
                return Some(TermView::Vte(t.clone()));
            }
        }
        #[cfg(any(feature = "ghostty", target_os = "windows"))]
        {
            if let Some(v) = GhosttyView::from_widget(w) {
                return Some(TermView::Ghostty(v));
            }
        }
        None
    }

    /// 挂一个事件控制器（按键、手势、焦点……）。
    pub fn add_controller(&self, c: impl IsA<gtk::EventController>) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.add_controller(c),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.area().add_controller(c),
        }
    }

    /// 把焦点抢到终端上。
    pub fn grab_focus(&self) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                t.grab_focus();
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                v.area().grab_focus();
            }
        }
    }

    /// 撑满容器（VTE 必须显式 expand，否则最大化后右边留白）。
    pub fn set_expand(&self, hexpand: bool, vexpand: bool) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                t.set_hexpand(hexpand);
                t.set_vexpand(vexpand);
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                v.area().set_hexpand(hexpand);
                v.area().set_vexpand(vexpand);
            }
        }
    }

    /* ---------------- 外观 ---------------- */

    /// 字号与响铃（会话级覆盖）。
    pub fn set_appearance(&self, font_size: Option<f64>, bell: Option<bool>) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::set_appearance(t, font_size, bell),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ ghostty 后端：字号自己算（响铃暂不支持——内核的
                // BEL 事件已经在事件流里，应用层要的话自己去取）。
                // ⚠️ `bell` 在这条分支用不上 ⇒ 显式丢掉（否则 Windows 上
                // 编译会报「unused variable」）。
                let _ = bell;
                // ⚠️ 字体规则与 VTE 后端**共用一份**（见 `font_description`）。
                v.set_font(crate::termview::font_description(font_size));
            }
        }
    }

    /// 按当前配色方案刷终端配色。
    pub fn apply_colors(&self) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::apply_colors(t),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ ghostty 自绘用的是**内核自己那套配色**（OSC 改色
                // 也要生效）⇒ 这里不刷界面主题的配色。
                v.queue_draw();
            }
        }
    }

    /// 按指定配色方案刷终端配色。
    pub fn apply_palette(&self, p: crate::theme::Palette) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::apply_palette(t, p),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                let _ = p;
                v.queue_draw();
            }
        }
    }

    /// 鼠标滚到底自动隐藏指针。
    pub fn set_mouse_autohide(&self, on: bool) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.set_mouse_autohide(on),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ 自绘终端还没有接鼠标 ⇒ 没有「系统指针」可藏。
                let _ = on;
                v.queue_draw();
            }
        }
    }

    /// 回滚缓冲行数。
    pub fn set_scrollback_lines(&self, n: i64) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.set_scrollback_lines(n),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ ghostty 的回滚缓冲是**按字节**限的（内核 Options
                // 里的 max_scrollback），不是按行 ⇒ 这里只能忽略。
                let _ = n;
                v.queue_draw();
            }
        }
    }

    /// 当前回滚缓冲行数（录制时用来判断「行被挤掉了没有」）。
    pub fn scrollback_lines(&self) -> i64 {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.scrollback_lines(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.scrollback_lines(),
        }
    }

    /* ---------------- 输出 / 输入 ---------------- */

    /// 把**远程来的字节**喂给终端。
    pub fn feed(&self, bytes: &[u8]) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.feed(bytes),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.feed(bytes),
        }
    }

    /// 把**用户敲的字节**写给子进程。
    pub fn feed_child(&self, bytes: &[u8]) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.feed_child(bytes),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.feed_child(bytes),
        }
    }

    /// 清屏 / 复位（重连时用）。
    pub fn reset(&self, hard: bool, clear: bool) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.reset(hard, clear),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.reset(hard, clear),
        }
    }

    /* ---------------- 尺寸 ---------------- */

    /// 行数。
    pub fn row_count(&self) -> i64 {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.row_count(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.row_count(),
        }
    }

    /// 列数。
    pub fn column_count(&self) -> i64 {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.column_count(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.column_count(),
        }
    }

    /// 设置终端尺寸（列 × 行）。
    pub fn set_size(&self, cols: i64, rows: i64) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.set_size(cols, rows),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.set_size(cols, rows),
        }
    }

    /* ---------------- 屏幕内容 ---------------- */

    /// **光标的绝对行号**（从终端启动起算）。
    ///
    /// ⚠️ 两个坑（VTE 后端的说明里有）：`cursor_position()` 返回的是
    /// **(列, 行)**，顺序反直觉；行号是**绝对**的。
    ///
    /// ⚠️ ghostty 后端给的是**视口内**的行号（内核的 `cursor_y()`）——
    /// 视口滚下去之后它就不变了。录制那边要的是绝对行号 ⇒ ghostty
    /// 后端的录制**暂时不准**（阶段 C5 再补）。
    pub fn cursor_row(&self) -> i64 {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::cursor_row(t),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.cursor_row(),
        }
    }

    /// 取绝对行号区间 `[start, end]` 的**带样式**屏幕行（每行一段 ANSI）。
    ///
    /// ⚠️ **统一产出 ANSI**（不是 HTML）——两个后端的取法不同
    /// （VTE 给 HTML、ghostty 给 VT 序列），但上层（录制/日志）只认 ANSI。
    ///
    /// ⚠️ ghostty 后端**只能给视口**（内核的 Formatter 只格式化当前
    /// 视口）⇒ 区间落在视口外时给空串。
    pub fn ansi_rows(&self, start: i64, end: i64) -> Vec<String> {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::ansi_rows(t, start, end),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.ansi_rows(start, end),
        }
    }

    /// 取屏幕文字（粗判「看见提示符了吗」用，不带样式）。
    pub fn screen_text(&self, rows: i64, cols: i64) -> String {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::screen_text(t, rows, cols),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.screen_text(rows, cols),
        }
    }

    /// 取**可见屏幕**的全部文字（不带样式）。
    ///
    /// ⚠️ 返回 `None` 表示「取不到」（不是「屏幕是空的」）——
    /// 终端日志与 PTY 自动应答都靠它扫屏幕。
    pub fn visible_text(&self) -> Option<String> {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::visible_text(t),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.visible_text(),
        }
    }

    /* ---------------- 查找 ---------------- */

    /// 设置查找模式（**普通文本**，不是正则）。
    pub fn search_set_pattern(&self, text: Option<&str>) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::search_set_pattern(t, text),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.search_set_pattern(text),
        }
    }

    /// 找下一个（找到返回 true）。
    pub fn search_find_next(&self) -> bool {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.search_find_next(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.search_find_next(),
        }
    }

    /// 找上一个（找到返回 true）。
    pub fn search_find_previous(&self) -> bool {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.search_find_previous(),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.search_find_previous(),
        }
    }

    /* ---------------- 剪贴板 ---------------- */

    /// 复制选区到剪贴板。
    pub fn copy_selection(&self) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::copy_selection(t),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ ghostty 的选区还没接（内核有 selection 模块，
                // 但我们没做鼠标拖选）⇒ 暂时无事可做。
                v.copy_selection();
            }
        }
    }

    /// 从剪贴板粘贴。
    pub fn paste_clipboard(&self) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => vte::paste_clipboard(t),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.paste_clipboard(),
        }
    }

    /* ---------------- 子进程与标题 ---------------- */

    /// 接管子进程（ssh 起来之后调用，让终端知道 pid）。
    pub fn watch_child(&self, pid: crate::platform::pty::ChildPid) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => t.watch_child(pid),
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                // ⚠️ ghostty 后端不接管子进程——**我们自己管**（PTY 读
                // 线程结束时就知道子进程没了）。
                let _ = pid;
                v.watch_child();
            }
        }
    }

    /// 子进程退出回调（参数是退出状态码）。
    pub fn connect_child_exited<F: Fn(i32) + 'static>(&self, cb: F) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                t.connect_child_exited(move |_t, status| cb(status));
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.connect_child_exited(cb),
        }
    }

    /// 远程标题变化回调（OSC 0/2）。
    pub fn connect_title_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                t.connect_window_title_notify(move |t| {
                    #[allow(deprecated)]
                    let title = t.window_title();
                    if let Some(title) = title.filter(|s| !s.is_empty()) {
                        cb(&title);
                    }
                });
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.connect_title_changed(cb),
        }
    }

    /// OSC 7 目录变化回调（`file://` URI）。
    pub fn connect_cwd_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                t.connect_termprop_changed(None, move |t, prop| {
                    if prop != "vte.current.directory" {
                        return;
                    }
                    #[allow(deprecated)]
                    if let Some(uri) = t.current_directory_uri() {
                        if !uri.is_empty() {
                            cb(&uri);
                        }
                    }
                });
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.connect_cwd_changed(cb),
        }
    }

    /* ---------------- PTY ---------------- */

    /// 把 PTY 交给终端（子进程的输出/输入都走它）。
    ///
    /// ⚠️ **Linux 上必须先接 PTY 再 spawn**（VTE 自己读 master 端）。
    ///
    /// ⚠️ ghostty 后端：**ghostty 不碰 PTY** ⇒ 这里要起读线程把
    /// master 的输出喂给内核，并把 pty 留下来给「写给子进程」用。
    pub fn attach_pty(&self, pty: &crate::platform::pty::PtySession) {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(t) => {
                #[cfg(target_os = "linux")]
                t.set_pty(Some(pty.vte_pty()));
            }
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => v.attach_pty(pty),
        }
    }

    /// 接上**进程内 SSH 通道**（阶段 C7：Windows 上不再 spawn `ssh.exe`）。
    ///
    /// ⚠️ **只有 ghostty 后端支持**：VTE 的输入必须经过一个 pty
    /// （`vte::Pty`）⇒ 没有 pty 就没法把键盘送出去 ⇒ 返回 `false`，
    /// 由调用方决定怎么办（Linux 主线本来就 spawn `ssh`，用不到这条）。
    pub fn attach_shell(
        &self,
        chan: crate::backend::shell::ShellChannel,
        rx: async_channel::Receiver<crate::backend::shell::ShellEvent>,
    ) -> bool {
        match self {
            #[cfg(target_os = "linux")]
            TermView::Vte(_) => false,
            #[cfg(any(feature = "ghostty", target_os = "windows"))]
            TermView::Ghostty(v) => {
                v.attach_shell(chan, rx);
                true
            }
        }
    }
}

impl Default for TermView {
    fn default() -> Self {
        Self::new()
    }
}
