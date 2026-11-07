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
use ghostty_vt::search::Search;
use ghostty_vt::terminal::{Event, Options, Terminal};

/// ghostty 终端内核的句柄。
pub struct GhosttyTerm {
    term: Terminal,
    search: Search,
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
        Ok(GhosttyTerm { term, search })
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
}
