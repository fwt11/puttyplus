//! 阶段 175 / UI-11 冒烟：**通知按级别分组 ＋ 队列摘要与筛选**（治 P2-4）。
//!
//! ⚠️ **背景**（UI 审核 P2-4）：通知中心是**平铺的时间列表**（用户得一条条
//!   读才知道哪条要管）；传输队列**信息完整但失败项处理入口不突出**，也没有
//!   失败筛选。
//!
//! **断言**：
//! 1. **通知按级别分组**：「失败」组排在「警告」「成功」**前面**
//!    （用户先看到需要处理的事项）；
//! 2. **级别用文字 ＋ 图标表达**（不只靠颜色）；
//! 3. 通知**带来源与批次**（同批的归在一起；进程内语义，不落盘）；
//! 4. **队列顶部有只读摘要**：完成 / 失败 / 跳过 / 取消各多少；
//! 5. **失败筛选只显示失败项**；
//! 6. **清除筛选后原顺序稳定**（正在传的行不会被重排）；
//! 7. **「重新传输」不可用时说明原因**（灰掉 + 说清为什么）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example queue_group_smoke

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::notify;
use puttyplus::panel::queue::{self, QueueItem};
use puttyplus::panel::worker::{XferKind, XferShared, XferState};
use puttyplus::panel::SftpPanel;

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

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

fn labels_of(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out = Vec::new();
    walk(&root, &mut |w| {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.text().to_string());
        }
    });
    out
}

fn has_icon(root: &impl IsA<gtk::Widget>) -> bool {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut found = false;
    walk(&root, &mut |w| {
        if w.downcast_ref::<gtk::Image>().is_some() {
            found = true;
        }
    });
    found
}

/// 造一个传输项（带状态）。
fn item(id: u64, name: &str, state: XferState, msg: Option<&str>) -> QueueItem {
    let shared = XferShared::new();
    {
        let mut g = shared.lock().unwrap();
        g.state = state;
        g.msg = msg.map(|m| m.to_string());
        if state == XferState::Done {
            g.done = 10;
            g.total = 10;
        }
    }
    QueueItem {
        id,
        src: format!("/tmp/{name}.txt"),
        dst_dir: "/tmp".to_string(),
        upload: true,
        kind: XferKind::Transfer,
        shared: shared as Arc<Mutex<XferShared>>,
    }
}

/// 队列窗口里**当前可见**的行（按显示顺序）。
fn visible_rows(st: &Rc<puttyplus::panel::PanelState>) -> Vec<String> {
    let qu = st.queue_ui.borrow();
    let Some(qu) = qu.as_ref() else { return Vec::new() };
    let Some(model) = qu.view.model() else { return Vec::new() };
    let mut out = Vec::new();
    let mut it = model.iter_first();
    while let Some(mut ti) = it {
        out.push(model.get::<String>(&ti, queue::QCOL_FILE));
        it = if model.iter_next(&mut ti) { Some(ti) } else { None };
    }
    out
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

    /* ---------- ① 通知：级别 ＋ 来源 ＋ 批次 ---------- */
    {
        notify::desktop_action(
            notify::Level::Success,
            "传输完成",
            "3 个文件都传完了",
            "查看明细",
            || {},
            || {},
        );
        notify::desktop_action(
            notify::Level::Error,
            "传输部分失败",
            "1 个文件没传上去：权限不足",
            "查看明细",
            || {},
            || {},
        );
        notify::desktop_action(
            notify::Level::Warn,
            "连接不稳",
            "重试了两次才连上",
            "查看明细",
            || {},
            || {},
        );
        let hist = notify::history();
        check!(
            "三条通知都记下来了",
            hist.len() == 3,
            format!("{} 条", hist.len())
        );
        check!(
            "每条都带**级别**",
            hist.iter().any(|e| e.level == notify::Level::Error)
                && hist.iter().any(|e| e.level == notify::Level::Warn)
                && hist.iter().any(|e| e.level == notify::Level::Success),
            format!("{hist:?}")
        );
        check!(
            "每条都带**来源**（传输）",
            hist.iter().all(|e| e.source == notify::Source::Xfer),
            format!("{hist:?}")
        );
        check!(
            "每条都带**批次号**（同批的归在一起）",
            hist.iter().all(|e| e.batch > 0),
            format!("{hist:?}")
        );
        check!(
            "级别各有**文字**（不只靠颜色）",
            !notify::Level::Error.label().is_empty()
                && !notify::Level::Warn.label().is_empty()
                && !notify::Level::Success.label().is_empty(),
            "级别没有文字".to_string()
        );
        check!(
            "级别各有**图标**",
            !notify::Level::Error.icon().is_empty()
                && !notify::Level::Warn.icon().is_empty()
                && !notify::Level::Success.icon().is_empty(),
            "级别没有图标".to_string()
        );
    }

    /* ---------- ② 通知中心：按级别分组 ---------- */
    {
        let win = gtk::Window::new();
        win.set_default_size(520, 420);
        win.present();
        pump(200);
        notify::open_center(Some(&win));
        pump(400);
        let center = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref().map(|t| t.contains("通知中心")).unwrap_or(false));
        check!(
            "通知中心窗口开着",
            center.is_some(),
            "没找到通知中心".to_string()
        );
        if let Some(c) = center {
            let texts = labels_of(&c);
            println!("[实测] 通知中心文字：{texts:?}");
            let pos = |s: &str| texts.iter().position(|t| t == s);
            check!(
                "有「失败」「警告」「成功」三个分组标题",
                pos("失败").is_some() && pos("警告").is_some() && pos("成功").is_some(),
                format!("{texts:?}")
            );
            if let (Some(e), Some(w), Some(s)) = (pos("失败"), pos("警告"), pos("成功")) {
                check!(
                    "**失败组排在最前**（用户先看到要处理的事项）",
                    e < w && w < s,
                    format!("失败={e} 警告={w} 成功={s}")
                );
            }
            check!(
                "通知中心里**有图标**（级别不只靠颜色）",
                has_icon(&c),
                "一个图标都没有".to_string()
            );
            c.close();
        }
        pump(150);
    }

    /* ---------- ③ 队列：摘要 ＋ 筛选 ---------- */
    {
        let panel = SftpPanel::new(&ConnOptions::default());
        let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);
        queue::ensure(&st);
        let items = vec![
            item(1, "a", XferState::Done, None),
            item(2, "b", XferState::Err, Some("权限不足")),
            item(3, "c", XferState::Wait, None),
            item(4, "d", XferState::Skip, None),
        ];
        {
            let mut list = st.items.borrow_mut();
            for it in &items {
                list.push(QueueItem {
                    id: it.id,
                    src: it.src.clone(),
                    dst_dir: it.dst_dir.clone(),
                    upload: it.upload,
                    kind: it.kind,
                    shared: it.shared.clone(),
                });
            }
        }
        for it in &items {
            queue::add_item(&st, it);
        }
        queue::flush(&st);
        pump(200);

        // ---- 摘要条 ----
        let qu = st.queue_ui.borrow();
        let summary = qu.as_ref().and_then(|q| q.summary.as_ref());
        check!(
            "队列顶部有只读摘要（完成/失败/跳过/取消）",
            summary.is_some(),
            "没有摘要".to_string()
        );
        if let Some(su) = summary {
            println!(
                "[实测] 摘要：完成={:?} 失败={:?} 跳过={:?} 取消={:?}",
                su.done.text(),
                su.failed.text(),
                su.skipped.text(),
                su.canceled.text()
            );
            check!(
                "摘要说清楚：完成 1 / 失败 1 / 跳过 1 / 取消 0",
                su.done.text() == "完成 1"
                    && su.failed.text() == "失败 1"
                    && su.skipped.text() == "跳过 1"
                    && su.canceled.text() == "取消 0",
                format!(
                    "{:?} / {:?} / {:?} / {:?}",
                    su.done.text(),
                    su.failed.text(),
                    su.skipped.text(),
                    su.canceled.text()
                )
            );
        }
        drop(qu);

        // ---- 筛选 ----
        let all = visible_rows(&st);
        println!("[实测] 全部筛选可见行：{all:?}");
        check!(
            "全部 ⇒ 四行都看得见",
            all.len() == 4,
            format!("{} 行", all.len())
        );

        queue::set_filter_mode(&st, queue::FILTER_FAILED);
        pump(150);
        let failed = visible_rows(&st);
        println!("[实测] 失败筛选可见行：{failed:?}");
        check!(
            "失败筛选**只显示失败项**",
            failed.len() == 1 && failed[0].contains('b'),
            format!("{failed:?}")
        );

        queue::set_filter_mode(&st, queue::FILTER_RUNNING);
        pump(150);
        let running = visible_rows(&st);
        println!("[实测] 进行中筛选可见行：{running:?}");
        check!(
            "进行中筛选只显示排队/正在传的",
            running.len() == 1 && running[0].contains('c'),
            format!("{running:?}")
        );

        queue::set_filter_mode(&st, queue::FILTER_ALL);
        pump(150);
        let again = visible_rows(&st);
        println!("[实测] 清除筛选后可见行：{again:?}");
        check!(
            "**清除筛选后原顺序稳定**（正在传的行不会被重排）",
            again == all,
            format!("之前 {all:?} / 之后 {again:?}")
        );

        // ---- 「重新传输」的可用性 ----
        let (can, why) = queue::retry_availability(&st);
        check!(
            "没选中失败项时「重新传输」不可用",
            !can,
            format!("can={can} why={why}")
        );
        check!(
            "不可用时**说明原因**（不是只灰着）",
            !why.is_empty(),
            "没有说明".to_string()
        );

        // 关掉队列窗口**不该取消传输**
        {
            let qu = st.queue_ui.borrow();
            let qu = qu.as_ref().unwrap();
            qu.win.close();
        }
        pump(150);
        let still_waiting = st
            .items
            .borrow()
            .iter()
            .any(|it| it.shared.lock().unwrap().state == XferState::Wait);
        check!(
            "**关闭队列不取消传输**",
            still_waiting,
            "等待中的那一项被取消了".to_string()
        );
    }

    if fails.is_empty() {
        println!("queue_group smoke ok");
    } else {
        println!("queue_group smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
