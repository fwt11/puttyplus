//! 树里的「活跃」标记（阶段 57）。
//!
//! 用户诉求：会话树（侧栏）里，**正在连着 / 正在跑的**东西
//! 要看得出来——SSH 会话、数据库连接、端口转发规则，一个都不能少。
//!
//! **为什么单独一个模块**：两棵树要的是同一份快照。各写一份必然走样
//! （一边写「已连接」、另一边写「未连接」，用户没法信这棵树）。

use std::collections::{HashMap, HashSet};

use crate::theme::State;
use crate::i18n::tr;
use crate::trf;

/// 一个会话里**终端**的存活状态（阶段 65 / 审计 P0-4）。
///
/// **为什么要三态而不是布尔**：没上报 ≠ 已经结束。没起过的终端、
/// 还没连上的会话都算 `Unknown` ⇒ 聚合时**不参与**判定，
/// tooltip 里写「终端：未知」——**不谎报绿点**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TermState {
    /// 没上报（还没起来 / 没有终端）
    #[default]
    Unknown,
    /// 会话活着
    Alive,
    /// 起过、现在结束了
    Dead,
}

impl TermState {
    /// tooltip 里的说法。
    pub fn label(self) -> &'static str {
        match self {
            TermState::Unknown => tr("未知"),
            TermState::Alive => tr("运行中"),
            TermState::Dead => tr("已结束"),
        }
    }
}

/// 一次活跃状态快照（两棵树共用同一份）。
#[derive(Debug, Default, Clone)]
pub struct ActiveInfo {
    /// 会话键 → 会话状态（Ok=已连接，Warn=正在连接，Err=未连接/失败）
    pub sessions: HashMap<String, State>,
    /// 会话键 → **正在运行**的转发规则键（`L:9000:host:80` 这种内部形）
    pub fwd_running: HashMap<String, HashSet<String>>,
    /// 会话键 → **已连上库**的数据库连接名
    pub db_connected: HashMap<String, HashSet<String>>,
    /// 会话键 → 终端存活状态（阶段 65）
    pub terminal: HashMap<String, TermState>,
}

/// 三个维度里更严重的那个（Err > Warn > Ok）。
fn worse(a: State, b: State) -> State {
    fn rank(s: State) -> u8 {
        match s {
            State::Ok => 0,
            State::Warn => 1,
            State::Err => 2,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// 把三个维度**聚合成**一个状态（阶段 65）。
///
/// 取**最严重**的（Err > Warn > Ok）：
/// - **文件面板**（SFTP）：Ok=已连接，Warn=连接中，Err=未连接/失败；
/// - **终端**：Alive → 不加砝码；Dead → **Warn**（连接还在、只是终端掉了，
///   标红太吓人）；Unknown → **不参与**（没上报就不猜）；
/// - **数据库**：连上了 → 不加砝码；没连 → **不参与**（数据库是**可选**功能，
///   用户没开数据库工作台不是错误）。
///
/// ⚠️ 终端死了**不是** Err：那样两棵树里就**不画点**了（Err 不画点，
/// 见 [`dot`]），用户看到的是"没连"——和"终端掉了"混成一样，看不出问题。
pub fn aggregate(sftp: Option<State>, term: TermState, db_connected: bool) -> Option<State> {
    let mut out = sftp;
    if term == TermState::Dead {
        out = Some(match out {
            Some(State::Err) => State::Err,
            // 文件通道还在、只是终端掉了 ⇒ 黄点（"有问题，但还能用"）
            Some(s) => worse(s, State::Warn),
            // 连文件通道的状态都没有 ⇒ 这个会话整个不对劲
            None => State::Err,
        });
    }
    // 数据库只加"好"的砝码，不加坏的 ⇒ 这里什么都不用做（保持可读性显式写出）
    let _ = db_connected;
    out
}

/// 状态点的 tooltip：三句话说清**哪一部分**出的问题。
///
/// 例：「文件面板：已连接 / 终端：已结束 / 数据库：未连接」。
pub fn state_tooltip(sftp: Option<State>, term: TermState, db_name: Option<&str>) -> String {
    let s = match sftp {
        Some(State::Ok) => tr("已连接"),
        Some(State::Warn) => tr("连接中…"),
        Some(State::Err) => tr("未连接"),
        None => tr("未知"),
    };
    let db = match db_name {
        Some(n) if !n.is_empty() => trf!("已连接（{n}）", n = n),
        _ => tr("未连接").to_string(),
    };
    trf!("文件面板：{s} / 终端：{v0} / 数据库：{db}", s = s, db = db, v0 = term.label())
}

impl ActiveInfo {
    /// 这个会话的状态（`None` = 这个会话没开着）。
    pub fn session_state(&self, key: &str) -> Option<State> {
        self.sessions.get(key).copied()
    }

    /// 这个会话的**聚合**状态（SFTP + 终端 + 数据库）。
    ///
    /// 标签上的状态点与两棵树上的点都用它 ⇒ 一处算，处处对。
    pub fn session_state_agg(&self, key: &str) -> Option<State> {
        let term = self.terminal.get(key).copied().unwrap_or_default();
        let db = self
            .db_connected
            .get(key)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        aggregate(self.session_state(key), term, db)
    }

    /// 这个会话的状态点 tooltip。
    ///
    /// ⚠️ **什么都没发生时返回 `None`**：连接列表里多数条目是
    /// **从没打开过的存档连接** ⇒ 给它们挂一句
    /// 「文件面板：未知 / 终端：未知 / 数据库：未连接」纯属噪音
    /// ⇒ 那种情况让调用方沿用原来的短提示（只有地址）。
    pub fn session_tooltip(&self, key: &str) -> Option<String> {
        let sftp = self.session_state(key);
        let term = self.terminal.get(key).copied().unwrap_or_default();
        let db = self
            .db_connected
            .get(key)
            .and_then(|s| s.iter().next().cloned());
        // 一个维度都没状态 ⇒ 没什么可说的
        if sftp.is_none() && term == TermState::Unknown && db.is_none() {
            return None;
        }
        Some(state_tooltip(sftp, term, db.as_deref()))
    }

    /// 这条转发规则是不是正在跑。
    pub fn fwd_running(&self, key: &str, rule: &str) -> bool {
        self.fwd_running
            .get(key)
            .map(|s| s.contains(rule))
            .unwrap_or(false)
    }

    /// 这条数据库连接是不是连着。
    pub fn db_connected(&self, key: &str, name: &str) -> bool {
        self.db_connected
            .get(key)
            .map(|s| s.contains(name))
            .unwrap_or(false)
    }
}

/// 采集当前**所有窗口**的活跃状态。
///
/// ⚠️ 侧栏是**独立面板**，不属于某个窗口；多窗口同时开着是常态
/// （堡垒机场景），所以这里跨窗口收集，而不是只看某一个 Tabs。
pub fn snapshot() -> ActiveInfo {
    let mut info = ActiveInfo::default();
    for (key, st) in crate::tabs::live_sessions() {
        if let Some(s) = crate::panel::current_state(&st) {
            info.sessions.insert(key.clone(), s);
        }
        // 终端：活 / 死 / 还没上报（阶段 65）
        info.terminal.insert(key.clone(), crate::panel::term_state(&st));
        // 转发：规则**启用且真的在监听**才算「运行中」
        //（启用 ≠ 在跑：绑定失败时 err 非空，enabled 仍为 true）
        for v in st.fwd_views.borrow().iter() {
            if v.enabled && v.active && v.err.is_none() {
                info.fwd_running
                    .entry(key.clone())
                    .or_default()
                    .insert(v.key.clone());
            }
        }
        // 数据库：这个会话的工作台连着库才算
        //
        // ⚠️ 读的是 `st.db_active` 的**副本**，不是工作台自己的 `connected`：
        // 后者要借 `db_ui` 的 RefCell，而工作台的事件处理正借着它——
        // 借两次直接 panic（阶段 57 实测踩过）。
        if let Some(name) = st.db_active.borrow().clone() {
            info.db_connected.entry(key.clone()).or_default().insert(name);
        }
    }
    info
}

/// 状态点的**形状**（不只靠颜色区分状态，审计 P2-6）。
///
/// 颜色看不见的人（色弱、灰度打印、高对比截图）也得能分辨：
///   · 已连接 → 实心圆 ●
///   · 连接中 → 半实心圆 ◐（"还没完"的意思）
///   · 没状态 → 不画点（树里省地方）
pub fn dot_char(state: Option<State>) -> &'static str {
    match state {
        Some(State::Ok) => "●",
        Some(State::Warn) => "◐",
        _ => "",
    }
}

/// 行首的活跃状态点（两棵树共用）。
///
/// **为什么用圆点而不是文字**：树很窄，写「已连接」三个字会把连接名挤成
/// 「…」。圆点一眼就够：颜色（绿=已连、黄=连接中）＋**形状**（● / ◐）
/// ＋悬停提示，三重冗余，不靠颜色单独说话。
///
/// `None` = 没状态（没连 / 没开着）→ 不画点。
pub fn dot(state: Option<State>) -> String {
    let c = dot_char(state);
    if c.is_empty() {
        return String::new();
    }
    format!("<span foreground='{}'>{}</span> ",
        crate::theme::state_hex(state.unwrap()),
        c
    )
}

/// 行首状态点 + 名字（**纯函数**：glib 的转义不依赖 GTK 初始化，能直接单测）。
pub fn name_markup(name: &str, state: Option<State>) -> String {
    format!("{}{}", dot(state), glib::markup_escape_text(name))
}

/// 摘要列的文案：活着就加后缀（"已连接" / "运行中"）。
pub fn meta_with(base: &str, live: bool, suffix: &str) -> String {
    if live {
        format!("{base} · {suffix}")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 聚合：三个维度取**最严重**的（阶段 65 / 审计 P0-4）。
    #[test]
    fn 聚合取最严重的那个() {
        // 文件通道正常 + 终端活着 ⇒ 绿点
        assert_eq!(
            aggregate(Some(State::Ok), TermState::Alive, false),
            Some(State::Ok)
        );
        // ⚠️ **终端掉了不再是绿点**（审计 P0-4 的核心）
        assert_eq!(
            aggregate(Some(State::Ok), TermState::Dead, false),
            Some(State::Warn)
        );
        // 终端没上报 ⇒ **不参与**判定（不能谎报绿点，也不能报故障）
        assert_eq!(
            aggregate(Some(State::Ok), TermState::Unknown, false),
            Some(State::Ok)
        );
        // 文件通道自己失败 ⇒ 照旧
        assert_eq!(
            aggregate(Some(State::Err), TermState::Alive, false),
            Some(State::Err)
        );
        assert_eq!(
            aggregate(Some(State::Err), TermState::Dead, false),
            Some(State::Err)
        );
        // 什么状态都没有、终端却掉了 ⇒ 这个会话整个不对劲
        assert_eq!(aggregate(None, TermState::Dead, false), Some(State::Err));
        // 数据库是**可选**功能 ⇒ 连不连都不影响判定
        assert_eq!(
            aggregate(Some(State::Ok), TermState::Alive, true),
            Some(State::Ok)
        );
    }

    /// tooltip 要说清**哪一部分**出的问题（别只靠颜色说话）。
    #[test]
    fn tooltip_说清哪一部分坏了() {
        let t = state_tooltip(Some(State::Ok), TermState::Dead, None);
        assert!(t.contains("文件面板：已连接"), "{t}");
        assert!(t.contains("终端：已结束"), "{t}");
        assert!(t.contains("数据库：未连接"), "{t}");
        // 终端没上报 ⇒ 说"未知"，不谎报
        let t2 = state_tooltip(Some(State::Ok), TermState::Unknown, None);
        assert!(t2.contains("终端：未知"), "{t2}");
        // 数据库连上了 ⇒ 把名字写出来
        let t3 = state_tooltip(Some(State::Ok), TermState::Alive, Some("本地 PG"));
        assert!(t3.contains("数据库：已连接（本地 PG）"), "{t3}");
    }

    /// 终端状态的三个说法。
    #[test]
    fn 终端状态的说法() {
        assert_eq!(TermState::Alive.label(), "运行中");
        assert_eq!(TermState::Dead.label(), "已结束");
        assert_eq!(TermState::Unknown.label(), "未知");
    }

    /// **什么都没发生**的会话（那些从没打开过的存档连接）
    /// ⇒ 不该给它们挂一句"文件面板：未知 / 终端：未知 …"的噪音。
    #[test]
    fn 没有任何状态时不挂细分提示() {
        let mut info = ActiveInfo::default();
        // 从没打开过 ⇒ 没有 sftp 状态、没有终端上报、没有数据库
        assert!(
            info.session_tooltip("s:没开过").is_none(),
            "从没打开过的连接不该挂细分提示"
        );
        // 终端起来了 ⇒ 有得说了
        info.terminal.insert("s:开着".into(), TermState::Alive);
        let t = info.session_tooltip("s:开着").expect("终端活着就该有提示");
        assert!(t.contains("终端：运行中"), "{t}");
        // 文件面板连上了 ⇒ 有得说了
        let mut info2 = ActiveInfo::default();
        info2.sessions.insert("s:x".into(), State::Ok);
        let t2 = info2.session_tooltip("s:x").expect("连上了就该有提示");
        assert!(t2.contains("文件面板：已连接"), "{t2}");
    }

    #[test]
    fn 状态点只在连上或连接中时出现() {
        assert!(dot(Some(State::Ok)).contains('●'));
        // ⚠ 连接中**形状不一样**（◐ 不是 ●）：状态不能只靠颜色（审计 P2-6）
        assert!(dot(Some(State::Warn)).contains('◐'));
        assert!(!dot(Some(State::Warn)).contains('●'));
        // 没连上不画点
        assert_eq!(dot(None), "");
        assert_eq!(dot(Some(State::Err)), "");
    }

    #[test]
    fn 状态点的形状不只靠颜色() {
        // 已连接与连接中：颜色不同，**形状也不同** ⇒ 色弱用户也分得出
        assert_ne!(dot_char(Some(State::Ok)), dot_char(Some(State::Warn)));
        assert_ne!(dot(Some(State::Ok)), dot(Some(State::Warn)));
    }

    #[test]
    fn 快照查询按会话键与规则键() {
        let mut info = ActiveInfo::default();
        info.sessions.insert("s:a".into(), State::Ok);
        info.fwd_running
            .entry("s:a".into())
            .or_default()
            .insert("L:9000:h:80".into());
        info.db_connected
            .entry("s:a".into())
            .or_default()
            .insert("本地 PG".into());
        assert_eq!(info.session_state("s:a"), Some(State::Ok));
        assert_eq!(info.session_state("s:b"), None);
        assert!(info.fwd_running("s:a", "L:9000:h:80"));
        assert!(!info.fwd_running("s:a", "L:9001:h:80"));
        assert!(info.db_connected("s:a", "本地 PG"));
        assert!(!info.db_connected("s:a", "别的库"));
    }

    #[test]
    fn 行首状态点与名字拼在一起() {
        // 连上了 → 点 + 名字
        let m = name_markup("堡垒机", Some(State::Ok));
        assert!(m.contains('●'), "{m}");
        assert!(m.contains("堡垒机"), "{m}");
        // 名字里的尖括号照样要转义（状态点不能破坏 markup）
        let m2 = name_markup("a<b", Some(State::Warn));
        assert!(m2.contains("a&lt;b"), "{m2}");
        // 没状态 → 只有名字
        let m3 = name_markup("堡垒机", None);
        assert!(!m3.contains('●'), "{m3}");
        assert_eq!(m3, "堡垒机");
    }

    #[test]
    fn 摘要列活着才加后缀() {
        assert_eq!(meta_with("进程内", true, "运行中"), "进程内 · 运行中");
        assert_eq!(meta_with("进程内", false, "运行中"), "进程内");
    }
}
