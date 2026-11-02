//! 传输完成通知（4.3）。
//!
//! 三个诉求，一处实现：
//! - **桌面通知**：批次结束时按结果发一条（成功/部分失败/全失败），
//!   免得用户盯着窗口等
//! - **失败醒目态**：失败项在队列里状态列染红；定位失败的入口是下面的
//!   汇总窗口（列出失败清单），**不做自动置顶重排**——那会让列表在传输
//!   过程中不断跳行，比不排更让人分不清哪行是哪行
//! - **批量汇总视图**：**用户点通知**时才打开"几成几败 + 失败清单"的
//!   小窗
//!
//! ⚠️ **传输完绝不弹窗**（用户 2026-10-27 明确要求）。这里的"绝不"包括
//! **通知发不出去的时候**——桌面没有通知守护进程是最常见的情况，那
//! 时兜底成模态窗等于"每传完一批弹一次"，正是用户要摆脱的。通知不可
//! 用时改为在**状态栏**写一句。
//!
//! 诚实说明能力边界：
//! - 通知走的是 **Gio 桌面通知**（`org.freedesktop.Notifications`）。装了
//!   通知守护进程才有东西弹；纯 Wayland 无 compositor、或桌面环境关了通知，
//!   `send()` 返回 false 或石沉大海——这是平台行为，不是我们能兜住的
//! - 通知正文只报数量与失败文件名，**不报文件内容**，也绝不把密码之类
//!   往通知里塞
//! - 失败项"置顶"用的是排序，不是真的插到最前；模型顺序要真变还得重排
//!   行，收益不值当

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::gio;
use gtk::gio::prelude::DBusProxyExtManual;
use gtk::glib;
use gtk::prelude::*;

use crate::i18n::tr;
use crate::panel::worker::XferState;
use crate::trf;

/// 批次结果统计（纯逻辑，可单测）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Batch {
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub canceled: usize,
    pub failed_names: Vec<String>,
}

impl Batch {
    pub fn total(&self) -> usize {
        self.done + self.failed + self.skipped + self.canceled
    }

    /// 一句话汇总。措辞按结果分级，别一律"传输完成"了事。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.done > 0 {
            parts.push(trf!("成功 {n}", n = self.done));
        }
        // ⚠️ 失败数**单独拎出来**：以前是拼好文案后再 `strip_prefix("失败 ")`
        // 把失败那截摘出来 ⇒ 那是在**拿文案当数据用**，一翻译就认不出来。
        let failed_part = if self.failed > 0 {
            Some(trf!("失败 {n}", n = self.failed))
        } else {
            None
        };
        if self.skipped > 0 {
            parts.push(trf!("跳过 {n}", n = self.skipped));
        }
        if self.canceled > 0 {
            parts.push(trf!("取消 {n}", n = self.canceled));
        }
        if parts.is_empty() && failed_part.is_none() {
            return tr("没有传输项").into();
        }
        // 全部成功给句好话；有失败就**只报失败数**（失败才是要人管的事）
        if failed_part.is_none() && self.canceled == 0 {
            trf!("全部完成：{v0}", v0 = parts.join("，"))
        } else {
            trf!("{n} 项{v0}", n = self.total(), v0 = failed_part.unwrap_or_default())
        }
    }

    /// 通知标题：失败优先，让人一眼分得出要不要管。
    pub fn title(&self) -> &'static str {
        if self.failed > 0 {
            "传输部分失败"
        } else if self.canceled > 0 {
            "传输已取消"
        } else if self.skipped > 0 && self.done == 0 {
            "传输全部跳过"
        } else {
            "传输完成"
        }
    }
}

/// 统计一个批次。`items` 是队列里的全部项（本批结束后没有新增混入，
/// 所以整体统计与批次统计等价——这点在文案上不撒谎：面板一次只跑一个批次）。
pub fn tally(items: &[(String, XferState)]) -> Batch {
    let mut b = Batch::default();
    for (name, st) in items {
        match st {
            XferState::Done => b.done += 1,
            XferState::Err => {
                b.failed += 1;
                if b.failed_names.len() < 20 {
                    b.failed_names.push(name.clone());
                }
            }
            XferState::Skip => b.skipped += 1,
            XferState::Cancel => b.canceled += 1,
            XferState::Wait | XferState::Run => {}
        }
    }
    b
}

/* ---------------- 桌面通知 ---------------- */

/// 通知服务的 D-Bus 位置（org.freedesktop.Notifications 标准接口）。
const NOTIFY_BUS: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

/// `Notify` 的参数签名。写的是 GVariant 的**规范形式**（内层元组要写全
/// `(ss)`），GVariant 解析出来的 `type_()` 正是这个字符串，所以可以直接
/// 拿来比对断言。
const NOTIFY_SIG: &str = "(susssa(ss)a{sv}i)";

/// GVariant 文本格式里的字符串字面量。
///
/// 走**文本格式**而不是手搓 `g_variant_new()`：单引号串里只有 `\` 和 `'`
/// 需要转义，比在 Rust 里拼 GVariant 树短得多，也不必为这个引入新依赖。
fn gv_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            // 通知是一行文本，控制字符（换行/制表）会让不同通知守护进程
            // 的呈现差异很大，直接换成空格
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 拼出 `Notify` 的参数。返回 None 表示拼错了类型（不该发生，签名写死了）。
///
/// `actions` 是通知的动作列表（`a(ss)`）：动作键 + 动作名。键为
/// `"default"` 的那个是**点通知本身**触发的（桌面通知规范如此），
/// 其余是通知里额外的小按钮。
fn notify_params(
    app: &str,
    summary: &str,
    body: &str,
    actions: &[(String, String)],
) -> Option<glib::Variant> {
    // 数字必须写 `@u 0` / `@i 0`：GVariant 文本格式不认 `0u` 后缀，
    // 空容器必须写 `@a(ss) []` / `@a{sv} {}`，否则推不出类型
    let acts = actions
        .iter()
        .map(|(k, l)| format!("({}, {})", gv_str(k), gv_str(l)))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "({}, @u 0, '', {}, {}, @a(ss) [{}], @a{{sv}} {{}}, @i 0)",
        gv_str(app),
        gv_str(summary),
        gv_str(body),
        acts
    );
    // 类型全靠文本里的 `@u` / `@a(ss)` / `@a{sv}` 标注推断（签名也一并断言，
    // 见下），这样不必为了一个签名去实现 StaticVariantType
    let v = glib::Variant::parse(None, &text).ok()?;
    // 必须正好推断成 (susssasa{sv}i：拼错时 GVariant 不报错，只会安静地
    // 发出一条残缺通知，所以显式钉一道
    if v.type_().as_str() == NOTIFY_SIG {
        Some(v)
    } else {
        None
    }
}

/// 通知服务的 D-Bus 代理。拿不到就是"这台机器没有桌面通知"。
fn notify_proxy() -> Option<gio::DBusProxy> {
    gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        gio::DBusProxyFlags::NONE,
        None,
        NOTIFY_BUS,
        NOTIFY_PATH,
        NOTIFY_IFACE,
        None::<&gio::Cancellable>,
    )
    .ok()
}

/// 发一条**可点击**的桌面通知：用户点它才打开汇总窗。
///
/// **为什么直连 D-Bus**：`gio::Notification` 在 gio-rs 里只有 setter，
/// 连 `g_notification_send()` 都没绑定，所以只能按标准自己去调
/// `org.freedesktop.Notifications.Notify` —— GLib 内部做的也是同一件事。
///
/// ⚠️ **`on_rejected` 里绝不能弹窗**（用户 2026-10-27 明确要求"传输完
/// 不要弹窗"）。`Notify` 是异步的，真正的受理结果在回调里才知道；
/// 而"桌面没有通知守护进程"恰恰是最常见的情况——那时装模态窗就是
/// **每传完一批弹一次**，正是用户要摆脱的。回调里只写状态栏。
///
/// 通知守护进程在用户点击时发 `ActionInvoked` 信号（动作键
/// `"default"`），那里再开汇总窗——**用户主动点的**才弹，不是"传完
/// 就弹"。失败明细就在那个窗里。
/* ---------------- 通知中心（阶段 77c-3 / 审计 P1-9） ---------------- */

/// 通知中心里的一条记录。
#[derive(Clone, Debug)]
pub struct Entry {
    /// 本地时间（`HH:MM:SS`）。
    pub at: String,
    /// 标题。
    pub title: String,
    /// 正文。
    pub body: String,
}

thread_local! {
    /// 通知历史（进程级）。
    ///
    /// ⚠️ **桌面通知发不出去时也要记**：没有通知守护进程是最常见的情况，
    /// 那时这条记录是用户**唯一**能回看"刚才发生了什么"的地方。
    static HISTORY: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// 历史上限：多了没人看，内存也白占。
const HISTORY_MAX: usize = 50;

/// 记一条到通知中心。
fn record(title: &str, body: &str) {
    let at = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%H:%M:%S").ok())
        .map(|g| g.to_string())
        .unwrap_or_default();
    HISTORY.with(|h| {
        let mut v = h.borrow_mut();
        v.insert(0, Entry { at, title: title.to_string(), body: body.to_string() });
        if v.len() > HISTORY_MAX {
            v.truncate(HISTORY_MAX);
        }
    });
}

/// 通知中心的历史（新的在前）。
pub fn history() -> Vec<Entry> {
    HISTORY.with(|h| h.borrow().clone())
}

/// 打开通知中心（可回看的通知列表）。
///
/// ⚠️ **工具窗**（单例 + 记尺寸）：同一时间只开一个，第二次点菜单是
/// 「聚焦已有的那个」而不是又开一个。
pub fn open_center(parent: Option<&gtk::Window>) {
    crate::windows::open_or_focus(crate::windows::ToolKind::Notify, || {
        let win = gtk::Window::new();
        win.set_title(Some(crate::windows::ToolKind::Notify.title()));
        // ⚠️ `set_transient_for` 收的是 `Option<&impl IsA<Window>>` ⇒ 这里
        // **直接传 `parent`**：`Option<&gtk::Window>` 正好对上（`impl` =
        // `gtk::Window`）。⚠️ 写成 `parent.as_ref()` 就变成 `Option<&&…>`
        // 了（`Option<&T>::as_ref()` 返回 `Option<&T>` 的引用），编译不过。
        win.set_transient_for(parent);
        let (dw, dh) = crate::settings::window_size("notify").unwrap_or((520, 420));
        win.set_default_size(dw, dh);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(10);
        root.set_margin_end(10);
        win.set_child(Some(&root));

        let head = gtk::Label::new(None);
        head.set_markup("<b>通知中心</b>");
        head.set_halign(gtk::Align::Start);
        root.append(&head);

        let hint = gtk::Label::new(Some(
            "这里留着程序发过的每一条通知（最新的在最上面）。桌面通知发不出去时，             这里就是唯一的记录。",
        ));
        hint.set_halign(gtk::Align::Start);
        hint.set_wrap(true);
        hint.set_max_width_chars(64);
        root.append(&hint);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        // ⚠️ **别在这里 append**：下面要把它塞进 ScrolledWindow ⇒ 同一个
        // widget 挂两个容器 ⇒ GTK 报 "gtk_scrolled_window_set_child: assertion
        // 'child == NULL || priv->child == child …' failed"，而且列表那份
        // 显示不出来。

        let entries = history();
        if entries.is_empty() {
            let empty = gtk::Label::new(Some("还没有通知"));
            empty.set_halign(gtk::Align::Start);
            empty.add_css_class("pp-dim");
            list.append(&empty);
        } else {
            for e in entries {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                row.set_margin_top(4);
                row.set_margin_bottom(6);
                let t = gtk::Label::new(Some(&format!("{}  {}", e.at, e.title)));
                t.set_halign(gtk::Align::Start);
                t.add_css_class("pp-menu-label");
                row.append(&t);
                let b = gtk::Label::new(Some(&e.body));
                b.set_halign(gtk::Align::Start);
                b.set_wrap(true);
                b.set_max_width_chars(64);
                b.add_css_class("pp-dim");
                row.append(&b);
                list.append(&row);
            }
        }

        let sc = gtk::ScrolledWindow::new();
        sc.set_vexpand(true);
        sc.set_child(Some(&list));
        root.append(&sc);

        win.present();
        win
    });
}

pub fn desktop_action(
    title: &str,
    body: &str,
    action_label: &str,
    on_click: impl Fn() + Send + Sync + 'static,
    on_rejected: impl Fn() + 'static,
) -> bool {
    // ⚠️ **先记进通知中心**，再去发桌面通知：没有通知守护进程时桌面
    // 通知根本发不出去（`notify_proxy()` 返回 None），但用户仍然应该
    // 能回看"刚才发生了什么"。
    record(title, body);
    let Some(proxy) = notify_proxy() else {
        return false;
    };
    let actions = vec![("default".to_string(), action_label.to_string())];
    let Some(params) = notify_params("PuTTYPlus", title, body, &actions) else {
        return false;
    };

    // 通知 id 要等 `Notify` 回来才知道，但**信号订阅得先挂上**：
    // D-Bus 消息有顺序，守护进程不可能在 `Notify` 的回包之前就发
    // `ActionInvoked`，所以先订阅再调用不会漏。
    //
    // ⚠️ `connect_g_signal` 的回调要求 `Send + Sync`，所以这里只能用
    // `Arc<Mutex<..>>`，不能图省事用 `Rc<Cell<..>>`（`Cell` 也不是 Sync）。
    let id_slot: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let click = Arc::new(on_click);
    {
        let id_slot = Arc::clone(&id_slot);
        let click = Arc::clone(&click);
        proxy.connect_g_signal(move |_proxy, _sender, name, params| {
            if name != "ActionInvoked" {
                return;
            }
            let Ok(id) = id_slot.lock().map(|g| *g) else {
                return;
            };
            let Some(id) = id else { return };
            let got = params.child_value(0).get::<u32>().unwrap_or(0);
            if got != id {
                return; // 不是我们这条通知
            }
            let key = params.child_value(1).get::<String>().unwrap_or_default();
            if key.is_empty() || key == "default" {
                (click)();
            }
        });
    }

    let id_slot2 = Arc::clone(&id_slot);
    proxy.call(
        "Notify",
        Some(&params),
        gio::DBusCallFlags::NONE,
        2000,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(v) => {
                // Notify 的返回值是 (u32)：通知 id
                let id = v.child_value(0).get::<u32>().unwrap_or(0);
                if let Ok(mut g) = id_slot2.lock() {
                    *g = Some(id);
                }
            }
            Err(e) => {
                eprintln!("[通知] 通知服务未受理（{e}），改走状态栏");
                on_rejected();
            }
        },
    );
    true
}

/// 发一条批次通知。**返回 `true` = 通知已受理**。
fn notify_batch(
    b: &Batch,
    on_rejected: impl Fn() + 'static,
    on_click: impl Fn() + Send + Sync + 'static,
) -> bool {
    // 点通知 → 打开汇总窗（用户**主动点**的，不算"传输完弹窗"）
    desktop_action(
        b.title(),
        &notify_body(b),
        "查看明细",
        on_click,
        on_rejected,
    )
}

/* ---------------- 汇总视图 ---------------- */

thread_local! {
    /// 队列窗口（汇总视图的父窗口）
    static QUEUE_WIN: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

pub fn set_queue_win(w: gtk::Window) {
    QUEUE_WIN.with(|q| *q.borrow_mut() = Some(w));
}

/// 批次结束：**只发通知，不弹窗**。
///
/// ⚠️ **通知发不出去时也绝不弹窗**（用户 2026-10-27："文件传输完不要
/// 弹窗，可以发个通知"）。桌面没有通知守护进程是最常见的情况，那时
/// 装一个模态汇总窗就是**每传完一批弹一次**，正是用户要摆脱的。
/// 所以兜底走**状态栏**（`status` 回调），不打扰，但也不让人完全
/// 不知道传完了。
///
/// 失败明细**没有丢**：通知正文里带着失败条数与文件名；**点通知**能
/// 打开汇总窗（`show_summary`）看完整清单——用户主动点的，不算
/// "传输完弹窗"。
pub fn report_batch(items: &[(String, XferState)], status: impl Fn(&str) + 'static) {
    if items.is_empty() {
        return;
    }
    let b = tally(items);
    let text = status_text(&b);
    // 兜底回调要用两遍（同步就失败 / 异步被拒），所以用 Rc 共享
    let st: Rc<dyn Fn(&str)> = Rc::new(status);
    let st_async = Rc::clone(&st);
    let st_sync = Rc::clone(&st);
    let b2 = b.clone();
    let text_async = text.clone();
    let ok = notify_batch(
        &b,
        move || (st_async)(&text_async),
        move || show_summary(&b2),
    );
    if !ok {
        // 连通知服务都连不上（没有会话总线）：立刻写状态栏
        (st_sync)(&text);
    }
}

/// 状态栏兜底文案（通知发不出去时写的那一句）。
fn status_text(b: &Batch) -> String {
    let mut parts: Vec<String> = Vec::new();
    if b.done > 0 {
        parts.push(format!("成功 {}", b.done));
    }
    if b.failed > 0 {
        parts.push(format!("失败 {}", b.failed));
    }
    if b.skipped > 0 {
        parts.push(format!("跳过 {}", b.skipped));
    }
    if b.canceled > 0 {
        parts.push(format!("取消 {}", b.canceled));
    }
    if parts.is_empty() {
        return b.title().to_string();
    }
    format!("{} · {}", b.title(), parts.join(" · "))
}

/// 汇总窗口：几成几败 + 失败清单 + 跳到队列。
pub fn show_summary(b: &Batch) {
    let parent = QUEUE_WIN.with(|q| q.borrow().clone());
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        "传输汇总",
        parent.as_ref(),
        &[("关闭", gtk::ResponseType::Close)],
    );
    dlg.set_default_size(420, -1);
    dlg.connect_response(|dlg, _| dlg.close());

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let head = gtk::Label::new(Some(&b.title()));
    head.set_markup(&format!(
        "<span size='large' weight='bold'>{}</span>",
        b.title()
    ));
    head.set_xalign(0.0);
    root.append(&head);

    // 一行一个数字，比一串挤在一起好读
    let grid = gtk::Grid::new();
    grid.set_column_spacing(14);
    grid.set_row_spacing(4);
    let rows: [(&str, usize); 4] = [
        ("成功", b.done),
        ("失败", b.failed),
        ("跳过", b.skipped),
        ("取消", b.canceled),
    ];
    for (i, (name, n)) in rows.iter().enumerate() {
        if *n == 0 {
            continue;
        }
        let l = gtk::Label::new(Some(name));
        l.set_xalign(1.0);
        let v = gtk::Label::new(Some(&n.to_string()));
        v.set_xalign(0.0);
        v.set_markup(&format!("<b>{n}</b>"));
        grid.attach(l.upcast_ref::<gtk::Widget>(), 0, i as i32, 1, 1);
        grid.attach(v.upcast_ref::<gtk::Widget>(), 1, i as i32, 1, 1);
    }
    root.append(&grid);

    if !b.failed_names.is_empty() {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        for n in &b.failed_names {
            let row = gtk::ListBoxRow::new();
            let l = gtk::Label::new(Some(n));
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            row.set_child(Some(l.upcast_ref::<gtk::Widget>()));
            list.append(&row);
        }
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        sw.set_child(Some(&list));
        sw.set_min_content_height(140);
        root.append(&sw);
    }

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
}

/// 让 `crate::panel::queue` 在建窗时登记窗口（避免两个模块互相 import）。
pub(crate) fn queue_window(w: &gtk::Window) {
    set_queue_win(w.clone());
}

/// 供测试：拼出通知正文（不依赖通知守护进程）。
pub fn notify_body(b: &Batch) -> String {
    if b.failed_names.is_empty() {
        b.summary()
    } else {
        let mut s = format!("{}；失败：", b.summary());
        s.push_str(&b.failed_names.join("、"));
        if b.failed > b.failed_names.len() {
            s.push_str(&format!("…等 {} 项", b.failed));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 泵 GTK 主循环 ms 毫秒。
    ///
    /// **必须泵**：`Notify` 是异步的，通知守护进程拒收的结果是**稍后**
    /// 才回到主线程的回调里——不泵就测不到"拒收之后会不会弹窗"。
    fn pump(ms: u64) {
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            while ctx.pending() {
                ctx.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// **传输完成不许弹窗**（用户 2026-10-27 明确要求）。
    ///
    /// 怎么验"没弹窗"：数顶层窗口。`show_summary` 是**同步**建 Dialog 的
    /// ——要是哪里还在弹，窗口数会多一个。
    ///
    /// ⚠️ **要泵主循环再数**：通知守护进程拒收是**异步**回调，拒收之后
    /// 才是最容易弹窗的地方（以前正是在那儿弹的）。只数一次会漏。
    #[test]
    fn 传输完成只发通知不弹窗() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() {
            eprintln!("跳过传输完成只发通知不弹窗：本机没有显示服务");
            return;
        }
        let before = gtk::Window::list_toplevels().len();
        let items = vec![
            ("a.txt".to_string(), XferState::Done),
            ("b.txt".to_string(), XferState::Done),
        ];
        let msgs = Rc::new(RefCell::new(Vec::<String>::new()));
        let m2 = Rc::clone(&msgs);
        report_batch(&items, move |s| m2.borrow_mut().push(s.to_string()));
        // 让异步的"通知被拒收"回到主线程
        pump(3000);
        let after = gtk::Window::list_toplevels().len();
        assert_eq!(before, after, "传输完成不该弹窗（只发通知）");
        // 通知发不出去时兜底文案要说清结果
        for s in msgs.borrow().iter() {
            assert!(s.contains("传输"), "兜底文案要说清结果：{s}");
        }
    }

    /// 失败的批次也**不弹窗**（失败明细在通知里，点通知才展开汇总）。
    #[test]
    fn 传输失败也不弹窗() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() {
            return;
        }
        let before = gtk::Window::list_toplevels().len();
        let items = vec![
            ("a.txt".to_string(), XferState::Done),
            ("b.txt".to_string(), XferState::Err),
        ];
        let msgs = Rc::new(RefCell::new(Vec::<String>::new()));
        let m2 = Rc::clone(&msgs);
        report_batch(&items, move |s| m2.borrow_mut().push(s.to_string()));
        pump(3000);
        let after = gtk::Window::list_toplevels().len();
        assert_eq!(before, after, "传输失败也不该弹窗");
        for s in msgs.borrow().iter() {
            assert!(s.contains("失败"), "兜底文案要带失败数：{s}");
        }
    }

    /// 状态栏兜底文案的措辞。
    #[test]
    fn 状态栏兜底文案() {
        let ok = Batch {
            done: 3,
            ..Default::default()
        };
        assert_eq!(status_text(&ok), "传输完成 · 成功 3");
        let bad = Batch {
            done: 1,
            failed: 2,
            ..Default::default()
        };
        assert_eq!(status_text(&bad), "传输部分失败 · 成功 1 · 失败 2");
    }

    #[test]
    fn 全部成功的措辞() {
        let b = Batch {
            done: 3,
            ..Default::default()
        };
        assert_eq!(b.title(), "传输完成");
        assert_eq!(b.summary(), "全部完成：成功 3");
    }

    #[test]
    fn 有失败时标题与措辞都把失败顶上来() {
        let b = tally(&[
            ("a".into(), XferState::Done),
            ("b".into(), XferState::Done),
            ("c".into(), XferState::Err),
        ]);
        assert_eq!(b.title(), "传输部分失败");
        assert!(b.summary().contains("失败 1"), "{}", b.summary());
        assert_eq!(b.failed_names, vec!["c".to_string()]);
    }

    #[test]
    fn 取消与跳过各算各的() {
        let b = tally(&[
            ("a".into(), XferState::Done),
            ("b".into(), XferState::Skip),
            ("c".into(), XferState::Cancel),
        ]);
        assert_eq!(b.total(), 3);
        assert_eq!((b.done, b.skipped, b.canceled), (1, 1, 1));
        assert_eq!(b.title(), "传输已取消");
        let all_skip = tally(&[("a".into(), XferState::Skip)]);
        assert_eq!(all_skip.title(), "传输全部跳过");
    }

    #[test]
    fn 运行中与等待中不计入() {
        // 批次结束事件到达时不该再有 Run/Wait；真有的话也不该算进汇总
        let b = tally(&[
            ("a".into(), XferState::Done),
            ("b".into(), XferState::Run),
            ("c".into(), XferState::Wait),
        ]);
        assert_eq!(b.total(), 1);
        assert_eq!(b.done, 1);
    }

    #[test]
    fn 失败清单只留前二十项() {
        let items: Vec<(String, XferState)> = (0..35)
            .map(|i| (format!("f{i}"), XferState::Err))
            .collect();
        let b = tally(&items);
        assert_eq!(b.failed, 35, "计数不能被截断");
        assert_eq!(b.failed_names.len(), 20, "清单限长，否则弹窗会撑爆");
        assert!(notify_body(&b).contains("…等 35 项"), "{}", notify_body(&b));
    }

    #[test]
    fn 空批次不硬报() {
        assert_eq!(tally(&[]).summary(), "没有传输项");
    }

    #[test]
    fn notify_参数类型必须是标准签名() {
        // 这一条钉住了"通知到底能不能发出去"：类型不对，D-Bus 会安静地
        // 拒收，界面上什么现象都看不到
        let v = notify_params("PuTTYPlus", "传输完成", "成功 3", &[]).expect("参数应能构造");
        assert_eq!(v.type_().as_str(), NOTIFY_SIG);
        assert_eq!(v.n_children(), 8, "Notify 是 8 个入参（含两个容器）");
    }

    #[test]
    fn notify_参数内容对得上() {
        let v = notify_params("PuTTYPlus", "传输完成", "成功 3，失败 1", &[]).unwrap();
        assert_eq!(v.child_value(0).str(), Some("PuTTYPlus"));
        // replaces_id=0 表示"不替换任何已有通知"
        assert_eq!(v.child_value(1).get::<u32>(), Some(0));
        assert_eq!(v.child_value(3).str(), Some("传输完成"));
        assert_eq!(v.child_value(4).str(), Some("成功 3，失败 1"));
        assert_eq!(v.child_value(5).n_children(), 0, "actions 应为空");
        assert_eq!(v.child_value(6).type_().as_str(), "a{sv}", "hints 是字典");
        assert_eq!(v.child_value(7).get::<i32>(), Some(0), "expire_timeout=0 = 永不自动消失");
    }

    #[test]
    fn notify_动作数组要带得上() {
        // "default" 动作 = **点通知本身**触发（桌面通知规范如此）：
        // 点它才开汇总窗，所以这个动作必须真的发出去
        let acts = vec![("default".to_string(), "查看明细".to_string())];
        let v = notify_params("PuTTYPlus", "传输部分失败", "失败 1", &acts).unwrap();
        assert_eq!(v.type_().as_str(), NOTIFY_SIG);
        let a = v.child_value(5);
        assert_eq!(a.n_children(), 1, "应有一个动作");
        let one = a.child_value(0);
        assert_eq!(one.child_value(0).str(), Some("default"));
        assert_eq!(one.child_value(1).str(), Some("查看明细"));
    }

    #[test]
    fn notify_参数里的引号与反斜杠不越界() {
        // 用户起的文件名可以带引号；拼错了会破坏整个 GVariant 结构
        let v = notify_params("PuTTYPlus", "传输部分失败", r"o'brien\dir\file.txt", &[]).unwrap();
        assert_eq!(v.type_().as_str(), NOTIFY_SIG);
        assert_eq!(v.child_value(4).str(), Some(r"o'brien\dir\file.txt"));
    }

    #[test]
    fn notify_参数里的换行压成空格() {
        // 不同通知守护进程对换行的呈现差异很大，压平更稳
        let v = notify_params("PuTTYPlus", "t", "a\nb", &[]).unwrap();
        assert_eq!(v.child_value(4).str(), Some("a b"));
    }
}
