//! VTE 后端（Linux）。
//!
//! 这里装的是**从 `src/term.rs` 平移过来的 VTE 特有实现**——
//! 行为与改造前**逐字节一致**，只是搬了家（跨平台版 阶段 C1）。
//!
//! ⚠️ 搬过来时**连注释一起搬**：那些坑（VTE 的 HTML 导出取到的是
//! 会话历史最前面几行、`cursor_position()` 返回 (列, 行)……）
//! 是踩过的，丢了注释就等于丢了知识。

use gtk::gdk;
use gtk::prelude::*;
use vte4 as vte;
use vte4::prelude::*;

/// VTE 的**光标绝对行号**。
///
/// ⚠⚠ 两个坑，踩过就懂：
///   1. `cursor_position()` 返回 **(列, 行)**，顺序反直觉。写反了会把列当行用，
///      症状是"录到的全是空行"。VTE 官方文档原话：
///      *"The row coordinate is absolute."*（行号是绝对的，从终端启动起算）
///   2. `text_range_format(0, 0, rows-1, cols-1)` 取到的**不是屏幕**，
///      而是**会话历史最前面的 rows 行**——那几行早就滚走了，
///      之后再也不会变 ⇒ 采样多少次都是"没变化" ⇒ **什么也录不下来**。
///      （实测：一次性 2000 行输出，老实现录到 **0 行**。）
pub fn cursor_row(term: &vte::Terminal) -> i64 {
    term.cursor_position().1
}

/// 取绝对行号区间 `[start, end]` 的**带颜色**屏幕行（每行一段 ANSI）。
///
/// 行数 = `end - start + 1`。区间前头若超出滚动缓冲保留范围，
/// 那几行取回来是**空串**——那部分是真丢了（`rec_poll` 里在数）。
pub fn ansi_rows(term: &vte::Terminal, start: i64, end: i64) -> Vec<String> {
    if end < start {
        return Vec::new();
    }
    let cols = term.column_count().max(1);
    let (html, _) = term.text_range_format(vte::Format::Html, start, 0, end, cols - 1);
    match html {
        Some(h) => crate::replay::ansi_lines(&h),
        None => Vec::new(),
    }
}

/// 读终端屏幕上的文字（不带样式，给「看见提示符了吗」那种粗判用）。
///
/// 与 `examples/fileops_smoke.rs` 的 `screen_text` 同一个读法
/// （VTE 0.80 还没有 `text()`，只能按区域取）。
pub fn screen_text(term: &vte::Terminal, rows: i64, cols: i64) -> String {
    term.text_range_format(vte::Format::Text, 0, 0, rows.max(1) - 1, cols.max(1) - 1)
        .0
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 读**可见屏幕**的全部文字。
///
/// ⚠️ 返回 `None` = 取不到（不是「屏幕是空的」）——日志与自动应答都靠它。
pub fn visible_text(term: &vte::Terminal) -> Option<String> {
    term.text_format(vte::Format::Text).map(|s| s.to_string())
}

/// 字号与响铃（1.5 会话级覆盖）。两处都要即时生效：开标签时一次，
/// 会话选项对话框改完再一次。
///
/// ⚠️ 字号优先级：**会话/连接里设的 > 全局默认（首选项中心）> 内置 10.5**。
/// 全局兜底放在这里（而不是每个调用点各自拼）——会话选项改完要**立即**
/// 生效，那条路径也得用全局默认值，否则"没设过字号"的会话在改一次响铃后
/// 字号会跳回 10.5。
pub fn set_appearance(term: &vte::Terminal, font_size: Option<f64>, bell: Option<bool>) {
    let font_env = std::env::var("PUTTYPLUS_FONT").unwrap_or_default();
    let font = if font_env.is_empty() {
        let size = font_size
            .or_else(crate::settings::term_font_size)
            .unwrap_or(10.5);
        format!("Monospace {size}")
    } else {
        font_env
    };
    term.set_font(Some(&gtk::pango::FontDescription::from_string(&font)));
    term.set_audible_bell(bell.unwrap_or(true));
}

/// 终端配色（4.1）。与界面主题各选各的：界面浅色不代表终端也要浅色。
pub fn apply_colors(term: &vte::Terminal) {
    apply_palette(term, crate::theme::palette());
}

/// 按指定配色方案刷终端配色。
pub fn apply_palette(term: &vte::Terminal, p: crate::theme::Palette) {
    let (fgs, bgs, hex) = p.colors();
    let pal: Vec<gdk::RGBA> = hex.iter().filter_map(|h| gdk::RGBA::parse(*h).ok()).collect();
    let pal_ref: Vec<&gdk::RGBA> = pal.iter().collect();
    let fg = gdk::RGBA::parse(fgs).unwrap();
    let bg = gdk::RGBA::parse(bgs).unwrap();
    term.set_colors(Some(&fg), Some(&bg), &pal_ref);
}

/// 复制选区到剪贴板（纯文本）。
pub fn copy_selection(term: &vte::Terminal) {
    term.copy_clipboard_format(vte::Format::Text);
}

/// 从剪贴板粘贴。
pub fn paste_clipboard(term: &vte::Terminal) {
    <vte::Terminal as vte::TerminalExt>::paste_clipboard(term);
}

/// 设置查找模式。
///
/// ⚠️ VTE 的查找用的是**正则**（PCRE）⇒ 普通文本要先转义，
/// 否则用户搜 `a.b` 会把 `a` + 任意字符 + `b` 也匹配上。
pub fn search_set_pattern(term: &vte::Terminal, pattern: Option<&str>) {
    let rx = pattern.map(|p| {
        let pat = regex_escape(p);
        vte::Regex::for_match(&pat, 0).ok()
    });
    match rx {
        // 模式是空 ⇒ 取消查找（VTE 收 None）
        None => term.search_set_regex(None, 0),
        Some(None) => term.search_set_regex(None, 0),
        Some(Some(r)) => term.search_set_regex(Some(&r), 0),
    }
}

/// 把普通文本转义成正则源码（只保留字母数字与下划线）。
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if !c.is_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 正则转义只留字母数字下划线() {
        assert_eq!(regex_escape("abc_123"), "abc_123");
        assert_eq!(regex_escape("a.b"), "a\\.b");
        assert_eq!(regex_escape("a b"), "a\\ b");
        assert_eq!(regex_escape("a+b"), "a\\+b");
    }

    #[test]
    fn 转义后点号不再当通配符() {
        // 「a.b」应当只匹配字面的 a.b，不匹配 aXb
        let pat = regex_escape("a.b");
        assert!(pat.contains("\\."));
        assert!(!pat.contains("a.b"));
    }
}
