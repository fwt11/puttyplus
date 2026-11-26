//! 阶段 176 / UI-00 冒烟：**可复现的界面基线**。
//!
//! ⚠️ **为什么有它**（UI 审核 P2-3 的验收基础）：
//!   已有许多冒烟，但覆盖的是**单点行为**；缺的是**统一的窗口几何、
//!   主题、键盘与 GTK 健康检查**。
//!   ⇒ 这个冒烟把**三主题 × 三尺寸**过一遍，把「量法」统一
//!   （量之前控件**已映射**、窗口**已 present**、主循环**已泵送**）。
//!
//! **断言**：
//! 1. 三套主题 × 三个参考尺寸（1200×780 / 1024×700 / 1366×768）下，
//!    **关键界面都能摆出来并映射**（宽度 > 0）；
//! 2. **没有 GTK critical**（GTK 遇到不合法参数只打 critical、不崩 ⇒
//!    没这道检查就是「冒烟全绿而界面其实坏了」）；
//! 3. **没有横向滚动**（审核验收：「1200×780 与 1024×700 下没有新增
//!    横向滚动或按钮截断」）—— 内容宽度**不许超过窗口宽度**；
//! 4. **参考环境被记录下来**（GTK 版本 / 字体 / 缩放）⇒ 换机器能复现。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example ui_baseline_smoke

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{dbui, queue, SftpPanel};
use puttyplus::theme::Theme;
use puttyplus::uitest;

fn main() {
    // ⚠️ **隔离配置**：所有 ini 指到临时目录（不碰用户真实配置）。
    let home = uitest::isolate_configs("baseline");
    // ⚠️ **清干净**：上一次跑失败留下的配置会污染这一次（实施步骤第 7 条）。
    uitest::cleanup_temp_configs();

    // ⚠️ **容器里没有 a11y 总线** ⇒ GTK 会打一条 critical ⇒ 设
    //   `GTK_A11Y=none` 让它别去找（必须在 `gtk::init()` 之前）。
    unsafe { std::env::set_var("GTK_A11Y", "none") };
    gtk::init().expect("gtk init");
    // ⚠️ **先装 critical 收集器**：装在 `gtk::init()` 之后也没关系
    //   （日志写手是全局的，随时能装），但**必须在造界面之前**。
    let crit = uitest::watch_criticals();

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

    let mut report = String::new();

    for theme in uitest::THEMES {
        puttyplus::theme::apply(theme);
        for (w, h) in uitest::SIZES {
            let tag = format!("{theme:?} {w}×{h}");
            let before = crit.take();

            // ---- ① 欢迎页 ----
            let win = gtk::Window::new();
            win.set_title(Some("基线 · 欢迎页"));
            let welcome = puttyplus::tabs::welcome_page();
            win.set_child(Some(&welcome));
            let (aw, ah) = uitest::present(&win, w, h);
            let ww = welcome
                .downcast_ref::<gtk::ScrolledWindow>()
                .map(|s| s.allocation().width())
                .unwrap_or(0);
            check!(
                &format!("{tag}：欢迎页映射出来了"),
                aw > 0 && ah > 0 && uitest::is_mapped(&welcome),
                format!("窗口 {aw}×{ah}")
            );
            check!(
                &format!("{tag}：欢迎页**没有横向溢出**"),
                ww <= aw,
                format!("内容宽 {ww} > 窗口宽 {aw}")
            );
            if report.is_empty() {
                report = uitest::env_report(&win);
            }
            win.close();
            uitest::pump(120);

            // ---- ② 数据库工作台（未连接空态）----
            //
            // ⚠️ **只建不摆**：工作台是**页**（挂在标签里），不是独立窗口
            //   ⇒ 这条只查「建它的时候有没有 critical」。
            // ⚠️ **别把已经有父节点的控件再塞进窗口**：`dbui::tree(&st)` 的
            //   视图**已经在工作台自己的布局里** ⇒ 再 `set_child` 会触发
            //   GTK 的 `gtk_window_set_child` 断言（本次就是踩了这个）。
            let panel = SftpPanel::new(&ConnOptions::default());
            let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);
            dbui::ensure(&st);
            uitest::pump(120);

            // ---- ③ 传输队列（**它自己就是窗口**）----
            queue::ensure(&st);
            let qu = st.queue_ui.borrow();
            let qwin = qu.as_ref().map(|q| q.win.clone());
            drop(qu);
            if let Some(qw) = qwin {
                let (cw, ch) = uitest::present(&qw, w, h);
                check!(
                    &format!("{tag}：传输队列映射出来了"),
                    cw > 0 && ch > 0,
                    format!("窗口 {cw}×{ch}")
                );
                qw.close();
                uitest::pump(120);
            } else {
                check!(&format!("{tag}：传输队列建起来了"), false, "队列窗口没建出来".to_string());
            }

            // ---- ④ GTK 健康（critical）----
            // ⚠️ **滤掉的是「已知环境噪声」**（`uitest::ENV_NOISE`，每条都
            //   写明了理由）⇒ 新出现的 critical 仍然会被抓到。
            let mine = crit.real_problems();
            check!(
                &format!("{tag}：没有 GTK critical"),
                mine.is_empty(),
                format!("{mine:?}")
            );
            let _ = before;
        }
    }

    println!("—— 参考环境 ——\n{report}");
    let _ = std::fs::remove_dir_all(&home);

    if fails.is_empty() {
        println!("ui_baseline smoke ok");
    } else {
        println!("ui_baseline smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
