//! GTK4 下的对话框辅助（替代 C 版 gtk_dialog_run 的同步等待）。

use gtk::prelude::*;

/// 危险操作词表：按钮文案里有这些词 ⇒ 标成 destructive（红色）。
///
/// 阶段 79 补「**覆盖**」：传输冲突的「覆盖全部」是**破坏性**动作
/// （把目标机上已有的文件换掉），以前它跟「跳过已存在」长得一模一样，
/// 用户看不出哪个更危险。
pub fn destructive_words() -> &'static [&'static str] {
    &["删除", "移除", "清空", "停止", "重置", "覆盖"]
}

/// 主操作词表：按钮文案里有这些词 ⇒ 标成 suggested-action（强调色）。
pub fn primary_words() -> &'static [&'static str] {
    &[
        "确定", "确认", "连接", "开始", "创建", "生成", "导入", "应用", "打开", "选择", "登录", "解锁",
        "转换", "启用", "导出", "测试", "继续", "重试", "上传", "同步",
    ]
}

/* ---------------- 按钮词表（阶段 82 / 审计 P2-5） ----------------
 *
 * 同一个动作，界面上**只能有一个说法**。以前同一个界面里混着
 * 「好」「确定」「OK」「Cancel」「Close」——看着像三个不同的程序。
 *
 * ⚠ 不用 `gtk::ButtonsType::OkCancel` 之类的**系统按钮**：
 * 它们按系统语言翻译 ⇒ 没装中文翻译的机器上就冒出 "OK / Cancel"。
 */

/// 确认。
pub const BTN_OK: &str = "确定";
/// 取消。
pub const BTN_CANCEL: &str = "取消";
/// 关闭。
pub const BTN_CLOSE: &str = "关闭";
/// 保存。
pub const BTN_SAVE: &str = "保存";

/// 标准按钮词表（lint 用：别的对话框也得用这几个词）。
pub fn standard_button_labels() -> &'static [&'static str] {
    &[BTN_OK, BTN_CANCEL, BTN_CLOSE, BTN_SAVE]
}

/// 给**默认按钮**（回车按的那个）画一道看得见的框。
///
/// GTK4 的 `set_default_response` 只决定"回车按哪个"，**不挪键盘焦点**——
/// 用户看不见默认落在哪。危险操作的默认往往是「取消」：它既不是强调色
/// 也不是危险色，跟旁边的按钮长得一模一样 ⇒ 手快按回车就出事。
pub fn mark_default(dlg: &gtk::Dialog, label: &str) {
    if let Some(b) = find_button(dlg.upcast_ref(), label) {
        b.add_css_class("pp-default");
    }
}

/// 对话框里的按钮文字（冒烟用：检查词表统一）。
pub fn button_labels(dlg: &gtk::Dialog) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dlg.upcast_ref::<gtk::Widget>().clone()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if let Some(child) = b.child() {
                if let Some(l) = child.downcast_ref::<gtk::Label>() {
                    out.push(l.text().to_string());
                }
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            stack.push(ch.clone());
            c = next;
        }
    }
    out
}

/// 默认按钮（回车按的那个）的文字（冒烟用）。
pub fn default_button_label(dlg: &gtk::Dialog) -> Option<String> {
    let mut stack = vec![dlg.upcast_ref::<gtk::Widget>().clone()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if b.has_css_class("pp-default") {
                if let Some(child) = b.child() {
                    if let Some(l) = child.downcast_ref::<gtk::Label>() {
                        return Some(l.text().to_string());
                    }
                }
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            stack.push(ch.clone());
            c = next;
        }
    }
    None
}

/// 在对话框里按文字找到那个按钮（GTK4 的 action area 里按钮的文字是个 Label）。
fn find_button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        if let Some(b) = w.downcast_ref::<gtk::Button>() {
            if let Some(child) = b.child() {
                if let Some(l) = child.downcast_ref::<gtk::Label>() {
                    if l.text() == label {
                        return Some(b.clone());
                    }
                }
            }
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            stack.push(ch.clone());
            c = next;
        }
    }
    None
}

/// 给对话框底部动作区统一"主/次/危险"层次与间距。
///
/// GTK4 的 Dialog 只是把按钮塞进 `.dialog-action-area`，不会自动加
/// `suggested-action`/`destructive-action`；这里按按钮文案归类：
/// - 删除/移除/清空/停止/重置/覆盖 → 红色（destructive-action）
/// - 保存并连接 / 保存 / 确定 / 连接 / 开始 / 创建 / 生成 / 导入 / 应用 …
///   → 强调色（suggested-action）；同时存在"保存"和"保存并连接"时只强调后者
/// - 取消/关闭 → 中性描边（CSS 默认）
pub fn style_dialog(dlg: &gtk::Dialog) {
    fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
        if w.has_css_class("dialog-action-area") {
            out.push(w.clone());
            return;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            let next = ch.next_sibling();
            walk(&ch, out);
            c = next;
        }
    }
    let mut areas = Vec::new();
    walk(dlg.upcast_ref::<gtk::Widget>(), &mut areas);
    let Some(area) = areas.into_iter().next() else {
        return;
    };
    if let Some(b) = area.downcast_ref::<gtk::Box>() {
        b.set_spacing(8);
        b.set_homogeneous(false);
        b.set_halign(gtk::Align::Fill);
        b.set_hexpand(true);
        // 前面垫一个可扩展空盒，把按钮推到右边（GtkDialog 默认从左边排）
        let has_spacer = b
            .first_child()
            .map(|c| c.has_css_class("pp-action-spacer"))
            .unwrap_or(false);
        if !has_spacer {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.add_css_class("pp-action-spacer");
            spacer.set_hexpand(true);
            b.prepend(&spacer);
        }
    }
    let mut labels = Vec::new();
    let mut btns = Vec::new();
    let mut c = area.first_child();
    while let Some(ch) = c {
        let next = ch.next_sibling();
        if let Some(b) = ch.downcast_ref::<gtk::Button>() {
            labels.push(b.label().unwrap_or_default());
            btns.push(b.clone());
        }
        c = next;
    }
    let has_save_connect = labels.iter().any(|l| l.contains("保存并连接"));
    let has_save = labels
        .iter()
        .any(|l| l.contains("保存") && !l.contains("并连接"));
    let destructive = destructive_words();
    let primary = primary_words();
    for (b, label) in btns.iter().zip(labels.iter()) {
        if destructive.iter().any(|k| label.contains(k)) {
            b.add_css_class("destructive-action");
            continue;
        }
        // 一个对话框只留一个主操作：有"保存并连接"时，保存/仅连接都退成次要
        let is_primary = if label.contains("保存并连接") {
            true
        } else if label.contains("保存") {
            !has_save_connect
        } else if label.contains("仅连接") {
            !has_save && !has_save_connect
        } else {
            primary.iter().any(|k| label.contains(k))
        };
        if is_primary {
            b.add_css_class("suggested-action");
        }
    }
    // 没有任何按钮被认出主操作时，把默认控件当主按钮（取消/关闭不算）
    if !btns.iter().any(|b| b.has_css_class("suggested-action")) {
        if let Some(dw) = dlg.default_widget() {
            if let Some(b) = dw.downcast_ref::<gtk::Button>() {
                let t = b.label().unwrap_or_default();
                if !b.has_css_class("destructive-action")
                    && !t.contains("取消")
                    && !t.contains("关闭")
                {
                    b.add_css_class("suggested-action");
                }
            }
        }
    }
}

/// 在主线程里**同步**等一个模态对话框的结果。
///
/// ⚠️ GTK4 把 `gtk_dialog_run()` 移除了（gtk4-rs 也没绑它），这里自己跑一个
/// **嵌套主循环**：把事件派发出去，直到对话框拿到响应。
///
/// ⚠️ 嵌套循环期间，别的定时器与 worker 回包**也会**被派发 ⇒ 只在
/// 「关窗确认」这种一次性、幂等的场景用，别拿它做长流程。
///
/// `may_block = true`：没事件时**阻塞**等，不空转烧 CPU。
pub fn run_modal(dlg: &gtk::Dialog) -> gtk::ResponseType {
    use std::cell::Cell;
    use std::rc::Rc;

    let resp = Rc::new(Cell::new(None::<gtk::ResponseType>));
    let r2 = Rc::clone(&resp);
    dlg.connect_response(move |_, r| r2.set(Some(r)));
    dlg.present();
    let ctx = glib::MainContext::default();
    while resp.get().is_none() {
        ctx.iteration(true);
    }
    resp.get().unwrap()
}

/// 单行文本输入；回调收 None 表示取消。对应 C 版 ask_text。
pub fn ask_text<F: FnOnce(Option<String>) + 'static>(
    parent: Option<&gtk::Window>,
    title: &str,
    defval: &str,
    done: F,
) {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        title,
        parent,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("确定", gtk::ResponseType::Ok),
        ],
    );
    let entry = gtk::Entry::new();
    entry.set_text(defval);
    entry.set_hexpand(true);
    // 回车即确定（对应 C 版 activates_default）
    {
        let dlg2 = dlg.clone();
        entry.connect_activate(move |_| dlg2.response(gtk::ResponseType::Ok));
    }
    dlg.content_area().append(&entry);
    style_dialog(&dlg);
    dlg.present();
    let entry2 = entry.clone();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        let text = entry2.text().to_string();
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(if resp == gtk::ResponseType::Ok && !text.is_empty() {
                Some(text)
            } else {
                None
            });
        }
    });
}

/// 密码输入（连接失败后询问）；回调收 None 表示取消。
pub fn ask_password<F: FnOnce(Option<String>) + 'static>(
    parent: Option<&gtk::Window>,
    title: &str,
    message: &str,
    done: F,
) {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        title,
        parent,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("确定", gtk::ResponseType::Ok),
        ],
    );
    let lbl = gtk::Label::new(Some(message));
    lbl.set_wrap(true);
    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_hexpand(true);
    {
        let dlg2 = dlg.clone();
        entry.connect_activate(move |_| dlg2.response(gtk::ResponseType::Ok));
    }
    let ca = dlg.content_area();
    ca.set_margin_top(8);
    ca.set_margin_bottom(8);
    ca.set_margin_start(8);
    ca.set_margin_end(8);
    ca.append(&lbl);
    ca.append(&entry);
    style_dialog(&dlg);
    dlg.present();
    let entry2 = entry.clone();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        let text = entry2.text().to_string();
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(if resp == gtk::ResponseType::Ok && !text.is_empty() {
                Some(text)
            } else {
                None
            });
        }
    });
}

/// 密码输入 +「记住密码」勾选（数据库连接用）。
///
/// 回调收 `(Option<String>, bool)`：密码（取消 / 没填 = `None`）+ 要不要记住。
///
/// **为什么不复用 `ask_password`**：那个是"问一次密码"（SSH/SFTP 用），不带
/// "记住"。数据库连接需要"记住"——不然每次连都要重输一遍（Navicat 就是
/// 连接时问密码 + 可选保存）。
///
/// ⚠️ **密码不进 argv、不进日志**：回调拿到的密码由调用方负责
/// `Zeroizing` 包裹。
pub fn ask_password_remember<F: FnOnce(Option<String>, bool) + 'static>(
    parent: Option<&gtk::Window>,
    title: &str,
    message: &str,
    remember_default: bool,
    done: F,
) {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        title,
        parent,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("确定", gtk::ResponseType::Ok),
        ],
    );
    let lbl = gtk::Label::new(Some(message));
    lbl.set_wrap(true);
    lbl.set_xalign(0.0);
    let entry = gtk::Entry::new();
    entry.set_visibility(false);
    entry.set_hexpand(true);
    {
        let dlg2 = dlg.clone();
        entry.connect_activate(move |_| dlg2.response(gtk::ResponseType::Ok));
    }
    // ⚠️ 阶段 79：「记住密码」**三处共用**同一个构件（连接表单 / DB 密码提示 /
    // 凭据库管理器）——凭据库没启用或锁着时**不可勾**，并说清为什么；
    // 锁着时还给一个「解锁凭据库…」入口。
    let (remember, status_box) = crate::vaultui::remember_row(parent, remember_default);
    let ca = dlg.content_area();
    ca.set_margin_top(8);
    ca.set_margin_bottom(8);
    ca.set_margin_start(8);
    ca.set_margin_end(8);
    ca.append(&lbl);
    ca.append(&entry);
    ca.append(&remember);
    ca.append(&status_box);
    style_dialog(&dlg);
    dlg.present();
    let entry2 = entry.clone();
    let remember2 = remember.clone();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        let text = entry2.text().to_string();
        let keep = remember2.is_active();
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(
                if resp == gtk::ResponseType::Ok && !text.is_empty() {
                    Some(text)
                } else {
                    None
                },
                keep,
            );
        }
    });
}

/// 确认对话框；回调收 true 表示确认。对应 C 版 show_error 前的 MessageDialog。
///
/// 按钮用自建的中文标签而不是 `ButtonsType::OkCancel`：后者在没装 GTK 中文
/// 翻译的环境里会显示成 "OK / Cancel"，与全中文界面割裂。
pub fn confirm<F: FnOnce(bool) + 'static>(
    parent: Option<&gtk::Window>,
    message: &str,
    done: F,
) {
    confirm_with(parent, message, "确定", done);
}

/// 带自定义确认按钮文案的确认框（危险操作用「删除」这种动词）。
pub fn confirm_with<F: FnOnce(bool) + 'static>(
    parent: Option<&gtk::Window>,
    message: &str,
    ok_label: &str,
    done: F,
) {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        "请确认",
        parent,
        &[
            ("取消", gtk::ResponseType::Cancel),
            (ok_label, gtk::ResponseType::Ok),
        ],
    );
    let lbl = gtk::Label::new(Some(message));
    lbl.set_wrap(true);
    lbl.set_max_width_chars(52);
    lbl.set_xalign(0.0);
    let ca = dlg.content_area();
    ca.set_margin_top(12);
    ca.set_margin_bottom(12);
    ca.set_margin_start(12);
    ca.set_margin_end(12);
    ca.append(&lbl);
    // 危险操作的回车默认落在「取消」上，免得手快按回车就删了
    dlg.set_default_response(gtk::ResponseType::Cancel);
    // ⚠ 光设默认响应**看不出**默认落在哪（GTK 不挪焦点）⇒ 给它画一道框
    mark_default(&dlg, BTN_CANCEL);
    style_dialog(&dlg);
    dlg.present();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(resp == gtk::ResponseType::Ok);
        }
    });
}

/* ---------------- 表单骨架 ----------------
 *
 * 统一的表单版式：**标签在上、控件在下**（而不是 GTK 默认的"标签右对齐贴
 * 在左边"）。中文标签长短差很多，右侧对齐会把输入框推成锯齿；顶对齐在窄窗
 * 里也稳，标签换成两行也不会挤控件。
 */

/// 表单根容器：16px 内边距 + 12px 行距。
pub fn form_box() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.set_margin_top(16);
    b.set_margin_bottom(16);
    b.set_margin_start(16);
    b.set_margin_end(16);
    b
}

/// 分区标题（小号、弱化）。
pub fn form_section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("pp-section");
    l
}

/// 一行表单：小号灰标签 + 控件，返回（行容器, 标签）。
pub fn form_row_with(label: &str, w: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    l.add_css_class("pp-hint");
    row.append(&l);
    w.set_hexpand(true);
    row.append(w);
    (row, l)
}

/// 一行表单（不需要改标签文字时用这个）。
pub fn form_row(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    form_row_with(label, w).0
}

/// 并排两行（短字段配对，省一半高度）。两个控件都 `hexpand`，各占一半。
/// 返回（并排容器, 左行, 右行）：需要单独隐藏某一侧时用后两个句柄。
pub fn form_pair(
    la: &str,
    wa: &impl IsA<gtk::Widget>,
    lb: &str,
    wb: &impl IsA<gtk::Widget>,
) -> (gtk::Box, gtk::Box, gtk::Box) {
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let ra = form_row(la, wa);
    ra.set_hexpand(true);
    let rb = form_row(lb, wb);
    rb.set_hexpand(true);
    h.append(&ra);
    h.append(&rb);
    (h, ra, rb)
}

/// 结果提示（成功也算）：标题进窗口标题栏，正文可选中复制。
///
/// 以前成功消息复用 `show_error`，于是"已生成""转换完成"看起来与报错同款。
/// 这不是错误提示，不该用错误对话框的形态。
pub fn info(parent: Option<&gtk::Window>, title: &str, body: &str) {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(title, parent, &[(BTN_OK, gtk::ResponseType::Ok)]);
    dlg.set_default_size(360, -1);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
    vbox.set_margin_top(14);
    vbox.set_margin_bottom(14);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    let body = gtk::Label::new(Some(body));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true); // 路径 / 指纹 / 错误原因都该能复制
    body.set_max_width_chars(56);
    vbox.append(&body);

    dlg.content_area().append(&vbox);
    // 只有「确定」一个按钮：回车即关闭
    dlg.set_default_response(gtk::ResponseType::Ok);
    mark_default(&dlg, BTN_OK);
    dlg.connect_response(|d, _| d.destroy());
    style_dialog(&dlg);
    dlg.present();
}

/// 错误提示（异步，无返回值）。
///
/// ⚠ 以前用 `gtk::AlertDialog`：它的按钮文字**跟着系统语言走**——
/// 没装中文翻译的机器上就冒出 "Close"，跟全中文界面割裂。
/// 现在走统一构件（中文按钮 + 有父窗口 + 默认框看得见）。
pub fn show_error(parent: Option<&gtk::Window>, title: &str, msg: &str) {
    let dlg = crate::windows::dialog(title, parent, &[(BTN_OK, gtk::ResponseType::Ok)]);
    dlg.set_default_size(360, -1);
    let lbl = gtk::Label::new(Some(msg));
    lbl.set_xalign(0.0);
    lbl.set_wrap(true);
    lbl.set_selectable(true); // 错误原因要能复制走
    lbl.set_max_width_chars(56);
    let ca = dlg.content_area();
    ca.set_margin_top(14);
    ca.set_margin_bottom(14);
    ca.set_margin_start(16);
    ca.set_margin_end(16);
    ca.append(&lbl);
    dlg.set_default_response(gtk::ResponseType::Ok);
    mark_default(&dlg, BTN_OK);
    dlg.connect_response(|d, _| d.destroy());
    style_dialog(&dlg);
    dlg.present();
}

/// 「重试」按钮的响应码。
///
/// ⚠️ **GTK 没有 `GTK_RESPONSE_RETRY`**（gtk4-rs 的 `ResponseType` 里也没这个
/// 变体）⇒ 用自定义响应码（GTK 规定自定义码从 100 起）。
const RESP_RETRY: gtk::ResponseType = gtk::ResponseType::Other(100);

/// 错误对话框 + **一颗当场重试的按钮**（复查 R1 / §4.2）。
///
/// **验收标准：用户不需要离开错误对话框，就知道下一步点哪。**
///
/// 以前 SFTP 连接失败只有一颗「关闭」⇒ 用户关掉弹窗只能干瞪眼，而
/// 「连接 → 重新连接 SFTP」就在两步之外，弹窗却不说（§4.2「错误恢复不
/// 闭环」）。⇒ 这里直接给一颗「重试」按钮：点一下就重来，不用去菜单里翻。
///
/// ⚠️ **别用文字叙述点击路径**（§4.1）：按钮就在眼前，一句"点重试再试一次"
/// 是多余的。
pub fn show_error_with_retry(
    parent: Option<&gtk::Window>,
    title: &str,
    msg: &str,
    retry_label: &str,
    on_retry: impl Fn() + 'static,
) {
    let dlg = crate::windows::dialog(
        title,
        parent,
        &[
            (retry_label, RESP_RETRY),
            (BTN_CLOSE, gtk::ResponseType::Close),
        ],
    );
    dlg.set_default_size(360, -1);
    let lbl = gtk::Label::new(Some(msg));
    lbl.set_xalign(0.0);
    lbl.set_wrap(true);
    lbl.set_selectable(true); // 错误原因要能复制走
    lbl.set_max_width_chars(56);
    let ca = dlg.content_area();
    ca.set_margin_top(14);
    ca.set_margin_bottom(14);
    ca.set_margin_start(16);
    ca.set_margin_end(16);
    ca.append(&lbl);
    // 默认响应 = 重试（用户最想干的就是再来一次）
    dlg.set_default_response(RESP_RETRY);
    mark_default(&dlg, retry_label);
    // 「重试」是这一颗框里唯一该突出的动作 ⇒ 明确标成主操作
    // （`style_dialog` 的兜底靠 default_widget，而 `mark_default` 只加
    //  描边类、不设 default_widget ⇒ 这里自己标，别让它变成一颗灰按钮）。
    if let Some(b) = find_button(dlg.upcast_ref(), retry_label) {
        b.add_css_class("suggested-action");
    }
    dlg.connect_response(move |d, resp| {
        d.destroy();
        if resp == RESP_RETRY {
            on_retry();
        }
    });
    style_dialog(&dlg);
    dlg.present();
}

/// 传输目标冲突的批量询问。回调参数：
/// None=取消整批，Some(true)=覆盖全部，Some(false)=跳过已存在的。
/// 传输目标冲突时用户的选择（阶段 79）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictChoice {
    /// 全部覆盖。
    All,
    /// 跳过已存在的（不动目标机上已有的文件）。
    Skip,
    /// 逐个确认的结果：与冲突列表**一一对应**（true = 覆盖，false = 跳过）。
    PerItem(Vec<bool>),
}

/// 传输目标冲突的询问。
///
/// 阶段 79 的三处调整：
/// - **默认响应 = 「跳过已存在」**：回车不该顺手覆盖掉目标机上的文件，
///   危险的那一条得用户**明确点**；
/// - 「覆盖全部」标成 **destructive**（红色）——它是破坏性动作，
///   以前跟「跳过已存在」长得一模一样，用户看不出哪个更危险；
/// - 多一个「**逐个确认**」：一批里只想覆盖其中几个时用。
///
/// 正文由**这里**统一生成（冲突数量 + 示例）⇒ 只有一份说法。
pub fn ask_conflict<F: FnOnce(Option<ConflictChoice>) + 'static>(
    parent: Option<&gtk::Window>,
    conflicts: &[String],
    done: F,
) {
    let n = conflicts.len();
    let mut msg = format!("有 {n} 个目标已存在：\n");
    for p in conflicts.iter().take(5) {
        msg.push_str(p);
        msg.push('\n');
    }
    if n > 5 {
        msg.push_str(&format!("… 等共 {n} 个\n"));
    }
    msg.push_str("\n要覆盖它们，还是跳过已存在的文件？");

    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        "目标已存在",
        parent,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("跳过已存在", gtk::ResponseType::Apply),
            ("逐个确认", gtk::ResponseType::Other(1)),
            ("覆盖全部", gtk::ResponseType::Ok),
        ],
    );
    // ⚠️ 默认响应 = 「跳过已存在」：回车 = 不动目标机上已有的文件
    dlg.set_default_response(gtk::ResponseType::Apply);
    // 默认框看得见：用户得知道回车按的是哪一个
    mark_default(&dlg, "跳过已存在");
    let lbl = gtk::Label::new(Some(&msg));
    lbl.set_wrap(true);
    lbl.set_use_markup(false);
    lbl.set_halign(gtk::Align::Start);
    lbl.set_selectable(true);
    let ca = dlg.content_area();
    ca.set_margin_top(8);
    ca.set_margin_bottom(8);
    ca.set_margin_start(12);
    ca.set_margin_end(12);
    ca.append(&lbl);
    style_dialog(&dlg);
    dlg.present();
    let pwin = parent.cloned();
    let items: Vec<String> = conflicts.to_vec();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        dlg.destroy();
        let Some(f) = done.borrow_mut().take() else { return };
        match resp {
            gtk::ResponseType::Ok => f(Some(ConflictChoice::All)),
            gtk::ResponseType::Apply => f(Some(ConflictChoice::Skip)),
            gtk::ResponseType::Other(1) => {
                // 逐个确认：一个一个问
                let d = std::rc::Rc::new(std::cell::RefCell::new(Some(f)));
                ask_one_by_one(
                    pwin.clone(),
                    items.clone(),
                    0,
                    std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
                    d,
                );
            }
            _ => f(None),
        }
    });
}

/// 「逐个确认」：一个一个问（阶段 79）。
///
/// 每个冲突目标问一次「覆盖这一个 / 跳过这一个」，答完把结果按
/// **与冲突列表一一对应**的顺序交回去。
fn ask_one_by_one<F: FnOnce(Option<ConflictChoice>) + 'static>(
    parent: Option<gtk::Window>,
    items: Vec<String>,
    idx: usize,
    out: std::rc::Rc<std::cell::RefCell<Vec<bool>>>,
    done: std::rc::Rc<std::cell::RefCell<Option<F>>>,
) {
    let Some(item) = items.get(idx).cloned() else {
        let f = done.borrow_mut().take();
        if let Some(f) = f {
            f(Some(ConflictChoice::PerItem(out.borrow().clone())));
        }
        return;
    };
    let total = items.len();
    let dlg = crate::windows::dialog(
        "逐个确认",
        parent.as_ref(),
        &[
            ("跳过这一个", gtk::ResponseType::Cancel),
            ("覆盖这一个", gtk::ResponseType::Ok),
        ],
    );
    // 默认 = 跳过（不覆盖目标机上已有的文件）
    dlg.set_default_response(gtk::ResponseType::Cancel);
    // 默认框看得见
    mark_default(&dlg, "跳过这一个");
    let lbl = gtk::Label::new(Some(&format!(
        "目标已存在（第 {} / {} 个）：\n{}\n\n要覆盖它吗？",
        idx + 1,
        total,
        item
    )));
    lbl.set_wrap(true);
    lbl.set_halign(gtk::Align::Start);
    lbl.set_selectable(true);
    let ca = dlg.content_area();
    ca.set_margin_top(8);
    ca.set_margin_bottom(8);
    ca.set_margin_start(12);
    ca.set_margin_end(12);
    ca.append(&lbl);
    style_dialog(&dlg);
    dlg.present();

    let out2 = out.clone();
    let done2 = done.clone();
    let p2 = parent.clone();
    dlg.connect_response(move |d, resp| {
        d.destroy();
        out2.borrow_mut().push(resp == gtk::ResponseType::Ok);
        ask_one_by_one(p2.clone(), items.clone(), idx + 1, out2.clone(), done2.clone());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 危险词表含覆盖() {
        // 阶段 79：传输冲突的「覆盖全部」是破坏性动作 ⇒ 得进危险词表
        assert!(destructive_words().iter().any(|w| *w == "覆盖"));
    }

    #[test]
    fn 覆盖是危险操作跳过不是() {
        // 「覆盖全部」⇒ 红色（destructive）
        let over = "覆盖全部";
        assert!(destructive_words().iter().any(|w| over.contains(w)));
        // 「跳过已存在」⇒ 不是危险操作（它是**安全**的那一条）
        let skip = "跳过已存在";
        assert!(!destructive_words().iter().any(|w| skip.contains(w)));
    }

    #[test]
    fn 主操作词表不含危险词() {
        // 两个词表别重叠：同一个按钮不能既是主操作又是危险操作
        for k in primary_words() {
            assert!(
                !destructive_words().iter().any(|d| d == k),
                "「{k}」同时在主操作与危险词表里"
            );
        }
    }
}
