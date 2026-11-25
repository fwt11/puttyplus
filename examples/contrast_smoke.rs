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

fn rgba_hex(c: &gtk::gdk::RGBA) -> String {
    let channel = |v: f32| (f64::from(v) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(c.red()),
        channel(c.green()),
        channel(c.blue())
    )
}

fn resolved_color(sc: &gtk::StyleContext, name: &str) -> String {
    rgba_hex(
        &sc.lookup_color(name)
            .unwrap_or_else(|| panic!("GTK 没解析出颜色 {name}")),
    )
}

fn shot(path: &str) {
    let _ = std::process::Command::new("xdotool")
        .args(["windowraise"])
        .status();
    match std::process::Command::new("import")
        .args(["-window", "root", path])
        .status()
    {
        Ok(s) if s.success() => println!("截图已存：{path}"),
        _ => eprintln!("截图失败，不影响测试结果：{path}"),
    }
}

fn pump_until_mapped(widget: &impl IsA<gtk::Widget>) {
    for _ in 0..100 {
        while glib::MainContext::default().iteration(false) {}
        if widget.is_mapped() {
            return;
        }
        glib::MainContext::default().iteration(true);
    }
    panic!("控件一直没映射，无法量实际颜色");
}

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
    let win = gtk::Window::new();
    win.set_default_size(720, 240);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_top(24);
    row.set_margin_bottom(24);
    row.set_margin_start(24);
    row.set_margin_end(24);
    win.set_child(Some(&row));
    win.present();

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
        check_solid_button(t, &row);
        checked += 1;
    }
    assert!(checked >= 3, "至少要验到 3 套主题，实得 {checked}");
    if !skipped.is_empty() {
        eprintln!("（跳过：{}）", skipped.join("、"));
    }
    win.close();
    println!("contrast_smoke: OK");
}

fn check_solid_button(t: Theme, row: &gtk::Box) {
    assert!(theme::apply(t), "{t:?}：主题没有装上");
    let primary = gtk::Button::with_label("主操作");
    primary.add_css_class("pp-primary");
    let suggested = gtk::Button::with_label("建议操作");
    suggested.add_css_class("suggested-action");
    let danger = gtk::Button::with_label("危险操作");
    danger.add_css_class("destructive-action");
    let checked = gtk::ToggleButton::with_label("选中操作");
    checked.set_active(true);
    for b in [&primary, &suggested, &danger] {
        row.append(b);
    }
    row.append(&checked);
    pump_until_mapped(&primary);
    pump_until_mapped(&suggested);
    pump_until_mapped(&danger);
    pump_until_mapped(&checked);

    let probe = gtk::Label::new(None);
    let sc = probe.style_context();
    let accent = resolved_color(&sc, "pp_accent");
    let err = resolved_color(&sc, "pp_err");
    let accent_hover = resolved_color(&sc, "pp_accent_hover");
    let accent_active = resolved_color(&sc, "pp_accent_active");
    let err_hover = resolved_color(&sc, "pp_err_hover");
    let err_active = resolved_color(&sc, "pp_err_active");
    let on_accent = resolved_color(&sc, "pp_on_accent");
    let on_err = resolved_color(&sc, "pp_on_err");
    let states = [
        ("primary", "pp_accent", &primary, accent.clone()),
        (
            "primary:hover",
            "pp_accent_hover",
            &primary,
            accent_hover.clone(),
        ),
        (
            "primary:active",
            "pp_accent_active",
            &primary,
            accent_active.clone(),
        ),
        ("suggested-action", "pp_accent", &suggested, accent.clone()),
        (
            "suggested-action:hover",
            "pp_accent_hover",
            &suggested,
            accent_hover.clone(),
        ),
        (
            "suggested-action:active",
            "pp_accent_active",
            &suggested,
            accent_active,
        ),
        ("destructive-action", "pp_err", &danger, err.clone()),
        (
            "destructive-action:hover",
            "pp_err_hover",
            &danger,
            err_hover,
        ),
        (
            "destructive-action:active",
            "pp_err_active",
            &danger,
            err_active,
        ),
        (
            "checked",
            "pp_accent",
            checked.upcast_ref::<gtk::Button>(),
            accent.clone(),
        ),
    ];
    for (name, token, button, background) in states {
        let label = button
            .child()
            .and_then(|w| w.downcast::<gtk::Label>().ok())
            .unwrap_or_else(|| panic!("{t:?} {name}：按钮没有 Label"));
        let actual_fg = rgba_hex(&label.style_context().color());
        let ratio = theme::contrast_ratio(&actual_fg, &background);
        assert!(
            ratio >= theme::CONTRAST_AA,
            "{t:?} {name}：GTK 实际 Label 色 {actual_fg} / {token} 背景 {background} = {ratio:.2}:1"
        );
    }
    let token_ratio = theme::contrast_ratio(&on_accent, &accent);
    assert!(
        token_ratio >= theme::CONTRAST_AA,
        "{t:?}：accent_fg {on_accent} / accent_bg {accent} = {token_ratio:.2}:1"
    );
    let danger_ratio = theme::contrast_ratio(&on_err, &err);
    assert!(
        danger_ratio >= theme::CONTRAST_AA,
        "{t:?}：danger_fg {on_err} / danger_bg {err} = {danger_ratio:.2}:1"
    );
    println!("{t:?}：实心按钮实际 Label 色与各状态背景对比度够 ✓");

    primary.set_sensitive(false);
    pump_until_mapped(&primary);
    let disabled_label = primary
        .child()
        .and_then(|w| w.downcast::<gtk::Label>().ok())
        .expect("主按钮有 Label");
    let disabled_fg = rgba_hex(&disabled_label.style_context().color());
    println!("{t:?}：禁用态单独记录 {disabled_fg}（不作为 AA 通过项）");
    if let Some(base) = std::env::var_os("PP_CONTRAST_SHOT") {
        shot(&format!("{}-{t:?}.png", base.to_string_lossy()));
    }
    row.remove(&primary);
    row.remove(&suggested);
    row.remove(&danger);
    row.remove(&checked);
}
