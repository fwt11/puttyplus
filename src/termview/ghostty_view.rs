//! ghostty 自绘终端控件（跨平台版 阶段 C3）。
//!
//! ## 为什么不能直接用 GTK 控件
//!
//! VTE（libvte）是 Linux 专属的 GTK 控件，Windows 上根本没有 ⇒
//! 终端这一层只能**自己画**：一个 [`gtk::DrawingArea`] + cairo + pango。
//!
//! ## 画字怎么画（最大的坑）
//!
//! ⚠️ **cairo 的「玩具文字」接口只认 ASCII** —— `cairo_show_text`
//! 遇到中文会画成一串问号（AGENTS.md 里记着这个坑：ER 图的表框改用
//! 真控件就是因为它）。
//!
//! 正解是 **pango 排版 + pangocairo 落笔**：`pango_cairo_show_layout`
//! 把 Pango 排好的版画到 cairo 上，字体回退、中文、emoji 全对。
//!
//! ⚠️ **gtk4-rs 没有绑 pangocairo**（`gtk::pango` 只是 pango crate 的
//! 重导出，没有 `pango_cairo_*`）⇒ 这里自己 extern 两个函数。
//!
//! ## 输入法怎么挂
//!
//! ⚠️ **GTK4 的 `WidgetExt` 没有 `set_im_context`**（GTK3 有）⇒
//! 自绘控件要自己挂：
//!
//! 1. `im.set_client_widget(Some(&area))`（GTK4 的 `set_client_window`
//!    在 GTK4 里没了，`set_client_widget` 才是对的那个）；
//! 2. 焦点进出时 `im.focus_in()` / `im.focus_out()`；
//! 3. 按键先给输入法 `im.filter_keypress(event)`，**它吃掉了就别再
//!    当普通按键处理**（否则预编辑时会打出重复的字符）。
//!
//! 预编辑（还没上屏的拼音）画在光标处；commit 之后按 UTF-8 字节发出去。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use super::ghostty::{GhosttyTerm, KeyAction, KeyCode, KeyMods};
use ghostty_vt::render::{CellIterator, RenderState, RowIterator};
use ghostty_vt::style::RgbColor;

/* ---------------- pangocairo（gtk4-rs 没绑） ---------------- */

mod pangocairo {
    use std::sync::OnceLock;

    use glib::translate::ToGlibPtr;
    use gtk::pango;

    #[link(name = "pangocairo-1.0")]
    unsafe extern "C" {
        fn pango_cairo_font_map_get_default() -> *mut pango::ffi::PangoFontMap;
        // ⚠️⚠️ **参数顺序是 (cairo_t*, PangoLayout*)**——cairo 在**前**！
        // 写成 (PangoContext*, PangoLayout*) 会让 cairo 拿着 PangoContext
        // 的指针去建 surface ⇒ 直接 abort
        // （症状：`cairo-surface.c: _cairo_surface_create_in_error`）。
        fn pango_cairo_update_layout(
            cr: *mut gtk::cairo::ffi::cairo_t,
            layout: *mut pango::ffi::PangoLayout,
        );
        fn pango_cairo_show_layout(
            cr: *mut gtk::cairo::ffi::cairo_t,
            layout: *mut pango::ffi::PangoLayout,
        );
    }

    #[link(name = "pango-1.0")]
    unsafe extern "C" {
        fn pango_font_map_create_context(
            font_map: *mut pango::ffi::PangoFontMap,
        ) -> *mut pango::ffi::PangoContext;
    }

    /// 一个**用 cairo font map 建的** PangoContext（全局一份）。
    ///
    /// ⚠️⚠️ **别用 GTK 给的 `widget.pango_context()`**：那是 **Fc**
    /// font map 的 context，**和 pangocairo 混用会炸**（pango 的
    /// cairo 后端要往 font map 里塞 cairo 的字体选项，遇到不是
    /// cairo font map 的就一路走到「建不出 surface」而 abort——
    /// 症状是 `cairo-surface.c: _cairo_surface_create_in_error`）。
    ///
    /// 正确的做法就是 pangocairo 官方用法：**自己用
    /// `pango_cairo_font_map_get_default()` 建一个 context**。
    ///
    /// ⚠️ 这个 context 是**故意泄漏**的（PangoContext 不是 Send/Sync，
    /// 没法放进 `OnceLock<T>`；存裸指针又会被 Send 检查拦下 ⇒ 存
    /// `usize`）。它一辈子只用一次，泄漏一个 context 无所谓。
    pub fn context() -> &'static pango::Context {
        static PTR: OnceLock<usize> = OnceLock::new();
        let ptr = *PTR.get_or_init(|| unsafe {
            let font_map = pango_cairo_font_map_get_default();
            let ctx = pango_font_map_create_context(font_map);
            let wrapper = glib::translate::from_glib_full(ctx);
            Box::leak(Box::new(wrapper)) as *const pango::Context as usize
        });
        unsafe { &*(ptr as *const pango::Context) }
    }

    /// 按 cairo 上下文的字体设置刷新 layout（换字体/改字号后要调）。
    pub unsafe fn update_layout(cr: &gtk::cairo::Context, layout: &pango::Layout) {
        unsafe {
            pango_cairo_update_layout(cr.to_raw_none(), layout.to_glib_none().0);
        }
    }

    /// 把 layout 画到 cairo 上（当前 cairo 源色就是文字颜色）。
    pub unsafe fn show_layout(cr: &gtk::cairo::Context, layout: &pango::Layout) {
        unsafe {
            pango_cairo_show_layout(cr.to_raw_none(), layout.to_glib_none().0);
        }
    }
}

/* ---------------- 状态 ---------------- */

/// 控件上挂 State's 的键（从控件反查后端句柄用）。
const STATE_DATA_KEY: &str = "pp-ghostty-state";

/// 自绘终端控件的 CSS 类（`is_terminal_widget` 靠它认人）。
///
/// ⚠️ 不能只靠「是不是 DrawingArea」认——界面上别的 DrawingArea 多的是。
const TERM_CSS_CLASS: &str = "pp-ghostty-term";

/// 控件的全部可变状态（放在 `Rc` 里，好让 GTK 回调共享）。
struct State {
    /// 控件本体（**弱引用**）。
    ///
    /// ⚠️ 必须是弱的：控件的 data 里存着本 State（`Rc<State>`）⇒
    /// 强引用会成环（控件 → State → 控件），关掉标签也回收不掉。
    area: glib::WeakRef<gtk::DrawingArea>,
    /// 终端内核。
    term: RefCell<GhosttyTerm>,
    /// 渲染状态（ghostty 的增量刷新机制）。
    render: RefCell<RenderState>,
    /// 行迭代器。
    rows: RefCell<RowIterator>,
    /// 单元格迭代器。
    cells: RefCell<CellIterator>,
    /// 字体。
    font: RefCell<gtk::pango::FontDescription>,
    /// 单元格尺寸（像素）。
    cell: RefCell<(f64, f64)>,
    /// 输出回调：把字节交给子进程（PTY）。
    out: RefCell<Option<Rc<dyn Fn(&[u8])>>>,
    /// 输入法（GTK4 里要自己挂到控件上）。
    im: RefCell<Option<gtk::IMMulticontext>>,
    /// 输入法的预编辑文字（还没上屏的拼音）。
    preedit: RefCell<String>,
    /// 光标闪烁相位（true = 画实心光标）。
    cursor_on: Cell<bool>,
    /// 挂着的 PTY（写给子进程用）。
    pty: RefCell<Option<crate::platform::pty::PtySession>>,
    /// 远程标题变化的回调（OSC 0/2）。
    title_cb: RefCell<Vec<Box<dyn Fn(&str)>>>,
    /// OSC 7 目录变化的回调。
    cwd_cb: RefCell<Vec<Box<dyn Fn(&str)>>>,
    /// 子进程退出的回调。
    exit_cb: RefCell<Option<Box<dyn Fn(i32)>>>,
}

impl State {
    /// 把字节交给输出回调。
    fn emit(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let cb = self.out.borrow().clone();
        if let Some(cb) = cb {
            cb(bytes);
        }
    }

    /// 重画（GTK 控件那边）。
    fn queue_draw(&self, area: &gtk::DrawingArea) {
        area.queue_draw();
    }

    /// 重画（控件可能已经没了 ⇒ 取不到就跳过）。
    fn queue_draw_self(&self) {
        if let Some(area) = self.area.upgrade() {
            area.queue_draw();
        }
    }

    /// 设置视口大小（**内核与 pty 一起**）。
    ///
    /// ⚠️ 两者必须同步：内核决定画几列，pty 决定**远端**以为屏幕多大。
    /// 不同步 ⇒ 远端一直按旧尺寸排版（画面看着正常其实错位）。
    fn set_term_size(&self, cols: u16, rows: u16) {
        let _ = self.term.borrow_mut().set_size(cols, rows);
        let pty = self.pty.borrow().clone();
        if let Some(p) = pty {
            let _ = p.set_size(cols, rows);
        }
    }

    /// 写给子进程（pty 在的话）。
    fn write_to_child(&self, bytes: &[u8]) {
        let pty = self.pty.borrow().clone();
        if let Some(p) = pty {
            let _ = p.write(bytes);
        }
    }

    /// 触发「远程标题变了」的回调。
    ///
    /// ⚠️ **先把回调取出来再调**（`drain` 出去、调完再放回去）——
    /// 回调里要是又碰这个 RefCell（比如顺手改标题），直接
    /// `borrow()` 就会死锁。
    fn fire_title(&self, title: &str) {
        let cbs: Vec<Box<dyn Fn(&str)>> = self.title_cb.borrow_mut().drain(..).collect();
        for cb in &cbs {
            cb(title);
        }
        self.title_cb.borrow_mut().extend(cbs);
    }

    /// 触发「远程目录变了」的回调。
    fn fire_cwd(&self, uri: &str) {
        let cbs: Vec<Box<dyn Fn(&str)>> = self.cwd_cb.borrow_mut().drain(..).collect();
        for cb in &cbs {
            cb(uri);
        }
        self.cwd_cb.borrow_mut().extend(cbs);
    }

    /// 触发「子进程退出了」的回调（只报一次）。
    fn fire_exit(&self, status: i32) {
        let cb = self.exit_cb.borrow_mut().take();
        if let Some(cb) = cb {
            cb(status);
        }
    }
}

/// 一段连续的、样式相同的单元格（画的时候合成一次，省一堆 layout）。
#[derive(Default)]
struct Run {
    /// 段：`(文字, 占几列)`。
    ///
    /// ⚠️ **空格也要占位**：空单元格没有文字，若直接拼起来，
    /// 行首的缩进就整个丢了（"    marker" 被画成 "marker" 贴在左边）。
    segs: Vec<(String, u16)>,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    bold: bool,
    underline: bool,
    selected: bool,
    /// 总共占了几列。
    cols: u16,
}

impl Run {
    /// 加一个单元格（`text` 为空就是空格）。
    ///
    /// ⚠️ **一个单元格 = 一列**：网格里每个格子放一个 grapheme ⇒
    /// 合并文字时列数也要跟着加（漏了的话 x 会短一截）。
    ///
    /// ⚠️ `width` 是这个格子**占几列**：宽字符（中文）占 2 列。
    fn push(&mut self, text: &str, width: u16) {
        if text.is_empty() {
            // 连续的空格合成一段
            match self.segs.last_mut() {
                Some((t, n)) if t.is_empty() => *n += width,
                _ => self.segs.push((String::new(), width)),
            }
        } else {
            match self.segs.last_mut() {
                Some((t, n)) if !t.is_empty() => {
                    t.push_str(text);
                    *n += width;
                }
                _ => self.segs.push((text.to_string(), width)),
            }
        }
        self.cols += width;
    }

    /// 整段文字（空格补齐）。
    fn text(&self) -> String {
        let mut out = String::new();
        for (t, n) in &self.segs {
            out.push_str(t);
            for _ in t.chars().count() as u16..*n {
                out.push(' ');
            }
        }
        out
    }
}

/* ---------------- 控件 ---------------- */

/// 自绘的终端控件（DrawingArea + ghostty 内核）。
#[derive(Clone)]
pub struct GhosttyView {
    area: gtk::DrawingArea,
    state: Rc<State>,
}

impl GhosttyView {
    /// 新建一个 80×24 的自绘终端。
    pub fn new() -> Self {
        let term = GhosttyTerm::new(80, 24).expect("建 ghostty 终端");
        let render = RenderState::new().expect("建渲染状态");
        let rows = RowIterator::new().expect("建行迭代器");
        let cells = CellIterator::new().expect("建单元格迭代器");
        let font = gtk::pango::FontDescription::from_string("Monospace 10.5");
        let area = gtk::DrawingArea::new();
        area.set_focusable(true);
        area.set_can_focus(true);
        area.set_hexpand(true);
        area.set_vexpand(true);
        if let Some(cur) = gdk::Cursor::from_name("text", None) {
            area.set_cursor(Some(&cur));
        }

        area.add_css_class(TERM_CSS_CLASS);
        let state = Rc::new(State {
            area: glib::object::ObjectExt::downgrade(&area),
            term: RefCell::new(term),
            render: RefCell::new(render),
            rows: RefCell::new(rows),
            cells: RefCell::new(cells),
            font: RefCell::new(font),
            cell: RefCell::new((0.0, 0.0)),
            out: RefCell::new(None),
            im: RefCell::new(None),
            preedit: RefCell::new(String::new()),
            cursor_on: Cell::new(true),
            pty: RefCell::new(None),
            title_cb: RefCell::new(Vec::new()),
            cwd_cb: RefCell::new(Vec::new()),
            exit_cb: RefCell::new(None),
        });
        // 控件上挂一份 State ⇒ 能从控件反查回后端句柄
        // （`TermView::from_widget` 要用）。
        //
        // SAFETY：State 是 'static 的（只含 GTK 对象与 Rc）⇒ glib 的
        // data 槽能安全持有它；控件销毁时 glib 会把它 drop 掉。
        unsafe {
            area.set_data(STATE_DATA_KEY, state.clone());
        }

        let view = GhosttyView { area, state };
        view.install_draw();
        view.install_keys();
        view.install_ime();
        view
    }

    /// 控件本体（塞进容器用）。
    pub fn widget(&self) -> &gtk::Widget {
        self.area.upcast_ref::<gtk::Widget>()
    }

    /// DrawingArea 本体（要拿焦点、加控制器时用）。
    pub fn area(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// 设输出回调（把字节交给子进程）。
    pub fn set_output_handler<F: Fn(&[u8]) + 'static>(&self, f: F) {
        *self.state.out.borrow_mut() = Some(Rc::new(f));
    }

    /// 换字体。
    pub fn set_font(&self, desc: gtk::pango::FontDescription) {
        *self.state.font.borrow_mut() = desc;
        self.area.queue_draw();
    }

    /// 把远端来的字节喂给内核（并重画）。
    ///
    /// ⚠️ **喂完要把事件取出来**（标题变了、目录变了、响铃……）——
    /// 不取的话事件队列会一直涨，标题回调也永远不触发。
    pub fn feed(&self, bytes: &[u8]) {
        let events = {
            let mut term = self.state.term.borrow_mut();
            term.feed(bytes);
            term.drain_events()
        };
        self.handle_events(events);
        self.area.queue_draw();
    }

    /// 处理内核排队的事件（标题、目录、响铃……）。
    fn handle_events(&self, events: Vec<ghostty_vt::terminal::Event>) {
        use ghostty_vt::terminal::Event;
        for ev in events {
            match ev {
                Event::TitleChanged(title) => {
                    if !title.is_empty() {
                        self.state.fire_title(&title);
                    }
                }
                Event::PwdChanged(uri) => {
                    if !uri.is_empty() {
                        self.state.fire_cwd(&uri);
                    }
                }
                _ => {}
            }
        }
    }

    /// 模拟一次按键（**走的是真实按键回调同一条路**）。
    ///
    /// ⚠️ 给冒烟/测试用：GTK 的按键回调自己也会调它 ⇒ 测的就是
    /// 「按键 ⇒ 字节」这条真路径，不是另写一份。
    pub fn press_key(&self, keyval: gdk::Key, mods: gdk::ModifierType) {
        handle_key(&self.state, keyval, mods);
    }

    /// 模拟输入法的 commit（**走的是真实 commit 回调同一条路**）。
    pub fn commit_text(&self, text: &str) {
        commit_text(&self.state, text);
    }

    /* ---------------- 尺寸 / 内容 ---------------- */

    /// 视口行数。
    pub fn row_count(&self) -> i64 {
        self.state.term.borrow().row_count()
    }

    /// 视口列数。
    pub fn column_count(&self) -> i64 {
        self.state.term.borrow().column_count()
    }

    /// 设置视口大小（**并同步给 pty**）。
    ///
    /// ⚠️ 远端 ssh 靠 pty 的窗口大小知道屏幕多大 ⇒ 不同步的话，
    /// 远端会一直按 80×24 排版（画面能看，但不对齐）。
    pub fn set_size(&self, cols: i64, rows: i64) {
        self.state
            .set_term_size(cols.max(1) as u16, rows.max(1) as u16);
        self.area.queue_draw();
    }

    /// 光标行号。
    ///
    /// ⚠️ **视口内**的行号（内核的 `cursor_y()`）——VTE 那边给的是
    /// **绝对**行号 ⇒ 两个后端这里语义不同，录制那边要注意。
    pub fn cursor_row(&self) -> i64 {
        self.state.term.borrow().cursor_row()
    }

    /// 回滚缓冲里的行数（屏幕外还存着的历史行）。
    pub fn scrollback_lines(&self) -> i64 {
        let t = self.state.term.borrow();
        (t.total_rows() - t.row_count()).max(0)
    }

    /// 带样式的屏幕行（每行一段 ANSI）。
    ///
    /// ⚠️ **行号是视口内的**（0 起）——ghostty 的 Formatter 只能格式化
    /// 当前视口，给不了绝对行号区间。
    pub fn ansi_rows(&self, start: i64, end: i64) -> Vec<String> {
        let rows = self.state.term.borrow().ansi_rows();
        let mut out = Vec::new();
        for i in start..=end {
            if i < 0 || i as usize >= rows.len() {
                out.push(String::new());
            } else {
                out.push(rows[i as usize].clone());
            }
        }
        out
    }

    /// 屏幕文字（不带样式）。
    pub fn screen_text(&self, _rows: i64, _cols: i64) -> String {
        self.state.term.borrow().screen_text()
    }

    /// 可见屏幕的文字。
    ///
    /// ⚠️ ghostty 侧**总是取得到**（内核里就有屏幕）⇒ 不会返回 `None`。
    pub fn visible_text(&self) -> Option<String> {
        Some(self.state.term.borrow().screen_text())
    }

    /* ---------------- 查找 ---------------- */

    /// 设置查找模式（**普通文本**，不是正则）。
    pub fn search_set_pattern(&self, text: Option<&str>) {
        self.state.term.borrow_mut().search_set_pattern(text);
    }

    /// 找下一个。
    pub fn search_find_next(&self) -> bool {
        self.state.term.borrow_mut().search_find_next()
    }

    /// 找上一个。
    pub fn search_find_previous(&self) -> bool {
        self.state.term.borrow_mut().search_find_previous()
    }

    /* ---------------- 剪贴板 ---------------- */

    /// 复制选区到剪贴板。
    ///
    /// ⚠️ **还没做鼠标选区**（内核有 selection 模块，我们没接拖选）
    /// ⇒ 现在复制的是空选区。
    pub fn copy_selection(&self) {}

    /// 从剪贴板粘贴。
    ///
    /// ⚠️ GTK4 的剪贴板**只能异步读** ⇒ 读完再写给子进程。
    pub fn paste_clipboard(&self) {
        let state = self.state.clone();
        crate::platform::clipboard::read_text(move |text| {
            if let Some(t) = text {
                state.write_to_child(t.as_bytes());
            }
        });
    }

    /* ---------------- 子进程 ---------------- */

    /// 接管子进程。
    ///
    /// ⚠️ ghostty 后端**不接管**——子进程归 PTY 层管（读线程结束时
    /// 就知道它没了）⇒ 这里什么都不做。
    pub fn watch_child(&self) {}

    /// 子进程退出回调（退出时会被调一次）。
    pub fn connect_child_exited<F: Fn(i32) + 'static>(&self, cb: F) {
        *self.state.exit_cb.borrow_mut() = Some(Box::new(cb));
    }

    /// 远程标题变化回调（OSC 0/2）。
    pub fn connect_title_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        self.state.title_cb.borrow_mut().push(Box::new(cb));
    }

    /// OSC 7 目录变化回调（`file://` URI）。
    pub fn connect_cwd_changed<F: Fn(&str) + 'static>(&self, cb: F) {
        self.state.cwd_cb.borrow_mut().push(Box::new(cb));
    }

    /* ---------------- PTY ---------------- */

    /// 把 PTY 交给终端：起读线程把 master 的输出喂给内核。
    ///
    /// ⚠️ 与 VTE 后端的区别：**VTE 自己读 master**（`set_pty` 之后），
    /// ghostty 不碰 PTY ⇒ 这里要自己读。
    ///
    /// ⚠️ **读线程结束时 = 子进程没了**（pty 关闭）⇒ 这时上报退出。
    pub fn attach_pty(&self, pty: &crate::platform::pty::PtySession) {
        *self.state.pty.borrow_mut() = Some(pty.clone());

        // ⚠️ **pty 的窗口大小要跟终端对齐**：远端 ssh 靠它知道屏幕多大
        // （不同步 ⇒ 远端一直按 80×24 排版，画面和实际宽度对不上）。
        {
            let t = self.state.term.borrow();
            let _ = pty.set_size(t.column_count().max(1) as u16, t.row_count().max(1) as u16);
        }

        // worker 线程读 pty ⇒ 事件通道 ⇒ 主线程喂给内核
        // （glib 0.22 移除了 MainContext::channel，用 async-channel +
        // 主线程 spawn_local 循环）。
        let (tx, rx) = async_channel::unbounded::<Vec<u8>>();
        let state = self.state.clone();
        if pty
            .spawn_reader(Box::new(move |bytes| {
                let _ = tx.send_blocking(bytes.to_vec());
            }))
            .is_err()
        {
            return;
        }
        let area = self.area.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(bytes) = rx.recv().await {
                {
                    let mut term = state.term.borrow_mut();
                    term.feed(&bytes);
                    let events = term.drain_events();
                    drop(term);
                    // 事件（标题/目录）要在**主线程**回调 ⇒ 这里直接处理。
                    for ev in events {
                        use ghostty_vt::terminal::Event;
                        match ev {
                            Event::TitleChanged(t) if !t.is_empty() => state.fire_title(&t),
                            Event::PwdChanged(u) if !u.is_empty() => state.fire_cwd(&u),
                            _ => {}
                        }
                    }
                }
                area.queue_draw();
            }
            // 读完了 ⇒ pty 关闭 ⇒ 子进程大概没了
            state.fire_exit(0);
        });
    }

    /// 把**用户敲的字节**写给子进程。
    pub fn feed_child(&self, bytes: &[u8]) {
        self.state.write_to_child(bytes);
    }

    /// 复位（重连时用）。
    pub fn reset(&self, _hard: bool, _clear: bool) {
        self.state.term.borrow_mut().reset();
        self.area.queue_draw();
    }

    /// 重画。
    pub fn queue_draw(&self) {
        self.area.queue_draw();
    }

    /// 这个控件**是不是一个自绘终端**。
    pub fn is_terminal_widget(w: &gtk::Widget) -> bool {
        w.has_css_class(TERM_CSS_CLASS)
    }

    /// 从控件反查回后端句柄。
    pub fn from_widget(w: &gtk::Widget) -> Option<Self> {
        let area = w.downcast_ref::<gtk::DrawingArea>()?;
        if !area.has_css_class(TERM_CSS_CLASS) {
            return None;
        }
        // SAFETY：data 里存的是 `Rc<State>`（'static）⇒ 指针有效，
        // 直到控件销毁为止（我们此刻正拿着控件的引用）。
        let ptr = unsafe { area.data::<Rc<State>>(STATE_DATA_KEY) }?;
        let state = unsafe { (*ptr.as_ptr()).clone() };
        Some(GhosttyView {
            area: area.clone(),
            state,
        })
    }

    /// 内核的当前屏幕文字（每行一段，去掉行尾空白）。
    ///
    /// ⚠️ 这是**模型**（网格里的字符），不是画出来的像素——用来做断言
    /// 和「看见提示符了吗」那种粗判。
    pub fn screen_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        let mut term = self.state.term.borrow_mut();
        let mut render = self.state.render.borrow_mut();
        let Ok(snapshot) = term.update_render(&mut render) else {
            return rows;
        };
        let mut rows_iter = self.state.rows.borrow_mut();
        let mut cells_iter = self.state.cells.borrow_mut();
        let Ok(mut row_iter) = rows_iter.update(&snapshot) else {
            return rows;
        };
        while let Some(row) = row_iter.next() {
            let mut line = String::new();
            let Ok(mut cell_iter) = cells_iter.update(row) else {
                continue;
            };
            while let Some(cell) = cell_iter.next() {
                // ⚠️ 宽字符的尾巴格不算内容（否则中文后面会多一个空格）
                let wide = cell
                    .raw_cell()
                    .ok()
                    .and_then(|c| c.wide().ok())
                    .unwrap_or(ghostty_vt::screen::CellWide::Narrow);
                if matches!(
                    wide,
                    ghostty_vt::screen::CellWide::SpacerTail
                        | ghostty_vt::screen::CellWide::SpacerHead
                ) {
                    continue;
                }
                let g = cell.graphemes().unwrap_or_default();
                if g.is_empty() {
                    // ⚠️ 空格单元格**没有 grapheme** ⇒ 不补空格的话，
                    // 行首的缩进就整个丢了（"    marker" 变成 "marker"）。
                    line.push(' ');
                } else {
                    line.extend(g);
                }
            }
            rows.push(line.trim_end().to_string());
        }
        rows
    }

    /* ---------------- 画 ---------------- */

    fn install_draw(&self) {
        let area = self.area.clone();
        let state = self.state.clone();
        area.set_draw_func(move |area, cr, _w, _h| {
            draw_frame(area, &state, cr);
        });
    }

    /* ---------------- 按键 ---------------- */

    fn install_keys(&self) {
        let area = self.area.clone();
        let state = self.state.clone();
        let ctrl = gtk::EventControllerKey::new();
        ctrl.connect_key_pressed(move |ctrl, keyval, _code, mods| {
            // ⚠️ **先给输入法**：预编辑中的按键必须被输入法吃掉，
            // 否则拼音会既进预编辑又发出去（屏幕上出现重复字符）。
            if let Some(im) = state.im.borrow().clone() {
                if let Some(ev) = ctrl.current_event() {
                    if im.filter_keypress(ev) {
                        return glib::Propagation::Stop;
                    }
                }
            }
            handle_key(&state, keyval, mods);
            glib::Propagation::Stop
        });
        area.add_controller(ctrl);
    }

    /* ---------------- 输入法 ---------------- */

    fn install_ime(&self) {
        let im = gtk::IMMulticontext::new();
        // ⚠️ GTK4 没有 `Widget::set_im_context` ⇒ 自己挂：
        // `set_client_widget` + 焦点进出时 focus_in/focus_out。
        im.set_client_widget(Some(&self.area));

        let state = self.state.clone();
        im.connect_commit(move |_im, text| {
            commit_text(&state, text);
        });

        let state2 = self.state.clone();
        im.connect_preedit_changed(move |im| {
            let (text, _attrs, _cursor) = im.preedit_string();
            *state2.preedit.borrow_mut() = text.to_string();
            state2.queue_draw_self();
        });

        let im2 = im.clone();
        self.area.connect_has_focus_notify(move |area| {
            if area.has_focus() {
                im2.focus_in();
            } else {
                im2.focus_out();
            }
        });

        *self.state.im.borrow_mut() = Some(im);
    }
}

/* ---------------- 画一帧 ---------------- */

/// 画一帧：背景 → 网格 → 光标 → 预编辑。
fn draw_frame(area: &gtk::DrawingArea, state: &Rc<State>, cr: &gtk::cairo::Context) {
    // 1. 算单元格尺寸（按当前字体量）
    let font = state.font.borrow().clone();
    let (cell_w, cell_h) = measure_cell(area, &font);
    *state.cell.borrow_mut() = (cell_w, cell_h);
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return;
    }

    // 2. 视口大小（按像素算行列）
    let cols = ((area.width() as f64) / cell_w).floor().max(1.0) as u16;
    let rows = ((area.height() as f64) / cell_h).floor().max(1.0) as u16;

    // 3. 尺寸变了就告诉内核（内核会重排屏幕）
    //
    // ⚠️ **必须连 pty 一起改**（`set_term_size`）——远端 ssh 靠 pty 的
    // 窗口大小知道屏幕多大 ⇒ 只改内核的话，远端会一直按旧尺寸排版。
    {
        let changed = {
            let term = state.term.borrow();
            term.column_count() != cols as i64 || term.row_count() != rows as i64
        };
        if changed {
            state.set_term_size(cols, rows);
        }
    }

    // 4. 背景
    let (fg_default, bg_default) = default_colors();
    cr.set_source_rgb(bg_default.0, bg_default.1, bg_default.2);
    cr.rectangle(0.0, 0.0, area.width() as f64, area.height() as f64);
    cr.fill();

    // 5. 网格
    let mut term = state.term.borrow_mut();
    let mut render = state.render.borrow_mut();
    let Ok(snapshot) = term.update_render(&mut render) else {
        return;
    };
    let Ok(defaults) = snapshot.colors() else {
        return;
    };
    let fg_default = (
        defaults.foreground.r as f64 / 255.0,
        defaults.foreground.g as f64 / 255.0,
        defaults.foreground.b as f64 / 255.0,
    );
    let bg_default = (
        defaults.background.r as f64 / 255.0,
        defaults.background.g as f64 / 255.0,
        defaults.background.b as f64 / 255.0,
    );

    let mut cursor_pos: Option<(f64, f64)> = None;
    let mut rows_iter = state.rows.borrow_mut();
    let mut cells_iter = state.cells.borrow_mut();
    let Ok(mut row_iter) = rows_iter.update(&snapshot) else {
        return;
    };
    while let Some(row) = row_iter.next() {
        let vy = row.viewport_y().unwrap_or(-1);
        if vy < 0 || vy as f64 >= rows as f64 {
            continue;
        }
        let y = vy as f64 * cell_h;
        let Ok(mut cell_iter) = cells_iter.update(row) else {
            continue;
        };
        let mut col = 0u16;
        let mut run = Run::default();
        while let Some(cell) = cell_iter.next() {
            // ⚠️ **宽字符（中文）的尾巴格不能画、也不占列**——
            // 画了会多出一个空格，位置就全歪了。
            let wide = cell
                .raw_cell()
                .ok()
                .and_then(|c| c.wide().ok())
                .unwrap_or(ghostty_vt::screen::CellWide::Narrow);
            if matches!(
                wide,
                ghostty_vt::screen::CellWide::SpacerTail
                    | ghostty_vt::screen::CellWide::SpacerHead
            ) {
                continue;
            }
            let cell_width = if wide == ghostty_vt::screen::CellWide::Wide {
                2
            } else {
                1
            };
            let fg = cell.fg_color().ok().flatten();
            let bg = cell.bg_color().ok().flatten();
            let selected = cell.is_selected().unwrap_or(false);
            let style = cell.style().ok();
            let bold = style.map(|s| s.bold).unwrap_or(false);
            let underline = style.map(|s| s.underline != ghostty_vt::style::Underline::None)
                .unwrap_or(false);
            let text: String = cell.graphemes().unwrap_or_default().iter().collect();

            // 样式变了 ⇒ 先把上一段画掉
            if run.cols > 0
                && (run.fg != fg
                    || run.bg != bg
                    || run.bold != bold
                    || run.underline != underline
                    || run.selected != selected)
            {
                draw_run(area, state, cr, &run, col as f64 * cell_w - run.cols as f64 * cell_w, y, cell_w, cell_h, fg_default);
                run = Run::default();
            }
            run.fg = fg;
            run.bg = bg;
            run.bold = bold;
            run.underline = underline;
            run.selected = selected;
            run.push(&text, cell_width);
            col += cell_width;
        }
        if run.cols > 0 {
            draw_run(
                area,
                state,
                cr,
                &run,
                (col - run.cols) as f64 * cell_w,
                y,
                cell_w,
                cell_h,
                fg_default,
            );
        }
    }

    // 6. 光标
    if let Ok(cursor) = snapshot.cursor() {
        if cursor.visible {
            if let Some(vp) = cursor.viewport {
                let x = vp.x as f64 * cell_w;
                let y = vp.y as f64 * cell_h;
                cursor_pos = Some((x, y));
                if state.cursor_on.get() {
                    draw_cursor(cr, x, y, cell_w, cell_h, &defaults.cursor, fg_default, bg_default);
                }
            }
        }
    }

    // 7. 预编辑（输入法还没上屏的拼音，画在光标处）
    let preedit = state.preedit.borrow().clone();
    if !preedit.is_empty() {
        let (x, y) = cursor_pos.unwrap_or((0.0, 0.0));
        draw_text(area, state, cr, &preedit, x, y, cell_w, cell_h, fg_default, false, true);
    }
}

/// 画一段（样式相同的一串单元格）。
fn draw_run(
    area: &gtk::DrawingArea,
    state: &Rc<State>,
    cr: &gtk::cairo::Context,
    run: &Run,
    x: f64,
    y: f64,
    cell_w: f64,
    cell_h: f64,
    fg_default: (f64, f64, f64),
) {
    // 背景色
    if let Some(bg) = run.bg {
        cr.set_source_rgb(bg.r as f64 / 255.0, bg.g as f64 / 255.0, bg.b as f64 / 255.0);
        cr.rectangle(x, y, run.cols as f64 * cell_w, cell_h);
        cr.fill();
    }
    if run.selected {
        // 选区：底色画成前景色（反白），和 VTE 的观感一致
        cr.set_source_rgb(fg_default.0, fg_default.1, fg_default.2);
        cr.rectangle(x, y, run.cols as f64 * cell_w, cell_h);
        cr.fill();
    }
    if run.text().trim().is_empty() {
        return;
    }
    let fg = run.fg.unwrap_or(RgbColor {
        r: (fg_default.0 * 255.0) as u8,
        g: (fg_default.1 * 255.0) as u8,
        b: (fg_default.2 * 255.0) as u8,
    });
    let color = (fg.r as f64 / 255.0, fg.g as f64 / 255.0, fg.b as f64 / 255.0);
    // ⚠️ **按段画**：空格段也要往前挪（否则行首缩进会丢）。
    let mut seg_x = x;
    for (text, cols) in &run.segs {
        if !text.is_empty() {
            draw_text(
                area,
                state,
                cr,
                text,
                seg_x,
                y,
                cell_w,
                cell_h,
                color,
                run.bold,
                run.underline,
            );
        }
        seg_x += *cols as f64 * cell_w;
    }
}

/// 画一串文字（pango 排版 + pangocairo 落笔）。
#[allow(clippy::too_many_arguments)]
fn draw_text(
    area: &gtk::DrawingArea,
    state: &Rc<State>,
    cr: &gtk::cairo::Context,
    text: &str,
    x: f64,
    y: f64,
    _cell_w: f64,
    cell_h: f64,
    color: (f64, f64, f64),
    bold: bool,
    underline: bool,
) {
    let ctx = pangocairo::context();
    let layout = gtk::pango::Layout::new(ctx);
    layout.set_font_description(Some(&state.font.borrow()));
    layout.set_text(text);
    // 粗体 / 下划线：pango 的属性
    if bold || underline {
        let mut attrs = gtk::pango::AttrList::new();
        if bold {
            attrs.insert(gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold));
        }
        if underline {
            attrs.insert(gtk::pango::AttrInt::new_underline(gtk::pango::Underline::Single));
        }
        layout.set_attributes(Some(&attrs));
    }
    unsafe { pangocairo::update_layout(cr, &layout) };
    cr.set_source_rgb(color.0, color.1, color.2);
    // 基线：字体的 ascent 差不多在格子上沿往下 80% 处
    cr.move_to(x, y + cell_h * 0.8);
    unsafe { pangocairo::show_layout(cr, &layout) };
}

/// 画光标（块状）。
fn draw_cursor(
    cr: &gtk::cairo::Context,
    x: f64,
    y: f64,
    cell_w: f64,
    cell_h: f64,
    cursor_color: &Option<RgbColor>,
    fg_default: (f64, f64, f64),
    bg_default: (f64, f64, f64),
) {
    let color = cursor_color.unwrap_or(RgbColor {
        r: (fg_default.0 * 255.0) as u8,
        g: (fg_default.1 * 255.0) as u8,
        b: (fg_default.2 * 255.0) as u8,
    });
    cr.set_source_rgb(color.r as f64 / 255.0, color.g as f64 / 255.0, color.b as f64 / 255.0);
    cr.rectangle(x, y, cell_w, cell_h);
    cr.fill();
    let _ = bg_default;
}

/// 量一个单元格有多大（像素）。
fn measure_cell(area: &gtk::DrawingArea, font: &gtk::pango::FontDescription) -> (f64, f64) {
    let ctx = pangocairo::context();
    let layout = gtk::pango::Layout::new(ctx);
    layout.set_font_description(Some(font));
    layout.set_text("M");
    let (w, _) = layout.pixel_size();
    layout.set_text("M\nM");
    let (_, h2) = layout.pixel_size();
    (w as f64, (h2 / 2).max(1) as f64)
}

/// 默认的前景色/背景色（内核没给的时候用）。
fn default_colors() -> ((f64, f64, f64), (f64, f64, f64)) {
    ((0.85, 0.85, 0.85), (0.10, 0.10, 0.12))
}

/* ---------------- 按键处理 ---------------- */

/// 输入法的 commit ⇒ 按 UTF-8 字节发出去。
fn commit_text(state: &Rc<State>, text: &str) {
    let bytes = state.term.borrow_mut().encode_key(
        KeyAction::Press,
        KeyCode::Unidentified,
        KeyMods::empty(),
        Some(text),
    );
    state.emit(&bytes);
}

/// 一个按键 ⇒ 该发给终端什么。
fn handle_key(state: &Rc<State>, keyval: gdk::Key, mods: gdk::ModifierType) {
    let Some(code) = keycode_from_gdk(keyval) else {
        return;
    };
    let key_mods = mods_from_gdk(mods);
    // 文本：可打印字符才带（控制键不带）。
    // ⚠️ **别滤掉空格**：空格键也要打出 " "（VTE 就是这么做的）——
    // 滤掉的话编码器只能靠逻辑键推，有些布局下会推不出来。
    let text = keyval
        .to_unicode()
        .filter(|c| !c.is_control())
        .map(|c| c.to_string());
    let bytes = state
        .term
        .borrow_mut()
        .encode_key(KeyAction::Press, code, key_mods, text.as_deref());
    state.emit(&bytes);
}

/// GTK 的修饰键 ⇒ ghostty 的修饰键。
fn mods_from_gdk(mods: gdk::ModifierType) -> KeyMods {
    let mut m = KeyMods::empty();
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        m |= KeyMods::SHIFT;
    }
    if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        m |= KeyMods::CTRL;
    }
    if mods.contains(gdk::ModifierType::ALT_MASK) {
        m |= KeyMods::ALT;
    }
    if mods.contains(gdk::ModifierType::SUPER_MASK) {
        m |= KeyMods::SUPER;
    }
    m
}

/// GTK 的键值 ⇒ ghostty 的逻辑键。
///
/// ⚠️ **拿不到就返回 None**（不认识就不发字节）——宁可少发一个键，
/// 也不能发错一个键（发错了会打乱用户的命令行）。
fn keycode_from_gdk(key: gdk::Key) -> Option<KeyCode> {
    use gdk::Key as K;
    Some(match key {
        K::Return | K::KP_Enter => KeyCode::Enter,
        K::Tab | K::KP_Tab => KeyCode::Tab,
        K::BackSpace => KeyCode::Backspace,
        K::Escape => KeyCode::Escape,
        K::space => KeyCode::Space,
        K::Delete | K::KP_Delete => KeyCode::Delete,
        K::Insert | K::KP_Insert => KeyCode::Insert,
        K::Home | K::KP_Home => KeyCode::Home,
        K::End | K::KP_End => KeyCode::End,
        K::Page_Up | K::KP_Page_Up => KeyCode::PageUp,
        K::Page_Down | K::KP_Page_Down => KeyCode::PageDown,
        K::Up | K::KP_Up => KeyCode::ArrowUp,
        K::Down | K::KP_Down => KeyCode::ArrowDown,
        K::Left | K::KP_Left => KeyCode::ArrowLeft,
        K::Right | K::KP_Right => KeyCode::ArrowRight,
        K::F1 => KeyCode::F1,
        K::F2 => KeyCode::F2,
        K::F3 => KeyCode::F3,
        K::F4 => KeyCode::F4,
        K::F5 => KeyCode::F5,
        K::F6 => KeyCode::F6,
        K::F7 => KeyCode::F7,
        K::F8 => KeyCode::F8,
        K::F9 => KeyCode::F9,
        K::F10 => KeyCode::F10,
        K::F11 => KeyCode::F11,
        K::F12 => KeyCode::F12,
        _ => {
            // 字母与数字
            let name = key.name().unwrap_or_default().to_string();
            if let Some(c) = name.chars().next() {
                if c.is_ascii_alphabetic() {
                    return Some(match c {
                        'a' => KeyCode::A,
                        'b' => KeyCode::B,
                        'c' => KeyCode::C,
                        'd' => KeyCode::D,
                        'e' => KeyCode::E,
                        'f' => KeyCode::F,
                        'g' => KeyCode::G,
                        'h' => KeyCode::H,
                        'i' => KeyCode::I,
                        'j' => KeyCode::J,
                        'k' => KeyCode::K,
                        'l' => KeyCode::L,
                        'm' => KeyCode::M,
                        'n' => KeyCode::N,
                        'o' => KeyCode::O,
                        'p' => KeyCode::P,
                        'q' => KeyCode::Q,
                        'r' => KeyCode::R,
                        's' => KeyCode::S,
                        't' => KeyCode::T,
                        'u' => KeyCode::U,
                        'v' => KeyCode::V,
                        'w' => KeyCode::W,
                        'x' => KeyCode::X,
                        'y' => KeyCode::Y,
                        'z' => KeyCode::Z,
                        _ => return None,
                    });
                }
                if c.is_ascii_digit() {
                    return Some(match c {
                        '0' => KeyCode::Digit0,
                        '1' => KeyCode::Digit1,
                        '2' => KeyCode::Digit2,
                        '3' => KeyCode::Digit3,
                        '4' => KeyCode::Digit4,
                        '5' => KeyCode::Digit5,
                        '6' => KeyCode::Digit6,
                        '7' => KeyCode::Digit7,
                        '8' => KeyCode::Digit8,
                        '9' => KeyCode::Digit9,
                        _ => return None,
                    });
                }
            }
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 字母键映射到逻辑键() {
        assert_eq!(keycode_from_gdk(gdk::Key::a), Some(KeyCode::A));
        assert_eq!(keycode_from_gdk(gdk::Key::z), Some(KeyCode::Z));
        assert_eq!(keycode_from_gdk(gdk::Key::m), Some(KeyCode::M));
    }

    #[test]
    fn 数字键映射到数字键() {
        assert_eq!(keycode_from_gdk(gdk::Key::_0), Some(KeyCode::Digit0));
        assert_eq!(keycode_from_gdk(gdk::Key::_9), Some(KeyCode::Digit9));
    }

    #[test]
    fn 控制键映射到控制键() {
        assert_eq!(keycode_from_gdk(gdk::Key::Return), Some(KeyCode::Enter));
        assert_eq!(keycode_from_gdk(gdk::Key::Tab), Some(KeyCode::Tab));
        assert_eq!(keycode_from_gdk(gdk::Key::BackSpace), Some(KeyCode::Backspace));
        assert_eq!(keycode_from_gdk(gdk::Key::Escape), Some(KeyCode::Escape));
    }

    #[test]
    fn 方向键映射到方向键() {
        assert_eq!(keycode_from_gdk(gdk::Key::Up), Some(KeyCode::ArrowUp));
        assert_eq!(keycode_from_gdk(gdk::Key::Down), Some(KeyCode::ArrowDown));
        assert_eq!(keycode_from_gdk(gdk::Key::Left), Some(KeyCode::ArrowLeft));
        assert_eq!(keycode_from_gdk(gdk::Key::Right), Some(KeyCode::ArrowRight));
    }

    #[test]
    fn 不认识的键不发字节() {
        // 超级键、单独的修饰键 ⇒ 没有对应的逻辑键 ⇒ 不发。
        assert_eq!(keycode_from_gdk(gdk::Key::Super_L), None);
        assert_eq!(keycode_from_gdk(gdk::Key::Shift_L), None);
    }

    #[test]
    fn 修饰键映射() {
        let m = mods_from_gdk(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
        assert!(m.contains(KeyMods::CTRL));
        assert!(m.contains(KeyMods::SHIFT));
        assert!(!m.contains(KeyMods::ALT));
        assert!(!m.contains(KeyMods::SUPER));
    }

    #[test]
    fn 空格段也要占列() {
        // 行首 4 个空格 + marker ⇒ 画的时候 x 要往后挪 4 格。
        // ⚠️ 网格里**一个格子一个 grapheme** ⇒ marker 是 6 个格子。
        let mut run = Run::default();
        for _ in 0..4 {
            run.push("", 1);
        }
        for c in "marker".chars() {
            run.push(&c.to_string(), 1);
        }
        assert_eq!(run.cols, 10);
        assert_eq!(run.text(), "    marker");
        assert_eq!(run.segs.len(), 2);
        assert_eq!(run.segs[0], (String::new(), 4));
        assert_eq!(run.segs[1], ("marker".to_string(), 6));
    }
}
