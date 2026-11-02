//! 语法高亮分词器（自己写的，不引入 GtkSourceView）。
//!
//! **为什么不引入 GtkSourceView**：GTK4 对应的是 gtksourceview-5，本机
//! （Deepin/Debian 系）只打包了 GTK3 时代的 gtksourceview-3/4；要用 5 就得
//! 给每台堡垒机源码编译一个系统库，deb 包还得跟着加依赖。这里自己写分词器，
//! 换来的是**零新依赖**：单二进制照旧、deb 包不用改、装到任何 GTK4 机器上
//! 都能用。代价是高亮不如 gtksourceview 精细——但对「看配置 / 看日志」这个
//! 主场景已经够用。
//!
//! 输出的偏移量是**字符偏移**（GtkTextBuffer 用字符定位，不是字节），
//! UTF-8 中文文件也能对上位置。
//!
//! 能力边界：
//! - 只做词法级高亮（关键字/字符串/注释/数字…），不做语法分析；
//! - 分不清"字符串里的关键字"是关键字——整段字符串一个颜色；
//! - 未识别扩展名按纯文本处理，但会看 shebang（`#!/bin/sh` → Shell）。

use gtk::prelude::*;
use std::cell::Cell;
use crate::i18n::tr;

/* ---------------- 扫描进度守卫 ---------------- */

/// 扫描进度守卫：保证游标**每轮至少推进一格**。
///
/// **为什么需要它**：这些扫描器全是"手写游标 + while 循环"，只要有一条
/// 分支忘了推进游标，就是**原地打转的无限循环**。而高亮跑在 GTK 主线程
/// 上（`panel::fileedit::apply_highlight`），主线程一进去就再也回不来——
/// 窗口冻结、单核 100%、零 syscall，用户只能杀进程。
///
/// 现场实锤（2026-10-26，办公机 strace）：`scan_markdown` 的代码围栏分支
/// 原地打转 13 分钟，进程既不退出也不报错，就是个"假活"的僵尸会话。
/// `scan_log` 之前也栽过一次同类坑（见该函数里的注释）——说明**不能靠
/// "下次小心"**，得让这类 bug 不可能把程序冻住。
///
/// 做法：每轮开头检查游标有没有比上一轮前进；没前进就**强制走一格**。
/// 于是死循环不再可能发生，最坏情况是那一格没被正确着色（而不是整个程序
/// 卡死）。强制推进的次数记在 `STALLS` 里，测试据此断言「没有发生原地
/// 打转」——把一次线上冻结变成一次测试失败。
struct Progress {
    /// 上一轮开头的游标位置
    last: usize,
}

impl Progress {
    fn new() -> Self {
        Progress { last: usize::MAX }
    }

    /// 每轮开头调一次；返回 false 表示扫完了。
    #[inline]
    fn step(&mut self, i: &mut usize, n: usize) -> bool {
        if *i >= n {
            return false;
        }
        if *i == self.last {
            // 上一轮原地没动：强制推进一格，绝不让它转圈
            *i += 1;
            if *i >= n {
                return false;
            }
            note_stall();
        }
        self.last = *i;
        true
    }
}

thread_local! {
    /// 本次高亮里被守卫挡下的"原地打转"次数（正常恒为 0）。
    static STALLS: Cell<u32> = Cell::new(0);
}

/// 记一次"原地打转"。
fn note_stall() {
    STALLS.with(|s| s.set(s.get() + 1));
}

/// 取"原地打转"计数。
fn stall_count() -> u32 {
    STALLS.with(|s| s.get())
}

/// 清零计数（每次高亮开始时）。
fn reset_stalls() {
    STALLS.with(|s| s.set(0));
}

/// 词法类别。每一类对应一种颜色（见 `theme::syn_colors`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tok {
    /// 不着色（普通标量）
    Plain,
    Comment,
    String,
    Number,
    Keyword,
    Type,
    Literal,
    Var,
    Func,
    Meta,
    Tag,
    Attr,
    Heading,
    Strong,
    Emph,
    Add,
    Del,
}

/// 所有词法类别（建 tag 时按这个顺序建，保证下标稳定）。
pub const ALL_TOKS: [Tok; 17] = [
    Tok::Plain,
    Tok::Comment,
    Tok::String,
    Tok::Number,
    Tok::Keyword,
    Tok::Type,
    Tok::Literal,
    Tok::Var,
    Tok::Func,
    Tok::Meta,
    Tok::Tag,
    Tok::Attr,
    Tok::Heading,
    Tok::Strong,
    Tok::Emph,
    Tok::Add,
    Tok::Del,
];

impl Tok {
    /// tag 名字用的短标识。
    ///
    /// **必须一类一个、互不重复**：GTK 的 tag 表按**名字**索引，同名的 tag
    /// 只有第一个能进表，后面的会被 GTK 直接拒掉（`gtk_text_tag_table_add`
    /// 返回 false 并打 "A tag named '…' is already in the tag table"）；
    /// 而 `gtk_text_buffer_apply_tag` 又要求 tag 属于本 buffer 的表，
    /// 于是被拒的那些一个都上不去——整屏不着色（这坑踩过，别再犯）。
    pub fn slug(self) -> &'static str {
        match self {
            Tok::Plain => "plain",
            Tok::Comment => "comment",
            Tok::String => "string",
            Tok::Number => "number",
            Tok::Keyword => "keyword",
            Tok::Type => "type",
            Tok::Literal => "literal",
            Tok::Var => "var",
            Tok::Func => "func",
            Tok::Meta => "meta",
            Tok::Tag => "tag",
            Tok::Attr => "attr",
            Tok::Heading => "heading",
            Tok::Strong => "strong",
            Tok::Emph => "emph",
            Tok::Add => "add",
            Tok::Del => "del",
        }
    }
}

/// 支持的语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Plain,
    Rust,
    C,
    Cpp,
    Java,
    Go,
    Js,
    Ts,
    CSharp,
    Kotlin,
    Swift,
    Scala,
    Php,
    Python,
    Shell,
    Json,
    Yaml,
    Toml,
    Ini,
    Markdown,
    Html,
    Css,
    Sql,
    Makefile,
    Log,
    Diff,
    Lua,
    Ruby,
}

impl Lang {
    /// 状态栏里显示的名字（中文，与本项目文案一致）。
    pub fn label(self) -> &'static str {
        match self {
            Lang::Plain => tr("纯文本"),
            Lang::Rust => "Rust",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::Java => "Java",
            Lang::Go => "Go",
            Lang::Js => "JavaScript",
            Lang::Ts => "TypeScript",
            Lang::CSharp => "C#",
            Lang::Kotlin => "Kotlin",
            Lang::Swift => "Swift",
            Lang::Scala => "Scala",
            Lang::Php => "PHP",
            Lang::Python => "Python",
            Lang::Shell => "Shell",
            Lang::Json => "JSON",
            Lang::Yaml => "YAML",
            Lang::Toml => "TOML",
            Lang::Ini => "INI",
            Lang::Markdown => "Markdown",
            Lang::Html => "HTML/XML",
            Lang::Css => "CSS",
            Lang::Sql => "SQL",
            Lang::Makefile => "Makefile",
            Lang::Log => tr("日志"),
            Lang::Diff => tr("差异"),
            Lang::Lua => "Lua",
            Lang::Ruby => "Ruby",
        }
    }

    /// 按文件名/扩展名判断语言；认不出来时看首行 shebang。
    ///
    /// `head` 是文件开头的一小段（一般取第一行），只在扩展名不认识时才用。
    pub fn detect(path: &str, head: &str) -> Lang {
        let base = path.rsplit('/').next().unwrap_or(path);
        let lower = base.to_ascii_lowercase();
        let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");

        let by_ext = match ext {
            "rs" => Lang::Rust,
            "c" | "h" => Lang::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" => Lang::Cpp,
            "java" => Lang::Java,
            "go" => Lang::Go,
            "js" | "mjs" | "cjs" | "jsx" => Lang::Js,
            "ts" | "mts" | "cts" | "tsx" => Lang::Ts,
            "cs" => Lang::CSharp,
            "kt" | "kts" => Lang::Kotlin,
            "swift" => Lang::Swift,
            "scala" | "sc" => Lang::Scala,
            "php" => Lang::Php,
            "py" | "pyw" | "pyi" => Lang::Python,
            "sh" | "bash" | "zsh" | "ksh" | "ash" => Lang::Shell,
            "json" => Lang::Json,
            "yaml" | "yml" => Lang::Yaml,
            "toml" => Lang::Toml,
            "ini" | "cfg" | "conf" | "properties" | "env" | "editorconfig" => Lang::Ini,
            "md" | "markdown" | "mdown" | "mkd" => Lang::Markdown,
            "html" | "htm" | "xhtml" | "xml" | "svg" | "xsd" | "plist" => Lang::Html,
            "css" | "scss" | "less" => Lang::Css,
            "sql" => Lang::Sql,
            "mk" | "mak" => Lang::Makefile,
            "log" => Lang::Log,
            "diff" | "patch" | "rej" => Lang::Diff,
            "lua" => Lang::Lua,
            "rb" => Lang::Ruby,
            _ => {
                // 没有扩展名（或扩展名不认识）：看文件名本身
                if lower.starts_with("makefile") || lower.starts_with("gnumakefile") {
                    Lang::Makefile
                } else if lower == "dockerfile" || lower.starts_with("dockerfile.") {
                    Lang::Ini
                } else {
                    Lang::Plain
                }
            }
        };
        if by_ext != Lang::Plain {
            return by_ext;
        }
        // 扩展名不认识：看 shebang
        let first = head.lines().next().unwrap_or("");
        if !first.starts_with("#!") {
            return Lang::Plain;
        }
        let shebang = first.to_ascii_lowercase();
        if shebang.contains("python") {
            Lang::Python
        } else if shebang.contains("perl") {
            Lang::Plain
        } else if shebang.contains("ruby") {
            Lang::Ruby
        } else if shebang.contains("lua") {
            Lang::Lua
        } else if shebang.contains("node") || shebang.contains("deno") {
            Lang::Js
        } else if shebang.contains("sh") {
            Lang::Shell
        } else {
            Lang::Plain
        }
    }
}

/// 词法类别在 [`ALL_TOKS`] 里的下标（建 tag 数组时按它对应）。
pub fn tok_index(t: Tok) -> usize {
    ALL_TOKS.iter().position(|x| *x == t).unwrap_or(0)
}

/// 按类别建一个高亮 tag（颜色取当前主题）。
///
/// **tag 名字必须唯一**（`pp-tok-<slug>`）：GTK4 的 tag 表按名字索引，同名的
/// 只有第一个进得去，后来的会被直接拒收——踩过一次：17 个 tag 全叫
/// `pp-tok`，结果一个都上不了色（见 ROADMAP 7.7 的补丁说明）。
///
/// 放在这里而不是各写各的：文件编辑器与数据库 SQL 编辑器用的是同一套
/// 类别与配色，规则只应有一份。
pub fn make_tok_tag(tok: Tok, colors: &crate::theme::SynColors) -> gtk::TextTag {
    let tag = gtk::TextTag::new(Some(&format!("pp-tok-{}", tok.slug())));
    if tok != Tok::Plain {
        // Plain 不着色：用正文自己的颜色
        tag.set_foreground(Some(colors.of(tok)));
    }
    if tok == Tok::Comment {
        tag.set_style(gtk::pango::Style::Italic);
    }
    if tok == Tok::Heading || tok == Tok::Strong {
        tag.set_weight(gtk::glib::translate::IntoGlib::into_glib(gtk::pango::Weight::Bold));
    }
    tag
}

/// 一个高亮片段：字符偏移 `[start, end)` + 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub tok: Tok,
}

/// 分词结果 + 语言。
pub struct Highlight {
    pub lang: Lang,
    pub spans: Vec<Span>,
    /// 扫描时被进度守卫挡下的"原地打转"次数。
    ///
    /// 正常恒为 0。非 0 说明扫描器有 bug（某轮忘了推进游标，被守卫强制
    /// 跳过一格），此时**高亮结果不完全可信**，调用方应当放弃本次高亮。
    pub stalls: u32,
}

/// 对文本做词法高亮。`lang` 决定用哪套词法规则。
pub fn highlight(text: &str, lang: Lang) -> Highlight {
    reset_stalls();
    let spans = match lang {
        Lang::Plain => Vec::new(),
        Lang::Json => scan_json(text),
        Lang::Yaml => scan_yaml(text),
        Lang::Toml => scan_toml(text),
        Lang::Ini => scan_ini(text),
        Lang::Markdown => scan_markdown(text),
        Lang::Html => scan_html(text),
        Lang::Css => scan_css(text),
        Lang::Sql => scan_sql(text),
        Lang::Makefile => scan_makefile(text),
        Lang::Log => scan_log(text),
        Lang::Diff => scan_diff(text),
        _ => {
            let p = profile(lang);
            scan_generic(text, &p)
        }
    };
    Highlight {
        lang,
        spans,
        stalls: stall_count(),
    }
}

/* ---------------- 通用扫描器 ---------------- */

/// 一套语言 profile。通用扫描器（C 家族 / Python / Shell …）按它切词。
struct Profile {
    /// 行注释前缀（`//`、`#`）
    line_comment: &'static [&'static str],
    /// 块注释（`/* */`）
    block_comment: Option<(&'static str, &'static str)>,
    /// 双引号字符串
    dq_string: bool,
    /// 单引号字符串（Rust/C 里是字符字面量，不当字符串）
    sq_string: bool,
    /// 反斜杠转义
    escape: bool,
    /// 三引号字符串（Python）
    triple: bool,
    /// 字符字面量 `'a'`
    char_literal: bool,
    /// 原始字符串 `r"..."` / `r#"..."#`（Rust）
    raw_string: bool,
    /// 关键字
    keywords: &'static [&'static str],
    /// 额外的关键字（如 TypeScript 比 JavaScript 多的一批）
    extra_keywords: &'static [&'static str],
    /// 类型名
    types: &'static [&'static str],
    /// 字面量（true/false/null）
    literals: &'static [&'static str],
    /// 标识符里允许的额外字符（Shell 的 `$`）
    ident_extra: &'static [char],
    /// 行首 `#` 是预处理器指令
    preprocessor: bool,
    /// `@name` 是装饰器
    decorator: bool,
    /// `$VAR` / `${VAR}` 是变量
    dollar_var: bool,
    /// 反引号是命令替换
    backtick: bool,
}

const EMPTY: &[&str] = &[];

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "type", "unsafe", "use",
    "where", "while", "yield", "union", "macro_rules", "gen", "dyn",
];
const RUST_TYPES: &[&str] = &[
    "bool", "char", "str", "String", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32",
    "i64", "i128", "isize", "f32", "f64", "Vec", "Option", "Result", "Box", "Rc", "Arc", "HashMap",
    "HashSet", "BTreeMap", "Cow", "Path", "PathBuf", "Duration",
];
const RUST_LIT: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];

const C_KW: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern",
    "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static", "struct",
    "switch", "typedef", "union", "volatile", "while", "_Bool", "_Atomic", "_Static_assert",
    "_Thread_local",
];
const C_TYPES: &[&str] = &[
    "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "size_t",
    "ssize_t", "ptrdiff_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t",
    "uint32_t", "uint64_t", "FILE", "bool",
];

const CPP_KW: &[&str] = &[
    "alignas", "alignof", "and", "asm", "auto", "break", "case", "catch", "class", "concept",
    "const", "consteval", "constexpr", "constinit", "continue", "co_await", "co_return",
    "co_yield", "decltype", "default", "delete", "do", "dynamic_cast", "else", "enum", "explicit",
    "export", "extern", "for", "friend", "goto", "if", "inline", "mutable", "namespace", "new",
    "noexcept", "operator", "private", "protected", "public", "reinterpret_cast", "requires",
    "return", "sizeof", "static", "static_assert", "static_cast", "struct", "switch", "template",
    "this", "thread_local", "throw", "try", "typedef", "typeid", "typename", "union", "using",
    "virtual", "volatile", "while",
];
const CPP_TYPES: &[&str] = &[
    "bool", "char", "char8_t", "char16_t", "char32_t", "double", "float", "int", "long", "short",
    "signed", "unsigned", "void", "wchar_t", "string", "string_view", "vector", "map", "set",
    "size_t", "auto",
];

const JAVA_KW: &[&str] = &[
    "abstract", "assert", "break", "case", "catch", "class", "const", "continue", "default", "do",
    "else", "enum", "extends", "final", "finally", "for", "goto", "if", "implements", "import",
    "instanceof", "interface", "native", "new", "package", "private", "protected", "public",
    "record", "return", "sealed", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "var", "while", "yield",
];
const JAVA_TYPES: &[&str] = &[
    "boolean", "byte", "char", "double", "float", "int", "long", "short", "void", "String",
    "Object", "Integer", "Long", "Double", "Boolean", "List", "Map", "Set", "Exception",
];

const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for",
    "func", "go", "goto", "if", "import", "interface", "map", "package", "range", "return",
    "select", "struct", "switch", "type", "var",
];
const GO_TYPES: &[&str] = &[
    "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8",
    "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64",
    "uintptr", "any",
];

const JS_KW: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
    "default", "delete", "do", "else", "export", "extends", "finally", "for", "function", "if",
    "import", "in", "instanceof", "let", "new", "of", "return", "static", "super", "switch",
    "this", "throw", "try", "typeof", "var", "void", "while", "with", "yield", "from", "as",
];
const TS_EXTRA_KW: &[&str] = &[
    "abstract", "declare", "enum", "implements", "interface", "is", "keyof", "namespace",
    "never", "private", "protected", "public", "readonly", "type", "unknown", "infer",
];
const JS_TYPES: &[&str] = &[
    "Array", "Object", "String", "Number", "Boolean", "Promise", "Map", "Set", "WeakMap",
    "WeakSet", "Symbol", "JSON", "Math", "Date", "RegExp", "Error", "BigInt", "Uint8Array",
];

const CSHARP_KW: &[&str] = &[
    "abstract", "as", "async", "await", "base", "break", "case", "catch", "checked", "class",
    "const", "continue", "default", "delegate", "do", "else", "enum", "event", "explicit",
    "extern", "finally", "fixed", "for", "foreach", "get", "goto", "if", "implicit", "in",
    "interface", "internal", "is", "lock", "namespace", "new", "operator", "out", "override",
    "params", "private", "protected", "public", "readonly", "record", "ref", "return", "sealed",
    "set", "sizeof", "stackalloc", "static", "struct", "switch", "this", "throw", "try",
    "typeof", "unchecked", "unsafe", "using", "var", "virtual", "volatile", "while", "yield",
];
const CSHARP_TYPES: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "float", "int", "long", "object", "sbyte",
    "short", "string", "uint", "ulong", "ushort", "void", "List", "Dictionary", "Task",
];

const KOTLIN_KW: &[&str] = &[
    "as", "break", "class", "continue", "do", "else", "false", "for", "fun", "if", "in",
    "interface", "is", "null", "object", "package", "return", "super", "this", "throw", "try",
    "typealias", "val", "var", "when", "while", "by", "catch", "constructor", "delegate",
    "dynamic", "field", "file", "finally", "get", "import", "init", "param", "property",
    "receiver", "set", "setparam", "where",
];
const KOTLIN_TYPES: &[&str] = &[
    "Boolean", "Byte", "Char", "Double", "Float", "Int", "Long", "Short", "String", "Unit",
    "Any", "Nothing", "List", "Map", "Set",
];

const SWIFT_KW: &[&str] = &[
    "associatedtype", "class", "deinit", "enum", "extension", "fileprivate", "func", "import",
    "init", "inout", "internal", "let", "open", "operator", "private", "protocol", "public",
    "rethrows", "static", "struct", "subscript", "typealias", "var", "break", "case", "continue",
    "default", "defer", "do", "else", "fallthrough", "for", "guard", "if", "in", "repeat",
    "return", "switch", "where", "while", "as", "is", "nil", "true", "false", "self", "Self",
];
const SWIFT_TYPES: &[&str] = &[
    "Int", "Double", "Float", "Bool", "String", "Character", "Array", "Dictionary", "Set",
    "Optional", "Any", "AnyObject",
];

const SCALA_KW: &[&str] = &[
    "abstract", "case", "catch", "class", "def", "do", "else", "extends", "final", "finally",
    "for", "forSome", "if", "implicit", "import", "lazy", "match", "new", "object", "override",
    "package", "private", "protected", "return", "sealed", "super", "this", "throw", "trait",
    "try", "type", "val", "var", "while", "with", "yield", "true", "false", "null",
];
const SCALA_TYPES: &[&str] = &["Int", "Long", "Double", "Float", "Boolean", "String", "Char", "Any"];

const PHP_KW: &[&str] = &[
    "abstract", "and", "array", "as", "break", "callable", "case", "catch", "class", "clone",
    "const", "continue", "declare", "default", "do", "echo", "else", "elseif", "empty",
    "enddeclare", "endfor", "endforeach", "endif", "endswitch", "endwhile", "extends", "final",
    "finally", "fn", "for", "foreach", "function", "global", "goto", "if", "implements",
    "include", "instanceof", "insteadof", "interface", "isset", "list", "match", "namespace",
    "new", "or", "print", "private", "protected", "public", "readonly", "require", "return",
    "static", "switch", "throw", "trait", "try", "unset", "use", "var", "while", "xor", "yield",
    "true", "false", "null", "int", "float", "string", "bool", "void", "mixed", "iterable",
];

const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match",
    "case",
];
const PY_TYPES: &[&str] = &[
    "int", "float", "str", "bool", "bytes", "list", "dict", "set", "tuple", "complex", "object",
    "type", "self", "cls", "frozenset", "bytearray",
];
const PY_LIT: &[&str] = &["True", "False", "None"];

const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until", "do", "done",
    "in", "function", "select", "time", "return", "break", "continue", "local", "export",
    "readonly", "declare", "typeset", "unset", "shift", "source", "eval", "exec", "exit", "trap",
    "set", "echo",
];
const SH_LIT: &[&str] = &["true", "false"];

const SQL_KW: &[&str] = &[
    "add", "all", "alter", "and", "as", "asc", "begin", "between", "by", "case", "cast", "check",
    "column", "commit", "constraint", "create", "cross", "database", "default", "delete", "desc",
    "distinct", "drop", "else", "end", "exists", "foreign", "from", "full", "grant", "group",
    "having", "in", "index", "inner", "insert", "into", "is", "join", "key", "left", "like",
    "limit", "not", "null", "offset", "on", "or", "order", "outer", "primary", "references",
    "revoke", "right", "rollback", "select", "set", "table", "then", "transaction", "union",
    "unique", "update", "values", "view", "when", "where", "with",
];
const SQL_LIT: &[&str] = &["true", "false", "null"];

const LUA_KW: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

const RUBY_KW: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
    "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo",
    "rescue", "retry", "return", "self", "super", "then", "true", "undef", "unless", "until",
    "when", "while", "yield", "require", "attr_accessor", "attr_reader", "attr_writer", "puts",
    "include", "extend", "lambda", "proc",
];

fn profile(lang: Lang) -> Profile {
    match lang {
        Lang::Rust => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: false,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: true,
            keywords: RUST_KW,
            extra_keywords: EMPTY,
            types: RUST_TYPES,
            literals: RUST_LIT,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::C => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: false,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: false,
            keywords: C_KW,
            extra_keywords: EMPTY,
            types: C_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: true,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Cpp => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: false,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: true,
            keywords: CPP_KW,
            extra_keywords: EMPTY,
            types: CPP_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: true,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Java => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: false,
            keywords: JAVA_KW,
            extra_keywords: EMPTY,
            types: JAVA_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Go => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: true,
            keywords: GO_KW,
            extra_keywords: EMPTY,
            types: GO_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Js | Lang::Ts => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: JS_KW,
            // TypeScript 比 JavaScript 多的一批关键字
            extra_keywords: if lang == Lang::Ts { TS_EXTRA_KW } else { EMPTY },
            types: JS_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: true,
        },
        Lang::CSharp => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: false,
            keywords: CSHARP_KW,
            extra_keywords: EMPTY,
            types: CSHARP_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: true,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Kotlin => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: true,
            char_literal: true,
            raw_string: false,
            keywords: KOTLIN_KW,
            extra_keywords: EMPTY,
            types: KOTLIN_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: true,
            dollar_var: false,
            backtick: false,
        },
        Lang::Swift => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: false,
            escape: true,
            triple: false,
            char_literal: true,
            raw_string: true,
            keywords: SWIFT_KW,
            extra_keywords: EMPTY,
            types: SWIFT_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: true,
            dollar_var: false,
            backtick: false,
        },
        Lang::Scala => Profile {
            line_comment: &["//"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: true,
            char_literal: true,
            raw_string: false,
            keywords: SCALA_KW,
            extra_keywords: EMPTY,
            types: SCALA_TYPES,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: true,
            dollar_var: false,
            backtick: false,
        },
        Lang::Php => Profile {
            line_comment: &["//", "#"],
            block_comment: Some(("/*", "*/")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: PHP_KW,
            extra_keywords: EMPTY,
            types: EMPTY,
            literals: EMPTY,
            ident_extra: &['$'],
            preprocessor: false,
            decorator: false,
            dollar_var: true,
            backtick: false,
        },
        Lang::Python => Profile {
            line_comment: &["#"],
            block_comment: None,
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: true,
            char_literal: false,
            raw_string: false,
            keywords: PY_KW,
            extra_keywords: EMPTY,
            types: PY_TYPES,
            literals: PY_LIT,
            ident_extra: &[],
            preprocessor: false,
            decorator: true,
            dollar_var: false,
            backtick: false,
        },
        Lang::Shell => Profile {
            line_comment: &["#"],
            block_comment: None,
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: SH_KW,
            extra_keywords: EMPTY,
            types: EMPTY,
            literals: SH_LIT,
            ident_extra: &['$'],
            preprocessor: false,
            decorator: false,
            dollar_var: true,
            backtick: true,
        },
        Lang::Lua => Profile {
            line_comment: &["--"],
            block_comment: Some(("--[[", "]]")),
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: LUA_KW,
            extra_keywords: EMPTY,
            types: EMPTY,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
        Lang::Ruby => Profile {
            line_comment: &["#"],
            block_comment: None,
            dq_string: true,
            sq_string: true,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: RUBY_KW,
            extra_keywords: EMPTY,
            types: EMPTY,
            literals: EMPTY,
            ident_extra: &['@', '$'],
            preprocessor: false,
            decorator: false,
            dollar_var: true,
            backtick: true,
        },
        // 其余语言走各自的专用扫描器
        _ => Profile {
            line_comment: &[],
            block_comment: None,
            dq_string: true,
            sq_string: false,
            escape: true,
            triple: false,
            char_literal: false,
            raw_string: false,
            keywords: EMPTY,
            extra_keywords: EMPTY,
            types: EMPTY,
            literals: EMPTY,
            ident_extra: &[],
            preprocessor: false,
            decorator: false,
            dollar_var: false,
            backtick: false,
        },
    }
}

/* ---------------- 通用扫描器实现 ---------------- */

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// 通用扫描器：按 profile 切出注释/字符串/数字/标识符。
fn scan_generic(text: &str, p: &Profile) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0usize;

    // 行首状态：判断当前是否在行首（用于 #include / 装饰器）
    let mut at_line_start = true;

    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];

        // 行注释
        if p
            .line_comment
            .iter()
            .any(|pfx| b[i..].starts_with(pfx.as_bytes()))
        {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            at_line_start = false;
            continue;
        }

        // 块注释
        if let Some((open, close)) = p.block_comment {
            if b[i..].starts_with(open.as_bytes()) {
                let end = find_sub(b, i + open.len(), close.as_bytes()).unwrap_or(n);
                out.push((i, end, Tok::Comment));
                i = end;
                at_line_start = false;
                continue;
            }
        }

        // 预处理器（仅行首）
        if p.preprocessor && c == b'#' && at_line_start {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Meta));
            i = end;
            at_line_start = true; // 指令本身不换行
            continue;
        }

        // 装饰器 @name
        if p.decorator && c == b'@' && i + 1 < n && is_ident_start(b[i + 1]) {
            let mut j = i + 1;
            while j < n && is_ident_cont(b[j]) {
                j += 1;
            }
            out.push((i, j, Tok::Meta));
            i = j;
            at_line_start = false;
            continue;
        }

        // 变量 $VAR / ${VAR}
        if p.dollar_var && c == b'$' {
            let mut j = i + 1;
            if j < n && b[j] == b'{' {
                while j < n && b[j] != b'}' {
                    j += 1;
                }
                j = (j + 1).min(n);
            } else {
                while j < n && (is_ident_cont(b[j]) || b[j] == b'$') {
                    j += 1;
                }
            }
            if j > i + 1 {
                out.push((i, j, Tok::Var));
                i = j;
                at_line_start = false;
                continue;
            }
        }

        // 原始字符串 r"..." / r#"..."#（Rust）
        if p.raw_string && (c == b'r' || c == b'b') && i + 1 < n {
            let mut j = i + 1;
            if b[j] == b'r' {
                j += 1;
            }
            let mut hashes = 0usize;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                let content_start = j + 1;
                let mut k = content_start;
                let mut end = n;
                while k < n {
                    if b[k] == b'"' {
                        let mut h = 0usize;
                        while h < hashes && k + 1 + h < n && b[k + 1 + h] == b'#' {
                            h += 1;
                        }
                        if h == hashes {
                            end = k + 1 + hashes;
                            break;
                        }
                    }
                    k += 1;
                }
                out.push((i, end, Tok::String));
                i = end;
                at_line_start = false;
                continue;
            }
        }

        // 字符串
        if (c == b'"' && p.dq_string) || (c == b'\'' && p.sq_string) {
            let end = scan_string(b, i, c, p.escape, p.triple);
            out.push((i, end, Tok::String));
            i = end;
            at_line_start = false;
            continue;
        }

        // 字符字面量 'a'（Rust/C/C++/Java…）；生命周期 'a 不是字面量
        if p.char_literal && c == b'\'' {
            if let Some(end) = scan_char_literal(b, i) {
                out.push((i, end, Tok::String));
                i = end;
                at_line_start = false;
                continue;
            }
            // 生命周期：跳过 'x
            i += 1;
            at_line_start = false;
            continue;
        }

        // 反引号（Shell 命令替换 / JS 模板字符串）
        if p.backtick && c == b'`' {
            let mut j = i + 1;
            while j < n && b[j] != b'`' {
                if p.escape && b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            out.push((i, end, Tok::String));
            i = end;
            at_line_start = false;
            continue;
        }

        // 数字
        if c.is_ascii_digit() {
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            at_line_start = false;
            continue;
        }

        // 标识符
        if is_ident_start(c) || (p.ident_extra.contains(&(c as char)) && i + 1 < n && is_ident_start(b[i + 1]))
        {
            let mut j = i;
            if p.ident_extra.contains(&(c as char)) {
                j += 1;
            }
            while j < n && (is_ident_cont(b[j]) || p.ident_extra.contains(&(b[j] as char))) {
                j += 1;
            }
            let word = &text[i..j];
            let tok = classify(word, p, b, j);
            if let Some(t) = tok {
                out.push((i, j, t));
            }
            i = j;
            at_line_start = false;
            continue;
        }

        // 换行前后要更新行首状态
        if c == b'\n' {
            at_line_start = true;
        }
        i += 1;
    }

    to_char_spans(text, &out)
}

/// 标识符归类：字面量 / 类型 / 关键字 / 函数名 / 其它。
fn classify(word: &str, p: &Profile, b: &[u8], end: usize) -> Option<Tok> {
    if p.literals.contains(&word) {
        return Some(Tok::Literal);
    }
    if p.types.contains(&word) {
        return Some(Tok::Type);
    }
    if p.keywords.contains(&word) || p.extra_keywords.contains(&word) {
        return Some(Tok::Keyword);
    }
    // 函数名：后面紧跟 '('
    let mut k = end;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    if k < b.len() && b[k] == b'(' {
        return Some(Tok::Func);
    }
    None
}

/// 扫描字符串（含三引号与转义），返回结束后的字节下标。
fn scan_string(b: &[u8], start: usize, quote: u8, escape: bool, triple: bool) -> usize {
    let n = b.len();
    // 三引号
    if triple && start + 2 < n && b[start + 1] == quote && b[start + 2] == quote {
        let mut i = start + 3;
        while i + 2 < n {
            if escape && b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == quote && b[i + 1] == quote && b[i + 2] == quote {
                return i + 3;
            }
            i += 1;
        }
        return n;
    }
    let mut i = start + 1;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        if escape && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == quote {
            return i + 1;
        }
        if b[i] == b'\n' && !triple {
            // 普通字符串不跨行（Python 之外的多数语言如此）
            return i;
        }
        i += 1;
    }
    n
}

/// 扫描字符字面量 `'a'` / `'\n'`。不是字面量（如生命周期 `'a`）时返回 None。
fn scan_char_literal(b: &[u8], start: usize) -> Option<usize> {
    let n = b.len();
    if start + 2 >= n {
        return None;
    }
    if b[start + 1] == b'\\' {
        // 转义字符：'\' 至少两个字节
        if start + 3 < n && b[start + 2] == b'\'' {
            return Some(start + 3);
        }
        return None;
    }
    if b[start + 2] == b'\'' {
        return Some(start + 3);
    }
    None
}

/// 扫描数字（含 0x / 小数 / 指数 / 词法后缀）。
fn scan_number(b: &[u8], start: usize) -> usize {
    let n = b.len();
    let mut i = start;
    if b[i] == b'0' && i + 1 < n && (b[i + 1] | 0x20) == b'x' {
        i += 2;
        while i < n && (b[i].is_ascii_hexdigit() || b[i] == b'_') {
            i += 1;
        }
        return i;
    }
    if b[i] == b'0' && i + 1 < n && (b[i + 1] | 0x20) == b'b' {
        i += 2;
        while i < n && (b[i] == b'0' || b[i] == b'1' || b[i] == b'_') {
            i += 1;
        }
        return i;
    }
    while i < n && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i < n && b[i] == b'.' && i + 1 < n && b[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    if i < n && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if j < n && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < n && b[j].is_ascii_digit() {
            i = j;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    // 类型后缀：u8 / i32 / f64 / L / UL
    while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        // 后缀只吃一小段，避免把 `123abc` 之后的标识符全吞掉
        if i - start > 12 {
            break;
        }
        i += 1;
    }
    i
}

fn find_line_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() && b[i] != b'\n' {
        i += 1;
    }
    i
}

/// 跳到**下一行的行首**。
///
/// ⚠️ **不能拿 `find_line_end` 的返回值当"下一行起点"**：它返回的是 '\n'
/// **自身**的下标（'\n' 不算进 span，所以它是"行尾"而不是"下一行头"）。
/// 若游标停在 '\n' 上、而 `line_start` 又为 true，下一轮 `find_line_end`
/// 会**立刻返回同一个下标**——游标原地不动，死循环。
/// `scan_markdown` 的代码围栏分支就栽在这上面（2026-10-26 现场冻结）。
#[inline]
fn next_line_start(b: &[u8], from: usize) -> usize {
    let e = find_line_end(b, from);
    if e < b.len() {
        e + 1
    } else {
        b.len()
    }
}

fn find_sub(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= b.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/* ---------------- 专用扫描器 ---------------- */

/// JSON：键（字符串后跟 ':'）与值区分。
fn scan_json(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if c == b'"' {
            let end = scan_string(b, i, b'"', true, false);
            // 后面紧跟 ':' 就是键
            let mut k = end;
            while k < n && (b[k] == b' ' || b[k] == b'\t' || b[k] == b'\n' || b[k] == b'\r') {
                k += 1;
            }
            let tok = if k < n && b[k] == b':' {
                Tok::Attr
            } else {
                Tok::String
            };
            out.push((i, end, tok));
            i = end;
            continue;
        }
        if c.is_ascii_digit() || (c == b'-' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i;
            while j < n && is_ident_cont(b[j]) {
                j += 1;
            }
            let w = &text[i..j];
            let tok = if w == "true" || w == "false" || w == "null" {
                Tok::Literal
            } else {
                Tok::Keyword
            };
            out.push((i, j, tok));
            i = j;
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// YAML：`key:` 是键，`#` 后是注释，值里的字符串/数字/布尔。
///
/// 除了这四类，还认 YAML **特有**的结构（用户 2026-11-02「增加 yml 的语法高亮」）：
/// - **文档分隔符** `---` / `...`（必须顶格）
/// - **序列项** `- item`（行首的 `-`；**后面跟空格**才是序列项，`-5` 是负数）
/// - **块标量** `|` / `>`（含 `|-` `>-` `|2` 标记；**内容整段当字符串**）
/// - **标签** `!!str` / `!tag`
/// - **锚点/别名** `&a` / `*a`
/// - **日期/时间** `2024-01-31` / `12:30:00`（当**一个**数字，不拆成三段）
fn scan_yaml(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        // 文档分隔符 `---` / `...`：**必须顶格**（前面有缩进就不算）
        if (i == 0 || b[i - 1] == b'\n')
            && (b[i..].starts_with(b"---") || b[i..].starts_with(b"..."))
        {
            out.push((i, i + 3, Tok::Heading));
            i += 3;
            line_start = false;
            continue;
        }
        // '#' 在行首、或前面是空白时都是注释（YAML 的规矩）
        if c == b'#' && (line_start || i > 0 && (b[i - 1] == b' ' || b[i - 1] == b'\t')) {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        // 标签 `!!str` / `!tag`（`!` 后面必须跟名字，否则当普通标量）
        if c == b'!' {
            let mut j = i + 1;
            if j < n && b[j] == b'!' {
                j += 1;
            }
            let name = j;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-' || b[j] == b'/') {
                j += 1;
            }
            if j > name {
                out.push((i, j, Tok::Tag));
                i = j;
                line_start = false;
                continue;
            }
        }
        if c == b'"' || c == b'\'' {
            let end = scan_string(b, i, c, c == b'"', false);
            out.push((i, end, Tok::String));
            i = end;
            line_start = false;
            continue;
        }
        // 数字（负号、日期、时间都算**一个**数）
        if c.is_ascii_digit() || (c == b'-' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let end = scan_yaml_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            line_start = false;
            continue;
        }
        // 序列项的 `-`：**行首**且 `-` 后面是空白
        //（`-5` 已经在上面当数字处理掉了）
        if c == b'-' && line_start && (i + 1 >= n || b[i + 1] == b' ' || b[i + 1] == b'\t') {
            out.push((i, i + 1, Tok::Meta));
            i += 1;
            line_start = false;
            continue;
        }
        // 块标量 `|` / `>`：标记本身 + **整段内容**
        if c == b'|' || c == b'>' {
            if let Some((ind_end, body_start)) = yaml_block_indicator(b, i) {
                out.push((i, ind_end, Tok::Meta));
                // 内容行的缩进要比**键那行**深，所以先量出键的缩进
                let indent = yaml_line_indent(b, i);
                let body_end = yaml_block_body_end(b, body_start, indent);
                if body_end > body_start {
                    out.push((body_start, body_end, Tok::String));
                }
                // 整段跳过（别再逐字扫一遍），停在最后一行内容的末尾
                i = body_end;
                line_start = true;
                continue;
            }
        }
        if c == b'&' || c == b'*' {
            // 锚点/别名
            let mut j = i + 1;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            out.push((i, j, Tok::Meta));
            i = j;
            line_start = false;
            continue;
        }
        // 合并键 `<<:`（YAML 拿它合并映射）
        if c == b'<' && b[i..].starts_with(b"<<") {
            let mut j = i + 2;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j < n && b[j] == b':' {
                out.push((i, i + 2, Tok::Attr));
                i += 2;
                line_start = false;
                continue;
            }
        }
        // 空值 `~`（必须**单独一个词**，否则 `~foo` 是普通标量）
        if c == b'~' && (i + 1 >= n || !is_ident_cont(b[i + 1])) {
            out.push((i, i + 1, Tok::Literal));
            i += 1;
            line_start = false;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            let w = &text[i..j];
            // 后面（跳过空格）是 ':' → 键
            let mut k = j;
            while k < n && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            let is_key = k < n && b[k] == b':';
            let tok = if is_key {
                Tok::Attr
            } else if w.eq_ignore_ascii_case("true")
                || w.eq_ignore_ascii_case("false")
                || w.eq_ignore_ascii_case("null")
                || w == "yes"
                || w == "no"
                || w == "on"
                || w == "off"
            {
                Tok::Literal
            } else {
                // 普通标量不着色（着成关键字色会满屏发紫）
                Tok::Plain
            };
            if tok != Tok::Plain {
                out.push((i, j, tok));
            }
            i = j;
            line_start = false;
            continue;
        }
        if c == b'\n' {
            line_start = true;
        } else if c != b' ' && c != b'\t' && c != b'\r' {
            line_start = false;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// YAML 里的数字：比通用的多了**日期**和**时间**。
///
/// `2024-01-31`、`12:30:00` 在 YAML 里极常见；通用数字扫描器会把
/// `2024-01-31` 拆成「2024」「-01」「-31」三段（中间还夹一个负号），
/// 颜色跳来跳去很难看。
fn scan_yaml_number(b: &[u8], start: usize) -> usize {
    let n = b.len();
    let mut i = start;
    // 负号（`-5`）：调用方保证 `-` 后面就是数字
    if b[i] == b'-' {
        i += 1;
    }
    let d0 = i;
    while i < n && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    let digits = i - d0;
    // 日期 `2024-01-31`（年 4 位 - 月 2 位 - 日 2 位）
    if digits == 4 && i + 2 < n && b[i] == b'-' && b[i + 1].is_ascii_digit() && b[i + 2].is_ascii_digit() {
        let mut j = i + 3;
        if j + 1 < n && b[j] == b'-' && b[j + 1].is_ascii_digit() {
            j += 1;
            while j < n && b[j].is_ascii_digit() {
                j += 1;
            }
            return j;
        }
    }
    // 小数
    if i < n && b[i] == b'.' && i + 1 < n && b[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    // 指数 `1e3` / `1.5e-3`
    if i < n && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if j < n && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < n && b[j].is_ascii_digit() {
            i = j;
            while i < n && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    // 时间 `12:30` / `12:30:00`
    if i < n && b[i] == b':' && i + 1 < n && b[i + 1].is_ascii_digit() {
        let mut j = i + 1;
        while j < n && b[j].is_ascii_digit() {
            j += 1;
        }
        if j < n && b[j] == b':' && j + 1 < n && b[j + 1].is_ascii_digit() {
            j += 1;
            while j < n && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    i
}

/// 块标量标记（`|` / `>`）扫到哪儿结束，外加**内容从哪儿开始**。
///
/// 返回 `None` 表示这**不是**块标量（比如 `a|b` 里的 `|`）。
/// 判据：标记后面只能跟缩进指示器（`2`）、chomping（`+` / `-`）、
/// 空白和注释，然后就该换行了。
fn yaml_block_indicator(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let n = b.len();
    let mut j = i + 1;
    // 缩进指示器 / chomping 标记（顺序随意：`|2-`、`|-2` 都行）
    while j < n && (b[j].is_ascii_digit() || b[j] == b'+' || b[j] == b'-') {
        j += 1;
    }
    let ind_end = j;
    // 后面只能是空白 + 注释
    while j < n && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    if j < n && b[j] == b'#' {
        j = find_line_end(b, j);
    }
    if j < n && b[j] != b'\n' {
        return None;
    }
    Some((ind_end, next_line_start(b, j)))
}

/// `pos` 所在那一行的**缩进**（前导空白个数）。
fn yaml_line_indent(b: &[u8], pos: usize) -> usize {
    let mut s = pos;
    while s > 0 && b[s - 1] != b'\n' {
        s -= 1;
    }
    let mut n = 0;
    let mut i = s;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        n += 1;
        i += 1;
    }
    n
}

/// 块标量**内容**的末尾（不含最后一行的换行符）。
///
/// 内容 = 后面那些**缩进比键更深**的行；遇到空行算内容的一部分，
/// 遇到缩进 <= 键的行就是下一个键了，块到此为止。
fn yaml_block_body_end(b: &[u8], start: usize, key_indent: usize) -> usize {
    let n = b.len();
    let mut i = start;
    let mut end = start;
    while i < n {
        let mut j = i;
        let mut ind = 0;
        while j < n && (b[j] == b' ' || b[j] == b'\t') {
            j += 1;
            ind += 1;
        }
        let line_end = find_line_end(b, j);
        if j >= line_end {
            // 空行：属于块的一部分
            end = next_line_start(b, j);
            i = end;
            continue;
        }
        if ind <= key_indent {
            // 同级或更浅 ⇒ 块结束（后面是下一个键）
            break;
        }
        end = line_end;
        i = next_line_start(b, j);
    }
    end
}
/// TOML：`[section]`、`key = value`、字符串/数字/布尔。
fn scan_toml(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if c == b'#' {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if c == b'[' {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Heading));
            i = end;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let end = scan_string(b, i, c, c == b'"', false);
            out.push((i, end, Tok::String));
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            let w = &text[i..j];
            let mut k = j;
            while k < n && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            let is_key = k < n && b[k] == b'=';
            let tok = if is_key {
                Tok::Attr
            } else if w == "true" || w == "false" {
                Tok::Literal
            } else {
                Tok::Plain
            };
            if tok != Tok::Plain {
                out.push((i, j, tok));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// INI / conf：`[section]`、`key = value`。
fn scan_ini(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if line_start && (c == b'#' || c == b';') {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if line_start && c == b'[' {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Heading));
            i = end;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let end = scan_string(b, i, c, true, false);
            out.push((i, end, Tok::String));
            i = end;
            line_start = false;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            line_start = false;
            continue;
        }
        if is_ident_start(c) || c == b'.' || c == b'-' || c == b'/' {
            let mut j = i;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'.' || b[j] == b'-' || b[j] == b'/') {
                j += 1;
            }
            let mut k = j;
            while k < n && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            let is_key = k < n && (b[k] == b'=' || b[k] == b':');
            if is_key {
                out.push((i, j, Tok::Attr));
            }
            i = j;
            line_start = false;
            continue;
        }
        if c == b'\n' {
            line_start = true;
        } else if c != b' ' && c != b'\t' && c != b'\r' {
            line_start = false;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// Markdown：ATX 标题、围栏代码块、行内代码、粗体、斜体、链接。
fn scan_markdown(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut in_fence = false;
    let mut line_start = true;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        if line_start {
            let line_end = find_line_end(b, i);
            let line = &text[i..line_end];
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_fence = !in_fence;
                out.push((i, line_end, Tok::Meta));
                // 走"下一行行首"，不能停在 '\n' 上（见 next_line_start 的警告）
                i = next_line_start(b, i);
                line_start = true;
                continue;
            }
            if !in_fence && trimmed.starts_with('#') {
                let hashes = trimmed.chars().take_while(|c| *c == '#').count();
                if hashes <= 6 {
                    out.push((i, i + indent + hashes, Tok::Heading));
                    let rest = i + indent + hashes;
                    if rest < line_end {
                        out.push((rest, line_end, Tok::Heading));
                    }
                    i = next_line_start(b, i);
                    line_start = true;
                    continue;
                }
            }
            if !in_fence && trimmed.starts_with('>') {
                out.push((i, i + indent + 1, Tok::Comment));
            }
        }
        if in_fence {
            // 围栏内按普通文本处理。
            // ⚠️ 这里必须走 next_line_start：原来写的是 `i = line_end`，
            // 而 line_end 正是 '\n' 的下标 → 下一轮 find_line_end 返回同一个
            // 下标 → 游标原地打转 → 无限循环（2026-10-26 现场冻结的根因）。
            i = next_line_start(b, i);
            line_start = true;
            continue;
        }
        let c = b[i];
        match c {
            b'`' => {
                let mut j = i + 1;
                while j < n && b[j] != b'`' {
                    j += 1;
                }
                let end = (j + 1).min(n);
                out.push((i, end, Tok::String));
                i = end;
                line_start = false;
            }
            b'*' | b'_' => {
                let marker = c;
                let strong = i + 1 < n && b[i + 1] == marker;
                let width = if strong { 2 } else { 1 };
                let mut j = i + width;
                while j < n && b[j] != marker {
                    j += 1;
                }
                if j < n && j > i + width {
                    out.push((i, i + width, Tok::Meta));
                    out.push((i + width, j, if strong { Tok::Strong } else { Tok::Emph }));
                    out.push((j, j + 1, Tok::Meta));
                    i = j + 1;
                } else {
                    i += 1;
                }
                line_start = false;
            }
            b'[' => {
                // [文字](链接)
                let close = find_sub(b, i + 1, b"]").unwrap_or(n);
                if close < n && close + 1 < n && b[close + 1] == b'(' {
                    let paren = find_sub(b, close + 2, b")").unwrap_or(n);
                    out.push((i, close + 1, Tok::Attr));
                    out.push((close + 2, paren, Tok::String));
                    i = (paren + 1).min(n);
                } else {
                    i += 1;
                }
                line_start = false;
            }
            b'\n' => {
                line_start = true;
                i += 1;
            }
            _ => {
                if c != b' ' && c != b'\t' && c != b'\r' {
                    line_start = false;
                }
                i += 1;
            }
        }
    }
    to_char_spans(text, &out)
}

/// HTML / XML：注释、标签名、属性名、属性值。
fn scan_html(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        if b[i..].starts_with(b"<!--") {
            let end = find_sub(b, i + 4, b"-->").map(|e| e + 3).unwrap_or(n);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if b[i] == b'<' {
            let mut j = i + 1;
            if j < n && (b[j] == b'/' || b[j] == b'!' || b[j] == b'?') {
                j += 1;
            }
            let name_start = j;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-' || b[j] == b':') {
                j += 1;
            }
            if j > name_start {
                out.push((name_start, j, Tok::Tag));
            }
            // 标签内部：属性名 = value
            while j < n && b[j] != b'>' {
                if b[j] == b'"' || b[j] == b'\'' {
                    let end = scan_string(b, j, b[j], true, false);
                    out.push((j, end, Tok::String));
                    j = end;
                    continue;
                }
                if is_ident_start(b[j]) {
                    let a = j;
                    while j < n && (is_ident_cont(b[j]) || b[j] == b'-' || b[j] == b':') {
                        j += 1;
                    }
                    // 后面是 '=' 才是属性名
                    let mut k = j;
                    while k < n && (b[k] == b' ' || b[k] == b'\t') {
                        k += 1;
                    }
                    if k < n && b[k] == b'=' {
                        out.push((a, j, Tok::Attr));
                    }
                    continue;
                }
                j += 1;
            }
            i = (j + 1).min(n);
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// CSS：注释、选择器、属性名、值、数字。
fn scan_css(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut in_block = false;
    let mut after_colon = false;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if b[i..].starts_with(b"/*") {
            let end = find_sub(b, i + 2, b"*/").map(|e| e + 2).unwrap_or(n);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let end = scan_string(b, i, c, true, false);
            out.push((i, end, Tok::String));
            i = end;
            continue;
        }
        if c == b'{' {
            in_block = true;
            after_colon = false;
            i += 1;
            continue;
        }
        if c == b'}' {
            in_block = false;
            after_colon = false;
            i += 1;
            continue;
        }
        if c == b':' {
            after_colon = true;
            i += 1;
            continue;
        }
        if c == b';' {
            after_colon = false;
            i += 1;
            continue;
        }
        if c == b'#' && i + 1 < n && is_ident_start(b[i + 1]) {
            let mut j = i + 1;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            out.push((i, j, Tok::Type));
            i = j;
            continue;
        }
        if c == b'.' && i + 1 < n && is_ident_start(b[i + 1]) {
            let mut j = i + 1;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            out.push((i, j, Tok::Attr));
            i = j;
            continue;
        }
        if c == b'@' && i + 1 < n && is_ident_start(b[i + 1]) {
            let mut j = i + 1;
            while j < n && is_ident_cont(b[j]) {
                j += 1;
            }
            out.push((i, j, Tok::Meta));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = scan_number(b, i);
            // 单位（px / em / % …）
            while j < n && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            out.push((i, j, Tok::Number));
            i = j;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            let w = &text[i..j];
            let tok = if in_block && !after_colon {
                Tok::Attr
            } else if in_block && after_colon {
                Tok::String
            } else {
                Tok::Tag
            };
            let _ = w;
            out.push((i, j, tok));
            i = j;
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// SQL：关键字（大小写不敏感）、字符串、注释、数字。
fn scan_sql(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if b[i..].starts_with(b"--") {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let end = find_sub(b, i + 2, b"*/").map(|e| e + 2).unwrap_or(n);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if c == b'"' || c == b'\'' || c == b'`' {
            let end = scan_string(b, i, c, true, false);
            out.push((i, end, Tok::String));
            i = end;
            continue;
        }
        if c.is_ascii_digit() {
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i;
            while j < n && is_ident_cont(b[j]) {
                j += 1;
            }
            let w = text[i..j].to_ascii_lowercase();
            let tok = if SQL_LIT.contains(&w.as_str()) {
                Tok::Literal
            } else if SQL_KW.contains(&w.as_str()) {
                Tok::Keyword
            } else {
                Tok::Func
            };
            out.push((i, j, tok));
            i = j;
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// Makefile：目标、变量 `$(VAR)`、注释。
fn scan_makefile(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        let c = b[i];
        if line_start && c == b'#' {
            let end = find_line_end(b, i);
            out.push((i, end, Tok::Comment));
            i = end;
            continue;
        }
        if c == b'$' {
            let mut j = i + 1;
            if j < n && b[j] == b'(' {
                let mut depth = 0;
                while j < n {
                    if b[j] == b'(' {
                        depth += 1;
                    } else if b[j] == b')' {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    j += 1;
                }
            } else {
                while j < n && (is_ident_cont(b[j]) || b[j] == b'$') {
                    j += 1;
                }
            }
            if j > i + 1 {
                out.push((i, j, Tok::Var));
                i = j;
                line_start = false;
                continue;
            }
        }
        if line_start {
            // 目标：不含空格的 token 后跟 ':'
            if !c.is_ascii_whitespace() && c != b'\t' {
                let mut j = i;
                while j < n && b[j] != b':' && b[j] != b'=' && b[j] != b' ' && b[j] != b'\t' && b[j] != b'\n' {
                    j += 1;
                }
                if j < n && (b[j] == b':' || b[j] == b'=') && j > i {
                    out.push((i, j, Tok::Func));
                    i = j;
                    continue;
                }
            }
        }
        if c == b'"' || c == b'\'' {
            let end = scan_string(b, i, c, true, false);
            out.push((i, end, Tok::String));
            i = end;
            line_start = false;
            continue;
        }
        if c == b'\n' {
            line_start = true;
        } else if c != b' ' && c != b'\t' && c != b'\r' {
            line_start = false;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// 日志：时间戳、级别（INFO/WARN/ERROR…）。
fn scan_log(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        // 时间戳：2026-10-24 12:34:56 或 12:34:56
        if b[i].is_ascii_digit() {
            let mut j = i;
            let mut digits = 0;
            while j < n && (b[j].is_ascii_digit() || b[j] == b'-' || b[j] == b':' || b[j] == b'.'
                || b[j] == b'/' || b[j] == b'T' || b[j] == b'Z')
            {
                if b[j].is_ascii_digit() {
                    digits += 1;
                }
                j += 1;
            }
            if digits >= 4 && j > i {
                out.push((i, j, Tok::Meta));
                i = j;
                continue;
            }
            let end = scan_number(b, i);
            out.push((i, end, Tok::Number));
            i = end;
            continue;
        }
        if is_ident_start(b[i]) {
            let mut j = i;
            while j < n && (is_ident_cont(b[j]) || b[j] == b'-') {
                j += 1;
            }
            let w = text[i..j].to_ascii_uppercase();
            let tok = match w.as_str() {
                "ERROR" | "ERR" | "FATAL" | "PANIC" | "CRITICAL" | "CRIT" => Tok::Del,
                "WARN" | "WARNING" => Tok::Number,
                "INFO" | "NOTICE" | "DEBUG" | "TRACE" => Tok::Keyword,
                "OK" | "SUCCESS" => Tok::Literal,
                // 不是级别就当普通词，但**必须**把 i 往前挪，
                // 否则这里会原地打转（死循环）
                _ => {
                    i = j;
                    continue;
                }
            };
            out.push((i, j, tok));
            i = j;
            continue;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/// 差异（diff/patch）：文件头、hunk、增删行。
fn scan_diff(text: &str) -> Vec<Span> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    let mut pg = Progress::new();

    while pg.step(&mut i, n) {
        if line_start {
            let line_end = find_line_end(b, i);
            if b[i..].starts_with(b"+++") || b[i..].starts_with(b"---") {
                out.push((i, line_end, Tok::Meta));
                i = line_end;
                continue;
            }
            if b[i..].starts_with(b"@@") {
                out.push((i, line_end, Tok::Heading));
                i = line_end;
                continue;
            }
            if b[i] == b'+' {
                out.push((i, line_end, Tok::Add));
                i = line_end;
                continue;
            }
            if b[i] == b'-' {
                out.push((i, line_end, Tok::Del));
                i = line_end;
                continue;
            }
            if b[i] == b'd' && b[i..].starts_with(b"diff ") {
                out.push((i, line_end, Tok::Meta));
                i = line_end;
                continue;
            }
        }
        if b[i] == b'\n' {
            line_start = true;
        } else if b[i] != b'\r' {
            line_start = false;
        }
        i += 1;
    }
    to_char_spans(text, &out)
}

/* ---------------- 偏移换算 ---------------- */

/// 字节偏移 → 字符偏移。GtkTextBuffer 的 `iter_at_offset` 用的是字符数，
/// 直接拿字节偏移去定位会在含中文的文件里错位。
fn to_char_spans(text: &str, raw: &[(usize, usize, Tok)]) -> Vec<Span> {
    // starts[i] = 第 i 个字符的字节偏移；末尾补一个哨兵
    let mut starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    starts.push(text.len());
    let conv = |byte: usize| -> usize {
        // 找到最后一个 <= byte 的起点
        match starts.binary_search(&byte) {
            Ok(idx) => idx,
            Err(idx) => idx - 1,
        }
    };
    raw.iter()
        .map(|(s, e, t)| Span {
            start: conv(*s),
            end: conv(*e),
            tok: *t,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(text: &str, lang: Lang) -> Vec<(Tok, &str)> {
        let h = highlight(text, lang);
        h.spans
            .iter()
            .map(|s| {
                let chars: Vec<char> = text.chars().collect();
                let piece: String = chars[s.start..s.end].iter().collect();
                (s.tok, Box::leak(piece.into_boxed_str()) as &str)
            })
            .collect()
    }

    fn has(text: &str, lang: Lang, tok: Tok, needle: &str) -> bool {
        toks(text, lang)
            .iter()
            .any(|(t, p)| *t == tok && p.contains(needle))
    }

    #[test]
    fn 扩展名决定语言() {
        assert_eq!(Lang::detect("/tmp/a.rs", ""), Lang::Rust);
        assert_eq!(Lang::detect("/tmp/a.PY", ""), Lang::Python);
        assert_eq!(Lang::detect("/tmp/a.json", ""), Lang::Json);
        assert_eq!(Lang::detect("/tmp/Makefile", ""), Lang::Makefile);
        assert_eq!(Lang::detect("/tmp/a.unknownext", ""), Lang::Plain);
        assert_eq!(Lang::detect("/tmp/a", ""), Lang::Plain);
    }

    #[test]
    fn shebang兜底() {
        assert_eq!(Lang::detect("/tmp/run", "#!/bin/bash\n"), Lang::Shell);
        assert_eq!(Lang::detect("/tmp/run", "#!/usr/bin/env python3\n"), Lang::Python);
        assert_eq!(Lang::detect("/tmp/run", "#!/usr/bin/env node\n"), Lang::Js);
        assert_eq!(Lang::detect("/tmp/run", "hello\n"), Lang::Plain);
    }

    #[test]
    fn rust_关键字与字符串() {
        let src = "fn main() { let s = \"hi\"; }";
        assert!(has(src, Lang::Rust, Tok::Keyword, "fn"));
        assert!(has(src, Lang::Rust, Tok::Keyword, "let"));
        assert!(has(src, Lang::Rust, Tok::String, "hi"));
        assert!(has(src, Lang::Rust, Tok::Func, "main"));
    }

    #[test]
    fn rust_注释与生命周期不误判() {
        let src = "// 注释\nlet x = 1; // 尾注";
        assert!(has(src, Lang::Rust, Tok::Comment, "注释"));
        let src2 = "struct S<'a> { x: &'a str }";
        // 生命周期 'a 不该被当成字符串
        assert!(!toks(src2, Lang::Rust)
            .iter()
            .any(|(t, p)| *t == Tok::String && p.contains("'a")));
    }

    #[test]
    fn 块注释() {
        let src = "a /* b */ c";
        assert!(has(src, Lang::C, Tok::Comment, "b"));
    }

    #[test]
    fn python_三引号与装饰器() {
        let src = "@decorator\ndef f():\n    \"\"\"doc\n    more\"\"\"\n    return 1";
        assert!(has(src, Lang::Python, Tok::Meta, "@decorator"));
        assert!(has(src, Lang::Python, Tok::Keyword, "def"));
        assert!(has(src, Lang::Python, Tok::String, "doc"));
    }

    #[test]
    fn shell_变量与注释() {
        let src = "#!/bin/sh\nNAME=\"world\"\necho $NAME ${OTHER}";
        assert!(has(src, Lang::Shell, Tok::Comment, "#!/bin/sh"));
        assert!(has(src, Lang::Shell, Tok::Var, "$NAME"));
        assert!(has(src, Lang::Shell, Tok::Var, "${OTHER}"));
        assert!(has(src, Lang::Shell, Tok::String, "world"));
    }

    #[test]
    fn json_键与值区分() {
        let src = "{\"name\": \"pp\", \"n\": 12, \"ok\": true}";
        assert!(has(src, Lang::Json, Tok::Attr, "name"));
        assert!(has(src, Lang::Json, Tok::String, "pp"));
        assert!(has(src, Lang::Json, Tok::Number, "12"));
        assert!(has(src, Lang::Json, Tok::Literal, "true"));
    }

    #[test]
    fn yaml_键() {
        let src = "key: value # 注释\nlist:\n  - a";
        assert!(has(src, Lang::Yaml, Tok::Attr, "key"));
        assert!(has(src, Lang::Yaml, Tok::Comment, "注释"));
    }

    /// **文档分隔符** `---` / `...`（顶格）。
    #[test]
    fn yaml_文档分隔符() {
        let src = "---\nname: x\n...\n";
        assert!(has(src, Lang::Yaml, Tok::Heading, "---"));
        assert!(has(src, Lang::Yaml, Tok::Heading, "..."));
        // 缩进的就不是分隔符了
        let src2 = "  ---\n";
        assert!(!has(src2, Lang::Yaml, Tok::Heading, "---"));
    }

    /// **序列项**的行首 `-` 是标记，**`-5` 却是负数**（别搞混）。
    #[test]
    fn yaml_序列项与负数() {
        let src = "list:\n  - a\n  - -5\n";
        assert!(has(src, Lang::Yaml, Tok::Meta, "-"));
        // 负数整体当**一个**数
        let neg = toks(src, Lang::Yaml);
        assert!(
            neg.iter().any(|(t, p)| *t == Tok::Number && p.contains("-5")),
            "负数应整体着成数字：{neg:?}"
        );
    }

    /// **块标量** `|` / `>`：标记 + **整段内容**（内容当字符串）。
    #[test]
    fn yaml_块标量() {
        let src = "script: |\n  echo 一\n  echo 二\nnext: 1\n";
        assert!(has(src, Lang::Yaml, Tok::Meta, "|"));
        assert!(has(src, Lang::Yaml, Tok::String, "echo 一"));
        // 内容到下一个键为止 ⇒ `next` 还是键
        assert!(has(src, Lang::Yaml, Tok::Attr, "next"));
        // 块里的内容**不该**被当成键
        let ks: Vec<&str> = toks(src, Lang::Yaml)
            .iter()
            .filter(|(t, _)| *t == Tok::Attr)
            .map(|(_, p)| *p)
            .collect();
        assert!(!ks.iter().any(|k| k.contains("echo")), "块内容被当成键了：{ks:?}");
    }

    /// `|-` / `>-` 这类 chomping 标记也算块标量。
    #[test]
    fn yaml_块标量带标记() {
        let src = "a: >-\n  缩进的内容\nb: 2\n";
        assert!(has(src, Lang::Yaml, Tok::Meta, ">-"));
        assert!(has(src, Lang::Yaml, Tok::String, "缩进的内容"));
    }

    /// **标签** `!!str` / `!tag`。
    #[test]
    fn yaml_标签与锚点() {
        let src = "a: !!str 123\nb: &锚 1\nc: *锚\n";
        assert!(has(src, Lang::Yaml, Tok::Tag, "!!str"));
        assert!(has(src, Lang::Yaml, Tok::Meta, "&锚"));
        assert!(has(src, Lang::Yaml, Tok::Meta, "*锚"));
    }

    /// **日期/时间**当**一个**数（不拆成三段）。
    #[test]
    fn yaml_日期时间() {
        let src = "d: 2024-01-31\nt: 12:30:00\n";
        let ns: Vec<&str> = toks(src, Lang::Yaml)
            .iter()
            .filter(|(t, _)| *t == Tok::Number)
            .map(|(_, p)| *p)
            .collect();
        assert!(ns.contains(&"2024-01-31"), "日期应整体着成数字：{ns:?}");
        assert!(ns.contains(&"12:30:00"), "时间应整体着成数字：{ns:?}");
    }

    /// **合并键** `<<:` 算键；**空值** `~` 算字面量。
    #[test]
    fn yaml_合并键与空值() {
        let src = "base: &b\n  x: 1\nchild:\n  <<: *b\n  y: ~\n";
        assert!(has(src, Lang::Yaml, Tok::Attr, "<<"));
        assert!(has(src, Lang::Yaml, Tok::Literal, "~"));
    }

    #[test]
    fn ini_节与键() {
        let src = "[section]\nkey = value\n; 注释";
        assert!(has(src, Lang::Ini, Tok::Heading, "[section]"));
        assert!(has(src, Lang::Ini, Tok::Attr, "key"));
        assert!(has(src, Lang::Ini, Tok::Comment, "; 注释"));
    }

    #[test]
    fn markdown_标题与行内代码() {
        let src = "# 标题\n\n正文 `code` 与 **粗体**\n";
        assert!(has(src, Lang::Markdown, Tok::Heading, "标题"));
        assert!(has(src, Lang::Markdown, Tok::String, "code"));
        assert!(has(src, Lang::Markdown, Tok::Strong, "粗体"));
    }

    #[test]
    fn html_标签与属性() {
        let src = "<a href=\"x.html\">文字</a>";
        assert!(has(src, Lang::Html, Tok::Tag, "a"));
        assert!(has(src, Lang::Html, Tok::Attr, "href"));
        assert!(has(src, Lang::Html, Tok::String, "x.html"));
    }

    #[test]
    fn diff_增删() {
        let src = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        assert!(has(src, Lang::Diff, Tok::Meta, "--- a/x"));
        assert!(has(src, Lang::Diff, Tok::Del, "-old"));
        assert!(has(src, Lang::Diff, Tok::Add, "+new"));
    }

    #[test]
    fn 日志_时间戳与级别() {
        let src = "2026-10-24 12:00:00 INFO 启动成功\n";
        assert!(has(src, Lang::Log, Tok::Meta, "2026-10-24"));
        assert!(has(src, Lang::Log, Tok::Keyword, "INFO"));
    }

    #[test]
    fn 中文文件偏移不错位() {
        // 第二行有中文：字节偏移远大于字符偏移
        let src = "// 中文注释\nlet x = 1;\n";
        let h = highlight(src, Lang::Rust);
        for s in &h.spans {
            assert!(s.start <= s.end);
            let chars: Vec<char> = src.chars().collect();
            assert!(s.end <= chars.len(), "片段越界: {s:?}");
        }
        // 关键字 "let" 应该在正确的字符位置上
        let chars: Vec<char> = src.chars().collect();
        let pos: String = chars[8..11].iter().collect();
        assert_eq!(pos, "let");
    }

    #[test]
    fn sql_关键字大小写不敏感() {
        let src = "SELECT * FROM t WHERE a = 'x';";
        assert!(has(src, Lang::Sql, Tok::Keyword, "SELECT"));
        assert!(has(src, Lang::Sql, Tok::Keyword, "WHERE"));
        assert!(has(src, Lang::Sql, Tok::String, "x"));
    }

    #[test]
    fn makefile_变量与目标() {
        let src = "CC = gcc\nall: $(CC) main.o\n";
        assert!(has(src, Lang::Makefile, Tok::Var, "$(CC)"));
        assert!(has(src, Lang::Makefile, Tok::Func, "all"));
    }

    #[test]
    fn typescript_多出来的关键字() {
        // interface / enum 这些是 TS 独有的，JS 里不该有
        let src = "interface Foo { x: number }";
        assert!(has(src, Lang::Ts, Tok::Keyword, "interface"));
        assert!(!has(src, Lang::Js, Tok::Keyword, "interface"));
    }

    #[test]
    fn 纯文本没有片段() {
        let h = highlight("随便写点什么\n", Lang::Plain);
        assert!(h.spans.is_empty());
    }

    /* ---------------- 回归：代码围栏（2026-10-26 现场冻结） ---------------- */

    #[test]
    fn markdown_代码围栏() {
        // 围栏分支原来把游标停在 '\n' 上（i = line_end），下一轮
        // find_line_end 返回同一个下标 → 原地打转 → 无限循环，
        // 整个窗口冻住（办公机现场 strace 实锤）。
        let src = "```rust\nfn main() {}\n```\n";
        let h = highlight(src, Lang::Markdown);
        assert_eq!(h.stalls, 0, "围栏分支又原地打转了");
        // 围栏行本身是 Meta，围栏里的内容不着一色
        assert!(has(src, Lang::Markdown, Tok::Meta, "```"));
        assert!(!has(src, Lang::Markdown, Tok::Keyword, "fn"));
    }

    #[test]
    fn markdown_围栏未闭合也不卡() {
        // 没闭合的围栏更常见（用户正敲到一半），同样不能卡
        let src = "```\nlet x = 1;\n";
        let h = highlight(src, Lang::Markdown);
        assert_eq!(h.stalls, 0);
    }

    #[test]
    fn markdown_波浪号围栏() {
        let src = "~~~sh\nls -al\n~~~\n";
        let h = highlight(src, Lang::Markdown);
        assert_eq!(h.stalls, 0);
        assert!(has(src, Lang::Markdown, Tok::Meta, "~~~"));
    }

    #[test]
    fn markdown_标题后接围栏() {
        // 标题分支也是"吃掉整行"，和围栏分支挨着，别互相带偏
        let src = "# 标题\n\n```py\nprint(1)\n```\n";
        let h = highlight(src, Lang::Markdown);
        assert_eq!(h.stalls, 0);
        assert!(has(src, Lang::Markdown, Tok::Heading, "标题"));
        assert!(has(src, Lang::Markdown, Tok::Meta, "```"));
    }

    /* ---------------- 进度守卫 ---------------- */

    #[test]
    fn 进度守卫_原地不动就强制推进() {
        let mut pg = Progress::new();
        let mut i = 0usize;
        let n = 10usize;
        assert!(pg.step(&mut i, n));
        assert_eq!(i, 0);
        // 模拟"忘了推进游标"：i 原封不动
        assert!(pg.step(&mut i, n));
        assert_eq!(i, 1, "守卫应当强制推进一格");
        assert_eq!(stall_count(), 1);
    }

    #[test]
    fn 进度守卫_推进过界就收工() {
        let mut pg = Progress::new();
        let mut i = 9usize;
        let n = 10usize;
        assert!(pg.step(&mut i, n));
        // 又一轮原地不动 → 强制推进到 n → 扫完
        assert!(!pg.step(&mut i, n));
        assert_eq!(i, 10);
    }

    /// 语料：各种"容易让扫描器不推进游标"的输入。
    ///
    /// 断言：**一次都不许原地打转**。以前这类 bug 只会在线上表现为
    /// "窗口冻住"，现在它在单元测试里就红。
    #[test]
    fn 扫描器一律不原地打转() {
        let 语料: &[(Lang, &str)] = &[
            (Lang::Markdown, "```rust\nfn main() {}\n```\n"),
            (Lang::Markdown, "```\n未闭合的围栏\n"),
            (Lang::Markdown, "~~~\ntilde\n~~~\n"),
            (Lang::Markdown, "# 标题\n```\n```\n"),
            (Lang::Markdown, "> 引用\n```x\ny\n```\n"),
            (Lang::Rust, "r#\"未闭合的原始字符串\n"),
            (Lang::Rust, "r\"\"\"三引号\n"),
            (Lang::Rust, "/* 块注释没闭合\nfn x() {}\n"),
            (Lang::Python, "'''三引号没闭合\n"),
            (Lang::Python, "'''闭合了'''\n"),
            (Lang::C, "'未闭合的字符字面量\n"),
            (Lang::C, "\"字符串没闭合\n"),
            (Lang::Shell, "echo \"$UNCLOSED\n"),
            (Lang::Shell, "echo `未闭合的反引号\n"),
            (Lang::Shell, "${VAR}\n"),
            (Lang::Html, "<div class=\"x\n"),
            (Lang::Html, "<!-- 注释没闭合\n"),
            (Lang::Css, "/* 注释没闭合\n"),
            (Lang::Css, "a { b: \"c\n"),
            (Lang::Sql, "SELECT '没闭合\n"),
            (Lang::Sql, "/* 没闭合\n"),
            (Lang::Json, "{\"k\": \"没闭合\n"),
            (Lang::Yaml, "k: \"没闭合\n"),
            (Lang::Yaml, "# 注释\nk: v\n"),
            (Lang::Toml, "k = \"没闭合\n"),
            (Lang::Ini, "[节]\n键 = 值\n"),
            (Lang::Makefile, "CC = $(UNCLOSED\n"),
            (Lang::Makefile, "all: dep\n"),
            (Lang::Log, "2026-10-26 12:00:00 INFO 随便一条\n"),
            (Lang::Diff, "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"),
            (Lang::Diff, "@@ -1 +1 @@\n"),
            (Lang::Plain, "什么都不特殊\n"),
        ];
        for (lang, text) in 语料 {
            let h = highlight(text, *lang);
            assert_eq!(h.stalls, 0, "{lang:?} 的扫描器原地打转了：{text:?}");
            // 片段不能越界
            let chars = text.chars().count();
            for s in &h.spans {
                assert!(s.start <= s.end && s.end <= chars, "{lang:?} 片段越界: {s:?}");
            }
        }
    }
}
