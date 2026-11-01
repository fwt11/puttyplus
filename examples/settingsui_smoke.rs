//! 首选项中心的冒烟（审计 P1-4，阶段 69）。
//!
//! **问题**：设置项散在**四个地方**——「外观」窗口、「会话选项」、连接
//! 编辑器、传输队列窗口顶部 ⇒ 用户想调一个设置得先知道它在哪。
//!
//! **断言**：
//! 1. **每个设置项有且只有一个归属页**（注册表里的每一项，它的标题只
//!    出现在它归属的那一页里）——这是审计 P1-4 的验收点；
//! 2. 窗口有**六个页签**，名字就是那六页；
//! 3. 从别的入口进来能**切到指定的那一页**（「外观…」进的是外观页）；
//! 4. 同类**只开一个**窗口（阶段 67 的规矩）。
//!
//! ```text
//! xvfb-run -a cargo run --example settingsui_smoke
//! ```

use gtk::prelude::*;
use puttyplus::settingsui::{self, Page, SETTINGS};
use puttyplus::windows::ToolKind;

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

fn main() {
    gtk::init().expect("gtk init");
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

    // ---------- 1. 每个设置项有且只有一个归属页 ----------
    {
        let pages: Vec<(Page, Vec<String>)> = Page::all()
            .into_iter()
            .map(|p| {
                let labels = settingsui::page_labels(p);
                (p, labels)
            })
            .collect();
        for it in SETTINGS {
            let mut hits: Vec<Page> = Vec::new();
            for (p, labels) in &pages {
                if labels.iter().any(|l| l == it.label) {
                    hits.push(*p);
                }
            }
            check!(
                &format!("设置项「{}」只出现在一页", it.label),
                hits.len() == 1 && hits[0] == it.page,
                format!(
                    "出现在 {:?}（登记在 {:?}）",
                    hits.iter().map(|p| p.title()).collect::<Vec<_>>(),
                    it.page.title()
                )
            );
        }
    }

    // ---------- 2. 窗口：六个页签 ----------
    settingsui::open(Page::Appearance);
    pump(300);
    check!("首选项窗口开着", settingsui::is_open(), "开都没开");
    let nb = settingsui::pages_notebook().expect("窗口里应当有页签容器");
    check!(
        "六个页签",
        nb.n_pages() == 6,
        format!("{} 个", nb.n_pages())
    );

    // 页签名字就是那六页
    let mut titles: Vec<String> = Vec::new();
    for i in 0..nb.n_pages() {
        // ⚠️ GTK4 的 `tab_label` 认的是**页控件**，不是页序号
        let Some(page_w) = nb.nth_page(Some(i)) else {
            continue;
        };
        let w = nb.tab_label(&page_w);
        if let Some(l) = w.and_then(|w| w.downcast::<gtk::Label>().ok()) {
            titles.push(l.text().to_string());
        }
    }
    let want: Vec<String> = Page::all().iter().map(|p| p.title().to_string()).collect();
    check!(
        "页签名字就是那六页",
        titles == want,
        format!("{titles:?} vs {want:?}")
    );

    // ---------- 3. 能切到指定的那一页 ----------
    settingsui::open(Page::Security);
    pump(200);
    check!(
        "切到「安全」页",
        nb.current_page() == Some(Page::Security.index()),
        format!("当前页 {:?}", nb.current_page())
    );
    settingsui::open(Page::Transfer);
    pump(200);
    check!(
        "切到「传输」页",
        nb.current_page() == Some(Page::Transfer.index()),
        format!("当前页 {:?}", nb.current_page())
    );

    // ---------- 4. 同类只开一个 ----------
    settingsui::open(Page::Appearance);
    pump(200);
    check!(
        "同类只开一个窗口",
        puttyplus::windows::tool_window(ToolKind::Settings).is_some()
            && puttyplus::windows::open_count() == 1,
        format!("登记了 {} 个工具窗", puttyplus::windows::open_count())
    );

    // ---------- 5. 传输设置搬走了：队列窗口顶部只剩摘要 ----------
    {
        let sum = puttyplus::panel::queue::xfer_summary();
        check!(
            "队列顶部摘要说清三样设置",
            sum.contains("断点续传") && sum.contains("SHA-1") && sum.contains("并发"),
            sum
        );
    }

    if fails.is_empty() {
        println!("settingsui smoke ok");
    } else {
        println!("settingsui smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
