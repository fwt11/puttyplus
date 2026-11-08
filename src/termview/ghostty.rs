//! ghostty 终端内核封装（跨平台版 阶段 C2）。
//!
//! ## 这一层的职责
//!
//! 把 libghostty-vt 的 API 翻译成**我们自己的说法**，FFI 细节
//! （错误码、句柄生命周期、formatter 的选项组合、搜索的驱动顺序）
//! 一律封在这个文件里。上层（阶段 C3 的自绘渲染、阶段 C4 的会话）
//! 只看见 [`GhosttyTerm`] 这一个类型。
//!
//! ⚠️ **和 VTE 后端的差别**（别拿 VTE 的直觉套过来）：
//!
//! | | VTE（Linux） | ghostty（跨平台） |
//! |---|---|---|
//! | 屏幕内容 | `text_range_format` 按**绝对行号区间**取 | Formatter 只能取**当前视口** |
//! | 光标行号 | `cursor_position().1` = **绝对**行号 | `cursor_y()` = **视口内**行号（0 起） |
//! | 查找 | 内部是正则（要自己转义） | **原生普通文本**（不用转义） |
//!
//! 所以「按绝对行号取行」这件事在 ghostty 侧还没解决（阶段 C3 接
//! 自绘渲染时一并处理：视口 + 回滚缓冲分开取）。
//!
//! ⚠️ `Terminal` 是 `!Send + !Sync`（底层 C 库不是线程安全的）⇒
//! 这个类型只能在**创建它的那个线程**上用（GTK 主线程）。

use ghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use ghostty_vt::key::{Action, Encoder, Key, Mods};
use ghostty_vt::search::Search;
use ghostty_vt::terminal::{Event, Options, Terminal};

pub use ghostty_vt::key::{Action as KeyAction, Key as KeyCode, Mods as KeyMods};

/// ghostty 终端内核的句柄。
pub struct GhosttyTerm {
    term: Terminal,
    search: Search,
    encoder: Encoder,
}

impl GhosttyTerm {
    /// 新建一个终端内核（`cols` 列 × `rows` 行）。
    ///
    /// ⚠️ 这**只建内核**，不画界面——界面是阶段 C3 的事。
    pub fn new(cols: u16, rows: u16) -> Result<Self, ghostty_vt::Error> {
        let term = Terminal::new(Options {
            cols: cols.max(1),
            rows: rows.max(1),
            ..Options::default()
        })?;
        let search = Search::new(&term)?;
        let encoder = Encoder::new()?;
        Ok(GhosttyTerm {
            term,
            search,
            encoder,
        })
    }

    /* ---------------- 输出 ---------------- */

    /// 把**远端来的字节**喂给终端（VT 流解析）。
    ///
    /// ⚠️ 这个函数**不会失败**：输入是不可信数据，解析错误由内核自己
    /// 记日志，不往上传。
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// 取走内核排队的事件（响铃、标题变化、目录变化、PTY 回写……）。
    ///
    /// ⚠️ **每次 feed 之后都要取**，否则队列会一直涨；里面的
    /// `PtyWrite` 是**要写回给子进程**的字节（查询应答之类）。
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.term.events().collect()
    }

    /* ---------------- 尺寸 ---------------- */

    /// 视口行数。
    pub fn row_count(&self) -> i64 {
        self.term.rows().unwrap_or(0) as i64
    }

    /// 视口列数。
    pub fn column_count(&self) -> i64 {
        self.term.cols().unwrap_or(0) as i64
    }

    /// 改终端尺寸。
    ///
    /// ⚠️ 像素尺寸传 0（内核不关心，只按行列算布局）。
    pub fn set_size(&mut self, cols: u16, rows: u16) -> Result<(), ghostty_vt::Error> {
        self.term.resize(cols.max(1), rows.max(1), 0, 0)
    }

    /// 设置「DECSCUSR 0 重置后」的默认光标**是否闪烁**。
    ///
    /// 内核内置默认是**不闪** ⇒ 普通 shell 的光标是死的；我们的观感要跟
    /// VTE / PuTTY 对齐（默认闪）⇒ 建视图时设 `Some(true)`。远端程序
    /// 仍可用 DECSCUSR 随时改（2/4/6 稳态、1/3/5 闪），内核会把结果报在
    /// `Cursor.blinking` 里，渲染层照办。
    pub fn set_default_cursor_blink(&mut self, v: Option<bool>) {
        let _ = self.term.set_default_cursor_blink(v);
    }

    /* ---------------- 光标 ---------------- */

    /// 光标在**视口内**的行号（0 起）。
    ///
    /// ⚠️ **不是绝对行号**（VTE 的 `cursor_position().1` 才是）——
    /// 视口滚下去之后这里就变了。要绝对行号得自己拿
    /// [`GhosttyTerm::scrollback_rows`] 换算。
    pub fn cursor_row(&self) -> i64 {
        self.term.cursor_y().unwrap_or(0) as i64
    }

    /// 光标的列号（0 起）。
    pub fn cursor_col(&self) -> i64 {
        self.term.cursor_x().unwrap_or(0) as i64
    }

    /// 回滚缓冲里有多少行（历史行数）。
    pub fn scrollback_rows(&self) -> i64 {
        self.term.scrollback_rows().unwrap_or(0) as i64
    }

    /// 屏幕 + 回滚缓冲一共多少行。
    pub fn total_rows(&self) -> i64 {
        self.term.total_rows().unwrap_or(0) as i64
    }

    /* ---------------- 屏幕内容 ---------------- */

    /// **视口**的纯文本（不带样式）。
    ///
    /// ⚠️ 两处裁剪：行**尾**空白会被去掉，**末尾的空行**也会被裁掉
    /// （Formatter 的行为）⇒ 返回的行数可能**少于**视口高度。
    pub fn screen_text(&self) -> String {
        self.format(Format::Plain)
    }

    /// **视口**的带样式文本（VT 序列）。
    pub fn screen_ansi(&self) -> String {
        self.format(Format::Vt)
    }

    /// 视口的带样式文本**按行切开**（每行一段 ANSI）。
    ///
    /// ⚠️ 这就是 [`crate::termview::TermView::ansi_rows`] 想要的东西，
    /// 但 ghostty 侧**只能给视口**（给不了绝对行号区间）——
    /// 阶段 C3 之前先用它验证内核。
    ///
    /// ⚠️ **行数补齐到视口高度**：Formatter 会把**末尾的空行裁掉**
    /// （实测 24 行的视口只给 3 行）⇒ 不补齐的话，**行号会与屏幕
    /// 错位**（第 2 行明明有内容，返回的却是第 0 行）。补齐用空串，
    /// 下标才对得上屏幕行。
    pub fn ansi_rows(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .screen_ansi()
            .split('\n')
            .map(|s| s.trim_end_matches('\r').to_string())
            .collect();
        let height = self.row_count().max(0) as usize;
        rows.truncate(height);
        while rows.len() < height {
            rows.push(String::new());
        }
        rows
    }

    /// 用 Formatter 取一段文本。
    fn format(&self, fmt: Format) -> String {
        let opts = FormatterOptions::new().with_format(fmt).with_trim(true);
        let Ok(mut f) = Formatter::new(&self.term, opts) else {
            return String::new();
        };
        let Ok(bytes) = f.format_alloc() else {
            return String::new();
        };
        String::from_utf8_lossy(&bytes.to_vec()).into_owned()
    }

    /* ---------------- 标题 / 目录 ---------------- */

    /// 远程标题（OSC 0/2）。
    pub fn title(&self) -> Option<String> {
        self.term.title().ok().filter(|s| !s.is_empty()).map(|s| s.to_string())
    }

    /// 工作目录（OSC 7，`file://` URI）。
    pub fn pwd(&self) -> Option<String> {
        self.term.pwd().ok().filter(|s| !s.is_empty()).map(|s| s.to_string())
    }

    /* ---------------- 查找 ---------------- */

    /// 设置查找模式（**普通文本**，不是正则）。
    ///
    /// ⚠️ ghostty 原生就是普通文本 ⇒ **不用转义**（VTE 那边要转，
    /// 那是 VTE 的锅，不是用户输入的问题）。
    pub fn search_set_pattern(&mut self, text: Option<&str>) {
        let needle = text.filter(|t| !t.is_empty());
        let _ = self.search.set_needle(&self.term, needle);
        // 改 needle 之后要把搜索**跑起来**才知道有没有命中。
        let _ = self.search.run(&self.term);
    }

    /// 找下一个（找到返回 true）。
    pub fn search_find_next(&mut self) -> bool {
        let _ = self.search.run(&self.term);
        self.search.select_next(&self.term).unwrap_or(false)
    }

    /// 找上一个（找到返回 true）。
    pub fn search_find_previous(&mut self) -> bool {
        let _ = self.search.run(&self.term);
        self.search.select_prev(&self.term).unwrap_or(false)
    }

    /// 当前查找到的匹配数。
    pub fn search_match_count(&self) -> usize {
        self.search.total_matches().unwrap_or(0)
    }

    /* ---------------- 复位 ---------------- */

    /// 全复位（RIS）：模式、回滚缓冲、滚动区、屏幕内容全回到初始状态。
    pub fn reset(&mut self) {
        self.term.reset();
    }

    /* ---------------- 渲染 ---------------- */

    /// 把内核的当前画面交给渲染状态（增量刷新机制）。
    ///
    /// ⚠️ 返回的快照**借用渲染状态** ⇒ 渲染状态在那期间不能动
    /// （`Snapshot` 借用的是 `rs`，不是 `self`）。
    pub fn update_render<'s>(
        &mut self,
        rs: &'s mut ghostty_vt::render::RenderState,
    ) -> Result<ghostty_vt::render::Snapshot<'s>, ghostty_vt::Error> {
        rs.update(&self.term)
    }

    /* ---------------- 按键编码 ---------------- */

    /// 按键事件 → **要写给子进程的字节**。
    ///
    /// ⚠️ `text` 是**这个键产生的文字**（键盘布局给出的字符；
    /// 中文输入法的 commit 也走这里）。传 `None` 时编码器只按
    /// 逻辑键 + 修饰键推。
    ///
    /// ⚠️ 中文 commit 时**不要**传 C0 控制字符（U+0000–U+001F、
    /// U+007F）——那种情况传 `None`，让编码器按逻辑键来。
    pub fn encode_key(
        &mut self,
        action: Action,
        key: Key,
        mods: Mods,
        text: Option<&str>,
    ) -> Vec<u8> {
        let Ok(mut ev) = ghostty_vt::key::Event::new() else {
            return Vec::new();
        };
        ev.set_action(action);
        ev.set_key(key);
        ev.set_mods(mods);
        if let Some(t) = text {
            ev.set_utf8(Some(t.to_string()));
        }
        // 终端的模式（应用光标键、kitty 键盘协议……）会变 ⇒
        // 每次编码前重新从终端同步一次。
        self.encoder.set_options_from_terminal(&self.term);
        let mut out = Vec::new();
        if self.encoder.encode_to_vec(&ev, &mut out).is_err() {
            return Vec::new();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 新建出来的终端是空的() {
        let t = GhosttyTerm::new(80, 24).expect("建终端");
        assert_eq!(t.row_count(), 24);
        assert_eq!(t.column_count(), 80);
        assert_eq!(t.cursor_row(), 0);
        assert_eq!(t.cursor_col(), 0);
    }

    #[test]
    fn 喂进去的文本落在屏幕上() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.feed(b"hello");
        let text = t.screen_text();
        assert!(
            text.starts_with("hello"),
            "屏幕上应该有 hello，实际是：{text:?}"
        );
        assert_eq!(t.cursor_col(), 5);
    }

    #[test]
    fn 换行会把光标推到下一行() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.feed(b"abc\r\ndef");
        assert_eq!(t.cursor_row(), 1);
        assert_eq!(t.cursor_col(), 3);
        let text = t.screen_text();
        assert!(text.contains("abc"), "实际：{text:?}");
        assert!(text.contains("def"), "实际：{text:?}");
    }

    /* ---------------- 按键编码 ---------------- */

    #[test]
    fn 回车键编成回车() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let b = t.encode_key(Action::Press, Key::Enter, Mods::empty(), None);
        assert_eq!(b, b"\r");
    }

    #[test]
    fn 控制键加字母编成控制码() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let b = t.encode_key(Action::Press, Key::C, Mods::CTRL, None);
        assert_eq!(b, b"\x03");
    }

    #[test]
    fn 普通字母编成自己() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let b = t.encode_key(Action::Press, Key::A, Mods::empty(), Some("a"));
        assert_eq!(b, b"a");
    }

    #[test]
    fn 方向键编成CSI序列() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let b = t.encode_key(Action::Press, Key::ArrowUp, Mods::empty(), None);
        assert_eq!(b, b"\x1b[A");
    }

    #[test]
    fn 中文commit编成UTF8字节() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let b = t.encode_key(Action::Press, Key::Unidentified, Mods::empty(), Some("中"));
        assert_eq!(b, "中".as_bytes());
    }
}
