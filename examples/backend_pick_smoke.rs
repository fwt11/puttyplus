//! 终端引擎（libvte / ghostty）选择的冒烟（阶段 4）。
//!
//! **背景**：2026-11-10 用户拍板「Linux 默认构建就带 ghostty，用户可以在
//! 设置里选用 libvte 还是 ghostty」⇒ 两个后端编进同一个二进制 ⇒ 跨平台
//! 开发时不用改环境变量就能在 Linux 上跑 Windows 那条渲染路径。
//!
//! **断言**：
//! 1. 两个后端**都能用**时，首选项「终端」页有「终端引擎」下拉框；
//!    只有**一个**能用时（Windows：没有 VTE）**没有**这个下拉框
//!    —— 不给用户一个「选了也没用」的选项；
//! 2. 下拉框的当前值 = **实际生效**的后端（别出现"界面显示 A、实际跑 B"）；
//! 3. 切换下拉框 ⇒ 设置**真的写进了 ini**，读回来一致，
//!    `effective_term_backend()` 跟着变。
//!
//! ⚠️ 设置文件由**本例子自己**指到临时文件（`PUTTYPLUS_SETTINGS_INI`），
//!    不许写用户真实配置。
//!
//! ```text
//! xvfb-run -a cargo run --example backend_pick_smoke
//! ```

use gtk::prelude::*;
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

/// 递归找出某个容器里的全部 ComboBoxText。
fn combos(w: &gtk::Widget, out: &mut Vec<gtk::ComboBoxText>) {
    if let Some(c) = w.downcast_ref::<gtk::ComboBoxText>() {
        out.push(c.clone());
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        combos(&c, out);
        cur = c.next_sibling();
    }
}

/// 这就是「终端引擎」那个下拉框（按它自己的选项认，不靠位置）。
fn is_engine_combo(c: &gtk::ComboBoxText) -> bool {
    matches!(c.active_id().as_deref(), Some(id) if id == settings::BACKEND_VTE || id == settings::BACKEND_GHOSTTY)
}

fn main() {
    // ⚠️ **先指走设置文件**：examples/ 里的例子不许写用户真实配置。
    let dir = std::env::temp_dir().join(format!("pp-backend-pick-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let ini = dir.join("settings.ini");
    unsafe {
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &ini);
    }

    gtk::init().expect("gtk init");

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

    let vte_ok = settings::backend_usable(settings::BACKEND_VTE);
    let ghostty_ok = settings::backend_usable(settings::BACKEND_GHOSTTY);
    println!("本机可用：vte={vte_ok} ghostty={ghostty_ok}");

    // ---------- 1. 打开首选项的「终端」页，找出那个下拉框 ----------
    settingsui::open(Page::Terminal);
    pump(400);
    let nb = settingsui::pages_notebook().expect("首选项窗口应当开着");
    let page = nb.nth_page(Some(Page::Terminal.index())).expect("终端页");
    let mut all: Vec<gtk::ComboBoxText> = Vec::new();
    combos(&page, &mut all);
    let engines: Vec<gtk::ComboBoxText> = all.iter().filter(|c| is_engine_combo(c)).cloned().collect();

    if vte_ok && ghostty_ok {
        check!(
            "两个后端都能用 ⇒ 有「终端引擎」下拉框",
            engines.len() == 1,
            format!("找到 {} 个", engines.len())
        );
    } else {
        check!(
            "只有一个后端能用 ⇒ 不给「终端引擎」下拉框",
            engines.is_empty(),
            format!("还是找到了 {} 个", engines.len())
        );
    }

    // ---------- 2. 当前值 = 实际生效的后端 ----------
    if let Some(combo) = engines.first() {
        let want = settings::effective_term_backend();
        check!(
            "下拉框的当前值就是实际生效的后端",
            combo.active_id().as_deref() == Some(want),
            format!("界面 {:?} vs 实际 {want}", combo.active_id())
        );

        // ---------- 3. 切换 ⇒ 真的写进 ini，读回来一致 ----------
        for pick in [settings::BACKEND_GHOSTTY, settings::BACKEND_VTE] {
            if !settings::backend_usable(pick) {
                continue;
            }
            combo.set_active_id(Some(pick));
            pump(120);
            let stored = settings::term_backend();
            check!(
                &format!("选了 {pick} ⇒ 设置写进 ini 了"),
                stored.as_deref() == Some(pick),
                format!("读回来 {stored:?}")
            );
            check!(
                &format!("选了 {pick} ⇒ 实际生效的后端跟着变"),
                settings::effective_term_backend() == pick,
                format!("实际 {}", settings::effective_term_backend())
            );
            // ini 文件里确实有这一行（不是只在内存里）
            let text = std::fs::read_to_string(&ini).unwrap_or_default();
            check!(
                &format!("ini 里确实有 term_backend={pick}"),
                text.contains(&format!("term_backend={pick}")),
                format!("ini 内容：{text:?}")
            );
        }

        // ---------- 4. 选一个本机用不了的 ⇒ 退回平台默认（别卡住） ----------
        settings::set_term_backend(Some("不存在的引擎"));
        check!(
            "选了不存在的引擎 ⇒ 退回平台默认",
            settings::effective_term_backend() == settings::effective_term_backend_with(
                None,
                vte_ok,
                ghostty_ok
            ),
            format!("实际 {}", settings::effective_term_backend())
        );
    }

    let _ = std::fs::remove_dir_all(&dir);

    if fails.is_empty() {
        println!("backend_pick smoke ok");
    } else {
        println!("backend_pick smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
