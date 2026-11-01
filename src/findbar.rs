//! 查找/替换条（可复用构件，阶段 84）。
//!
//! 终端早就有一条查找条（`term.rs` 里那个 `gtk::SearchBar`），文件编辑器
//! 又要加「查找 / 替换」（阶段 84）——两处各画一套的话，样式和按键约定
//! 很快就会打架（一个 Enter 下一个、另一个 Shift+Enter 上一个；一个有
//! 匹配计数、另一个没有）。这里把**外观 + 按键约定**抽成一个构件，两边共用。
//!
//! 构件只管「长什么样、按键怎么响应」，**不碰被搜索的内容**：
//! - 终端搜的是 VTE 的回滚缓冲（`vte::Terminal::search_set_regex`）；
//! - 编辑器搜的是 `gtk::TextBuffer`。
//!
//! 搜索算法本身放在这里（`find_matches`），因为它是**纯函数**、跟界面无关，
//! 两边都能用（终端那边暂时还是用 VTE 自带的正则搜索）。
//!
//! ⚠️ **偏移单位是"字符"不是"字节"**：GTK 的 `TextIter` 偏移按字符算，
//! 而 `String` 的下标按字节算——中文文件里两者差着好几倍，直接拿字节
//! 下标去 `iter_at_offset` 会定位到完全错误的位置（见 `find_matches`）。

use std::cell::RefCell;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

/// 查找/替换条。
///
/// 字段全是 `pub`：调用方自己接信号（构件不替你决定"搜什么"）。
pub struct FindBar {
    /// 整块（查找行 + 替换行）。**默认是隐藏的**，显示时机由调用方决定。
    pub root: gtk::Box,
    /// 查找输入框
    pub find_entry: gtk::Entry,
    /// 替换输入框
    pub replace_entry: gtk::Entry,
    /// 替换那一行（只在替换模式下显示）
    pub replace_row: gtk::Box,
    /// 匹配计数（「3/12」「无匹配」）
    pub count_lbl: gtk::Label,
    /// 「上一个」
    pub prev_btn: gtk::Button,
    /// 「下一个」
    pub next_btn: gtk::Button,
    /// 「区分大小写」
    pub case_btn: gtk::ToggleButton,
    /// 「替换」
    pub replace_btn: gtk::Button,
    /// 「全部替换」
    pub replace_all_btn: gtk::Button,
    /// 「关闭」
    pub close_btn: gtk::Button,
}

/// 查找条要哪些零件。
///
/// 终端只搜索回滚缓冲（VTE 自带的正则搜索），没有"替换"、也不报匹配数
/// ⇒ 那些零件不要；编辑器全都要。
#[derive(Debug, Clone, Copy)]
pub struct FindOpts {
    /// 要不要「替换」那一行
    pub with_replace: bool,
    /// 要不要「Aa」区分大小写
    pub with_case: bool,
    /// 要不要匹配计数标签
    pub with_count: bool,
    /// 要不要「✕」关闭按钮
    ///
    /// ⚠️ 终端那边外层是 `gtk::SearchBar`，**自带**关闭按钮 ⇒ 组件里那个
    /// 就别摆（两个 ✕ 挨在一起，用户要点哪个都不知道）。
    pub with_close: bool,
}

impl FindOpts {
    /// 编辑器：全都要。
    pub fn full() -> FindOpts {
        FindOpts {
            with_replace: true,
            with_case: true,
            with_count: true,
            with_close: true,
        }
    }

    /// 终端：只要输入框 + 上一个/下一个（关闭按钮由外层 SearchBar 提供）。
    pub fn minimal() -> FindOpts {
        FindOpts {
            with_replace: false,
            with_case: false,
            with_count: false,
            with_close: false,
        }
    }
}

/// 建一条查找条。
pub fn build(opts: FindOpts) -> FindBar {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);

    // ---- 查找行 ----
    let find_entry = gtk::Entry::new();
    find_entry.set_placeholder_text(Some("查找…"));
    find_entry.set_hexpand(true);

    // 「区分大小写」：用文字 "Aa"（业界通用写法，没有对应的标准图标）
    let case_btn = gtk::ToggleButton::new();
    case_btn.set_label("Aa");
    case_btn.set_tooltip_text(Some("区分大小写"));
    // 默认**区分**大小写：编辑器的主要用途是看代码，大小写不同就是不同的东西
    case_btn.set_active(true);
    case_btn.set_visible(opts.with_case);

    let prev_btn = crate::a11y::icon_button("go-up-symbolic");
    prev_btn.set_tooltip_text(Some("上一个匹配 (Shift+Enter)"));
    let next_btn = crate::a11y::icon_button("go-down-symbolic");
    next_btn.set_tooltip_text(Some("下一个匹配 (Enter)"));
    let close_btn = crate::a11y::icon_button("window-close-symbolic");
    close_btn.set_tooltip_text(Some("关闭查找条 (Esc)"));
    close_btn.set_visible(opts.with_close);

    let count_lbl = gtk::Label::new(Some("—"));
    count_lbl.set_width_chars(8);
    count_lbl.set_xalign(0.5);
    count_lbl.add_css_class("pp-hint");
    count_lbl.set_visible(opts.with_count);

    let find_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    find_row.append(&find_entry);
    find_row.append(&case_btn);
    find_row.append(&prev_btn);
    find_row.append(&next_btn);
    find_row.append(&count_lbl);
    find_row.append(&close_btn);
    root.append(&find_row);

    // ---- 替换行 ----
    // 「替换 / 全部替换」**保留文字**：两个都是替换动作，光看图标分不出
    // 「替换一个」和「替换全部」——这跟弹窗里的表单按钮一个道理。
    let replace_entry = gtk::Entry::new();
    replace_entry.set_placeholder_text(Some("替换为…"));
    replace_entry.set_hexpand(true);

    let replace_btn = gtk::Button::with_label("替换");
    replace_btn.set_tooltip_text(Some("替换当前这一个匹配"));
    let replace_all_btn = gtk::Button::with_label("全部替换");
    replace_all_btn.set_tooltip_text(Some("替换所有匹配"));

    let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    replace_row.append(&replace_entry);
    replace_row.append(&replace_btn);
    replace_row.append(&replace_all_btn);
    replace_row.set_visible(opts.with_replace);
    root.append(&replace_row);

    FindBar {
        root,
        find_entry,
        replace_entry,
        replace_row,
        count_lbl,
        prev_btn,
        next_btn,
        case_btn,
        replace_btn,
        replace_all_btn,
        close_btn,
    }
}

/// 给查找输入框装上**通用按键约定**：
///
/// - Enter：下一个；Shift+Enter：上一个
/// - Esc：关闭（调用方负责把焦点还给内容区）
///
/// 这套约定跟终端查找条一致——两边共用一个构件，就不能让两边的回键行为不一样。
pub fn bind_entry_keys(
    entry: &gtk::Entry,
    on_next: impl FnMut() + 'static,
    on_prev: impl FnMut() + 'static,
    on_close: impl FnMut() + 'static,
) {
    let ek = gtk::EventControllerKey::new();
    // ⚠️ GTK 的按键回调是 `Fn`（不是 `FnMut`），而被调用的动作是 `FnMut`
    // ——用 RefCell 包一层才能既"能改状态"又"能被 Fn 闭包调用"。
    let next = RefCell::new(on_next);
    let prev = RefCell::new(on_prev);
    let close = RefCell::new(on_close);
    ek.connect_key_pressed(move |_, keyval, _code, mods| {
        let name = keyval.name();
        let name = name.as_deref().unwrap_or("");
        if name == "Return" || name == "KP_Enter" {
            if mods.contains(gdk::ModifierType::SHIFT_MASK) {
                (prev.borrow_mut())();
            } else {
                (next.borrow_mut())();
            }
            return glib::Propagation::Stop;
        }
        if name == "Escape" {
            (close.borrow_mut())();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(ek);
}

/// 找出 `text` 里所有 `needle` 的位置。
///
/// 返回的是**字符偏移**区间 `(起点, 终点)`，和 GTK 的 `TextIter` 偏移同一个
/// 单位（`gtk_text_buffer_iter_at_offset` 收的就是字符下标）。
///
/// - 大小写敏感：走 `str::match_indices`（快、不漏 Unicode）；
/// - 大小写不敏感：把小写串和"小写串第 k 个字符对应原文第几个字符"的映射
///   一起建出来。
///
/// ⚠️ **不能**简单地把原文转成小写再找：`'İ'`（U+0130）的小写是
/// `'i̇'`（**两个**字符），转完下标就和原文对不上了。映射表就是为了
/// 把"小写串里的位置"换算回"原文里的位置"。
pub fn find_matches(text: &str, needle: &str, case_sensitive: bool) -> Vec<(i32, i32)> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        let n = needle.chars().count() as i32;
        let mut out = Vec::new();
        // `match_indices` 给的是字节下标，这里边扫边换算成字符下标
        // （换算是**增量**的：只数上一处匹配到这一处之间的字符，
        //  整体 O(n)，不会每匹配一次就从头数一遍）。
        let mut char_base = 0i32;
        let mut byte_base = 0usize;
        for (byte, _) in text.match_indices(needle) {
            char_base += text[byte_base..byte].chars().count() as i32;
            byte_base = byte;
            out.push((char_base, char_base + n));
        }
        return out;
    }

    let (low, map) = lower_with_map(text);
    let pat: String = needle.to_lowercase();
    if pat.is_empty() {
        return Vec::new();
    }
    let pat_chars = pat.chars().count();
    let mut out = Vec::new();
    let mut char_base = 0usize;
    let mut byte_base = 0usize;
    for (byte, _) in low.match_indices(&pat) {
        char_base += low[byte_base..byte].chars().count();
        byte_base = byte;
        let start_lc = char_base;
        let end_lc = start_lc + pat_chars;
        if end_lc > map.len() {
            break;
        }
        // 小写串里的位置 → 原文里的位置
        let orig_start = map[start_lc] as i32;
        let orig_end = map[end_lc - 1] as i32 + 1;
        out.push((orig_start, orig_end));
    }
    out
}

/// 小写化 + "小写串第 k 个字符 ↔ 原文第几个字符"的映射。
fn lower_with_map(text: &str) -> (String, Vec<u32>) {
    let mut low = String::with_capacity(text.len());
    let mut map: Vec<u32> = Vec::with_capacity(text.len());
    for (ci, ch) in text.chars().enumerate() {
        for lc in ch.to_lowercase() {
            low.push(lc);
            map.push(ci as u32);
        }
    }
    (low, map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把字符偏移换算成字符串切片（便于断言）。
    fn slice(text: &str, s: i32, t: i32) -> String {
        let cs: Vec<char> = text.chars().collect();
        cs[s as usize..t as usize].iter().collect()
    }

    #[test]
    fn 空查找词没有匹配() {
        assert!(find_matches("hello", "", true).is_empty());
        assert!(find_matches("hello", "", false).is_empty());
    }

    #[test]
    fn 区分大小写时大小写不同不算匹配() {
        let ms = find_matches("Foo foo FOO", "foo", true);
        assert_eq!(ms.len(), 1);
        assert_eq!(slice("Foo foo FOO", ms[0].0, ms[0].1), "foo");
    }

    #[test]
    fn 不区分大小写时全都算匹配() {
        let t = "Foo foo FOO";
        let ms = find_matches(t, "foo", false);
        assert_eq!(ms.len(), 3);
        assert_eq!(ms[0], (0, 3));
        assert_eq!(ms[1], (4, 7));
        assert_eq!(ms[2], (8, 11));
    }

    #[test]
    fn 偏移是字符不是字节() {
        // "中文abc" 里 "abc" 的字节下标是 6，字符下标是 2
        let t = "中文abc";
        let ms = find_matches(t, "abc", true);
        assert_eq!(ms, vec![(2, 5)]);
        assert_eq!(slice(t, ms[0].0, ms[0].1), "abc");
    }

    #[test]
    fn 中文照样能找() {
        let t = "第一行\n第二行里有中文\n第三行";
        let ms = find_matches(t, "中文", true);
        assert_eq!(ms.len(), 1);
        assert_eq!(slice(t, ms[0].0, ms[0].1), "中文");
        // 字符下标："第一行\n第二行里有" 共 9 个字符（"中文"从第 9 个开始）
        assert_eq!(ms[0].0, 9);
    }

    #[test]
    fn 大小写不敏感时中文与英文混排也对得上() {
        let t = "Abc中文abc";
        let ms = find_matches(t, "abc", false);
        assert_eq!(ms.len(), 2);
        assert_eq!(slice(t, ms[0].0, ms[0].1), "Abc");
        assert_eq!(slice(t, ms[1].0, ms[1].1), "abc");
        // 第二个 "abc" 的字符下标是 5（"Abc中文" 五个字符）
        assert_eq!(ms[1].0, 5);
    }

    #[test]
    fn 土耳其大写I的小写是两字符也认得() {
        // 'İ' 的小写是 'i' + U+0307（两个字符）——直接转小写再找会错位
        let t = "İstanbul";
        let ms = find_matches(t, "İ", false);
        assert_eq!(ms.len(), 1);
        assert_eq!(slice(t, ms[0].0, ms[0].1), "İ");
        assert_eq!(ms[0], (0, 1));
    }

    #[test]
    fn 匹配不重叠() {
        let t = "aaaa";
        let ms = find_matches(t, "aa", true);
        assert_eq!(ms, vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn 没有匹配时是空列表() {
        assert!(find_matches("hello", "zzz", true).is_empty());
        assert!(find_matches("hello", "zzz", false).is_empty());
    }
}
