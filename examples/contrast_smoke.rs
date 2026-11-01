//! 阶段 73 冒烟：**说明文字的对比度**（审计 P1-10）。
//!
//! **为什么要有这个冒烟**：单测测的是"算法"（`dim_for` 算出来的色够不够
//! 4.5:1），而用户真正看到的是**GTK 实际加载的那份 CSS**——中间隔着
//! `@define-color` 的定义、`pp_dim` 被谁引用、`.pp-hint` / `.dim-label`
//! 这些类到底用没用它。哪一环掉了，用户看到的都是"太淡、看不清"，而界面
//! 不报错、日志也不吭声。
//!
//! 所以这里验**真正交给 GTK 的那份 CSS**：`theme::css_text(t)` ⇒ 从里面
//! 把 `pp_dim` / `pp_bg` / `pp_panel` 三个颜色抠出来 ⇒ 量对比度。
//! 「跟随系统」那一套最关键：以前它的 `pp_dim` 就是系统的**禁用色**
//! （`insensitive_fg_color`，白底上只有 3.03:1）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example contrast_smoke

use gtk::prelude::*;
use puttyplus::theme::{self, Theme};

/// 从 CSS 里抠出 `@define-color <名字> <颜色>;` 的颜色值。
fn css_color(css: &str, name: &str) -> Option<String> {
    let prefix = format!("@define-color {name}");
    for line in css.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let v = rest.trim().trim_end_matches(';').trim();
        if v.starts_with('#') {
            return Some(v.to_string());
        }
    }
    None
}

fn main() {
    // ⚠️ **必须走 gtk::Application**：「跟随系统」那套要读系统主题的语义色，
    // 那需要 GTK 已经初始化、也已经有显示服务（`system_tokens` 里判
    // `is_initialized_main_thread`）。不初始化 ⇒ 系统主题直接被跳过，
    // 而它恰恰是这次要修的那一套。
    let app = gtk::Application::builder()
        .application_id("x.y.PpContrast")
        .build();
    app.connect_activate(|app| {
        check_all();
        app.quit();
    });
    app.run_with_args::<&str>(&[]);
}

fn check_all() {
    let mut checked = 0;
    let mut skipped: Vec<&str> = Vec::new();
    for t in Theme::ALL {
        // 「跟随系统」要 GTK 解析出系统语义色才拿得到 CSS
        let Some(css) = theme::css_text(t) else {
            eprintln!("跳过 {t:?}：这套主题的 CSS 拿不到（系统色还没解析出来）");
            skipped.push(t.label());
            continue;
        };
        let dim = css_color(&css, "pp_dim").unwrap_or_else(|| panic!("{t:?}：CSS 里缺 pp_dim"));
        let bg = css_color(&css, "pp_bg").unwrap_or_else(|| panic!("{t:?}：CSS 里缺 pp_bg"));
        let panel =
            css_color(&css, "pp_panel").unwrap_or_else(|| panic!("{t:?}：CSS 里缺 pp_panel"));
        // 说明文字可能落在窗口底色上，也可能落在面板/输入框底色上 ⇒ 两个都算
        for b in [&bg, &panel] {
            let c = theme::contrast_ratio(&dim, b);
            assert!(
                c >= theme::CONTRAST_AA,
                "{t:?}：说明文字 {dim} 在 {b} 上只有 {c:.2}:1，不到 {}:1",
                theme::CONTRAST_AA
            );
        }
        println!("{t:?}：说明文字 {dim} / 底色 {bg}、{panel} ⇒ 对比度够 ✓");
        checked += 1;
    }
    assert!(checked >= 3, "至少要验到 3 套主题，实得 {checked}");
    if !skipped.is_empty() {
        eprintln!("（跳过：{}）", skipped.join("、"));
    }
    println!("contrast_smoke: OK");
}
