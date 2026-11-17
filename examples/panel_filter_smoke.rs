//! 文件面板**按文件名过滤**冒烟（用户 2026-11-17 那一串「查找」需求的一部分）。
//!
//! **为什么做这个**：目录里文件一多，用户想只看**名字带某个词**的那些
//! ⇒ 面板工具条上加一个 🔍（点开是过滤框）⇒ 输入即过滤。
//!
//! **断言**（都落在「动作的结果」上）：
//! 1. 没过滤时列���全部条目；
//! 2. 点 🔍 ⇒ 过滤框出现；输入 ⇒ **只留下名字含这个词的**；
//! 3. 过滤**不区分大小写**（敲 "TXT" 能找到 "a.txt"）；
//! 4. 清空过滤词 ⇒ 全部条目回来；
//! 5. 关掉 🔍 ⇒ **过滤词被清掉、全部条目回来**（不然用户以为文件弄丢了）；
//! 6. 远程面板也有同一个过滤入口（两个面板各管各的）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example panel_filter_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::pane;

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

/// 面板当前列出的**文件名**（不含 ".."）。
fn names(p: &std::rc::Rc<pane::Pane>) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = p.model.iter_first();
    while let Some(mut i) = it {
        let n: String = p.model.get(&i, pane::COL_NAME);
        if n != ".." {
            out.push(n);
        }
        it = if p.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    out
}

fn main() {
    // 一个临时目录：三个文件 ＋ 一个目录。
    let dir = std::env::temp_dir().join(format!("pp_filter_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("alpha.txt"), b"x").unwrap();
    std::fs::write(dir.join("beta.log"), b"x").unwrap();
    std::fs::write(dir.join("gamma.txt"), b"x").unwrap();
    std::fs::create_dir_all(dir.join("subdir")).unwrap();
    unsafe {
        std::env::set_var("PUTTYPLUS_LOCALDIR", &dir);
    }

    gtk::init().expect("gtk init");
    let panel = puttyplus::panel::SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(800);

    let lp = panel.st.pane(false);
    let rp = panel.st.pane(true);

    // ---- 1) 起点：全部条目都在 ----
    let all = names(&lp);
    assert!(all.contains(&"alpha.txt".to_string()), "起点缺 alpha.txt：{all:?}");
    assert!(all.contains(&"beta.log".to_string()), "起点缺 beta.log：{all:?}");
    assert!(all.contains(&"gamma.txt".to_string()), "起点缺 gamma.txt：{all:?}");
    println!("[1] 起点列了 {} 项：{all:?}", all.len());

    // ---- 2) 过滤框默认藏着；点 🔍 才出来 ----
    assert!(!gtk::prelude::WidgetExt::is_visible(&lp.filter_entry), "过滤框默认应当是藏着的");
    assert!(!lp.filter_btn.is_active(), "🔍 默认应当是没按下的");
    lp.filter_btn.set_active(true);
    pump(200);
    assert!(gtk::prelude::WidgetExt::is_visible(&lp.filter_entry), "点了 🔍 过滤框应当出现");
    println!("[2] 点 🔍 ⇒ 过滤框出现了 ✔");

    // ---- 3) 输入 ⇒ 只留名字含这个词的 ----
    lp.filter_entry.set_text("log");
    pump(400); // 过滤有 120ms 去抖
    let hit = names(&lp);
    assert!(
        hit.contains(&"beta.log".to_string()) && hit.len() == 1,
        "过滤「log」之后应当只剩 beta.log，实际 {hit:?}"
    );
    println!("[3] 过滤「log」⇒ 只剩 {hit:?} ✔");

    // ---- 4) 过滤**不区分大小写** ----
    lp.filter_entry.set_text("TXT");
    pump(400);
    let hit = names(&lp);
    let mut hit_sorted = hit.clone();
    hit_sorted.sort();
    assert_eq!(
        hit_sorted,
        vec!["alpha.txt".to_string(), "gamma.txt".to_string()],
        "过滤「TXT」应当留下两个 .txt，实际 {hit:?}"
    );
    println!("[4] 过滤「TXT」⇒ {hit:?}（不区分大小写 ✔）");

    // 截图（可选）：PP_FILTER_SHOT=/tmp/x.png
    //
    // ⚠️ 窄面板里第一行本来就挤（标题 + 状态 + 传输按钮）⇒ 过滤框露出来
    //   之后**状态文字会不会被挤没** ⇒ 只能靠眼睛看 ⇒ 留个截图口。
    if let Ok(p) = std::env::var("PP_FILTER_SHOT") {
        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg(&p)
            .status();
        println!("[4b] 截图：{p}");
    }

    // ---- 5) 清空 ⇒ 全部回来 ----
    lp.filter_entry.set_text("");
    pump(400);
    let back = names(&lp);
    assert_eq!(back.len(), all.len(), "清空过滤词后应当全部回来：{back:?}");
    println!("[5] 清空过滤词 ⇒ 全部 {back:?} 回来了 ✔");

    // ---- 6) 关掉 🔍 ⇒ 过滤词被清掉、全部回来 ----
    lp.filter_entry.set_text("log");
    pump(400);
    assert_eq!(names(&lp).len(), 1, "过滤应当生效");
    lp.filter_btn.set_active(false);
    pump(400);
    assert!(lp.filter_entry.text().is_empty(), "关掉 🔍 应当把过滤词清掉");
    assert_eq!(
        names(&lp).len(),
        all.len(),
        "关掉 🔍 应当把完整列表还回来"
    );
    println!("[6] 关掉 🔍 ⇒ 过滤词清掉、列表复原 ✔");

    // ---- 7) 远程面板也有同一个入口 ----
    assert!(!rp.filter_btn.is_active(), "远程面板也该有 🔍");
    rp.filter_btn.set_active(true);
    pump(200);
    assert!(gtk::prelude::WidgetExt::is_visible(&rp.filter_entry), "远程面板点 🔍 也该出过滤框");
    println!("[7] 远程面板也有过滤入口 ✔");

    puttyplus::panel::shutdown(&panel.st);
    let _ = std::fs::remove_dir_all(&dir);
    println!("panel filter smoke ok");
}
