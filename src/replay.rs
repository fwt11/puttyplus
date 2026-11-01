//! 会话录制回放（6.1）的格式与引擎部分——asciicast **v3 写出 / v2 与 v3 都读入**。
//!
//! 刻意做成**不依赖 GTK 的纯逻辑**，这样 JSON 转义、HTML→ANSI、asciicast
//! 解析都能在单测里跑（终端实录的 bug 全在这些字符串处理上）。
//! 界面部分在 `src/replayui.rs`，录制状态挂在 `term.rs`。
//!
//! ## 为什么输出侧是"屏幕快照"而不是原始字节
//!
//! VTE 自己读 PTY master，没有导出原始输出的接口（`text-inserted` 信号在
//! GTK4 构建里是"deprecated and never emitted"，无参数；AT-SPI 那条路要
//! 可用性总线，环境依赖太重）。所以输出只能是**定期抓屏幕**——与 3.4 的
//! 日志同一路子。
//!
//! 但比 3.4 多做了一件事：**抓带颜色的屏幕**（`text_range_format(Html)`）
//! 再转成 ANSI 落盘。这样录出来的 `.cast` 颜色是对的，`asciinema play`
//! 也能直接播。代价是每行以"屏幕当时的样子"为准，两次采样之间滚过一屏
//! 仍会漏——这个边界 UI 和文档都写明。
//!
//! vte 生成 HTML 的形状（读 `src/vte.cc` 的 `attributes_to_html` 确认）：
//! `<pre>` 包裹，行间是 `\n`，**`<span>` 不跨行**（源码原话："Newlines are
//! treated specially, so that the `<span>` do not cover multiple lines"），
//! 标签只有 `<b> <i> <u style=…> <font color="#RRGGBB"> <strike> <blink>`
//! 和 `<span style="background-color:#RRGGBB|text-decoration-line:overline">`，
//! 实体用 GLib 的 `g_markup_escape_text`（`&lt; &gt; &amp; &quot; &apos;`）。
//! ——正因为 span 不跨行，转换才能逐行独立做，逐行也就自足、好比对。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/* ==================================================================
 * JSON（asciicast 就是 JSON Lines）
 *
 * 不引 serde：这个文件格式只有"字符串/数字/定长数组"三种形状，
 * 手写扫描器 ~120 行且能单测，比拉一个 serde + serde_json 进来划算
 * （项目定位是零多余依赖）。
 * ================================================================== */

/// JSON 字符串转义（**不含**外层引号）。
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // 其余控制字符（含 ESC = \x1b）必须走 \u00XX：转义记录里全是 ESC 序列
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 带外层引号的 JSON 字符串。
pub fn json_quote(s: &str) -> String {
    let mut s2 = String::with_capacity(s.len() + 2);
    s2.push('"');
    s2.push_str(&json_escape(s));
    s2.push('"');
    s2
}

/// JSON 扫描器：只认 asciicast 用到的三种形状，不做通用 JSON。
struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scan<'a> {
    fn new(s: &'a str) -> Self {
        Scan {
            b: s.as_bytes(),
            i: 0,
        }
    }
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn byte(&mut self, c: u8) -> bool {
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == c {
            self.i += 1;
            true
        } else {
            false
        }
    }
    /// 读一个 JSON 字符串（调用前已吃掉开头的 `"`）。
    fn string(&mut self) -> Option<String> {
        if !self.byte(b'"') {
            return None;
        }
        let mut out = String::new();
        // [plain, self.i) 是一段还没进 out 的原文（无转义时零拷贝拼一次）
        let mut plain = self.i;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'"' => {
                    out.push_str(str_from(&self.b[plain..self.i]));
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    out.push_str(str_from(&self.b[plain..self.i]));
                    self.i += 1;
                    let esc = *self.b.get(self.i)?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            // 代理对：高位 0xD800..=0xDBFF 要跟一个低 surrogate
                            let ch = if (0xd800..=0xdbff).contains(&hi) {
                                if self.byte(b'\\') && self.byte(b'u') {
                                    let lo = self.hex4()?;
                                    if (0xdc00..=0xdfff).contains(&lo) {
                                        let cp = 0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00);
                                        char::from_u32(cp)?
                                    } else {
                                        '\u{fffd}'
                                    }
                                } else {
                                    '\u{fffd}'
                                }
                            } else {
                                char::from_u32(hi).unwrap_or('\u{fffd}')
                            };
                            out.push(ch);
                        }
                        _ => return None,
                    }
                    plain = self.i;
                }
                _ => self.i += 1,
            }
        }
        None
    }
    fn hex4(&mut self) -> Option<u32> {
        let s = str_from(self.b.get(self.i..self.i + 4)?);
        self.i += 4;
        u32::from_str_radix(s, 16).ok()
    }
    fn number(&mut self) -> Option<f64> {
        self.ws();
        let start = self.i;
        while self.i < self.b.len()
            && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
        {
            self.i += 1;
        }
        if start == self.i {
            return None;
        }
        str_from(&self.b[start..self.i]).parse().ok()
    }
}

fn str_from(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("")
}

/// 取 `"key"` 之后的整数（asciicast 头里 width/height/timestamp 用）。
fn json_int(s: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\"");
    let at = s.find(&pat)? + pat.len();
    let rest = s[at..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        if c.is_ascii_digit() || (i == 0 && c == '-') {
            end = i + 1;
        } else {
            break;
        }
    }
    rest[..end].parse().ok()
}

/// 在 JSON 的某个**子对象**里取整数（v3 的尺寸在 `term` 里）。
fn json_int_in(s: &str, section: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{section}\"");
    let at = s.find(&pat)? + pat.len();
    let rest = &s[at..];
    let open = rest.find('{')?;
    let mut depth = 0i32;
    let mut end = rest.len();
    for (i, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    json_int(&rest[open..end], key)
}

/* ==================================================================
 * HTML → ANSI
 * ================================================================== */

/// SGR 全复位。
pub const SGR_RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Attrs {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    overline: bool,
    blink: bool,
    fg: Option<[u8; 3]>,
    bg: Option<[u8; 3]>,
}

/// 该属性集对应的 SGR 参数（不含 ESC/m）。
fn params(a: &Attrs) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if a.bold {
        v.push("1".into());
    }
    if a.italic {
        v.push("3".into());
    }
    // 下划线样式（实/双/波/点/划线）统一降级成单线 4：VTE 的 SGR 解析器
    // 不认冒号子参数，而 ':'(0x3A) 本身是 CSI 的**终止字节**——
    // 发 `\x1b[4:3m` 会让序列在 ':' 处截断，后面的 "3m" 当普通字符打到屏幕上。
    if a.underline {
        v.push("4".into());
    }
    if a.strike {
        v.push("9".into());
    }
    if a.overline {
        v.push("53".into());
    }
    if a.blink {
        v.push("5".into());
    }
    if let Some(c) = a.fg {
        v.push(format!("38;2;{};{};{}", c[0], c[1], c[2]));
    }
    if let Some(c) = a.bg {
        v.push(format!("48;2;{};{};{}", c[0], c[1], c[2]));
    }
    v
}

/// `last` 的每一项在 `want` 里都同值存在 → 可以只追加，不用复位。
fn superset(want: &Attrs, last: &Attrs) -> bool {
    (!last.bold || want.bold)
        && (!last.italic || want.italic)
        && (!last.underline || want.underline)
        && (!last.strike || want.strike)
        && (!last.overline || want.overline)
        && (!last.blink || want.blink)
        && (last.fg.is_none() || want.fg == last.fg)
        && (last.bg.is_none() || want.bg == last.bg)
}

/// 输出"从 `last` 变成 `want`"的最小 SGR。
///
/// 属性只增不减时只追加新增的那几项（vte 的 HTML 里 `</b>` 常在
/// `</font>` 之前，每步都全量复位会把文件撑大好几倍）；有减项才复位重来。
fn push_delta(out: &mut String, last: &Attrs, want: &Attrs) {
    let p = params(want);
    if superset(want, last) {
        let lp = params(last);
        let fresh: Vec<&String> = p.iter().filter(|x| !lp.contains(x)).collect();
        if !fresh.is_empty() {
            out.push_str(&format!(
                "\x1b[{}m",
                fresh
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(";")
            ));
        }
        return;
    }
    out.push_str(SGR_RESET);
    if !p.is_empty() {
        out.push_str(&format!("\x1b[{}m", p.join(";")));
    }
}

/// 取标签属性里 `name:#RRGGBB` 的颜色。
fn attr_color(tag: &str, name: &str) -> Option<[u8; 3]> {
    let at = tag.find(name)?;
    parse_hex(&tag[at + name.len()..])
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let at = s.find('#')? + 1;
    let h = s.get(at..at + 6)?;
    if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8])
}

/// HTML 实体解码（GLib 的 `g_markup_escape_text` 那几种 + 数字实体）。
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e > 0 && *e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                // 不认识的实体原样留着，别吞字符
                out.push_str(&tail[..=end]);
                rest = &tail[end + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 标签 → 属性变更。
fn apply_tag(st: &mut Attrs, tag: &str) {
    let closing = tag.starts_with('/');
    let body = tag.trim_start_matches('/');
    let name = body
        .split([' ', '\t', '\n', '\r'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match name.as_str() {
        "b" => st.bold = !closing,
        "i" => st.italic = !closing,
        "strike" => st.strike = !closing,
        "blink" => st.blink = !closing,
        "u" => st.underline = !closing,
        "font" => {
            // 闭合标签也要清：漏清会让后面的普通文字继承上一个 run 的颜色
            st.fg = if closing {
                None
            } else {
                attr_color(body, "color")
            };
        }
        "span" => {
            let style = body
                .split_once("style=\"")
                .map(|(_, r)| r.split('"').next().unwrap_or(""))
                .unwrap_or("");
            if closing {
                st.bg = None;
                st.overline = false;
            } else if style.contains("background-color") {
                st.bg = attr_color(style, "background-color");
            } else if style.contains("overline") {
                st.overline = true;
            }
        }
        _ => {}
    }
}

/// 单行 HTML（不带换行）→ 一段**自足**的 ANSI 文本。
///
/// 自足 = 行首行尾都复位，这样同一行无论前面是什么样式都能照原样渲染，
/// 逐行比对时也不会因为"继承了上一行的颜色"而把相同的行判成不同。
fn row_to_ansi(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let mut st = Attrs::default();
    // 已发出的属性。每行开头终端必然停在默认态（上一行行尾复位了），
    // 所以初始就是 default。
    let mut last = Attrs::default();
    let mut any_sgr = false;
    let mut rest = src;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push_str(&decode_entities(rest));
            break;
        };
        if lt > 0 {
            out.push_str(&decode_entities(&rest[..lt]));
        }
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            // 不是标签（文本里裸奔的 '<'）：当普通字符
            out.push_str(&decode_entities(after));
            break;
        };
        let before = st;
        apply_tag(&mut st, &after[1..gt]);
        if st != before {
            push_delta(&mut out, &before, &st);
            any_sgr = true;
        }
        rest = &after[gt + 1..];
    }
    // 纯空白且没带样式 = 视觉上就是空行，归一成空串（比对时才认得出"没变"）
    if !any_sgr && out.trim().is_empty() {
        return String::new();
    }
    if any_sgr && st != Attrs::default() {
        out.push_str(SGR_RESET);
    }
    out
}

/// 整屏 HTML → 每行一段 ANSI 文本。
///
/// 行数与屏幕行数一致（空行给空串），这样 `term::new_lines` 的前后缀
/// 对齐能直接复用。
pub fn ansi_lines(html: &str) -> Vec<String> {
    let body = html
        .strip_prefix("<pre>")
        .unwrap_or(html)
        .strip_suffix("</pre>")
        .unwrap_or_else(|| {
            html.strip_prefix("<pre>")
                .unwrap_or(html)
                .trim_end_matches("</pre>")
        });
    body.split('\n').map(row_to_ansi).collect()
}

/* ==================================================================
 * asciicast v3 写出
 * ================================================================== */

/// 首行（JSON 对象）—— **asciicast v3**。
///
/// 规范：`docs.asciinema.org/manual/asciicast/v3/`（2025-09-10 版）。
/// v3 与 v2 **不向后兼容**：头多了 `term` 对象，事件时间从"绝对"改成"**距上一条的间隔**"。
///
///   · `version` / `term.cols` / `term.rows` 是**必填**；
///   · `term.type` / `term.version` / `term.theme` 可选（终端类型与配色）；
///   · `title` 是**顶层**属性；
///   · `env` **只放 SHELL**——规范原话："Official asciinema recorder captures only
///     SHELL variable by default. All implementations of asciicast-compatible
///     terminal recorder should not capture any additional environment variables
///     unless explicitly requested by the user."
pub fn header_line(cols: i64, rows: i64, ts: i64, title: &str) -> String {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let term_type = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
    format!(
        "{{\"version\": 3, \"term\": {{\"cols\": {}, \"rows\": {}, \"type\": {}}}, \"timestamp\": {}, \"title\": {}, \"env\": {{\"SHELL\": {}}}}}\n",
        cols.max(1),
        rows.max(1),
        json_quote(&term_type),
        ts,
        json_quote(title),
        json_quote(&shell)
    )
}

/// 事件行（`[时间, "类型", "数据"]`）。类型：`o` 输出 / `i` 输入 / `r` 改尺寸 / `m` 标记。
pub fn event_line(t: f64, kind: &str, data: &str) -> String {
    format!(
        "[{:.6}, {}, {}]\n",
        if t.is_finite() { t.max(0.0) } else { 0.0 },
        json_quote(kind),
        json_quote(data)
    )
}

/* ==================================================================
 * asciicast v2 / v3 读入
 * ================================================================== */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// 写到终端的输出
    Output,
    /// 当时发给子进程的输入
    Input,
    /// `NNxNN` 尺寸变化
    Resize,
    /// 标题/备注标记
    Marker,
    /// 会话退出（v3）：数据是退出码
    Exit,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub t: f64,
    pub kind: EventKind,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct Cast {
    /// 文件格式版本（2 或 3）
    pub version: i32,
    pub cols: i64,
    pub rows: i64,
    pub events: Vec<Event>,
    /// 最后一条事件的时间（秒）。空录制为 0。
    pub duration: f64,
    /// 解析时跳过的不合法行数（不是错误，只是如实报给用户）。
    pub skipped: usize,
}

impl Cast {
    pub fn duration_text(&self) -> String {
        fmt_time(self.duration)
    }
}

/// 输入字节 → 给人看的键位写法。
///
/// 回放时**不把录到的输入真的发出去**（回放终端没有子进程；对着真会话
/// 重放按键更危险），所以只在状态条上显示"当时敲了什么"。
pub fn input_display(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\r' => out.push('⏎'),
            '\n' => out.push('⏊'),
            '\t' => out.push('⇥'),
            '\u{8}' => out.push('⌫'),
            '\u{1b}' => out.push('⎋'),
            '\u{7f}' => out.push('⌦'),
            ' ' => out.push('␣'),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 秒 → `m:ss.s`。
pub fn fmt_time(sec: f64) -> String {
    if !sec.is_finite() || sec < 0.0 {
        return "0:00.0".into();
    }
    let m = (sec / 60.0).floor() as i64;
    let s = sec - (m as f64) * 60.0;
    if m > 0 {
        format!("{m}:{:04.1}", s)
    } else {
        format!("{s:.1}s")
    }
}

/// 解析一行事件。形状不对/类型不认识 → None（调用方计数后如实告知）。
pub fn parse_event(line: &str) -> Option<Event> {
    let mut sc = Scan::new(line);
    if !sc.byte(b'[') {
        return None;
    }
    let t = sc.number()?;
    if !sc.byte(b',') {
        return None;
    }
    let k = sc.string()?;
    if !sc.byte(b',') {
        return None;
    }
    let d = sc.string()?;
    if !sc.byte(b']') {
        return None;
    }
    let kind = match k.as_str() {
        "o" => EventKind::Output,
        "i" => EventKind::Input,
        "r" => EventKind::Resize,
        "m" => EventKind::Marker,
        "x" => EventKind::Exit,
        _ => return None,
    };
    Some(Event { t, kind, data: d })
}

/// 读一个 `.cast`：**asciicast v2 与 v3** 都读。
///
/// ⚠ v2 与 v3 **不兼容**（规范原话）：
///   · v2：头是 `{version, width, height, …}`，事件时间是**绝对**秒；
///   · v3：头是 `{version, term:{cols, rows, …}, …}`，事件时间是
///     **距上一条的间隔**（interval）⇒ 读的时候要一路累加。
pub fn load(path: &Path) -> Result<Cast, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    let mut lines = text.lines();
    let head = lines.next().ok_or("文件是空的")?;
    let ver = json_int(head, "version").unwrap_or(0) as i32;
    let (cols, rows) = match ver {
        2 => (
            json_int(head, "width").unwrap_or(80).max(1),
            json_int(head, "height").unwrap_or(24).max(1),
        ),
        3 => (
            json_int_in(head, "term", "cols").unwrap_or(80).max(1),
            json_int_in(head, "term", "rows").unwrap_or(24).max(1),
        ),
        1 => {
            return Err(
                "这是 asciicast v1（2015 年的老格式），本程序只读 v2 / v3。\n\
                 可用 asciinema rec --format asciicast-v2 重录一遍。"
                    .to_string(),
            );
        }
        _ => {
            return Err(format!(
                "认不出这个录制文件的格式（version={ver}），只支持 asciicast v2 / v3。"
            ));
        }
    };

    let mut events = Vec::new();
    let mut skipped = 0usize;
    let mut acc = 0.0f64; // v3 的累加时刻
    for l in lines {
        if l.trim().is_empty() {
            continue;
        }
        match parse_event(l) {
            Some(mut e) => {
                if ver == 3 {
                    // v3：时间是间隔 ⇒ 累加成绝对时刻（播放器按绝对时刻放事件）
                    acc += e.t;
                    e.t = acc;
                }
                events.push(e);
            }
            None => skipped += 1,
        }
    }
    let duration = events.last().map(|e| e.t).unwrap_or(0.0);
    Ok(Cast {
        version: ver,
        cols,
        rows,
        events,
        duration,
        skipped,
    })
}

/* ==================================================================
 * 录制
 * ================================================================== */

/// 正在写的 `.cast`。
pub struct Recorder {
    file: std::fs::File,
    path: PathBuf,
    t0: Instant,
    /// 上一条事件的时刻（v3 的时间写的是**间隔**）
    last_t: f64,
    broken: bool,
    events: u64,
}

impl Recorder {
    /// 新建/追加一个录制文件并写好首行。
    pub fn start(path: &Path, cols: i64, rows: i64, title: &str) -> std::io::Result<Recorder> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        file.write_all(header_line(cols, rows, ts, title).as_bytes())?;
        let r = Recorder {
            file,
            path: path.to_path_buf(),
            t0: Instant::now(),
            last_t: 0.0,
            broken: false,
            events: 0,
        };
        Ok(r)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 写盘已经失败（磁盘满/拔盘）。上层据此提示，别让用户以为还在录。
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    pub fn event_count(&self) -> u64 {
        self.events
    }

    pub fn elapsed(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn put(&mut self, kind: &str, data: &str) {
        if self.broken || data.is_empty() {
            return;
        }
        // v3：事件时间 = **距上一条的间隔**（规范：interval from the previous event）
        let now = self.elapsed();
        let delta = (now - self.last_t).max(0.0);
        self.last_t = now;
        let line = event_line(delta, kind, data);
        // 每条都 flush：进程被 kill 也要留下已发生的部分（与 3.4 一致）
        if self
            .file
            .write_all(line.as_bytes())
            .and_then(|_| self.file.flush())
            .is_err()
        {
            self.broken = true;
        } else {
            self.events += 1;
        }
    }

    /// 屏幕新增的行（每行自带 `\r\n`）。
    pub fn output(&mut self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let mut s = String::new();
        for l in lines {
            s.push_str(l);
            s.push_str("\r\n");
        }
        self.put("o", &s);
    }

    /// **原样**写一条输出字节（**不补换行**）——录"重写光标行"时用。
    ///
    /// 重写一行得写成 `\r` + 清行 + 行文本，和真终端一个路数；
    /// `output()` 会给每行补 `\r\n`，重写时不能那样（那是"追加"）。
    pub fn output_raw(&mut self, data: &str) {
        self.put("o", data);
    }

    /// 已经编码好的输入字节（IME/死键在更早一步就合成完了）。
    pub fn input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.put("i", &String::from_utf8_lossy(bytes));
    }

    pub fn resize(&mut self, cols: i64, rows: i64) {
        self.put("r", &format!("{}x{}", cols.max(1), rows.max(1)));
    }

    pub fn marker(&mut self, text: &str) {
        self.put("m", text);
    }

    /// 会话退出（v3 的 `x` 事件）：数据是退出码。
    ///
    /// 规范说它"通常只出现一次，作为录制的最后一条事件"。
    pub fn exit(&mut self, code: i64) {
        self.put("x", &code.to_string());
    }

    pub fn stop(mut self) {
        let _ = self.file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    /// 去掉 SGR 序列，只看屏幕上的字（断言文字内容用）。
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' && it.peek() == Some(&'[') {
                it.next();
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /* ---- JSON ---- */

    #[test]
    fn json_转义引号与反斜杠() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_quote("x"), "\"x\"");
    }

    #[test]
    fn json_控制字符走_u00xx() {
        // ESC 必须转成 \u001b，否则 JSON 非法
        assert_eq!(json_escape("\x1b[0m"), "\\u001b[0m");
        assert_eq!(json_escape("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
    }

    #[test]
    fn json_中文原样保留() {
        assert_eq!(json_quote("会话-记录"), "\"会话-记录\"");
    }

    #[test]
    fn scan_能读回转义串() {
        let src = json_quote("a\"b\\c\nd\te\u{1}f");
        let mut sc = Scan::new(&src);
        assert_eq!(sc.string().as_deref(), Some("a\"b\\c\nd\te\u{1}f"));
    }

    #[test]
    fn scan_能拼代理对() {
        let mut sc = Scan::new(r#""😀""#);
        assert_eq!(sc.string().as_deref(), Some("😀"));
    }

    #[test]
    fn scan_截断的串返回_none() {
        let mut sc = Scan::new("\"没闭合");
        assert!(sc.string().is_none());
    }

    /* ---- HTML 实体 ---- */

    #[test]
    fn 实体_四种加撇号() {
        assert_eq!(
            decode_entities("&lt;&gt;&amp;&quot;&apos;"),
            "<>&\"'"
        );
        // GLib 某些版本用数字实体，两种都认
        assert_eq!(decode_entities("&#39;"), "'");
    }

    #[test]
    fn 实体_数字与不认识的不动() {
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        // 不认识的实体原样留着，别把字符吞了
        assert_eq!(decode_entities("&nope;"), "&nope;");
        assert_eq!(decode_entities("100% & 1"), "100% & 1");
    }

    /* ---- HTML → ANSI ---- */

    // 这段是从 vte 0.80 真跑出来的（examples 里探针过）
    const REAL: &str = "<pre><font color=\"#C00000\"><b>RED bold</b></font> plain \
<span style=\"background-color:#0000C0\"><font color=\"#FFFFFF\">BG cyan</font></span>\n\
<font color=\"#00C000\">green</font> &lt;&amp;&gt; &quot;q&quot; \
<font color=\"#C0C000\"><u style=\"text-decoration-style:solid\"><b>bold-under</b></u></font>\n\
</pre>";

    #[test]
    fn html_转_ansi_前景色与加粗() {
        let lines = ansi_lines(REAL);
        assert_eq!(
            lines[0],
            "\x1b[38;2;192;0;0m\x1b[1mRED bold\x1b[0m\x1b[38;2;192;0;0m\x1b[0m plain \
             \x1b[48;2;0;0;192m\x1b[38;2;255;255;255mBG cyan\x1b[0m\x1b[48;2;0;0;192m\x1b[0m"
        );
    }

    #[test]
    fn html_转_ansi_只追加不重发() {
        // `</b>` 在 `</font>` 之前：属性只减不增时追加即可，
        // 全量复位会把录制文件撑大好几倍
        let lines = ansi_lines("<pre><font color=\"#FF0000\">a<blink>b</blink></font></pre>");
        assert_eq!(lines[0], "\x1b[38;2;255;0;0ma\x1b[5mb\x1b[0m\x1b[38;2;255;0;0m\x1b[0m");
    }

    #[test]
    fn html_转_ansi_关闭标签清掉颜色() {
        // 回归护栏：`</font>` 漏清会让后面的普通文字继承上一个 run 的红色
        let lines = ansi_lines("<pre><font color=\"#FF0000\">red</font> plain</pre>");
        let tail = lines[0].split("red").nth(1).unwrap();
        assert!(tail.starts_with("\x1b[0m plain"), "得到 {tail:?}");
    }

    #[test]
    fn html_转_ansi_行尾复位_行间不串味() {
        let lines = ansi_lines(REAL);
        // 第二行开头没有红色残留：每行自足
        assert!(lines[1].starts_with("\x1b[38;2;0;192;0m"));
        assert!(lines[1].ends_with(SGR_RESET));
    }

    #[test]
    fn html_转_ansi_实体还原() {
        let lines = ansi_lines(REAL);
        assert!(plain(&lines[1]).contains(r#"green <&> "q" "#), "{:?}", plain(&lines[1]));
    }

    #[test]
    fn html_转_ansi_下划线不产生冒号序列() {
        // 回归护栏：':' 是 CSI 终止字节，发 `\x1b[4:3m` 会让流截断
        // （vte 的 SGR 解析器没有冒号子参数支持）
        let lines = ansi_lines(REAL);
        assert!(lines[1].contains("\x1b[4m"));
        for l in &lines {
            for esc in l.split('\x1b').skip(1) {
                let seq: String = esc.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ';').collect();
                assert!(!seq.contains(':'), "SGR 序列里出现了冒号：\\x1b[{seq}m");
            }
        }
    }

    #[test]
    fn html_转_ansi_空行是空串() {
        let lines = ansi_lines("<pre>a\n\n   \nb</pre>");
        assert_eq!(lines, v(&["a", "", "", "b"]));
    }

    #[test]
    fn html_转_ansi_只有背景色的空格行不算空() {
        // 带背景色的空格是**看得见的色块**，不能归成空行
        let lines = ansi_lines("<pre><span style=\"background-color:#FF0000\">   </span>\n</pre>");
        assert_eq!(lines[0], "\x1b[48;2;255;0;0m   \x1b[0m");
    }

    #[test]
    fn html_转_ansi_删除线与上划线() {
        let lines = ansi_lines("<pre><strike>x</strike><span style=\"text-decoration-line:overline\">y</span></pre>");
        assert_eq!(lines[0], "\x1b[9mx\x1b[0m\x1b[53my\x1b[0m");
    }

    #[test]
    fn html_转_ansi_裸小于号不当标签() {
        // 文本里未转义的 '<'：vte 不会这么发，但解析器不能因此崩/吞字
        let lines = ansi_lines("<pre>a < b</pre>");
        assert!(lines[0].contains("a < b"), "得到 {:?}", lines[0]);
    }

    #[test]
    fn html_转_ansi_纯文本行不带转义() {
        // 没有样式的行保持原样：文件更小，比对也更省心
        let lines = ansi_lines("<pre>hello\n<font color=\"#FF0000\">red</font></pre>");
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn html_转_ansi_没有_pre_包裹也能吃() {
        assert_eq!(ansi_lines("plain"), v(&["plain"]));
    }

    /* ---- asciicast 读写 ---- */

    #[test]
    fn 事件_往返() {
        let line = event_line(1.5, "o", "hi\nthere\u{1b}[0m");
        let e = parse_event(line.trim_end()).expect("应能解析");
        assert_eq!(e.t, 1.5);
        assert_eq!(e.kind, EventKind::Output);
        assert_eq!(e.data, "hi\nthere\u{1b}[0m");
    }

    #[test]
    fn 事件_四种类型() {
        for (k, want) in [
            ("o", EventKind::Output),
            ("i", EventKind::Input),
            ("r", EventKind::Resize),
            ("m", EventKind::Marker),
        ] {
            let l = event_line(0.0, k, "x");
            assert_eq!(parse_event(l.trim_end()).unwrap().kind, want);
        }
    }

    #[test]
    fn 事件_时间负数与_nan_被夹住() {
        let l = event_line(f64::NAN, "o", "x");
        assert_eq!(parse_event(l.trim_end()).unwrap().t, 0.0);
        assert_eq!(parse_event(event_line(-3.0, "o", "x").trim_end()).unwrap().t, 0.0);
    }

    #[test]
    fn 事件_坏行返回_none() {
        assert!(parse_event("").is_none());
        assert!(parse_event("{").is_none());
        assert!(parse_event("[1.0]").is_none());
        assert!(parse_event("[1.0,\"z\",\"x\"]").is_none());
        assert!(parse_event("hello world").is_none());
    }

    #[test]
    fn 头_能取尺寸() {
        let h = header_line(120, 40, 1_700_000_000, "web@host:22");
        assert_eq!(json_int(&h, "version"), Some(3));
        // v3 的尺寸在 term 里
        assert_eq!(json_int_in(&h, "term", "cols"), Some(120));
        assert_eq!(json_int_in(&h, "term", "rows"), Some(40));
        assert!(h.contains("\"title\": \"web@host:22\""));
    }

    #[test]
    fn 头_是_v3_规范() {
        // asciicast v3：term{cols,rows,type} + 顶层 title + env 只放 SHELL
        let h = header_line(80, 24, 1_700_000_000, "web@host:22");
        assert!(h.contains("\"version\": 3"), "{h}");
        assert!(h.contains("\"term\": {"), "{h}");
        let env_at = h.find("\"env\"").expect("应有 env 段");
        let env = &h[env_at..];
        assert!(
            !env.contains("TITLE"),
            "env 里不该再塞 TITLE（规范只让放 SHELL）：{env}"
        );
        assert!(env.contains("SHELL"), "env 里应有 SHELL");
        // 顶层 title 必须在 env 之前
        let title_at = h.find("\"title\"").expect("顶层应有 title");
        assert!(title_at < env_at, "title 应是顶层属性，不在 env 里");
    }

    #[test]
    fn v3_增量时间能读回来() {
        let dir = std::env::temp_dir().join("pp-replay-v3-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.cast");
        let _ = std::fs::remove_file(&p);
        // 手写一个 asciinema 3.x 样子的文件：时间是**距上一条的间隔**
        let text = concat!(
            "{\"version\": 3, \"term\": {\"cols\": 80, \"rows\": 24, \"type\": \"xterm-256color\"}, \"timestamp\": 1700000000, \"title\": \"Demo\", \"env\": {\"SHELL\": \"/bin/sh\"}}\n",
            "[0.248, \"o\", \"hello\"]\n",
            "[1.001, \"o\", \" world\"]\n",
            "[0.500, \"x\", \"0\"]\n",
        );
        std::fs::write(&p, text).unwrap();
        let c = load(&p).unwrap();
        assert_eq!(c.version, 3);
        assert_eq!(c.cols, 80);
        assert_eq!(c.rows, 24);
        assert_eq!(c.events.len(), 3);
        // v3 的时间是间隔 ⇒ 读的时候一路累加成绝对时刻
        assert!((c.events[0].t - 0.248).abs() < 1e-6, "{:?}", c.events[0]);
        assert!((c.events[1].t - 1.249).abs() < 1e-6, "{:?}", c.events[1]);
        assert!((c.events[2].t - 1.749).abs() < 1e-6, "{:?}", c.events[2]);
        assert_eq!(c.events[2].kind, EventKind::Exit);
        assert_eq!(c.events[2].data, "0");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn v2_绝对时间照旧能读() {
        let dir = std::env::temp_dir().join("pp-replay-v2-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.cast");
        let _ = std::fs::remove_file(&p);
        let text = concat!(
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"timestamp\": 1700000000}\n",
            "[0.248, \"o\", \"hello\"]\n",
            "[1.001, \"o\", \" world\"]\n",
        );
        std::fs::write(&p, text).unwrap();
        let c = load(&p).unwrap();
        assert_eq!(c.version, 2);
        assert_eq!(c.cols, 80);
        // v2 的时间是绝对值 ⇒ 不累加
        assert!((c.events[1].t - 1.001).abs() < 1e-6, "{:?}", c.events[1]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 头_尺寸下限为一() {
        let h = header_line(0, -5, 0, "");
        // v3：尺寸在 term 里
        assert_eq!(json_int_in(&h, "term", "cols"), Some(1));
        assert_eq!(json_int_in(&h, "term", "rows"), Some(1));
    }

    #[test]
    fn 录制_写完能被自己读回来() {
        let dir = std::env::temp_dir().join("pp_cast_rt");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("rt.cast");
        let _ = std::fs::remove_file(&p);

        let mut r = Recorder::start(&p, 100, 30, "sess").expect("应能建文件");
        r.marker("开始");
        r.output(&v(&["\x1b[32mok\x1b[0m", "第二行"]));
        r.input(b"ls\r");
        r.resize(120, 40);
        assert!(!r.is_broken());
        assert_eq!(r.event_count(), 4);
        r.stop();

        let c = load(&p).expect("应能读回");
        assert_eq!(c.cols, 100);
        assert_eq!(c.rows, 30);
        assert_eq!(c.skipped, 0);
        assert_eq!(c.events.len(), 4);
        assert_eq!(c.events[0].kind, EventKind::Marker);
        assert_eq!(c.events[1].data, "\x1b[32mok\x1b[0m\r\n第二行\r\n");
        assert_eq!(c.events[2].kind, EventKind::Input);
        assert_eq!(c.events[2].data, "ls\r");
        assert_eq!(c.events[3].data, "120x40");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 录制_空输出不写事件() {
        let dir = std::env::temp_dir().join("pp_cast_rt");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty.cast");
        let _ = std::fs::remove_file(&p);
        let mut r = Recorder::start(&p, 80, 24, "s").unwrap();
        r.output(&[]);
        r.input(&[]);
        assert_eq!(r.event_count(), 0);
        r.stop();
        assert_eq!(load(&p).unwrap().events.len(), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 读_v1_给明确提示而不是裸报错() {
        let dir = std::env::temp_dir().join("pp_cast_rt");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("v1.cast");
        std::fs::write(
            &p,
            "{\"version\": 1, \"width\": 80, \"height\": 24, \"stdout\": [[0, \"hi\"]]}\n",
        )
        .unwrap();
        let e = load(&p).unwrap_err();
        assert!(e.contains("v2"), "应说清只支持 v2：{e}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 读_坏行计数后继续() {
        let dir = std::env::temp_dir().join("pp_cast_rt");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("mix.cast");
        std::fs::write(
            &p,
            "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"a\"]\n\
             这行不是 JSON\n\n[0.2, \"o\", \"b\"]\n",
        )
        .unwrap();
        let c = load(&p).unwrap();
        assert_eq!(c.events.len(), 2);
        assert_eq!(c.skipped, 1);
        assert_eq!(c.duration, 0.2);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 时间格式() {
        assert_eq!(fmt_time(0.0), "0.0s");
        assert_eq!(fmt_time(9.3), "9.3s");
        assert_eq!(fmt_time(65.0), "1:05.0");
        assert_eq!(fmt_time(f64::NAN), "0:00.0");
    }

    #[test]
    fn 输入显示_控制键变成符号() {
        assert_eq!(input_display("ls\r"), "ls⏎");
        assert_eq!(input_display("\t\x1b[A"), "⇥⎋[A");
        assert_eq!(input_display("a b"), "a␣b");
        assert_eq!(input_display("\u{1}"), "\\x01");
    }

    #[test]
    fn 输入显示_普通字符原样() {
        // 空格刻意画成 ␣：命令末尾那几个空格在状态条上是看不见的
        assert_eq!(
            input_display("git commit -m \"你好\""),
            "git␣commit␣-m␣\"你好\""
        );
    }
}
