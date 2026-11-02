//! 界面密度与界面字号（4.5）。
//!
//! **用 CSS 而不是改代码**：`set_margin_*` / `set_size_request` 在本项目里
//! 有 119 处，逐个改成"按密度取值"既易错又难 review。GTK 的正经做法是给
//! 窗口挂一个 CSS 类，用规则统一压 padding / min-height —— 一处规则覆盖
//! 全树，改密度不用碰任何业务代码。
//!
//! **与主题正交**：紧凑模式的 CSS 走**独立的 CssProvider**，哪怕主题是
//! "跟随系统"（那套不注入任何 CSS）紧凑模式照样生效。两者各管各的，跟
//! 终端配色一样。
//!
//! **界面字号与终端字号互不影响**：界面走 `gtk-xft-dpi`（GTK 全局字体
//! 缩放），终端字号是 1.5 那个会话级覆盖（绝对磅值，不吃 xft-dpi）。
//! 两边独立，正是这里要的效果。

use gtk::gdk;
use gtk::prelude::*;

/// 界面密度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Normal,
    Compact,
}

impl Density {
    pub fn label(self) -> &'static str {
        match self {
            Density::Normal => tr("标准"),
            Density::Compact => tr("紧凑"),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Density::Normal => tr("常规留白，工具栏与文件列表舒展"),
            Density::Compact => tr("压缩留白，同屏显示更多文件行"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Density::Normal => "normal",
            Density::Compact => "compact",
        }
    }

    pub fn parse(s: &str) -> Density {
        match s {
            "compact" => Density::Compact,
            _ => Density::Normal,
        }
    }
}

/// 界面字号档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiFont {
    Small,
    #[default]
    Normal,
    Large,
}

impl UiFont {
    pub const ALL: [UiFont; 3] = [UiFont::Small, UiFont::Normal, UiFont::Large];

    pub fn label(self) -> &'static str {
        match self {
            UiFont::Small => tr("小"),
            UiFont::Normal => tr("中"),
            UiFont::Large => tr("大"),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            UiFont::Small => "small",
            UiFont::Normal => "normal",
            UiFont::Large => "large",
        }
    }

    pub fn parse(s: &str) -> UiFont {
        match s {
            "small" => UiFont::Small,
            "large" => UiFont::Large,
            _ => UiFont::Normal,
        }
    }

    /// 相对 96dpi 的缩放倍率。
    pub fn scale(self) -> f64 {
        match self {
            UiFont::Small => 0.9,
            UiFont::Normal => 1.0,
            UiFont::Large => 1.15,
        }
    }
}

/// 紧凑模式的 CSS 规则。挂在主窗口上的 `pp-compact` 类下。
///
/// 只压**留白与控件高度**，不动字号、不动颜色——字号归 `gtk-xft-dpi` 管，
/// 颜色归主题管，三者各司其职。
const COMPACT_CSS: &str = r#"
.pp-compact button,
.pp-compact .circular,
.pp-compact .flat,
.pp-compact modelbutton,
.pp-compact menuname {
  padding: 1px 6px;
  min-height: 24px;
}

.pp-compact entry,
.pp-compact spinbutton {
  padding: 1px 6px;
  min-height: 24px;
}

/* 文件列表：行距主要来自 cell 的 padding */
.pp-compact treeview.view cell {
  padding: 1px 4px;
}
.pp-compact treeview.view header button {
  padding: 1px 4px;
}

.pp-compact notebook > header {
  min-height: 28px;
}
.pp-compact notebook > header > tabs > tab {
  min-height: 26px;
  padding: 0 8px;
}

.pp-compact popover > contents,
.pp-compact menu > contents {
  padding: 2px;
}

/* 滚动条拖块：小一点，密排布局里才不挤地方 */
.pp-compact scrollbar slider {
  min-height: 20px;
  min-width: 20px;
}
"#;

/// 独立于主题的 provider：主题选"跟随系统"时紧凑模式照样有效。
struct Loaded {
    provider: gtk::CssProvider,
}

thread_local! {
    static LOADED: RefCell<Option<Loaded>> = const { RefCell::new(None) };
    /// 当前密度（缓存：watcher 每 150ms 用一次，不能每次都读盘）
    static CURRENT: std::cell::Cell<Density> = const { std::cell::Cell::new(Density::Normal) };
    /// 紧凑模式下的"新窗口自动收紧"巡检器
    static WATCH: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

use std::cell::RefCell;
use crate::i18n::tr;

/// 给单个窗口套/摘紧凑类。
fn tag_with(widget: &gtk::Widget, d: Density) {
    match d {
        Density::Compact => {
            if !widget.has_css_class("pp-compact") {
                widget.add_css_class("pp-compact");
            }
        }
        Density::Normal => {
            if widget.has_css_class("pp-compact") {
                widget.remove_css_class("pp-compact");
            }
        }
    }
}

/// 给任意窗口补一次密度类（对话框自己也可以主动调；不调也行，巡检器会补）。
pub fn tag<W: IsA<gtk::Window>>(w: &W) {
    let win: &gtk::Window = w.upcast_ref();
    let widget: &gtk::Widget = win.upcast_ref();
    tag_with(widget, CURRENT.with(|c| c.get()));
}

/// 显式登记一个窗口（可选；保留给主窗，语义上"这是我们的主窗口"）。
pub fn set_window(_w: gtk::Window) {}

/// 紧凑模式下，新弹出的对话框也要跟着收紧——GTK4 没有"窗口创建"的全局钩子，
/// 所以用一个 150ms 的巡检器给还没套类的顶层窗口补上（只在紧凑模式运行，
/// 顶层窗口通常只有个位数，开销可以忽略）。
fn start_watch() {
    WATCH.with(|w| {
        if w.borrow().is_some() {
            return;
        }
        let id = gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), || {
            let d = CURRENT.with(|c| c.get());
            if d != Density::Compact {
                return gtk::glib::ControlFlow::Continue;
            }
            for win in gtk::Window::list_toplevels() {
                tag_with(&win, d);
            }
            gtk::glib::ControlFlow::Continue
        });
        *w.borrow_mut() = Some(id);
    });
}

fn stop_watch() {
    WATCH.with(|w| {
        if let Some(id) = w.borrow_mut().take() {
            id.remove();
        }
    });
}

/// 装载紧凑模式的 CSS（幂等）。启动时调一次。
pub fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    LOADED.with(|l| {
        if l.borrow().is_some() {
            return;
        }
        let p = gtk::CssProvider::new();
        p.load_from_string(COMPACT_CSS);
        gtk::StyleContext::add_provider_for_display(&display, &p, PRIORITY_USER);
        *l.borrow_mut() = Some(Loaded { provider: p });
    });
}

const PRIORITY_USER: u32 = 800;

pub fn density() -> Density {
    Density::parse(&crate::settings::get_str("ui", "density"))
}

/// 当前生效密度（不读盘；watcher 与 tag 用）。
pub fn current() -> Density {
    CURRENT.with(|c| c.get())
}

pub fn ui_font() -> UiFont {
    UiFont::parse(&crate::settings::get_str("ui", "ui_font"))
}

/// 应用密度：给当前所有顶层窗口加/去 `pp-compact` 类，并开启/关闭巡检器。
pub fn apply_density(d: Density) {
    CURRENT.with(|c| c.set(d));
    for win in gtk::Window::list_toplevels() {
        tag_with(&win, d);
    }
    match d {
        Density::Compact => start_watch(),
        Density::Normal => stop_watch(),
    }
}

/// 应用界面字号（gtk-xft-dpi 是 1024 为 96dpi 的定点数）。
pub fn apply_ui_font(f: UiFont) {
    let Some(s) = gtk::Settings::default() else { return };
    s.set_gtk_xft_dpi((96.0 * f.scale() * 1024.0).round() as i32);
}

pub fn set_density(d: Density) {
    crate::settings::set_str("ui", "density", d.key());
    apply_density(d);
}

pub fn set_ui_font(f: UiFont) {
    crate::settings::set_str("ui", "ui_font", f.key());
    apply_ui_font(f);
}

/// 启动时套用已保存的设置。
pub fn load_saved() {
    install();
    apply_density(density());
    apply_ui_font(ui_font());
}

/// 供 CSS 解析校验用（theme.rs 那条测试同款做法：GTK 只在 stderr 打警告，
/// 语法错了界面照样起得来）。
pub fn css_text() -> &'static str {
    COMPACT_CSS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 密度键值往返() {
        for d in [Density::Normal, Density::Compact] {
            assert_eq!(Density::parse(d.key()), d);
        }
        assert_eq!(Density::parse("不存在"), Density::Normal);
    }

    #[test]
    fn 字号键值往返() {
        for f in UiFont::ALL {
            assert_eq!(UiFont::parse(f.key()), f);
        }
        assert_eq!(UiFont::parse("不存在"), UiFont::Normal);
    }

    #[test]
    fn 字号档位单调且默认居中() {
        assert!(UiFont::Small.scale() < UiFont::Normal.scale());
        assert!(UiFont::Normal.scale() < UiFont::Large.scale());
        assert_eq!(UiFont::default(), UiFont::Normal);
    }

    #[test]
    fn xft_dpi_换算落在合理区间() {
        for f in UiFont::ALL {
            let dpi = (96.0 * f.scale() * 1024.0).round() as i32;
            // 90dpi 太小会糊、133dpi 太大像放大镜；越界说明换算写错了
            assert!(
                (82 * 1024..=140 * 1024).contains(&dpi),
                "{f:?} 算出 {dpi}，超出合理区间"
            );
        }
    }

    #[test]
    fn 紧凑_css_能被_gtk_真解析() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() {
            eprintln!("跳过紧凑_css_能被_gtk_真解析：本机没有显示服务");
            return;
        }
        let p = gtk::CssProvider::new();
        let errs: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        {
            let errs = errs.clone();
            p.connect_parsing_error(move |_, sec, err| {
                let line = sec.start_location().lines() + 1;
                errs.borrow_mut().push(format!("第 {line} 行：{err}"));
            });
        }
        p.load_from_string(css_text());
        assert!(errs.borrow().is_empty(), "紧凑 CSS 有解析错误：{:?}", errs.borrow());
    }

    #[test]
    fn 紧凑_css_覆盖了主要控件() {
        let css = css_text();
        for sel in [
            "button",
            "entry",
            "treeview.view cell",
            "notebook > header",
            "popover > contents",
            "scrollbar slider",
        ] {
            assert!(css.contains(sel), "紧凑 CSS 漏了 {sel}");
        }
    }
}
