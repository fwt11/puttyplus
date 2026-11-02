//! 应用主题与终端配色（4.1）。
//!
//! **两个独立维度**，不搅在一起：
//! - 应用主题（界面）：深色 / 浅色 / 高对比，另加"跟随系统"
//! - 终端配色：只管终端的前景/背景/16 色，与界面主题各选各的
//!
//! 为什么界面主题只能自己写 CSS：`gtk4-rs 0.11` **没有** `StyleManager`
//! 绑定，也就拿不到 `gtk_style_manager_set_color_scheme()`。想切暗色只能
//! 自己注入 CssProvider 覆盖控件样式——这也正是"高对比"这种非系统预设
//! 唯一可行的实现路径。
//!
//! 注入用 `gtk_style_context_add_provider_for_display`（GTK4 里
//! `gtk_style_context_add_provider_for_entire_application` 只是它的新名字，
//! Rust 绑定只暴露了旧的这个）。优先级取 USER：比应用默认样式高，
//! 又不至于盖掉用户自己写进 `~/.config/gtk-4.0/gtk.css` 的东西。
//!
//! 能力边界：这里改的是本程序控件的配色，**不会**改变系统主题，
//! 也不会去动别的程序。另外 GTK 对 `TreeView` 这类老控件的样式名
//! （`.view` / `.treeview.view`）版本间有差异，CSS 里对二者都写了规则。

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

/// 应用主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// 不注入任何 CSS：完全跟随系统 GTK 主题（默认）
    #[default]
    System,
    Dark,
    Light,
    HighContrast,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::System, Theme::Dark, Theme::Light, Theme::HighContrast];

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "跟随系统",
            Theme::Dark => "深色",
            Theme::Light => "浅色",
            Theme::HighContrast => "高对比",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Theme::System => "不覆盖系统 GTK 主题",
            Theme::Dark => "暗色底，夜间/暗光环境",
            Theme::Light => "亮色底，白天/投影",
            Theme::HighContrast => "高对比（黑底白字），投影与弱视场景",
        }
    }

    pub fn parse(s: &str) -> Theme {
        match s {
            "dark" => Theme::Dark,
            "light" => Theme::Light,
            "contrast" => Theme::HighContrast,
            _ => Theme::System,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::HighContrast => "contrast",
        }
    }

    /// 主题的**原始** CSS：末尾还是 `BASE_PLACEHOLDER`，必须经 `css_with`
    /// 拼上骨架后才能交给 GTK。对外一律用 [`css_text`]——`apply` 曾经直接
    /// 把这份原始串塞给 `load_from_string`，结果只加载了 `@define-color`，
    /// 整套组件规则（窗口底色、选中态、滚动条…）从未生效，而单测比对的是
    /// 拼好的 `css_text`，所以一直是绿的。
    fn css(self) -> Option<&'static str> {
        match self {
            Theme::System => None,
            Theme::Dark => Some(DARK_CSS),
            Theme::Light => Some(LIGHT_CSS),
            Theme::HighContrast => Some(CONTRAST_CSS),
        }
    }
}

/// 状态语义（连接中 / 已连接 / 失败 / 危险）。
///
/// 颜色**跟着主题走**：业务代码不再写死 `#2ecc71` 之类的 hex，否则切明暗或
/// 高对比主题后，状态点永远是同一批颜色（低对比、与整体不协调）。
/// CSS 里的 `pp_ok/pp_warn/pp_err` 与这里保持同一组值；`state_hex` 供
/// Pango markup 与 `CellRendererText.foreground`（只吃颜色字符串）使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Warn,
    Err,
}

impl State {
    pub const ALL: [State; 3] = [State::Ok, State::Warn, State::Err];
}

/// 某主题下某个状态的颜色（纯函数，便于单测）。
pub fn state_hex_for(t: Theme, s: State) -> &'static str {
    match (t, s) {
        // 浅色：压白底要够深
        (Theme::Light, State::Ok) => "#1a7f4b",
        (Theme::Light, State::Warn) => "#8a5a00",
        (Theme::Light, State::Err) => "#c0392b",
        // 高对比：纯黑底上用高亮色
        (Theme::HighContrast, State::Ok) => "#00ff88",
        (Theme::HighContrast, State::Warn) => "#ffd400",
        (Theme::HighContrast, State::Err) => "#ff5555",
        // 深色与「跟随系统」：系统底色不可知，取一组在明暗底上都站得住的
        // 中间色（非文字指示器，只需满足 3:1）
        (_, State::Ok) => "#2fa86b",
        (_, State::Warn) => "#c98a1f",
        (_, State::Err) => "#d9534f",
    }
}

/// 当前主题下的状态色（走缓存，不读磁盘）。
pub fn state_hex(s: State) -> &'static str {
    state_hex_for(current_theme(), s)
}

/* ---------------- 颜色对比度（WCAG 2.x） ---------------- */

/// WCAG AA **正文**的对比度门槛。
pub const CONTRAST_AA: f64 = 4.5;

/// `#rrggbb` → `(r, g, b)`（0..=255）。认不出来返回 `None`。
fn parse_hex_rgb(h: &str) -> Option<(f64, f64, f64)> {
    let h = h.trim();
    let h = h.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r as f64, g as f64, b as f64))
}

/// WCAG 相对亮度。
fn rel_luminance(rgb: (f64, f64, f64)) -> f64 {
    let f = |c: f64| {
        let c = c / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(rgb.0) + 0.7152 * f(rgb.1) + 0.0722 * f(rgb.2)
}

/// WCAG 对比度（1.0 ~ 21.0）。
///
/// **为什么自己算**：GTK4 的 `gdk::RGBA` 没有对比度接口，而审计（阶段 73 /
/// P1-10）要求"四套主题各加对比度断言" ⇒ 得有个**能单测的纯函数**（单测里
/// 没有显示服务，问不了系统主题）。
pub fn contrast_ratio(a: &str, b: &str) -> f64 {
    let (Some(x), Some(y)) = (parse_hex_rgb(a), parse_hex_rgb(b)) else {
        return 0.0;
    };
    let (l1, l2) = (rel_luminance(x), rel_luminance(y));
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// 把前景色往背景色混（`t` = 往背景混的比例：0 = 原色，1 = 全背景）。
fn mix_hex(fg: &str, bg: &str, t: f64) -> String {
    let (Some(f), Some(b)) = (parse_hex_rgb(fg), parse_hex_rgb(bg)) else {
        return fg.to_string();
    };
    let c = |f: f64, b: f64| (f + (b - f) * t).round().clamp(0.0, 255.0) as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        c(f.0, b.0),
        c(f.1, b.1),
        c(f.2, b.2)
    )
}

/// 给「跟随系统」算一个**读得清**的次要文字色。
///
/// ⚠️ **不能直接用系统的 `insensitive_fg_color`**（阶段 73 / 审计 P1-10）：
/// 那个色是"控件**禁用**时"用的，Adwaita 浅色下大约 `#8e8e8e` ⇒ 白底上只有
/// **3.03:1**，低于 WCAG AA 正文的 4.5:1 ⇒ 表单标签、欢迎页快捷键、空态
/// 副标题这些**要读的文字**全都偏淡（实测截图里说明文字最深也就 `#8e8e8e`）。
///
/// 现在改成：从正文色出发往背景色混，混到**对比度还够 4.5:1** 为止 ⇒
/// 依然是"淡"的（比正文淡），但读得清。文字可能落在窗口底色（bg）或输入框
/// 底色（base）上 ⇒ **两个底色都要满足**（取更差的那个）。
///
/// 正文色本身都不够 4.5:1 时（极怪的主题）就退回正文色——那已经是最优解。
pub fn dim_for(fg: &str, bgs: &[&str]) -> String {
    let worst = |c: &str| -> f64 {
        bgs.iter()
            .map(|b| contrast_ratio(c, b))
            .fold(f64::INFINITY, f64::min)
    };
    if bgs.is_empty() || worst(fg) < CONTRAST_AA {
        return fg.to_string();
    }
    // 二分找**最大的**混合比例（混得越狠越淡，但要留得住 4.5:1）
    let bg = bgs[0];
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut best = fg.to_string();
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        let cand = mix_hex(fg, bg, mid);
        if worst(&cand) >= CONTRAST_AA {
            best = cand;
            lo = mid;
        } else {
            hi = mid;
        }
    }
    best
}

/// 当前主题的次要文字色（CSS 里的 `pp_dim`）。
///
/// **用途**：冒烟/单测要能拿到"这个主题下说明文字到底是哪个色"来量对比度。
pub fn dim_hex() -> String {
    match current_theme() {
        Theme::Dark => "#9aa0a6".into(),
        Theme::Light => "#5c636a".into(),
        Theme::HighContrast => "#ffffff".into(),
        Theme::System => system_dim().unwrap_or_else(|| "#5c636a".into()),
    }
}

/// 当前主题的底色（CSS 里的 `pp_bg`）。
pub fn bg_hex() -> String {
    match current_theme() {
        Theme::Dark => "#1e2124".into(),
        Theme::Light => "#f3f4f6".into(),
        Theme::HighContrast => "#000000".into(),
        Theme::System => system_bg().unwrap_or_else(|| "#f3f4f6".into()),
    }
}

/// 次要文字色（行内地址、说明文字用的 markup 颜色）。
///
/// Pango markup 只吃颜色字符串，所以这里得把主题令牌"翻译"成 hex；
/// 「跟随系统」时按 [`dim_for`] 现算一个**读得清**的（不是系统的禁用色）。
pub fn muted_hex() -> String {
    dim_hex()
}

/// 当前主题的正文色（CSS 里的 `pp_fg`）。
///
/// **用途**：行内 markup 的**主标题**要显式上色。
/// ⚠️ 阶段 91a 的教训：行里的文字不能靠 GTK 的默认色——某些系统主题下
/// 那个默认色是白的（白底白字，字看着"没了"）。
pub fn fg_hex() -> String {
    match current_theme() {
        Theme::Dark => "#dcdfe4".into(),
        Theme::Light => "#1f2328".into(),
        Theme::HighContrast => "#ffffff".into(),
        Theme::System => system_fg_bg()
            .map(|(fg, _, _)| fg)
            .unwrap_or_else(|| "#1f2328".into()),
    }
}

/// 计数徽章的底色（CSS 里的 `pp_chip`）。
///
/// ⚠️ **徽章上的字用正文色，不用次要色**：徽章底色是"正文色往底色混
/// 0.86"来的（比底色深/浅一档），次要色压在这种底上只有 ~4.1:1
/// （低于 WCAG AA 的 4.5:1）⇒ 徽章里的数字读不清。正文色压上去
/// 恒 ≥ 4.5:1（底色夹在正文色与底色之间）。
pub fn chip_bg_hex() -> String {
    match current_theme() {
        Theme::Dark => "#33383f".into(),
        Theme::Light => "#e4e8ec".into(),
        Theme::HighContrast => "#333333".into(),
        Theme::System => system_fg_bg()
            .map(|(fg, bg, _)| mix_hex(&fg, &bg, 0.86))
            .unwrap_or_else(|| "#e4e8ec".into()),
    }
}

fn system_fg_bg() -> Option<(String, String, String)> {
    if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
        return None;
    }
    let probe = gtk::Label::new(None);
    let sc = probe.style_context();
    let hex = |n: &str| sc.lookup_color(n).map(|c| rgba_hex(&c));
    let bg = hex("theme_bg_color")?;
    let fg = hex("theme_fg_color")?;
    let base = hex("theme_base_color").unwrap_or_else(|| bg.clone());
    Some((fg, bg, base))
}

fn system_bg() -> Option<String> {
    system_fg_bg().map(|(_, bg, _)| bg)
}

/// 「跟随系统」主题下的次要文字色（≥ 4.5:1）。
fn system_dim() -> Option<String> {
    let (fg, bg, base) = system_fg_bg()?;
    Some(dim_for(&fg, &[&bg, &base]))
}

/// 主题切换后让侧边栏重画行内灰字。
pub(crate) fn refresh_sidebar() {
    crate::sidebar::refresh_theme();
}

/* ---------------- 语法高亮配色 ---------------- */

/// 语法高亮用的一组颜色。
///
/// **为什么单独一份而不是走 CSS**：GTK4 的 `GtkTextTag` 只认字面颜色
/// （`foreground` 属性收字符串），没有 CSS class——没法像其它控件那样
/// 写成 `.pp-tok-keyword { color: … }` 让主题自动接管。所以颜色在这里
/// 按主题算好，建 tag 时写死；切主题时重建一遍 tag 即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynColors {
    /// 不着色的普通文字（用正文色）
    pub plain: &'static str,
    pub keyword: &'static str,
    pub string: &'static str,
    pub comment: &'static str,
    pub number: &'static str,
    pub type_: &'static str,
    pub literal: &'static str,
    pub var: &'static str,
    pub func: &'static str,
    pub meta: &'static str,
    pub tag: &'static str,
    pub attr: &'static str,
    pub heading: &'static str,
    pub strong: &'static str,
    pub emph: &'static str,
    pub add: &'static str,
    pub del: &'static str,
}

/// 深色底（One Dark 味）。
const SYN_DARK: SynColors = SynColors {
    plain: "#dcdfe4",
    keyword: "#c678dd",
    string: "#98c379",
    comment: "#7f848e",
    number: "#d19a66",
    type_: "#56b6c2",
    literal: "#e5c07b",
    var: "#e06c75",
    func: "#61afef",
    meta: "#c678dd",
    tag: "#e06c75",
    attr: "#d19a66",
    heading: "#61afef",
    strong: "#e5c07b",
    emph: "#c678dd",
    add: "#98c379",
    del: "#e06c75",
};

/// 浅色底（One Light 味）：颜色要压得够深，白底上才看得清。
const SYN_LIGHT: SynColors = SynColors {
    plain: "#1f2328",
    keyword: "#a626a4",
    string: "#50a14f",
    comment: "#9d9d9f",
    number: "#986801",
    type_: "#0184bc",
    literal: "#c18401",
    var: "#e45649",
    func: "#4078f2",
    meta: "#a626a4",
    tag: "#e45649",
    attr: "#986801",
    heading: "#4078f2",
    strong: "#c18401",
    emph: "#a626a4",
    add: "#50a14f",
    del: "#e45649",
};

/// 高对比主题（纯黑底）：全部用高饱和亮色。
const SYN_CONTRAST: SynColors = SynColors {
    plain: "#ffffff",
    keyword: "#ff7bff",
    string: "#55ff55",
    comment: "#b0b0b0",
    number: "#ffb86c",
    type_: "#66d9ef",
    literal: "#ffff55",
    var: "#ff79c6",
    func: "#8be9fd",
    meta: "#ff7bff",
    tag: "#ff79c6",
    attr: "#ffb86c",
    heading: "#8be9fd",
    strong: "#ffff55",
    emph: "#ff7bff",
    add: "#55ff55",
    del: "#ff79c6",
};

impl SynColors {
    /// 某个词法类别用哪种颜色。
    pub fn of(&self, tok: crate::panel::syntax::Tok) -> &'static str {
        use crate::panel::syntax::Tok;
        match tok {
            Tok::Plain => self.plain,
            Tok::Comment => self.comment,
            Tok::String => self.string,
            Tok::Number => self.number,
            Tok::Keyword => self.keyword,
            Tok::Type => self.type_,
            Tok::Literal => self.literal,
            Tok::Var => self.var,
            Tok::Func => self.func,
            Tok::Meta => self.meta,
            Tok::Tag => self.tag,
            Tok::Attr => self.attr,
            Tok::Heading => self.heading,
            Tok::Strong => self.strong,
            Tok::Emph => self.emph,
            Tok::Add => self.add,
            Tok::Del => self.del,
        }
    }
}

/// 某套主题下的语法高亮配色（纯函数，便于单测）。
pub fn syn_colors_for(t: Theme) -> SynColors {    match t {
        Theme::Dark => SYN_DARK,
        Theme::Light => SYN_LIGHT,
        Theme::HighContrast => SYN_CONTRAST,
        // 「跟随系统」的底色不可知：问系统主题要 base 色，亮底用浅色配色、
        // 暗底用深色配色；问不到就按亮底（Adwaita 默认是亮底）。
        Theme::System => {
            if system_base_is_dark() {
                SYN_DARK
            } else {
                SYN_LIGHT
            }
        }
    }
}

/// 当前主题下的语法高亮配色。
pub fn syn_colors() -> SynColors {
    syn_colors_for(current_theme())
}

/// 当前主题下面板/行号栏的底色（`#rrggbb`）。
///
/// 与 CSS 里的 `@pp_panel` **保持一致**：行号栏的"影子字符"要画成这个
/// 颜色才隐形——GTK4 不吃前景色的 alpha（实测 alpha=0 的字照样画出来），
/// 只能用同色盖住。
pub fn panel_hex() -> String {
    match current_theme() {
        Theme::Dark => "#26292e".into(),
        Theme::Light => "#ffffff".into(),
        Theme::HighContrast => "#000000".into(),
        Theme::System => system_base_hex().unwrap_or_else(|| "#ffffff".into()),
    }
}

/// 「跟随系统」时系统主题的 base 色（CSS 里 `@pp_panel` 就是它）。
fn system_base_hex() -> Option<String> {
    if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
        return None;
    }
    let probe = gtk::Label::new(None);
    let sc = probe.style_context();
    sc.lookup_color("theme_base_color")
        .or_else(|| sc.lookup_color("theme_bg_color"))
        .map(|c| rgba_hex(&c))
}

/// 「跟随系统」时判断系统底色是亮是暗。
fn system_base_is_dark() -> bool {
    if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
        return false;
    }
    let probe = gtk::Label::new(None);
    let Some(c) = probe.style_context().lookup_color("theme_base_color") else {
        return false;
    };
    // 相对亮度：0.5 以下算暗底
    let lum = 0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue();
    lum < 0.5
}

/// 终端配色方案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Palette {
    /// 项目一直在用的深色（默认）
    #[default]
    Dark,
    Light,
    Solarized,
    HighContrast,
}

impl Palette {
    pub const ALL: [Palette; 4] = [
        Palette::Dark,
        Palette::Light,
        Palette::Solarized,
        Palette::HighContrast,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Palette::Dark => "深色（默认）",
            Palette::Light => "浅色",
            Palette::Solarized => "Solarized",
            Palette::HighContrast => "高对比",
        }
    }

    pub fn parse(s: &str) -> Palette {
        match s {
            "light" => Palette::Light,
            "solarized" => Palette::Solarized,
            "contrast" => Palette::HighContrast,
            _ => Palette::Dark,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Palette::Dark => "dark",
            Palette::Light => "light",
            Palette::Solarized => "solarized",
            Palette::HighContrast => "contrast",
        }
    }

    /// (前景, 背景, 16 色 ANSI)
    pub fn colors(self) -> (&'static str, &'static str, [&'static str; 16]) {
        match self {
            Palette::Dark => (
                "#d9ded3",
                "#1c1f21",
                [
                    "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd",
                    "#e5e5e5", "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6",
                    "#29b8db", "#ffffff",
                ],
            ),
            Palette::Light => (
                "#2e3436",
                "#fbfbfb",
                [
                    "#000000", "#c01c28", "#26a269", "#a2734c", "#12488b", "#a347ba", "#2aa1b3",
                    "#d3d7cf", "#555753", "#f66151", "#33d17a", "#e9ad0c", "#2a7bde", "#c061cb",
                    "#33c7de", "#eeeeec",
                ],
            ),
            Palette::Solarized => (
                "#93a1a1",
                "#002b36",
                [
                    "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
                    "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4",
                    "#93a1a1", "#fdf6e3",
                ],
            ),
            Palette::HighContrast => (
                "#ffffff",
                "#000000",
                [
                    "#000000", "#ff5555", "#55ff55", "#ffff55", "#5555ff", "#ff55ff", "#55ffff",
                    "#ffffff", "#808080", "#ff8080", "#80ff80", "#ffff80", "#8080ff", "#ff80ff",
                    "#80ffff", "#ffffff",
                ],
            ),
        }
    }
}

/* ---------------- 应用 ---------------- */

thread_local! {
    /// 当前注入的 provider：切主题时先摘旧的再挂新的。
    /// GTK 允许多个 provider 叠加，不摘的话两套 CSS 会打架。
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };

    /// 当前生效的主题。`state_hex` 会被传输进度刷新（每秒多次）调用，
    /// 每次去读 settings.ini 是不可接受的；由 `apply` 负责维护。
    static CURRENT: std::cell::Cell<Theme> = const { std::cell::Cell::new(Theme::System) };
}

/// 当前生效的主题（不读磁盘）。
pub fn current_theme() -> Theme {
    CURRENT.with(|c| c.get())
}

/// 当前主题下的注册键，供 settings 使用（读磁盘，仅在切换时用）。
fn saved_theme() -> Theme {
    Theme::parse(&crate::settings::get_str("ui", "theme"))
}

use std::cell::RefCell;

/// 把主题装到当前 display 上。`Theme::System` = 摘掉 provider，还原成系统样式。
///
/// 返回是否真的装上了 CSS。`Theme::System` 依赖系统主题的语义色，某些平台
/// （Wayland / 设置门户）在首帧时主题还没解析出来，`system_tokens()` 会拿到
/// 空 → 整份 CSS 被跳过。调用方据此做短退避重试。
pub fn apply(t: Theme) -> bool {
    CURRENT.with(|c| c.set(t));
    let mut ok = false;
    // 无显示（不该发生）时静默跳过：总比 panic 好
    if let Some(display) = gdk::Display::default() {
        let old = PROVIDER.with(|p| p.borrow_mut().take());
        if let Some(p) = old {
            gtk::StyleContext::remove_provider_for_display(&display, &p);
        }
        // 必须用拼好骨架的 css_text：t.css() 里还是 BASE_PLACEHOLDER
        if let Some(css) = css_text(t) {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&css);
            gtk::StyleContext::add_provider_for_display(&display, &provider, PRIORITY_USER);
            PROVIDER.with(|p| *p.borrow_mut() = Some(provider));
            ok = true;
        }
    }
    // 状态点、队列失败色走的是 state_hex（markup / CellRenderer 只吃颜色字符串，
    // 不经过 CSS）。主题一换必须重刷，否则已连接状态还留着上一套主题的颜色。
    crate::tabs::refresh_theme_colors();
    // 侧边栏行内地址用的是 markup 灰字，同理
    refresh_sidebar();
    ok
}

/// 套用主题；系统主题尚未就绪时按短退避补几次。
///
/// 现场：首次启动（主题=跟随系统）滚动条等组件样式全部回落成系统默认，
/// 手动切一次主题再切回来就正常——说明首次 `apply` 太早，不是选择器问题。
fn apply_eventually(t: Theme, attempt: u32) {
    // 用户在这期间切了主题就不再补旧主题
    if theme() != t {
        return;
    }
    if apply(t) {
        return;
    }
    const DELAYS: [u64; 5] = [50, 120, 250, 500, 1000];
    if attempt as usize >= DELAYS.len() {
        return;
    }
    let delay = DELAYS[attempt as usize];
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        apply_eventually(t, attempt + 1);
    });
}

/// USER 优先级：盖过 Adwaita 的默认样式，但让位于用户自己写的 gtk.css。
const PRIORITY_USER: u32 = 800;

/// 启动时套用已保存的主题/配色。
pub fn load_saved() {
    apply_eventually(theme(), 0);
    crate::tabs::sync_term_palettes();
}

pub fn theme() -> Theme {
    saved_theme()
}

pub fn set_theme(t: Theme) {
    crate::settings::set_str("ui", "theme", t.key());
    // 切到"跟随系统"时系统主题也可能尚未就绪：同样走带重试的入口
    apply_eventually(t, 0);
}

pub fn palette() -> Palette {
    Palette::parse(&crate::settings::get_str("ui", "term_palette"))
}

pub fn set_palette(p: Palette) {
    crate::settings::set_str("ui", "term_palette", p.key());
    crate::tabs::sync_term_palettes();
}

/* ---------------- CSS ---------------- */

/// `gdk::RGBA` → `#rrggbb`（CSS 里只用到不透明色）。
pub fn rgba_hex(c: &gdk::RGBA) -> String {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", f(c.red()), f(c.green()), f(c.blue()))
}

/// 「跟随系统」时从当前 GTK 主题里读一组语义色，供**修复规则**使用。
///
/// 为什么要读而不是不注入：不注入的话，选中行仍是 Adwaita 的整行 accent 实底
/// （行内说明文字看不清）、滚动条拖块仍是主文字色。可我们又不想改用户主题的
/// 配色——所以只取它自己的颜色名，派生几个中间色（mix），再套一层小的修复
/// 规则。拿不到颜色名（非 Adwaita 系主题）时返回 None，退回"完全不动"。
fn system_tokens() -> Option<String> {
    // 构造控件前必须先有 GTK 与显示：单测在没有显示的环境里也会调到这里
    if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
        return None;
    }
    let probe = gtk::Label::new(None);
    let sc = probe.style_context();
    let hex = |n: &str| sc.lookup_color(n).map(|c| rgba_hex(&c));

    let bg = hex("theme_bg_color")?;
    let fg = hex("theme_fg_color")?;
    let base = hex("theme_base_color").unwrap_or_else(|| bg.clone());
    let border = hex("borders")
        .or_else(|| hex("unfocused_borders"))
        .or_else(|| hex("theme_base_color"))?;
    let accent = hex("theme_selected_bg_color")
        .or_else(|| hex("accent_bg_color"))
        .or_else(|| hex("theme_fg_color"))?;
    let on_accent = hex("theme_selected_fg_color")
        .or_else(|| hex("accent_fg_color"))
        .unwrap_or_else(|| "#ffffff".into());
    // ⚠️ **次要文字色不要用系统的 `insensitive_fg_color`**（阶段 73 / 审计
    // P1-10）：那是"控件**禁用**时"用的色，Adwaita 浅色下约 `#8e8e8e` ⇒
    // 白底上只有 3.03:1，低于 WCAG AA 正文的 4.5:1 ⇒ 表单标签、欢迎页
    // 快捷键、空态副标题这些**要读**的文字全都偏淡。
    // 现在按 [`dim_for`] 现算：从正文色往底色混，混到对比度还够 4.5:1 为止
    //（窗口底色与输入框底色两个都要满足）。
    let dim = dim_for(&fg, &[&bg, &base]);
    // GTK 默认主题有 success/warning/error 三个语义色；缺失时用一组
    // 在明暗底上都站得住的颜色兜底
    let ok = hex("success_color").unwrap_or_else(|| "#2e9e63".into());
    let warn = hex("warning_color").unwrap_or_else(|| "#b07d1a".into());
    let err = hex("error_color").unwrap_or_else(|| "#c0392b".into());

    Some(format!(
        "@define-color pp_bg {bg};\n\
         @define-color pp_panel {base};\n\
         @define-color pp_fg {fg};\n\
         @define-color pp_dim {dim};\n\
         @define-color pp_border {border};\n\
         @define-color pp_accent {accent};\n\
         @define-color pp_on_accent {on_accent};\n\
         @define-color pp_ok {ok};\n\
         @define-color pp_warn {warn};\n\
         @define-color pp_err {err};\n\
         @define-color pp_hover mix(@pp_fg, @pp_bg, 0.92);\n\
         @define-color pp_accent_soft mix(@pp_accent, @pp_bg, 0.86);\n\
         @define-color pp_scroll mix(@pp_fg, @pp_bg, 0.72);\n\
         @define-color pp_chip mix(@pp_fg, @pp_bg, 0.86);\n\
         @define-color pp_dim_sel @pp_fg;\n"
    ))
}

/// 「跟随系统」用的**修复规则**：只改对比度真正有问题的几处，不动按钮/输入框
/// 这些已经由系统主题画好的东西。
const FIX: &str = r#"
list > row:hover, listbox > row:hover { background-color: @pp_hover; }
list > row:selected, listbox > row:selected {
  background-color: @pp_accent_soft;
  color: @pp_fg;
}
list > row:selected label, listbox > row:selected label { color: @pp_fg; }
list > row:selected .dim-label, listbox > row:selected .dim-label {
  color: @pp_dim_sel;
}
treeview.view:selected { background-color: @pp_accent_soft; color: @pp_fg; }
treeview.view:selected:focus {
  background-color: @pp_accent;
  color: @pp_on_accent;
}
scrollbar slider {
  background-color: @pp_scroll;
  min-height: 12px;
  min-width: 12px;
  border-radius: 6px;
}
scrollbar slider:hover { background-color: @pp_dim; }
"#;

/// 组件类：任何主题都该有的排版/小控件样式（只用 `@pp_*` 令牌，不写死颜色）。
const COMPONENTS: &str = r#"
.pp-section {
  font-size: 10.5pt;
  font-weight: 600;
  color: @pp_fg;
  opacity: 0.62;
  letter-spacing: 0.3px;
  margin-bottom: 6px;
}
.pp-hint { font-size: 11pt; color: @pp_dim; }
/* 内置查看/编辑器的行号栏：跟正文一个底色，**不画分隔线**——Zed、gedit、
   VS Code 都没有这条线，有了这道边框加上两栏之间的缝隙，整个行号栏看着
   就像"另一个窗口"。行号栏自己也不画滚动条（整屏只该有正文那一根）：
   实现上用 PolicyType::External 关掉，下面几条是兜底。 */
.pp-gutter { background-color: @pp_panel; }
.pp-gutter-scroll scrollbar.vertical {
  min-width: 0; background: transparent; border: none; margin: 0; padding: 0;
}
/* 正文滚动区：底色跟行号栏一致，两者连成一片（不画边框、不留缝） */
.pp-editor-scroll { background-color: @pp_panel; }
/* 编辑器的**字号档**（阶段 84）：正文与行号栏**同一档**——
   行号栏靠"影子字符"把行高撑成和正文一致，字号不同步整个文件看着是歪的。
   中间那档（2）是默认。 */
.pp-editor-zoom-0 { font-size: 9pt; }
.pp-editor-zoom-1 { font-size: 10.5pt; }
.pp-editor-zoom-2 { font-size: 12pt; }
.pp-editor-zoom-3 { font-size: 14pt; }
.pp-editor-zoom-4 { font-size: 17pt; }
/* 内置编辑器的**工具栏**（阶段 84）：一行紧凑的按钮条。
   底色跟正文一致、下面一条细分隔线——像 gedit / VS Code 的编辑器头，
   但不抢正文的注意力。子标签里原先摆的是一块内层 HeaderBar（长得像
   窗口标题栏、其实操作的是子标签页），现在换成这条工具栏。 */
.pp-toolbar {
  background-color: @pp_panel;
  border-bottom: 1px solid @pp_border;
  padding: 3px 6px;
}
.pp-toolbar button { padding: 2px 5px; }
/* 查找/替换条（阶段 84）：贴在正文上头，底色跟窗口一致 + 一条分隔线。
   默认是收起来的（`set_visible(false)`），按 Ctrl+F / Ctrl+H 才出来。 */
.pp-findbar {
  background-color: @pp_bg;
  border-bottom: 1px solid @pp_border;
  padding: 4px 8px;
}
.pp-findbar entry { min-width: 160px; }
.pp-gutter-scroll scrollbar.vertical > trough {
  min-width: 0; background: transparent; border: none; margin: 0; padding: 0;
}
.pp-gutter-scroll scrollbar.vertical > slider {
  min-width: 0; min-height: 0; background: transparent; border: none; margin: 0;
}
/* 快捷键一览的键位列：等宽 + 加粗，扫读时先看到键位。
   不用 @pp_accent：高对比主题的 accent 是纯蓝，黑底上读不清。 */
.pp-key { color: @pp_fg; font-weight: 600; }
.pp-app-title { font-weight: 700; }
.pp-strong { font-weight: 600; }
.pp-chip {
  border-radius: 6px;
  padding: 2px 8px;
  background-color: @pp_accent_soft;
  color: @pp_fg;
}
.pp-chip-warn { background-color: mix(@pp_warn, @pp_bg, 0.84); color: @pp_fg; }
.pp-chip-err { background-color: mix(@pp_err, @pp_bg, 0.82); color: @pp_fg; }
/* 凭据库状态标志：绿=已启用且加密（这是"一切正常"） */
.pp-chip-ok { background-color: mix(@pp_ok, @pp_bg, 0.84); color: @pp_fg; }
/* 凭据库状态标志（侧栏标题栏）：它是个**状态**，不是标题栏里的主按钮。
   以前直接用 `.pp-chip` 的 accent 底 ⇒ 整个标题栏最响的就是它
   （用户 2026-11-01 看侧栏截图说"有点丑"）⇒ 底色去掉，只留
   **彩色文字 + 一圈同色淡边框**：状态还在，但不抢戏。
   ⚠️ 状态类（`pp-chip-ok` / `-warn` / `-err`）**仍然挂在按钮上**
   （冒烟要数它们），这里用 `.pp-vault-badge.pp-chip-xx` 提一级
   specificity，只改文字/边框色，底色归透明。 */
.pp-vault-badge {
  background-color: transparent;
  border: 1px solid @pp_border;
  color: @pp_dim;
}
.pp-vault-badge.pp-chip-ok { color: @pp_ok; border-color: @pp_ok; }
.pp-vault-badge.pp-chip-warn { color: @pp_warn; border-color: @pp_warn; }
.pp-vault-badge.pp-chip-err { color: @pp_err; border-color: @pp_err; }
/* 条件条（阶段 48）：dbx 风格的 WHERE / ORDER BY 条 + 已生效条件的 chip。
   ⚠️ **不借用 `.pp-chip`**：那个类带着状态色语义（绿=正常 / 黄=警告 /
   红=出错），条件 chip 是"中性的当前筛选"，混用会让人以为条件出了问题。 */
.pp-condbar { padding: 1px 0px; }
.pp-condbar button, .pp-condbar entry { padding-top: 2px; padding-bottom: 2px; }
.pp-cond-chip {
  border-radius: 6px;
  padding: 1px 6px;
  background-color: @pp_accent_soft;
  color: @pp_fg;
}
.pp-cond-chip label { padding: 0px; }
.pp-cond-chip button { padding: 0px 2px; min-height: 0px; min-width: 0px; margin: 0px; }
/* 分段控件（阶段 49）：表设计器里的 `字段 | 索引 | 外键 | 触发器 | DDL`。
   是一排自己画的 ToggleButton（**不是** GtkStackSwitcher——它内部那排
   按钮是 homogeneous 的，每个按钮平分整行宽度，5 个按钮摊满 640px，
   看着像一排导航链接）。当前段用强调色底标出来。 */
.pp-segmented { background-color: @pp_panel; border-radius: 8px; padding: 2px; }
.pp-segmented button {
  border-radius: 6px;
  border: none;
  background-color: transparent;
  color: @pp_dim;
  padding: 4px 14px;
  font-weight: 600;
  box-shadow: none;
  min-height: 26px;
  min-width: 0px;
}
.pp-segmented button:hover { background-color: @pp_hover; color: @pp_fg; }
/* 选中的那一段：底色用 accent_soft（不是实心 accent——整排实心太抢眼） */
.pp-segmented button:checked,
.pp-segmented button:checked:hover {
  background-color: @pp_accent_soft;
  color: @pp_fg;
}
.pp-segmented button:focus-visible { outline: 2px solid alpha(@pp_accent, 0.55); }
.pp-swatch { border-radius: 4px; }
/* 空态（阶段 51 结构化；**阶段 96 重新设计**）：工作台还没有任何页签时
   那一屏。
   阶段 51：原来是**一句居中的提示文字** + 一大片空白 ⇒ 改成一张"开始页"
     卡片（标题 / 副标题 / 连接按钮 / 快速开始）。
   阶段 96（用户 2026-11-01：「这个页面有点丑」）：那张卡**贴在主区左边**
     （GtkBox 里 halign=CENTER 根本不居中 ⇒ 卡片一直靠左）、标题居中而
     小节标题左对齐、按钮各按自己的文字宽度排成**锯齿**、快捷键提示跟
     工具栏上的一模一样 ⇒ 整张卡看着像"随手拼的"。
     现在：**真的居中**（dbui.rs 里左右各一个会伸展的撑杆）、全部左对齐、
     每行 = 图标 + 文字且**整列等宽**、快捷键提示删掉（工具栏上已有）。 */
.pp-empty {
  background-color: @pp_panel;
  border: 1px solid @pp_border;
  border-radius: 12px;
  /* ⚠️ **宽度不能超过容器**（审计 P0-2，2026-10-30）：DB 工作台活在一块
     被 SFTP 面板挤过的窄区里（默认窗口 1200 宽 → 工作台只剩 630px，
     再扣掉对象树，主区 300px）。原来 padding 左右各 28px + min-width
     260px ⇒ 卡片本身就要 316px，比主区还宽 → **整张卡连同里面的主 CTA
     「新建数据库连接…」一起被裁**（用户以为功能坏了）。
     padding 收窄、min-width 降下来。⚠️ **GTK4 的 CSS 没有 `max-width`
     属性**（写了会报 "No property named max-width"，整条规则被丢弃）——
     所以上限只能靠**内容自己收窄**（副标题限宽 + 省略号）。
     ⚠️ **别写 min-width，也别把 padding 写在这一层**（阶段 96 实测）：
     ① GTK4 的 CSS `min-width` 量的是**内容盒**，而控件分配到的宽度是
        **含 padding 与边框**的 ⇒ 两者对不上 ⇒ GtkBox 按 min-width 摆位置、
        按实际宽分配 ⇒ 卡片偏出去十几像素；
     ② GtkBox 摆子控件时**按内容宽算位置**、却按**含 padding 的宽**分配
        ⇒ padding 写在这一层 ⇒ 卡片偏出去 padding/2（实测 16px）。
     ⇒ **padding 挪到内层**（`.pp-empty-inner`），这一层只留底色与边框
     （边框只差 1px，肉眼看不见）。 */
}
/* 内层：只管 padding（内容装在这里） */
.pp-empty-inner { padding: 18px 16px 16px; }
/* 标题 / 副标题：跟下面那一列行同靠左 */
.pp-empty-title { font-size: 1.15em; font-weight: 700; }
.pp-empty-sub { color: @pp_dim; font-size: 0.9em; margin-top: 2px; }
.pp-empty-hint { color: @pp_dim; font-size: 0.8em; margin-top: 14px; }
/* 一行 = 图标 + 文字，整列等宽（卡片是竖排 Box，子控件默认 FILL ⇒
   同一列的行拿到同样的宽度） */
.pp-empty-btn {
  padding: 6px 10px;
  margin-top: 3px;
  border-radius: 8px;
}
/* 主 CTA（新建连接）：实心强调色。
   ⚠️ **别给主按钮加 `flat`**：Adwaita 的 `button.flat` 是
   `background-color: transparent`，优先级比自定义类高 ⇒ 实心底没了
   （`.pp-chip` 踩过同一个坑）。实心底由 `button.pp-primary` 提供。 */
button.pp-empty-primary { margin-top: 6px; }
/* ---- 品牌页（阶段 97）----
   用户 2026-11-02：「不知道这里应该显示啥，或者显示个漂亮的 logo？图片？艺术字？」
   ⇒ 上面加一座"招牌"：**应用自己的图标 + 产品名 + 这一屏是什么**。
   招牌**居中**，下面才接左对齐的活 ⇒ 一眼能看出"这是谁的界面、我在干嘛"。 */
.pp-empty-name {
  font-size: 1.5em;
  font-weight: 800;
  margin-top: 8px;
}
.pp-empty-what {
  color: @pp_dim;
  font-size: 0.95em;
  margin-top: 1px;
}
/* 分隔线：招牌与"手上的活"之间（没有它就成一坨）。
   ⚠️ 用 `gtk::Separator` 画的——自己拿空 Box 画的话，GTK4 **不画空 Box
   的背景**（实测：background-color 完全没生效 ⇒ 那条线根本看不见）。 */
.pp-empty-sep {
  margin-top: 14px;
  margin-bottom: 2px;
}
/* chip：当前开着哪个库（**只表示状态、不可点**）。
   ⚠️ 别用 `flat` 那套按钮样式——那是一排"能点的行"。 */
.pp-empty-chip {
  background-color: @pp_bg;
  border: 1px solid @pp_border;
  border-radius: 8px;
  padding: 5px 10px;
  margin-top: 6px;
}
.pp-empty-chip-label { font-weight: 600; }
.pp-empty-chip-sub { color: @pp_dim; font-size: 0.85em; }
/* ER 图（阶段 19）：表框 / 列 / 标记。
   字号用 em / 相对值，跟着缩放档（.er-zoom-*）一起变——
   .er-table 上**不能**写死 font-size，否则会盖掉缩放档的继承。 */
.er-table {
  background-color: @pp_bg;
  border: 1px solid @pp_border;
  border-radius: 6px;
}
.er-table-title {
  background-color: @pp_accent_soft;
  font-weight: 700;
  padding: 3px 8px;
}
.er-col { padding: 0px 8px; }
.er-mark { font-size: 0.8em; color: @pp_dim; }
.er-type { font-size: 0.9em; color: @pp_dim; }
/* 缩放档：几何尺寸在 Rust 里算，字号在这里定（两者一起变才不失调） */
.er-zoom-0 { font-size: 8pt; }
.er-zoom-1 { font-size: 10.5pt; }
.er-zoom-2 { font-size: 15pt; }
.er-zoom-3 { font-size: 20pt; }
/* 侧边栏与内容区之间的界线：浅色主题下两侧都是白底，没这条线会糊成一片。
   ⚠️ 阶段 93：**整条侧栏一个底色**（跟窗口一致），里面的树透明——
   以前树是白卡片、上面是灰底，过滤框底下突然贴了块白板。 */
.pp-sidebar { border-right: 1px solid @pp_border; background-color: @pp_bg; }
.pp-sidebar treeview.view { background-color: transparent; }
/* 分屏：活动面板描边（outline 不吃布局，不会把终端挤动） */
.pp-pane-active { outline: 1px solid alpha(@pp_accent, 0.55); outline-offset: -1px; }
/* 面板之间的分割线：2px 细线（悬停高亮，仍可拖动调宽窄） */
paned > separator {
  min-width: 2px; min-height: 2px; margin: 0; padding: 0;
  background-color: @pp_border;
}
paned > separator:hover { background-color: @pp_accent; }
/* 对话框底部动作区：统一尺寸、圆角与"主/次/危险"三级层次 */
.dialog-action-area {
  padding: 12px 16px 14px 16px;
  border-top: 1px solid @pp_border;
}
.dialog-action-area button {
  min-height: 34px;
  min-width: 64px;
  padding: 5px 16px;
  border-radius: 8px;
  border: 1px solid @pp_border;
  background-color: transparent;
  color: @pp_fg;
  font-weight: 600;
  box-shadow: none;
}
.dialog-action-area button:hover { background-color: @pp_hover; }
.dialog-action-area button:active { background-color: mix(@pp_fg, @pp_bg, 0.14); }
.dialog-action-area button:disabled { opacity: 0.45; }
.dialog-action-area button:focus-visible {
  outline: 2px solid alpha(@pp_accent, 0.55);
  outline-offset: 2px;
}
/* 对话框的**默认按钮**（回车按的那个）：画一道看得见的框。
   危险操作的默认往往是「取消」——它既不是强调色也不是危险色，
   跟旁边的按钮长得一模一样 ⇒ 用户看不出回车会按哪个（审计 P2-5/P2-6）。 */
.dialog-action-area button.pp-default {
  outline: 2px solid @pp_fg;
  outline-offset: 2px;
}
.dialog-action-area button.suggested-action {
  background-color: @pp_accent;
  border-color: @pp_accent;
  color: @pp_on_accent;
}
.dialog-action-area button.suggested-action:hover { background-color: shade(@pp_accent, 0.92); }
.dialog-action-area button.destructive-action {
  background-color: @pp_err;
  border-color: @pp_err;
  color: @pp_on_accent;
}
.dialog-action-area button.destructive-action:hover { background-color: shade(@pp_err, 0.92); }

/* 文件列表专用：窄滚动条（横向只占 8px，不跟终端抢高度） */
.pp-thin-scrollbar scrollbar { background: transparent; border: none; margin: 0; min-width: 4px; min-height: 4px; }
.pp-thin-scrollbar scrollbar > trough {
  background: transparent; border: none; margin: 0; min-width: 4px; min-height: 4px;
}
.pp-thin-scrollbar scrollbar slider {
  min-width: 3px; min-height: 3px; border-radius: 2px; border: none;
  background-color: alpha(@pp_fg, 0.30);
}
.pp-thin-scrollbar scrollbar.vertical { min-width: 4px; }
.pp-thin-scrollbar scrollbar.horizontal { min-height: 4px; }
.pp-thin-scrollbar scrollbar.vertical slider { min-height: 36px; }
.pp-thin-scrollbar scrollbar.horizontal slider { min-width: 36px; }
.pp-thin-scrollbar scrollbar slider:hover { background-color: alpha(@pp_fg, 0.50); }
/* 文件名列专用：只滚名称文字，窄到 8px */
scrollbar.pp-name-scroll { min-height: 4px; background: transparent; border: none; margin: 0; }
scrollbar.pp-name-scroll slider {
  min-height: 3px; min-width: 24px; border-radius: 2px; border: none;
  background-color: alpha(@pp_fg, 0.30);
}
scrollbar.pp-name-scroll slider:hover { background-color: alpha(@pp_fg, 0.50); }

/* ---------------- 右键菜单（自建 Popover） ----------------
   终端右键菜单是 Popover + Box + Button，不是 PopoverMenu，所以系统主题不会
   给它任何"菜单感"：默认是一摞等宽居中按钮。这里用一套 .pp-menu-* 类补齐：
   圆角卡片、图标 + 左对齐文字、右侧快捷键/勾选、危险项红字。
   颜色只用 @pp_* 令牌，深色/浅色/高对比/跟随系统都能用。

   ⚠️ **弹层表面必须是不透明的**：GtkPopover 的表面在 X11 上是**不透明**
   的，没画到的像素会显示成**黑块**——原来这里写 transparent + 圆角，
   结果菜单四周（以及圆角外的那圈）是一圈黑框（2026-10 用户截图里能看到）。
   改成 @pp_panel（跟卡片同色）之后，黑边没了、圆角外的留白跟卡片融在一起。

   ⚠️ **box-shadow 也不能要**：GTK4 把弹层上的 box-shadow 画成**不透明**
   黑块（不是半透明投影），整张菜单会被一圈黑色矩形包住（实测 34px 宽）。
   去掉阴影后弹层表面 = 卡片本身，左对齐也就简单了（见 menu.rs 的
   CARD_CHROME）。 */
popover.pp-menu, popover.pp-menu.background {
  background-color: @pp_panel;
  border: none;
  box-shadow: none;
}
popover.pp-menu > contents {
  background-color: @pp_panel;
  border: 1px solid @pp_border;
  border-radius: 10px;
  padding: 6px;
  box-shadow: none;
}
.pp-menu-body { min-width: 236px; }
button.pp-menu-item {
  background-color: transparent;
  background-image: none;
  border: none;
  border-radius: 6px;
  padding: 4px 10px;
  /* ⚠️ **高度必须跟字号走，不能写死 px**（用户 2026-10-29：右键菜单里
     「左右分屏」的「左」字**底下一横不见了**，顶栏菜单标题下半截也被削）。
     机制：GTK4 的 `GtkLabel` 在**分配高度小于文字的自然高度**时，
     直接把多出来的部分裁掉（不缩字、不换行）——底部那一横就这么没了。
     而这里的分配高度来自按钮的 `min-height`（GTK CSS 里是 **content box**，
     再加上下 padding）。写死 24px 时，本机字体的自然高度是 20px，
     看着富余 4px；可换个中文字体（fallback 字体的 ascent/descent 比西文大）
     或用户调大字号，自然高度就超过 24px → **削字**。
     写成 `em` 就跟着字号走，1.8em 足够容下中文 fallback 字体的行高
     （实测 Noto Sans CJK 的 ascent+descent ≈ 1.45em）。 */
  min-height: 1.8em;
  /* ⚠️ **菜单项比正文小一号**（用户 2026-10-28：「菜单的字体又有些大」）。
     菜单项是"内容"，正文是"内容"，顶栏菜单栏标题是"入口"——
     层级必须是 **标题 ≥ 正文 > 菜单项**，不然界面看着"标题比内容还小"。
     阶段 41 把菜单栏标题压到 0.88em、菜单项却留着默认 1em，正好反了，
     这次把两边一起摆正。 */
  font-size: 0.92em;
  color: @pp_fg;
  box-shadow: none;
}
button.pp-menu-item:hover { background-color: @pp_hover; }
/* 显式 hover 类：Popover 里按钮的 :hover 有时只更新 PRELIGHT 状态而不刷
   背景（文件面板右键菜单实测），menu.rs 的 Motion 控制器直接加这个类。 */
button.pp-menu-item.pp-menu-hover { background-color: @pp_hover; }
button.pp-menu-item:active { background-color: mix(@pp_fg, @pp_panel, 0.16); }
button.pp-menu-item:focus-visible {
  /* 键盘焦点用淡强调底，不画蓝框：鼠标点开时 GTK 也可能把首项设成
     focus-visible，蓝框看起来像“默认选中了第一项”。 */
  background-color: @pp_accent_soft;
  outline: none;
}
button.pp-menu-item .pp-menu-label { color: @pp_fg; }
button.pp-menu-item .pp-menu-accel { color: @pp_dim; font-size: 9pt; margin-left: 18px; }
button.pp-menu-item image.pp-menu-icon { color: @pp_dim; }
button.pp-menu-item:hover image.pp-menu-icon { color: @pp_fg; }
button.pp-menu-item.pp-menu-hover image.pp-menu-icon { color: @pp_fg; }
button.pp-menu-item image.pp-menu-check { color: @pp_accent; }
button.pp-menu-item:disabled { opacity: 0.45; }
button.pp-menu-item.pp-menu-danger .pp-menu-label { color: @pp_err; }
button.pp-menu-item.pp-menu-danger image.pp-menu-icon { color: @pp_err; }
button.pp-menu-item.pp-menu-danger:hover { background-color: mix(@pp_err, @pp_panel, 0.88); }
button.pp-menu-item.pp-menu-danger.pp-menu-hover {
  background-color: mix(@pp_err, @pp_panel, 0.88);
}
/* 顶栏菜单栏（自绘）：一排 MenuButton，点开的是统一的 pp-menu 弹层。
   原来是原生 PopoverMenuBar——下拉是 gio::Menu 模型渲染的，跟终端右键
   菜单对不上（用户要求"所有菜单风格都要像终端右键菜单看齐"）。 */
.pp-menubar { margin: 0; }
button.pp-menubar-item {
  background-color: transparent;
  background-image: none;
  border: none;
  border-radius: 6px;
  /* ⚠️ **别太宽**（用户 2026-10-28："主菜单宽度有些太大"）：标题
     挤在顶栏，padding 是"看不见但占地方"的大头——Adwaita 默认按钮
     padding 左右各 12px，7 项就是 168px 的空白。阶段 40 收到 5px 后
     每项 43→35px、总宽 301→245px，用户反馈"看不出变化"——19% 的差别
     肉眼分辨不出来。阶段 41 因此把顶级菜单从 7 个减到 5 个（编辑并入
     终端、帮助并入工具），这里再把 padding / 字号各收一档做叠加。
     ⚠️ **字号别压过菜单项**（用户 2026-10-28：「菜单栏的字体有些小」）：
     菜单栏标题是"入口"、菜单项是"内容"，**标题要 ≥ 正文**，
     菜单项才是小一号的那个（见 `button.pp-menu-item` 的 font-size）。
     阶段 41 一度把标题压到 0.88em、比菜单项的 0.92em 还小——层级反了。 */
  padding: 1px 4px;
  /* ⚠️ **同理，高度跟着字号走**（用户 2026-10-29：顶栏菜单标题下半截被截断）。
     写死 20px 时，1em 的中文字体自然高度就有 22px → 顶栏标题天天少一截。
     详见 `button.pp-menu-item` 里那段说明。 */
  min-height: 1.8em;
  /* 标题与正文同大：菜单栏是"入口"，但也不该比正文还小——
     那会让顶栏那排字看着像"脚注"。 */
  font-size: 1em;
  color: @pp_fg;
  box-shadow: none;
}
button.pp-menubar-item:hover { background-color: @pp_hover; }
/* 顶栏菜单的键盘焦点：底色 + **看得见的框**。
   以前只写 `outline: none` ⇒ 键盘用户完全看不出焦点在哪（审计 P2-6：
   高对比主题下尤其要命）。框画在按钮内侧（offset 为负），不占布局。 */
button.pp-menubar-item:focus-visible {
  background-color: @pp_accent_soft;
  outline: 2px solid @pp_fg;
  outline-offset: -2px;
}
/* ⚠️ **别在这里写 `button.pp-menubar-item image { … }`**：弹层是挂在菜单栏
   按钮上的（`pop.set_parent(&btn)`），后代选择器会连弹层里的菜单图标
   一起命中——图标会凭空消失（实测踩过）。 */

separator.pp-menu-sep {
  min-height: 1px;
  margin: 5px 4px;
  background-color: @pp_border;
  border: none;
}
/* 分组标题 / 空状态：菜单里的小字，不抢行内容 */
.pp-menu-section {
  font-size: 9pt;
  font-weight: 600;
  color: @pp_dim;
  margin: 6px 10px 2px 10px;
}
.pp-menu-empty { color: @pp_dim; margin: 8px 10px; }
/* 原生 PopoverMenu 若显式挂了 pp-menu 类（连接管理器 ⋯），也给同一张卡片。
   GTK 的模型菜单节点是 modelbutton，不能套 .pp-menu-item 的图标骨架，
   但可把内边距/圆角/悬停对齐到自绘菜单。 */
popover.pp-menu > contents { min-width: 236px; }
popover.pp-menu modelbutton {
  padding: 4px 10px;
  min-height: 24px;
  border-radius: 6px;
  /* 跟自绘菜单项同号（见 button.pp-menu-item）：原生菜单项不该比
     自绘的大一号，否则同一个菜单里两种字号。 */
  font-size: 0.92em;
}
popover.pp-menu modelbutton:hover { background-color: @pp_hover; }
popover.pp-menu separator { margin: 5px 4px; background-color: @pp_border; }
"#;

/// 共用骨架：只列需要改的属性，靠 `@define-color` 给整套配色命名。
const BASE: &str = r#"
/* 注意：这里**不要**再写「把 pp_bg 指向 @bg」这种别名定义。
   那三行会把每套主题自己定义的 pp_bg/pp_fg/pp_border 覆盖成对
   @bg / @fg / @border 的引用，而这三个名字在 GTK 默认主题里并不存在——
   实际解析结果退化成白色，于是"深色"主题的窗口底与文字全白、形同没生效。
   颜色只由各主题块定义，BASE 只负责使用。 */
window, .background { background-color: @pp_bg; color: @pp_fg; }

headerbar {
  background-color: @pp_bg;
  background-image: none;
  color: @pp_fg;
  border-bottom: 1px solid @pp_border;
  box-shadow: none;
}
headerbar .pp-app-title { font-weight: 700; color: @pp_fg; }

label, button, entry, checkbutton, radiobutton, treeview, textview,
menubutton, notebook, treeview.view {
  color: @pp_fg;
}

/* ⚠️ **给文字留"字形余量"**（用户 2026-10-29：右键菜单里「左右分屏」的
   「左」字**底下一横不见了**，顶栏菜单标题下半截也被削掉）。

   **机制**：GTK4 的 `GtkLabel` 在**分配高度恰好等于**文字的自然高度
   （ascent+descent）时，最后一行的字形正好压在绘制边界上——抗锯齿 +
   半像素落点，**底部那一横就被削掉了**。多一分余量就没这回事。
   而分配高度来自父容器的 `min-height`；之前那些值是**写死的 px**，
   字号一变（中文字体 fallback 的 ascent/descent 比西文大，
   用户再调个系统字号）自然高度就追上来，余量归零 → 削字。

   **为什么用 em**：写死的 px 跟不上字号；`em` 是"按当前字号留够空间"，
   1.7em 足够盖住中文 fallback 字体的行高（Noto Sans CJK 的
   ascent+descent ≈ 1.45em），还剩 ~0.25em 的余量。

   **为什么写在这几个选择器上**：Label 的 minimum 会顺着
   Label → 行 Box → 按钮 一路传上去，父容器因此**必须**给足高度，
   不管那个按钮的 `min-height` 写的是多少。
   ⚠️ **没有写成全局 `label { … }`**：全局规则会把每一个 Label（包括
   小 chip、分组标题）都抬高，界面变松；而且**一旦设了 min-height，
   Label 的 natural height 就被抬成这个值**，余量就没法量了
   （见 `examples/menu_fit_smoke.rs`：判定要用"同字号的对照 Label"
   才能量出纯文本高度）。 */
.pp-menu-label {
  min-height: 1.7em;
}
/* 原生 PopoverMenu 的菜单项文字（连接管理器 ⋯ 那种）——它们不是自绘的
   `.pp-menu-item`，得单独点名。 */
popover.pp-menu modelbutton label {
  min-height: 1.7em;
}
/* 通用兜底：按钮里的文字（对话框「保存修改」、**顶栏菜单标题**、页签标题…） */
button label {
  min-height: 1.6em;
}

/* GtkNotebook 的节点名是 notebook（类型选择器），`.notebook`（类选择器）
   匹配不到任何控件——这条曾经写成 `.notebook > header`，于是标签栏一直用
   Adwaita 的浅灰底，跟深色主题割裂。 */
notebook > header { background-color: @pp_bg; border-color: @pp_border; }
notebook > header > tabs > tab { color: @pp_fg; }
notebook > header > tabs > tab:checked { background-color: @pp_panel; }
notebook > header > tabs > tab:hover { background-color: @pp_hover; }
/* 页面区（标签内容）用面板色：不设的话 Adwaita 会给一层白色 base 底，
   深色主题下就是"黑壳子里的白纸" */
notebook > stack { background-color: @pp_panel; }

button, .circular, .flat {
  background-image: none;
  background-color: @pp_panel;
  border-color: @pp_border;
  color: @pp_fg;
}
button:hover { background-color: @pp_hover; }
button:checked { background-color: @pp_accent; color: @pp_on_accent; }
button.flat { background-color: transparent; border-color: transparent; }
button.flat:hover { background-color: @pp_hover; }
/* 平面开关按钮（隐藏文件 / 跟随目录）checked 时必须单独给底色与前景色：
   通用 `button:checked` 是「强调底 + on_accent 文字」，而 `.flat` 随后又把底
   改成透明——浅色主题下 on_accent 是白色，于是图标白字白底，按钮看着像空
   了一块。这里换成浅强调底 + 正常文字色，两种状态都看得见、也还能分辨。 */
button.flat:checked { background-color: @pp_accent_soft; color: @pp_fg; }
button.flat:checked:hover { background-color: mix(@pp_accent, @pp_bg, 0.80); }
/* 工具栏上的纯图标按钮：压到 30px 见方。
   结果表工具栏一排要放 5-8 颗（下一页/导出/编辑/插入/删除/保存/放弃），
   Adwaita 默认 padding 一颗就 40px 宽，右侧会被窗口切掉。文字改成图标后
   再压一压内边距，一整排才放得下。 */
button.pp-icon-btn {
  padding: 2px 4px;
  min-width: 30px;
  min-height: 30px;
}
/* 主操作按钮（阶段 44）：dbx 的「新增字段」「应用变更」是**实心主按钮**，
   一眼就知道该点哪个。我们阶段 39 把按钮文字全去掉后，所有按钮**权重完全
   一样**——用户不知道哪个是"主操作"，哪个是"辅助"。
   ⚠️ **别给主按钮加 `flat`**：Adwaita 的 `button.flat` 是
   `background-color: transparent`，优先级比自定义类高——加了实心底就没了
   （跟 `.pp-chip` 一个毛病）。 */
button.pp-primary {
  background-color: @pp_accent;
  color: @pp_on_accent;
  border-color: @pp_accent;
  font-weight: 600;
}
button.pp-primary:hover { background-color: mix(@pp_accent, @pp_fg, 0.15); }
button.pp-primary:active { background-color: mix(@pp_accent, @pp_fg, 0.28); }
button.pp-primary:disabled {
  background-color: @pp_hover;
  color: @pp_dim;
  border-color: @pp_border;
}
/* 主按钮里的图标要跟着文字变白（图标默认跟文字色走，但要显式写一遍：
   `.pp-menu-icon` 那条规则给图标上过 @pp_dim）。 */
button.pp-primary image { color: @pp_on_accent; }
/* 主/危险按钮必须保住强调色：上面那条通用 button 规则会盖掉 Adwaita 的
   .suggested-action 配色，结果是"生成""连接"这类主按钮和普通按钮长得一样。 */
button.suggested-action {
  background-color: @pp_accent;
  color: @pp_on_accent;
  border-color: @pp_accent;
}
button.suggested-action:hover { background-color: shade(@pp_accent, 0.92); }
button.suggested-action:active { background-color: shade(@pp_accent, 0.84); }
button.destructive-action {
  background-color: @pp_err;
  color: @pp_on_accent;
  border-color: @pp_err;
}
button.destructive-action:hover { background-color: shade(@pp_err, 0.92); }

entry, spinbutton, textview, .view {
  background-color: @pp_panel;
  color: @pp_fg;
  border-color: @pp_border;
}
treeview.view { background-color: @pp_panel; }
/* ⚠️ **别给整棵树加 hover 底色**（阶段 93）：`treeview.view:hover` 是
   「指针在树上任意位置」⇒ 鼠标一进列表，**所有行**一起变灰——看着像
   「整块被选中」。GTK4 的 TreeView 没有「按行 hover」的 CSS 节点，
   行级高亮只能靠选中态。 */

treeview header button, .treeview header button {
  background-color: @pp_bg;
  color: @pp_fg;
  border-color: @pp_border;
}

popover, modelbutton, .background.popover { background-color: @pp_bg; color: @pp_fg; border-color: @pp_border; }
/* GTK4 的弹层真正画底色的是内部 contents 节点：只改 popover 节点的话，
   深色主题下弹层仍是一张白纸（白底 + 浅色字，几乎读不出来）。 */
popover > contents, popover.background > contents {
  background-color: @pp_bg;
  color: @pp_fg;
  border-color: @pp_border;
}
popover > arrow { background-color: @pp_bg; border-color: @pp_border; }
modelbutton:hover { background-color: @pp_hover; }
/* 复选框/单选框：默认 Adwaita 的浅色方块压在深色弹层里像块白斑 */
checkbutton check, radiobutton radio {
  background-color: @pp_panel;
  border-color: @pp_border;
  color: @pp_accent;
}
checkbutton check:checked, radiobutton radio:checked {
  background-color: @pp_accent;
  border-color: @pp_accent;
  color: @pp_on_accent;
}

/* 选中态：**淡底 + 正常文字**，不用整行 accent 实底。
   整行实底会把行内说明文字（.dim-label）压成灰字压蓝底，几乎读不出来；
   实底只留给「聚焦的」树视图，作为"当前焦点在这里"的强提示。 */
/* GtkListBox 的节点名是 list：底色必须一起改，否则未选中的行会露出
   Adwaita 的白色 base（深色主题下就是浅灰字压白底，几乎读不出来）。 */
list, listbox { background-color: @pp_panel; }
list > row, listbox > row { background-color: transparent; }
/* 行里的**文字**：**所有**行都显式上前景色。
   ⚠️ 以前只给「选中行」的 label 上色（下面那条 `:selected label`）⇒ 没选中的
   行用的是 **GTK 的默认色**——在某些系统主题下那个色**是白的**（用户
   2026-11-01 报：首选项「终端」页四个配色名只剩第一行看得见，而那一行正好
   是**当前选中的**那行）。阶段 89 把「选中行」当事实来源去掉后
   （`SelectionMode::None`）**所有**行都没颜色了 ⇒ 必须给所有行上色。 */
list > row label, listbox > row label { color: @pp_fg; }
list > row:hover, listbox > row:hover { background-color: @pp_hover; }
list > row:selected, listbox > row:selected {
  background-color: @pp_accent_soft;
  color: @pp_fg;
}
list > row:selected label, listbox > row:selected label { color: @pp_fg; }
list > row:selected .dim-label, listbox > row:selected .dim-label {
  color: @pp_dim_sel;
}
treeview.view:selected { background-color: @pp_accent_soft; color: @pp_fg; }
treeview.view:selected:focus { background-color: @pp_accent; color: @pp_on_accent; }

/* 侧栏标题栏的「＋新建」（阶段 92）。
   ⚠️ 不用 `list-add-symbolic`：symbolic 图标是细线条，浅色底上几乎看不见
   （用户 2026-11-01：「『＋』号是否可以搞得粗一点、明显一点」）⇒ 改用
   **全角文字加号**，各主题下都粗、都显眼。
   ⚠️ 高度用 em：字形余量跟着字号走（阶段 60 的坑——高度写死 px，用户调大
   字号时最后一行会被削掉半截）。 */
.pp-plus {
  font-size: 18pt;
  font-weight: 700;
  color: @pp_fg;
  min-height: 1.25em;
}
button.pp-plus-btn:hover label.pp-plus { color: @pp_accent; }

/* 欢迎页（阶段 91）：标题 + 最近连接卡片。
   ⚠️ **卡片别加 `flat`**：Adwaita 的 `button.flat` 是
   `background-color: transparent`，优先级比自定义类高 ⇒ 加了卡片的底色和
   边框就没了（看着像没卡片，只剩一行字）。 */
.pp-welcome-title { font-size: 15pt; font-weight: 700; color: @pp_fg; }
button.pp-recent-card {
  background-color: @pp_panel;
  border: 1px solid @pp_border;
  border-radius: 10px;
  padding: 8px 12px;
  /* 高度用 em：两行文字 + 内边距，字号变了余量也跟着变（阶段 60 的坑：
     高度写死 px 的话，用户调大字号时最后一行会被削掉半截）。 */
  min-height: 3.4em;
}
button.pp-recent-card:hover {
  background-color: @pp_accent_soft;
  border-color: @pp_accent;
}
button.pp-recent-card:focus-visible {
  outline-style: solid;
  outline-width: 2px;
  outline-color: @pp_accent;
  outline-offset: 1px;
}
button.pp-recent-card label.pp-card-title { font-weight: 600; color: @pp_fg; }
button.pp-recent-card label.pp-card-sub { font-size: 10.5pt; color: @pp_dim; }
button.pp-recent-card image.pp-card-arrow { color: @pp_dim; }
button.pp-recent-card:hover image.pp-card-arrow { color: @pp_accent; }

/* 滚动条拖块：次要控件，用中性色而不是主文字色（原来 28px 纯白/纯黑，
   在长列表旁边像一根实心杠）。 */
scrollbar { background-color: @pp_bg; }
scrollbar slider {
  background-color: @pp_scroll;
  min-height: 14px;
  min-width: 14px;
  border-radius: 7px;
}
scrollbar slider:hover { background-color: @pp_dim; }
scrollbar slider:active { background-color: @pp_accent; }

separator { background-color: @pp_border; min-width: 1px; min-height: 1px; }

tooltip { background-color: @pp_bg; color: @pp_fg; border-color: @pp_border; }
.dim-label { color: @pp_dim; }

/* 面包屑芯片（阶段 43）：对标 dbx 标签页下面那排
   `SQLite_du3h / test@main / 3 字段`——一眼看清"我在哪"。
   ⚠️ **别复用 `.pp-chip`**：那个类带着状态色语义（绿/黄/红，
   用来标「凭据库 未启用」），拿来标面包屑会看成"告警"。 */
.pp-crumb {
  background-color: @pp_hover;
  color: @pp_fg;
  border-radius: 9px;
  padding: 1px 8px;
  font-size: 0.85em;
}
/* 面包屑最后一片（"N 字段" / "N 列"）：是补充信息，压暗一点 */
.pp-crumb.pp-crumb-dim { color: @pp_dim; }

/* 错误提示（解锁失败、约束冲突…）。
 *
 * ⚠️ **不能用 GTK 内置的 `.error`**：Adwaita 写的是 `label.error`，但本项目
 * 上面有一条 `label, button, … { color: @pp_fg; }`（见 BASE 段），把
 * Adwaita 那条盖掉了——实际渲染出来错误提示是**正常前景色**，一点都
 * 不像错误。所以自带一个 `.pp-error`。
 */
label.pp-error { color: @pp_err; }
"#;

const DARK_CSS: &str = concat!(
    r#"
@define-color pp_bg      #1e2124;
@define-color pp_panel   #26292e;
@define-color pp_fg      #dcdfe4;
@define-color pp_dim     #9aa0a6;
@define-color pp_border  #3a3f47;
@define-color pp_hover   #31353c;
@define-color pp_accent  #3d7dd6;
@define-color pp_on_accent #ffffff;
@define-color pp_accent_soft #253349;
@define-color pp_dim_sel #aab2bb;
@define-color pp_scroll  #454b54;
@define-color pp_chip    #33383f;
@define-color pp_ok      #2fa86b;
@define-color pp_warn    #c98a1f;
@define-color pp_err     #d9534f;
"#,
    "BASE_PLACEHOLDER"
);

const LIGHT_CSS: &str = concat!(
    r#"
@define-color pp_bg      #f3f4f6;
@define-color pp_panel   #ffffff;
@define-color pp_fg      #1f2328;
@define-color pp_dim     #5c636a;
@define-color pp_border  #d3d7dd;
@define-color pp_hover   #e6e9ed;
@define-color pp_accent  #2f6fd0;
@define-color pp_on_accent #ffffff;
@define-color pp_accent_soft #e8f0fc;
@define-color pp_dim_sel #4a5560;
@define-color pp_scroll  #c5cad1;
@define-color pp_chip    #e4e8ec;
@define-color pp_ok      #1a7f4b;
@define-color pp_warn    #8a5a00;
@define-color pp_err     #c0392b;
"#,
    "BASE_PLACEHOLDER"
);

const CONTRAST_CSS: &str = concat!(
    r#"
@define-color pp_bg      #000000;
@define-color pp_panel   #000000;
@define-color pp_fg      #ffffff;
@define-color pp_dim     #ffffff;
@define-color pp_border  #ffffff;
@define-color pp_hover   #333333;
@define-color pp_accent  #0000ff;
@define-color pp_on_accent #ffffff;
@define-color pp_accent_soft #0000ff;
@define-color pp_dim_sel #ffffff;
@define-color pp_scroll  #ffffff;
@define-color pp_chip    #333333;
@define-color pp_ok      #00ff88;
@define-color pp_warn    #ffd400;
@define-color pp_err     #ff5555;
"#,
    "BASE_PLACEHOLDER"
);

/// `concat!` 不接受变量，用一个包装函数把 BASE 拼进去（编译期完成，零运行时开销）。
fn css_with(base: &str) -> String {
    base.replace("BASE_PLACEHOLDER", BASE)
}

/// 某主题实际要交给 GTK 的完整 CSS。
///
/// - 显式主题：主题色 + 骨架 + 组件类
/// - 跟随系统：系统语义色 + 修复规则 + 组件类（拿不到系统色时 None，
///   即完全不注入，保持原来的行为）
pub fn css_text(t: Theme) -> Option<String> {
    match t {
        Theme::System => system_tokens().map(|tok| format!("{tok}{FIX}{COMPONENTS}")),
        _ => t.css().map(|base| format!("{}{}", css_with(base), COMPONENTS)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn 主题键值往返() {
        for t in Theme::ALL {
            assert_eq!(Theme::parse(t.key()), t, "{t:?} 往返失败");
        }
        assert_eq!(Theme::parse("不存在"), Theme::System);
        assert_eq!(Theme::default(), Theme::System);
    }

    #[test]
    fn 配色键值往返() {
        for p in Palette::ALL {
            assert_eq!(Palette::parse(p.key()), p, "{p:?} 往返失败");
        }
        assert_eq!(Palette::parse("不存在"), Palette::Dark);
        assert_eq!(Palette::default(), Palette::Dark);
    }

    #[test]
    fn 每套配色都是合法颜色() {
        for p in Palette::ALL {
            let (fg, bg, ansi) = p.colors();
            for c in [fg, bg].into_iter().chain(ansi) {
                assert!(
                    gdk::RGBA::parse(c).is_ok(),
                    "{p:?} 里 {c} 不是合法颜色"
                );
            }
        }
    }

    #[test]
    fn 高对比配色必须真有对比度() {
        // 高对比的意义就在这儿，写死了也要钉一道：fg/bg 不能撞色
        let (fg, bg, _) = Palette::HighContrast.colors();
        assert_ne!(fg, bg);
        assert_eq!(fg, "#ffffff");
        assert_eq!(bg, "#000000");
    }

    /// CSS 写错时 GTK 只在 stderr 打一行警告，界面照样起得来——
    /// 那样就变成"主题不生效但没人知道"。这里把解析错误收集起来断言。
    #[test]
    fn css_能被_gtk_真解析() {
        // CssProvider 的构造要过 GTK 初始化，没有显示服务就跳过（并说清楚）
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() {
            eprintln!("跳过 css_能被_gtk_真解析：本机没有显示服务，CssProvider 起不来");
            return;
        }
        // System 只有在能读到系统语义色时才生成 CSS（需要显示环境）
        let mut themes = vec![Theme::Dark, Theme::Light, Theme::HighContrast];
        if gdk::Display::default().is_some() {
            themes.push(Theme::System);
        }
        for t in themes {
            let Some(css) = css_text(t) else {
                eprintln!("跳过 {t:?}：拿不到 CSS（无显示或系统色名缺失）");
                continue;
            };
            let p = gtk::CssProvider::new();
            let errs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            {
                let errs = errs.clone();
                p.connect_parsing_error(move |_, sec, err| {
                    let line = sec.start_location().lines() + 1;
                    errs.borrow_mut().push(format!("第 {line} 行：{err}"));
                });
            }
            p.load_from_string(&css);
            assert!(
                errs.borrow().is_empty(),
                "{t:?} 的 CSS 有解析错误：{:?}",
                errs.borrow()
            );
        }
    }

    #[test]
    fn 每个主题的状态色都是合法颜色() {
        for t in Theme::ALL {
            for s in State::ALL {
                let c = state_hex_for(t, s);
                assert!(
                    gdk::RGBA::parse(c).is_ok(),
                    "{t:?}/{s:?} 的 {c} 不是合法颜色"
                );
            }
        }
    }

    #[test]
    fn 浅色状态色压白底够深() {
        // 状态点（●）使用状态色：浅色主题下三色都必须够深，
        // 否则白底上那颗点等于没有
        for s in State::ALL {
            let c = gdk::RGBA::parse(state_hex_for(Theme::Light, s)).unwrap();
            let lum = 0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue();
            assert!(lum < 0.45, "浅色主题 {s:?} 太亮（{lum:.2}），白底上看不清");
        }
    }

    #[test]
    fn 拼好的_css_才是给_gtk_的那份() {
        // 这条钉的是曾经的真实 bug：apply() 直接把带 BASE_PLACEHOLDER 的原始串
        // 交给 GTK，只有颜色定义生效、组件规则全丢。给 GTK 的必须是 css_text。
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            assert!(!css.contains("BASE_PLACEHOLDER"), "{t:?} 没拼上骨架");
            assert!(
                css.contains("window, .background"),
                "{t:?} 缺窗口底色规则"
            );
            assert!(css.contains("scrollbar slider"), "{t:?} 缺滚动条规则");
            assert!(css.contains(".pp-section"), "{t:?} 缺组件类");
        }
    }

    #[test]
    fn 跟随系统也能拿到修复规则() {
        // 有显示 → 从系统主题读语义色并生成修复规则；没显示 → 不注入（None）
        match css_text(Theme::System) {
            Some(css) => {
                assert!(css.contains("@define-color pp_bg"));
                assert!(css.contains("list > row:selected"));
                assert!(css.contains("scrollbar slider"));
                assert!(
                    !css.contains("window, .background"),
                    "跟随系统不该重刷窗口底色（那是系统主题的事）"
                );
            }
            None => {
                // 拿不到修复规则**是合法的**（见 `system_tokens` 的注释：非
                // Adwaita 系主题拿不到颜色名就退回"完全不动"）。但**不能**在
                // 「有显示、GTK 也在这个线程初始化过了」的情况下还拿不到。
                //
                // ⚠️ **`is_initialized_main_thread()` 是线程局部的**
                // （gtk4-rs 的 `rt.rs` 里是 `thread_local!`）⇒ 单测跑在
                // libtest 的**子线程**上，除非这个线程恰好是当初调
                // `gtk::init()` 的那个线程，否则它永远是 false ⇒ 单测里
                // "拿不到"是**常态**，不是坏了。
                // （旧断言只认「没有显示」⇒ 并行跑时 2/3 概率变红，
                //  害得阶段 86 的"全量回归全绿"差点卡在一条假红上。）
                assert!(
                    gdk::Display::default().is_none() || !gtk::is_initialized_main_thread(),
                    "有显示、GTK 也在这个线程初始化过了，却还是拿不到系统色 ⇒ 颜色名探测失效了"
                );
            }
        }
    }

    #[test]
    fn 次要文字色永远是合法颜色() {
        // markup 里的 foreground 只吃颜色字符串：拼错了 GTK 会静默丢样式
        assert!(gdk::RGBA::parse(&muted_hex()).is_ok(), "muted_hex 不合法");
    }

    #[test]
    fn 当前主题缓存默认为跟随系统() {
        // 未 apply 过时 state_hex 也该给出合法颜色（不能 panic / 读磁盘）
        assert_eq!(current_theme(), Theme::System);
        for s in State::ALL {
            assert!(gdk::RGBA::parse(state_hex(s)).is_ok());
        }
    }

    #[test]
    fn 主题令牌没有被未定义别名覆盖() {
        // 曾经 BASE 开头写了 `@define-color pp_bg @bg;`，把每套主题的
        // pp_bg/pp_fg 覆盖成对不存在的 @bg/@fg 的引用（解析退化成白色），
        // 结果"深色"主题白底白字。这里钉死：主题块里的定义必须原样生效。
        for (t, bg) in [
            (Theme::Dark, "#1e2124"),
            (Theme::Light, "#f3f4f6"),
            (Theme::HighContrast, "#000000"),
        ] {
            let css = css_text(t).unwrap();
            assert!(
                css.contains(&format!("@define-color pp_bg      {bg};"))
                    || css.contains(&format!("@define-color pp_bg {bg};")),
                "{t:?} 的 pp_bg 定义被覆盖了"
            );
            assert!(
                !css.contains("@define-color pp_bg @bg;"),
                "{t:?} 里还有 `@define-color pp_bg @bg;` 这种覆盖"
            );
        }
    }

    #[test]
    fn css_带了选中态与滚动条规则() {
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            for sel in [
                "list > row:selected",
                "pp_accent_soft",
                "pp_dim_sel",
                "scrollbar slider",
                "pp_scroll",
                "button.suggested-action",
                "button.flat:checked",
                ".pp-key",
            ] {
                assert!(css.contains(sel), "{t:?} 的 CSS 缺 {sel}");
            }
        }
    }

    #[test]
    fn css_带右键菜单规则() {
        // 终端右键菜单的观感全在这几条类上：圆角卡片、行高/悬停、图标与
        // 右侧勾选、危险色。少一条就会退回"一摞居中按钮"。
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            for sel in [
                "popover.pp-menu > contents",
                ".pp-menu-body",
                "button.pp-menu-item",
                ".pp-menu-label",
                ".pp-menu-accel",
                ".pp-menu-check",
                ".pp-menu-danger",
                ".pp-menu-section",
                "popover.pp-menu modelbutton",
                "separator.pp-menu-sep",
            ] {
                assert!(css.contains(sel), "{t:?} 的菜单 CSS 缺 {sel}");
            }
        }
    }

    #[test]
    fn css_骨架拼进去了() {
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            assert!(!css.contains("BASE_PLACEHOLDER"), "{t:?} 骨架没拼上");
            assert!(css.contains("@define-color pp_bg"), "{t:?} 缺变量定义");
            assert!(css.contains("treeview.view"), "{t:?} 缺 TreeView 规则");
        }
        if gdk::Display::default().is_none() {
            assert!(css_text(Theme::System).is_none(), "无显示时系统色解析不了");
        }
    }

    /* ---------------- 对比度（阶段 73 / 审计 P1-10） ---------------- */

    /// WCAG 对比度算得对不对（拿标准里的黑白与几个已知值校）。
    #[test]
    fn 对比度算法符合_wcag() {
        assert!((contrast_ratio("#000000", "#ffffff") - 21.0).abs() < 0.01);
        assert!((contrast_ratio("#ffffff", "#ffffff") - 1.0).abs() < 0.01);
        // #767676 是 WCAG 给出的"白底上刚好过 4.5:1"的那个灰
        let c = contrast_ratio("#767676", "#ffffff");
        assert!((4.4..=4.6).contains(&c), "#767676 对白底应是 4.5:1 左右，实得 {c}");
    }

    /// 三套**固定**主题的说明文字色都要够 4.5:1（窗口底色与面板底色都算）。
    ///
    /// ⚠️ 这是阶段 73 的核心断言：审计 P1-10 说默认主题（跟随系统）把
    /// **禁用色**当说明文字用，白底上只有 3.03:1；固定主题本来就没问题，
    /// 但也得钉住——别哪天改配色改出个偏淡的。
    #[test]
    fn 固定主题的说明文字对比度够_aa() {
        // (主题, pp_dim, pp_bg, pp_panel)
        let cases = [
            (Theme::Dark, "#9aa0a6", "#1e2124", "#26292e"),
            (Theme::Light, "#5c636a", "#f3f4f6", "#ffffff"),
            (Theme::HighContrast, "#ffffff", "#000000", "#000000"),
        ];
        for (t, dim, bg, panel) in cases {
            for b in [bg, panel] {
                let c = contrast_ratio(dim, b);
                assert!(
                    c >= CONTRAST_AA,
                    "{t:?} 的说明文字 {dim} 在 {b} 上只有 {c:.2}:1，不到 {CONTRAST_AA}:1"
                );
            }
        }
    }

    /// 「跟随系统」算出来的说明文字色要够 4.5:1（拿几套真实系统配色代跑）。
    ///
    /// ⚠️ 单测里没有显示服务，问不到系统主题 ⇒ 这里**代跑算法**：喂几套
    /// Adwaita 的浅/深配色，断言算出来的色在**两个底色**上都够 4.5:1。
    #[test]
    fn 跟随系统的说明文字对比度够_aa() {
        // (系统主题的 正文色 / 窗口底色 / 输入框底色)
        let cases = [
            ("Adwaita 浅色", "#2e3436", "#f6f5f4", "#ffffff"),
            ("Adwaita 深色", "#ffffff", "#2e3436", "#242424"),
            ("Yaru 浅色", "#3d3846", "#f6f5f4", "#ffffff"),
            ("高对比浅色", "#000000", "#ffffff", "#ffffff"),
        ];
        for (name, fg, bg, base) in cases {
            let dim = dim_for(fg, &[bg, base]);
            for b in [bg, base] {
                let c = contrast_ratio(&dim, b);
                assert!(
                    c >= CONTRAST_AA,
                    "{name}：算出来的说明文字 {dim} 在 {b} 上只有 {c:.2}:1"
                );
            }
            // 还得**确实是淡的**（比正文色淡）——不然就退化成正文色了
            assert_ne!(dim, fg, "{name}：说明文字色没变淡");
        }
    }

    /// ⚠️ **回归钉**：系统那个**禁用色**在白底上只有 3.03:1 ⇒ 不能拿它当
    /// 说明文字色用（审计 P1-10 的原始实测值）。
    #[test]
    fn 系统禁用色不够_aa_不能当说明文字() {
        let insensitive = "#8e8e8e";
        let c = contrast_ratio(insensitive, "#ffffff");
        assert!(
            c < CONTRAST_AA,
            "禁用色 {insensitive} 在白底上是 {c:.2}:1（应当不够 {CONTRAST_AA}:1，\
             否则这条测试的前提变了）"
        );
        // 而我们算出来的替代色必须够
        let dim = dim_for("#2e3436", &["#f6f5f4", "#ffffff"]);
        assert!(contrast_ratio(&dim, "#ffffff") >= CONTRAST_AA);
    }

    /// ⚠️ **回归钉**：ListBox 行里的文字必须**显式**上前景色。
    ///
    /// 用户 2026-11-01 报「首选项终端页的配色名只剩第一行看得见」——那一行
    /// 正好是**当前选中的**那一行（我们只给 `:selected` 的 label 上过色），
    /// 其余行用的是 **GTK 的默认色**，在某些系统主题下那个色**是白的**
    /// ⇒ 白底白字，看不见。阶段 89 把「选中行」当事实来源去掉之后更糟：
    /// **所有**行都没颜色了。
    #[test]
    fn 列表行里的文字都显式上前景色() {
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            assert!(
                css.contains("list > row label, listbox > row label { color: @pp_fg; }"),
                "{t:?}：ListBox 行里的文字没显式上色 ⇒ 系统主题下可能白字白底"
            );
        }
    }

    /// 行里的文字色（@pp_fg）压在行底色（@pp_panel）上要够 AA。
    ///
    /// 上一条钉的是"规则在不在"，这条钉的是"这个色够不够深"——
    /// 规则在、但前景色本身太浅，照样看不见。
    #[test]
    fn 列表行文字对比度够_aa() {
        // (主题, pp_fg, pp_panel)
        let cases = [
            (Theme::Dark, "#dcdfe4", "#26292e"),
            (Theme::Light, "#1f2328", "#ffffff"),
            (Theme::HighContrast, "#ffffff", "#000000"),
        ];
        for (t, fg, panel) in cases {
            let c = contrast_ratio(fg, panel);
            assert!(
                c >= CONTRAST_AA,
                "{t:?} 的行文字色 {fg} 在行底色 {panel} 上只有 {c:.2}:1"
            );
        }
    }

    /// 计数徽章：底色与上面的字必须读得清（阶段 93）。
    ///
    /// ⚠️ 徽章底色是"正文色往底色混 0.86"来的 ⇒ 次要色压上去只有
    /// ~4.1:1（低于 AA）⇒ 徽章里的数字必须用**正文色**。
    #[test]
    fn 计数徽章的对比度够_aa() {
        // (主题, 徽章底色 pp_chip, 徽章上的字 pp_fg)
        let cases = [
            (Theme::Dark, "#33383f", "#dcdfe4"),
            (Theme::Light, "#e4e8ec", "#1f2328"),
            (Theme::HighContrast, "#333333", "#ffffff"),
        ];
        for (t, chip, fg) in cases {
            let c = contrast_ratio(fg, chip);
            assert!(
                c >= CONTRAST_AA,
                "{t:?} 的徽章底色 {chip} 上的字 {fg} 只有 {c:.2}:1"
            );
        }
    }

    /// 徽章底色得跟调色板里的 `pp_chip` 对得上（一个写死、一个现算，
    /// 差了就说明有人只改了一处）。
    #[test]
    fn 徽章底色与调色板一致() {
        for (t, chip) in [
            (Theme::Dark, "#33383f"),
            (Theme::Light, "#e4e8ec"),
            (Theme::HighContrast, "#333333"),
        ] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            assert!(
                css.contains(&format!("@define-color pp_chip    {chip};")),
                "{t:?} 的调色板里没有 pp_chip {chip}"
            );
        }
    }

    /// 说明文字的字号：10pt → 11pt（审计 P1-10："表单标签 10pt 提到 11pt
    /// 或至少提高对比"）。两条都做了 ⇒ 字号也钉死。
    #[test]
    fn 说明文字字号是_11pt() {
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let css = css_text(t).expect("非 System 主题都该有 CSS");
            assert!(
                css.contains(".pp-hint { font-size: 11pt"),
                "{t:?}：说明文字的字号还是 10pt（10pt 在小屏上偏小）"
            );
        }
    }
}
