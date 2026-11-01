//! 统一关闭确认的冒烟（审计 P0-1，阶段 63）。
//!
//! **要治的病**：同一个「未保存的工作」，从不同入口关闭得到不同结果 ——
//! 文件编辑器子标签上的 × 会问「未保存的修改」，但关**连接标签**、关**窗口**、
//! 关**数据库子标签**都是直接关：用户改了半天的文件 / 数据说没就没。
//!
//! **断言**：
//! 1. 有未保存修改时，脏状态统计能看见它；
//! 2. 关之前**一定弹确认框**（框的正文写清楚了有几处未保存的工作）；
//! 3. 默认焦点落在「取消」上（手快按回车不会把东西删了）；
//! 4. 用户点「取消」⇒ **不关**，编辑器还在、改动还在；
//! 5. 用户点「不保存关闭」⇒ 放行；
//! 6. 干净时 ⇒ **不弹框**直接放行（别打扰用户）。
//!
//! ```text
//! xvfb-run -a cargo run --example close_confirm_smoke
//! ```

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{closeguard, fileedit, SftpPanel};
use puttyplus::theme::{self, Theme};

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 找到刚弹出来的「确认关闭」对话框。
fn find_close_dialog() -> Option<gtk::Dialog> {
    for w in gtk::Window::list_toplevels() {
        if let Some(d) = w.downcast_ref::<gtk::Dialog>() {
            if d.title().as_deref() == Some("确认关闭") {
                return Some(d.clone());
            }
        }
    }
    None
}

/// 对话框正文里所有 Label 的文字（拼起来）。
fn collect_labels(w: &gtk::Widget, out: &mut String) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push_str(l.text().as_str());
        out.push('\n');
    }
    let mut cur = w.first_child();
    while let Some(c) = cur {
        let next = c.next_sibling();
        collect_labels(&c, out);
        cur = next;
    }
}

fn dialog_text(dlg: &gtk::Dialog) -> String {
    let mut out = String::new();
    collect_labels(dlg.upcast_ref::<gtk::Widget>(), &mut out);
    out
}

/// 安排一个"稍后自动回应弹窗"的定时器。
///
/// ⚠️ 关闭确认框是**模态 + 嵌套主循环**（`run_modal`），所以这个定时器
/// 会在嵌套循环里被派发 ⇒ 能真的点到按钮。
///
/// 返回一个计数器：被回应的次数（用来断言"干净时压根没弹框"）。
fn schedule_answer(resp: gtk::ResponseType) -> Rc<Cell<u32>> {
    let n = Rc::new(Cell::new(0));
    let n2 = Rc::clone(&n);
    gtk::glib::timeout_add_local_once(Duration::from_millis(40), move || {
        if let Some(dlg) = find_close_dialog() {
            n2.set(n2.get() + 1);
            dlg.response(resp);
        }
    });
    n
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let shot = args.iter().any(|a| a == "--shot");

    gtk::init().expect("gtk init");
    theme::apply(Theme::Light);

    // ⚠️ **别碰用户的真实配置**（AGENTS.md 规则 6）
    let tmp = std::env::temp_dir().join(format!("pp-close-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("建临时目录");
    let path = tmp.join("note.txt");
    std::fs::write(&path, "第一行\n").expect("写临时文件");
    let path_s = path.to_string_lossy().to_string();

    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("close confirm smoke"));
    win.set_default_size(900, 600);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(200);

    // ---- 打开本地文件并"打字" ⇒ 真的有未保存的修改 ----
    fileedit::open_local(&panel.st, &path_s, false);
    pump(200);
    assert!(
        fileedit::type_into(&panel.st, &path_s, "第二行\n"),
        "没找到刚打开的编辑器"
    );
    pump(120);
    assert!(
        fileedit::is_dirty(&panel.st, &path_s),
        "打了字还不算有未保存的修改？"
    );

    // ---- 1) 脏状态统计看得见它 ----
    let d = closeguard::collect_dirty(&panel.st);
    eprintln!("脏状态：{d:?}");
    assert_eq!(d.edited_files, 1, "没统计到那个有未保存修改的文件");
    assert!(!d.is_clean(), "有未保存的修改却报干净");
    assert!(d.savable(), "有能保存的东西 ⇒ 该给「保存并关闭」");

    // ---- 截图模式：把确认框弹出来停住，让外层脚本拍 ----
    if shot {
        println!("确认框已弹出，等截图…");
        // ⚠️ 这里会**卡住**（模态 + 嵌套主循环），外层脚本拍完就 kill 掉
        let _ = closeguard::request_close(&panel.st, || {});
        return;
    }

    // ---- 2) 关之前一定弹框，正文写清楚 ----
    let answered = schedule_answer(gtk::ResponseType::Cancel);
    let proceeded = Rc::new(Cell::new(false));
    let p2 = Rc::clone(&proceeded);
    let ok = closeguard::request_close(&panel.st, move || {
        p2.set(true);
    });
    assert_eq!(answered.get(), 1, "确认框没弹出来（断言会空转）");
    assert!(!ok, "用户点了「取消」，不该放行");
    assert!(!proceeded.get(), "点了「取消」还继续关了");
    assert_eq!(
        fileedit::dirty_count(&panel.st),
        1,
        "点了「取消」编辑器却被关了"
    );
    assert!(
        fileedit::is_dirty(&panel.st, &path_s),
        "点了「取消」未保存的修改却没了"
    );

    // ---- 3) 默认焦点落在「取消」上，正文写清楚 ----
    {
        let seen = Rc::new(Cell::new(0u32));
        let s2 = Rc::clone(&seen);
        gtk::glib::timeout_add_local_once(Duration::from_millis(20), move || {
            if let Some(dlg) = find_close_dialog() {
                s2.set(s2.get() + 1);
                let def = dlg.default_widget();
                let label = def
                    .as_ref()
                    .and_then(|w| w.downcast_ref::<gtk::Button>())
                    .and_then(|b| b.label())
                    .unwrap_or_default();
                assert_eq!(
                    label, "取消",
                    "默认焦点不在「取消」上（落在「{label}」）——手快按回车会丢东西"
                );
                let body = dialog_text(&dlg);
                assert!(
                    body.contains("未保存"),
                    "确认框正文没写清楚有未保存的工作：\n{body}"
                );
                // 按钮角色：「不保存关闭」丢东西 ⇒ 危险色；「保存并关闭」⇒ 主色。
                // ⚠️ 这两条一起防"两个蓝按钮"（`style_dialog` 按文案认主操作，
                // 「不保存关闭」里也有"保存"两个字）。
                let role = |resp: gtk::ResponseType| {
                    dlg.widget_for_response(resp)
                        .and_downcast::<gtk::Button>()
                        .map(|b| {
                            (
                                b.has_css_class("suggested-action"),
                                b.has_css_class("destructive-action"),
                                b.label().unwrap_or_default(),
                            )
                        })
                };
                let discard = role(gtk::ResponseType::Other(1)).expect("没有「不保存关闭」按钮");
                assert!(
                    !discard.0 && discard.1,
                    "「不保存关闭」的角色不对（主色={} 危险色={}）——它会丢东西，不该是主色",
                    discard.0,
                    discard.1
                );
                let save = role(gtk::ResponseType::Other(2)).expect("没有「保存并关闭」按钮");
                assert!(
                    save.0 && !save.1,
                    "「保存并关闭」的角色不对（主色={} 危险色={}）",
                    save.0,
                    save.1
                );
                dlg.response(gtk::ResponseType::Cancel);
            }
        });
        let _ = closeguard::request_close(&panel.st, || {});
        assert_eq!(seen.get(), 1, "确认框没弹出来");
    }

    // ---- 4) 「不保存关闭」⇒ 放行 ----
    {
        let answered = schedule_answer(gtk::ResponseType::Other(1));
        let proceeded = Rc::new(Cell::new(false));
        let p2 = Rc::clone(&proceeded);
        let ok = closeguard::request_close(&panel.st, move || {
            p2.set(true);
        });
        assert_eq!(answered.get(), 1);
        assert!(ok, "选了「不保存关闭」却没放行");
        assert!(proceeded.get(), "选了「不保存关闭」却没继续关");
    }

    // ---- 5) 干净时**不弹框**直接放行 ----
    {
        fileedit::close_all(&panel.st);
        assert_eq!(fileedit::dirty_count(&panel.st), 0);
        let answered = schedule_answer(gtk::ResponseType::Cancel);
        let proceeded = Rc::new(Cell::new(false));
        let p2 = Rc::clone(&proceeded);
        let ok = closeguard::request_close(&panel.st, move || {
            p2.set(true);
        });
        assert!(ok, "干净时不该拦着不让关");
        assert!(proceeded.get(), "干净时没继续关");
        assert_eq!(answered.get(), 0, "干净时还弹了确认框（不该打扰用户）");
    }

    let _ = std::fs::remove_dir_all(&tmp);
    println!("close_confirm_smoke: OK");
}
