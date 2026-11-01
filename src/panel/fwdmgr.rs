//! 端口转发管理窗口（Rust-only 功能）：列出当前 SSH 连接上的进程内
//! 转发规则，支持运行时新增 / 启用停用 / 删除。
//!
//! **三种类型都在进程内**（都跑在应用自己那条 SSH 连接上）：
//! - 本地（-L）：本机监听 → 服务器侧目标
//! - 远程（-R）：远端监听 → 本机侧目标
//! - 动态（-D）：本机 SOCKS5 代理
//!
//! 新增的规则只走进程内实现（不经终端 ssh argv），与命令行 -L/-R/-D 的
//! 承接策略互不冲突；连接断开时列表随 Forwards 事件清空。
//!
//! 停用只作用于当前会话：规则本体保留（列表里仍可见、可再启用），
//! 但不随连接持久化——持久化在连接管理器里改（connections.ini 的
//! 「进程内端口映射」）。由终端 ssh argv 承接的规则不在这里列出
//! （进程内没绑，详见下方说明）。

use std::rc::Rc;

use gtk::prelude::*;

use super::worker::{FwdView, WorkerCmd};
use super::PanelState;

pub struct FwdUi {
    pub win: gtk::Window,
    list: gtk::ListBox,
    /// 类型下拉（冒烟要断言三种类型都在）
    kind_box: gtk::ComboBoxText,
    /// 按类型切换的字段页（冒烟要断言换页）
    stack: gtk::Stack,
    /// 「保存到连接配置」按钮（冒烟要点**真按钮**，见 [`save_button`]）
    save_cfg: gtk::Button,
    /// 连接状态提示（连接没建立时列表必然是空的，得说清楚）
    conn_hint: gtk::Label,
}

/// 立刻启停一条**进程内**转发规则（会话正开着才有效）。
///
/// 两棵树的右键菜单用：配置层已经改好了，这里让**当前会话**马上跟上
/// （不然用户得断开重连才看得出启停生效）。
pub fn set_enabled_now(st: &Rc<PanelState>, key: &str, enabled: bool) {
    let tx = st.tx.borrow().clone();
    if let Some(tx) = tx {
        let _ = tx.send(WorkerCmd::ForwardSetEnabled {
            key: key.to_string(),
            enabled,
        });
    }
}

/// 启用/停用一条转发规则（两棵树的右键菜单用，阶段 58）。
///
/// - **配置层**：规则本体的 `enabled` 翻一下并写回 connections.ini
///   （**下次连接**自动带上）；
/// - **运行层**：会话正开着的话，进程内规则**立刻**启停。终端承接的规则由
///   ssh 进程管，运行态改不了，只能等下次连接。
///
/// 返回启停后的新状态；`Err` = **没改**（原因给用户看）。
pub fn toggle_rule(
    entry: &crate::connmgr::ConnEntry,
    source: crate::connmgr::FwdSource,
    rule_key: &str,
) -> Result<bool, String> {
    let cur = crate::connmgr::fwd_enabled(entry, source, rule_key)
        .ok_or_else(|| "规则不在列表里".to_string())?;
    let next = !cur;
    crate::connmgr::set_fwd_enabled(entry, source, rule_key, next)?;
    if source == crate::connmgr::FwdSource::InProc {
        let skey = crate::connmgr::entry_session_key(entry);
        if let Some(st) = crate::tabs::find_session(&skey) {
            set_enabled_now(&st, rule_key, next);
        }
    }
    Ok(next)
}

/// 冒烟测试用：拿「保存到连接配置」那个**真按钮**。
///
/// ⚠️ 冒烟要点它（`emit_clicked`），不要去调内部函数——绕开按钮层的话，
/// 按钮灰着、回调写错了都照样「通过」（AGENTS.md 记的坑）。
pub fn save_button(st: &Rc<PanelState>) -> Option<gtk::Button> {
    let slot = st.fwd_ui.borrow();
    let ui = slot.as_ref()?;
    Some(ui.save_cfg.clone())
}

/// 冒烟测试用：保存按钮是不是**贴着规则列表**（而不是在下面的表单旁边）。
///
/// 走真实控件树：从按钮往上找 `pp-fwd-list-head` 这个 CSS 类。
/// 「按钮位置会误导用户」是这次要修的东西——只断言按钮还在没用（它一直在）。
pub fn save_button_in_list_area(st: &Rc<PanelState>) -> bool {
    let slot = st.fwd_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let mut w: Option<gtk::Widget> = Some(ui.save_cfg.clone().upcast());
    while let Some(cur) = w {
        if cur.has_css_class("pp-fwd-list-head") {
            return true;
        }
        w = cur.parent();
    }
    false
}

/// 冒烟测试用：保存按钮上的文字（改名了，防回退）。
pub fn save_button_label(st: &Rc<PanelState>) -> String {
    let slot = st.fwd_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.save_cfg.label().map(|s| s.to_string()).unwrap_or_default()
}

/// 冒烟测试用：转发窗口的探针。
///
/// 返回若干行字符串：类型下拉的项数、当前选中的类型、当前显示的字段页，
/// 以及列表里每一行的文字。离线断言用（不连服务器）。
pub fn probe(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.fwd_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    out.push(format!(
        "kind={}",
        ui.kind_box
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_default()
    ));
    out.push(format!(
        "page={}",
        ui.stack
            .visible_child_name()
            .map(|s| s.to_string())
            .unwrap_or_default()
    ));
    let mut child = ui.list.first_child();
    while let Some(c) = child {
        if let Some(t) = first_label_text(&c) {
            out.push(format!("row={t}"));
        }
        child = c.next_sibling();
    }
    out
}

/// 冒烟测试用：切类型（走真实控件的 set_active_id，触发真实回调换页）。
pub fn set_kind(st: &Rc<PanelState>, kind: &str) {
    let slot = st.fwd_ui.borrow();
    if let Some(ui) = slot.as_ref() {
        ui.kind_box.set_active_id(Some(kind));
    }
}

/// 递归找第一个 Label 的文字（列表行是 Box 包 Label）。
fn first_label_text(w: &gtk::Widget) -> Option<String> {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return Some(l.text().to_string());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(t) = first_label_text(&c) {
            return Some(t);
        }
        child = c.next_sibling();
    }
    None
}

/// 懒创建并显示（传输队列同款：hide-on-close 非模态窗口）。
pub fn ensure(st: &Rc<PanelState>) {
    if st.fwd_ui.borrow().is_none() {
        let ui = build(st);
        st.fwd_ui.replace(Some(ui));
    }
    let ui = st.fwd_ui.borrow();
    let ui = ui.as_ref().unwrap();
    rebuild(st, &ui.list);
    ui.win.present();
}

/// Forwards 事件到达时刷新开着的窗口（状态与列表始终以事件为准）。
pub fn refresh(st: &Rc<PanelState>) {
    if let Some(ui) = st.fwd_ui.borrow().as_ref() {
        rebuild(st, &ui.list);
        set_conn_hint(st, &ui.conn_hint);
    }
}

fn icon_btn(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let img = gtk::Image::from_icon_name(icon);
    b.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走（读屏软件念的是它）
    crate::a11y::watch_tooltip(&b);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b
}

/// 带占位提示的输入框。
fn entry(placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_placeholder_text(Some(placeholder));
    e
}

/// 两行两列的字段网格（上排两个、下排两个）。
fn grid2(
    a: &gtk::Entry,
    b: &gtk::Entry,
    c: &gtk::Entry,
    d: &gtk::Entry,
) -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_row_spacing(6);
    g.set_column_spacing(6);
    g.attach(a, 0, 0, 1, 1);
    g.attach(b, 1, 0, 1, 1);
    g.attach(c, 0, 1, 1, 1);
    g.attach(d, 1, 1, 1, 1);
    g
}

fn build(st: &Rc<PanelState>) -> FwdUi {
    let win = gtk::Window::new();
    win.set_title(Some("端口转发 — PuTTYPlus"));
    let (fw, fh) = crate::settings::window_size("fwd").unwrap_or((640, 520));
    win.set_default_size(fw, fh);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_start(10);
    vbox.set_margin_end(10);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);

    let hint = gtk::Label::new(Some(
        "进程内端口映射：全部跑在应用自己这条 SSH 连接上（不新建 SSH 连接，终端标签页关了也照样在）。\n\
         本地（-L）本机监听 → 服务器侧目标；远程（-R）远程监听 → 本机侧目标；动态（-D）本机 SOCKS5 代理。\n\
         停用只在本次会话有效；要长期保留，点列表右上角的「保存列表到连接配置」\n\
         （ssh config 别名也可以，规则存进 PuTTYPlus 自己的配置，不动 ~/.ssh/config）。",
    ));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_max_width_chars(64);
    vbox.append(&hint);

    // 连接状态：没连上时列表必然是空的（规则加不进去），得说在前面，
    // 否则用户只能从「保存了 0 条规则」这种反话里猜
    let conn_hint = gtk::Label::new(Some(""));
    conn_hint.add_css_class("dim-label");
    conn_hint.set_xalign(0.0);
    conn_hint.set_wrap(true);
    conn_hint.set_max_width_chars(64);
    vbox.append(&conn_hint);
    set_conn_hint(st, &conn_hint);

    // ---- 规则列表区：标题行 + 「保存列表到连接配置」 ----
    //
    // ⚠ **这个按钮为什么放列表这儿（而不是跟「添加转发」并排放在表单底下）**：
    // 它保存的是**列表里的规则**，跟下面表单填的那几项**没关系**。按钮贴着
    // 表单时，用户理所当然地以为「保存 = 存我刚填的那条」，点完才发现存的是
    // 另一份东西（列表）——位置本身就是误导。挪到列表标题行，「保存什么」
    // 跟按钮贴在了一起。
    let list_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // CSS 类给冒烟用：从按钮往上找这个类，断言它确实在列表区里
    list_head.add_css_class("pp-fwd-list-head");
    let list_title = gtk::Label::new(Some("当前会话的转发规则"));
    list_title.set_xalign(0.0);
    list_title.set_hexpand(true);
    list_head.append(&list_title);
    let list_note = gtk::Label::new(Some("（保存按钮保存的就是这份列表）"));
    list_note.add_css_class("dim-label");
    list_note.set_xalign(0.0);
    list_head.append(&list_note);

    // 「保存到连接配置」按钮（冒烟要点**真按钮**，见 [`save_button`]）
    let save_cfg = gtk::Button::with_label("保存列表到连接配置");
    save_cfg.set_tooltip_text(Some(
        "把上面列表里的规则整份存进这条连接的配置（覆盖式）：\n\
         下次连接自动带上。\n\
         保存的是列表，不是下面表单里填的内容。",
    ));
    list_head.append(&save_cfg);
    vbox.append(&list_head);

    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_vexpand(true);
    // 给列表一个最小高度：不然窗口一挤，规则行就被裁掉（用户以为没规则）
    sw.set_min_content_height(150);
    let list = gtk::ListBox::new();
    sw.set_child(Some(&list));
    vbox.append(&sw);

    // ---- 添加区：类型下拉 + 按类型切换的字段 ----
    //
    // 三种类型的方向完全不同，字段摆在一页上必然有人填错：
    // - 本地（-L）：本机监听 → 服务器侧目标
    // - 远程（-R）：远端监听 → 本机侧目标
    // - 动态（-D）：本机 SOCKS5，目标由客户端自己说
    // 所以用 Stack 按类型换页，别用一堆 sensitive 开关。
    let kind_box = gtk::ComboBoxText::new();
    kind_box.append(Some("L"), "本地（-L）：本机监听 → 服务器侧目标");
    kind_box.append(Some("R"), "远程（-R）：远程监听 → 本机侧目标");
    kind_box.append(Some("D"), "动态（-D）：本机 SOCKS5 代理");
    // ComboBoxText 一个都不选会显示空白：默认选中第一项
    kind_box.set_active_id(Some("L"));
    vbox.append(&kind_box);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let e_lhost = entry("监听地址（默认 127.0.0.1）");
    let e_lport = entry("监听端口");
    let e_thost = entry("目标主机（服务器侧解析）");
    let e_tport = entry("目标端口");
    let g_local = grid2(&e_lhost, &e_lport, &e_thost, &e_tport);
    stack.add_named(&g_local, Some("L"));

    let e_bhost = entry("远程绑定地址（默认 127.0.0.1）");
    let e_bport = entry("远程监听端口");
    let e_rthost = entry("目标主机（本机侧解析）");
    let e_rtport = entry("目标端口");
    let g_remote = grid2(&e_bhost, &e_bport, &e_rthost, &e_rtport);
    stack.add_named(&g_remote, Some("R"));

    let e_shost = entry("监听地址（默认 127.0.0.1）");
    let e_sport = entry("SOCKS 端口");
    let g_socks = gtk::Grid::new();
    g_socks.set_row_spacing(6);
    g_socks.set_column_spacing(6);
    g_socks.attach(&e_shost, 0, 0, 1, 1);
    g_socks.attach(&e_sport, 1, 0, 1, 1);
    stack.add_named(&g_socks, Some("D"));

    stack.set_visible_child_name("L");
    vbox.append(&stack);

    {
        let stack = stack.clone();
        let kind_box = kind_box.clone();
        kind_box.connect_changed(move |cb| {
            let kind = cb
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "L".into());
            stack.set_visible_child_name(&kind);
        });
    }

    let add = gtk::Button::with_label("添加转发");
    {
        let st2 = Rc::clone(st);
        let kind_box = kind_box.clone();
        let e_lhost = e_lhost.clone();
        let e_lport = e_lport.clone();
        let e_thost = e_thost.clone();
        let e_tport = e_tport.clone();
        let e_bhost = e_bhost.clone();
        let e_bport = e_bport.clone();
        let e_rthost = e_rthost.clone();
        let e_rtport = e_rtport.clone();
        let e_shost = e_shost.clone();
        let e_sport = e_sport.clone();
        add.connect_clicked(move |_| {
            let kind = kind_box
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "L".into());
            // 内部形（与 ConnOptions.forwards 同形）：kind + 规格体
            let spec = match kind.as_str() {
                "R" => {
                    let bh = e_bhost.text().trim().to_string();
                    let bp = e_bport.text().trim().to_string();
                    let th = e_rthost.text().trim().to_string();
                    let tp = e_rtport.text().trim().to_string();
                    if bp.is_empty() || th.is_empty() || tp.is_empty() {
                        crate::panel::dialogs::show_error(
                            None,
                            "转发规则不完整",
                            "远程转发（-R）要填：远程监听端口、目标主机、目标端口。",
                        );
                        return;
                    }
                    format!("R:{bh}:{bp}:{th}:{tp}")
                }
                "D" => {
                    let sp = e_sport.text().trim().to_string();
                    if sp.is_empty() {
                        crate::panel::dialogs::show_error(
                            None,
                            "转发规则不完整",
                            "动态转发（-D）要填：SOCKS 端口。",
                        );
                        return;
                    }
                    let sh = e_shost.text().trim().to_string();
                    if sh.is_empty() {
                        format!("D:{sp}")
                    } else {
                        format!("D:{sh}:{sp}")
                    }
                }
                _ => {
                    let lh = e_lhost.text().trim().to_string();
                    let lp = e_lport.text().trim().to_string();
                    let th = e_thost.text().trim().to_string();
                    let tp = e_tport.text().trim().to_string();
                    if lp.is_empty() || th.is_empty() || tp.is_empty() {
                        crate::panel::dialogs::show_error(
                            None,
                            "转发规则不完整",
                            "本地转发（-L）要填：监听端口、目标主机、目标端口。",
                        );
                        return;
                    }
                    format!("L:{lh}:{lp}:{th}:{tp}")
                }
            };
            let _ = st2
                .tx
                .borrow()
                .as_ref()
                .unwrap()
                .send(WorkerCmd::ForwardAdd { spec });
        });
    }
    // 保存到连接配置：把当前这份规则集写回连接管理器，下次连上自动装载。
    // 堡垒机场景每次都要用这套映射，靠手敲一遍是不能忍的。
    {
        let st2 = Rc::clone(st);
        save_cfg.connect_clicked(move |_| {
            let key = crate::backend::dbstore::session_key(&st2.opts);
            // 会话标题 = 条目名：多个 ssh 别名指向同一台机器时用它区分
            let hint = st2.opts.title.clone();
            let target = match crate::connmgr::find_fwd_save_target(&key, hint.as_deref()) {
                Ok(t) => t,
                Err(crate::connmgr::FwdTargetErr::NoEntry) => {
                    crate::panel::dialogs::show_error(
                        None,
                        "保存失败",
                        "这条会话不是从连接管理器里的条目连上的（命令行直接开的），\n\
                         没有地方写回配置。\n\
                         先在连接管理器里建一个条目，或从已有的条目连上再来保存。",
                    );
                    return;
                }
                Err(crate::connmgr::FwdTargetErr::Ambiguous(names)) => {
                    crate::panel::dialogs::show_error(
                        None,
                        "保存失败",
                        &format!(
                            "有多个 ssh 别名指向这台机器（{}），\n\
                             说不清规则该存哪一条。\n\
                             请在连接管理器里建一个自建条目，再从它连接。",
                            names.join("、")
                        ),
                    );
                    return;
                }
            };
            // 借用别跨过保存调用（RefCell 借用在 match/if 里容易双借 panic）
            let value = {
                let views = st2.fwd_views.borrow();
                views
                    .iter()
                    .map(|v| {
                        if v.enabled {
                            v.key.clone()
                        } else {
                            format!("!{}", v.key)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let n = value.split(',').filter(|s| !s.is_empty()).count();
            // 别名的规则存在 ini 的单独一段里，文案要说清楚落在哪
            let (name, where_) = match &target {
                crate::connmgr::FwdSaveTarget::Custom(n) => {
                    (n.clone(), "的「进程内端口映射」".to_string())
                }
                crate::connmgr::FwdSaveTarget::Alias(a) => (
                    a.clone(),
                    "的「进程内端口映射」（ssh 别名，存 PuTTYPlus 自己的配置）".to_string(),
                ),
            };
            // ⚠️ **列表是空的，别直接写**：这时点「保存」等于把已存的规则清空。
            // 而连接没建立时列表**本来就是空的**（规则加不进去，worker 回
            // 「未连接」），用户很可能只是忘了先添加规则——直接清就把之前
            // 存的规则抹了。所以先问一句。
            if n == 0 {
                crate::panel::dialogs::confirm_with(
                    None,
                    &format!(
                        "当前列表里没有规则。\n\
                         点「保存」等于把连接「{name}」里已存的转发规则「清空」\n\
                         （下次连接不再自动带上）。\n\
                         要清空吗？\n\
                         如果你是想保存规则：先在上面的列表里添加规则，\n\
                         列表有内容了再保存。"
                    ),
                    "清空已存规则",
                    move |ok| {
                        if !ok {
                            return;
                        }
                        if crate::connmgr::save_inproc_forwards_to_target(&target, "") {
                            crate::panel::dialogs::info(
                                None,
                                "已清空",
                                &format!("已把连接「{name}」里存的转发规则清空。\n下次连接不再自动带上。"),
                            );
                        } else {
                            crate::panel::dialogs::show_error(
                                None,
                                "清空失败",
                                "写不进 connections.ini（目录权限或磁盘问题）。",
                            );
                        }
                    },
                );
                return;
            }
            if crate::connmgr::save_inproc_forwards_to_target(&target, &value) {
                crate::panel::dialogs::info(
                    None,
                    "已保存",
                    &format!(
                        "已把 {n} 条规则写进连接「{name}」{where_}。\n下次连接自动生效。"
                    ),
                );
            } else {
                crate::panel::dialogs::show_error(
                    None,
                    "保存失败",
                    "写不进 connections.ini（目录权限或磁盘问题）。",
                );
            }
        });
    }
    // 底部按钮行：只有「添加转发」。
    // ⚠ 「保存列表到连接配置」**不在这里**——它保存的是上面那份列表，
    // 跟这个表单填的内容无关（放一起会让人以为存的是刚填的那条）。
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    btns.append(&add);
    vbox.append(&btns);

    win.set_child(Some(&vbox));
    // 关闭即隐藏（对应传输队列窗口的语义）
    win.connect_close_request(|w| {
        crate::settings::save_window_size("fwd", w);
        w.hide();
        glib::Propagation::Stop
    });

    FwdUi {
        win,
        list,
        kind_box,
        stack,
        save_cfg,
        conn_hint,
    }
}

/// 连接状态提示文案。
///
/// **为什么要单独一行**：连接没建立时，「添加」会被 worker 挡回「未连接」，
/// 规则列表于是**一直是空的**。不提前说，用户只会看到「保存了 0 条规则」
/// 这种反话，猜不出真正的原因。
fn set_conn_hint(st: &Rc<PanelState>, lbl: &gtk::Label) {
    let text = match crate::panel::current_state(st) {
        Some(crate::theme::State::Ok) => "会话已连接：可以添加转发规则。".to_string(),
        _ => "会话还没连上：此时添加规则会失败（提示「未连接」），\n\
              规则列表要等连接建立后才有内容。"
            .to_string(),
    };
    lbl.set_text(&text);
}

/// 按 st.fwd_views 重建列表行（Forwards 事件驱动，不做增量）。
fn rebuild(st: &Rc<PanelState>, list: &gtk::ListBox) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let views = st.fwd_views.borrow();
    if views.is_empty() {
        let l = gtk::Label::new(Some("（无转发规则）"));
        l.add_css_class("dim-label");
        l.set_margin_top(12);
        l.set_margin_bottom(12);
        l.set_halign(gtk::Align::Center);
        list.append(&l);
        return;
    }
    for v in views.iter() {
        list.append(&row(st, v));
    }
}

/// 一行规则：描述（运行态） + 启用停用 + 删除
fn row(st: &Rc<PanelState>, v: &FwdView) -> gtk::ListBoxRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start(4);
    row.set_margin_end(4);

    let lbl = gtk::Label::new(Some(&v.line_text()));
    lbl.set_hexpand(true);
    lbl.set_halign(gtk::Align::Start);
    lbl.set_selectable(true);
    if !v.active {
        lbl.add_css_class("dim-label");
    }
    if let Some(e) = &v.err {
        lbl.set_tooltip_text(Some(&format!("启动失败：{e}")));
    }
    row.append(&lbl);

    let (icon, tip) = if v.enabled {
        ("media-playback-pause-symbolic", "停用该转发（规则保留，可再启用）")
    } else {
        ("media-playback-start-symbolic", "启用该转发")
    };
    let toggle = icon_btn(icon, tip);
    {
        let st2 = Rc::clone(st);
        let key = v.key.clone();
        let enabled = !v.enabled;
        // connect_clicked 要 Fn 闭包（可多次调用），key 得每次自己 clone
        toggle.connect_clicked(move |_| {
            let _ = st2
                .tx
                .borrow()
                .as_ref()
                .unwrap()
                .send(WorkerCmd::ForwardSetEnabled {
                    key: key.clone(),
                    enabled,
                });
        });
    }
    row.append(&toggle);

    let del = icon_btn("user-trash-symbolic", "停止并移除该转发");
    {
        let st2 = Rc::clone(st);
        let key = v.key.clone();
        del.connect_clicked(move |_| {
            let _ = st2
                .tx
                .borrow()
                .as_ref()
                .unwrap()
                .send(WorkerCmd::ForwardRemove { key: key.clone() });
        });
    }
    row.append(&del);

    let lbrow = gtk::ListBoxRow::new();
    lbrow.set_child(Some(&row));
    lbrow
}
