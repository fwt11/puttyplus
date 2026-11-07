//! 终端后端抽象（跨平台版 阶段 C1）。
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

pub mod vte;

use gtk::prelude::*;
use glib::prelude::*;
use vte4::prelude::*;

/// 终端视图：后端无关的句柄。
///
/// ⚠️ 克隆是**浅拷贝**（两个句柄指着同一个终端控件）——与改造前
/// `vte::Terminal` 的克隆语义一致。
#[derive(Clone)]
pub enum TermView {
    /// Linux：VTE 终端控件。
    Vte(vte4::Terminal),
}

impl TermView {
    /// 新建一个终端控件（还没接 PTY、没设外观）。
    pub fn new() -> Self {
        TermView::Vte(vte4::Terminal::new())
    }

    /// 用**现成的 VTE 终端控件**包一层。
    ///
    /// ⚠️ **Linux 专用**：给那些直接操作 VTE 控件的工具用
    /// （录制比对的 example 等）。跨平台的后端应当用 [`TermView::new`]。
    pub fn from_vte(t: vte4::Terminal) -> Self {
        TermView::Vte(t)
    }

    /* ---------------- GTK 侧 ---------------- */

    /// 终端控件本体（塞进容器 / 挂控制器都用它）。
    pub fn widget(&self) -> &gtk::Widget {
        match self {
            TermView::Vte(t) => t.upcast_ref::<gtk::Widget>(),
        }
    }

    /// 这个控件**是不是一个终端**（快速命令弹层据此决定弹层指向哪）。
    pub fn is_terminal_widget(w: &gtk::Widget) -> bool {
        w.is::<vte4::Terminal>()
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
        w.downcast_ref::<vte4::Terminal>().cloned().map(TermView::Vte)
    }

    /// 挂一个事件控制器（按键、手势、焦点……）。
    pub fn add_controller(&self, c: impl IsA<gtk::EventController>) {
        match self {
            TermView::Vte(t) => t.add_controller(c),
        }
    }

    /// 把焦点抢到终端上。
    pub fn grab_focus(&self) {
        match self {
            TermView::Vte(t) => {
                t.grab_focus();
            }
        }
    }

    /// 撑满容器（VTE 必须显式 expand，否则最大化后右边留白）。
    pub fn set_expand(&self, hexpand: bool, vexpand: bool) {
        match self {
            TermView::Vte(t) => {
                t.set_hexpand(hexpand);
                t.set_vexpand(vexpand);
            }
        }
    }

    /* ---------------- 外观 ---------------- */

    /// 字号与响铃（会话级覆盖）。
    pub fn set_appearance(&self, font_size: Option<f64>, bell: Option<bool>) {
        match self {
            TermView::Vte(t) => vte::set_appearance(t, font_size, bell),
        }
    }

    /// 按当前配色方案刷终端配色。
    pub fn apply_colors(&self) {
        match self {
            TermView::Vte(t) => vte::apply_colors(t),
        }
    }

    /// 按指定配色方案刷终端配色。
    pub fn apply_palette(&self, p: crate::theme::Palette) {
        match self {
            TermView::Vte(t) => vte::apply_palette(t, p),
        }
    }

    /// 鼠标滚到底自动隐藏指针。
    pub fn set_mouse_autohide(&self, on: bool) {
        match self {
            TermView::Vte(t) => t.set_mouse_autohide(on),
        }
    }

    /// 回滚缓冲行数。
    pub fn set_scrollback_lines(&self, n: i64) {
        match self {
            TermView::Vte(t) => t.set_scrollback_lines(n),
        }
    }

    /// 当前回滚缓冲行数（录制时用来判断「行被挤掉了没有」）。
    pub fn scrollback_lines(&self) -> i64 {
        match self {
            TermView::Vte(t) => t.scrollback_lines(),
        }
    }

    /* ---------------- 输出 / 输入 ---------------- */

    /// 把**远端来的字节**喂给终端。
    pub fn feed(&self, bytes: &[u8]) {
        match self {
            TermView::Vte(t) => t.feed(bytes),
        }
    }

    /// 把**用户敲的字节**写给子进程。
    pub fn feed_child(&self, bytes: &[u8]) {
        match self {
            TermView::Vte(t) => t.feed_child(bytes),
        }
    }

    /// 清屏 / 复位（重连时用）。
    pub fn reset(&self, hard: bool, clear: bool) {
        match self {
            TermView::Vte(t) => t.reset(hard, clear),
        }
    }

    /* ---------------- 尺寸 ---------------- */

    /// 行数。
    pub fn row_count(&self) -> i64 {
        match self {
            TermView::Vte(t) => t.row_count(),
        }
    }

    /// 列数。
    pub fn column_count(&self) -> i64 {
        match self {
            TermView::Vte(t) => t.column_count(),
        }
    }

    /// 设置终端尺寸（列 × 行）。
    pub fn set_size(&self, cols: i64, rows: i64) {
        match self {
            TermView::Vte(t) => t.set_size(cols, rows),
        }
    }

    /* ---------------- 屏幕内容 ---------------- */

    /// **光标的绝对行号**（从终端启动起算）。
    ///
    /// ⚠️ 两个坑（VTE 后端的说明里有）：`cursor_position()` 返回的是
    /// **(列, 行)**，顺序反直觉；行号是**绝对**的。
    pub fn cursor_row(&self) -> i64 {
        match self {
            TermView::Vte(t) => vte::cursor_row(t),
        }
    }

    /// 取绝对行号区间 `[start, end]` 的**带样式**屏幕行（每行一段 ANSI）。
    ///
    /// ⚠️ **统一产出 ANSI**（不是 HTML）——两个后端的取法不同
    /// （VTE 给 HTML、ghostty 给 VT 序列），但上层（录制/日志）只认 ANSI。
    pub fn ansi_rows(&self, start: i64, end: i64) -> Vec<String> {
        match self {
            TermView::Vte(t) => vte::ansi_rows(t, start, end),
        }
    }

    /// 取屏幕文字（粗判「看见提示符了吗」用，不带样式）。
    pub fn screen_text(&self, rows: i64, cols: i64) -> String {
        match self {
            TermView::Vte(t) => vte::screen_text(t, rows, cols),
        }
    }

    /// 取**可见屏幕**的全部文字（不带样式）。
    ///
    /// ⚠️ 返回 `None` 表示「取不到」（不是「屏幕是空的」）——
    /// 终端日志与 PTY 自动应答都靠它扫屏幕。
    pub fn visible_text(&self) -> Option<String> {
        match self {
            TermView::Vte(t) => vte::visible_text(t),
        }
    }

    /* ---------------- 查找 ---------------- */

    /// 设置查找模式（**普通文本**，不是正则）。
    pub fn search_set_pattern(&self, text: Option<&str>) {
        match self {
            TermView::Vte(t) => vte::search_set_pattern(t, text),
        }
    }

    /// 找下一个（找到返回 true）。
    pub fn search_find_next(&self) -> bool {
        match self {
            TermView::Vte(t) => t.search_find_next(),
        }
    }

    /// 找上一个（找到返回 true）。
    pub fn search_find_previous(&self) -> bool {
        match self {
            TermView::Vte(t) => t.search_find_previous(),
        }
    }

    /* ---------------- 剪贴板 ---------------- */

    /// 复制选区到剪贴板。
    pub fn copy_selection(&self) {
        match self {
            TermView::Vte(t) => vte::copy_selection(t),
        }
    }

    /// 从剪贴板粘贴。
    pub fn paste_clipboard(&self) {
        match self {
            TermView::Vte(t) => vte::paste_clipboard(t),
        }
    }

    /* ---------------- 子进程与标题 ---------------- */

    /// 接管子进程（ssh 起来之后调用，让终端知道 pid）。
    pub fn watch_child(&self, pid: glib::Pid) {
        match self {
            TermView::Vte(t) => t.watch_child(pid),
        }
    }

    /// 子进程退出回调（参数是退出状态码）。
    pub fn connect_child_exited<F: Fn(i32) + 'static>(&self, cb: F) {
        match self {
            TermView::Vte(t) => {
                t.connect_child_exited(move |_t, status| cb(status));
            }
        }
    }

    /// 远程标题变化回调（OSC 0/2）。
    pub fn connect_title_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        match self {
            TermView::Vte(t) => {
                t.connect_window_title_notify(move |t| {
                    #[allow(deprecated)]
                    let title = t.window_title();
                    if let Some(title) = title.filter(|s| !s.is_empty()) {
                        cb(&title);
                    }
                });
            }
        }
    }

    /// OSC 7 目录变化回调（`file://` URI）。
    pub fn connect_cwd_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        match self {
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
        }
    }

    /* ---------------- PTY ---------------- */

    /// 把 PTY 交给终端（子进程的输出/输入都走它）。
    ///
    /// ⚠️ **Linux 上必须先接 PTY 再 spawn**（VTE 自己读 master 端）。
    pub fn attach_pty(&self, pty: &crate::platform::pty::PtySession) {
        match self {
            TermView::Vte(t) => {
                #[cfg(target_os = "linux")]
                t.set_pty(Some(pty.vte_pty()));
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = pty;
                }
            }
        }
    }
}

impl Default for TermView {
    fn default() -> Self {
        Self::new()
    }
}
