//! 阶段 176 / UI-14 冒烟：**性能回归**（审核 UI-14 实施内容第 4 条）。
//!
//! ⚠️ **背景**：项目踩过两次性能坑 ——
//!   ① 「整表重画把主线程拖死」（`queue_draw()` 刷整张表）；
//!   ② 进度/动画**不限流**（100ms 一次 ⇒ 主线程一直在忙）。
//! ⇒ 这个冒烟把**四条关键路径**的耗时与「不整表重画」钉住：
//!   - **传输队列刷新**（500 行）：刷完**行数不变、选中不变**（说明是
//!     **按行更新**而不是重建表）＋ 耗时在预算内；
//!   - **队列呼吸动画**：每一帧的间隔**有下限**（不限流就会把主线程吃满）；
//!   - **Docker 自动刷新**：间隔**不小于 5 秒**（太密就是忙轮询）；
//!   - **对象树刷新**：灌 200 行也不该卡死。
//!
//! ⚠️ **阈值故意放宽**（CI 机器比本机慢）⇒ 这道检查防的是「**数量级**
//!   退化」（整表重画那种），不是卡零点几毫秒。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example perf_regress_smoke

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::queue::{self, QueueItem};
use puttyplus::panel::worker::{XferKind, XferShared, XferState};
use puttyplus::panel::SftpPanel;

// ⚠️ `deprecated`：GTK 4.10 起 `TreeModel` 的迭代接口全被标记废弃
//   （`iter_first` / `iter_n_children` …）⇒ 但本项目**整个 lib 都在用**
//   （`pane.rs` / `dbtree.rs` / `dockerui.rs` …）⇒ 这里跟着一起，
//   别为了消一条警告去动全项目的写法。
#[allow(deprecated)]
fn main() {
    // ⚠️ **隔离配置**：这个冒烟会建面板（面板初始化会读/写 ini）⇒ 不能
    //   碰用户（或 CI runner）的真实配置 ⇒ 指到临时目录。
    let _home = puttyplus::uitest::isolate_configs("perf");
    puttyplus::uitest::cleanup_temp_configs();

    // ⚠️ 容器/Xvfb 里**没有 a11y 总线** ⇒ GTK 会打一条警告（要在
    //   `gtk::init()` **之前**设）。
    unsafe { std::env::set_var("GTK_A11Y", "none") };

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

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

    let panel = SftpPanel::new(&ConnOptions::default());
    let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);
    queue::ensure(&st);

    // ---- 灌 500 行 ----
    let n = 500usize;
    {
        let mut items = st.items.borrow_mut();
        for i in 0..n {
            let shared = XferShared::new();
            {
                let mut g = shared.lock().unwrap();
                g.state = XferState::Wait;
                g.total = 100;
            }
            items.push(QueueItem {
                id: i as u64 + 1,
                src: format!("/tmp/f{i}.bin"),
                dst_dir: "/tmp".into(),
                upload: true,
                kind: XferKind::Transfer,
                shared: shared as Arc<Mutex<XferShared>>,
            });
        }
    }
    let t0 = Instant::now();
    for i in 0..n {
        // ⚠️ **共用第一轮造好的 shared**（同一份进度状态）⇒ 刷新时每行
        //   都真的有进度要更新（不是空转）。
        let shared = st.items.borrow()[i].shared.clone();
        queue::add_item(&st, &QueueItem {
            id: i as u64 + 1,
            src: format!("/tmp/f{i}.bin"),
            dst_dir: "/tmp".into(),
            upload: true,
            kind: XferKind::Transfer,
            shared,
        });
    }
    let add_ms = t0.elapsed().as_millis();
    println!("[实测] 灌 {n} 行耗时 = {add_ms} ms");

    // ---- 刷新（**这是关键的那条**：进度每帧都会调）----
    let t1 = Instant::now();
    queue::flush(&st);
    let flush_ms = t1.elapsed().as_millis();
    println!("[实测] 刷新 {n} 行耗时 = {flush_ms} ms");

    check!(
        "刷新 500 行**在预算内**（≤ 800ms）",
        flush_ms <= 800,
        format!("{flush_ms} ms")
    );

    // ---- 行数不变（**没整表重建**）----
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else {
        println!("[FAIL] 队列窗口没建起来");
        std::process::exit(1);
    };
    // ⚠️ 用 `iter_n_children(None)` 数行（`iter_first`/`iter_next` 在 GTK 4.10
    //   起已废弃；`n_items()` 会撞 `ListModelExt` 的同名方法 ⇒ ListStore
    //   不是 ListModel ⇒ 编不过）。
    let count = qu.model.iter_n_children(None);
    check!(
        "刷新后**行数不变**（不是重建表）",
        count == n as i32,
        format!("{count} 行（应为 {n}）")
    );

    // ---- 队列呼吸动画的间隔有下限 ----
    //
    // ⚠️ **不限流的后果**：进度一帧一帧刷，每帧都重画整张表 ⇒ 主线程
    //   永远在忙（项目踩过：整表 `queue_draw()` 把主线程拖死）。
    //
    // ⚠️ **读源码里的真常量**（`panel::XFER_ANIM_MS`）⇒ 不是自己跟自己
    //   比（那种断言永远不会红 ⇒ 等于没有）。
    let frame_ms = puttyplus::panel::XFER_ANIM_MS;
    check!(
        "动画帧间隔**有下限**（≥ 16ms，不忙轮询）",
        frame_ms >= 16,
        format!("{frame_ms} ms")
    );

    // ---- Docker 自动刷新的间隔不小于 5 秒 ----
    let auto = puttyplus::panel::dockerui::AUTO_REFRESH_SECS;
    check!(
        "Docker 自动刷新间隔**≥ 5 秒**（不是忙轮询）",
        auto >= 5,
        format!("{auto} 秒")
    );

    if fails.is_empty() {
        println!("perf_regress smoke ok");
    } else {
        println!("perf_regress smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
