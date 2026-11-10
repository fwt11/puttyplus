//! SFTP 双栏面板（对应 C 版 sftp_panel.c 的主体）。
//!
//! 线程模型：UI 状态集中在 `Rc<PanelState>`（仅主线程访问）；
//! 连接与传输在 worker 线程（见 worker.rs），通过 enum 消息往来。

// TreeView/ListStore 自 GTK 4.10 起标记弃用（推荐 ListView/ColumnView）。
// 为与 C 版行为/代码结构保持一致，第一版沿用旧 API。
#![allow(deprecated)]

pub mod dbconn;
pub mod dbgrid;
pub mod dbimport;
pub mod dbtransfer;
pub mod erview;
pub mod dbtree;
pub mod dbui;
pub mod closeguard;
pub mod clipfiles;
pub mod dialogs;
pub mod dnd;
/// **终态文案的淡出节奏**（「完成」显示一会儿再渐隐）。
pub mod fade;
pub mod fileedit;
pub mod fwdmgr;
/// **二进制内容的 hex 转储**（hex 模式打开非文本文件时用）。
pub mod hexdump;
pub mod mirrorui;
pub mod namecell;
pub mod pane;
pub mod pulse;
pub mod quickcmd;
pub mod queue;
pub mod relay;
pub mod subtabs;
pub mod relayui;
pub mod sessopt;
pub mod syntax;
pub mod worker;
/// X11 选区直读（阶段 101）—— **只在 X11/Unix 上存在**。
///
/// Windows 上没有 X11 选区协议 ⇒ 拖拽传文件走**剪贴板**那条路
/// （`clipfiles::read_local_paths`，见「粘贴上传」）。
#[cfg(unix)]
pub mod xdnd11;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use zeroize::Zeroizing;

use crate::args::ConnOptions;
use crate::platform::localpath;
use crate::theme::State;
use crate::util::{path_basename, path_join, path_parent};

use pane::Pane;
use queue::{QueueItem, QueueUi};
use worker::{FwdView, PanelEvent, SimpleOp, WorkerCmd, XferKind, XferSpec, XferState};
use crate::i18n::tr;
use crate::trf;

pub struct PanelState {
    pub opts: ConnOptions,
    /// 会话期保存的密码（来自 -pw 或用户输入），释放时抹零
    pub pw: RefCell<Option<Zeroizing<String>>>,
    pub conn_ok: Cell<bool>,
    /// **文件面板的宽度**（阶段 97）。
    ///
    /// 文件面板是**会话级的一个 widget**，但它的**宿主 Paned 跟着当前终端
    /// 页签走**（切到别的终端页签就把它搬过去）⇒ 宽度得**跨宿主同步**，
    /// 存在这儿，搬家时按它复位。
    ///
    /// 持久化仍走 `settings::sftp_pane_pos`（关连接标签时写盘）。
    pub file_w: Cell<i32>,
    /// **会话是不是已经死了**（worker 与 UI 共享，阶段 99）。
    ///
    /// ⚠️ **为什么共享**：SFTP 失败有两种——「连接断了」（**能自愈**：重连
    /// 一次 + 重试原来的操作）与「服务器说不行」（权限不足、重名 ⇒ 只能
    /// 报错）。UI 得知道是哪种。worker 在**发事件之前**就把标志写好 ⇒
    /// UI 读到的一定是当时的真实状态（没有竞态）。
    pub conn_dead: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 「连接断了」⇒ 重连之后**要重试**的那条操作（阶段 99）。
    pub pending_retry: RefCell<Option<RetryOp>>,
    /// 各类 SFTP 操作**"怎么重发"**（阶段 99）。
    ///
    /// ⚠️ **为什么要按种类分别记**：失败事件只带"路径"这类信息，够不着
    /// 一条完整命令（「重命名」要知道改成什么名、「写文件」要知道内容）。
    /// ⇒ 发命令时把**整条命令**记在这儿，失败时按种类取出来重发。
    pub retry_ops: RefCell<HashMap<RetryKind, RetryOp>>,
    /// **这条操作已经重试过一次了**（阶段 99）。
    ///
    /// ⚠️ **为什么要有**：用户原话是「重连**一次**，如果还是失败就报错」。
    /// 没有这道闸门 ⇒ 重连成功、重试又失败 ⇒ 又重连 ⇒ 无限循环（用户看着
    /// 是"程序在疯狂重连"）。
    /// ⇒ 成功了就清掉（下一条操作还能自愈），又失败了就报给用户并清掉。
    pub retried: Cell<bool>,
    /// 文件面板**当前的宿主 Paned**（阶段 98：文本编辑页也能托管它）。
    ///
    /// ⚠️ **为什么留一份强引用**：GTK4 里容器被销毁（finalize）时会把它
    /// 的子控件一起 `gtk_widget_destroy` ⇒ **宿主页签一关，文件面板就跟着
    /// 没了**（`PanelState` 手里那个 Rust 引用救不回来——widget 已经被标成
    /// 「不可再用」）。留一份强引用 ⇒ 宿主 Paned 不会被 finalize ⇒ 文件
    /// 面板安全等到下一次搬家（`host_file_panel` 把它挪走时才放手）。
    pub file_host: RefCell<Option<gtk::Paned>>,
    /// **正在连接 / 重连**（避免连点好几次，连一次就够）。
    pub connecting: Cell<bool>,
    /// 用户输入了路径、但**当时没连着** ⇒ 记在这儿，连上之后接着跳过去。
    pub pending_cd: RefCell<Option<String>>,
    pub tx: RefCell<Option<Sender<WorkerCmd>>>,
    /// 数据库「**测连**」的回包去处（新建数据库连接的对话框在等）。
    ///
    /// ⚠️ 为什么要有：那个对话框要**当场验密码**——密码错了就留在
    /// 那个对话框里、在密码框那儿提示，而不是先打开工作台再弹一个
    /// 「请输入密码」的框（那个框很容易被错过）。
    ///
    /// **一次只有一个在等**（对话框是模态流程，不需要并发）。
    pub db_test_cb: RefCell<Option<(u64, Rc<dyn Fn(Result<(), String>, bool)>)>>,
    /// 测连请求的编号（回包按编号对得上）。
    pub next_db_test_id: Cell<u64>,

    pub root: gtk::Paned,
    pub remote_pane: RefCell<Option<Rc<Pane>>>,
    pub local_pane: RefCell<Option<Rc<Pane>>>,
    pub remote_cwd: RefCell<String>,
    pub remote_home: RefCell<Option<String>>,
    pub local_cwd: RefCell<String>,

    /* 外部文件变化自动刷新 */
    /// 本地面板当前目录的 GIO 监视器（命令行 rm/touch 等即时反映）
    pub local_monitor: RefCell<Option<gio::FileMonitor>>,
    /// 监视器对应的目录，目录不变就不重建
    pub local_monitor_path: RefCell<String>,
    /// 本地刷新去抖：一次变更可能连着来好多个事件
    pub local_refresh_pending: Cell<bool>,
    /// 远端当前目录上次看到的 mtime（SFTP stat）
    pub remote_mtime: Cell<i64>,
    /// 已发出 DirStamp、等 worker 回包，避免堆积请求
    pub remote_stat_pending: Cell<bool>,
    /// 远程列表请求 id：只接受最新一次，防止慢的旧回包把目录改回去
    pub next_list: Cell<u64>,
    pub last_list: Cell<u64>,
    /// 最新一次 List 还在路上；期间不要发目录 stat 轮询
    pub remote_list_pending: Cell<bool>,

    pub rreconnect: RefCell<Option<gtk::Button>>,
    pub follow_btn: RefCell<Option<gtk::ToggleButton>>,

    /* 终端目录跟随 */
    pub follow: Cell<bool>,
    pub follow_pending: Cell<bool>,
    pub term_spawn_wall: Cell<i64>,
    /// 当前会话的保活间隔（秒）；会话选项改完存这里，重连时随 Connect 下发
    pub keepalive_sec: Cell<u32>,
    /// 认证优先级（2.2）；同上，重连时随 Connect 下发
    pub auth_order: Cell<crate::args::AuthOrder>,
    /// 本标签的终端（会话选项对话框要即时改字号/响铃）
    pub term: RefCell<Option<crate::termview::TermView>>,
    pub exec_fail: Cell<i32>,
    pub follow_poll_dead: Cell<bool>,
    pub follow_last: RefCell<Option<String>>,
    pub dead: Cell<bool>, // shutdown 后停轮询

    /* 传输队列 */
    pub next_id: Cell<u64>,
    /// 上传预检请求 id（每次 CheckExists 唯一，避免并发批次互相覆盖）
    pub next_check: Cell<u64>,
    pub items: RefCell<Vec<QueueItem>>,
    pub queue_ui: RefCell<Option<QueueUi>>,
    /// 文件面板工具栏上的「传输队列」按钮（阶段 102）。
    ///
    /// 传输进行中时它要变忙态并带**进行中数量角标** ⇒ 队列刷新时得能
    /// 找到它。⚠️ 按钮挂在面板上，面板属于本状态的控件树 ⇒ 这里存
    /// **强引用**不会成环（按钮不反过来抓 `PanelState`）。
    pub queue_btn: RefCell<Option<gtk::Button>>,
    /// **传输动画定时器**（呼吸相位）——只在**有东西在传**时存在。
    ///
    /// 干什么用（用户 2026-11-10）：传输中的文件行背景呼吸、队列按钮图标
    /// 跟着闪一下 ⇒ 空闲时一点 CPU 都不占。
    pub xfer_anim: RefCell<Option<glib::SourceId>>,

    /* 进程内端口转发（worker → Forwards 事件回填；状态栏摘要、tooltip 明细、
       转发管理窗口共用同一份视图快照，含停用项与失败原因） */
    pub fwd_views: RefCell<Vec<FwdView>>,
    pub fwd_ui: RefCell<Option<fwdmgr::FwdUi>>,

    /// 数据库窗口（7.2，懒创建，hide-on-close）
    pub db_ui: RefCell<Option<dbui::DbUi>>,

    /// **还活着的终端个数**（阶段 65 / 审计 P0-4）。
    ///
    /// 分屏 / 额外终端页签 ⇒ 一个会话可能有多个终端 ⇒ 记个数而不是布尔。
    /// 子进程起来 +1、退出 -1。
    pub term_alive_count: Cell<i32>,
    /// **起过的终端**（有没有成功起来过）。
    ///
    /// 用来区分「终端掉了」（Dead）和「终端还没起来」（Unknown）——
    /// 后者不该报成故障。
    pub term_ever_alive: Cell<bool>,

    /// **当前连着库的数据库连接名**（阶段 57：两棵树上的活跃标记用）。
    ///
    /// 为什么不直接读 `db_ui` 里的 `connected`：那边是工作台自己的状态，
    /// 读它要借 `db_ui` 的 RefCell——而工作台的事件处理**正借着**它，
    /// 借两次直接 panic。这里存一份副本：谁改谁写，别的模块只读。
    pub db_active: RefCell<Option<String>>,

    /// **当场输入 / 临时连接**的数据库密码（按连接名存，不进凭据库）。
    ///
    /// **为什么挂在面板状态上、而不是工作台里**：工作台页签是可以被用户
    /// 关掉的（关掉就自动断开），而密码是**会话级**的东西——用户刚输过
    /// 一次密码，关掉页签再打开**不该再问一遍**。挂在工作台上会跟着
    /// 页签一起没了（实测 e2e 里表现为"重新打开就要密码"）。
    ///
    /// **按连接名分开存**：一条连接的密码不能串到另一条上去——用户给 A
    /// 输了密码，切到 B 时不能拿 A 的密码去连 B。
    pub db_pass: RefCell<HashMap<String, zeroize::Zeroizing<String>>>,

    /// 目录镜像窗口（懒创建，hide-on-close）；worker 的扫描结果回填到它
    pub mirror_ui: RefCell<Option<mirrorui::MirrorUi>>,

    /// 已打开的本地/远程文本查看/编辑器窗口（每个文件一个独立窗口）
    pub file_edits: RefCell<Vec<Rc<fileedit::FileEdit>>>,
    /// 正在等 worker 回包的远端文件读取：id → 是否只读（查看 vs 编辑）
    pub pending_file_reads: RefCell<HashMap<u64, bool>>,

    /* 拖拽源记录（同面板自拖忽略） */
    pub drag_from: Cell<Option<bool>>,

    /// 状态变化钩子（标签栏的状态点用）：面板不关心谁在听。
    pub status_hook: RefCell<Option<Box<dyn Fn(State)>>>,

    /// 当前状态行的内容（级别 / 文案 / tooltip）。
    ///
    /// 存起来是为了**主题切换后能重画**：状态色来自 `theme::state_hex`，
    /// 是写进 Pango markup 的字符串，不跟着 CSS 变。
    pub status: RefCell<Option<(State, String, String)>>,

    /// 上传两阶段预检：请求 id → 等待结果的批次。
    /// 用 map 而不是单个 Vec：第一个预检还没回来时又提交第二批，
    /// 两批不能互相覆盖。
    pub pending_uploads: RefCell<HashMap<u64, Vec<(String, String, bool)>>>,
    /// 本连接页的子标签（终端 / 数据库 / 文件编辑器）
    ///
    /// **阶段 12**：文件编辑器与数据库工作台从独立弹窗改成**连接页里的子标签**
    /// （对标 XTerminal 的第二层标签栏）——弹窗把屏幕切碎了，数据库工作台
    /// 那点宽度根本不够用。
    pub sub_tabs: RefCell<Option<Rc<subtabs::SubTabs>>>,
}

impl PanelState {
    /// **测一下**这条数据库连接连不连得上（**不建会话、不动当前连接**）。
    ///
    /// 给「新建数据库连接」的对话框用：**当场验密码**——
    /// 连不上就留在那个对话框里、在**密码框**那儿提示，
    /// 而不是先打开工作台再弹一个「请输入密码」的框
    /// （那个框很容易被错过，错过之后用户就不知道该怎么办了）。
    ///
    /// `cb(结果, 服务器是不是要密码)`：结果 `Err(msg)` 里是后端的原文。
    pub fn db_test(
        &self,
        cfg: crate::backend::db::DbConfig,
        cb: impl Fn(Result<(), String>, bool) + 'static,
    ) {
        let id = self.next_db_test_id.get();
        self.next_db_test_id.set(id + 1);
        *self.db_test_cb.borrow_mut() = Some((id, Rc::new(cb)));
        let tx = self.tx.borrow().clone();
        let Some(tx) = tx else {
            // 没连上（SSH 都没通）⇒ 没什么可测的，当场回"连不上"
            if let Some((cid, c)) = self.db_test_cb.borrow_mut().take() {
                if cid == id {
                    c(Err(tr("SSH 连接还没建好").to_string()), false);
                }
            }
            return;
        };
        if tx.send(WorkerCmd::DbTest { id, config: cfg }).is_err() {
            if let Some((cid, c)) = self.db_test_cb.borrow_mut().take() {
                if cid == id {
                    c(Err(tr("后台连接线程不在了").to_string()), false);
                }
            }
        }
    }

    pub fn pane(&self, remote: bool) -> Rc<Pane> {
        if remote {
            Rc::clone(self.remote_pane.borrow().as_ref().unwrap())
        } else {
            Rc::clone(self.local_pane.borrow().as_ref().unwrap())
        }
    }

    pub fn pane_cwd(&self, remote: bool) -> String {
        if remote {
            self.remote_cwd.borrow().clone()
        } else {
            self.local_cwd.borrow().clone()
        }
    }

    pub fn set_pane_show_hidden(&self, remote: bool, v: bool) {
        self.pane(remote).show_hidden.set(v);
    }

    pub fn attach_pane_widget(&self, remote: bool, w: gtk::Box) {
        if remote {
            self.root.set_start_child(Some(&w));
        } else {
            self.root.set_end_child(Some(&w));
        }
    }

    /// 顶层窗口（对话框父窗口用）。
    pub fn topwin(&self) -> Option<gtk::Window> {
        self.root
            .ancestor(gtk::Window::static_type())
            .and_downcast::<gtk::Window>()
    }

    /// 本连接页的子标签容器（阶段 12）。
    pub fn sub_tabs(&self) -> Option<Rc<subtabs::SubTabs>> {
        self.sub_tabs.borrow().clone()
    }
}

pub struct SftpPanel {
    pub st: Rc<PanelState>,
}

impl SftpPanel {
    pub fn new(opts: &ConnOptions) -> SftpPanel {
        let st = Rc::new(PanelState {
            pw: RefCell::new(opts.password.clone()),
            conn_ok: Cell::new(false),
            file_w: Cell::new(340),
            file_host: RefCell::new(None),
            conn_dead: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            pending_retry: RefCell::new(None),
            retry_ops: RefCell::new(HashMap::new()),
            retried: Cell::new(false),
            connecting: Cell::new(false),
            pending_cd: RefCell::new(None),
            tx: RefCell::new(None),
            db_test_cb: RefCell::new(None),
            next_db_test_id: Cell::new(1),
            root: gtk::Paned::new(gtk::Orientation::Vertical),
            remote_pane: RefCell::new(None),
            local_pane: RefCell::new(None),
            remote_cwd: RefCell::new("/".into()),
            remote_home: RefCell::new(None),
            local_cwd: RefCell::new(String::new()),
            local_monitor: RefCell::new(None),
            local_monitor_path: RefCell::new(String::new()),
            local_refresh_pending: Cell::new(false),
            remote_mtime: Cell::new(0),
            remote_stat_pending: Cell::new(false),
            next_list: Cell::new(1),
            last_list: Cell::new(0),
            remote_list_pending: Cell::new(false),
            rreconnect: RefCell::new(None),
            follow_btn: RefCell::new(None),
            follow: Cell::new(true),
            follow_pending: Cell::new(false),
            term_spawn_wall: Cell::new(0),
            keepalive_sec: Cell::new(opts.keepalive_sec),
            auth_order: Cell::new(opts.auth_order),
            term: RefCell::new(None),
            exec_fail: Cell::new(0),
            follow_poll_dead: Cell::new(false),
            follow_last: RefCell::new(None),
            dead: Cell::new(false),
            next_id: Cell::new(1),
            next_check: Cell::new(1),
            items: RefCell::new(Vec::new()),
            queue_ui: RefCell::new(None),
            queue_btn: RefCell::new(None),
            /// **传输动画定时器**（呼吸相位）——只在**有东西在传**时存在。
            ///
            /// 干什么用（用户 2026-11-10）：传输中的文件行背景呼吸、队列
            /// 按钮图标闪一下 ⇒ 空闲时一点 CPU 都不占。
            xfer_anim: RefCell::new(None),
            mirror_ui: RefCell::new(None),
            file_edits: RefCell::new(Vec::new()),
            pending_file_reads: RefCell::new(HashMap::new()),
            fwd_views: RefCell::new(Vec::new()),
            fwd_ui: RefCell::new(None),
            db_ui: RefCell::new(None),
            term_alive_count: Cell::new(0),
            term_ever_alive: Cell::new(false),
            db_active: RefCell::new(None),
            db_pass: RefCell::new(HashMap::new()),
            drag_from: Cell::new(None),
            status_hook: RefCell::new(None),
            status: RefCell::new(None),
            pending_uploads: RefCell::new(HashMap::new()),
            sub_tabs: RefCell::new(None),
            opts: opts.clone(),
        });

        // 本地起始目录：PUTTYPLUS_LOCALDIR → 上次关闭时的目录
        //              → ~/Downloads → ~
        let mut local = std::env::var("PUTTYPLUS_LOCALDIR").unwrap_or_default();
        if local.is_empty() {
            local = crate::settings::last_local_dir()
                .filter(|p| usable_local_dir(p))
                .unwrap_or_else(default_local_dir);
        }
        *st.local_cwd.borrow_mut() = local;

        st.root.set_position(420); // 远程/本地 ≈ 55:45
        let remote_pane = Rc::new(pane::build_pane(&st, true));
        let local_pane = Rc::new(pane::build_pane(&st, false));
        *st.remote_pane.borrow_mut() = Some(remote_pane);
        *st.local_pane.borrow_mut() = Some(local_pane);

        fill_local_pane(&st);

        // worker 线程 + 事件通道（glib 0.22 已移除 MainContext::channel，
        // 用 async-channel + 主线程 spawn_local 循环替代）
        let (tx, rx) = std::sync::mpsc::channel::<WorkerCmd>();
        let (ui_tx, ui_rx) = async_channel::unbounded::<PanelEvent>();
        // 转发承接策略：⚠️ **跟终端用哪条路走**（`term::use_inproc_ssh`），
        // **不是**「系统里有没有 ssh/plink」——
        //
        // - 终端走 **spawn**（Linux 主线）：ssh/plink 已经用 `-L/-R/-D` 的
        //   argv 承接了转发，进程内再绑同一个端口只会 EADDRINUSE 撞车；
        // - 终端走 **inproc**（Windows 默认）：终端**根本没有** ssh 进程
        //   ⇒ 转发只能由程序内的转发引擎承接 ⇒ 这里要把规则交下去。
        //
        // ⚠️ **旧判据在 Windows 上必错**（用户 2026-11-10 追这个特色时
        //    挖出来的）：旧判据是「系统里找得到 ssh/plink 就当转发归它管」
        //    ⇒ Windows 机器上**只要装了 OpenSSH**（Win10/11 的可选功能，
        //    很多人装了），转发就被判给了一个**根本不会被 spawn** 的 ssh
        //    ⇒ 转发**静默失效**（界面上一点提示都没有）。
        //
        // ⚠️ Linux 主线**零变化**：`use_inproc_ssh()` 在 Linux 上恒为 false
        //    ⇒ 转发照旧进 ssh 的 argv。
        let worker_forwards = if crate::backend::forward::needs_inproc_forwards(
            opts.is_ssh(),
            crate::term::use_inproc_ssh(),
        ) {
            opts.forwards.clone()
        } else {
            Vec::new()
        };
        worker::spawn_worker(
            opts.host.clone(),
            opts.port,
            opts.user.clone(),
            opts.proxycmd.clone(),
            worker_forwards,
            opts.inproc_forwards.clone(),
            rx,
            ui_tx,
            // ⚠️ **会话生死标志与 UI 共享**（阶段 99）：UI 靠它判断
            // "这次失败能不能自愈（重连 + 重试）"。worker 在发事件**之前**
            // 写 ⇒ UI 读到的就是当时的真实状态。
            std::sync::Arc::clone(&st.conn_dead),
        );
        *st.tx.borrow_mut() = Some(tx);

        let st_loop = Rc::clone(&st);
        glib::MainContext::default().spawn_local(async move {
            loop {
                match ui_rx.recv().await {
                    Ok(ev) => handle_event(&st_loop, ev),
                    Err(_) => break, // sender 全部释放
                }
            }
        });

        if opts.is_ssh() && !opts.host.is_empty() {
            set_remote_sensitive(&st, false);
            set_remote_status(
                &st,
                State::Warn,
                &trf!("正在连接 {v0}@{v1}:{v2}…", v0 = opts.user, v1 = opts.host, v2 = opts.port),
            );
            submit_connect(&st);
        } else {
            set_remote_sensitive(&st, false);
            set_remote_status(&st, State::Err, tr("仅 SSH 协议支持文件传输"));
            if let Some(b) = st.rreconnect.borrow().as_ref() {
                b.set_sensitive(false);
            }
        }

        // 目录跟随轮询（默认 2 秒；C 对照基线是 4 秒。PUTTYPLUS_FOLLOW_MS 可调）
        // 上一次 exec 没回来时下面的回调会跳过这一拍（follow_pending），
        // 所以慢链路上不会把命令越堆越多。
        let mut ms: u64 = 2000;
        if let Ok(fm) = std::env::var("PUTTYPLUS_FOLLOW_MS") {
            ms = crate::util::atoi_like(&fm).max(0) as u64;
        }
        if ms > 0 {
            let st2 = Rc::clone(&st);
            glib::timeout_add_local(std::time::Duration::from_millis(ms), move || {
                follow_poll_cb(&st2)
            });
        }

        // 远端目录自动刷新：SFTP stat 当前目录，mtime 变了就静默重列。
        // 不用每次全量 read_dir；PUTTYPLUS_WATCH_MS 可调（0 = 关闭，测试用）。
        let mut watch_ms: u64 = 2500;
        if let Ok(v) = std::env::var("PUTTYPLUS_WATCH_MS") {
            watch_ms = crate::util::atoi_like(&v).max(0) as u64;
        }
        if watch_ms > 0 {
            let st3 = Rc::clone(&st);
            glib::timeout_add_local(
                std::time::Duration::from_millis(watch_ms),
                move || remote_watch_cb(&st3),
            );
        }

        SftpPanel { st }
    }

    pub fn widget(&self) -> &gtk::Paned {
        &self.st.root
    }
}

/* ---------------- 文件面板的"搬家"（阶段 97 / 98） ---------------- */

/// 把文件面板**搬到**某个宿主页的 Paned 里（阶段 97）。
///
/// ⚠️ **GTK4：一个 widget 只能有一个父级** ⇒ 搬家必须先把旧框里的槽位
/// 清掉，再挂新的框，否则 GTK 报 `gtk_paned_set_start_child: assertion
/// 'child->parent == NULL' failed`（AGENTS.md 记过同类坑：「同一个 widget
/// 别 append 进两个 Box」）。
///
/// **为什么走 Paned 的槽位 API**（而不是直接 `w.unparent()`）：
/// `set_start_child(None)` 会让 GTK **自己**把旧孩子解除父级并清掉槽位
/// ⇒ 旧框不会留着一个野指针（它析构时不会顺手把孩子销毁掉）。
///
/// **谁能当宿主**：**终端子页**与**文本编辑子页**（阶段 98）——这两页的
/// 左边都摆文件面板；**数据库工作台页不摆**（它要整页宽度）。
///
/// **为什么文件面板要搬家**：它是**会话级的一个 widget**（`PanelState::root`），
/// 而一个连接页里可以有好几个终端页签、好几个编辑器页签 ⇒ 文件面板跟着
/// **当前**那一页走。
///
/// **宽度**：搬家后按 `st.file_w`（跨宿主同步的那个值）复位。
pub fn host_file_panel(st: &Rc<PanelState>, host: &gtk::Paned) {
    let w = &st.root; // 文件面板 widget
    if let Some(old) = w.parent().and_downcast::<gtk::Paned>() {
        // 旧宿主先把槽位让出来（GTK 顺带把文件面板解除父级）
        let me = w.upcast_ref::<gtk::Widget>();
        if old.start_child().as_ref() == Some(me) {
            old.set_start_child(None::<&gtk::Widget>);
        } else if old.end_child().as_ref() == Some(me) {
            old.set_end_child(None::<&gtk::Widget>);
        }
    } else if w.parent().is_some() {
        // 不是 Paned 的父级（少见）⇒ 直接解除父级
        w.unparent();
    }
    host.set_start_child(Some(w));       // 放进新框
    host.set_position(st.file_w.get());  // 宽度按记忆值复位
    // ⚠️ **记住新宿主（强引用）**：见 `PanelState::file_host` 的注释——
    // 宿主页签被关掉时，宿主 Paned 还得活着，文件面板才不会被连带销毁。
    *st.file_host.borrow_mut() = Some(host.clone());
}

/// 让宿主的分割条位置**同步回** `file_w`（搬家之后宽度得跟着走）。
///
/// ⚠️ **只有当前正托管文件面板的那个框才算数**：别的框 position=0，
/// 照样写回会把记住的宽度冲成 0（用户下次打开就变成默认宽度了）。
pub fn track_file_width(st: &Rc<PanelState>, host: &gtk::Paned) {
    let stw = Rc::downgrade(st);
    host.connect_notify_local(Some("position"), move |p, _| {
        let Some(st) = stw.upgrade() else { return };
        if st.root.parent().as_ref() == Some(p.upcast_ref::<gtk::Widget>()) {
            st.file_w.set(p.position());
        }
    });
}

/* ---------------- 断线自愈：失败 ⇒ 重连一次 ⇒ 重试原来的操作（阶段 99） ---------------- */

/// 「连接断了」时**要重试**的那条操作（阶段 99）。
///
/// ⚠️ **为什么要单独列出来**：SFTP 失败分两种——「连接断了」（能自愈：
/// 重连一次 + 重试原来的操作）与「服务器说不行」（权限不足、重名 ⇒ 只能
/// 报错给用户）。后者**不能**重试（重试还是同样的错）。
///
/// **用户原话**（2026-11-06）：「在异常处理程序中重连一次，如果还是失败
/// 就报错，否则就继续执行原来的操作」。
#[derive(Debug, Clone)]
pub enum RetryOp {
    /// 列目录（点目录 / 刷新 / 终端切目录跟随）
    List { path: String },
    /// 新建 / 重命名 / 删除
    Simple {
        op: SimpleOp,
        path: String,
        new_path: Option<String>,
    },
    /// 读远程文件（内置编辑器打开）
    ReadFile { id: u64, path: String, max: u64 },
    /// 写远程文件（内置编辑器保存）
    WriteFile { id: u64, path: String },
}

/// [`RetryOp`] 的**种类**（决定失败事件去哪个槽里取"怎么重发"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetryKind {
    List,
    Simple,
    ReadFile,
    WriteFile,
}

/// 记下"这条命令怎么再发一遍"（断线失败 ⇒ 重连后重试用）。
fn remember_retry(st: &Rc<PanelState>, kind: RetryKind, op: RetryOp) {
    st.retry_ops.borrow_mut().insert(kind, op);
}

/// 这次失败是不是"连接断了"（⇒ 能自愈）。
///
/// ⚠️ **两个来源都看**：worker 写的标志（精确到那一刻）+ UI 自己的
/// `conn_ok`（收到「连接已断开」事件后就一直是 false）。
fn failure_is_conn_dead(st: &Rc<PanelState>) -> bool {
    st.conn_dead.load(std::sync::atomic::Ordering::SeqCst) || !st.conn_ok.get()
}

/// 安排"重连一次 ⇒ 重试**这条**操作"。返回 true = **已安排**（调用方就
/// 别再弹错误框了——重连成功后会接着做，不成功才由重试那次的失败来报）。
///
/// ⚠️ **已经在重连中 ⇒ 不安排**：否则会无限重试（一次失败触发一次重连，
/// 重连又失败再触发……）。这时候这一次失败照常报错。
///
/// ⚠️ **为什么由调用方决定"是哪一条"**：失败事件说的是**哪一条**失败了，
/// 而 `retry_ops` 记的是**最后一次发出去**的那条——两者可能不是同一条
/// （中间插进来一条后台刷新就会改写它）⇒ 失败事件自带路径的用自带路径，
/// 不自带的（重命名要知道改成什么名、写文件要知道内容）**核对过再用**。
fn schedule_retry_op(st: &Rc<PanelState>, op: RetryOp) -> bool {
    if st.connecting.get() {
        return false;
    }
    if st.retried.get() {
        // ⚠️ **已经重试过一次了**（用户：「重连**一次**，如果还是失败就报错」）
        // ⇒ 这次就报给用户，并把这道闸门清掉（下一条操作还能自愈）。
        st.retried.set(false);
        return false;
    }
    *st.pending_retry.borrow_mut() = Some(op);
    st.retried.set(true);
    // ⚠️ **先重连、后写状态**：`on_reconnect` 自己也会写状态（「正在连接
    // ……」），写在它后面就把「正在重新连接」盖掉了。
    on_reconnect(st);
    set_remote_status(st, State::Warn, tr("连接已断开，正在重新连接…"));
    true
}

/// 「记住的那条操作」是不是「这次失败的那一条」？
///
/// ⚠️ **为什么要核对**：记住的是**最后一次发出去**的那条；两条操作都在路上
/// 时（用户连着点了两个目录、开了两个文件），失败的可能不是最后那条。
/// ⇒ 对不上就**不重试**（只能报错）——重试错的那条等于替用户干了一件事
/// 他没要求的事（读别的文件 / 翻别的目录）。
///
/// ⚠️ **纯函数**（好测：不碰 GTK、不连服务器）。
fn retry_op_matches(op: &RetryOp, path: &str) -> bool {
    match op {
        RetryOp::List { path: p } => p == path,
        RetryOp::Simple { path: p, .. } => p == path,
        RetryOp::ReadFile { path: p, .. } => p == path,
        RetryOp::WriteFile { path: p, .. } => p == path,
    }
}

/// 取出「记住的那条操作」，**并核对它是不是这次失败的那一条**。
fn take_retry_op(st: &Rc<PanelState>, kind: RetryKind, path: &str) -> Option<RetryOp> {
    let op = st.retry_ops.borrow().get(&kind).cloned()?;
    if retry_op_matches(&op, path) {
        Some(op)
    } else {
        None
    }
}

/// 重连成功之后：**把要重试的操作再发一遍**（继续执行原来的操作）。
///
/// 返回 true = 确实重发过（调用方就别再走"回到原目录"那条路了）。
fn run_pending_retry(st: &Rc<PanelState>) -> bool {
    let Some(op) = st.pending_retry.borrow_mut().take() else {
        return false;
    };
    match &op {
        RetryOp::List { path } => submit_list(st, path),
        RetryOp::Simple {
            op,
            path,
            new_path,
        } => submit_simple(st, *op, path, new_path.as_deref()),
        RetryOp::ReadFile { id, path, max } => fileedit::resend_read(st, *id, path, *max),
        RetryOp::WriteFile { id, path } => fileedit::resend_save(st, *id, path),
    }
    true
}

/// SFTP 断了就重连一次（"重连终端时顺带救回 SFTP"用）。
///
/// ⚠️ **幂等**：已经在连了就不重复发起（`connecting` 挡着）。
pub fn reconnect_sftp_if_dead(st: &Rc<PanelState>) {
    if st.connecting.get() {
        return;
    }
    if failure_is_conn_dead(st) {
        on_reconnect(st);
    }
}

/* ---------------- 本地目录记忆 ---------------- */

/// 路径是否真的能读：不只 stat 是目录，还要能列——省得恢复到一个
/// 没权限的目录后，一启动就弹「读取本地目录失败」。
fn usable_local_dir(path: &str) -> bool {
    std::path::Path::new(path).is_absolute() && std::fs::read_dir(path).is_ok()
}

/// 没有记忆时的默认起点：~/Downloads（可读）→ ~。
fn default_local_dir() -> String {
    let dl = glib::home_dir().join("Downloads");
    if usable_local_dir(&dl.to_string_lossy()) {
        dl.to_string_lossy().into_owned()
    } else {
        glib::home_dir().to_string_lossy().into_owned()
    }
}

/// 记住本地目录（相对路径先转绝对，避免从桌面启动时 cwd 不同导致跑偏）。
pub fn remember_local_dir(path: &str) {
    if path.is_empty() {
        return;
    }
    let abs = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());
    crate::settings::set_last_local_dir(&abs);
}

/// 本地目录的唯一变更入口：改状态 + 立即落盘。
fn set_local_cwd(st: &Rc<PanelState>, path: &str) {
    *st.local_cwd.borrow_mut() = path.to_string();
    remember_local_dir(path);
}

/// 终端 ssh 启动时刻（秒），目录跟随轮询用。
pub fn set_term_spawn(st: &Rc<PanelState>, t: i64) {
    st.term_spawn_wall.set(t);
}

/// 把本标签的终端交给面板：会话选项对话框要即时改字号/响铃，
/// 不用穿过 Tabs 再回来。
pub fn set_term(st: &Rc<PanelState>, term: &crate::termview::TermView) {
    *st.term.borrow_mut() = Some(term.clone());
}

/// OSC 7 报告的终端目录（file:// URI）。
pub fn notify_cwd_uri(st: &Rc<PanelState>, uri: &str) {
    let Some(path) = uri.strip_prefix("file://").and_then(|r| r.find('/').map(|i| &r[i..])) else {
        return;
    };
    let dec = percent_decode(path);
    follow_apply(st, &dec);
}

/// 这条终端**当前所在的远程目录**（远端那个 shell 的 cwd）。
///
/// 干什么用：**重连后回到原来的目录**（用户 2026-11-10）。终端自己
/// 不知道 cwd（OSC 7 只有部分 shell 会发）⇒ 这里知道——目录跟随轮询
/// （2 秒一次）问远端那个 shell 要它的 cwd，OSC 7 也往这儿汇。
///
/// ⚠️ **断线时不清空**：断了之后这个值正好就是「重连后要回去的目录」。
///
/// ⚠️ 目录跟随被用户关掉时，这个值会**停在最后一次观察到的目录**——
///    那仍旧是「shell 上次在哪儿」，比没有强。
pub fn last_term_cwd(st: &Rc<PanelState>) -> Option<String> {
    st.follow_last.borrow().clone().filter(|d| !d.is_empty())
}

/// 这个会话的**终端存活状态**（阶段 65 / 审计 P0-4）。
///
/// 三态：没起来过（Unknown）/ 活着（Alive）/ 起过但结束了（Dead）。
pub fn term_state(st: &Rc<PanelState>) -> crate::active::TermState {
    use crate::active::TermState;
    if st.term_alive_count.get() > 0 {
        TermState::Alive
    } else if st.term_ever_alive.get() {
        TermState::Dead
    } else {
        TermState::Unknown
    }
}

/// 终端的存活状态变了（子进程起来 / 退出）。
///
/// 改完要**刷新状态点**：标签上的点与两棵树上的点都靠它。
pub fn note_term_alive(st: &Rc<PanelState>, alive: bool) {
    if alive {
        st.term_alive_count.set(st.term_alive_count.get() + 1);
        st.term_ever_alive.set(true);
    } else {
        let n = st.term_alive_count.get() - 1;
        st.term_alive_count.set(n.max(0));
    }
    // 状态点要跟着变（标签上的点 + 两棵树上的点）
    crate::tabs::refresh_dots();
    refresh_active_marks();
}

/// 冒烟 / e2e 用：设置面板状态（**不真的连**）。
///
/// 状态点是聚合的（阶段 65），冒烟要能单独造出"文件面板正常、终端已死"
/// 这种组合 ⇒ 得能单独设状态。
pub fn set_status_for_smoke(st: &Rc<PanelState>, level: State, text: &str) {
    set_remote_status(st, level, text);
}

/// 正在跑或排队的传输个数（阶段 63：关窗口确认要用）。
///
/// "排队"也算：还没开始传，但关窗口一样会被取消掉。
pub fn running_xfer_count(st: &Rc<PanelState>) -> usize {
    st.items
        .borrow()
        .iter()
        .filter(|it| {
            let g = it.shared.lock().unwrap();
            matches!(g.state, XferState::Wait | XferState::Run)
        })
        .count()
}

/// 退出前清理：取消传输、销毁队列窗口、通知 worker 退出。
pub fn shutdown(st: &Rc<PanelState>) {
    st.dead.set(true);
    for it in st.items.borrow().iter() {
        it.shared.lock().unwrap().cancel = true;
    }
    // 队列窗口是 hide-on-close 的独立窗口，不销毁会拖着进程不退出
    if let Some(qu) = st.queue_ui.borrow().as_ref() {
        qu.win.destroy();
    }
    if let Some(fu) = st.fwd_ui.borrow().as_ref() {
        fu.win.destroy();
    }
    // 数据库工作台**不再有属于自己的窗口**（阶段 12：它是连接页里的子标签），
    // 所以这里只能关它自己那两个浮窗（搜索结果 / 执行历史）。
    // ⚠️ 千万别去 destroy `db_ui.win`——那是宿主窗口（主窗口），
    // destroy 掉等于关一个连接标签就把整个程序关了。
    dbui::close_now(st);
    // 查看/编辑窗口是每个文件一个，关标签时一并销毁（含未保存内容）
    fileedit::close_all(st);
    st.local_monitor.borrow_mut().take();
    st.tx.borrow_mut().take(); // drop sender → worker 退出
}

/* ---------------- 状态显示 ---------------- */

fn set_remote_status(st: &Rc<PanelState>, level: State, text: &str) {
    set_remote_status_tip(st, level, text, text);
}

/// 状态栏 + 独立 tooltip（转发明细较长，tooltip 放完整信息）。
fn set_remote_status_tip(st: &Rc<PanelState>, level: State, text: &str, tip: &str) {
    *st.status.borrow_mut() = Some((level, text.to_string(), tip.to_string()));
    render_status(st);
    // 活跃标记（阶段 57）：会话状态变了 → 两棵树上的状态点要跟着变
    refresh_active_marks();
}

/// 活跃状态变了 → 会话树（侧栏）重画。
///
/// ⚠️ **别在这儿做重活**：状态变化只在连接/断开/转发增删时发生，频率很低，
/// 重建一棵几十行的树完全够用；但要是改成轮询就会把树的选中状态冲掉。
pub fn refresh_active_marks() {
    crate::sidebar::refresh();
    // 标签上的状态点也是聚合的（SFTP + 终端 + 数据库）⇒ 一起刷（阶段 65）
    crate::tabs::refresh_dots();
}

/// 注册状态变化钩子（标签栏状态点用），注册后立刻用当前状态回调一次。
pub fn set_status_hook<F: Fn(State) + 'static>(st: &Rc<PanelState>, f: F) {
    *st.status_hook.borrow_mut() = Some(Box::new(f));
    render_status(st);
}

/// 当前状态级别（没设过就 None）。
pub fn current_state(st: &Rc<PanelState>) -> Option<State> {
    st.status.borrow().as_ref().map(|(l, _, _)| *l)
}

/// 按当前主题重画状态行（主题切换后由 `tabs::refresh_theme_colors` 调）。
pub fn reapply_status(st: &Rc<PanelState>) {
    render_status(st);
}

/// 传输进行中的条目数（按方向分，阶段 102）。
///
/// ⚠️ **只数"还在传"的**（`Run` / `Wait`）：传完的、失败的、取消的都不算
/// —— 否则传完了角标还挂着，用户以为还在传。
///
/// ⚠️ `st.items` 里**完成的条目不会被自动清掉**（只有用户在队列窗口里
/// 「清除已完成」才清）⇒ 判定必须看 `shared.state`，不能看列表长度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XferRunning {
    /// 还在传的上传条目数。
    pub upload: usize,
    /// 还在传的下载条目数。
    pub download: usize,
}

impl XferRunning {
    /// 还在传的条目总数。
    pub fn total(&self) -> usize {
        self.upload + self.download
    }

    /// 状态行 / tooltip 上显示的那一句（`None` = 现在没在传）。
    ///
    /// ⚠️ **分方向说**：上传和下载对用户是两件事（一个往远端送、一个从
    /// 远端取），混成「传输中 2 个」用户还得自己猜是哪个方向。
    pub fn label(&self) -> Option<String> {
        match (self.upload, self.download) {
            (0, 0) => None,
            (u, 0) => Some(trf!("上传中 {u} 个", u = u)),
            (0, d) => Some(trf!("下载中 {d} 个", d = d)),
            (u, d) => Some(trf!("传输中 {n} 个", n = u + d)),
        }
    }
}

/// 传输进行中的条目数（按方向分）。
pub fn xfer_running(st: &Rc<PanelState>) -> XferRunning {
    let states: Vec<(worker::XferState, bool)> = st
        .items
        .borrow()
        .iter()
        .map(|it| {
            let state = it.shared.lock().map(|g| g.state).unwrap_or(worker::XferState::Wait);
            (state, it.upload)
        })
        .collect();
    count_running(&states)
}

/// 从「（状态, 是不是上传）」算还在传几个（**纯函数**，方便单测）。
pub fn count_running(states: &[(worker::XferState, bool)]) -> XferRunning {
    let mut r = XferRunning::default();
    for (state, upload) in states {
        if !matches!(
            state,
            worker::XferState::Run | worker::XferState::Wait
        ) {
            continue;
        }
        if *upload {
            r.upload += 1;
        } else {
            r.download += 1;
        }
    }
    r
}

/// 传输进行中的条目总数。
pub fn xfer_running_count(st: &Rc<PanelState>) -> usize {
    xfer_running(st).total()
}

/// **传输动画**（呼吸相位）：有东西在传就起定时器，传完就停。
///
/// 干什么用（用户 2026-11-10）：传输中的**文件行背景呼吸**、队列按钮图标
/// 跟着闪一下 ⇒ 比光看进度文字显眼得多。
///
/// ⚠️ **相位按时间算**（见 `pulse::phase`）⇒ 这个定时器只负责"持续重绘"，
///    多个面板各起各的也**天然同步**（不会互相打架）。
///
/// ⚠️ 定时器闭包只抓**弱引用**（面板没了就停）——抓强引用就是引用环。
pub fn sync_xfer_anim(st: &Rc<PanelState>) {
    let busy = xfer_running(st).total() > 0;
    // ⚠️ **还有正在淡出的终态文案 ⇒ 动画也得继续**（用户 2026-11-10：
    //    「完成」慢慢消失 ⇒ 传输早就结束了，但那一行还得继续淡）。
    let fading = any_fading(st);
    if busy || fading {
        if st.xfer_anim.borrow().is_some() {
            return; // 已经在跑了
        }
        let weak = Rc::downgrade(st);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(60), move || {
            let Some(st) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if st.dead.get() {
                return glib::ControlFlow::Break;
            }
            // ⚠️ **推进终态文案的淡出**（「完成」慢慢消失）
            let now = pane::now_ms();
            // 文件列表（两栏）：正在传的行背景要跟着呼吸重画
            let rp = st.remote_pane.borrow().clone();
            if let Some(p) = rp {
                pane::tick_fade(&p, now);
                p.view.queue_draw();
            }
            let lp = st.local_pane.borrow().clone();
            if let Some(p) = lp {
                pane::tick_fade(&p, now);
                p.view.queue_draw();
            }
            // 队列窗口：方向列的 accent 色在呼吸
            let qu = st.queue_ui.borrow().as_ref().map(|q| q.view.clone());
            if let Some(v) = qu {
                v.queue_draw();
            }
            // 工具栏「传输队列」按钮的图标：透明度呼吸
            queue::breathe_btn(&st);
            // ⚠️ **没事做了就停**（省 CPU）。
            //    ⚠️ 回调里**不能** `id.remove()` 自己（那会 panic）⇒ 只把
            //    存起来的 SourceId 清掉，靠 `Break` 让 GLib 收掉这个源。
            if xfer_running(&st).total() == 0 && !any_fading(&st) {
                *st.xfer_anim.borrow_mut() = None;
                queue::reset_btn_opacity(&st);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *st.xfer_anim.borrow_mut() = Some(id);
    } else {
        if let Some(id) = st.xfer_anim.borrow_mut().take() {
            id.remove();
        }
        // 停了 ⇒ 图标恢复成不透明（别留个半透明的鬼影）
        queue::reset_btn_opacity(st);
    }
}

/// 两栏里**还有没有**正在淡出的终态文案（"完成"慢慢消失）。
fn any_fading(st: &Rc<PanelState>) -> bool {
    let rp = st.remote_pane.borrow().clone();
    if let Some(p) = rp {
        if pane::has_fading(&p) {
            return true;
        }
    }
    let lp = st.local_pane.borrow().clone();
    if let Some(p) = lp {
        if pane::has_fading(&p) {
            return true;
        }
    }
    false
}

fn render_status(st: &Rc<PanelState>) {
    let Some((level, text, tip)) = st.status.borrow().clone() else {
        return;
    };
    let lbl = st.pane(true).status_lbl.clone();
    // 传输中 ⇒ 状态行追加「上传中 N 个」（阶段 102）。
    //
    // ⚠️ **追加而不是替换**：状态行平时显示的是「已连接 user@host:port」，
    // 那是常驻信息；直接拿它改成「开始上传…」会把连接信息抹掉。追加在后面，
    // 连接信息还在（完整内容在 tooltip 里）。
    //
    // ⚠️ 追加的那截**单独上色**（绿）：它是"现在有事在发生"，跟常驻的
    // 连接信息不是一回事 ⇒ 扫一眼就能看见。
    let running = xfer_running(st);
    let hint = running.label();
    let esc = glib::markup_escape_text(&text);
    let markup = match &hint {
        None => format!(
            "<span foreground='{}'>●</span> {}",
            crate::theme::state_hex(level),
            esc.as_str()
        ),
        Some(h) => format!(
            "<span foreground='{}'>●</span> {}<span foreground='{}'>｜{}</span>",
            crate::theme::state_hex(level),
            esc.as_str(),
            crate::theme::state_hex(crate::theme::State::Ok),
            glib::markup_escape_text(h).as_str()
        ),
    };
    lbl.set_markup(&markup);
    // 工具栏宽度有限，显示可截断；完整信息悬停可看
    let tip = match &hint {
        None => tip,
        Some(h) => format!("{tip}\n{h}"),
    };
    lbl.set_tooltip_text(Some(&tip));
    // 通知外部（标签栏状态点）
    if let Some(h) = st.status_hook.borrow().as_ref() {
        h(level);
    }
}

/// 已连接状态栏文案：基础信息 + 端口转发摘要（详情见 connected_tip）。
fn connected_text(st: &Rc<PanelState>, _detail: &str) -> String {
    // ⚠️ 阶段 76 / 审计 P1-15：**不重复路径**。路径栏就在上面一行，
    // 状态行再抄一遍完整路径 ⇒ 340px 的面板里被截断，什么都看不清。
    // 完整路径挪进 tooltip（悬停能看全）。
    let base = trf!("已连接 {v0}@{v1}:{v2}", v0 = st.opts.user, v1 = st.opts.host, v2 = st.opts.port);
    let n = st.fwd_views.borrow().len();
    if n > 0 {
        trf!("{base}｜转发 {n}", base = base, n = n)
    } else {
        base
    }
}

fn connected_tip(st: &Rc<PanelState>, detail: &str) -> String {
    // ⚠️ tooltip 里**保留完整路径**（状态行放不下的东西在这儿能看）
    let text = trf!("已连接 {v0}@{v1}:{v2} — {detail}", detail = detail, v0 = st.opts.user, v1 = st.opts.host, v2 = st.opts.port);
    let views = st.fwd_views.borrow();
    if views.is_empty() {
        text
    } else {
        let body = views
            .iter()
            .map(FwdView::line_text)
            .collect::<Vec<_>>()
            .join("\n");
        trf!("{text}\n\n端口转发：\n{body}", text = text, body = body)
    }
}

fn set_connected_status(st: &Rc<PanelState>, detail: &str) {
    set_remote_status_tip(
        st,
        State::Ok,
        &connected_text(st, detail),
        &connected_tip(st, detail),
    );
}

fn set_remote_sensitive(st: &Rc<PanelState>, sens: bool) {
    let pane = st.pane(true);
    let mut c = pane.toolbar.first_child();
    while let Some(w) = c {
        // 路径框保持可输入：断线时也能先写好路径，重连后回车即去
        if !w.is::<gtk::Entry>() && !w.is::<gtk::Label>() {
            w.set_sensitive(sens);
        }
        c = w.next_sibling();
    }
    pane.view.set_sensitive(sens);
}

/* ---------------- 提交任务 ---------------- */

/// 要用远端连接时调它：**断了就自动重连**。
///
/// 返回 `false` = "这次先别做，等连上再继续"（调用方把要做的事记下来，
/// 连上之后会自动接着做）。
///
/// ⚠️ **为什么要有**（2026-11-05，用户原话：「有任何变化，sftp自动重连，
/// 比如改变目录的时候、刷新的时候，发现 sftp断了就自动重连」）：
/// 以前 SFTP 会话断了，面板还显示「已连接」——用户点「刷新」只弹一个
/// 「读取远程目录失败」，还得自己想到去点「重新连接 SFTP」。
fn ensure_remote(st: &Rc<PanelState>) -> bool {
    if st.conn_ok.get() {
        return true;
    }
    if st.connecting.get() {
        // 已经在连了 ⇒ 别再发一次（连一次就够）
        return false;
    }
    st.connecting.set(true);
    set_remote_status(st, State::Warn, tr("连接已断开，正在重新连接…"));
    submit_connect(st);
    false
}

fn submit_connect(st: &Rc<PanelState>) {
    let pw = st
        .pw
        .borrow()
        .as_ref()
        .map(|p| p.to_string())
        // ⚠️ **阶段 C8：本次运行内问过的密码直接拿来用**——
        //    同一个会话里 SFTP / 终端 / 分屏是**三条独立连接**，
        //    认证各自独立 ⇒ 终端那边问过了，这边就不用再问一遍
        //    （用户 2026-11-08 报的就是"要分别输入密码"）。
        .or_else(|| {
            let key = crate::pwcache::key(&st.opts.host, st.opts.port, &st.opts.user);
            crate::pwcache::get(&key).map(|z| z.to_string())
        });
    let ka = st.keepalive_sec.get();
    let ao = st.auth_order.get();
    let _ = st.tx.borrow().as_ref().unwrap().send(WorkerCmd::Connect {
        keepalive_sec: ka,
        auth_order: ao,
        auth_method: st.opts.auth_method,
        password: pw,
        keyfile: st.opts.keyfile.clone(),
    });
}

fn submit_list_full(st: &Rc<PanelState>, path: &str, silent: bool) {
    let id = st.next_list.get();
    st.next_list.set(id + 1);
    st.last_list.set(id);
    st.remote_list_pending.set(true);
    // ⚠️ **先记下"怎么重发"**（阶段 99）：断线失败时要靠它重试同一条操作
    remember_retry(
        st,
        RetryKind::List,
        RetryOp::List {
            path: path.to_string(),
        },
    );
    if st
        .tx
        .borrow()
        .as_ref()
        .unwrap()
        .send(WorkerCmd::List {
            id,
            path: path.to_string(),
            silent,
        })
        .is_err()
    {
        st.remote_list_pending.set(false);
    }
}

fn submit_list(st: &Rc<PanelState>, path: &str) {
    submit_list_full(st, path, false);
}

fn submit_simple(st: &Rc<PanelState>, op: SimpleOp, path: &str, new_path: Option<&str>) {
    // ⚠️ **先记下"怎么重发"**（阶段 99）：断线失败时要靠它重试同一条操作
    remember_retry(
        st,
        RetryKind::Simple,
        RetryOp::Simple {
            op,
            path: path.to_string(),
            new_path: new_path.map(str::to_string),
        },
    );
    let _ = st.tx.borrow().as_ref().unwrap().send(WorkerCmd::Simple {
        op,
        path: path.to_string(),
        new_path: new_path.map(str::to_string),
    });
}

/* ---------------- 事件处理（主线程） ---------------- */

/// 连接被替换/断开时，旧调度器里的任务已经 abort；把 UI 侧还没落终态的
/// 项标成取消并汇总一次，免得队列永远停在「等待/传输中」。
fn cancel_aborted_xfers(st: &Rc<PanelState>) {
    let mut cancelled = Vec::new();
    for it in st.items.borrow().iter() {
        let mut g = it.shared.lock().unwrap();
        if matches!(g.state, XferState::Wait | XferState::Run) {
            g.state = XferState::Cancel;
            g.msg = Some(tr("连接已断开或已重建").into());
            cancelled.push((
                // ⚠️ src 可能是本地路径（上传）也可能是远端路径（下载）
                // ⇒ 取名用「两边都成立」的那个（POSIX 远端路径也能用）。
                localpath::basename(&it.src),
                XferState::Cancel,
            ));
        }
    }
    if !cancelled.is_empty() {
        // 通知发不出去时在状态栏写一句（**不弹窗**）
        let st2 = Rc::clone(st);
        crate::notify::report_batch(&cancelled, move |msg| {
            set_remote_status(&st2, State::Warn, msg);
        });
    }
}

/// 处理一个来自 worker 的事件（**公开**：冒烟要能自己喂事件）。
///
/// 正常运行时是 [`crate::panel::worker`] 的回包循环在调它；
/// 冒烟（`examples/db_conn_dialog_smoke.rs`）要能**自己造**一个
/// 「测连失败」的回包 ⇒ 得能直接调。
pub fn handle_event(st: &Rc<PanelState>, ev: PanelEvent) {
    match ev {
        PanelEvent::Disconnected => {
            // 底层会话断了（服务端关了 / 网络断了 / 空闲超时）⇒ 标成未连接。
            // 下一次操作（刷新 / 换目录）会**自动重连**（`ensure_remote`）。
            st.conn_ok.set(false);
            st.connecting.set(false);
            st.remote_list_pending.set(false);
            st.remote_stat_pending.set(false);
            set_remote_sensitive(st, false);
            if let Some(b) = st.rreconnect.borrow().as_ref() {
                b.set_sensitive(true);
            }
            set_remote_status(st, State::Err, tr("连接已断开"));
        }
        PanelEvent::Connected(Ok(cwd)) => {
            // 重连会 abort 旧传输调度器，先把旧队列里未完成的项标成取消
            cancel_aborted_xfers(st);
            st.conn_ok.set(true);
            st.connecting.set(false);
            st.remote_mtime.set(0);
            st.remote_stat_pending.set(false);
            st.remote_list_pending.set(false);
            *st.remote_home.borrow_mut() = Some(cwd.clone());
            set_connected_status(st, &cwd);
            set_remote_sensitive(st, true);
            // ⚠️ **阶段 99：先重试"因为断线失败的那条操作"**——
            // 用户原话：「重连一次……否则就继续执行原来的操作」。
            //  重试本身就是"继续原操作"（比如点开的那个目录）⇒ 不用再走
            //  下面"回到原目录"那条路（否则会连着列两次）。
            if run_pending_retry(st) {
                return;
            }
            // **连上之后去哪儿**：
            // ① 用户刚才输入了路径（当时没连着）⇒ 跳那个路径；
            // ② 否则回到**断之前那个目录**（重连不该把用户踢回主目录）；
            // ③ 第一次连接（还没待过任何目录）⇒ 主目录。
            let pending = st.pending_cd.borrow_mut().take();
            let last = st.remote_cwd.borrow().clone();
            let target = pending
                .filter(|p| !p.trim().is_empty())
                .or_else(|| if last.is_empty() { None } else { Some(last) })
                .unwrap_or(cwd);
            submit_list(st, &target);
        }
        PanelEvent::Connected(Err(err)) => {
            // 旧连接已经断开，在跑的传输不可能完成
            cancel_aborted_xfers(st);
            st.conn_ok.set(false);
            st.connecting.set(false);
            // ⚠️ **阶段 99：重连本身失败了 ⇒ 「重连一次」的那道闸门作废**
            // （用户：「凡是 sftp 因为没连接的失败都要进行一次重连」）。
            //  闸门是用来防「重连成功 ⇒ 重试又失败 ⇒ 又重连」的无限循环的；
            //  **重连没成功**就没进入那个循环 ⇒ 不该占着用户的额度，
            //  否则用户下一次点目录只会看到一个错、连重连都不会发起。
            st.retried.set(false);
            st.remote_mtime.set(0);
            st.remote_stat_pending.set(false);
            st.remote_list_pending.set(false);
            set_remote_sensitive(st, false);
            if let Some(b) = st.rreconnect.borrow().as_ref() {
                b.set_sensitive(true);
            }

            let have_pw = st.pw.borrow().is_some() || st.opts.password.is_some();
            let have_key = st
                .opts
                .keyfile
                .as_deref()
                .map_or(false, |k| !k.is_empty());
            // ⚠️ **阶段 C8：本次运行内问过的密码**。
            //    「这次连接用的密码是从缓存里拿的」⇒ 缓存里那份**可能已经
            //    过期**（用户改过密码）⇒ 失败时要**重新问**，不能拿旧的
            //    死磕（死磕 = 无限重连）。
            let session_key =
                crate::pwcache::key(&st.opts.host, st.opts.port, &st.opts.user);
            let pw_from_cache =
                !have_pw && crate::pwcache::get(&session_key).is_some();
            let need_ask = !have_key
                && crate::i18n::looks_like_auth_failure(&err)
                && (!have_pw || pw_from_cache);
            if need_ask {
                // 无凭据且认证失败：询问密码后重试
                let msg = trf!("SFTP 连接 {v0}@{v1}:{v2} 需要密码：\n{err}", err = err, v0 = st.opts.user, v1 = st.opts.host, v2 = st.opts.port);
                let top = st.topwin();
                let st = Rc::clone(st);
                // ⚠️ **同一条会话只弹一个框**（SFTP / 终端 / 分屏三条
                //    连接会同时要密码 ⇒ 只弹一个，答案共享）。
                let key = session_key.clone();
                crate::pwcache::ask_once(
                    session_key,
                    move |pw| {
                        let st = st.clone();
                        match pw {
                            Some(pw) => {
                                *st.pw.borrow_mut() = Some(Zeroizing::new(pw.clone()));
                                // ⚠️ 阶段 C8：**写进会话级密码文件** ⇒
                                //    Linux 上终端的 ssh（它的 askpass 助手正
                                //    在等这个文件）能拿到**同一份**密码 ⇒
                                //    **两个平台都是「问一次」**。
                                let _ = crate::pwcache::write_session_file(&key, &pw);
                                set_remote_status(&st, State::Warn, tr("正在连接…"));
                                submit_connect(&st);
                            }
                            None => {
                                // ⚠️ **取消也要写一次**（空密码）⇒ 正在等的
                                //    askpass 助手立刻收到答复（不用干等
                                //    60 秒）⇒ ssh 认证失败而不是卡住。
                                let _ = crate::pwcache::write_session_file(&key, "");
                                // 状态栏只显示状态词；完整错误在弹窗与 tooltip 里
                                set_remote_status(&st, State::Err, tr("未连接"));
                            }
                        }
                    },
                    move |done| {
                        dialogs::ask_password(
                            top.as_ref(),
                            tr("SFTP 密码"),
                            &msg,
                            move |pw| done(pw),
                        );
                    },
                );
                return;
            }
            set_remote_status(&st, State::Err, tr("未连接"));
            // ⚠️ 复查 R1 / §4.2「错误恢复不闭环」：以前这个框只有一颗
            // 「关闭」⇒ 用户关掉弹窗只能干瞪眼，而「连接 → 重新连接 SFTP」
            // 就在两步之外，弹窗却不说。⇒ 给一颗**当场重试**的按钮。
            let st2 = Rc::clone(st);
            dialogs::show_error_with_retry(
                st.topwin().as_ref(),
                tr("SFTP 连接失败"),
                &err,
                tr("重试"),
                move || {
                    set_remote_status(&st2, State::Warn, tr("正在连接…"));
                    submit_connect(&st2);
                },
            );
        }
        PanelEvent::Forwards { views, failed } => {
            *st.fwd_views.borrow_mut() = views;
            // 开着的管理窗口跟着刷新（增删/启停后列表要立刻反映事件）
            fwdmgr::refresh(&st);
            // 活跃标记（阶段 57）：转发在跑/停了 → 树上的「运行中」跟着变
            refresh_active_marks();
            // 刷新状态栏：Connected 已设置过文案，这里补上转发摘要
            let detail = st.remote_home.borrow().clone().unwrap_or_default();
            set_connected_status(&st, &detail);
            if !failed.is_empty() {
                dialogs::show_error(
                    st.topwin().as_ref(),
                    tr("端口转发"),
                    &failed.join("\n"),
                );
            }
        }
        PanelEvent::Listed {
            id,
            path,
            silent,
            result,
            dir_mtime,
        } => {
            // 慢的旧回包可能带着旧目录；只认最后一次请求
            if id != st.last_list.get() {
                return;
            }
            st.remote_list_pending.set(false);
            match result {
            Ok(entries) => {
                // 阶段 99：这次成功了 ⇒ 「已重试过」的闸门清掉
                // （下一条操作断线了还能自愈）
                st.retried.set(false);
                *st.remote_cwd.borrow_mut() = path.clone();
                st.pane(true).set_cwd_entry(&path);
                let show_hidden = st.pane(true).show_hidden.get();
                pane::fill_model(&st.pane(true), show_hidden, pane::entry_rows(&entries));
                st.remote_mtime.set(dir_mtime);
                set_connected_status(st, &path);
            }
            Err(err) if !silent => {
                // ⚠️ **阶段 99：连接断了 ⇒ 重连一次 + 重试同一条操作**
                // （用户原话：「在异常处理程序中重连一次，如果还是失败就
                //  报错，否则就继续执行原来的操作」）。
                //  ⇒ 这一条**不弹错**：重连成功后会自动重列这个目录。
                //
                // ⚠️ **用失败事件自带的 `path`**（不是"记住的那条"）：记住的
                // 是最后一次发出去的列目录，中间插进来一条后台刷新（目录跟随
                // / mtime 变化）就会改写它 ⇒ 重试的会变成别的目录。
                if failure_is_conn_dead(st)
                    && schedule_retry_op(st, RetryOp::List { path: path.clone() })
                {
                    return;
                }
                dialogs::show_error(
                    st.topwin().as_ref(),
                    tr("读取远程目录失败"),
                    &trf!("{err}\n目录：{path}", err = err, path = path),
                );
            }
            _ => {}
            }
        }
        PanelEvent::DirStamp { path, mtime } => {
            st.remote_stat_pending.set(false);
            if path != *st.remote_cwd.borrow() {
                return;
            }
            if mtime == 0 {
                st.remote_mtime.set(0);
                return;
            }
            let last = st.remote_mtime.get();
            st.remote_mtime.set(mtime);
            if last != 0 && last != mtime {
                submit_list_full(st, &path, true);
            }
        }
        PanelEvent::SimpleDone { path, result } => {
            if result.is_ok() {
                // 阶段 99：成功了 ⇒ 清掉「已重试过」的闸门
                st.retried.set(false);
            }
            if let Err(err) = &result {
                // ⚠️ 阶段 99：连接断了 ⇒ 重连一次 + 重试同一条操作（不弹错）
                //
                // ⚠️ **核对是不是同一条**：失败事件只带 path（「重命名」要知道
                // 改成什么名 ⇒ 只能靠记住的那条）⇒ 两条操作都在路上时可能
                // 对不上 ⇒ 对不上就不重试（重试错的那条 = 替用户干了别的事）。
                if failure_is_conn_dead(st) {
                    if let Some(op) = take_retry_op(st, RetryKind::Simple, &path) {
                        if schedule_retry_op(st, op) {
                            return;
                        }
                    }
                }
                dialogs::show_error(st.topwin().as_ref(), tr("操作失败"), err);
            }
            // 上级目录可能被整体删除，列父目录更安全
            let parent = path_parent(&path);
            if parent == *st.remote_cwd.borrow() || result.is_err() {
                submit_list(st, &parent);
            } else {
                let cwd = st.remote_cwd.borrow().clone();
                submit_list(st, &cwd);
            }
        }
        PanelEvent::PathResolved { path, result } => {
            on_path_resolved(st, &path, result);
        }
        PanelEvent::FileRead { id, path, result } => {
            if result.is_ok() {
                st.retried.set(false);
            }
            // ⚠️ 阶段 99：连接断了 ⇒ 重连一次 + 重发这次读（不弹错）
            //
            // ⚠️ **核对是不是同一条**：两个文件同时在读、记住的却是另一个 ⇒
            // 重发错的那个等于替用户读了另一个文件。
            if result.is_err() && failure_is_conn_dead(st) {
                if let Some(op) = take_retry_op(st, RetryKind::ReadFile, &path) {
                    if schedule_retry_op(st, op) {
                        return;
                    }
                }
            }
            fileedit::on_read(st, id, &path, result);
        }
        PanelEvent::FileWritten { id, path, result } => {
            if result.is_ok() {
                st.retried.set(false);
            }
            // ⚠️ 阶段 99：连接断了 ⇒ 重连一次 + 重发这次写（不弹错）
            //
            // ⚠️ **核对是不是同一条**（同上：写文件要知道内容 ⇒ 只能靠记住
            // 的那条）⇒ 对不上就不重试（写错的文件比不写更糟）。
            if result.is_err() && failure_is_conn_dead(st) {
                if let Some(op) = take_retry_op(st, RetryKind::WriteFile, &path) {
                    if schedule_retry_op(st, op) {
                        return;
                    }
                }
            }
            fileedit::on_written(st, id, &path, result);
        }
        PanelEvent::FollowDone(path) => {
            st.follow_pending.set(false);
            match path {
                Some(p) => follow_apply(st, &p),
                None => {
                    let n = st.exec_fail.get() + 1;
                    st.exec_fail.set(n);
                    if n > 15 {
                        st.follow_poll_dead.set(true);
                        eprintln!("{}", tr("PuTTYPlus: 远程 exec 通道不可用，目录跟随轮询已停止\
                             （shell 若支持 OSC 7 仍可跟随）"));
                    }
                }
            }
        }
        PanelEvent::CheckExistsDone { id, exists } => {
            let Some(pending) = st.pending_uploads.borrow_mut().remove(&id) else {
                return;
            };
            let conflicts: Vec<String> = pending
                .iter()
                .zip(exists.iter())
                .filter(|(_, ex)| **ex)
                .map(|((src, dst_dir, _), _)| {
                // 上传：dst_dir 是**远端**目录，src 是**本地**路径
                path_join(dst_dir, &localpath::basename(src))
            })
                .collect();
            ask_then_submit(st, pending, conflicts);
        }
        PanelEvent::MirrorPlan {
            local,
            remote,
            result,
        } => {
            mirrorui::on_scan_result(st, local, remote, result);
        }
        PanelEvent::RelayDone { id: _, ok, msg } => {
            // 不占用远程状态行/标签状态点（那是连接状态，别被一次传输失败染红）
            if !ok {
                super::panel::dialogs::show_error(
                    st.topwin().as_ref(),
                    tr("跨服务器传输失败"),
                    &msg,
                );
            }
        }
        PanelEvent::DbConnected(result) => {
            dbui::on_connected(st, result);
            // 活跃标记（阶段 57）：连上了 → 树上点亮
            refresh_active_marks();
        }
        // ⚠️ 测连的回包：**不是**给工作台的，是给「新建数据库连接」
        //    那个对话框的（它要留在原地、在密码框那儿提示）。
        PanelEvent::DbTest {
            id,
            result,
            needs_password,
        } => {
            let cb = st.db_test_cb.borrow_mut().take();
            if let Some((cid, c)) = cb {
                if cid == id {
                    c(result, needs_password);
                }
            }
        }
        PanelEvent::DbNeedPassword(detail) => {
            dbui::on_need_password(st, detail);
            refresh_active_marks();
        }
        PanelEvent::DbRows { id, rows } => dbui::on_rows(st, id, rows),
        PanelEvent::DbError(err) => dbui::on_error(st, err),
        // ⚠️ **阶段 99：数据库连接断了** ⇒ 重连一次 + 重试刚才那条操作
        // （不是普通失败：普通失败只能报错，断了能自愈）
        PanelEvent::DbLost { id } => dbui::on_lost(st, id),
        PanelEvent::DbClosed => {
            dbui::on_closed(st);
            // 活跃标记（阶段 57）：断开了 → 树上灭掉
            refresh_active_marks();
        }
        PanelEvent::DbMeta { id, req, result } => dbui::on_meta(st, id, req, result),
        PanelEvent::DbExec { id, result } => dbui::on_exec(st, id, result),
        PanelEvent::DbTx { id, op, result } => dbui::on_tx(st, id, op, result),
        PanelEvent::DbImport { id, result } => dbui::on_import_result(st, id, result),
        PanelEvent::DbEr { id, result } => dbui::on_er(st, id, result),
        PanelEvent::DbExport { id, result } => dbui::on_export(st, id, result),
        PanelEvent::XferTick => {
            queue::flush(st);
        }
        PanelEvent::XferDrained { results } => {
            // 队列空了 ⇒ 忙态与角标要撤掉（阶段 102）
            queue::refresh_btn(&st);
            // 呼吸动画也停掉（没东西在传了 ⇒ 别占 CPU）
            sync_xfer_anim(&st);
            // 整个传输队列清空（不是每个提交批次）→ 通知 + 有失败/取消时
            // 汇总一次。追加的新批次在同一个窗口里，不会提前弹。
            pane::clear_progress(&st.pane(true));
            pane::clear_progress(&st.pane(false));
            // 传输完成**只发通知，不弹窗**（用户 2026-10-27 明确要求）。
            // 桌面通知不可用时在状态栏写一句——不打扰，但也不让人完全
            // 不知道传完了。
            let level = if results.iter().any(|(_, s)| *s == XferState::Err) {
                State::Warn
            } else {
                State::Ok
            };
            let st2 = Rc::clone(st);
            crate::notify::report_batch(&results, move |msg| {
                set_remote_status(&st2, level, msg);
            });
            let cwd = st.remote_cwd.borrow().clone();
            if st.conn_ok.get() {
                submit_list(st, &cwd);
            }
            fill_local_pane(st);
        }
    }
}

/* ---------------- 目录跟随 ---------------- */

fn follow_apply(st: &Rc<PanelState>, path: &str) {
    if !st.follow.get() || !st.conn_ok.get() || path.is_empty() {
        return;
    }
    // follow_last 记录的是"shell 上次被观察到的目录"（OSC 7 与轮询都汇聚到这里）。
    // 只有 shell 真的移动了才切换面板，用户的手动浏览不受轮询影响
    if st.follow_last.borrow().as_deref() == Some(path) {
        return;
    }
    *st.follow_last.borrow_mut() = Some(path.to_string());
    if *st.remote_cwd.borrow() == path {
        return;
    }
    submit_list_full(st, path, true);
}

fn follow_poll_cb(st: &Rc<PanelState>) -> glib::ControlFlow {
    if st.dead.get() || st.follow_poll_dead.get() {
        return glib::ControlFlow::Break;
    }
    if !st.follow.get() || !st.conn_ok.get() || st.follow_pending.get() {
        return glib::ControlFlow::Continue;
    }
    st.follow_pending.set(true);
    let spawn_wall = st.term_spawn_wall.get();
    if spawn_wall == 0 {
        st.follow_pending.set(false);
        return glib::ControlFlow::Continue;
    }
    let age = chrono::Local::now().timestamp() - spawn_wall;
    let _ = st
        .tx
        .borrow()
        .as_ref()
        .unwrap()
        .send(WorkerCmd::Follow { age_sec: age });
    glib::ControlFlow::Continue
}

/// 远端目录自动刷新：每 2.5 秒 stat 一次当前目录的 mtime。文件增删/改名会
/// 更新目录 mtime；文件内容修改不会（那是文件自己的 mtime），后者仍靠手动
/// 刷新或重新进入目录。传输进行中跳过轮询，避免和队列刷新打架。
fn remote_watch_cb(st: &Rc<PanelState>) -> glib::ControlFlow {
    if st.dead.get() {
        return glib::ControlFlow::Break;
    }
    if !st.conn_ok.get()
        || st.remote_stat_pending.get()
        || st.remote_list_pending.get()
    {
        return glib::ControlFlow::Continue;
    }
    let busy = st.items.borrow().iter().any(|it| {
        matches!(
            it.shared.lock().unwrap().state,
            XferState::Wait | XferState::Run
        )
    });
    if busy {
        return glib::ControlFlow::Continue;
    }
    let cwd = st.remote_cwd.borrow().clone();
    if !cwd.starts_with('/') {
        return glib::ControlFlow::Continue;
    }
    st.remote_stat_pending.set(true);
    let sent = st
        .tx
        .borrow()
        .as_ref()
        .map(|tx| tx.send(WorkerCmd::DirStamp { path: cwd }).is_ok())
        .unwrap_or(false);
    if !sent {
        st.remote_stat_pending.set(false);
    }
    glib::ControlFlow::Continue
}

/* ---------------- 面板操作回调 ---------------- */

/// 编辑器保存成功后刷新对应面板：大小/修改时间变了，列表不该停在旧值。
pub(crate) fn refresh_after_edit_save(st: &Rc<PanelState>, remote: bool) {
    if !remote || st.conn_ok.get() {
        on_refresh(st, remote);
    }
}

pub fn on_refresh(st: &Rc<PanelState>, remote: bool) {
    if remote {
        // ⚠️ **断了就先重连**（用户原话：「刷新的时候，发现 sftp断了就自动重连」）
        if !ensure_remote(st) {
            return;
        }
        let cwd = st.remote_cwd.borrow().clone();
        submit_list(st, &cwd);
    } else {
        fill_local_pane(st);
    }
}

/// 文件面板右键「取消传输」：选中项里有普通上传/下载在等待或运行时，
/// 取消对应源文件的传输。目录递归传输的 src 就是目录本身，也能匹配。
pub fn on_cancel_transfers(st: &Rc<PanelState>, remote: bool) {
    let sel = st.pane(remote).selected(&st.pane_cwd(remote));
    if sel.is_empty() {
        return;
    }
    let mut canceled = false;
    {
        for it in st.items.borrow().iter() {
            if it.kind != XferKind::Transfer || (!it.upload) != remote {
                continue;
            }
            if !sel.iter().any(|(p, _)| p == &it.src) {
                continue;
            }
            let mut g = it.shared.lock().unwrap();
            match g.state {
                XferState::Run => {
                    g.cancel = true;
                    g.cancel_flag
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    canceled = true;
                }
                XferState::Wait => {
                    // 等待中的任务调度器仍会启动；cancel=true 让它在
                    // run_transfer 开头直接退出，状态立即显示为取消
                    g.cancel = true;
                    g.state = XferState::Cancel;
                    canceled = true;
                }
                _ => {}
            }
        }
    }
    if canceled {
        queue::flush(st);
    }
}

/// 文件面板右键「打开」：只能作用于单个文件（目录不进文本编辑器）。
///
/// `readonly` = 只读打开（只看不改）。菜单里的「打开」是可改可存的；
/// 只读模式仍留着给内部调用（非法 UTF-8 之类存不回去的情况由编辑器自己降级）。
pub fn on_open_file(st: &Rc<PanelState>, remote: bool, readonly: bool) {
    let sel = st.pane(remote).selected(&st.pane_cwd(remote));
    match sel.as_slice() {
        [] => dialogs::show_error(
            st.topwin().as_ref(),
            tr("无法打开文件"),
            tr("请先在文件列表里选择一个文件。"),
        ),
        [(path, false)] => {
            if remote {
                fileedit::open_remote(st, path, readonly);
            } else {
                fileedit::open_local(st, path, readonly);
            }
        }
        [(path, true)] => dialogs::show_error(
            st.topwin().as_ref(),
            tr("无法打开文件"),
            &trf!("「{v0}」是目录，内置查看/编辑器只处理文本文件。", v0 = if remote {
                path_basename(path).to_string()
            } else {
                localpath::basename(path)
            }),
        ),
        _ => dialogs::show_error(
            st.topwin().as_ref(),
            tr("无法打开文件"),
            &trf!("一次只能查看/编辑一个文件（已选中 {v0} 项）。", v0 = sel.len()),
        ),
    }
}

pub fn on_up(st: &Rc<PanelState>, remote: bool) {
    let cwd = st.pane_cwd(remote);
    if remote {
        // ⚠️ **远端路径恒为 POSIX**（`/srv/x`）⇒ 用 POSIX 的 parent。
        let parent = path_parent(&cwd);
        if !cwd.starts_with('/') {
            return; // ~ 相对路径保护
        }
        // 断了就先重连（连上之后会回到原来的目录）
        if !ensure_remote(st) {
            return;
        }
        submit_list(st, &parent);
    } else {
        // ⚠️ **本地路径在 Windows 上是 `C:\dir`** ⇒ 必须用本地语义的
        // parent（POSIX 那个在 Windows 上会直接跳到文件系统根）。
        let parent = localpath::parent(&cwd);
        set_local_cwd(st, &parent);
        fill_local_pane(st);
    }
}

pub fn on_home(st: &Rc<PanelState>, remote: bool) {
    if remote {
        // 断了就先重连
        if !ensure_remote(st) {
            return;
        }
        let home = st.remote_home.borrow().clone();
        if let Some(h) = home {
            submit_list(st, &h);
        }
    } else {
        set_local_cwd(st, &glib::home_dir().to_string_lossy());
        fill_local_pane(st);
    }
}

pub fn on_entry_activated(st: &Rc<PanelState>, remote: bool, txt: &str) {
    if txt.is_empty() {
        return;
    }
    if remote {
        let home = st.remote_home.borrow().clone();
        let target = match (&home, txt.strip_prefix('~')) {
            (Some(h), Some(rest)) if rest.is_empty() => h.clone(),
            (Some(h), Some(rest)) => path_join(h, rest.trim_start_matches('/')),
            _ => txt.to_string(),
        };
        // ⚠️ **断了就先重连**，并把"要去哪儿"记下来 ⇒ 连上之后自动跳过去
        // （用户原话：「改变目录的时候……发现 sftp断了就自动重连」）
        if !ensure_remote(st) {
            *st.pending_cd.borrow_mut() = Some(target);
            return;
        }
        submit_list(st, &target);
    } else {
        let p = std::path::Path::new(txt);
        if p.is_dir() {
            set_local_cwd(st, txt);
        }
        fill_local_pane(st); // 失败时 fill 会把 entry 改回 cwd
    }
}

pub fn on_row_activated(st: &Rc<PanelState>, remote: bool, name: &str, isdir: bool, islink: bool) {
    if name == ".." {
        // 列表首行的上级导航条目（与原工具栏「上级」同语义）
        on_up(st, remote);
        return;
    }
    let cwd = st.pane_cwd(remote);
    // ⚠️ 远端用 POSIX 拼接，本地用本地语义（Windows 上是 `C:\dir`）。
    let full = if remote {
        path_join(&cwd, name)
    } else {
        localpath::join(&cwd, name)
    };
    if isdir {
        if remote {
            submit_list(st, &full);
        } else {
            set_local_cwd(st, &full);
            fill_local_pane(st);
        }
    } else if remote && islink {
        // ⚠️ **软链**：列表里它是「软链自己」（不是目录）⇒ 不能直接当
        //    文件开编辑器（服务端 stat 会说「这是目录」⇒ 弹报错 + 留一个
        //    空编辑器页）。⇒ **先问一句它指向什么**：
        //    - 指向目录 ⇒ **跟进去**（列它的内容）；
        //    - 指向文件 ⇒ 照常开编辑器；
        //    - 问不出来（断链/没权限）⇒ 弹一句提示，**不开编辑器**。
        resolve_remote_link(st, &full);
    } else if remote {
        // 双击 → **编辑**（阶段 23）：不是下载。
        //
        // **为什么改**：双击一个文件，用户想的是"打开看看/改改"，不是
        // "把它拉到本地去"。下载/上传挪到工具栏那颗常驻按钮和右键菜单里了
        // （「下载…」/「上传…」），没丢功能。
        fileedit::open_remote(st, &full, false);
    } else {
        fileedit::open_local(st, &full, false);
    }
}

/// 问一句远端这个软链指向什么，再决定「进目录」还是「开编辑器」。
///
/// ⚠️ **为什么不能直接在 UI 线程问**：SFTP 连接**独占**在 worker 线程上
/// ⇒ 只能发命令、等回包（`PanelEvent::PathResolved`）。
fn resolve_remote_link(st: &Rc<PanelState>, path: &str) {
    let _ = st.tx.borrow().as_ref().map(|tx| {
        tx.send(WorkerCmd::ResolvePath {
            path: path.to_string(),
        })
    });
}

/// 回包：软链指向目录 ⇒ 跟进去；指向文件 ⇒ 开编辑器；问不出来 ⇒ 只弹提示。
fn on_path_resolved(st: &Rc<PanelState>, path: &str, result: Result<crate::backend::PathKind, String>) {
    use crate::backend::PathKind;
    // ⚠️ **防陈旧回包**：用户双击软链之后可能已经翻到别的目录了 ⇒
    //    这时再把面板拽回旧路径会莫名其妙。⇒ 只处理「还在这一层」的。
    let parent = crate::util::path_parent(path);
    if st.pane_cwd(true) != parent {
        return;
    }
    match result {
        Ok(PathKind::Dir) => {
            // 跟进去：列这个软链指向的目录（服务端自己会解析软链）
            submit_list(st, path);
        }
        Ok(PathKind::File) => {
            fileedit::open_remote(st, path, false);
        }
        Ok(PathKind::Other) => {
            // ⚠️ 复用**已有的**文案（别新增英文表条目：表是自动生成的，
            //    手工加会打乱排序 ⇒ 要加得整表重新生成）。
            dialogs::show_error(
                st.topwin().as_ref(),
                tr("无法打开文件"),
                &trf!("{path} 不是普通文件，内置查看/编辑器只处理文本文件", path = path),
            );
        }
        Err(err) => {
            // ⚠️ **断链 / 没权限**：只弹提示，**不留编辑器页**
            dialogs::show_error(
                st.topwin().as_ref(),
                tr("无法打开文件"),
                &format!("{path}\n{err}"),
            );
        }
    }
}

pub fn on_mkdir(st: &Rc<PanelState>, remote: bool) {
    let top = st.topwin();
    let title = if remote { tr("新建远程目录") } else { tr("新建本地目录") };
    let st = Rc::clone(st);
    dialogs::ask_text(top.as_ref(), title, "", move |name| {
        let st = st.clone();
        let Some(name) = name else { return };
        let cwd = st.pane_cwd(remote);
        // ⚠️ 远端 POSIX 拼接 / 本地本地语义（Windows 上是 `C:\dir`）。
        let full = if remote {
            path_join(&cwd, &name)
        } else {
            localpath::join(&cwd, &name)
        };
        if remote {
            submit_simple(&st, SimpleOp::Mkdir, &full, None);
        } else {
            if let Err(e) = std::fs::create_dir(&full) {
                dialogs::show_error(
                    st.topwin().as_ref(),
                    tr("创建目录失败"),
                    &format!("{e}: {full}"),
                );
            }
            fill_local_pane(&st);
        }
    });
}

pub fn on_rename(st: &Rc<PanelState>, remote: bool) {
    let sel = st.pane(remote).selected(&st.pane_cwd(remote));
    if sel.len() != 1 {
        return;
    }
    let (old, _) = sel[0].clone();
    // ⚠️ 本地路径在 Windows 上是 `C:\dir\a.txt` ⇒ POSIX 的 basename 会把
    // 整串当成文件名（重命名对话框里预填成完整路径）。
    let oldname = if remote {
        path_basename(&old).to_string()
    } else {
        localpath::basename(&old)
    };
    let top = st.topwin();
    let title = if remote { tr("重命名（远程）") } else { tr("重命名（本地）") };
    let st = Rc::clone(st);
    let defval = oldname.clone();
    dialogs::ask_text(top.as_ref(), title, &defval, move |newname| {
        let st = st.clone();
        let Some(newname) = newname else { return };
        if newname.is_empty() || newname == oldname {
            return;
        }
        let cwd = st.pane_cwd(remote);
        if remote {
            let full_new = path_join(&cwd, &newname);
            submit_simple(&st, SimpleOp::Rename, &old, Some(&full_new));
        } else {
            // ⚠️ **目标要带目录**：只给新名字的话，重命名会落到「进程当前
            // 工作目录」而不是面板当前目录（老 bug，Linux 上也一样）。
            let full_new = localpath::join(&cwd, &newname);
            if let Err(e) = std::fs::rename(&old, &full_new) {
                dialogs::show_error(st.topwin().as_ref(), tr("重命名失败"), &e.to_string());
            }
            fill_local_pane(&st);
        }
    });
}

pub fn on_delete(st: &Rc<PanelState>, remote: bool) {
    let sel = st.pane(remote).selected(&st.pane_cwd(remote));
    if sel.is_empty() {
        return;
    }
    let n = sel.len();
    let msg = trf!("确定删除选中的 {n} 项？\n{v0}", n = n, v0 = if remote {
            tr("远程删除不可恢复，目录将递归删除。")
        } else {
            tr("本地文件优先移入回收站（不可用时直接删除）。")
        });
    let top = st.topwin();
    let st = Rc::clone(st);
    dialogs::confirm(top.as_ref(), &msg, move |ok| {
        let st = st.clone();
        if !ok {
            return;
        }
        if remote {
            for (p, _) in sel {
                submit_simple(&st, SimpleOp::Remove, &p, None);
            }
        } else {
            for (p, _) in &sel {
                let f = gtk::gio::File::for_path(p);
                // 本地优先进回收站；失败详情取最后一个错误（与 C 版一致）
                let mut last_err = None;
                if let Err(e) = f.trash(None::<&gtk::gio::Cancellable>) {
                    last_err = Some(e);
                    if let Err(e2) = f.delete(None::<&gtk::gio::Cancellable>) {
                        last_err = Some(e2);
                    }
                }
                if let Some(e) = last_err {
                    dialogs::show_error(
                        st.topwin().as_ref(),
                        tr("删除失败"),
                        &format!("{p}: {}", e.message()),
                    );
                }
            }
            fill_local_pane(&st);
        }
    });
}

pub fn on_transfer_btn(st: &Rc<PanelState>, remote: bool) {
    let sel = st.pane(remote).selected(&st.pane_cwd(remote));
    let paths: Vec<String> = sel.into_iter().map(|(p, _)| p).collect();
    if paths.is_empty() {
        return;
    }
    if remote {
        start_downloads(st, paths);
    } else {
        start_uploads(st, paths);
    }
}

pub fn on_reconnect(st: &Rc<PanelState>) {
    if let Some(b) = st.rreconnect.borrow().as_ref() {
        b.set_sensitive(false);
    }
    st.connecting.set(true);
    set_remote_status(
        st,
        State::Warn,
        &trf!("正在连接 {v0}@{v1}:{v2}…", v0 = st.opts.user, v1 = st.opts.host, v2 = st.opts.port),
    );
    // worker 侧 Connect 会先关闭旧连接
    submit_connect(st);
}

pub fn on_open_queue(st: &Rc<PanelState>) {
    queue::ensure(st);
    if let Some(qu) = st.queue_ui.borrow().as_ref() {
        qu.win.present();
    }
}

/// 转发管理窗口入口（会话菜单 / 标签右键菜单）。
pub fn on_open_forwards(st: &Rc<PanelState>) {
    fwdmgr::ensure(st);
}

/// 数据库窗口入口（会话菜单 / 标签右键菜单 / Ctrl+Shift+D）。
///
/// 阶段 22：打开工作台**并且连上当前连接**（上次用的那条）——工作台里已经
/// 没有「连接」按钮了，点这里就该直接能用。
///
/// ⚠️ **工作台页面要等连上之后才出现**（用户 2026-11-02：「任何时候，必须
/// 连上数据库之后才允许显示数据库管理页面」）⇒ 这里只是**发起连接**，
/// 页面由 `dbui::mount` 在连上之后挂出来。连不上时弹对话框说清楚。
pub fn on_open_db(st: &Rc<PanelState>) {
    // ⚠️ **Windows 上先拦下来**（跨平台版 阶段 C5）：首版承诺的范围是
    //    「终端 + SFTP + 命令行契约」，DB 工作台在 Windows 上只保证能编译、
    //    没实测 ⇒ 这里弹一句提示就返回。
    #[cfg(windows)]
    {
        let _ = st;
        db_unavailable();
        return;
    }
    #[cfg(not(windows))]
    {
        if !check_db_channel(st) {
            return;
        }
        dbui::ensure(st);
        dbui::connect_current(st);
    }
}

/// Windows 上数据库工作台未验证 ⇒ 统一的一句话提示。
///
/// ⚠️ 入口有好几个（顶栏菜单、侧栏、「新建」菜单、标签页「+」），
///    逐个藏容易漏 ⇒ 都在这里收口。
#[cfg(windows)]
fn db_unavailable() {
    dialogs::info(
        None,
        "数据库工作台",
        "数据库工作台的 Windows 版尚未验证，首版暂不开放。",
    );
}

/// 只**准备**数据库工作台，**不连**。
///
/// ⚠️ 页面**不会**在这里出现（用户 2026-11-02：没连上不许显示数据库管理
/// 页面）——这里只是把工作台控件建好，等 `use_profile` 连上之后才挂出来。
///
/// 双击某条具体的数据库连接时走这条：那条连接由 `dbui::use_profile`
/// 选中并连上——要是这里先连了"上次用的那条"，就得连错一次再断开重连。
pub fn on_open_db_page(st: &Rc<PanelState>) {
    // ⚠️ 同 `on_open_db`：Windows 上这个入口也关着（见那里）。
    #[cfg(windows)]
    {
        let _ = st;
        db_unavailable();
        return;
    }
    #[cfg(not(windows))]
    {
        if !check_db_channel(st) {
            return;
        }
        dbui::ensure(st);
    }
}

/// 数据库通道只支持 SSH 会话：通道要挂在一条已认证的 SSH 连接上。
fn check_db_channel(st: &Rc<PanelState>) -> bool {
    if st.opts.is_ssh() {
        return true;
    }
    dialogs::show_error(
        st.topwin().as_ref(),
        tr("数据库"),
        tr("数据库通道只支持 SSH 会话：当前协议没有可复用的 SSH 连接。"),
    );
    false
}

/// 会话选项入口（会话菜单 / 标签右键菜单）：保活/字号/响铃的会话级覆盖。
pub fn on_open_session_opts(st: &Rc<PanelState>) {
    sessopt::open(st);
}

/// 快速命令入口（标签头按钮 / Ctrl+Shift+K）：把命令敲进 `term` 那个终端。
pub fn on_open_quickcmds(st: &Rc<PanelState>, term: &crate::termview::TermView, anchor: &gtk::Widget) {
    quickcmd::open_popover(st, term, anchor);
}

/* ---------------- 传输 ---------------- */

/// 传输唯一入口：先做目标冲突预检，再询问策略/直接提交。
fn submit_transfers(st: &Rc<PanelState>, specs: Vec<(String, String, bool)>) {
    if specs.is_empty() {
        return;
    }
    let all_upload = specs.iter().all(|(_, _, up)| *up);
    let all_download = specs.iter().all(|(_, _, up)| !*up);
    if all_download {
        // 本地目标：UI 线程同步预检
        let dst_dir = &specs[0].1;
        let conflicts: Vec<String> = specs
            .iter()
            // 下载：dir 是**本地**目录，src 是**远端**路径
            .map(|(src, dir, _)| localpath::join(dir, &path_basename(src)))
            .filter(|p| std::path::Path::new(p).is_file())
            .collect();
        ask_then_submit(st, specs, conflicts);
    } else if all_upload {
        // 远端目标：两阶段——先发预检命令，结果回来后询问
        let paths: Vec<String> = specs
            .iter()
            // 上传：dir 是**远端**目录，src 是**本地**路径
            .map(|(src, dir, _)| path_join(dir, &localpath::basename(src)))
            .collect();
        let check = st.next_check.get();
        st.next_check.set(check + 1);
        st.pending_uploads
            .borrow_mut()
            .insert(check, specs.clone());
        let _ = st
            .tx
            .borrow()
            .as_ref()
            .unwrap()
            .send(WorkerCmd::CheckExists { id: check, paths });
    } else {
        // 混合批次当前不会出现（上传/下载分开入口），防御性直接传
        do_submit(st, &specs, ConflictPolicy::All);
    }
}

/// 一批传输的冲突策略（阶段 79）。
///
/// 逐个确认时是「目标路径 → 覆盖？」的映射。
enum ConflictPolicy {
    /// 全部覆盖。
    All,
    /// 全部跳过已存在的。
    Skip,
    /// 逐个决定。
    PerItem(HashMap<String, bool>),
}

impl ConflictPolicy {
    /// 这个目标路径该怎么办。
    fn decide(&self, dest: &str) -> worker::XferConflict {
        match self {
            ConflictPolicy::All => worker::XferConflict::Overwrite,
            ConflictPolicy::Skip => worker::XferConflict::Skip,
            ConflictPolicy::PerItem(m) => match m.get(dest) {
                Some(true) => worker::XferConflict::Overwrite,
                Some(false) => worker::XferConflict::Skip,
                // 没冲突过 ⇒ 老样子（覆盖）
                None => worker::XferConflict::Overwrite,
            },
        }
    }
}

/// 冲突询问后按策略提交；conflicts 为空则直接提交（零打扰）。
fn ask_then_submit(
    st: &Rc<PanelState>,
    specs: Vec<(String, String, bool)>,
    conflicts: Vec<String>,
) {
    if conflicts.is_empty() {
        do_submit(st, &specs, ConflictPolicy::All);
        return;
    }
    let top = st.topwin();
    let st2 = Rc::clone(st);
    let conflicts2 = conflicts.clone();
    // ⚠️ 正文（冲突数量 + 示例）由 `dialogs::ask_conflict` 统一生成 ⇒
    // 只有一份说法（阶段 79）。
    dialogs::ask_conflict(top.as_ref(), &conflicts, move |choice| {
        let st = st2.clone();
        let Some(c) = choice else { return }; // 取消整批：什么都不提交
        let policy = match c {
            dialogs::ConflictChoice::All => ConflictPolicy::All,
            dialogs::ConflictChoice::Skip => ConflictPolicy::Skip,
            dialogs::ConflictChoice::PerItem(v) => {
                // 结果与冲突列表**一一对应** ⇒ 拼成「路径 → 覆盖？」
                let mut m: HashMap<String, bool> = HashMap::new();
                for (path, over) in conflicts2.iter().zip(v) {
                    m.insert(path.clone(), over);
                }
                ConflictPolicy::PerItem(m)
            }
        };
        do_submit(&st, &specs, policy);
    });
}

/// 策略确定后的实际提交。
fn do_submit(
    st: &Rc<PanelState>,
    specs: &[(String, String, bool)],
    policy: ConflictPolicy,
) {
    let mut items_ui = Vec::new();
    let mut specs_w = Vec::new();
    for (src, dst_dir, upload) in specs {
        let id = st.next_id.get();
        st.next_id.set(id + 1);
        let shared = worker::XferShared::new();
        items_ui.push(QueueItem {
            id,
            src: src.clone(),
            dst_dir: dst_dir.clone(),
            upload: *upload,
            kind: worker::XferKind::Transfer,
            shared: shared.clone(),
        });
        specs_w.push(XferSpec {
            id,
            src: src.clone(),
            dst_dir: dst_dir.clone(),
            upload: *upload,
            kind: worker::XferKind::Transfer,
            // ⚠️ 逐个确认时**每个文件**按自己的决定来（阶段 79）
            // ⚠️ 目标路径按方向拼：上传 = 远端目录 + 本地文件名；
            //    下载 = 本地目录 + 远端文件名。
            conflict: policy.decide(&if *upload {
                path_join(dst_dir, &localpath::basename(src))
            } else {
                localpath::join(dst_dir, &path_basename(src))
            }),
            shared,
        });
    }
    {
        let mut items = st.items.borrow_mut();
        for it in &items_ui {
            items.push(QueueItem {
                id: it.id,
                src: it.src.clone(),
                dst_dir: it.dst_dir.clone(),
                upload: it.upload,
                kind: it.kind,
                shared: it.shared.clone(),
            });
        }
    }
    for it in &items_ui {
        queue::add_item(st, it);
    }
    let _ = st
        .tx
        .borrow()
        .as_ref()
        .unwrap()
        .send(WorkerCmd::Transfers { items: specs_w });
}

pub fn start_uploads(st: &Rc<PanelState>, lpaths: Vec<String>) {
    if !st.conn_ok.get() {
        dialogs::show_error(st.topwin().as_ref(), tr("未连接"), tr("SFTP 尚未连接，无法上传。"));
        return;
    }
    let dst = st.remote_cwd.borrow().clone();
    submit_transfers(
        st,
        lpaths.into_iter().map(|p| (p, dst.clone(), true)).collect(),
    );
}

/// `gdk::FileList` → 本地路径列表。
///
/// **为什么抽成独立函数**：GTK **没有"模拟一次拖放"的 API**——
/// drop 回调只有真的拖一次才会触发，手工拖没法自动化验证。把"取路径"
/// 和"发起上传"拆开之后，冒烟可以直接调同一段代码验
/// （见 `drop_upload_files`）。
pub use dnd::filelist_paths;

/// 拖拽上传的**实际动作**：把 FileList 里的文件传到当前远程目录。
///
/// 返回 false 表示"这次拖拽没东西可传"（调用方据此拒绝这次 drop）。
///
/// 目标目录 = **面板当前浏览的远程目录**（`start_uploads` 自己取
/// `remote_cwd`）——拖到终端上的文件落在用户正看着的那个目录里，
/// 跟拖到远程文件列表是一个语义。
pub fn drop_upload_files(st: &Rc<PanelState>, fl: &gdk::FileList) -> bool {
    let paths = filelist_paths(fl);
    if paths.is_empty() {
        return false;
    }
    start_uploads(st, paths);
    true
}

/// 拖拽上传的入口（按**路径列表**）。
///
/// ⚠️ **空列表不能静默吞掉**：拖进来却什么都没发生，是这个功能历史上
/// 最容易被当成「坏了」的表现（用户完全看不出到底是拖放没触发，还是
/// 传完了但界面没刷新）。所以这里给一句人话提示。
pub fn drop_upload_paths(st: &Rc<PanelState>, paths: Vec<String>) -> bool {
    if paths.is_empty() {
        set_remote_status(
            st,
            crate::theme::State::Warn,
            &tr("拖进来的东西里没有可上传的本地文件"),
        );
        return false;
    }
    start_uploads(st, paths);
    true
}

/// 一次拖放的落地处理：**拿到路径就传，没拿到就说清楚**。
///
/// ⚠️ 「没拿到数据」必须**说出来**：X11 上源端可以接了拖拽却never应答，
/// 这时用户看到的现象和「拖拽根本没触发」一模一样 —— 不提示的话，用户
/// 只会一次次重拖，然后以为功能坏了。
pub fn on_drop_result(st: &Rc<PanelState>, res: dnd::DropResult) {
    match res {
        dnd::DropResult::Paths(paths) => {
            drop_upload_paths(st, paths);
        }
        dnd::DropResult::NoData => {
            set_remote_status(
                st,
                crate::theme::State::Warn,
                &tr("没拿到文件数据，请改用「上传…」"),
            );
        }
    }
}

/// 给终端挂上「**拖文件进来 = 上传到当前远程目录**」。
///
/// ⚠️ **只接本地文件**（`text/uri-list`）：VTE 自己的拖放是**拖选
/// 文本**（接 String），两者类型不同、互不抢——从终端往外拖文本到编辑器
/// 的老功能不受影响。
///
/// ⚠️ 闭包只抓**弱引用**：拖放控制器挂在终端上，终端属于 PanelState 的
/// 控件树；抓强引用就是「PanelState → 终端 → 控制器 → PanelState」
/// 的**引用环**，面板永远释放不掉（AGENTS.md 的红线）。
pub fn enable_drop_upload(st: &Rc<PanelState>, term: &crate::termview::TermView) {
    let weak = Rc::downgrade(st);
    dnd::attach(
        term.widget(),
        dnd::Want::Files,
        move |res| {
            let Some(st) = weak.upgrade() else { return };
            on_drop_result(&st, res);
        },
    );
}

/// 「粘贴上传」：把**剪贴板里的本地文件**传到当前远程目录。
///
/// **为什么要有它**：X11 的拖拽传文件依赖源程序提供选区数据，而源程序
/// （Deepin 文件管理器 = Qt）**挑问法**（详见 `xdnd11` 模块的说明）。
/// 拖拽这条路万一不通，还有剪贴板这条更直白的路 —— 而且它的**剪贴板是
/// 好的**（复制文件后剪贴板里有 `text/uri-list`）。
pub fn on_paste_upload(st: &Rc<PanelState>) {
    let weak = Rc::downgrade(st);
    clipfiles::read_local_paths(move |paths| {
        let Some(st) = weak.upgrade() else { return };
        if paths.is_empty() {
            set_remote_status(
                &st,
                crate::theme::State::Warn,
                &tr("剪贴板里没有可上传的本地文件"),
            );
            return;
        }
        start_uploads(&st, paths);
    });
}

pub fn start_downloads(st: &Rc<PanelState>, rpaths: Vec<String>) {
    if !st.conn_ok.get() {
        dialogs::show_error(st.topwin().as_ref(), tr("未连接"), tr("SFTP 尚未连接，无法下载。"));
        return;
    }
    let dst = st.local_cwd.borrow().clone();
    submit_transfers(
        st,
        rpaths.into_iter().map(|p| (p, dst.clone(), false)).collect(),
    );
}

/// 外部 URI（文件管理器）拖进本地面板：复制进当前目录（非递归，与 C 版一致）。
pub fn copy_uris_into_local(st: &Rc<PanelState>, paths: &[String], dstdir: &str) {
    let mut first_err: Option<String> = None;
    for lp in paths {
        if lp == dstdir {
            continue;
        }
        let bn = localpath::basename(lp);
        let dst = localpath::join(dstdir, &bn);
        let src = gtk::gio::File::for_path(lp);
        let df = gtk::gio::File::for_path(&dst);
        if let Err(e) = df.copy(
            &src,
            gtk::gio::FileCopyFlags::OVERWRITE,
            None::<&gtk::gio::Cancellable>,
            None::<&mut dyn FnMut(i64, i64)>,
        ) {
            first_err.get_or_insert_with(|| e.to_string());
        }
    }
    if let Some(e) = first_err {
        dialogs::show_error(st.topwin().as_ref(), tr("复制文件失败"), &e);
    }
    fill_local_pane(st);
}

/* ---------------- 本地面板填充 ---------------- */

/// 让本地面板盯住当前目录：命令行/外部程序 rm、touch、mv 都即时刷新。
/// 目录没变就不重建监视器；监视器本身由 PanelState 持有。
fn ensure_local_monitor(st: &Rc<PanelState>, cwd: &str) {
    if *st.local_monitor_path.borrow() == cwd && st.local_monitor.borrow().is_some() {
        return;
    }
    *st.local_monitor_path.borrow_mut() = cwd.to_string();
    st.local_monitor.borrow_mut().take();
    let file = gio::File::for_path(cwd);
    let Ok(mon) = file.monitor_directory(
        gio::FileMonitorFlags::NONE,
        None::<&gio::Cancellable>,
    ) else {
        return;
    };
    let stw = Rc::downgrade(st);
    mon.connect_changed(move |_m, _file, _other, event| {
        if matches!(
            event,
            gio::FileMonitorEvent::PreUnmount | gio::FileMonitorEvent::Unmounted
        ) {
            return;
        }
        if let Some(st) = stw.upgrade() {
            schedule_local_refresh(&st);
        }
    });
    *st.local_monitor.borrow_mut() = Some(mon);
}

/// 一次文件变动会连发多个事件，250ms 去抖后再整目录重列。
fn schedule_local_refresh(st: &Rc<PanelState>) {
    if st.dead.get() || st.local_refresh_pending.get() {
        return;
    }
    st.local_refresh_pending.set(true);
    let stw = Rc::downgrade(st);
    glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        if let Some(st) = stw.upgrade() {
            st.local_refresh_pending.set(false);
            if !st.dead.get() {
                fill_local_pane(&st);
            }
        }
    });
}

pub fn fill_local_pane(st: &Rc<PanelState>) {
    let pane = st.pane(false);
    let cwd = st.local_cwd.borrow().clone();
    match pane::read_local_dir(&cwd) {
        Ok(rows) => {
            let show_hidden = pane.show_hidden.get();
            pane::fill_model(&pane, show_hidden, rows);
            pane.set_cwd_entry(&cwd);
            ensure_local_monitor(st, &cwd);
        }
        Err(e) => {
            // 目录读不了时停掉旧目录的监视器，避免它继续触发刷新
            st.local_monitor.borrow_mut().take();
            st.local_monitor_path.borrow_mut().clear();
            pane.set_cwd_entry(&cwd);
            dialogs::show_error(
                st.topwin().as_ref(),
                tr("读取本地目录失败"),
                &trf!("{e}\n目录：{cwd}", e = e, cwd = cwd),
            );
        }
    }
}

/* ---------------- URI 解码 ---------------- */

/// 简单百分号解码（对应 g_uri_unescape_string）。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| c.is_ascii_hexdigit().then(|| (c as char).to_digit(16)).flatten();
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{count_running, percent_decode, retry_op_matches, usable_local_dir, RetryOp};
    use crate::panel::worker::SimpleOp;

    #[test]
    fn uri_decode() {
        assert_eq!(percent_decode("/home/user"), "/home/user");
        assert_eq!(percent_decode("/a%20b/c"), "/a b/c");
        assert_eq!(percent_decode("/%E4%B8%AD"), "/中");
        assert_eq!(percent_decode("/%zz"), "/%zz"); // 非法序列保留
    }

    /// **重试的必须是「失败的那一条」**，不是「最后记住的那一条」
    /// （阶段 99 复查时发现的竞态：中间插进来一条后台刷新会改写记住的那条）。
    #[test]
    fn 重试要认准失败的那条操作() {
        // 路径对得上 ⇒ 可以重试
        assert!(retry_op_matches(
            &RetryOp::List {
                path: "/tmp/a".into()
            },
            "/tmp/a"
        ));
        assert!(retry_op_matches(
            &RetryOp::Simple {
                op: SimpleOp::Rename,
                path: "/tmp/a".into(),
                new_path: Some("/tmp/b".into()),
            },
            "/tmp/a"
        ));
        assert!(retry_op_matches(
            &RetryOp::ReadFile {
                id: 1,
                path: "/tmp/a".into(),
                max: 1024,
            },
            "/tmp/a"
        ));
        assert!(retry_op_matches(
            &RetryOp::WriteFile {
                id: 1,
                path: "/tmp/a".into(),
            },
            "/tmp/a"
        ));
        // 路径对不上 ⇒ **不重试**（重试错的那条 = 替用户干了别的事）
        assert!(!retry_op_matches(
            &RetryOp::List {
                path: "/tmp/b".into()
            },
            "/tmp/a"
        ));
        assert!(!retry_op_matches(
            &RetryOp::WriteFile {
                id: 2,
                path: "/tmp/b".into(),
            },
            "/tmp/a"
        ));
    }

    #[test]
    fn local_dir_memory_only_restores_usable_abs_dirs() {
        let d = crate::util::testutil::temp_home("localdir");
        assert!(usable_local_dir(&d.to_string_lossy()), "临时目录应可恢复");

        let file = d.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!usable_local_dir(&file.to_string_lossy()), "文件不是目录");

        assert!(!usable_local_dir("relative/path"), "相对路径不恢复");
        assert!(
            !usable_local_dir("/definitely/not/exist/puttyplus"),
            "不存在的目录不恢复"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn 传输计数只算还在传的() {
        use crate::panel::worker::XferState as S;
        // 传完的、失败的、取消的**都不算**还在传 ⇒ 角标不能一直挂着
        let r = count_running(&[
            (S::Run, true),
            (S::Wait, true),
            (S::Done, true),
            (S::Err, true),
            (S::Cancel, true),
            (S::Skip, true),
        ]);
        assert_eq!(r.upload, 2, "只有 Run/Wait 算：{r:?}");
        assert_eq!(r.download, 0);
        assert_eq!(r.total(), 2);
        // 空的 ⇒ 什么都不显示（状态行不留「传输中 0 个」）
        assert!(count_running(&[]).label().is_none());
    }

    #[test]
    fn 传输提示分方向说() {
        use crate::panel::worker::XferState as S;
        // 只上传 ⇒ 说上传
        assert_eq!(
            count_running(&[(S::Run, true)]).label().unwrap(),
            "上传中 1 个"
        );
        // 只下载 ⇒ 说下载
        assert_eq!(
            count_running(&[(S::Run, false)]).label().unwrap(),
            "下载中 1 个"
        );
        // 两个方向都有 ⇒ 说不清方向，就只说「传输中」
        assert_eq!(
            count_running(&[(S::Run, true), (S::Run, false)])
                .label()
                .unwrap(),
            "传输中 2 个"
        );
    }
}
