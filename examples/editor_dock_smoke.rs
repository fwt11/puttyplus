//! 阶段 137 冒烟：**编辑器的三个停靠位**（主栏 / 底部 / 侧栏）。
//!
//! **背景**（用户 2026-11-19）：
//! - 「现在打开文本文档，会先和 term 显示在同一个窗口……我希望修改回去，
//!   就是默认打开还是显示为**和 term 并列的 tab**」；
//! - 「但是我希望这个 tab 可以**自由停靠在屏幕的多个位置**，就像很多软件
//!   那样，比如 vscode」；
//! - 「容器界面如果打开 docker compose 文件，我也希望它可以停靠在多个位置」。
//!
//! **断言**（**不连服务器** ⇒ 布局与拖放位）：
//! 1. **默认 = 主栏页签**（与终端并列）；
//! 2. 能挪到**底部编辑区**（终端列下面）；
//! 3. 能挪到**侧栏**（左右分栏）⇒ 侧栏露出来、有自己的页签；
//! 4. 能从侧栏**挪回主栏**；
//! 5. 侧栏能换到**左边**（拖到窗口左边缘）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example editor_dock_smoke

use puttyplus::args::ConnOptions;
use puttyplus::panel::{SftpPanel, fileedit, subtabs, subtabs::EditorPlace};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn main() {
    let settings_ini = std::env::temp_dir().join(format!("pp-editor-dock-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    let tmp = std::env::temp_dir().join(format!("pp_editor_dock_{}.txt", std::process::id()));
    std::fs::write(&tmp, "hello\n").expect("写临时文件");

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

    // 一个面板（连不上也没关系 ⇒ 这里只关心布局与停靠位）。
    let panel = SftpPanel::new(&ConnOptions::default());
    // ⚠️ **子标签容器要自己装上**（面板自己不建 ⇒ 上层搭页面时才建）。
    *panel.st.sub_tabs.borrow_mut() = Some(subtabs::SubTabs::new());
    let sub = panel.st.sub_tabs().expect("应当有子标签容器");
    pump(200);

    let pages_before = sub.page_count();

    // ---- ① 默认：打开文件 ⇒ **与终端并列**的主栏页签 ----
    fileedit::open_local(&panel.st, tmp.to_str().unwrap(), false);
    pump(400);
    let eid = fileedit::editor_id_for(&panel.st, tmp.to_str().unwrap());
    check!(
        "① 打开文件 ⇒ 默认是与终端并列的主栏页签",
        sub.editor_place(eid) == Some(EditorPlace::Main),
        format!("现在停在 {:?}", sub.editor_place(eid))
    );
    check!(
        "① 主栏多了一个页签（与终端并列）",
        sub.page_count() == pages_before + 1,
        format!("页签数 {} → {}", pages_before, sub.page_count())
    );
    check!(
        "① 底部编辑区这时没有编辑器（那只是另一个停靠位）",
        sub.editor_is_docked(eid) == Some(false),
        "编辑器已经在底部了"
    );

    // ---- ② 能挪到**底部编辑区** ----
    check!(
        "② 能挪到底部编辑区",
        sub.move_editor_to_bottom(eid),
        "挪不过去"
    );
    pump(300);
    check!(
        "② 底部时编辑区露出来了",
        sub.editor_area_visible(),
        "编辑区是隐的（开着编辑器却看不见）"
    );

    // ---- ③ 能挪到**侧栏**（左右分栏）----
    check!("③ 能挪到侧栏", sub.move_editor_to_side(eid), "挪不过去");
    pump(300);
    check!(
        "③ 侧栏有自己的页签（VS Code 的 editor group 那样）",
        sub.side_page_count() == 1,
        format!("侧栏页签数 {}", sub.side_page_count())
    );
    check!(
        "③ 底部这时空了（编辑器不在那儿了）",
        sub.editor_is_docked(eid) == Some(false),
        "编辑器还在底部"
    );

    // ---- ④ 能从侧栏挪回主栏 ----
    check!("④ 能从侧栏挪回主栏", sub.move_editor_to_main(eid), "挪不过去");
    pump(300);
    check!(
        "④ 回到主栏 ⇒ 侧栏空了",
        sub.side_page_count() == 0,
        format!("侧栏还有 {} 个页签", sub.side_page_count())
    );
    check!(
        "④ 回到主栏 ⇒ 主栏页签还在",
        sub.page_count() == pages_before + 1,
        format!("页签数 {}", sub.page_count())
    );

    // ---- ⑤ 侧栏能换到**左边**（拖到窗口左边缘）----
    sub.move_editor_to_side(eid);
    pump(200);
    sub.set_side_left_pub(true);
    pump(200);
    check!("⑤ 侧栏能换到左边", sub.side_is_left(), "侧栏还在右边");
    sub.set_side_left_pub(false);
    pump(200);
    check!("⑤ 侧栏也能换回右边", !sub.side_is_left(), "侧栏还在左边");

    let _ = std::fs::remove_file(&tmp);
    if fails.is_empty() {
        println!("editor dock smoke ok");
    } else {
        println!("[FAIL] {} 项没过：", fails.len());
        for f in &fails {
            println!("  - {f}");
        }
        std::process::exit(101);
    }
}
