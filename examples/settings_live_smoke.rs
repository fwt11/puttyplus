//! 「首选项 → 终端」的**实时生效**冒烟（用户 2026-11-09）。
//!
//! 用户要求：
//! > 「首选项-终端 那里增加字体设置，而且字体设置和字号修改后应该实时生效，
//! > 这样用户可以马上在终端上看出改变的效果。」
//! > 「切换语言也应该实时生效。」
//!
//! 断言：
//! 1. **字号改完立刻生效**：改设置 → 终端的字体描述**当场**变了
//!    （不用重开终端）；
//! 2. **字体名改完立刻生效**：同上；
//! 3. **优先级不变**：连接/会话里**特意设过**的字号仍然压过全局默认；
//! 4. **语言切完立刻生效**：首选项窗口的页签当场换成新语言（不用重启）；
//! 5. **界面语言切换后，界面文案确实变了**（中文 → 英文 → 中文）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example settings_live_smoke

use gtk::prelude::*;
use puttyplus::i18n::Lang;
use puttyplus::settings;
use puttyplus::settingsui::{self, Page};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}


/// 顶栏里的**菜单标题**（文件 / 编辑 / …）。
///
/// ⚠️ 菜单栏是「一排按钮 + 每个按钮一个弹层」⇒ 标题就是按钮上的文字。
fn menu_titles(header: &gtk::HeaderBar) -> Vec<String> {
    // ⚠️ 菜单按钮的 child 是**一层 Box**（图标 + 文字）⇒ 得递归找 Label。
    fn first_label(w: &gtk::Widget) -> Option<String> {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            return Some(l.text().to_string());
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            if let Some(t) = first_label(&x) {
                return Some(t);
            }
            c = x.next_sibling();
        }
        None
    }
    // ⚠️ 菜单按钮**不在顶栏的直接子节点里**（它们在一个 Box 里）⇒ 得
    // 整棵树走一遍。
    fn walk(w: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if let Some(t) = first_label(b.upcast_ref::<gtk::Widget>()) {
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            walk(&x, out);
            c = x.next_sibling();
        }
    }
    let mut out = Vec::new();
    walk(header.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

fn main() {
    gtk::init().expect("gtk init");
    // ⚠️ **测试不许碰用户真实配置**：设置文件、凭据库全指到临时目录。
    let dir = std::env::temp_dir().join(format!("pp_settings_live_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // SAFETY：单线程 example，设完立刻用。
    unsafe {
        std::env::set_var("HOME", &dir);
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("PUTTYPLUS_VAULT", dir.join("credentials.vault"));
    }
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    /* ---------------- 1. 字号改完立刻生效 ---------------- */
    {
        settings::set_term_font_size(Some(10.5));
        let before = puttyplus::termview::font_description(None);
        let before_size = before.size() as f64 / 1024.0;

        settings::set_term_font_size(Some(18.0));
        let after = puttyplus::termview::font_description(None);
        let after_size = after.size() as f64 / 1024.0;

        check!(
            "字号改完立刻生效（不用重开终端）",
            (before_size - 10.5).abs() < 0.01 && (after_size - 18.0).abs() < 0.01,
            format!("改之前 {before_size}，改之后 {after_size}")
        );
        // 改回去
        settings::set_term_font_size(Some(10.5));
    }

    /* ---------------- 2. 字体名改完立刻生效 ---------------- */
    {
        settings::set_term_font_family(None);
        let d0 = puttyplus::termview::font_description(None);
        let fam0 = d0.family().map(|f| f.to_string()).unwrap_or_default();

        settings::set_term_font_family(Some("DejaVu Sans Mono"));
        let d1 = puttyplus::termview::font_description(None);
        let fam1 = d1.family().map(|f| f.to_string()).unwrap_or_default();

        check!(
            "字体名改完立刻生效",
            fam0.eq_ignore_ascii_case("monospace") && fam1.eq_ignore_ascii_case("DejaVu Sans Mono"),
            format!("改之前 {fam0:?}，改之后 {fam1:?}")
        );
        settings::set_term_font_family(None);
    }

    /* ---------------- 3. 优先级：会话里设过的字号仍然压过全局 ---------------- */
    {
        settings::set_term_font_size(Some(12.0));
        // 会话/连接里特意设了 20 ⇒ 应当还是 20（全局默认不该覆盖它）
        let d = puttyplus::termview::font_description(Some(20.0));
        let size = d.size() as f64 / 1024.0;
        check!(
            "连接里设过的字号仍然压过全局默认",
            (size - 20.0).abs() < 0.01,
            format!("实际 {size}")
        );
        settings::set_term_font_size(Some(10.5));
    }

    /* ---------------- 4/5. 语言切完立刻生效 ---------------- */
    {
        // 回到中文，确认起点
        puttyplus::i18n::set_lang(Lang::Zh);
        settingsui::open(Page::Appearance);
        pump(300);

        let nb = settingsui::pages_notebook().expect("首选项窗口应当开着");
        let tab_text = |i: u32| -> String {
            let Some(page_w) = nb.nth_page(Some(i)) else {
                return String::new();
            };
            nb.tab_label(&page_w)
                .and_then(|w| w.downcast::<gtk::Label>().ok())
                .map(|l| l.text().to_string())
                .unwrap_or_default()
        };
        let zh_tab = tab_text(0);
        check!(
            "中文时「外观」页签是中文",
            zh_tab == "外观",
            format!("实得 {zh_tab:?}")
        );

        // ---- 顶栏/菜单栏也要跟着换（重建外壳）----
        //
        // ⚠️ 顶栏与菜单栏以前住在 bin 里 ⇒ 冒烟测不到 ⇒ 「语言实时切换」
        // 只能靠人眼看。现在搬进 lib（`topbar::build_header`）⇒ 这里
        // 能真的建一个顶栏、切语言、再建一次，比较两边的菜单标题。
        {
            let tabs = std::rc::Rc::new(puttyplus::tabs::Tabs::new());
            let sb = puttyplus::sidebar::Sidebar::new(std::rc::Rc::clone(&tabs));
            sb.setup();
            let app = gtk::Application::builder()
                .application_id("org.puttyplus.SmokeLive")
                .build();
            let win = gtk::ApplicationWindow::new(&app);
            let noop: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(|| {});
            let header_zh = puttyplus::topbar::build_header(&win, &tabs, &noop, &sb, &gtk::Label::new(Some("t")));
            let zh_menu_titles = menu_titles(&header_zh);
            check!(
                "中文时菜单栏是中文",
                zh_menu_titles.iter().any(|t| t == "文件"),
                format!("实得 {zh_menu_titles:?}")
            );

            // 切语言 ⇒ 重建顶栏（main.rs 里就是这么干的）
            puttyplus::i18n::set_lang(Lang::En);
            let header_en = puttyplus::topbar::build_header(&win, &tabs, &noop, &sb, &gtk::Label::new(Some("t")));
            let en_menu_titles = menu_titles(&header_en);
            check!(
                "切到英文后菜单栏当场变英文",
                en_menu_titles.iter().any(|t| t == "File"),
                format!("实得 {en_menu_titles:?}")
            );
            puttyplus::i18n::set_lang(Lang::Zh);
        }

        // ---- 切到英文：模拟用户在下拉框里选了 English ----
        puttyplus::i18n::set_lang(Lang::En);
        settings::set_language(Lang::En);
        puttyplus::i18n::notify_lang_changed();
        pump(400);

        let en_tab = tab_text(0);
        check!(
            "切到英文后页签当场变英文（不用重启）",
            en_tab == "Appearance",
            format!("实得 {en_tab:?}")
        );

        // 页面里的文案也该换了
        let labels = settingsui::page_labels(Page::Terminal);
        let has_en = labels.iter().any(|l| l == "Terminal" || l == "Terminal font");
        check!(
            "切到英文后页面里的文案也换了",
            has_en,
            format!("终端页文案：{labels:?}")
        );

        // ---- 切回中文 ----
        puttyplus::i18n::set_lang(Lang::Zh);
        settings::set_language(Lang::Zh);
        puttyplus::i18n::notify_lang_changed();
        pump(400);
        let zh_again = tab_text(0);
        check!(
            "切回中文也即时生效",
            zh_again == "外观",
            format!("实得 {zh_again:?}")
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    if fails.is_empty() {
        println!("settings live smoke ok");
    } else {
        println!("失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
