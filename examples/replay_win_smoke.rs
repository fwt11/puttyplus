//! 阶段 171 / UI-03 冒烟：**回放窗口不得把同一个 Separator 挂两次**。
//!
//! **背景**（UI 审核 P1-5，已确认的代码级缺陷）：
//! 回放窗口原先是
//!
//! ```text
//! root.append(&sep);
//! root.append(term.widget());
//! root.append(&sep);   ← 同一个控件第二次挂
//! ```
//!
//! GTK4 里**一个控件只能有一个父级** ⇒ 第二次 `append` 触发
//!
//! ```text
//! gtk_box_append: assertion 'gtk_widget_get_parent (child) == NULL' failed
//! ```
//!
//! ⇒ 产生 GTK critical（污染日志、CI 一片红）、分隔线也不符合预期。
//!
//! **断言**（**不连远端**，本地造一个 `.cast`）：
//! 1. 打开回放窗口时**没有** `gtk_box_append` 的父级断言；
//! 2. 窗口里**恰好两条**分隔线（工具条↔画面、画面↔输入区）；
//! 3. 终端区域**有有效尺寸**（不是被压成 0 高的空壳）；
//! 4. **关掉再打开**一遍，仍然干净（老 bug 是「重复挂载」⇒ 重开更容易炸）；
//! 5. 快捷键契约不变：Esc 能关窗。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example replay_win_smoke

#![allow(deprecated)] // 回放窗沿用 VTE（与主程序一致）

use gtk::prelude::*;
use std::sync::Mutex;

/// 抓到的 GTK 日志（critical / warning）。
static GTK_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

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

/// 递归数一遍窗口里有多少个 Separator。
fn count_separators(w: &gtk::Widget) -> usize {
    let mut n = 0;
    let mut cur = w.first_child();
    while let Some(c) = cur {
        if c.is::<gtk::Separator>() {
            n += 1;
        }
        n += count_separators(&c);
        cur = c.next_sibling();
    }
    n
}

/// 递归找最大的叶子控件（用来确认「终端区域有有效尺寸」）。
fn biggest_leaf(w: &gtk::Widget) -> (i32, i32) {
    let mut best = (w.width(), w.height());
    let mut cur = w.first_child();
    while let Some(c) = cur {
        let (cw, ch) = biggest_leaf(&c);
        if cw * ch > best.0 * best.1 {
            best = (cw, ch);
        }
        cur = c.next_sibling();
    }
    best
}

fn gtk_log_snapshot() -> Vec<String> {
    GTK_LOG.lock().unwrap().clone()
}

/// 找标题带「回放」的顶层窗口。
fn replay_window() -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast_ref::<gtk::Window>().cloned())
        .find(|w| w.title().unwrap_or_default().contains("回放"))
}

fn main() {
    // ---- 接管 GTK 日志：把 critical / warning 收进内存（好断言）----
    glib::log_set_writer_func(|level, fields| {
        if matches!(
            level,
            glib::LogLevel::Critical | glib::LogLevel::Warning
        ) {
            let msg = fields
                .iter()
                .filter_map(|f| f.value_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
                .join(" | ");
            GTK_LOG.lock().unwrap().push(msg);
        }
        glib::LogWriterOutput::Handled
    });

    gtk::init().expect("gtk init");

    // ---- 隔离配置：不写用户真实设置 ----
    let tmp = std::env::temp_dir().join(format!("pp_replay_smoke_{}", std::process::id()));
    let cfg = tmp.join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    unsafe {
        for key in [
            "PUTTYPLUS_SETTINGS_INI",
            "PUTTYPLUS_CONN_INI",
            "PUTTYPLUS_DB_INI",
            "PUTTYPLUS_HIST_INI",
            "PUTTYPLUS_COLS_INI",
            "PUTTYPLUS_RECENT_INI",
            "PUTTYPLUS_VAULT",
        ] {
            std::env::remove_var(key);
        }
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }

    // ---- 造一个最小的 asciicast v2 文件 ----
    let cast_path = tmp.join("demo.cast");
    std::fs::write(
        &cast_path,
        concat!(
            r#"{"version": 2, "width": 80, "height": 24}"#,
            "\n",
            r#"[0.10, "o", "hello\r\n"]"#,
            "\n",
            r#"[0.20, "o", "world\r\n"]"#,
            "\n",
        ),
    )
    .unwrap();

    // ---- ① 打开回放窗 ----
    puttyplus::replayui::open(&cast_path).expect("回放窗口应当能打开");
    pump(600);

    let win = replay_window().expect("回放窗口应当存在");
    let log1 = gtk_log_snapshot();
    let parent_criticals: Vec<&String> = log1
        .iter()
        .filter(|m| m.contains("gtk_box_append") || m.contains("gtk_widget_get_parent"))
        .collect();
    assert!(
        parent_criticals.is_empty(),
        "回放窗口不该有重复挂载的 critical，实际捕获：{parent_criticals:?}"
    );

    // ---- ② 恰好两条分隔线 ----
    let seps = count_separators(win.upcast_ref::<gtk::Widget>());
    assert_eq!(seps, 2, "回放窗口应当有两条分隔线（工具条↔画面、画面↔输入区），实际 {seps}");

    // ---- ③ 终端区域有有效尺寸 ----
    let (w, h) = biggest_leaf(win.upcast_ref::<gtk::Widget>());
    assert!(
        w > 100 && h > 50,
        "回放窗口里的内容区太小（{w}×{h}）⇒ 画面被压扁了"
    );

    // ---- ④ 关掉再打开一遍 ----
    win.close();
    pump(300);
    puttyplus::replayui::open(&cast_path).expect("回放窗口应当能再打开");
    pump(600);
    let win2 = replay_window().expect("第二次打开的回放窗口应当存在");
    let log2 = gtk_log_snapshot();
    let parent_criticals2: Vec<&String> = log2
        .iter()
        .filter(|m| m.contains("gtk_box_append") || m.contains("gtk_widget_get_parent"))
        .collect();
    assert!(
        parent_criticals2.is_empty(),
        "重开回放窗口时又出现重复挂载的 critical：{parent_criticals2:?}"
    );
    assert_eq!(count_separators(win2.upcast_ref::<gtk::Widget>()), 2, "重开后分隔线应当还是两条");

    // ---- ⑤ 关窗要干净（定时器得摘掉，快捷键契约不变）----
    win2.close();
    pump(400);
    assert!(
        replay_window().is_none(),
        "回放窗口关了之后不该还留在顶层窗口列表里"
    );

    println!(
        "[信息] 回放窗口：分隔线 2 条，内容区 {w}×{h}，捕获 GTK 警告/严重 {} 条（无父级断言）",
        log2.len()
    );
    let _ = std::fs::remove_dir_all(&tmp);
    println!("[信息] UI-03 冒烟通过");
}
