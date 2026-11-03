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
