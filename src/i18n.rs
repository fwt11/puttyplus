//! 界面文案的多语言支持（中文 / English）。
//!
//! **为什么用「中文原文当 key」**：界面文案有三千多条，如果再设计一套
//! `msg_id`（`"btn.new_conn"` 之类），等于给每条文案额外写一遍标识符 ——
//! 改一个字要同时改三处（中文、英文、key），漏一处就是「界面显示成
//! key 名」的经典事故。⇒ **key 就是中文原文本身**：查不到就原样显示中文，
//! 永远不会出现「显示出一串 key」。
//!
//! **查表**：英文表在 [`crate::i18n_en`]（**自动生成**，别手改），
//! 按中文原文排序 ⇒ 二分查找。
//!
//! **用法**：
//!
//! ```ignore
//! let title = tr("新建连接…");                  // 普通文案
//! let msg = trf!("已导入 {n} 个会话。", n = n);  // 带占位符的文案
//! ```
//!
//! ⚠️ **带占位符的文案不能直接用 `format!`**：`format!` 的模板参数
//! **必须是字面量**，而译文是运行时查出来的 ⇒ 用 [`trf!`] 宏（它自己
//! 做占位符替换）。
//!
//! **切换语言要重启**：界面控件是**建一次**的（菜单表、页签、面板都是
//! `&'static str` / 已建好的 widget），语言切换后重建整个窗口代价太大
//! ⇒ 设置里改语言后**提示重启生效**（见 `settingsui` 的「界面语言」）。

use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::i18n_en::EN;

/// 界面语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// 简体中文（默认）。
    Zh,
    /// 英文。
    En,
}

impl Lang {
    /// 存进设置文件里的标识符。
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    /// 从设置文件里的标识符还原（认不出来就回中文）。
    pub fn from_code(s: &str) -> Lang {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "en_us" | "en_gb" => Lang::En,
            _ => Lang::Zh,
        }
    }

    /// 语言**用它自己的语言写**的名字（切换之后仍然认得出哪一项是自己）。
    pub fn self_name(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }

    /// 下拉框里的标识符（`ComboBoxText::append` 的 id）。
    pub fn id(self) -> &'static str {
        self.code()
    }
}

/// 当前语言（进程内全局；启动时从设置里读）。
static LANG: AtomicU8 = AtomicU8::new(0);

/// 设置当前语言。
pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

/// 语言变化时的回调（界面靠它重建）。
struct LangHook {
    /// 回调还活着吗？死了就摘掉（别让列表越积越长）。
    alive: std::cell::Cell<bool>,
    f: std::rc::Rc<dyn Fn()>,
}

thread_local! {
    /// 语言变了要重建界面的回调。
    static LANG_HOOKS: std::cell::RefCell<Vec<LangHook>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 注册"语言变了"的回调（返回的 `alive` 置否即注销）。
pub fn add_lang_hook(f: impl Fn() + 'static) -> std::cell::Cell<bool> {
    let alive = std::cell::Cell::new(true);
    LANG_HOOKS.with(|h| {
        h.borrow_mut().push(LangHook {
            alive: alive.clone(),
            f: std::rc::Rc::new(f),
        })
    });
    alive
}

/// 通知界面：**语言变了**。
///
/// ⚠️ 先把回调**取出来**再调（不在借用里调）：回调里会重建控件。
pub fn notify_lang_changed() {
    let hooks: Vec<std::rc::Rc<dyn Fn()>> = LANG_HOOKS.with(|h| {
        let mut v = h.borrow_mut();
        v.retain(|x| x.alive.get());
        v.iter().map(|x| x.f.clone()).collect()
    });
    for h in hooks {
        h();
    }
}

/// 当前语言。
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Zh,
    }
}

/// 把中文原文翻成当前语言的文案。
///
/// 查不到英文译文时**原样返回中文**（不会显示成空串或 key 名）。
pub fn tr(zh: &'static str) -> &'static str {
    match lang() {
        Lang::Zh => zh,
        Lang::En => lookup(zh).unwrap_or(zh),
    }
}

/// 在英文表里二分查找。
fn lookup(zh: &str) -> Option<&'static str> {
    match EN.binary_search_by(|(k, _)| (*k).cmp(zh)) {
        Ok(i) => Some(EN[i].1),
        Err(_) => None,
    }
}

/// 翻译一个**运行时才有**的文案（不是字面量）。
///
/// 菜单项的文字来自 const 表（`&'static str`）⇒ 本来能用 [`tr`]；
/// 但 `menu::row` 收的是 `&str` ⇒ 用这个。
///
/// 查不到就原样返回（不会显示成空串，也不会显示成 key 名）。
pub fn tr_dyn(zh: &str) -> Cow<'_, str> {
    match lang() {
        Lang::Zh => Cow::Borrowed(zh),
        Lang::En => match lookup(zh) {
            Some(en) => Cow::Borrowed(en),
            None => Cow::Borrowed(zh),
        },
    }
}

/// 这个中文原文**有没有英文译文**（查不到就是没有）。
///
/// ⚠️ 只给**测试**用：const 表里的文案（菜单、快捷键、设置…）不进
///    `tr()`，只能靠"表里有没有它"来保证英文界面不漏译。
pub fn has_translation(zh: &str) -> bool {
    lookup(zh).is_some()
}

/// 一段**运行时生成的错误文本**是不是「认证失败」。
///
/// ⚠️ `panel/mod.rs` 要靠它判断「无凭据且认证失败 ⇒ 弹密码框」，
/// 而那句错误在英文界面里是 `Authentication failed: …` ⇒ 不能靠中文原文认。
pub fn looks_like_auth_failure(s: &str) -> bool {
    match lang() {
        Lang::Zh => s.contains("认证"),
        Lang::En => s.to_ascii_lowercase().contains("authentication"),
    }
}

/// 带占位符的翻译。
///
/// `format!` 的模板必须是**字面量**，译文是运行时查出来的 ⇒ 用这个宏
/// 做占位符替换。
///
/// ```ignore
/// trf!("已导入 {n} 个会话。", n = n)
/// ```
#[macro_export]
macro_rules! trf {
    ($zh:expr $(, $k:ident = $v:expr)* $(,)?) => {{
        let mut __s = $crate::i18n::tr($zh).to_string();
        $(
            __s = __s.replace(&format!("{{{}}}", stringify!($k)), &$v.to_string());
        )*
        __s
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    /* ---------------- 守门：源码里的 tr() 必须都有英文译文 ---------------- */

    /// 源码里所有 `tr()` / `trf!()` 用的**中文原文**（= 查表的 key）。
    ///
    /// ⚠️ **为什么要自己扫源码**：`const` 表里的文案（菜单、快捷键、危险词）
    ///    进不了 `tr()` ⇒ 只能靠"表里有没有它"来守（见 `topmenus` / `shortcuts`
    ///    / `dialogs` 各自的测试）。但 `tr()` 调用点**能**扫出来 ——
    ///    ⇒ 这条守门补的就是「`tr()` 了但没译文」这个洞。
    ///
    /// ⚠️ **这个洞是真踩过的**：阶段 109~118 新增的 docker 功能用了 58 处
    ///    `tr()`，**一条都没进英文表** ⇒ 英文界面上一堆中文（容器详情、
    ///    compose 项目、启停按钮…）⇒ 没人发现（旧的守门只管 const 表）。
    ///
    /// **扫法**：逐字符走一遍（跳过注释 / 原始字符串 / 字符字面量）⇒ 遇到
    ///    标识符 `tr` / `trf` 且后面跟着 `(` ⇒ 取**第一个字符串字面量**当 key。
    ///
    /// ⚠️ **`#[cfg(test)]` 之后的不算**：测试里的 `tr()` 是断言的一部分，
    ///    译文表里不该有它（有反而会让断言挂：断言写中文，`tr()` 却返回英文）。
    fn tr_call_keys() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut files = Vec::new();
        collect_rs(&src_root(), &mut files);
        for f in files {
            // 英文表本身是**生成的产物**，不是文案源（它的每一行都是
            // 「中文原文 + 英文译文」）⇒ 扫它等于自举。
            if f.file_name().map(|n| n == "i18n_en.rs").unwrap_or(false) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            // 测试代码从第一个 `#[cfg(test)]` 开始。
            let head = match src.find("#[cfg(test)]") {
                Some(i) => &src[..i],
                None => src.as_str(),
            };
            for (loc, key) in scan_tr_calls(head) {
                out.push((format!("{}:{loc}", f.display()), key));
            }
        }
        out
    }

    /// 仓库里的 `src/` 目录。
    fn src_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// 递归找出所有 `.rs` 文件。
    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push(p);
            }
        }
    }

    /// 扫出源码里 `tr("…")` / `trf!("…")` 的 key（返回 `(行号, 中文原文)`）。
    fn scan_tr_calls(src: &str) -> Vec<(usize, String)> {
        let b: Vec<char> = src.chars().collect();
        let mut i = 0usize;
        let mut line = 1usize;
        let mut out = Vec::new();
        while i < b.len() {
            let c = b[i];
            if c == '\n' {
                line += 1;
                i += 1;
                continue;
            }
            // 行注释
            if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            // 块注释
            if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
                continue;
            }
            // 字符字面量 `'"'`：里面的双引号不是字符串的开始
            if c == '\'' && i + 2 < b.len() && b[i + 1] == '"' && b[i + 2] == '\'' {
                i += 3;
                continue;
            }
            // 原始字符串 `r"…"` / `r#"…"#`：整段跳过（CSS 块在里面）。
            if c == 'r' {
                let mut j = i + 1;
                let mut hashes = 0usize;
                while j < b.len() && b[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                if j < b.len() && b[j] == '"' {
                    let mut k = j + 1;
                    let mut nl = line;
                    loop {
                        if k >= b.len() {
                            break;
                        }
                        if b[k] == '"' {
                            let mut h = 0usize;
                            while h < hashes && k + 1 + h < b.len() && b[k + 1 + h] == '#' {
                                h += 1;
                            }
                            if h == hashes {
                                k += 1 + hashes;
                                break;
                            }
                        }
                        if b[k] == '\n' {
                            nl += 1;
                        }
                        k += 1;
                    }
                    line = nl;
                    i = k;
                    continue;
                }
            }
            // 标识符：找 `tr` / `trf`
            if c.is_alphabetic() || c == '_' {
                let start = i;
                while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                    i += 1;
                }
                let word: String = b[start..i].iter().collect();
                if word != "tr" && word != "trf" {
                    continue;
                }
                // 后面：`(`（`trf` 允许先来一个 `!`）
                let mut j = i;
                if word == "trf" && j < b.len() && b[j] == '!' {
                    j += 1;
                }
                if j >= b.len() || b[j] != '(' {
                    continue;
                }
                // 跳过空白与注释，取**第一个字符串字面量**
                let (key, nl, nj) = match next_str_literal(&b, j + 1, line) {
                    Some(v) => v,
                    None => continue,
                };
                out.push((line, key));
                line = nl;
                i = nj;
                continue;
            }
            // 普通字符串字面量：跳过（避免里面的内容被当成代码）
            if c == '"' {
                let (_, nl, nj) = next_str_literal(&b, i, line).unwrap_or((String::new(), line, i + 1));
                line = nl;
                i = nj;
                continue;
            }
            i += 1;
        }
        out
    }

    /// 从 `from` 处开始找**下一个字符串字面量**（返回 `(原文, 结束行号, 结束下标)`）。
    fn next_str_literal(b: &[char], mut i: usize, line: usize) -> Option<(String, usize, usize)> {
        let mut line = line;
        // 跳过空白
        while i < b.len() && b[i].is_whitespace() {
            if b[i] == '\n' {
                line += 1;
            }
            i += 1;
        }
        if i >= b.len() || b[i] != '"' {
            return None;
        }
        i += 1;
        let mut raw = String::new();
        while i < b.len() {
            let c = b[i];
            if c == '\\' {
                if i + 1 < b.len() {
                    raw.push(c);
                    raw.push(b[i + 1]);
                    if b[i + 1] == '\n' {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                i += 1;
                return Some((unescape(&raw), line, i));
            }
            if c == '\n' {
                line += 1;
            }
            raw.push(c);
            i += 1;
        }
        None
    }

    /// Rust 字符串字面量反转义（**必须与 `tools/i18n.py` 的 `unescape` 同规则**）。
    ///
    /// ⚠️ **行续接**（`\` + 换行 + 缩进）：源码里为了排版折行用的 ⇒
    ///    **运行时字符串里没有** ⇒ 不折叠 ⇒ key 里会多出一截换行和缩进 ⇒
    ///    查表查不到 ⇒ 守门会误报「没译文」。
    fn unescape(raw: &str) -> String {
        let b: Vec<char> = raw.chars().collect();
        let mut out = String::new();
        let mut i = 0usize;
        while i < b.len() {
            let c = b[i];
            if c != '\\' || i + 1 >= b.len() {
                out.push(c);
                i += 1;
                continue;
            }
            let nxt = b[i + 1];
            // `\u{...}`
            if nxt == 'u' && i + 2 < b.len() && b[i + 2] == '{' {
                if let Some(end) = (i + 3..b.len()).position(|j| b[j] == '}') {
                    let hex: String = b[i + 3..i + 3 + end].iter().collect();
                    if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                            i = i + 3 + end + 1;
                            continue;
                        }
                    }
                }
            }
            // 行续接：反斜杠 + 换行 + 下一行的缩进**整段去掉**
            if nxt == '\n' {
                i += 2;
                while i < b.len() && (b[i] == ' ' || b[i] == '\t') {
                    i += 1;
                }
                continue;
            }
            match nxt {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                _ => out.push(nxt),
            }
            i += 2;
        }
        out
    }

    /// 这段文案里**有没有要翻译的内容**（汉字或全角标点）。
    ///
    /// ⚠️ **规则必须与 `tools/i18n.py` 的 `CJK` 正则一致**（那边扫词条、
    ///    这边守门，两边不一致就会出现「表里没有 ⇒ 守门报红」的假警报）。
    ///
    /// ⚠️ **纯英文/符号的 key 不需要译文**：`tr("—")`、`tr("ID")`、
    ///    `tr("CPU")`、`tr("hex")` 这种，英文界面本来就长这样 ⇒
    ///    进翻译表是多余的（表里只收**中文原文**当 key）。
    fn contains_cjk(s: &str) -> bool {
        s.chars().any(|c| {
            let cp = c as u32;
            (0x4E00..=0x9FFF).contains(&cp)      // 汉字
                || (0x3000..=0x303F).contains(&cp) // 全角标点
                || (0xFF00..=0xFFEF).contains(&cp) // 全角字母 / 标点
        })
    }

    /// ⚠️ **核心守门**：`tr()` 的 key **必须**在英文表里。
    ///
    /// 查不到时 `tr()` 会**原样返回中文** ⇒ 英文界面上那一条就是中文，
    /// 而界面**不会报错** ⇒ 没有这条断言，漏译永远发现不了（阶段 109~118
    /// 的 docker 文案就是这样漏了一百多条）。
    #[test]
    fn 源码里的tr调用都有英文译文() {
        let mut missing: Vec<String> = Vec::new();
        for (loc, key) in tr_call_keys() {
            if !contains_cjk(&key) {
                continue; // 纯英文/符号 ⇒ 不需要译文
            }
            if !has_translation(&key) {
                missing.push(format!("{loc}: {key}"));
            }
        }
        assert!(
            missing.is_empty(),
            "这些 tr() 的文案没有英文译文（英文界面上会显示中文）：\n{}",
            missing.join("\n")
        );
    }

    /// 反向验证用：扫出来的 key 应当**不止一个**（扫不到东西的守门等于没有）。
    #[test]
    fn tr调用的扫描器能扫到东西() {
        let keys = tr_call_keys();
        assert!(
            keys.len() > 500,
            "只扫到 {} 个 tr() key ⇒ 扫描器坏了（守门会变成永远绿）",
            keys.len()
        );
        // 抽查一个已知的 key（阶段 111 的 compose 文案）。
        assert!(
            keys.iter().any(|(_, k)| k == "compose 项目：{n}"),
            "没扫到已知的 tr() key ⇒ 扫描器坏了"
        );
    }

    #[test]
    fn 语言标识符能往返() {
        for l in [Lang::Zh, Lang::En] {
            assert_eq!(Lang::from_code(l.code()), l);
        }
        // 认不出来的 ⇒ 回中文（不许崩）
        assert_eq!(Lang::from_code("fr"), Lang::Zh);
        assert_eq!(Lang::from_code(""), Lang::Zh);
    }

    #[test]
    fn 英文表是排好序的且没有重复() {
        let mut keys: Vec<&str> = EN.iter().map(|(k, _)| *k).collect();
        let sorted = {
            let mut k = keys.clone();
            k.sort_unstable();
            k
        };
        assert_eq!(keys, sorted, "英文表没排序 ⇒ 二分查找会漏");
        keys.dedup();
        assert_eq!(keys.len(), EN.len(), "英文表里有重复的 key");
    }

    #[test]
    fn 英文表里没有空译文也没有漏占位符() {
        for (zh, en) in EN.iter() {
            assert!(!en.trim().is_empty(), "「{zh}」的译文是空的");
            assert_eq!(
                ph(zh),
                ph(en),
                "「{zh}」的占位符与译文「{en}」对不上"
            );
        }
    }

    /// 这条文案里的**占位符**（`trf!` 用的那种 `{名字}`）。
    ///
    /// ⚠️ **`${名字}` 这种不算**：那是文档里给用户看的 **shell 示例**
    /// （`${名字:默认值}` 是 shell 的默认值语法），不是 `trf!` 的占位符 ——
    /// 英文译文里它要变成 `${name:default}`，两边**本来就该不一样**
    /// （例：「tail -n ${行数:100} ${路径:/var/log/syslog}」）。
    fn ph(s: &str) -> Vec<&str> {
        let b = s.as_bytes();
        let mut out: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'{' {
                if i > 0 && b[i - 1] == b'$' {
                    // `${…}`：整个跳过（连后面的 `}` 一起）
                    i += 1;
                    while i < b.len() && b[i] != b'}' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                let rest = &s[i + 1..];
                if let Some(end) = rest.find('}') {
                    let name = &rest[..end];
                    if !name.is_empty() && !name.contains(' ') {
                        out.push(name);
                    }
                    i += 1 + end + 1;
                    continue;
                }
            }
            i += 1;
        }
        out.sort_unstable();
        out
    }

    #[test]
    fn 查不到就原样返回中文() {
        set_lang(Lang::En);
        // ⚠️ 这句话**故意**用得很古怪：它必须**不在**英文表里
        //    （表里只有真界面上的文案）。要是哪天它进了表 ⇒ 说明
        //    「测试代码里的字符串被当成界面文案」又发生了。
        assert_eq!(
            tr("这句话从来不会出现在任何界面上"),
            "这句话从来不会出现在任何界面上"
        );
        set_lang(Lang::Zh);
        assert_eq!(tr("原样"), "原样");
    }
}
