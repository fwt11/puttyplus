//! 跨服务器传输 UI：选目标、选方式，导出目标 SFTP 会话后交给源 worker。

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use super::queue::QueueItem;
use super::relay::{RelayMode, RelaySpec};
use super::worker::{WorkerCmd, XferShared, XferKind};
use super::{queue, PanelState};
use crate::util::path_basename;

pub struct RemoteTarget {
    pub label: String,
    pub st: Rc<PanelState>,
    pub cwd: String,
}

/// 远程文件列表右键菜单入口。
pub fn send_to_server(src: &Rc<PanelState>) {
    if !src.conn_ok.get() {
        super::dialogs::show_error(
            src.topwin().as_ref(),
            "未连接",
            "源服务器的 SFTP 尚未连接。",
        );
        return;
    }
    let paths: Vec<String> = src
        .pane(true)
        .selected(&src.pane_cwd(true))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    if paths.is_empty() {
        super::dialogs::info(src.topwin().as_ref(), "发送到其它服务器", "先选中要发送的文件或目录。");
        return;
    }
    let targets = crate::tabs::remote_targets(src);
    if targets.is_empty() {
        super::dialogs::info(
            src.topwin().as_ref(),
            "没有可用的目标",
            "需要另一个已连接 SFTP 的标签页。先在侧边栏打开目标服务器，再回来发送。",
        );
        return;
    }

    let targets = Rc::new(targets);
    let dlg = gtk::Dialog::builder()
        .title("发送到其它服务器")
        .transient_for(&src.topwin().unwrap())
        .modal(true)
        .build();
    dlg.add_button("取消", gtk::ResponseType::Cancel);
    dlg.add_button("开始传输", gtk::ResponseType::Ok);
    let area = dlg.content_area();
    // 与「新建连接 / 会话选项」共用同一套表单骨架：标签在上、控件在下，
    // 分区标题弱化，短字段并排。以前是裸 label + 8px 间距，跟其它对话框不像一套。
    let root = super::dialogs::form_box();

    let names: Vec<String> = targets.iter().map(|t| t.label.clone()).collect();
    let target_dd = gtk::DropDown::from_strings(&names.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    root.append(&super::dialogs::form_section("目标"));
    root.append(&super::dialogs::form_row("目标服务器", &target_dd));

    let dst_entry = gtk::Entry::new();
    dst_entry.set_text(&targets[0].cwd);
    dst_entry.set_hexpand(true);
    root.append(&super::dialogs::form_row("目标目录", &dst_entry));
    {
        let targets2 = Rc::clone(&targets);
        let entry = dst_entry.clone();
        target_dd.connect_selected_notify(move |dd| {
            if let Some(t) = targets2.get(dd.selected() as usize) {
                entry.set_text(&t.cwd);
            }
        });
    }

    root.append(&super::dialogs::form_section("传输"));
    let mode_dd = gtk::DropDown::from_strings(&[
        "自动（优先直连，失败转本机中转）",
        "仅直连（失败即报错）",
        "仅经本机中转",
    ]);
    let conflict_dd =
        gtk::DropDown::from_strings(&["目标已存在时跳过", "目标已存在时覆盖"]);
    // 方式/同名文件的下拉文字都长，不做并排（半宽会省略号截断）
    root.append(&super::dialogs::form_row("方式", &mode_dd));
    root.append(&super::dialogs::form_row("同名文件", &conflict_dd));

    let agent_cb = gtk::CheckButton::with_label(
        "允许把本地 ssh-agent 临时转发给源服务器（直连认证用）",
    );
    agent_cb.set_active(true);
    root.append(&agent_cb);

    let hint = gtk::Label::new(Some(
        "直连：在源服务器上执行 scp，把文件直接送到目标（需要源能登录目标）。\n\
         本机中转：读源服务器、写目标服务器，不依赖服务器上的 scp/ssh。",
    ));
    hint.set_halign(gtk::Align::Start);
    hint.set_wrap(true);
    hint.add_css_class("pp-hint");
    root.append(&hint);
    area.append(&root);

    {
        let src2 = Rc::clone(src);
        let targets2 = Rc::clone(&targets);
        let paths2 = paths.clone();
        dlg.connect_response(move |dlg, resp| {
            if resp != gtk::ResponseType::Ok {
                dlg.close();
                return;
            }
            let Some(t) = targets2.get(target_dd.selected() as usize) else {
                dlg.close();
                return;
            };
            let dst = dst_entry.text().to_string();
            let mode = match mode_dd.selected() {
                1 => RelayMode::DirectOnly,
                2 => RelayMode::RelayOnly,
                _ => RelayMode::Auto,
            };
            let agent = agent_cb.is_active();
            let overwrite = conflict_dd.selected() == 1;
            dlg.close();
            start(&src2, &t.st, paths2.clone(), dst, mode, agent, overwrite);
        });
    }
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
}

fn start(
    src: &Rc<PanelState>,
    dst: &Rc<PanelState>,
    paths: Vec<String>,
    dst_dir: String,
    mode: RelayMode,
    allow_agent: bool,
    overwrite: bool,
) {
    if paths.is_empty() {
        return;
    }
    if dst_dir.trim().is_empty() || !dst_dir.starts_with('/') {
        super::dialogs::show_error(
            src.topwin().as_ref(),
            "目标目录无效",
            "目标目录必须是以 / 开头的绝对路径。",
        );
        return;
    }
    let Some(dst_tx) = dst.tx.borrow().as_ref().cloned() else {
        super::dialogs::show_error(src.topwin().as_ref(), "未连接", "目标服务器的连接已断开。");
        return;
    };

    let id = src.next_id.get();
    src.next_id.set(id + 1);
    let shared = XferShared::new();
    let (tx, rx) = async_channel::bounded::<Result<russh_sftp::client::SftpSession, String>>(1);
    if dst_tx.send(WorkerCmd::ExportSftp { reply: tx }).is_err() {
        super::dialogs::show_error(src.topwin().as_ref(), "发送失败", "目标 worker 已退出。");
        return;
    }

    let src2 = Rc::clone(src);
    let dst2 = Rc::clone(dst);
    let first = path_basename(&paths[0]).to_string();
    let n = paths.len();
    glib::MainContext::default().spawn_local(async move {
        let sftp = match rx.recv().await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                super::dialogs::show_error(
                    src2.topwin().as_ref(),
                    "目标 SFTP 不可用",
                    &e,
                );
                return;
            }
            Err(_) => {
                super::dialogs::show_error(
                    src2.topwin().as_ref(),
                    "目标 SFTP 不可用",
                    "导出目标连接失败。",
                );
                return;
            }
        };
        let cancel = shared
            .lock()
            .map(|g| std::sync::Arc::clone(&g.cancel_flag))
            .unwrap_or_else(|_| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let spec = RelaySpec {
            id,
            paths: paths.clone(),
            dst_dir: dst_dir.clone(),
            dst_sftp: sftp,
            dst_user: dst2.opts.user.clone(),
            dst_host: dst2.opts.host.clone(),
            dst_port: dst2.opts.port,
            dst_label: dst2
                .opts
                .title
                .clone()
                .unwrap_or_else(|| format!("{}@{}", dst2.opts.user, dst2.opts.host)),
            mode,
            allow_agent,
            overwrite,
            shared: shared.clone(),
            cancel,
            tag: format!(
                "{}-{}-{}",
                std::process::id(),
                id,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ),
        };
        {
            let mut items = src2.items.borrow_mut();
            items.push(QueueItem {
                id,
                src: if n > 1 {
                    format!("{first} 等 {n} 项")
                } else {
                    first
                },
                dst_dir,
                upload: false,
                kind: XferKind::Relay,
                shared: shared.clone(),
            });
        }
        // 只在用户点「传输队列」时展示；发送本身不抢焦点
        queue::ensure(&src2);
        let sent = src2
            .tx
            .borrow()
            .as_ref()
            .map(|tx| tx.send(WorkerCmd::Relay { spec }).is_ok())
            .unwrap_or(false);
        if !sent {
            super::dialogs::show_error(
                src2.topwin().as_ref(),
                "发送失败",
                "源服务器 worker 已退出。",
            );
        }
    });
}
