//! 会话选项对话框（1.5）：按会话覆盖保活间隔、终端字号、响铃。
//!
//! 保存后分两路生效：
//! - 字号 / 响铃：**立即**作用于当前终端（term::set_appearance）
//! - 保活间隔：SFTP 连接层的参数，改完要重连才生效——按钮上写明了，
//!   面板工具栏本来就有「重连」，不在这里重复造一个
//!
//! 覆盖存 settings.ini 的 [session_overrides]，按会话键（会话名/标题，
//! 没有则 host:port）索引；同名会话共用一份，与 connections.ini 的
//! name 对齐。改完重开该会话仍然生效。

use std::rc::Rc;

use gtk::prelude::*;

use crate::args::AuthOrder;
use crate::settings::{self, SessionOverride};

use super::PanelState;

/// 打开会话选项对话框（工具栏按钮）。
pub fn open(st: &Rc<PanelState>) {
    let key = st.opts.session_key();
    let parent = st.topwin();

    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        &format!("会话选项 — {key}"),
        parent.as_ref(),
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("保存", gtk::ResponseType::Ok),
        ],
    );
    dlg.set_default_size(400, -1);

    let root = crate::panel::dialogs::form_box();

    // 保活：0 = 关闭。默认值取当前会话实际在用的值（可能被上次保存改过）
    let w_keep = gtk::Entry::new();
    w_keep.set_text(&st.keepalive_sec.get().to_string());
    w_keep.set_tooltip_text(Some(
        "无服务器数据多少秒后发一次 SSH 保活，连续 3 次无应答即断开。\n\
         0 = 关闭保活（完全依赖 TCP 超时）。\n\
         保存后需点「重新建立 SFTP 连接」才生效。",
    ));

    // 字号：空 = 沿用连接的默认值（连接编辑器 →「会话」页）
    //
    // ⚠️ 预填的是**生效值**（`st.opts` 已经把会话覆盖与连接默认值合并过），
    // 不是"覆盖里存了什么"——否则连接里配了 12、覆盖里没记，
    // 对话框却显示空白，用户以为没生效。
    let w_font = gtk::Entry::new();
    w_font.set_text(
        &st.opts
            .font_size
            .map(|v| format!("{v:.1}"))
            .unwrap_or_default(),
    );
    w_font.set_placeholder_text(Some("留空 = 沿用连接的默认值"));
    w_font.set_tooltip_text(Some("终端字号，立即生效"));

    // 响铃：默认开
    let w_bell = gtk::CheckButton::with_label("终端响铃");
    w_bell.set_active(st.opts.bell.unwrap_or(true));

    // 终端类型（TERM）：预置几项 + 连接里填的值（不在预置里就临时补一项）
    let w_term = gtk::ComboBoxText::new();
    const KNOWN_TERM: [&str; 5] = ["xterm-256color", "xterm", "screen-256color", "vt100", "linux"];
    for t in KNOWN_TERM {
        w_term.append(Some(t), t);
    }
    let term = st
        .opts
        .term_type
        .clone()
        .unwrap_or_else(|| "xterm-256color".to_string());
    if !KNOWN_TERM.contains(&term.as_str()) {
        w_term.append(Some(term.as_str()), term.as_str());
    }
    w_term.set_active_id(Some(term.as_str()));
    w_term.set_tooltip_text(Some(
        "决定远程 pty 的 TERM。留空 = xterm-256color。\n改完需重新连接才生效（子进程环境在启动时定下）。",
    ));

    // 认证优先级（2.2）
    let w_auth = gtk::ComboBoxText::new();
    w_auth.append(Some("password-first"), "密码优先（默认）");
    w_auth.append(Some("publickey-first"), "密钥优先");
    w_auth.set_active_id(Some(st.opts.auth_order.as_str()));
    w_auth.set_tooltip_text(Some(
        "进程内 SFTP 链路的认证顺序。\n密码优先：少一次无谓的公钥尝试，也避免把本地身份出示给堡垒机网关。\n改完需重新连接才生效。",
    ));

    root.append(&crate::panel::dialogs::form_section("连接"));
    root.append(&crate::panel::dialogs::form_row("保活（秒）", &w_keep));
    root.append(&crate::panel::dialogs::form_row("终端类型（TERM）", &w_term));
    root.append(&crate::panel::dialogs::form_row("认证优先级", &w_auth));

    root.append(&crate::panel::dialogs::form_section("终端"));
    root.append(&crate::panel::dialogs::form_row("终端字号", &w_font));
    root.append(&crate::panel::dialogs::form_row("响铃", &w_bell));

    let tip = gtk::Label::new(Some(
        "这是当前会话的临时覆盖，优先于连接里的默认设置（连接编辑器 →「会话」页）。\n\
         字号与响铃保存后立即生效；保活、终端类型、认证优先级需重新建立连接才生效\
         （子进程环境与认证链都在启动时定下）。配置按会话名保存，同名会话共用一份。",
    ));
    tip.add_css_class("dim-label");
    tip.add_css_class("pp-hint");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    root.append(&tip);

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
    let st2 = Rc::clone(st);
    dlg.connect_response(move |dlg, resp| {
        if resp != gtk::ResponseType::Ok {
            dlg.destroy();
            return;
        }
        let keep = crate::util::atoi_like(&w_keep.text().trim());
        let keep = if keep < 0 { 0 } else { keep.min(3600) } as u32;
        let font_txt = w_font.text().trim().to_string();
        let font = if font_txt.is_empty() {
            None
        } else {
            // 非法输入 / 超出 6..=48 一律当作"没填"，回退内置默认，
            // 不让一次手滑把字号写坏
            font_txt
                .parse::<f64>()
                .ok()
                .filter(|v| (6.0..=48.0).contains(v))
                .map(|v| (v * 10.0).round() / 10.0)
        };
        let ov = SessionOverride {
            keepalive_sec: Some(keep),
            font_size: font,
            bell: Some(w_bell.is_active()),
            term_type: w_term
                .active_id()
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty()),
            auth_order: w_auth
                .active_id()
                .as_deref()
                .and_then(AuthOrder::parse)
                .or(Some(AuthOrder::default())),
        };
        settings::set_session_override(&st2.opts.session_key(), &ov);
        // 立即生效的部分：终端字号/响铃；其余三个要重连才生效
        st2.keepalive_sec.set(keep);
        st2.auth_order.set(ov.auth_order.unwrap_or_default());
        if let Some(term) = st2.term.borrow().as_ref() {
            // 字号留空 = 沿用连接的默认值 ⇒ 用当前生效值兜底（阶段 69 之二）
            crate::term::set_appearance(term, font.or(st2.opts.font_size), ov.bell);
        }
        dlg.destroy();
    });
}
