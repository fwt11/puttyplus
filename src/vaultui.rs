//! 凭据库 UI：设置/解锁主密码、**凭据库管理器**（看库里有哪些连接、
//! 改主密码、重置、锁定）。
//!
//! 入口挂在连接管理器底部按钮条。解锁后保持到进程退出——本项目常被
//! 当堡垒机 launcher 用（`spawn_self` 派生新窗口），启动就拦一道没法用。
//!
//! ## 为什么有"管理器"而不是"解锁完就一句提示"
//!
//! 原先的流程是：点「凭据库…」→ 弹密码框 → 提示「凭据库已解锁」→ **完事**。
//! 用户解锁了却**什么也看不到**：库里存了哪些连接、密码对不对、有没有必要
//! 改主密码，一概不知道。管理器窗口把这些摆出来，还给出「改主密码」
//! 「重置」「锁定」三个动作。
//!
//! ## 空主密码（不加密）模式
//!
//! 堡垒机场景下，用户要的往往只是"本机存一下密码方便下次用"，并不需要
//! 一层主密码。勾「不设主密码」→ 凭据**明文**存在文件里，文件头明写
//! `protected=false`——**一眼就能看出这份库没加密**，而不是看到一堆
//! base64 就以为它是安全的。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use zeroize::Zeroizing;

use crate::connmgr::ConnEntry;
use crate::vault;

/// 主密码对话框的选项。
///
/// **公开**：冒烟测试要直接开这个对话框（验"密码输错时不关门"那种
/// 只有真对话框才测得出来的行为）。
pub struct MasterOpts {
    /// 标题。
    pub title: String,
    /// 顶部提示。
    pub hint: String,
    /// 要输两遍（设置 / 修改）。
    pub twice: bool,
    /// 允许留空 = **不加密**（勾复选框，带风险说明）。
    pub allow_empty: bool,
    /// 解锁模式：就地校验、**失败不关门**、带「忘记主密码？」出路。
    pub unlock: bool,
}

/// 主密码对话框（异步：GTK 对话框是事件驱动的，值只能回调出去）。
///
/// **解锁模式（`opts.unlock`）在这里就地解锁**：密码错时**不关对话框**，
/// 只在框里显示错误——用户当场就能改密码重试，也能直接点「忘记主密码？」
/// 走重置。用户最需要出路的时候，对话框却"啪"地关掉、只剩一个报错弹窗，
/// 那才叫把人堵死。
///
/// **返回对话框句柄**：一般调用方不用管，但冒烟测试要靠它找到那颗
/// 「忘记主密码？」按钮（按钮在内容区里，没有别的入口能拿到）。
pub fn ask_master<F: FnOnce(Option<Zeroizing<String>>) + 'static>(
    parent: Option<&gtk::Window>,
    opts: MasterOpts,
    done: F,
) -> gtk::Dialog {
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        &opts.title,
        parent,
        &[("取消", gtk::ResponseType::Cancel), ("确定", gtk::ResponseType::Ok)],
    );
    dlg.set_default_size(380, -1);
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    let h = gtk::Label::new(Some(&opts.hint));
    h.set_xalign(0.0);
    h.add_css_class("dim-label");
    vbox.append(&h);

    let e1 = gtk::Entry::new();
    e1.set_visibility(false);
    e1.set_placeholder_text(Some("主密码"));
    vbox.append(&crate::panel::dialogs::form_row("主密码", &e1));

    let e2 = gtk::Entry::new();
    if opts.twice {
        e2.set_visibility(false);
        e2.set_placeholder_text(Some("再输一遍"));
        vbox.append(&crate::panel::dialogs::form_row("确认主密码", &e2));
    }

    // 「不设主密码」= 不加密。**默认不勾**——这是安全倒退，得用户自己
    // 明确选，不能让"忘了密码"顺手滑到"不加密"上去。
    let no_pw = gtk::CheckButton::with_label("不设主密码（不加密保存）");
    let risk = gtk::Label::new(Some(
        "⚠ 凭据库里的密码将以明文存在本机文件里。\n\
         任何能读到这个文件的人都能拿到这些密码——\
         只在自己的机器上用、且清楚这个风险时再选。",
    ));
    risk.add_css_class("pp-error");
    risk.set_xalign(0.0);
    risk.set_wrap(true);
    risk.set_visible(false);
    if opts.allow_empty {
        no_pw.set_visible(true);
        risk.set_visible(false);
        vbox.append(&no_pw);
        vbox.append(&risk);
        // clone：e1/e2 后面还要在响应回调里用（这里 move 走了就编译不过）
        let (e1c, e2c) = (e1.clone(), e2.clone());
        no_pw.connect_toggled(move |b| {
            risk.set_visible(b.is_active());
            e1c.set_sensitive(!b.is_active());
            e2c.set_sensitive(!b.is_active());
        });
    } else {
        no_pw.set_visible(false);
        risk.set_visible(false);
    }

    // 错误提示行：解锁失败时显示，**对话框不关**。
    let err = gtk::Label::new(None);
    err.set_xalign(0.0);
    err.set_wrap(true);
    err.add_css_class("pp-error");
    err.set_visible(false);
    vbox.append(&err);

    // 解锁模式：给一条**出路**。
    //
    // ⚠️ 主密码忘了不是死路——代价只是"已保存的密码重新输一遍"。
    // 不给这个入口，用户就只能卡在"解锁失败"里，从此再也存不了密码。
    if opts.unlock {
        let b = gtk::Button::with_label("忘记主密码？重置凭据库");
        // 只用 `flat`（透明底、悬停有底色 → 看得出能点），**不**用
        // `pp-hint` 的弱化色：这是用户被堵住时**唯一的出路**，字太淡
        // 会没人注意到，那这入口等于白加。
        b.add_css_class("flat");
        b.set_halign(gtk::Align::Start);
        let pwin = parent.cloned();
        let dlg2 = dlg.clone();
        b.connect_clicked(move |_| {
            // 先关解锁框（它已经没用了），再开重置流程：两个模态框叠着
            // 会让用户分不清自己在哪个流程里。
            dlg2.close();
            let p = pwin.clone();
            reset_flow(p.as_ref());
        });
        vbox.append(&b);
    }

    dlg.content_area().append(&vbox);
    crate::panel::dialogs::style_dialog(&dlg);
    // 句柄留一份给调用方（connect_response 会把 dlg move 进闭包）
    let handle = dlg.clone();
    dlg.present();
    let done = std::cell::RefCell::new(Some(done));
    let allow_empty = opts.allow_empty;
    let unlock = opts.unlock;
    let twice = opts.twice;
    dlg.connect_response(move |dlg, resp| {
        if resp == gtk::ResponseType::Ok {
            // 勾了"不设主密码" → 主密码就是空串（= 不加密）
            let a = if allow_empty && no_pw.is_active() {
                String::new()
            } else {
                e1.text().to_string()
            };
            if twice && !allow_empty && a != e2.text().to_string() {
                err.set_text("两次输入的主密码不一致");
                err.set_visible(true);
                return; // 保持对话框打开
            }
            if unlock {
                // 解锁模式：当场验，失败不关门
                match vault::unlock(&a) {
                    Ok(()) => {
                        dlg.destroy();
                        notify_state_changed();
                        if let Some(f) = done.borrow_mut().take() {
                            f(Some(Zeroizing::new(a)));
                        }
                        return;
                    }
                    Err(e) => {
                        err.set_text(&format!(
                            "{e}\n\n忘了主密码？点下面的「忘记主密码？」可以重置凭据库——\
                             已保存的密码会清空、需要重新输入一遍。"
                        ));
                        err.set_visible(true);
                        e1.set_text("");
                        e1.grab_focus();
                        return;
                    }
                }
            }
            dlg.destroy();
            if let Some(f) = done.borrow_mut().take() {
                f(Some(Zeroizing::new(a)));
            }
            return;
        }
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(None);
        }
    });
    handle
}

/// **改主密码**对话框：旧密码（如果库有主密码）+ 新密码两遍。
///
/// ⚠️ **新密码不许留空**：留空 = 把库降级成明文。降级是**安全倒退**，
/// 不能跟"改密码"混在一个对话框里让人顺手点到——要降级就重置（那里
/// 有后果确认）。
fn ask_change_master<F: FnOnce(Option<(String, String)>) + 'static>(
    parent: Option<&gtk::Window>,
    has_old: bool,
    done: F,
) {
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        "改主密码",
        parent,
        &[("取消", gtk::ResponseType::Cancel), ("确定", gtk::ResponseType::Ok)],
    );
    dlg.set_default_size(380, -1);
    let vbox = crate::panel::dialogs::form_box();

    let hint = gtk::Label::new(Some(if has_old {
        "改主密码：先验证当前主密码，再设一个新的。\n\
         库里的连接密码会原样搬过去，一条都不会丢。"
    } else {
        "这个凭据库没有主密码（不加密模式）。\n\
         设一个主密码就会把它变成加密库——库里的连接密码原样保留。"
    }));
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.add_css_class("dim-label");
    vbox.append(&hint);

    let old = gtk::Entry::new();
    old.set_visibility(false);
    old.set_placeholder_text(Some("当前主密码"));
    if has_old {
        vbox.append(&crate::panel::dialogs::form_row("当前主密码", &old));
    }

    let e1 = gtk::Entry::new();
    e1.set_visibility(false);
    e1.set_placeholder_text(Some("新主密码"));
    vbox.append(&crate::panel::dialogs::form_row("新主密码", &e1));
    let e2 = gtk::Entry::new();
    e2.set_visibility(false);
    e2.set_placeholder_text(Some("再输一遍"));
    vbox.append(&crate::panel::dialogs::form_row("确认新主密码", &e2));

    let err = gtk::Label::new(None);
    err.set_xalign(0.0);
    err.set_wrap(true);
    err.add_css_class("pp-error");
    err.set_visible(false);
    vbox.append(&err);

    dlg.content_area().append(&vbox);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        if resp == gtk::ResponseType::Ok {
            let a = e1.text().to_string();
            if a.is_empty() {
                err.set_text("新主密码不能为空\n\
                             （想把库改成不加密，请用「重置凭据库」——那里会先说清后果）");
                err.set_visible(true);
                return;
            }
            if a != e2.text().to_string() {
                err.set_text("两次输入的新主密码不一致");
                err.set_visible(true);
                return;
            }
            let o = old.text().to_string();
            dlg.destroy();
            if let Some(f) = done.borrow_mut().take() {
                f(Some((o, a)));
            }
            return;
        }
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(None);
        }
    });
}

/// 重置凭据库：讲清后果 → 设新主密码 → 重建空库。
///
/// **分两步是刻意的**：先把"会丢什么、不会丢什么"摆出来并要一次确认，
/// 再要新主密码。用户看到"永久丢失"还能反悔；反过来先输完密码再被告知
/// 后果，就只剩"确认"这一个选择了。
fn reset_flow(parent: Option<&gtk::Window>) {
    let p2 = parent.cloned();
    crate::panel::dialogs::confirm_with(
        parent,
        // ⚠️ **别用 markdown 星号**：`dialogs::confirm_with` 里是**普通**
        // `gtk::Label`（不是 markup label），`**` 会**原样显示**出来。
        // 强调靠措辞和分段，不靠星号。
        "重置凭据库？\n\n\
         主密码是解锁凭据库的唯一钥匙。忘了它（或文件损坏），\
         里面的密码就再也解不开——这里没有后门。\n\n\
         重置会丢弃整个凭据库：已保存的连接密码全部永久丢失，\
         需要重新输入一遍。\n\n\
         不会丢的：连接列表、数据库连接的定义、\
         「记住密码」的勾选都还在。重新连上时弹一次密码框、\
         勾上「记住密码」，就又存进新库了。",
        "重置凭据库",
        move |ok| {
            if !ok {
                return;
            }
            let p3 = p2.clone();
            // 闭包要 `move` 走自己那份 clone：直接捕获 p3 会和
            // `p3.as_ref()` 的借用打架（不能 move 出被借用的值）。
            let p4 = p3.clone();
            ask_master(
                p3.as_ref(),
                MasterOpts {
                    title: "设置新主密码".into(),
                    hint: "给重置后的凭据库设一个新主密码。\n\
                           忘了它同样只能重置，所以别用太随手的主密码。\n\
                           不设主密码也行——那样就是明文保存（堡垒机场景常用）。"
                        .into(),
                    twice: true,
                    allow_empty: true,
                    unlock: false,
                },
                move |pw| {
                    let Some(pw) = pw else { return };
                    let p5 = p4.clone();
                    match vault::reset(&pw) {
                        Ok(()) => {
                            // 重置后直接解锁：设完主密码就能接着用
                            if let Err(e) = vault::unlock(&pw) {
                                alert(p5.as_ref(), "解锁失败", &e);
                                return;
                            }
                            notify_state_changed();
                            info(
                                p5.as_ref(),
                                "凭据库已重置",
                                "凭据库已用新主密码重建。之前保存的密码已清空——\
                                 需要时重新输入一次即可（勾「记住密码」会存进新库）。",
                            );
                            open_vault_window(p5.as_ref());
                        }
                        Err(e) => alert(p5.as_ref(), "重置凭据库失败", &e),
                    }
                },
            );
        },
    );
}

/// 连接管理器里的凭据库入口。
pub fn open_manager_vault(parent: Option<&gtk::Window>, entries: Vec<ConnEntry>) {
    // 父窗口要活过回调（对话框是异步的），所以闭包得自己持有一份 clone，
    // 不能借用函数参数里的引用。
    let parent = parent.cloned();
    if !vault::exists() {
        setup_vault(parent.as_ref(), entries);
        return;
    }
    // ⚠️ **明文（不加密）库没有主密码要输**：直接进管理器。
    // 否则这里会弹一个"请输入主密码"的框——而这个库**压根没有主密码**，
    // 用户输什么都不对，最后只剩「重置」那条路（**等于白丢凭据**）。
    if !vault::is_protected() {
        let _ = vault::unlock("");
        notify_state_changed();
        open_vault_window(parent.as_ref());
        return;
    }
    if !vault::is_unlocked() {
        unlock_vault(parent.as_ref());
        return;
    }
    open_vault_window(parent.as_ref());
}

/// 第一次建库：设主密码（**可以不设**）+ 把 ini 里的明文密码收进来。
fn setup_vault(parent: Option<&gtk::Window>, entries: Vec<ConnEntry>) {
    let pwin = parent.cloned();
    ask_master(
        parent,
        MasterOpts {
            title: "设置主密码".into(),
            hint: "主密码用于加密本机保存的连接密码。\n\
                   忘了可以重置（已保存的密码要重新输一遍），但没有找回的办法。\n\
                   也可以不设主密码——那样就不加密（堡垒机场景常用）。"
                .into(),
            twice: true,
            allow_empty: true,
            unlock: false,
        },
        move |pw| {
            let Some(pw) = pw else { return };
            let pwin = pwin.clone();
            let creds = collect_passwords(&entries);
            if let Err(e) = vault::create(&pw, &creds) {
                alert(pwin.as_ref(), "创建凭据库失败", &e);
                return;
            }
            if let Err(e) = vault::unlock(&pw) {
                alert(pwin.as_ref(), "解锁失败", &e);
                return;
            }
            // 明文已进凭据库，ini 里的 password 键删掉。
            // 这一步刻意放在 create 成功之后：失败了明文还在，不会两头空。
            crate::connmgr::purge_ini_passwords();
            notify_state_changed();
            let mode = if pw.is_empty() {
                "凭据库已启用（未加密）"
            } else {
                "凭据库已启用"
            };
            info(
                pwin.as_ref(),
                mode,
                &format!(
                    "{} 条密码已从 connections.ini 迁入存储，ini 中的明文已删除。\n\n{}",
                    creds.len(),
                    status(&entries)
                ),
            );
            open_vault_window(pwin.as_ref());
        },
    );
}

/// 解锁（失败不关门，框里有「忘记主密码？」的出路）。
fn unlock_vault(parent: Option<&gtk::Window>) {
    let pwin = parent.cloned();
    ask_master(
        parent,
        MasterOpts {
            title: "解锁凭据库".into(),
            hint: "输入主密码以解密本机保存的连接密码。".into(),
            twice: false,
            allow_empty: false,
            unlock: true,
        },
        move |pw| {
            let Some(pw) = pw else { return };
            let pwin = pwin.clone();
            let _ = pw;
            open_vault_window(pwin.as_ref());
        },
    );
}

/// **凭据库管理器**：看库里存了些什么 / 改主密码 / 重置 / 锁定。
///
/// 用户解锁之后原来只得到一句「已解锁」就结束了——**什么也看不到**。
/// 这里把库里每条凭据（连接名 + 所属会话）列出来，并把「改主密码」
/// 「重置」「锁定」摆在明面上。
fn open_vault_window(parent: Option<&gtk::Window>) {
    let parent = parent.cloned();
    // 阶段 67（审计 P2-17）：凭据库管理器是**工具窗**——
    // 非模态 + **单例**（连开两次不再多一个）+ 记尺寸。
    crate::windows::open_or_focus(crate::windows::ToolKind::Vault, || {
    let win = gtk::Window::new();
    win.set_title(Some(crate::windows::ToolKind::Vault.title()));
    let (vw, vh) = crate::settings::window_size("vault").unwrap_or((600, 420));
    win.set_default_size(vw, vh);
    win.set_transient_for(parent.as_ref());
    // ⚠️ **不设 modal**：工具窗不阻塞主窗口（以前模态 ⇒ 主窗口整个不能用）

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(14);
    root.set_margin_bottom(14);
    root.set_margin_start(14);
    root.set_margin_end(14);
    win.set_child(Some(&root));

    // 状态行
    let state = gtk::Label::new(None);
    state.set_xalign(0.0);
    state.add_css_class("pp-section");
    root.append(&state);

    // 凭据列表
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let sw = gtk::ScrolledWindow::new();
    sw.set_vexpand(true);
    sw.set_child(Some(&list));
    root.append(&sw);

    // 底部按钮
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btns.set_halign(gtk::Align::End);
    root.append(&btns);

    let win_weak = win.downgrade();
    let list_weak = list.downgrade();
    let state_weak = state.downgrade();

    /// 重建列表（改主密码 / 重置 / 锁定后要刷新）。
    fn refresh(
        win: &gtk::Window,
        list: &gtk::ListBox,
        state: &gtk::Label,
        btn_unlock: &gtk::Button,
    ) {
        // 列表
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        let names = vault::names();
        if names.is_empty() {
            let l = gtk::Label::new(Some("库里还没有凭据。\n连上某个连接并勾「记住密码」就会存进来。"));
            l.set_xalign(0.0);
            l.add_css_class("dim-label");
            list.append(&l);
        } else {
            for n in &names {
                let (sess, cname) = crate::backend::dbstore::split_vault_name(n);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.set_margin_top(6);
                row.set_margin_bottom(6);
                row.set_margin_start(8);
                row.set_margin_end(8);
                let disp = if cname.is_empty() {
                    sess.clone()
                } else {
                    cname.clone()
                };
                let name = gtk::Label::new(Some(disp.as_str()));
                name.set_xalign(0.0);
                name.set_selectable(true);
                let s = gtk::Label::new(Some(&sess));
                s.set_xalign(1.0);
                s.set_hexpand(true);
                s.add_css_class("dim-label");
                s.set_selectable(true);
                s.set_tooltip_text(Some(sess.as_str()));
                row.append(&name);
                row.append(&s);
                list.append(&row);
            }
        }
        // 状态
        let n = vault::names().len();
        let text = if !vault::is_unlocked() {
            "● 已锁定 —— 要看库里的内容，得先解锁。".to_string()
        } else if vault::is_protected() {
            format!("● 已解锁（主密码保护） · 库内 {n} 条凭据")
        } else {
            format!("● 已解锁（未加密，明文保存） · 库内 {n} 条凭据")
        };
        state.set_text(&text);
        // ⚠️ 阶段 79：锁着时给一个**就地解锁**的入口——
        // 状态行光说"得先解锁"，却不给地方解锁，用户得去顶栏菜单里找。
        btn_unlock.set_visible(!vault::is_unlocked());
        win.present();
    }

    // ---- 「解锁凭据库…」（锁着时才露出来）----
    let b_unlock = unlock_button(Some(&win));
    btns.append(&b_unlock);

    // 初次填
    refresh(&win, &list, &state, &b_unlock);

    // ---- 「改主密码」----
    {
        let b = gtk::Button::with_label("改主密码…");
        b.add_css_class("flat");
        let w = win.clone();
        let l = list.clone();
        let s = state.clone();
        let bu = b_unlock.clone();
        b.connect_clicked(move |_| {
            let has_old = vault::is_protected();
            let w2 = w.clone();
            let l2 = l.clone();
            let s2 = s.clone();
            let bu2 = bu.clone();
            ask_change_master(Some(&w), has_old, move |r| {
                let Some((old, new)) = r else { return };
                match vault::change_master(&old, &new) {
                    Ok(()) => {
                        notify_state_changed();
                        info(
                            Some(&w2),
                            "主密码已改",
                            "凭据库已用新主密码重新加密，库里的连接密码一条不少。",
                        );
                        refresh(&w2, &l2, &s2, &bu2);
                    }
                    Err(e) => alert(Some(&w2), "改主密码失败", &e),
                }
            });
        });
        btns.append(&b);
    }

    // ---- 「重置凭据库」----
    {
        let b = gtk::Button::with_label("重置凭据库…");
        b.add_css_class("flat");
        let w = win.clone();
        let l = list.clone();
        let s = state.clone();
        b.connect_clicked(move |_| {
            let w2 = w.clone();
            let l2 = l.clone();
            let s2 = s.clone();
            reset_flow(Some(&w));
            // 重置是**异步**流程（确认框 → 设新密码 → 重建），完成后会自己
            // 重新打开管理器；这里把当前窗口先关掉，免得叠两个。
            w2.close();
            let _ = (l2, s2);
        });
        btns.append(&b);
    }

    // ---- 「锁定」----
    {
        let b = gtk::Button::with_label("锁定");
        b.add_css_class("flat");
        let w = win.clone();
        let l = list.clone();
        let s = state.clone();
        let bu = b_unlock.clone();
        b.connect_clicked(move |_| {
            vault::lock();
            notify_state_changed();
            refresh(&w, &l, &s, &bu);
        });
        btns.append(&b);
    }

    // ---- 「关闭」----
    {
        let b = gtk::Button::with_label("关闭");
        // ⚠️ 抓**弱引用**：抓强引用就是 窗口 → 按钮 → 闭包 → 窗口 的环，
        // 窗口永远回收不掉。
        let w = win.downgrade();
        b.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.close();
            }
        });
        btns.append(&b);
    }

    let _ = (win_weak, list_weak, state_weak);
    // ⚠️ 尺寸由 `windows::open_or_focus` 统一记（关窗口时存进设置）
    win
    });
}

/* ---------------- 状态标志（界面上一眼看得见凭据库的状态） ---------------- */

/// 一条"状态变了"的回调。
struct StateHook {
    /// 控件还活着吗？死了就把这条摘掉——否则每开一次连接管理器就往
    /// 列表里塞一条，列表越积越长（虽然每条都很小）。
    alive: Rc<Cell<bool>>,
    f: Rc<dyn Fn(vault::State)>,
}

thread_local! {
    /// 凭据库状态变化时的回调（界面上的标志靠它刷新）。
    static STATE_HOOKS: RefCell<Vec<StateHook>> = const { RefCell::new(Vec::new()) };
}

/// 注册"凭据库状态变了"的回调。返回的 `alive` 标志置 `false` 即注销。
///
/// ⚠️ 回调**必须**在控件销毁时把 `alive` 置否（见 `badge_button`），
/// 否则死掉的回调会一直留在列表里。
pub fn add_state_hook(f: impl Fn(vault::State) + 'static) -> Rc<Cell<bool>> {
    let alive = Rc::new(Cell::new(true));
    STATE_HOOKS.with(|h| {
        h.borrow_mut().push(StateHook {
            alive: Rc::clone(&alive),
            f: Rc::new(f),
        })
    });
    alive
}

/// 通知界面：凭据库状态变了。
///
/// ⚠️ 先把回调**取出来**再调（不在借用里调）：回调里可能又碰凭据库
/// （比如刷新标志时又要读状态），在借用里调就是二次借用 → panic。
pub fn notify_state_changed() {
    let st = vault::state();
    // 借用在这里就结束（`with` 返回值出来之后），后面调回调不再持有借用
    let hooks: Vec<Rc<dyn Fn(vault::State)>> = STATE_HOOKS.with(|h| {
        let mut v = h.borrow_mut();
        v.retain(|x| x.alive.get());
        v.iter().map(|x| Rc::clone(&x.f)).collect()
    });
    for h in hooks {
        h(st);
    }
}

/// 状态的短文案（菜单项、标志都用它）。
pub fn state_short(st: vault::State) -> &'static str {
    match st {
        vault::State::Off => "未启用",
        vault::State::Unlocked => "已启用",
        vault::State::Locked => "已锁定",
        vault::State::Plain => "未加密",
    }
}

/* ---------------- 「记住密码」的共用构件（阶段 79） ---------------- */

/// 凭据库状态对「记住密码」的影响（纯函数，可单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RememberPolicy {
    /// 「记住密码」能不能勾。
    pub can_remember: bool,
    /// 「记住密码」这一项**要不要显示**。
    ///
    /// ⚠️ **凭据库存不住密码 ⇒ 不显示**（用户 2026-11-02，两次强调：
    /// 「只有在凭证库开启的时候才可以显示保存密码的选项」）。
    ///
    /// 两种存不住的情况：
    /// - **没启用**（`Off`）：压根没有库；
    /// - **锁着**（`Locked`）：库在，但没解锁 ⇒ 密码写不进去。
    ///
    /// ⛔ 之前只挡住了 `Off`，**漏了 `Locked`** —— 而"忘了主密码"的用户
    /// 恰恰落在 `Locked` 里（用户 2026-10-31 问过怎么重置凭据库），
    /// 于是他看到的还是一个勾了也没用的「记住密码」。
    ///
    /// 锁着时**状态说明照旧留着**（还给一个「解锁凭据库…」入口）⇒ 用户
    /// 就地解锁后，这个勾选框**立刻冒出来**（`remember_row` 的状态钩子）。
    pub show_checkbox: bool,
    /// 状态说明（人话）。
    pub text: &'static str,
    /// 要不要给一个「解锁」入口。
    pub offer_unlock: bool,
}

/// 凭据库状态 ⇒「记住密码」能不能用、怎么跟用户说。
///
/// 阶段 79：连接表单、DB 密码提示、凭据库管理器**三处**以前各写各的
/// 判定与文案 ⇒ 同一件事三种说法，还三处都得各自更新。
/// 现在只有这一份，控件与文案都从这儿出。
pub fn remember_policy(st: vault::State) -> RememberPolicy {
    match st {
        vault::State::Unlocked => RememberPolicy {
            can_remember: true,
            show_checkbox: true,
            text: "凭据库已启用并已解锁：勾上「记住密码」= 密码加密存进本机凭据库。",
            offer_unlock: false,
        },
        vault::State::Plain => RememberPolicy {
            can_remember: true,
            show_checkbox: true,
            text: "凭据库已启用但没有主密码：密码明文存在本机文件里。设一个主密码就能加密。",
            offer_unlock: false,
        },
        vault::State::Locked => RememberPolicy {
            can_remember: false,
            // ⚠️ **锁着 ⇒ 也不显示**（用户 2026-11-02）：库锁着时密码
            //    **存不进去**，摆一个勾了也没用的选项纯属添乱。
            //    （之前只挡住了"没启用"，忘了主密码的用户全落在这一档。）
            //    状态说明 + 「解锁凭据库…」入口照旧 ⇒ 解锁后立刻冒出来。
            show_checkbox: false,
            text: "凭据库已启用但当前锁定：解锁后才能把密码加密存进本机。",
            offer_unlock: true,
        },
        vault::State::Off => RememberPolicy {
            can_remember: false,
            // ⚠️ **没启用就不显示**（用户 2026-11-02）——
            //    勾了也存不住（存的那头会拦下并说清楚）⇒ 干脆不摆。
            show_checkbox: false,
            text: "凭据库还没启用：这次的密码只用这一次，不保存。\
                   想加密保存，先去「工具 → 凭据库…」启用凭据库。",
            offer_unlock: false,
        },
    }
}

/// 「解锁凭据库…」按钮（阶段 79：**三处共用**的就地解锁入口）。
///
/// 连接表单 / DB 密码提示 / 凭据库管理器共用这一颗：
/// 用户在哪儿看到"要解锁"，就在那儿能解，不用关掉当前窗口去顶栏菜单里找。
pub fn unlock_button(parent: Option<&gtk::Window>) -> gtk::Button {
    let b = gtk::Button::with_label("解锁凭据库…");
    b.set_halign(gtk::Align::Start);
    b.set_tooltip_text(Some(
        "输入主密码解锁本机凭据库；解锁后就能勾「记住密码」，密码会加密保存。",
    ));
    let p = parent.cloned();
    b.connect_clicked(move |_| unlock_vault(p.as_ref()));
    b
}

/// 「记住密码」勾选框 + 凭据库状态说明（**三处共用**的构件）。
///
/// 返回 (勾选框, 状态区)：
/// - 凭据库**存不住密码**（没启用 / 锁着）⇒ 勾选框**整个不显示**，
///   状态区说清为什么；
/// - 凭据库**锁着** ⇒ 状态区额外给一个「解锁凭据库…」入口
///   （用户不用关掉表单去顶栏菜单里找）；解锁后勾选框**立刻冒出来**；
/// - 凭据库状态**变了**（比如刚在状态区里解锁了）⇒ 勾选框与说明自动跟着变。
pub fn remember_row(
    parent: Option<&gtk::Window>,
    default_active: bool,
) -> (gtk::CheckButton, gtk::Box) {
    let pol = remember_policy(vault::state());
    let cb = gtk::CheckButton::with_label("记住密码");
    cb.set_sensitive(pol.can_remember);
    cb.set_active(default_active && pol.can_remember);
    // ⚠️ **凭据库存不住密码 ⇒ 整个不显示**（用户 2026-11-02）：
    //    勾了也存不住 ⇒ 摆一个"点了没反应"的选项没用还添乱。
    cb.set_visible(pol.show_checkbox);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let lbl = gtk::Label::new(Some(pol.text));
    lbl.add_css_class("dim-label");
    lbl.add_css_class("pp-hint");
    lbl.set_xalign(0.0);
    lbl.set_wrap(true);
    box_.append(&lbl);

    if pol.offer_unlock {
        box_.append(&unlock_button(parent));
    }

    // 状态变了就跟着变（用户可能在状态区里就地解锁了）
    {
        let cb2 = cb.clone();
        let lbl2 = lbl.clone();
        let alive = add_state_hook(move |st| {
            let p = remember_policy(st);
            cb2.set_sensitive(p.can_remember);
            // 状态变了 ⇒ 显隐也跟着变（刚在别的窗口启用了凭据库 ⇒ 冒出来）
            cb2.set_visible(p.show_checkbox);
            if !p.can_remember {
                cb2.set_active(false);
            }
            lbl2.set_text(p.text);
        });
        // 控件销毁时注销这条回调（不然死回调会一直留在列表里）
        cb.connect_destroy(move |_| alive.set(false));
    }
    (cb, box_)
}

/// 顶栏那个**常驻的凭据库状态标志**。
///
/// **为什么要有**：凭据库是"开了才生效"的开关，但**开了没有、密码存到哪
/// 去了，全靠用户自己记得**——忘了自己没开凭据库，就会以为"记住密码"把
/// 密码加密存起来了，其实它明文躺在 `connections.ini` 里。这个标志常驻在
/// 顶栏，状态一眼可见，不用点进去查。
pub fn badge_button() -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("pp-chip");
    // ⚠️ **专用类**（阶段 92）：屏幕上有几处凭据库标志，用这个类数得清
    // （顶栏那个已删，现在只剩侧栏标题栏这一处）。
    b.add_css_class("pp-vault-badge");
    // ⚠️ **不要**加 `flat`：Adwaita 的 `button.flat` 是 `background-color:
    // transparent`，优先级比 `.pp-chip` 高——加了 chip 的底色就没了，
    // 标志变成"一串白字"，状态一眼看不出来（那跟没标志一样）。

    let img = gtk::Image::new();
    let lbl = gtk::Label::new(None);
    // ⚠️ 阶段 93：徽标在侧栏**标题栏**里，标题栏窄的时候（用户存的是
    // 203px）它太长 ⇒ 整条侧栏溢出、左边被裁掉。文字可省略 ⇒ 徽标
    // 不再撑住最小宽度（窄的时候先省略「凭据库 未加密」这类长文案）。
    lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // ⚠️ 再给一个**最小宽度**（`width_chars`，不是 `max_width_chars`）：
    // 光有 ellipsize 的话，窄的时候会被压成「…」——连状态都看不出来是
    // 哪个状态。`width_chars(4)` = 最小留 4 列（两个汉字「凭据」），
    // 宽的时候照样显示完整文案（`max_width_chars` 才是**永久**截短，
    // 侧栏够宽也只剩「凭据库 已…」⇒ 不能用）。
    lbl.set_width_chars(4);
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    box_.append(&img);
    box_.append(&lbl);
    b.set_child(Some(&box_));

    // 按状态刷图标 / 文字 / 配色 / 提示
    fn apply(b: &gtk::Button, img: &gtk::Image, lbl: &gtk::Label, st: vault::State) {
        let (icon, text, tip, css) = match st {
            vault::State::Off => (
                "dialog-password-symbolic",
                "未启用",
                "凭据库还没启用：这次的密码只用这一次，不保存。\n\
                 点这里可以设置主密码（启用后密码加密保存）。",
                None,
            ),
            vault::State::Unlocked => (
                "channel-secure-symbolic",
                "已启用",
                "凭据库已启用并已解锁：连接密码加密存在本机。\n点这里查看/管理库里的凭据。",
                Some("pp-chip-ok"),
            ),
            vault::State::Locked => (
                "dialog-password-symbolic",
                "已锁定",
                "凭据库已启用（有主密码），当前处于锁定状态：要用里面的密码得先解锁。\n点这里解锁。",
                Some("pp-chip-warn"),
            ),
            vault::State::Plain => (
                "dialog-warning-symbolic",
                "未加密",
                "⚠ 凭据库已启用，但没有主密码：密码以明文存在本机文件里。\n点这里设一个主密码就能加密。",
                Some("pp-chip-err"),
            ),
        };
        img.set_icon_name(Some(icon));
        lbl.set_text(&format!("凭据库 {text}"));
        b.set_tooltip_text(Some(tip));
        // ⚠️ **基础类 `pp-chip` 始终保留**（它带底色、圆角、内边距），
        //    只换"状态类"。把基础类也摘掉的话，标志就变成一串没底色的
        //    白字——那跟没有标志一样。
        for c in ["pp-chip-ok", "pp-chip-warn", "pp-chip-err"] {
            b.remove_css_class(c);
        }
        b.add_css_class("pp-chip");
        if let Some(c) = css {
            b.add_css_class(c);
        }
    }

    {
        // ⚠️ 抓**弱引用**：钩子是进程级的，窗口关了钩子还在——抓强引用
        // 就等于把控件钉死（每开一次窗口漏一份）。
        let wb = b.downgrade();
        let wimg = img.downgrade();
        let wlbl = lbl.downgrade();
        let alive = add_state_hook(move |st| {
            let (Some(b), Some(img), Some(lbl)) = (wb.upgrade(), wimg.upgrade(), wlbl.upgrade())
            else {
                return;
            };
            apply(&b, &img, &lbl, st);
        });
        // 控件销毁时注销这条回调（不然死回调会一直留在列表里）
        b.connect_destroy(move |_| alive.set(false));
    }
    apply(&b, &img, &lbl, vault::state());
    b
}

fn collect_passwords(entries: &[ConnEntry]) -> Vec<(String, Zeroizing<String>)> {
    entries
        .iter()
        .filter_map(|e| {
            let p = e.password.as_ref()?;
            if p.is_empty() {
                return None;
            }
            Some((e.name.clone(), Zeroizing::new(p.to_string())))
        })
        .collect()
}

fn status(entries: &[ConnEntry]) -> String {
    let n = vault::names().len();
    if n == 0 {
        return "库内暂无凭据。".into();
    }
    let with_pw = entries.iter().filter(|e| e.password.is_some()).count();
    format!("库内 {n} 条凭据；当前会话可见 {with_pw} 条连接的密码。")
}

/// 结果提示（成功）：标题与正文分开，不走"错误弹窗"的形态。
fn info(parent: Option<&gtk::Window>, title: &str, body: &str) {
    crate::panel::dialogs::info(parent, title, body);
}

fn alert(parent: Option<&gtk::Window>, title: &str, body: &str) {
    crate::panel::dialogs::show_error(parent, title, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 凭据库没启用或锁着时记住密码不可勾() {
        assert!(!remember_policy(vault::State::Off).can_remember);
        assert!(!remember_policy(vault::State::Locked).can_remember);
        // 锁着 ⇒ 得给「解锁」入口
        assert!(remember_policy(vault::State::Locked).offer_unlock);
        // 没启用 ⇒ 解锁也没用（库还不存在）
        assert!(!remember_policy(vault::State::Off).offer_unlock);
    }

    /// **凭据库存不住密码 ⇒ 整个「记住密码」不显示**（用户 2026-11-02，
    /// 两次强调：「只有在凭证库开启的时候才可以显示保存密码的选项」）。
    ///
    /// ⚠️ **锁着也算"存不住"** —— 忘了主密码的用户全落在这一档（用户
    /// 2026-10-31 问过怎么重置凭据库）。之前只挡住了"没启用"，于是他
    /// 看到的还是一个勾了也存不住的「记住密码」。
    #[test]
    fn 凭据库存不住密码时不显示记住密码() {
        assert!(!remember_policy(vault::State::Off).show_checkbox);
        assert!(!remember_policy(vault::State::Locked).show_checkbox);
        assert!(remember_policy(vault::State::Plain).show_checkbox);
        assert!(remember_policy(vault::State::Unlocked).show_checkbox);
        // 没显示的时候，说明里**不该**再提「勾上「记住密码」」
        //（根本没有那个勾选框，提了用户会找）
        for st in [vault::State::Off, vault::State::Locked] {
            let p = remember_policy(st);
            assert!(!p.text.contains("勾上「记住密码」"), "{}", p.text);
            assert!(!p.text.contains("勾「记住密码」"), "{}", p.text);
        }
    }

    /// 锁着时**状态说明和「解锁」入口照旧留着** —— 勾选框不显示，
    /// 但用户得能在原地解锁（解锁后勾选框立刻冒出来）。
    #[test]
    fn 锁着时还给解锁入口() {
        let p = remember_policy(vault::State::Locked);
        assert!(p.offer_unlock, "锁着时得给「解锁凭据库…」入口");
        assert!(!p.can_remember);
        assert!(!p.show_checkbox);
        assert!(p.text.contains("锁定"), "{}", p.text);
    }

    #[test]
    fn 凭据库能用时记住密码可勾() {
        assert!(remember_policy(vault::State::Unlocked).can_remember);
        // 没主密码的明文库：能记（只是没加密），要说清
        assert!(remember_policy(vault::State::Plain).can_remember);
        assert!(remember_policy(vault::State::Plain).text.contains("明文"));
        assert!(remember_policy(vault::State::Unlocked).text.contains("加密"));
    }

    #[test]
    fn 三处文案只有一份() {
        // 同一状态 ⇒ 同一个判定（连接表单 / DB 密码提示 / 凭据库管理器共用）
        for st in [
            vault::State::Off,
            vault::State::Locked,
            vault::State::Plain,
            vault::State::Unlocked,
        ] {
            let p = remember_policy(st);
            assert_eq!(p, remember_policy(st));
            assert!(!p.text.is_empty(), "状态说明不能是空的");
        }
    }
}
