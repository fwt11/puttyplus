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
use ghostty_vt::style::{Palette as GtPalette, RgbColor};
use ghostty_vt::terminal::{Event, Options, Terminal};

pub use ghostty_vt::key::{Action as KeyAction, Key as KeyCode, Mods as KeyMods};

/// `#rrggbb` ⇒ RGB（解析不了就给 `None`）。
///
/// ⚠️ **自己写**（不借 GTK 的 `gdk::RGBA`）：这一层是**纯内核封装**，
/// 不该拖上 GTK 依赖（Linux 上没有 GTK 时也要能用）。
fn parse_hex_rgb(hex: &str) -> Option<RgbColor> {
    let h = hex.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(RgbColor { r, g, b })
}

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

    /* ---------------- 回滚缓冲 ---------------- */

    /// 设置回滚缓冲的**行数上限**（与 VTE 后端的 `set_scrollback_lines`
    /// 语义对齐）。
    ///
    /// ## 为什么不能只设行数
    ///
    /// 内核**同时有字节上限与行数上限**，**先到先算** ⇒ 只设行数的话，
    /// 内核默认的字节上限（10MB）可能先到 ⇒ 用户设的 10000 行形同虚设
    /// （几百 KB 的输出就把历史挤掉了）。
    ///
    /// ⇒ 这里把**字节上限按行数放大**（每行 4096 字节是很宽松的上界：
    /// 80 列 × 宽字符 + UTF-8 也到不了），让**行数**成为真正管事的那个
    /// 限制；字节上限仍留 10MB 地板（与内核默认一致）。
    ///
    /// ## 行数 0 = 不要回滚缓冲
    ///
    /// ⚠️ 行数上限**关不掉**回滚缓冲（内核的最小值是「一页的行数」）⇒
    /// 真要关，得把**字节上限设成 0**（内核据此关掉回滚缓冲并清空历史）。
    pub fn set_scrollback_lines(&mut self, n: i64) {
        let lines = n.max(0) as usize;
        if lines == 0 {
            let _ = self.term.set_scrollback_max_bytes(Some(0));
            let _ = self.term.set_scrollback_max_lines(Some(0));
            return;
        }
        let bytes = lines.saturating_mul(4096).max(10 * 1024 * 1024);
        let _ = self.term.set_scrollback_max_bytes(Some(bytes));
        let _ = self.term.set_scrollback_max_lines(Some(lines));
    }

    /* ---------------- 光标 ---------------- */

    /// 光标在**活动区**里的行号（0 起）。
    ///
    /// ⚠️ **不是绝对行号**（VTE 的 `cursor_position().1` 才是）——
    /// 它是「活动区（屏幕）」内的行号，要绝对行号得加上回滚缓冲的行数
    /// （见 [`GhosttyTerm::cursor_row`]）。
    ///
    /// ⚠️ 视口滚上去**不会**改它（光标一直在活动区里）——这一点
    /// 2026-11-14 用探针实测过（滚 3 行后仍是 23）。
    pub fn cursor_row_in_screen(&self) -> i64 {
        self.term.cursor_y().unwrap_or(0) as i64
    }

    /// 光标的**绝对行号**（与 VTE 后端的 `cursor_position().1` 语义对齐）。
    ///
    /// ## 怎么算的
    ///
    /// 绝对行号 = **回滚缓冲的行数** + 光标在活动区里的行号。
    ///
    /// ⚠️ 「回滚缓冲的行数」= `total_rows - 视口行数` ⇒ 这个行号是从
    /// **最早还留着的那行**起算的：内核裁掉历史之后，行号会**整体前移、
    /// 甚至倒退**（实测：scrollback=10 灌 2000 行 ⇒ cursor_row=266；
    /// 再灌 500 行 ⇒ **188**——内核按页裁剪，行号随之重新编号）。
    /// VTE 那边是「从终端启动起算」，裁掉也不回头（同场景实测 row=2500）。
    ///
    /// ⇒ 没越过回滚上限时两边语义一致（录制没问题）；**越过后录制会
    /// 丢行**——内核不提供裁剪事件 ⇒ 包装层没法补一个稳定的绝对行号
    /// （拿行内容对齐是启发式，不能当实现），等上游支持了再补。
    ///
    /// ## 为什么不用内核的坐标换算
    ///
    /// 内核也能换算（`grid_ref(Active)` → `point_from_grid_ref(Screen)`），
    /// 实测两者**完全一致**（探针：都是 30）⇒ 用算术这条更直白的路，
    /// 省掉一次页表遍历。
    pub fn cursor_row(&self) -> i64 {
        let history = self.term.scrollback_rows().unwrap_or(0);
        history as i64 + self.cursor_row_in_screen()
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
    /// ⚠️ **内核的 Formatter 给的是「整个缓冲区」**（回滚缓冲 + 屏幕），
    /// **从最顶上那行开始**——不是「当前视口」！
    /// （2026-11-14 实测：灌 30 行进 24 行的屏幕，Formatter 给的是
    /// 第 0 行 = "line 0"（在回滚缓冲里），共 30 行；滚视口也不变。）
    ///
    /// ⇒ 下标就是**缓冲区行号**（= 本程序说的绝对行号，基准是
    /// **最早还留着的那行**）。
    ///
    /// ⚠️ **行数补齐到缓冲区总行数**（不是视口高度）：Formatter 会把
    /// **末尾的空行裁掉**（实测 31 行的缓冲区只给 30 行）⇒ 不补齐的
    /// 话，**行号会与屏幕错位**。
    ///
    /// ⚠️ 以前这里补齐到的是**视口高度** ⇒ 把**最新的那几行砍掉了**
    /// （录出来的是最老的几行，与屏幕对不上——真 bug）。
    pub fn ansi_rows(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .screen_ansi()
            .split('\n')
            .map(|s| s.trim_end_matches('\r').to_string())
            .collect();
        let total = self.total_rows().max(0) as usize;
        rows.truncate(total);
        while rows.len() < total {
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

    /* ---------------- 配色 ---------------- */

    /// 按配色方案刷终端配色（前景 / 背景 / 16 色 ANSI）。
    ///
    /// ⚠️ 用的是内核的 **`default_*`** 接口 ⇒ **OSC 改色仍然压得过它**
    /// （OSC 10/11 改前景背景、OSC 4 改调色板 ⇒ 那是「有效色」，
    /// 这里设的是「默认色」）。两个是**分开**的，不是二选一。
    ///
    /// ⚠️ 256 色表只改**前 16 格**（ANSI 色），16~255 留着内核自己那张
    /// （xterm 的 6×6×6 色立方 + 灰阶）⇒ 与 VTE 后端的行为一致。
    pub fn apply_palette(&mut self, p: crate::theme::Palette) {
        let (fgs, bgs, hex) = p.colors();
        if let Some(c) = parse_hex_rgb(fgs) {
            let _ = self.term.set_default_fg_color(Some(c));
        }
        if let Some(c) = parse_hex_rgb(bgs) {
            let _ = self.term.set_default_bg_color(Some(c));
        }
        // 拿当前**默认**调色板当底（第一次调用时是内核自带的），
        // 把前 16 格换成我们的 ANSI 色。
        let mut pal = self
            .term
            .default_color_palette()
            .unwrap_or_else(|_| GtPalette::default());
        for (i, h) in hex.iter().enumerate() {
            if let Some(c) = parse_hex_rgb(h) {
                pal.0[i] = c;
            }
        }
        let _ = self.term.set_default_color_palette(Some(pal));
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

    /* ---------------- 回滚缓冲 ---------------- */

    #[test]
    fn 回滚缓冲按行数限() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.set_scrollback_lines(200);
        let mut buf = String::new();
        for i in 0..5000 {
            buf.push_str(&format!("line {i}\r\n"));
        }
        t.feed(buf.as_bytes());
        let sb = t.scrollback_rows();
        println!("限 200 行后，回滚缓冲实际留了 {sb} 行");
        // ⚠️ 内核按**页**裁剪（不是精确到行）⇒ 实际留的会比上限多一截
        // （实测 200 ⇒ 353）⇒ 断言给个带宽，别写死。
        assert!(
            (200..=600).contains(&sb),
            "限了 200 行却留着 {sb} 行 ⇒ 行数上限没生效"
        );
    }

    #[test]
    fn 行数上限才是管事的那个限制() {
        // ⚠️ 内核**两个限制先到先算** ⇒ 只设行数不够：字节上限（内核默认
        // 10MB）可能先到 ⇒ 用户设的行数形同虚设。
        // ⇒ 断言「字节上限被放宽到 ≥ 行数 × 每行 4096 字节」。
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.set_scrollback_lines(100_000);
        let bytes = t
            .term
            .scrollback_max_bytes()
            .expect("读字节上限")
            .expect("字节上限应是 Some");
        let lines = t
            .term
            .scrollback_max_lines()
            .expect("读行数上限")
            .expect("行数上限应是 Some");
        println!("字节上限 {bytes}、行数上限 {lines}");
        assert_eq!(lines, 100_000, "行数上限没设上");
        assert!(
            bytes >= 100_000usize * 4096,
            "字节上限 {bytes} 太小 ⇒ 会先于行数上限触发 ⇒ 行数上限形同虚设"
        );
    }

    #[test]
    fn 行数零就是不要回滚缓冲() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.set_scrollback_lines(0);
        let mut buf = String::new();
        for i in 0..500 {
            buf.push_str(&format!("line {i}\r\n"));
        }
        t.feed(buf.as_bytes());
        let sb = t.scrollback_rows();
        println!("设 0 行后，滚回缓冲留了 {sb} 行");
        assert_eq!(sb, 0, "设 0 行还留着 {sb} 行历史");
    }

    /* ---------------- 配色 ---------------- */

    #[test]
    fn formatter给的是整个缓冲区不是视口() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let mut buf = String::new();
        for i in 0..30 {
            buf.push_str(&format!("line {i}\r\n"));
        }
        t.feed(buf.as_bytes());
        let rows = t.ansi_rows();
        // ⚠️ 内核的 Formatter 给的是**整个缓冲区**（回滚缓冲 + 屏幕），
        //    从最顶上那行开始 ⇒ 补齐后的行数 = 缓冲区总行数（31），
        //    **不是**视口高度（24）。
        assert_eq!(
            rows.len(),
            31,
            "补齐后应覆盖整个缓冲区（31 行），实际 {} 行",
            rows.len()
        );
        assert!(rows[0].contains("line 0"), "第 0 行应是历史里那行：{:?}", rows[0]);
        assert!(rows[7].contains("line 7"), "第 7 行不对：{:?}", rows[7]);
        assert!(
            rows[29].contains("line 29"),
            "第 29 行（最新的有内容那行）不对：{:?}",
            rows[29]
        );
        assert_eq!(rows[30], "", "最后一行（光标那行）是空的");
        // 滚视口不影响（Formatter 不看视口）
        t.term
            .scroll_viewport(ghostty_vt::terminal::ScrollViewport::Delta(-3));
        let rows2 = t.ansi_rows();
        assert_eq!(rows2[0], rows[0], "滚视口不该改变 Formatter 的输出");
    }

    #[test]
    fn 光标行号是绝对的不是视口内的() {
        use ghostty_vt::terminal::{Point, PointCoordinate, PointSpace};
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        let mut buf = String::new();
        for i in 0..30 {
            buf.push_str(&format!("line {i}\r\n"));
        }
        t.feed(buf.as_bytes());
        // 30 行灌进 24 行的屏幕 ⇒ 7 行进了回滚缓冲
        assert_eq!(t.scrollback_rows(), 7, "回滚缓冲行数不对");
        // 绝对行号 = 回滚缓冲行数 + 光标在活动区里的行号
        assert_eq!(t.cursor_row(), 30, "光标绝对行号不对");
        // 与内核自己的坐标换算**完全一致**（Active → Screen）
        let gr = t
            .term
            .grid_ref(Point::Active(PointCoordinate {
                x: t.term.cursor_x().unwrap(),
                y: t.term.cursor_y().unwrap() as u32,
            }))
            .expect("拿光标那格");
        let scr = t
            .term
            .point_from_grid_ref(&gr, PointSpace::Screen)
            .expect("换算 Screen 坐标")
            .expect("应该有坐标");
        assert_eq!(
            t.cursor_row(),
            scr.y as i64,
            "算出来的绝对行号与内核换算对不上"
        );
        // 视口滚上去也不影响（光标一直在活动区里）
        t.term
            .scroll_viewport(ghostty_vt::terminal::ScrollViewport::Delta(-3));
        assert_eq!(t.cursor_row(), 30, "滚视口不该改光标的绝对行号");
    }

    #[test]
    fn 自定义配色刷到内核里() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.apply_palette(crate::theme::Palette::Solarized);
        // 前景 / 背景（Solarized：#93a1a1 / #002b36）
        let fg = t.term.default_fg_color().expect("读前景").expect("前景应是 Some");
        assert_eq!((fg.r, fg.g, fg.b), (0x93, 0xa1, 0xa1), "前景没刷上");
        let bg = t.term.default_bg_color().expect("读背景").expect("背景应是 Some");
        assert_eq!((bg.r, bg.g, bg.b), (0x00, 0x2b, 0x36), "背景没刷上");
        // 16 色 ANSI 的第 1 格（红）= #dc322f
        let pal = t.term.default_color_palette().expect("读调色板");
        assert_eq!(
            (pal.0[1].r, pal.0[1].g, pal.0[1].b),
            (0xdc, 0x32, 0x2f),
            "ANSI 红没刷上"
        );
        // 16~255 留给内核自己那张（xterm 色立方 + 灰阶）⇒ 不能被清掉
        let pal2 = t.term.default_color_palette().expect("读调色板");
        assert_eq!(pal2.0[200], pal.0[200], "256 色表的后半截被动过了");
    }

    #[test]
    fn osc改色仍然压过自定义配色() {
        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
        t.apply_palette(crate::theme::Palette::Dark);
        // OSC 10 = 改前景色（#ff0000）；OSC 11 = 改背景色
        t.feed(b"\x1b]10;#ff0000\x07\x1b]11;#0000ff\x07");
        let fg = t.term.fg_color().expect("读有效前景").expect("前景应是 Some");
        assert_eq!((fg.r, fg.g, fg.b), (255, 0, 0), "OSC 10 没压过自定义配色");
        let bg = t.term.bg_color().expect("读有效背景").expect("背景应是 Some");
        assert_eq!((bg.r, bg.g, bg.b), (0, 0, 255), "OSC 11 没压过自定义配色");
    }

    #[test]
    fn hex解析认得六位色() {
        assert_eq!(
            parse_hex_rgb("#ffffff"),
            Some(RgbColor { r: 255, g: 255, b: 255 })
        );
        assert_eq!(
            parse_hex_rgb("#1c1f21"),
            Some(RgbColor { r: 0x1c, g: 0x1f, b: 0x21 })
        );
        assert_eq!(parse_hex_rgb("1c1f21"), None, "少了 # 不该认");
        assert_eq!(parse_hex_rgb("#fff"), None, "三位不该认");
        assert_eq!(parse_hex_rgb("#gggggg"), None, "非十六进制不该认");
    }

    /* ---------------- 按键编码 ---------------- */

    #[test]
    fn 回车键编成回车() {        let mut t = GhosttyTerm::new(80, 24).expect("建终端");
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
