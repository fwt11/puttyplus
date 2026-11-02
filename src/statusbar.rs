//! 顶栏全局状态区（阶段 77c-2 / 审计 P1-9）。
//!
//! 审计 P1-9 原话：「标签点 = SFTP；终端状态在终端 banner；DB 状态在 DB
//! 状态行；传输在队列窗口 / 通知 / 汇总对话框……**没有一个全局状态**」。
//! 多窗口时每个窗口各自一套，关掉窗口后队列与汇总可能看不见。
//!
//! 现在顶栏常驻一个**状态区**：一个 chip 显示**当前窗口当前会话**的聚合
//! 状态，tooltip 把四个维度（SFTP / 终端 / 数据库 / 传输）逐条说清。
//!
//! ⚠️ **状态数据不另造一份**：前三个维度直接取 [`crate::active`] 的那份
//! 快照（阶段 57 / 65 已经聚合好），传输维度从会话的队列项现算 ——
//! 宁可多算一次，也不要出现"第二个真相来源"。
//!
//! ⚠️ **chip 上的文字只说连接维度**（SFTP / 终端），传输状态只进 tooltip：
//! 一次传输失败就把 chip 涂红，用户会以为**连接断了** —— 两件事不能混。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::active::TermState;
use crate::theme::State;
use crate::i18n::tr;
use crate::trf;

/// 传输维度（第四维）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum XferState {
    /// 没有传输
    #[default]
    Idle,
    /// 正在传（含排队等待）
    Running,
    /// 有失败
    Failed,
}

impl XferState {
    pub fn label(self) -> &'static str {
        match self {
            XferState::Idle => tr("空闲"),
            XferState::Running => tr("传输中"),
            XferState::Failed => tr("有失败"),
        }
    }
}

/// 一个会话的**四个维度**。
#[derive(Clone, Debug, Default)]
pub struct SessionStatus {
    /// 文件面板（SFTP）通道状态。
    pub sftp: Option<State>,
    /// 终端存活状态。
    pub term: TermState,
    /// 这个会话连着的数据库连接名（没连 ⇒ `None`）。
    pub db_name: Option<String>,
    /// 数据库连上了没有。
    pub db_connected: bool,
    /// 传输队列的状态。
    pub xfer: XferState,
}

/// 四个维度逐条说清（状态区面板 / tooltip 用）。
///
/// ⚠️ **顺序固定**（SFTP / 终端 / 数据库 / 传输）：用户扫一眼就能定位，
/// 顺序一变就变成"自己找"。
pub fn status_lines(st: &SessionStatus) -> Vec<(&'static str, String)> {
    let sftp = match st.sftp {
        Some(State::Ok) => tr("已连接").to_string(),
        Some(State::Warn) => tr("连接中…").to_string(),
        Some(State::Err) => tr("未连接").to_string(),
        None => tr("未知").to_string(),
    };
    let db = match (&st.db_name, st.db_connected) {
        (Some(n), true) if !n.is_empty() => trf!("已连接（{n}）", n = n),
        (Some(n), false) if !n.is_empty() => trf!("未连接（{n}）", n = n),
        _ => tr("未连接").to_string(),
    };
    vec![
        (tr("文件面板"), sftp),
        (tr("终端"), st.term.label().to_string()),
        (tr("数据库"), db),
        (tr("传输"), st.xfer.label().to_string()),
    ]
}

/// chip 上的文字：只说**最要紧的那一条**。
///
/// ⚠️ 传输状态**不进这里**：一次传输失败就把 chip 说成"传输有失败"，
/// 用户会以为连接出了问题 —— 两件事混在一起就都说不清了。
pub fn chip_text(st: &SessionStatus) -> String {
    match st.sftp {
        Some(State::Err) => tr("文件面板未连接").to_string(),
        Some(State::Warn) => tr("正在连接").to_string(),
        Some(State::Ok) => {
            if st.term == TermState::Dead {
                tr("终端已结束").to_string()
            } else {
                tr("已连接").to_string()
            }
        }
        None => {
            if st.term == TermState::Dead {
                tr("终端已结束").to_string()
            } else {
                tr("未连接").to_string()
            }
        }
    }
}

/// chip 的状态色：跟树上的状态点**同一份判定**（[`crate::active::aggregate`]）。
pub fn chip_state(st: &SessionStatus) -> Option<State> {
    crate::active::aggregate(st.sftp, st.term, st.db_connected)
}

/// tooltip：四维一行一条。
pub fn status_tooltip(st: &SessionStatus) -> String {
    status_lines(st)
        .iter()
        .map(|(k, v)| trf!("{k}：{v}", k = k, v = v))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 这个会话的传输状态（第四维，现算）。
pub fn xfer_state(st: &crate::panel::PanelState) -> XferState {
    let mut running = false;
    let mut failed = false;
    for it in st.items.borrow().iter() {
        let Ok(g) = it.shared.lock() else { continue };
        match g.state {
            crate::panel::worker::XferState::Run | crate::panel::worker::XferState::Wait => {
                running = true;
            }
            crate::panel::worker::XferState::Err => failed = true,
            _ => {}
        }
    }
    if failed {
        XferState::Failed
    } else if running {
        XferState::Running
    } else {
        XferState::Idle
    }
}

thread_local! {
    /// **测试钩子**：强制状态区显示某个状态。
    ///
    /// 真实状态要连上服务器才有 ⇒ 冒烟没法造。覆盖值存在时用它，
    /// 状态区照常渲染（构件层照样能测）。
    static OVERRIDE: RefCell<Option<SessionStatus>> = const { RefCell::new(None) };
}

/// 测试钩子：强制状态区显示某个状态（`None` = 恢复真实状态）。
pub fn set_override(v: Option<SessionStatus>) {
    OVERRIDE.with(|o| *o.borrow_mut() = v);
}

/// **当前窗口当前会话**的四维状态（没有会话 ⇒ `None`）。
pub fn current_status(tabs: &Rc<crate::tabs::Tabs>) -> Option<SessionStatus> {
    if let Some(v) = OVERRIDE.with(|o| o.borrow().clone()) {
        return Some(v);
    }
    let st = tabs.current_panel_state()?;
    // 会话键：live_sessions 是 (键, 会话) 的表 ⇒ 反查这一个会话的键
    let key = crate::tabs::live_sessions()
        .into_iter()
        .find(|(_, s)| Rc::ptr_eq(s, &st))
        .map(|(k, _)| k)?;
    let act = crate::active::snapshot();
    let db_set = act.db_connected.get(&key);
    Some(SessionStatus {
        sftp: act.session_state(&key),
        term: act.terminal.get(&key).copied().unwrap_or_default(),
        db_name: db_set.and_then(|s| s.iter().next().cloned()),
        db_connected: db_set.map(|s| !s.is_empty()).unwrap_or(false),
        xfer: xfer_state(&st),
    })
}

/* ---------------- 构件 ---------------- */

thread_local! {
    /// 所有窗口的状态区 chip（弱引用）：状态一变就全部刷新一遍。
    ///
    /// ⚠️ 抓**弱引用**：窗口关了 chip 就没了，抓强引用等于把窗口钉死。
    static LIVE: RefCell<Vec<(glib::WeakRef<gtk::Button>, Rc<crate::tabs::Tabs>)>> =
        const { RefCell::new(Vec::new()) };
}

/// 建一个状态区 chip（顶栏用）。
///
/// ⚠️ **纯图标按钮不带文字**是顶栏的规矩，但状态区**要带文字**：它说的
/// 是"现在到底怎么了"，光靠颜色用户读不出来。
pub fn build(tabs: Rc<crate::tabs::Tabs>) -> gtk::Button {
    let b = gtk::Button::new();
    // ⚠️ **不要**加 `flat`：Adwaita 的 `button.flat` 是 `background-color:
    // transparent`，优先级比 `.pp-chip` 高 —— 加了 chip 的底色就没了，
    // 状态一眼看不出来（同 `vaultui::badge_button` 的教训）。
    b.add_css_class("pp-chip");

    let img = gtk::Image::new();
    let lbl = gtk::Label::new(None);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&img);
    row.append(&lbl);
    b.set_child(Some(&row));

    b.set_tooltip_text(Some(tr("当前会话的状态")));
    // 没有会话时先藏着
    b.set_visible(false);

    LIVE.with(|l| l.borrow_mut().push((b.downgrade(), tabs.clone())));
    refresh_one(&b, &tabs);
    b
}

/// 把一个 chip 刷成当前状态（内部用）。
fn refresh_one(b: &gtk::Button, tabs: &Rc<crate::tabs::Tabs>) {
    let Some(st) = current_status(tabs) else {
        b.set_visible(false);
        return;
    };
    b.set_visible(true);

    // 文字 + tooltip
    let Some(child) = b.child() else { return };
    let Some(row) = child.downcast_ref::<gtk::Box>() else { return };
    let mut cur = row.first_child();
    while let Some(c) = cur {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            l.set_text(&chip_text(&st));
        }
        if let Some(i) = c.downcast_ref::<gtk::Image>() {
            // ⚠️ **图标必须在当前主题里真的存在**：Adwaita **没有**
            // `emblem-ok-symbolic`（只有 Papirus 有）⇒ GTK 会静默回退成
            // 一个默认图标（看着像"文档"，完全不是"正常"的意思）。
            // 这几个在 Adwaita 的 symbolic/{status,actions} 里都有。
            i.set_icon_name(Some(match chip_state(&st) {
                Some(State::Ok) => "object-select-symbolic",
                Some(State::Warn) => "dialog-warning-symbolic",
                Some(State::Err) => "dialog-error-symbolic",
                None => "dialog-information-symbolic",
            }));
        }
        cur = c.next_sibling();
    }
    b.set_tooltip_text(Some(&status_tooltip(&st)));

    // 配色：只换状态类，**基础类 `pp-chip` 始终保留**
    for c in ["pp-chip-ok", "pp-chip-warn", "pp-chip-err"] {
        b.remove_css_class(c);
    }
    b.add_css_class("pp-chip");
    match chip_state(&st) {
        Some(State::Ok) => b.add_css_class("pp-chip-ok"),
        Some(State::Warn) => b.add_css_class("pp-chip-warn"),
        Some(State::Err) => b.add_css_class("pp-chip-err"),
        None => {}
    }
}

/// 刷新**所有窗口**的状态区。
///
/// 什么时候调：连接状态变了（`panel::refresh_active_marks`）、切了标签、
/// 传输队列状态变了。
pub fn refresh_all() {
    let pairs: Vec<(gtk::Button, Rc<crate::tabs::Tabs>)> = LIVE.with(|l| {
        l.borrow()
            .iter()
            .filter_map(|(w, t)| w.upgrade().map(|b| (b, t.clone())))
            .collect()
    });
    for (b, t) in pairs {
        refresh_one(&b, &t);
    }
}

/// 状态区里现在有几个 chip（测试钩子）。
pub fn live_count() -> usize {
    LIVE.with(|l| l.borrow().iter().filter(|(w, _)| w.upgrade().is_some()).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(sftp: Option<State>, term: TermState, db: Option<&str>, xfer: XferState) -> SessionStatus {
        SessionStatus {
            sftp,
            term,
            db_name: db.map(|s| s.to_string()),
            db_connected: db.is_some(),
            xfer,
        }
    }

    #[test]
    fn 四个维度都摆得出来() {
        let s = st(Some(State::Ok), TermState::Alive, Some("demo"), XferState::Idle);
        let lines = status_lines(&s);
        assert_eq!(lines.len(), 4, "状态区要摆四个维度：{lines:?}");
        let names: Vec<&str> = lines.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, vec!["文件面板", "终端", "数据库", "传输"]);
        assert!(lines[2].1.contains("demo"), "数据库维度要带上连的哪个库：{lines:?}");
    }

    #[test]
    fn chip_文字只说连接维度() {
        // 传输失败**不**进 chip 文字：否则用户以为连接断了
        let s = st(Some(State::Ok), TermState::Alive, None, XferState::Failed);
        assert_eq!(chip_text(&s), "已连接");
        // 但 tooltip 里要看得见
        assert!(status_tooltip(&s).contains("有失败"), "{:?}", status_tooltip(&s));
    }

    #[test]
    fn 终端死了要说出来() {
        let s = st(Some(State::Ok), TermState::Dead, None, XferState::Idle);
        assert_eq!(chip_text(&s), "终端已结束");
        // 聚合状态是黄点不是红点（连接还在，只是终端掉了）
        assert_eq!(chip_state(&s), Some(State::Warn));
    }

    #[test]
    fn 文件面板失败是最严重() {
        let s = st(Some(State::Err), TermState::Alive, None, XferState::Idle);
        assert_eq!(chip_text(&s), "文件面板未连接");
        assert_eq!(chip_state(&s), Some(State::Err));
    }
}
