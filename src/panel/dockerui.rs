//! 连接页里的**「容器」子标签**（阶段 109）。
//!
//! ## 干什么用
//!
//! 用户 2026-11-15：「我们要在软件里面显示**远程服务器上的容器运行情况**」
//! ⇒ 在连接页里开一个「容器」子标签，看这台服务器上跑了哪些容器。
//!
//! ## 列表长什么样（**树形**）
//!
//! 用户 2026-11-15：「容器列表要中要体现单独的容器和使用 docker-compose
//! 组织的容器，每个 docker-compose 项目**点开之后**才显示它组织的容器」
//!
//! ⇒ 表格用 [`gtk::TreeStore`]：**compose 项目是父行**（默认收起，点开才
//! 看见它带的那些容器），**独立容器是顶层行**（一眼就看得见）。
//!
//! ```text
//! ┌ 工具条 ──────────────────────────────────────────────────────┐
//! │ [搜索框]  [全部][运行中][已停止]   上次刷新 11:23:45         │
//! │                                  [自动刷新 ◉] [↻ 刷新]        │
//! ├ 表格 ────────────────────────────────────────────────────────┤
//! │ ▼ myproject                              3 个容器  compose 项目 │
//! │    web       a1b2c3  nginx:latest  0.0.0.0:80→80  2.3%  运行中 │
//! │    db        d4e5f6  postgres:17                         运行中 │
//! │   standalone  g7h8i9  redis:7                            已停止 │
//! └ 状态行 ──────────────────────────────────────────────────────┘
//! ```
//!
//! ## 三条要紧的规矩
//!
//! 1. **不新增 SSH 连接**：数据走**共享连接**上的 exec 通道（见
//!    [`crate::backend::docker`]）⇒ 一台服务器仍然只有一条连接。
//! 2. **刷新不卡界面**：命令在后台 tokio 任务里跑 ⇒ 结果回主线程更新表格。
//! 3. **别整表重画**：容器数量很少（几十个），整表重建可以接受；但**别**
//!    在每帧重画（项目踩过「整表重画把主线程拖死」的坑，见 AGENTS.md）。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::docker::{
    self, Action, ComposeAction, ContainerDetail, ContainerInfo, DockerInfo, LOG_TAIL,
};
use crate::backend::shell::ShellEvent;
use crate::i18n::tr;
use crate::panel::PanelState;
use crate::trf;

/// 概览页在 `Stack` 里的名字（**内部标识**，不是界面文案）。
const PAGE_OVERVIEW: &str = "overview";
/// 日志页在 `Stack` 里的名字（**内部标识**，不是界面文案）。
const PAGE_LOGS: &str = "logs";
/// 监控页在 `Stack` 里的名字（**内部标识**，不是界面文案）。
const PAGE_MONITOR: &str = "monitor";
/// 文件页在 `Stack` 里的名字（**内部标识**，不是界面文案）。
const PAGE_FILES: &str = "files";

/// 采样间隔（秒）：**阶段 114**。
///
/// ⚠️ `docker stats` 的持续模式**每 500ms 刷一次** ⇒ 直接用会得到一堆
///   挤在一起的点 ⇒ 我们**每 2 秒出一个点**。
const STATS_INTERVAL: f64 = 2.0;

/// 曲线上保留多少个点（2 秒一个 ⇒ 60 个 = **2 分钟**的窗口）。
pub const STATS_POINTS: usize = 60;

/// **顶层分段**（阶段 115）：容器 / 镜像 / 卷 / 网络。
///
/// ⚠️ **跟详情区的三个子页不是一回事**：详情区那三个（概览 / 日志 /
///   监控）是**选中某个容器之后**才有的；这四个是**顶层**（先选看什么，
///   再看细节）。
pub const SEG_CONTAINERS: &str = "containers";
pub const SEG_IMAGES: &str = "images";
pub const SEG_VOLUMES: &str = "volumes";
pub const SEG_NETWORKS: &str = "networks";

/// 表格的列（**顺序 = `TreeStore` 的列顺序**）。
const COL_NAME: usize = 0;
const COL_ID: usize = 1;
const COL_IMAGE: usize = 2;
const COL_PORTS: usize = 3;
const COL_CPU: usize = 4;
const COL_MEM: usize = 5;
const COL_STATE: usize = 6;
const COL_DETAIL: usize = 7;
const N_COLS: usize = 8;

/// 过滤：全部 / 运行中 / 已停止。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Running,
    Stopped,
}

/// 自动刷新的间隔（秒）——跟 dbx 一致。
const AUTO_REFRESH_SECS: u32 = 7;

/// 容器页的界面状态。
pub struct DockerUi {
    /// 子标签页的内容。
    ///
    /// ⚠️ **阶段 113 之后**：它是「外层竖向 Paned」——上＝页面内容、
    ///   下＝**编辑区的落点**（用户 2026-11-16：编辑器要**马上能看到**，
    ///   不能藏在 term 界面里）。
    pub page: gtk::Widget,
    /// 外层那根竖向 Paned（**编辑区的宿主**，见 `mount` 里登记的地方）。
    pub column: gtk::Paned,
    /// 本页在子标签里的页 id。
    pub tab_id: Cell<u64>,
    /// 表格的模型（**树形**：compose 项目是父行）。
    model: gtk::TreeStore,
    /// 表格。
    view: gtk::TreeView,
    /// 状态行（错误 / 提示）。
    status: gtk::Label,
    /// 「上次刷新 hh:mm:ss」。
    last: gtk::Label,
    /// 自动刷新开关。
    auto: gtk::Switch,
    /// 当前的过滤。
    filter: Cell<Filter>,
    /// 搜索词（空 = 不过滤）。
    search: RefCell<String>,
    /// 正在刷新（防重入：慢命令不能叠加）。
    busy: Cell<bool>,
    /// **工具条里的小转圈**（取数据的时候转）。
    ///
    /// ⚠️ **干什么用**（用户 2026-11-16）：「容器界面打开后需要一段时间
    ///   才能看到容器」⇒ 这段时间里界面是**空的**，用户不知道程序是在
    ///   干活还是卡死了 ⇒ 给个**看得见的动画**。
    spin: gtk::Spinner,
    /// **表格区居中的大转圈**（**首次**加载、表格还是空的时候显示）。
    ///
    /// ⚠️ 为什么要两个：工具条那个只是「右上角有个小东西在转」⇒ 不够
    ///   醒目；这个**在列表正中间**，是「正在加载」的经典样子。
    spin_big: gtk::Spinner,
    /// 自动刷新的定时器（页签关掉时要停掉）。
    timer: Cell<Option<glib::SourceId>>,
    /// **后台任务 → 主线程**的结果通道（⚠️ 见 [`refresh`] 的说明）。
    tx: async_channel::Sender<Msg>,
    /// 最近一次取到的容器（**冒烟测试读它**）。
    pub containers: RefCell<Vec<ContainerInfo>>,
    /// 远程 docker 版本（显示在状态行里）。
    pub version: RefCell<String>,
    /// **名称列**的渲染器（**冒烟用它验「字真的画出来了」**）。
    ///
    /// ⚠️ GTK4 的 `pack_start` 不会把列的文本喂给渲染器 ⇒ 必须
    ///    `add_attribute` 绑定 ⇒ 画完之后渲染器的 "text" 属性里**有值**；
    ///    没绑定就是空的（表格一片空白）。
    name_renderer: Option<gtk::CellRendererText>,
    /// **详情区**（阶段 110：选中容器才显示）。
    detail: DetailUi,
    /// **日志页**（阶段 112）。
    log: LogUi,
    /// 详情区里的 **Stack**（概览 / 日志两页）。
    stack: gtk::Stack,
    /// 正在看**哪个容器**的日志（空 = 没在看）。
    ///
    /// ⚠️ **用来认「这条数据是不是现在这条流的」**：换了容器以后，旧流
    ///    还在往通道里写 ⇒ 它的数据必须丢掉（不然两个容器的日志混一块）。
    log_name: RefCell<String>,
    /// 收到的**全部**日志行（**暂停时也照收** ⇒ 恢复时能补上）。
    log_lines: RefCell<Vec<String>>,
    /// **还没收全**的半行（跨块的一行 ⇒ 拼起来）。
    log_partial: RefCell<String>,
    /// **暂停**中（数据照收，显示不更新）。
    log_paused: Cell<bool>,
    /// 日志的**搜索词**（空 = 全显示）。
    log_search: RefCell<String>,
    /// 停日志流的**信号**（`oneshot` ⇒ 发了就停）。
    log_stop: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    /// 日志流**是不是在跑**。
    log_running: Cell<bool>,
    /// **容器内终端**的通道（阶段 113；空 = 没开）。
    term_chan: RefCell<Option<crate::backend::shell::ShellChannel>>,
    /// 容器内终端**是不是开着**。
    term_running: Cell<bool>,
    /// 容器内终端的**控件句柄**（测试钩子读屏幕用）。
    ///
    /// ⚠️ 窗口关掉时**要清掉**（`stop_terminal`）⇒ 不然这里攥着一个
    ///    已经被销毁的控件 ⇒ 用它会炸。
    term_view: RefCell<Option<crate::termview::TermView>>,
    /// 容器内终端的**窗口**（测试钩子用来关掉它）。
    ///
    /// ⚠️ **为什么留着**：冒烟要截「监控页」的图 ⇒ 得先把**盖在上面的
    ///   终端窗口**关掉（截图拍的是整屏）。
    term_win: RefCell<Option<gtk::Window>>,
    /// **正在关**容器内终端（**重入保护**，阶段 125）。
    ///
    /// ⚠️ **为什么需要**：窗口的 `close_request` 也会调 `stop_terminal`
    ///   ⇒ 而 `stop_terminal` 现在要**关窗口** ⇒ 不挡一下就是
    ///   「自己调自己」（窗口 → stop_terminal → 关窗口 → 窗口 → …）。
    term_closing: Cell<bool>,
    /// **监控页**（阶段 114）。
    monitor: MonitorUi,
    /// 监控曲线的数据（**共享**给四张图 ⇒ 外面套 `Rc<RefCell<…>>`）。
    monitor_data: Rc<RefCell<MonitorData>>,
    /// 正在看**哪个容器**的监控（空 = 没在看）。
    ///
    /// ⚠️ **用来认「这条数据是不是现在这条流的」**：换了容器以后，旧流
    ///   还在往通道里写 ⇒ 它的数据必须丢掉（不然两个容器的曲线混一块）。
    stat_name: RefCell<String>,
    /// 停监控流的**信号**（`oneshot` ⇒ 发了就停）。
    stat_stop: RefCell<Option<tokio::sync::oneshot::Sender<()>>>,
    /// 监控流**是不是在跑**。
    stat_running: Cell<bool>,
    /// **顶层分段**（阶段 115）：容器 / 镜像 / 卷 / 网络 四页。
    seg: gtk::Stack,
    /// 四个**分段按钮**（点谁切谁）。
    seg_buttons: RefCell<Vec<(gtk::ToggleButton, &'static str)>>,
    /// **当前分段**（阶段 115）。
    seg_current: Cell<&'static str>,
    /// **镜像表**（阶段 115）。
    ///
    /// ⚠️ **用 TreeStore（平表）**：项目里读表值统一走 `model.get(&it, col)`
    ///   （跟容器表一个写法）⇒ 三张新表也用树表（只是不建子行）。
    images_store: gtk::TreeStore,
    images_view: gtk::TreeView,
    /// **卷表**（阶段 115）。
    volumes_store: gtk::TreeStore,
    volumes_view: gtk::TreeView,
    /// **网络表**（阶段 115）。
    networks_store: gtk::TreeStore,
    networks_view: gtk::TreeView,
    /// 三个分段**各自**的「正在取」标志（阶段 115）。
    ///
    /// ⚠️ **为什么不跟容器共用一个 `busy`**：容器正在刷新时，用户切到
    ///   「镜像」页 ⇒ 镜像那次取数会被 `busy` 挡掉 ⇒ 用户干等 ⇒
    ///   ⇒ 每个分段各管各的。
    images_busy: Cell<bool>,
    volumes_busy: Cell<bool>,
    networks_busy: Cell<bool>,
    /// **「文件」页**（阶段 116）。
    files: FilesUi,
    /// 文件页**正在列的是哪个容器**（空 = 没选容器）。
    files_name: RefCell<String>,
    /// 文件页**当前目录**（远程 POSIX 路径）。
    files_path: RefCell<String>,
    /// 文件页**正在传**（上传 / 下载 ⇒ 不许再开一个）。
    files_xfer: Cell<bool>,
    /// 文件页**已经完成了几次传输**（上传 / 下载各算一次）。
    ///
    /// ⚠️ **给冒烟用的**：状态行会被刷新冲掉 ⇒ 冒烟没法靠它判断「传完了
    ///   没有」⇒ 用一个**只增不减的计数**（每完成一次 +1）⇒ 冒烟比大小。
    files_xfer_done: Cell<u64>,
    /// 文件页**取列表的序号**（只认最新一次的结果）。
    ///
    /// ⚠️ **为什么需要**：用户连点目录时，**好几个请求同时在飞** ⇒ 谁先
    ///   回来谁先上屏 ⇒ **旧结果会盖掉新结果**（看着像「刷新了但内容不对」）
    ///   ⇒ 每次请求发一个递增序号 ⇒ 只认**序号最大**的那次。
    files_seq: Cell<u64>,
    /// **最后一个采样点的原始行**（**带 ANSI** ⇒ 原样）。
    ///
    /// ⚠️ **为什么留原始行**：冒烟要能验证「**剥 ANSI 之后**解析出来的
    ///   值跟远端那一行对得上」⇒ 光看指标卡不够（卡上只有数字）⇒ 得
    ///   留着**原文**才能做对照。
    stat_last_raw: RefCell<Option<String>>,
    /// 当前选中的**容器名**（空 = 没选容器）。
    ///
    /// ⚠️ **刷新要保住它**：自动刷新每 7 秒重建一次表格 ⇒ 不记住选中行的话，
    ///    用户刚点开的容器就被「刷新」顶掉了。
    selected: RefCell<String>,
    /// 当前选中的 **compose 项目名**（空 = 没选项目行）。
    ///
    /// ⚠️ 与 `selected` **互斥**：项目行是父行 ⇒ 选中它就不选容器。
    selected_project: RefCell<String>,
    /// 正在**程序性**地恢复选中（⇒ 别因此又去取一次详情）。
    restoring: Cell<bool>,
    /// **最近一次写操作的结果**（空 = 没执行过写操作）。
    ///
    /// ⚠️ 单独记一份（不靠状态行）：状态行会被「刷新」改掉 ⇒ 拿它判断
    ///    「动作有没有执行」会**误判**（刷新先到了就会被当成动作跑了）。
    last_action: RefCell<String>,
}

/// 后台任务回主线程的消息（⚠️ 通道里只能传**纯数据**）。
enum Msg {
    /// 容器列表刷新完了。
    List(Result<DockerInfo, String>),
    /// 某个容器的详情取回来了（⚠️ 带上容器名 ⇒ 旧结果能认出来、丢掉）。
    Detail {
        name: String,
        r: Result<ContainerDetail, String>,
    },
    /// 写操作（启停/重启）的结果（中文提示）。
    Action(String),
    /// 日志**增量**（⚠️ 带上容器名 ⇒ 旧容器的残留能认出来、丢掉）。
    LogChunk {
        name: String,
        data: String,
    },
    /// 日志流**结束了**（远端命令退出；`code` 是退出码）。
    LogEnd {
        name: String,
        code: i32,
    },
    /// 日志流**开不起来**（连不上 / 没权限 / 容器没了）。
    LogError {
        name: String,
        msg: String,
    },
    /// **监控采样**的一个点（阶段 114）。
    ///
    /// ⚠️ 带上容器名 ⇒ 换了容器以后，旧流的数据能认出来、丢掉。
    StatPoint {
        name: String,
        pt: docker::StatPoint,
        /// **原始行**（**带 ANSI** ⇒ 原样，不剥）。
        ///
        /// ⚠️ 带上它是为了**测试能对照**：冒烟拿它跟指标卡比 ⇒ 能抓到
        ///   「忘了剥 ANSI ⇒ 解析出来的全是 0」这类退化。
        raw: String,
    },
    /// 监控流**结束了**（远端命令退出；`code` 是退出码）。
    StatEnd {
        name: String,
        code: i32,
    },
    /// 监控流**开不起来**（连不上 / 没权限 / 容器没了）。
    StatError {
        name: String,
        msg: String,
    },
    /// **镜像列表**回来了（阶段 115）。
    Images(Result<Vec<docker::ImageInfo>, String>),
    /// **卷列表**回来了（阶段 115）。
    Volumes(Result<Vec<docker::VolumeInfo>, String>),
    /// **网络列表**回来了（阶段 115）。
    Networks(Result<Vec<docker::NetInfo>, String>),
    /// **文件列表**回来了（阶段 116）。
    ///
    /// ⚠️ 带上**容器名与目录** ⇒ 换了容器 / 换了目录以后，旧结果能认出来、
    ///   丢掉（不然界面显示的是上一个目录的内容）。
    Files {
        name: String,
        path: String,
        /// **请求序号**（只认最新一次 ⇒ 旧结果不许盖掉新结果）。
        seq: u64,
        r: Result<Vec<docker::ContainerFile>, String>,
    },
    /// **上传 / 下载**的进度（阶段 116）。
    FileProgress { msg: String },
    /// **上传 / 下载**结束了（成功或失败）。
    FileXferDone { msg: String },
    /// **查看文件**的内容回来了（阶段 116）。
    FileView {
        path: String,
        r: Result<String, String>,
    },
}

/// 详情区的控件（阶段 110）。
struct DetailUi {
    /// 整块（**没选容器时不显示**）。
    root: gtk::Box,
    /// 标题（「详情：web（运行中）」）。
    title: gtk::Label,
    /// 四张卡片的正文。
    config: gtk::Label,
    net: gtk::Label,
    mount: gtk::Label,
    env: gtk::Label,
    /// 「启动」「停止」「重启」三个按钮（**写操作 ⇒ 点了要二次确认**）。
    btn_start: gtk::Button,
    btn_stop: gtk::Button,
    btn_restart: gtk::Button,
    /// **「终端」按钮**（阶段 113：开容器内终端）。
    btn_term: gtk::Button,
    /// **compose 项目详情**那块（阶段 111：选中项目行时显示，与容器详情互斥）。
    proj_root: gtk::Box,
    /// 项目标题（「compose 项目：postgres」）。
    proj_title: gtk::Label,
    /// 项目正文（配置文件 / 工作目录 / 容器清单）。
    proj_info: gtk::Label,
    /// 「编辑 compose 文件」。
    btn_edit: gtk::Button,
    /// 「启动」「停止」「销毁」「重建」（**写操作 ⇒ 点了要二次确认**）。
    btn_cstart: gtk::Button,
    btn_cstop: gtk::Button,
    btn_cdown: gtk::Button,
    btn_cup: gtk::Button,
}

/// 监控页的控件（**阶段 114**）。
struct MonitorUi {
    /// 整块（**只在监控页里显示**）。
    root: gtk::Box,
    /// 四个指标卡的**当前值**（CPU / 内存 / 网络 / 磁盘）。
    val_cpu: gtk::Label,
    val_mem: gtk::Label,
    val_net: gtk::Label,
    val_blk: gtk::Label,
    /// 四张折线图（cairo 自绘）。
    chart_cpu: gtk::DrawingArea,
    chart_mem: gtk::DrawingArea,
    chart_net: gtk::DrawingArea,
    chart_blk: gtk::DrawingArea,
    /// 状态行（「正在采样…」「已结束」「出错原因」）。
    status: gtk::Label,
}

/// 「文件」页的控件（**阶段 116**）。
///
/// ⚠️ **容器里没有 SFTP** ⇒ 浏览 / 上传 / 下载全靠 `docker exec`
///   （命令与解析见 [`crate::backend::docker`]）。
struct FilesUi {
    /// 整块（**只在文件页里显示**）。
    root: gtk::Box,
    /// **路径栏**（当前远程目录；回车 ⇒ 去这个目录）。
    path: gtk::Entry,
    /// 文件表（**平表**：名字 / 类型 / 大小 / 权限 ＋ 一个隐藏的类型键列）。
    store: gtk::TreeStore,
    /// 文件表。
    view: gtk::TreeView,
    /// 状态行（**列表**的状态：几个条目 / 出错原因）。
    status: gtk::Label,
    /// **传输状态行**（上传 / 下载的进度与结果）。
    ///
    /// ⚠️ **为什么不跟列表状态挤在一行**：列表结果**随时**会到（刷新、切
    ///   目录）⇒ 挤在一行 ⇒ 传输刚报完「上传完成」就被「N 个条目」冲掉
    ///   ⇒ 用户（和冒烟）根本看不到传完了没有。
    xfer_status: gtk::Label,
}

/// 监控曲线的数据（**共享**给四张图 ⇒ 外面套 `Rc<RefCell<…>>`）。
///   拿到最新的点 ⇒ 得和界面共享同一份数据（`Rc<RefCell<…>>`）。
///
/// ⚠️ **为什么 `Vec<(f64, f64)>`**：`(x, y)`；`x` 是**点的序号**（等间隔
///   ⇒ 画的时候归一化成横坐标）。
#[derive(Default)]
struct MonitorData {
    /// CPU（%）。
    cpu: Vec<(f64, f64)>,
    /// 内存（%）。
    mem: Vec<(f64, f64)>,
    /// 网络接收（字节/秒）。
    net_rx: Vec<(f64, f64)>,
    /// 网络发送（字节/秒）。
    net_tx: Vec<(f64, f64)>,
    /// 磁盘读（字节/秒）。
    blk_read: Vec<(f64, f64)>,
    /// 磁盘写（字节/秒）。
    blk_write: Vec<(f64, f64)>,
}

impl MonitorData {
    /// 清空（换容器 / 重新起流时）。
    fn clear(&mut self) {
        self.cpu.clear();
        self.mem.clear();
        self.net_rx.clear();
        self.net_tx.clear();
        self.blk_read.clear();
        self.blk_write.clear();
    }

    /// 追加一个点（**只保留最近 `STATS_POINTS` 个**）。
    fn push(&mut self, pt: docker::StatPoint) {
        Self::push_one(&mut self.cpu, pt.cpu);
        Self::push_one(&mut self.mem, pt.mem);
        Self::push_one(&mut self.net_rx, pt.net_rx);
        Self::push_one(&mut self.net_tx, pt.net_tx);
        Self::push_one(&mut self.blk_read, pt.blk_read);
        Self::push_one(&mut self.blk_write, pt.blk_write);
    }

    /// 追加一条线的点（**满了就丢最旧的**）。
    fn push_one(series: &mut Vec<(f64, f64)>, v: f64) {
        let x = series.len() as f64;
        series.push((x, v));
        if series.len() > STATS_POINTS {
            let drop = series.len() - STATS_POINTS;
            series.drain(..drop);
        }
    }
}

/// 画一张折线图（**纯绘图**：cairo + 点集 ⇒ 好测好读**）。
///
/// ⚠️ `series`：每条 = (点集, 颜色 RGB)。
/// ⚠️ **纵轴自动量程**：取所有线的最大值（至少 1 ⇒ 不然全是 0 时线贴在底
///   上看不见）。
/// ⚠️ **横轴**：按点数归一化（等间隔 ⇒ 不用管 x 的绝对值）。
fn draw_chart(
    cr: &gtk::cairo::Context,
    w: i32,
    h: i32,
    series: &[(&[(f64, f64)], (f64, f64, f64))],
    unit: &str,
) {
    let (ww, hh) = (f64::from(w), f64::from(h));
    if ww < 20.0 || hh < 20.0 {
        return;
    }
    let pad_l = 6.0;
    let pad_r = 6.0;
    let pad_t = 6.0;
    let pad_b = 14.0;
    let plot_w = (ww - pad_l - pad_r).max(1.0);
    let plot_h = (hh - pad_t - pad_b).max(1.0);

    // 背景 + 边框
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.rectangle(0.5, 0.5, ww - 1.0, hh - 1.0);
    let _ = cr.fill_preserve();
    cr.set_source_rgb(0.62, 0.62, 0.62);
    let _ = cr.set_line_width(1.0);
    let _ = cr.stroke();

    // 网格（三条横线）
    cr.set_source_rgba(0.75, 0.75, 0.75, 0.45);
    for i in 1..=3 {
        let y = pad_t + plot_h * (i as f64) / 4.0;
        let _ = cr.move_to(pad_l, y);
        let _ = cr.line_to(pad_l + plot_w, y);
    }
    let _ = cr.stroke();

    // 纵轴量程：所有线的最大值（至少 1）
    let mut max_y = 1.0f64;
    for (pts, _) in series {
        for (_, v) in pts.iter() {
            if v.is_finite() && *v > max_y {
                max_y = *v;
            }
        }
    }

    // 曲线
    for (pts, color) in series {
        if pts.len() < 2 {
            continue;
        }
        let n = pts.len() as f64;
        cr.set_source_rgb(color.0, color.1, color.2);
        let _ = cr.set_line_width(1.6);
        for (i, (_, v)) in pts.iter().enumerate() {
            let x = pad_l + plot_w * (i as f64) / (n - 1.0).max(1.0);
            let vv = if v.is_finite() { *v } else { 0.0 };
            let y = pad_t + plot_h * (1.0 - (vv / max_y).clamp(0.0, 1.0));
            if i == 0 {
                let _ = cr.move_to(x, y);
            } else {
                let _ = cr.line_to(x, y);
            }
        }
        let _ = cr.stroke();
    }

    // 单位（**ASCII** ⇒ cairo 的玩具文字接口能用）
    cr.set_source_rgb(0.35, 0.35, 0.35);
    let _ = cr.set_font_size(10.0);
    let _ = cr.move_to(pad_l, hh - 4.0);
    let _ = cr.show_text(unit);
}

/// 把字节/秒变成**人能读**的字符串（`B/s` / `KB/s` / `MB/s`）。
fn fmt_rate(v: f64) -> String {
    if !v.is_finite() || v < 1.0 {
        return "0 B/s".to_string();
    }
    if v < 1024.0 {
        return format!("{v:.0} B/s");
    }
    if v < 1024.0 * 1024.0 {
        return format!("{:.1} KB/s", v / 1024.0);
    }
    if v < 1024.0 * 1024.0 * 1024.0 {
        return format!("{:.1} MB/s", v / (1024.0 * 1024.0));
    }
    format!("{:.1} GB/s", v / (1024.0 * 1024.0 * 1024.0))
}

/// 日志页的控件（阶段 112）。
struct LogUi {
    /// 整块（**只在日志页里显示** ⇒ 概览页看不见它）。
    root: gtk::Box,
    /// 日志正文（**只读**、等宽）。
    view: gtk::TextView,
    /// 正文缓冲（**增量追加** ⇒ 不整表重画）。
    buffer: gtk::TextBuffer,
    /// 包着正文的滚动容器（**自动跟随**要拿它的 adjustment）。
    scroll: gtk::ScrolledWindow,
    /// 搜索框（按内容过滤）。
    search: gtk::SearchEntry,
    /// 「暂停 / 继续」按钮。
    btn_pause: gtk::Button,
    /// 「清空」按钮。
    btn_clear: gtk::Button,
    /// 「自动跟随」开关（默认开）。
    follow: gtk::Switch,
    /// 状态行（「正在接收…」「已结束」「出错原因」）。
    status: gtk::Label,
}

/// 表格里的一个**顶层行**（重建时用的中间形状）。
///
/// ⚠️ 分两种：
/// - **项目行**（`compose` 非空）：compose 项目，**父行**，点开才见它的容器；
/// - **容器行**（`compose` 空）：独立跑的容器，**顶层行**。
#[derive(Debug, Clone)]
enum Node {
    /// 一个 compose 项目（父行）。
    Project {
        /// 项目名（`com.docker.compose.project` 标签）。
        name: String,
        /// 这个项目带的容器（子行）。
        items: Vec<ContainerInfo>,
    },
    /// 一个独立容器（顶层行）。
    Solo(ContainerInfo),
}

impl DockerUi {
    /// 把容器列表**按 compose 项目分组**（保首次出现顺序）。
    ///
    /// ⚠️ **纯函数** ⇒ 能单测（分组错了用户一眼就看出来）。
    fn group(rows: &[ContainerInfo]) -> Vec<Node> {
        let mut out: Vec<Node> = Vec::new();
        for r in rows {
            match r.compose_group() {
                None => out.push(Node::Solo(r.clone())),
                Some(p) => {
                    // 找到已有的项目行 ⇒ 追加进去；没有 ⇒ 新建一行。
                    let mut hit = false;
                    for n in out.iter_mut() {
                        if let Node::Project { name, items } = n {
                            if name == p {
                                items.push(r.clone());
                                hit = true;
                                break;
                            }
                        }
                    }
                    if !hit {
                        out.push(Node::Project {
                            name: p.to_string(),
                            items: vec![r.clone()],
                        });
                    }
                }
            }
        }
        out
    }

    /// **当前展开了哪些 compose 项目**（项目名集合）。
    ///
    /// ⚠️ **为什么自己算而不是听信号**：刷新时表格会 `clear()` 重建 ⇒
    /// GTK 在清空的过程中也会发 `row-collapsed` ⇒ 用信号记账的话，
    /// 「清空」这个动作会把用户刚点开的项目**从记录里抹掉** ⇒
    /// 刷新后展开状态全丢。
    ///
    /// ⇒ 改成**清空前问一次表格**（`row_expanded`）⇒ 拿到的是用户眼前的
    /// 真状态，不受重建过程影响。
    fn expanded_projects(&self) -> HashSet<String> {
        let mut out = HashSet::new();
        let mut it = match self.model.iter_first() {
            Some(i) => i,
            None => return out,
        };
        loop {
            // 有子行的才是项目行。
            if self.model.iter_children(Some(&it)).is_some() {
                let path = self.model.path(&it);
                if self.view.row_expanded(&path) {
                    let name: String = self.model.get(&it, COL_NAME as i32);
                    if !name.is_empty() {
                        out.insert(name);
                    }
                }
            }
            let mut next = it;
            if !self.model.iter_next(&mut next) {
                break;
            }
            it = next;
        }
        out
    }

    /// 重建整张表（**容器数量很少，整表重建没问题**）。
    ///
    /// ⚠️ **展开状态要保住**：用户刚点开的 compose 项目，刷新后还得是开的。
    /// ⚠️ **选中行也要保住**：自动刷新每 7 秒重建一次 ⇒ 不恢复的话，用户刚
    ///    选中的容器就被「刷新」顶掉了（详情也跟着没了）。
    fn rebuild(&self, rows: &[ContainerInfo]) {
        let expanded = self.expanded_projects();
        let sel = self.selected.borrow().clone();
        let sel_proj = self.selected_project.borrow().clone();
        // ⚠️ **整段重建都算「程序性」**（`restoring`）。
        //
        // ⚠️ **为什么必须挡这一段**：清空模型会**清掉光标** ⇒ GTK 会发
        //    `cursor-changed` ⇒ 那时候「界面上选中的行」已经没了 ⇒ 处理器
        //    会把 `selected` 清空、把详情区收起来 ⇒ **刷新一次，用户刚选中
        //    的容器就丢了**（详情区消失、启停按钮失灵、日志流也停）。
        //    ⇒ 用 `restoring` 把这段「假的光标变化」挡掉。
        self.restoring.set(true);
        self.model.clear();
        for node in Self::group(rows) {
            match node {
                Node::Solo(c) => {
                    let it = self.model.append(None);
                    self.set_container_row(&it, &c);
                }
                Node::Project { name, items } => {
                    // 父行：项目名 + 「N 个容器」。
                    let it = self.model.append(None);
                    self.set_project_row(&it, &name, items.len());
                    for c in items {
                        let sub = self.model.append(Some(&it));
                        self.set_container_row(&sub, &c);
                    }
                    // 之前是展开的 ⇒ 展开回来。
                    if expanded.contains(&name) {
                        let path = self.model.path(&it);
                        self.view.expand_row(&path, false);
                    }
                }
            }
        }
        // 恢复选中（**程序性** ⇒ 不触发重新取详情）。
        //
        // ⚠️ **项目行与容器行二选一**：项目名优先（它也是顶层行 ⇒
        //    `find_path` 能找到）。
        let restore = if !sel_proj.is_empty() {
            sel_proj.clone()
        } else {
            sel.clone()
        };
        if !restore.is_empty() {
            if let Some(path) = self.find_path(&restore) {
                gtk::prelude::TreeViewExt::set_cursor(&self.view, &path, None, false);
            }
        }
        self.restoring.set(false);
    }

    /// 写一个容器行。
    fn set_container_row(&self, it: &gtk::TreeIter, c: &ContainerInfo) {
        let state = c.state_label().to_string();
        self.model.set(
            it,
            &[
                (COL_NAME as u32, &c.name),
                (COL_ID as u32, &c.id),
                (COL_IMAGE as u32, &c.image),
                (COL_PORTS as u32, &c.ports),
                (COL_CPU as u32, &c.cpu),
                (COL_MEM as u32, &c.mem),
                (COL_STATE as u32, &state),
                (COL_DETAIL as u32, &c.status),
            ],
        );
    }

    /// 写一个项目行（父行）。
    fn set_project_row(&self, it: &gtk::TreeIter, name: &str, n: usize) {
        let cnt = trf!("{n} 个容器", n = n);
        let kind = tr("compose 项目").to_string();
        self.model.set(
            it,
            &[
                (COL_NAME as u32, &name.to_string()),
                (COL_ID as u32, &String::new()),
                (COL_IMAGE as u32, &String::new()),
                (COL_PORTS as u32, &String::new()),
                (COL_CPU as u32, &String::new()),
                (COL_MEM as u32, &String::new()),
                (COL_STATE as u32, &cnt),
                (COL_DETAIL as u32, &kind),
            ],
        );
    }

    /// 刷新完了：填表 + 更新状态行。
    fn apply(&self, info: &DockerInfo) {
        *self.containers.borrow_mut() = info.containers.clone();
        *self.version.borrow_mut() = info.version.clone();
        let shown = self.filter_rows(&info.containers);
        self.rebuild(&shown);
        // ⚠️ 选中的**项目** ⇒ 项目详情也跟着刷新（容器状态变了）。
        let proj = self.selected_project.borrow().clone();
        if !proj.is_empty() {
            self.show_project(&proj);
        }
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        self.last.set_text(&trf!("上次刷新 {t}", t = now));
        let msg = trf!(
            "共 {n} 个容器（远程 Docker {v}）",
            n = info.containers.len(),
            v = info.version
        );
        self.status.set_text(&msg);
        // ⚠️ **放最后**：大转圈要不要消失，取决于「列表空不空」⇒ 得等
        //   列表填好了再判断（见 `set_loading` 的说明）。
        self.set_loading(false);
    }

    /// 失败：清空表格 + 说清楚为什么（**中文 + 怎么办**）。
    fn fail(&self, msg: &str) {
        self.model.clear();
        self.containers.borrow_mut().clear();
        self.set_loading(false);
        self.status.set_text(msg);
    }

    /// **loading 动画**的开关（用户 2026-11-16）。
    ///
    /// ⚠️ **干什么用**：打开容器界面后要走 SSH 去远端跑 `docker ps`
    ///   ⇒ 要等一会儿 ⇒ 这期间列表是**空的** ⇒ 用户不知道程序是在干活
    ///   还是卡死了 ⇒ 给个**看得见的动画**。
    ///
    /// - 工具条的小转圈：**每次**取数据都转（自动刷新时也让用户知道
    ///   刚刚更新过）；
    /// - 列表正中间的大转圈：只在「正在加载 **且** 列表还是空的」时候
    ///   显示（**首次**加载）⇒ 经典的「正在加载」的样子。
    ///
    /// ⚠️ **顺序要紧**：大转圈要不要显示，取决于「列表空不空」⇒ 所以
    ///   **先填列表，再调这个函数**（否则会按旧数据判断）。
    fn set_loading(&self, loading: bool) {
        self.busy.set(loading);
        self.spin.set_visible(loading);
        if loading {
            self.spin.start();
        } else {
            self.spin.stop();
        }
        let big = loading && self.containers.borrow().is_empty();
        self.spin_big.set_visible(big);
        if big {
            self.spin_big.start();
        } else {
            self.spin_big.stop();
        }
    }

    /// 按「过滤 + 搜索」筛一遍。
    fn filter_rows(&self, rows: &[ContainerInfo]) -> Vec<ContainerInfo> {
        let f = self.filter.get();
        let q = self.search.borrow().to_ascii_lowercase();
        rows.iter()
            .filter(|r| match f {
                Filter::All => true,
                Filter::Running => r.state == "running",
                Filter::Stopped => r.state != "running",
            })
            .filter(|r| {
                if q.is_empty() {
                    return true;
                }
                r.name.to_ascii_lowercase().contains(&q)
                    || r.image.to_ascii_lowercase().contains(&q)
                    || r.id.to_ascii_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// 重新按当前过滤/搜索刷一遍表格（**不重新连服务器**）。
    pub fn refilter(&self) {
        let rows = self.containers.borrow().clone();
        let shown = self.filter_rows(&rows);
        self.rebuild(&shown);
        self.status.set_text(&trf!("共 {n} 个容器", n = shown.len()));
    }

    /// 表格**模型**里的全部行（**冒烟测试读它**）。
    ///
    /// 返回 `(名字, 是不是项目行, 是不是顶层行)`，**按模型顺序**
    /// （先父行后子行）。
    ///
    /// ⚠️ 这是**模型**的形状，**不是屏幕上显示了什么**——收起着的项目，
    ///    它的子行在模型里存在、但屏幕上不显示。屏幕上显示了什么由
    ///    冒烟用 `path_at_pos` 逐行问 GTK（见 `examples/docker_ui_smoke.rs`）。
    pub fn model_rows(&self) -> Vec<(String, bool, bool)> {
        let mut out = Vec::new();
        let mut it = match self.model.iter_first() {
            Some(i) => i,
            None => return out,
        };
        loop {
            let name: String = self.model.get(&it, COL_NAME as i32);
            let is_proj = self.model.iter_children(Some(&it)).is_some();
            out.push((name, is_proj, true));
            // 子行
            let mut sub = self.model.iter_children(Some(&it));
            while let Some(s) = sub {
                let n: String = self.model.get(&s, COL_NAME as i32);
                out.push((n, false, false));
                let mut next = s;
                if !self.model.iter_next(&mut next) {
                    break;
                }
                sub = Some(next);
            }
            let mut next = it;
            if !self.model.iter_next(&mut next) {
                break;
            }
            it = next;
        }
        out
    }

    /// 找到某个项目行的迭代器（**只找顶层行**）。
    fn project_iter(&self, name: &str) -> Option<gtk::TreeIter> {
        let mut it = self.model.iter_first()?;
        loop {
            let n: String = self.model.get(&it, COL_NAME as i32);
            if n == name && self.model.iter_children(Some(&it)).is_some() {
                return Some(it);
            }
            let mut next = it;
            if !self.model.iter_next(&mut next) {
                break;
            }
            it = next;
        }
        None
    }

    /* ---------------- 阶段 110：详情 ---------------- */

    /// **当前选中的容器名**（选中的是**项目行** ⇒ `None`）。
    fn selected_container_name(&self) -> Option<String> {
        let (path, _col) = gtk::prelude::TreeViewExt::cursor(&self.view);
        let path = path?;
        let it = self.model.iter(&path)?;
        // 项目行（有子行）不是容器 ⇒ 不取它的详情。
        if self.model.iter_children(Some(&it)).is_some() {
            return None;
        }
        let name: String = self.model.get(&it, COL_NAME as i32);
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// **当前选中的 compose 项目名**（选中的是**容器行** ⇒ `None`）。
    ///
    /// ⚠️ **怎么认出项目行**：项目行是**父行**（模型里有子行）⇒ 容器行没有。
    fn selected_project_name(&self) -> Option<String> {
        let (path, _col) = gtk::prelude::TreeViewExt::cursor(&self.view);
        let path = path?;
        let it = self.model.iter(&path)?;
        // 只有**有子行的**才是项目行。
        if self.model.iter_children(Some(&it)).is_none() {
            return None;
        }
        let name: String = self.model.get(&it, COL_NAME as i32);
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// 找到某一行（**容器名**）的路径（顶层 + 子行都找）。
    fn find_path(&self, name: &str) -> Option<gtk::TreePath> {
        let mut it = self.model.iter_first()?;
        loop {
            let n: String = self.model.get(&it, COL_NAME as i32);
            if n == name {
                return Some(self.model.path(&it));
            }
            // 子行
            let mut sub = self.model.iter_children(Some(&it));
            while let Some(s) = sub {
                let sn: String = self.model.get(&s, COL_NAME as i32);
                if sn == name {
                    return Some(self.model.path(&s));
                }
                let mut next = s;
                if !self.model.iter_next(&mut next) {
                    break;
                }
                sub = Some(next);
            }
            let mut next = it;
            if !self.model.iter_next(&mut next) {
                break;
            }
            it = next;
        }
        None
    }

    /// 详情取回来了 ⇒ 填四张卡片。
    fn apply_detail(&self, d: &ContainerDetail) {
        // ⚠️ **顺手记下选中的容器**（阶段 112 修的 bug）：消息循环拿它比对
        //    「这条结果是不是现在选的那个」⇒ 它必须**一直是对的**。
        *self.selected.borrow_mut() = d.name.clone();
        self.detail
            .title
            .set_text(&trf!("详情：{n}（{s}）", n = d.name, s = d.state_label()));
        self.detail.config.set_text(&detail_config_text(d));
        self.detail.net.set_text(&detail_net_text(d));
        self.detail.mount.set_text(&detail_mount_text(d));
        self.detail.env.set_text(&detail_env_text(d));
        // 按钮：运行中只能停/重启；停了才能启动。
        self.detail.btn_start.set_sensitive(!d.is_running());
        self.detail.btn_stop.set_sensitive(d.is_running());
        self.detail.btn_restart.set_sensitive(true);
        self.detail.root.set_visible(true);
    }

    /// 详情取失败 ⇒ 说清楚为什么（**别静默**）。
    fn detail_error(&self, msg: &str) {
        self.detail.title.set_text(tr("取不到容器详情"));
        self.detail.config.set_text(msg);
        self.detail.net.set_text("");
        self.detail.mount.set_text("");
        self.detail.env.set_text("");
        self.detail.root.set_visible(true);
    }

    /// 没选容器 ⇒ 详情区收起来。
    fn clear_detail(&self) {
        *self.selected.borrow_mut() = String::new();
        *self.selected_project.borrow_mut() = String::new();
        self.detail.root.set_visible(false);
        self.detail.proj_root.set_visible(false);
    }

    /* ---------------- 阶段 111：compose 项目详情 ---------------- */

    /// 一个 compose 项目的**汇总信息**（从容器列表里推出来）。
    ///
    /// ⚠️ compose 的配置文件路径与工作目录取自**容器的标签**
    ///    （`com.docker.compose.project.config_files` /
    ///    `com.docker.compose.project.working_dir`）⇒ 每个容器都带一份，
    ///    这里取第一个非空的。
    fn project_summary(&self, name: &str) -> Option<ProjectSummary> {
        let mut files = String::new();
        let mut dir = String::new();
        let mut items: Vec<(String, String)> = Vec::new();
        for c in self.containers.borrow().iter() {
            if c.compose != name {
                continue;
            }
            if files.is_empty() && !c.compose_files.is_empty() {
                files = c.compose_files.clone();
            }
            if dir.is_empty() && !c.compose_dir.is_empty() {
                dir = c.compose_dir.clone();
            }
            items.push((c.name.clone(), c.state_label().to_string()));
        }
        if items.is_empty() {
            return None;
        }
        Some(ProjectSummary {
            name: name.to_string(),
            files,
            dir,
            items,
        })
    }

    /// 选中了一个 **compose 项目行** ⇒ 显示项目详情（藏起容器详情）。
    fn show_project(&self, name: &str) {
        *self.selected.borrow_mut() = String::new();
        *self.selected_project.borrow_mut() = name.to_string();
        let Some(sum) = self.project_summary(name) else {
            self.clear_detail();
            return;
        };
        self.detail
            .proj_title
            .set_text(&trf!("compose 项目：{n}", n = sum.name));
        self.detail.proj_info.set_text(&project_info_text(&sum));
        // 「编辑」要 compose 文件 ⇒ 没有文件就禁用（别给用户一个点不动的按钮）。
        self.detail.btn_edit.set_sensitive(!sum.files.is_empty());
        self.detail.proj_root.set_visible(true);
        self.detail.root.set_visible(false);
    }

    /* ---------------- 阶段 112：日志 ---------------- */

    /// **日志页是不是正开着**（用户在看日志）。
    fn log_page_visible(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some(PAGE_LOGS)
    }

    /// **监控页**是不是正开着（**阶段 114**）。
    fn stat_page_visible(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some(PAGE_MONITOR)
    }

    /// 收到**一个采样点** ⇒ 追加到曲线 ＋ 更新指标卡 ＋ 重画四张图
    /// （**阶段 114**）。
    fn push_stat_point(&self, pt: docker::StatPoint) {
        self.monitor_data.borrow_mut().push(pt);
        // ---- 指标卡的当前值 ----
        self.monitor
            .val_cpu
            .set_text(&format!("{:.2}%", pt.cpu));
        self.monitor
            .val_mem
            .set_text(&format!("{:.2}%", pt.mem));
        self.monitor
            .val_net
            .set_text(&fmt_rate(pt.net_rx + pt.net_tx));
        self.monitor
            .val_blk
            .set_text(&fmt_rate(pt.blk_read + pt.blk_write));
        // ---- 重画四张图 ----
        //
        // ⚠️ **只重画这四张**（不是整页）⇒ 采样每 2 秒一次 ⇒ 开销很小。
        self.monitor.chart_cpu.queue_draw();
        self.monitor.chart_mem.queue_draw();
        self.monitor.chart_net.queue_draw();
        self.monitor.chart_blk.queue_draw();
    }

    /// 收到**一块**日志 ⇒ 拆成行、收下、该显示就显示。
    ///
    /// ⚠️ **跨块的一行要拼起来**：SSH/TCP 的分块跟行没关系 ⇒ 一行可能
    ///    被拆成好几块 ⇒ 半行先存着，等下一块补齐。
    fn log_push_text(&self, text: &str) {
        let mut rest = std::mem::take(&mut *self.log_partial.borrow_mut());
        rest.push_str(text);
        let mut done: Vec<String> = Vec::new();
        let mut tail = String::new();
        for part in rest.split_inclusive('\n') {
            match part.strip_suffix('\n') {
                Some(s) => done.push(s.trim_end_matches('\r').to_string()),
                // 最后一段没有换行 ⇒ 还没收全 ⇒ 留着等下一块。
                None => tail.push_str(part),
            }
        }
        *self.log_partial.borrow_mut() = tail;
        for l in done {
            self.log_push_line(&l);
        }
    }

    /// 收到**一行** ⇒ **收下**（暂停也收）⇒ 该显示就显示。
    fn log_push_line(&self, line: &str) {
        self.log_lines.borrow_mut().push(line.to_string());
        // ⚠️ **暂停**：数据照收，显示不更新（恢复时一次性补上）。
        if self.log_paused.get() {
            return;
        }
        if !self.log_line_matches(line) {
            return;
        }
        self.log_append(line);
        self.log_follow_end();
    }

    /// 这一行**要不要显示**（按搜索词过滤）。
    fn log_line_matches(&self, line: &str) -> bool {
        let q = self.log_search.borrow().to_ascii_lowercase();
        if q.is_empty() {
            return true;
        }
        line.to_ascii_lowercase().contains(&q)
    }

    /// **追加**一行到显示（**增量** ⇒ 不整表重画）。
    ///
    // ⚠️ 项目踩过「整表重画把主线程拖死」的坑（AGENTS.md）⇒ 日志流
    //    是最容易被灌爆的地方 ⇒ 只追加新来的那一行。
    fn log_append(&self, line: &str) {
        let mut end = self.log.buffer.end_iter();
        self.log.buffer.insert(&mut end, &format!("{line}\n"));
    }

    /// **按当前搜索词重建**显示（从已收到的全部行里挑出该显示的）。
    fn log_rebuild(&self) {
        let shown: Vec<String> = {
            let lines = self.log_lines.borrow();
            lines
                .iter()
                .filter(|l| self.log_line_matches(l))
                .cloned()
                .collect()
        };
        self.log.buffer.set_text("");
        for l in shown.iter() {
            self.log_append(l);
        }
        self.log_follow_end();
    }

    /// **清空**（显示 + 已收到的行都清掉）。
    fn log_clear(&self) {
        self.log_lines.borrow_mut().clear();
        self.log_partial.borrow_mut().clear();
        self.log.buffer.set_text("");
    }

    /// **重置**整个日志区（换容器 / 重新起流时用）。
    fn log_reset(&self) {
        self.log_clear();
        *self.log_name.borrow_mut() = String::new();
        self.log_paused.set(false);
        self.log.btn_pause.set_label(tr("暂停"));
    }

    /// **滚到底**（**自动跟随**开着才滚）。
    ///
    /// ⚠️ **等版面算完再滚**：刚插完文字时 adjustment 的 `upper` 还是旧的
    ///    ⇒ 直接滚会差一行 ⇒ 丢一个 idle，等 GTK 排完版再滚到底。
    fn log_follow_end(&self) {
        if !self.log.follow.is_active() {
            return;
        }
        let scroll = self.log.scroll.clone();
        glib::idle_add_local_once(move || {
            let adj = scroll.vadjustment();
            let lo = adj.lower();
            let hi = (adj.upper() - adj.page_size()).max(lo);
            adj.set_value(hi);
        });
    }
}

/// 时间戳（ISO8601）⇒ **本地时间**（看得懂）。
///
/// ⚠️ docker 给的是 UTC 的 RFC3339 ⇒ 直接显示用户得自己换算 ⇒ 换成本地。
fn fmt_time(s: &str) -> String {
    if s.trim().is_empty() {
        return tr("—").to_string();
    }
    match chrono::DateTime::parse_from_rfc3339(s.trim()) {
        Ok(t) => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => s.trim().to_string(),
    }
}

/// 「运行配置」卡片。
fn detail_config_text(d: &ContainerDetail) -> String {
    let mut v: Vec<String> = Vec::new();
    v.push(trf!("镜像：{x}", x = d.image));
    let mut st = d.state_label().to_string();
    if !d.health.is_empty() && d.health != "none" {
        st = trf!("{s}（{h}）", s = st, h = d.health);
    }
    v.push(trf!("状态：{s}", s = st));
    v.push(trf!("创建时间：{t}", t = fmt_time(&d.created)));
    v.push(trf!("启动时间：{t}", t = fmt_time(&d.started_at)));
    v.push(trf!("退出时间：{t}", t = fmt_time(&d.finished_at)));
    if !d.exit_code.is_empty() {
        v.push(trf!("退出码：{c}", c = d.exit_code));
    }
    v.push(trf!("重启策略：{p}", p = d.restart_policy));
    if !d.restart_count.is_empty() {
        v.push(trf!("重启次数：{c}", c = d.restart_count));
    }
    v.push(trf!(
        "运行用户：{u}",
        u = if d.user.is_empty() {
            tr("root")
        } else {
            d.user.as_str()
        }
    ));
    v.push(trf!("工作目录：{w}", w = d.workdir));
    if !d.command.is_empty() {
        v.push(trf!("命令：{c}", c = d.command));
    }
    v.join("\n")
}

/// 「网络与端口」卡片。
fn detail_net_text(d: &ContainerDetail) -> String {
    let mut v: Vec<String> = Vec::new();
    v.push(trf!("网络模式：{m}", m = d.network_mode));
    for n in d.networks.iter() {
        v.push(trf!(
            "网络：{name}（{ip}，网关 {gw}）",
            name = n.name,
            ip = n.ip,
            gw = n.gateway
        ));
    }
    for p in d.ports.iter() {
        if p.host_port.is_empty() {
            v.push(trf!("端口：{c}（未发布）", c = p.container));
        } else {
            v.push(trf!(
                "端口：{c} → {ip}:{hp}",
                c = p.container,
                ip = p.host_ip,
                hp = p.host_port
            ));
        }
    }
    if v.len() == 1 {
        v.push(tr("（没有网络信息）").to_string());
    }
    v.join("\n")
}

/// 「挂载」卡片。
fn detail_mount_text(d: &ContainerDetail) -> String {
    if d.mounts.is_empty() {
        return tr("（没有挂载）").to_string();
    }
    let mut v: Vec<String> = Vec::new();
    for m in d.mounts.iter() {
        v.push(trf!(
            "{kind}：{src} → {dst}（{rw}）",
            kind = m.kind,
            src = m.source,
            dst = m.dest,
            rw = if m.rw == "true" {
                tr("可写")
            } else {
                tr("只读")
            }
        ));
    }
    v.join("\n")
}

/// 「环境变量」卡片。
fn detail_env_text(d: &ContainerDetail) -> String {
    if d.env.is_empty() {
        return tr("（没有环境变量）").to_string();
    }
    d.env.join("\n")
}

/// 一个 compose 项目的**汇总**（阶段 111）。
#[derive(Clone, Debug, Default)]
struct ProjectSummary {
    /// 项目名。
    name: String,
    /// compose 配置文件路径（可能是逗号分隔的多个）。
    files: String,
    /// compose 工作目录。
    dir: String,
    /// 项目里的容器：`(名字, 状态)`。
    items: Vec<(String, String)>,
}

/// compose 项目详情的正文。
fn project_info_text(s: &ProjectSummary) -> String {
    let mut v: Vec<String> = Vec::new();
    if s.files.is_empty() {
        v.push(tr("（这个项目没有记录 compose 配置文件路径）").to_string());
    } else {
        v.push(trf!("配置文件：{f}", f = s.files));
    }
    if s.dir.is_empty() {
        v.push(tr("工作目录：（未知）").to_string());
    } else {
        v.push(trf!("工作目录：{d}", d = s.dir));
    }
    v.push(trf!("容器（{n} 个）：", n = s.items.len()));
    for (name, state) in &s.items {
        v.push(trf!("  {name}（{state}）", name = name, state = state));
    }
    v.join("\n")
}

/// 建界面（**只建一次**；挂在 [`PanelState::docker_ui`] 上）。
fn build(st: &Rc<PanelState>) -> Rc<DockerUi> {
    // ---- 工具条 ----
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr("搜索容器")));
    search.set_hexpand(true);

    // 过滤：三个切换按钮（互斥）
    let f_all = gtk::ToggleButton::with_label(tr("全部"));
    let f_run = gtk::ToggleButton::with_label(tr("运行中"));
    let f_stop = gtk::ToggleButton::with_label(tr("已停止"));
    f_all.set_active(true);
    // 互斥：点谁谁上，另外两个关掉（**像单选**⇒ 再点已选中的那个不会全灭）。
    //
    // ⚠️ 关掉别人会**又触发别人的 toggled** ⇒ 用一把锁挡一下（否则递归）。
    {
        let lock = Rc::new(Cell::new(false));
        let pairs = [
            (f_all.clone(), f_run.clone(), f_stop.clone()),
            (f_run.clone(), f_all.clone(), f_stop.clone()),
            (f_stop.clone(), f_all.clone(), f_run.clone()),
        ];
        for (btn, o1, o2) in pairs {
            let l = lock.clone();
            btn.connect_toggled(move |b| {
                if l.get() {
                    return;
                }
                if b.is_active() {
                    l.set(true);
                    o1.set_active(false);
                    o2.set_active(false);
                    l.set(false);
                } else {
                    // 点已选中的那个 ⇒ GTK 会把它关掉 ⇒ 恢复选中（单选语义）。
                    let any = [&o1, &o2].iter().any(|o| o.is_active());
                    if !any {
                        b.set_active(true);
                    }
                }
            });
        }
    }

    let last = gtk::Label::new(Some(tr("还没刷新")));
    last.add_css_class("dim-label");

    let auto_lbl = gtk::Label::new(Some(tr("自动刷新")));
    let auto = gtk::Switch::new();
    auto.set_valign(gtk::Align::Center);

    // ⚠️ 图标按钮**统一走 `a11y::icon_button`**（AGENTS.md 的 lint 钉死了）。
    let refresh_btn = crate::a11y::icon_button("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some(tr("刷新")));
    refresh_btn.add_css_class("flat");

    let spin = gtk::Spinner::new();
    spin.set_size_request(16, 16);
    spin.set_valign(gtk::Align::Center);
    spin.set_visible(false);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    bar.append(&search);
    bar.append(&f_all);
    bar.append(&f_run);
    bar.append(&f_stop);
    bar.append(&last);
    bar.append(&auto_lbl);
    bar.append(&auto);
    // ⚠️ loading 动画的小转圈（用户 2026-11-16）。
    bar.append(&spin);
    bar.append(&refresh_btn);

    // ---- 表格（**树形**）----
    let types: Vec<glib::Type> = (0..N_COLS).map(|_| glib::Type::STRING).collect();
    let model = gtk::TreeStore::new(&types);
    let view = gtk::TreeView::new();
    view.set_model(Some(&model));
    // 树形表格 ⇒ 画树线（父行与子行的连线），一眼看得出谁是谁的。
    view.set_enable_tree_lines(true);
    // 名称列的渲染器（**冒烟用它验「字真的画出来了」**）。
    let mut name_renderer: Option<gtk::CellRendererText> = None;
    {
        let titles = [
            tr("名称"),
            tr("ID"),
            tr("镜像"),
            tr("端口"),
            tr("CPU"),
            tr("内存"),
            tr("状态"),
            tr("详情"),
        ];
        for (i, t) in titles.iter().enumerate() {
            let r = gtk::CellRendererText::new();
            r.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let col = gtk::TreeViewColumn::new();
            col.set_title(t);
            col.set_resizable(true);
            col.pack_start(&r, false);
            // ⚠️ **必须显式绑定 "text"**：GTK4 的 `pack_start` 只把渲染器塞进列，
            //    **不会**把这一列的文本喂给它 ⇒ 不绑就是一片空白（列头还在，
            //    看着像「查出来是空的」）。项目里 `panel/dbgrid.rs` 踩过同一个坑。
            col.add_attribute(&r, "text", i as i32);
            // ⚠️ **列宽**：不设下限的话，GTK 按内容自动分配 ⇒「详情」列的
            //    长文本会把「名称/镜像」挤成「s…」「im…」（用户一眼看不到
            //    容器叫啥）⇒ 名称/详情两列 **expand**，其余给**下限**。
            match i {
                COL_NAME => {
                    col.set_min_width(170);
                    col.set_expand(true);
                }
                COL_ID => col.set_min_width(80),
                COL_IMAGE => col.set_min_width(130),
                COL_PORTS => col.set_min_width(100),
                COL_CPU => col.set_fixed_width(60),
                COL_MEM => col.set_fixed_width(80),
                COL_STATE => col.set_fixed_width(90),
                COL_DETAIL => {
                    col.set_min_width(170);
                    col.set_expand(true);
                }
                _ => {}
            }
            if i == COL_NAME {
                name_renderer = Some(r.clone());
            }
            view.append_column(&col);
        }
    }
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_child(Some(&view));

    // ---- 阶段 115：镜像 / 卷 / 网络 三张表 ----
    //
    // ⚠️ **平表（ListStore）**：这三样都没有层级 ⇒ 不用 TreeStore。
    let (images_store, images_view) = flat_table(
        &[
            tr("仓库"),
            tr("标签"),
            tr("ID"),
            tr("大小"),
            tr("创建时间"),
        ],
        &[130, 100, 90, 90, 170],
        4,
    );
    let (volumes_store, volumes_view) =
        flat_table(&[tr("名称"), tr("驱动")], &[220, 140], 0);
    let (networks_store, networks_view) =
        flat_table(&[tr("名称"), tr("驱动"), tr("范围")], &[220, 140, 100], 0);

    let images_root = flat_page(&images_view);
    let volumes_root = flat_page(&volumes_view);
    let networks_root = flat_page(&networks_view);

    // ---- 阶段 115：顶层的四个分段（容器 / 镜像 / 卷 / 网络）----
    //
    // ⚠️ **用 ToggleButton ＋ group**：点一个别的自动取消 ⇒ 表现像
    //   「分段按钮」而不是「四个复选框」。
    let seg_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    seg_bar.set_margin_start(6);
    seg_bar.set_margin_end(6);
    seg_bar.set_margin_top(4);
    let mut seg_buttons: Vec<(gtk::ToggleButton, &'static str)> = Vec::new();
    {
        let mut group: Option<gtk::ToggleButton> = None;
        for (label, key) in [
            (tr("容器"), SEG_CONTAINERS),
            (tr("镜像"), SEG_IMAGES),
            (tr("卷"), SEG_VOLUMES),
            (tr("网络"), SEG_NETWORKS),
        ] {
            let b = gtk::ToggleButton::new();
            b.set_label(label);
            b.add_css_class("flat");
            match &group {
                Some(g) => b.set_group(Some(g)),
                None => group = Some(b.clone()),
            }
            b.set_active(key == SEG_CONTAINERS);
            seg_bar.append(&b);
            seg_buttons.push((b, key));
        }
    }
    // ⚠️ **全局刷新按钮**：分段切换后，容器页工具条里的那个刷新按钮就
    //   看不见了 ⇒ 每个分段都要能刷新（图标按钮**统一走 a11y::icon_button**）。
    let seg_refresh = crate::a11y::icon_button("view-refresh-symbolic");
    seg_refresh.set_tooltip_text(Some(tr("刷新")));
    seg_refresh.add_css_class("flat");
    seg_refresh.set_valign(gtk::Align::Center);
    seg_bar.append(&seg_refresh);

    // ---- loading 动画（用户 2026-11-16）----
    //
    // ⚠️ **为什么要有**：打开容器界面后要走 SSH 去远端跑 `docker ps`
    //   ⇒ 要等一会儿 ⇒ 这期间列表是**空的** ⇒ 用户不知道是在干活还是卡死。
    //
    // ⚠️ **两个转圈的分工**：
    // - `spin_big`：**列表正中间**的大转圈 ⇒ 只在「正在加载 **且** 列表
    //   还是空的」时候显示（首次加载）⇒ 经典的「正在加载」的样子；
    // - `spin`：工具条里的小转圈 ⇒ **每次**取数据都转（自动刷新时也让
    //   用户知道刚刚更新过）。
    let spin_big = gtk::Spinner::new();
    spin_big.set_size_request(32, 32);
    spin_big.set_halign(gtk::Align::Center);
    spin_big.set_valign(gtk::Align::Center);
    spin_big.set_visible(false);
    let table_overlay = gtk::Overlay::new();
    table_overlay.set_child(Some(&scroll));
    table_overlay.add_overlay(&spin_big);

    // ---- 详情区（阶段 110：选中容器才显示）----
    //
    // 布局：标题行（容器名 + 状态 + 三个动作按钮）＋ 四张卡片（2×2 网格）。
    let d_title = gtk::Label::new(Some(""));
    d_title.set_xalign(0.0);
    d_title.set_hexpand(true);
    d_title.add_css_class("heading");

    // ⚠️ **动作按钮是写操作** ⇒ 点了必须**二次确认**（见下面的事件接线）。
    let mk_btn = |label: &str| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b
    };
    let d_start = mk_btn(tr("启动"));
    let d_stop = mk_btn(tr("停止"));
    let d_restart = mk_btn(tr("重启"));
    // 「终端」⇒ 开**容器内终端**（阶段 113）。
    //
    // ⚠️ **不是写操作**（只是进容器里跑个 shell）⇒ 不用二次确认。
    let d_term = mk_btn(tr("终端"));

    let d_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    d_row.set_margin_start(8);
    d_row.set_margin_end(8);
    d_row.set_margin_top(4);
    d_row.append(&d_title);
    d_row.append(&d_start);
    d_row.append(&d_stop);
    d_row.append(&d_restart);
    d_row.append(&d_term);

    // 一张卡片 = 标题 + 正文（正文自己能滚 ⇒ 内容再多也不撑破版面）。
    let mk_card = |title: &str| -> (gtk::Box, gtk::Label) {
        let head = gtk::Label::new(Some(title));
        head.set_xalign(0.0);
        head.add_css_class("heading");
        let body = gtk::Label::new(Some(""));
        body.set_xalign(0.0);
        body.set_yalign(0.0);
        body.set_wrap(true);
        body.set_selectable(true);
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_child(Some(&body));
        sw.set_vexpand(true);
        let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
        b.set_hexpand(true);
        b.set_vexpand(true);
        b.append(&head);
        b.append(&sw);
        (b, body)
    };
    let (c_config, card_config) = mk_card(tr("运行配置"));
    let (c_net, card_net) = mk_card(tr("网络与端口"));
    let (c_mount, card_mount) = mk_card(tr("挂载"));
    let (c_env, card_env) = mk_card(tr("环境变量"));

    let grid = gtk::Grid::new();
    grid.set_column_spacing(8);
    grid.set_row_spacing(6);
    grid.set_hexpand(true);
    grid.set_vexpand(true);
    grid.attach(&c_config, 0, 0, 1, 1);
    grid.attach(&c_net, 1, 0, 1, 1);
    grid.attach(&c_mount, 0, 1, 1, 1);
    grid.attach(&c_env, 1, 1, 1, 1);

    // ---- 日志页（阶段 112）----
    //
    // ⚠️ **等宽 + 只读**：日志是「给人看的文本」，不是能编辑的文档。
    let log_view = gtk::TextView::new();
    log_view.set_editable(false);
    log_view.set_cursor_visible(false);
    log_view.set_monospace(true);
    // ⚠️ **不自动换行**：日志行往往很长 ⇒ 换行会把时间戳和正文拆开，
    //    看不出哪行是哪行 ⇒ 横向滚动更合适。
    log_view.set_wrap_mode(gtk::WrapMode::None);
    log_view.set_left_margin(6);
    log_view.set_right_margin(6);
    log_view.set_top_margin(4);
    log_view.set_bottom_margin(4);
    let log_buffer = log_view.buffer().clone();

    let log_search = gtk::SearchEntry::new();
    log_search.set_placeholder_text(Some(tr("搜索日志")));
    log_search.set_hexpand(true);

    let log_pause = gtk::Button::with_label(tr("暂停"));
    log_pause.add_css_class("flat");
    let log_follow_lbl = gtk::Label::new(Some(tr("自动跟随")));
    let log_follow = gtk::Switch::new();
    log_follow.set_valign(gtk::Align::Center);
    // ⚠️ **默认开**：看日志的人基本都要「跟着最新日志滚」。
    log_follow.set_active(true);
    let log_clear = gtk::Button::with_label(tr("清空"));
    log_clear.add_css_class("flat");

    let log_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    log_bar.set_margin_start(6);
    log_bar.set_margin_end(6);
    log_bar.set_margin_top(4);
    log_bar.append(&log_search);
    log_bar.append(&log_pause);
    log_bar.append(&log_follow_lbl);
    log_bar.append(&log_follow);
    log_bar.append(&log_clear);

    let log_scroll = gtk::ScrolledWindow::new();
    log_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    log_scroll.set_child(Some(&log_view));
    log_scroll.set_vexpand(true);

    let log_status = gtk::Label::new(Some(""));
    log_status.set_xalign(0.0);
    log_status.set_margin_start(6);
    log_status.set_margin_end(6);
    log_status.set_margin_bottom(2);
    log_status.add_css_class("dim-label");

    let log_root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    log_root.set_margin_start(6);
    log_root.set_margin_end(6);
    log_root.set_margin_bottom(6);
    log_root.append(&log_bar);
    log_root.append(&log_scroll);
    log_root.append(&log_status);

    // ---- 监控页（阶段 114）：4 个指标卡 ＋ 4 张折线图 ----
    //
    // ⚠️ **数据是共享的**：四张图的绘制回调是 GTK 异步调的 ⇒ 它们得能
    //   拿到最新的点 ⇒ 数据放在 `Rc<RefCell<MonitorData>>` 里，四张图
    //   与界面**共享同一份**。
    let monitor_data = Rc::new(RefCell::new(MonitorData::default()));

    /// 建一张折线图（`kind` 决定画哪几条线）。
    let mk_chart = |kind: u8| -> gtk::DrawingArea {
        let data = Rc::downgrade(&monitor_data);
        let d = gtk::DrawingArea::new();
        d.set_hexpand(true);
        d.set_vexpand(true);
        d.set_content_height(120);
        d.set_draw_func(move |_a, cr, w, h| {
            // ⚠️ **回调只抓弱引用**（AGENTS.md 红线：不然引用环 ⇒ 面板释放不掉）。
            let Some(data) = data.upgrade() else { return };
            let d = data.borrow();
            // 几条线的颜色（**固定**，别每次算）。
            const BLUE: (f64, f64, f64) = (0.20, 0.45, 0.85);
            const GREEN: (f64, f64, f64) = (0.20, 0.65, 0.35);
            const ORANGE: (f64, f64, f64) = (0.85, 0.45, 0.15);
            const PURPLE: (f64, f64, f64) = (0.55, 0.35, 0.70);
            const GRAY: (f64, f64, f64) = (0.45, 0.45, 0.45);
            match kind {
                0 => draw_chart(cr, w, h, &[(&d.cpu, BLUE)], "%"),
                1 => draw_chart(cr, w, h, &[(&d.mem, GREEN)], "%"),
                2 => draw_chart(
                    cr,
                    w,
                    h,
                    &[(&d.net_rx, ORANGE), (&d.net_tx, PURPLE)],
                    "B/s",
                ),
                _ => draw_chart(
                    cr,
                    w,
                    h,
                    &[(&d.blk_read, GRAY), (&d.blk_write, BLUE)],
                    "B/s",
                ),
            }
        });
        d
    };

    /// 建一个指标卡（标题 ＋ 当前值）。
    let mk_card = |title: &str| -> (gtk::Box, gtk::Label) {
        let t = gtk::Label::new(Some(title));
        t.set_xalign(0.0);
        t.add_css_class("dim-label");
        let v = gtk::Label::new(Some("-"));
        v.set_xalign(0.0);
        v.add_css_class("heading");
        let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
        b.set_margin_start(8);
        b.set_margin_end(8);
        b.set_margin_top(4);
        b.append(&t);
        b.append(&v);
        (b, v)
    };

    // ⚠️ **变量名加 `m` 前缀**：详情区那边也有 `card_net` / `card_env`
    //   之类 ⇒ 同名会互相覆盖（编译器只报类型不匹配，不报「覆盖」）。
    let (mcard_cpu, val_cpu) = mk_card(tr("CPU"));
    let (mcard_mem, val_mem) = mk_card(tr("内存"));
    let (mcard_net, val_net) = mk_card(tr("网络"));
    let (mcard_blk, val_blk) = mk_card(tr("磁盘"));
    let cards = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cards.append(&mcard_cpu);
    cards.append(&mcard_mem);
    cards.append(&mcard_net);
    cards.append(&mcard_blk);

    let chart_cpu = mk_chart(0);
    let chart_mem = mk_chart(1);
    let chart_net = mk_chart(2);
    let chart_blk = mk_chart(3);
    let charts = gtk::Grid::new();
    charts.set_row_spacing(6);
    charts.set_column_spacing(6);
    charts.attach(&chart_cpu, 0, 0, 1, 1);
    charts.attach(&chart_mem, 1, 0, 1, 1);
    charts.attach(&chart_net, 0, 1, 1, 1);
    charts.attach(&chart_blk, 1, 1, 1, 1);

    let mon_status = gtk::Label::new(Some(""));
    mon_status.set_xalign(0.0);
    mon_status.set_margin_start(6);
    mon_status.set_margin_end(6);
    mon_status.set_margin_bottom(2);
    mon_status.add_css_class("dim-label");

    let mon_root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    mon_root.set_margin_start(6);
    mon_root.set_margin_end(6);
    mon_root.set_margin_bottom(6);
    mon_root.append(&cards);
    mon_root.append(&charts);
    mon_root.append(&mon_status);

    // ---- 文件页（阶段 116）：路径栏 ＋ 文件表 ＋ 上传 / 下载 / 查看 ----
    //
    // ⚠️ **容器里没有 SFTP** ⇒ 一切靠 `docker exec`（见 `backend/docker.rs`）。
    let f_path = gtk::Entry::new();
    f_path.set_placeholder_text(Some(tr("容器里的目录")));
    f_path.set_hexpand(true);
    let f_up = gtk::Button::with_label(tr("上一级"));
    let f_refresh = gtk::Button::with_label(tr("刷新"));
    let f_upload = gtk::Button::with_label(tr("上传文件…"));
    let f_uploaddir = gtk::Button::with_label(tr("上传目录…"));
    let f_download = gtk::Button::with_label(tr("下载…"));
    let f_view = gtk::Button::with_label(tr("查看…"));

    let f_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    f_bar.set_margin_start(6);
    f_bar.set_margin_end(6);
    f_bar.set_margin_top(4);
    f_bar.append(&f_path);
    f_bar.append(&f_up);
    f_bar.append(&f_refresh);

    let f_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    f_actions.set_margin_start(6);
    f_actions.set_margin_end(6);
    f_actions.set_margin_top(2);
    f_actions.append(&f_upload);
    f_actions.append(&f_uploaddir);
    f_actions.append(&f_download);
    f_actions.append(&f_view);

    let (f_store, f_view_tbl) = flat_table(
        &[tr("名称"), tr("类型"), tr("大小"), tr("权限"), tr("类型键")],
        &[220, 60, 100, 120, 80],
        0,
    );
    // ⚠️ **最后一列是内部用的类型键**（"dir" / "file" …）⇒ 不给用户看。
    if let Some(col) = f_view_tbl.columns().get(4) {
        col.set_visible(false);
    }
    let f_scroll = gtk::ScrolledWindow::new();
    f_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    f_scroll.set_vexpand(true);
    f_scroll.set_hexpand(true);
    f_scroll.set_child(Some(&f_view_tbl));

    let f_status = gtk::Label::new(Some(""));
    f_status.set_xalign(0.0);
    f_status.set_margin_start(6);
    f_status.set_margin_end(6);
    f_status.set_margin_bottom(2);
    f_status.add_css_class("dim-label");

    // ⚠️ **传输状态另起一行**（别跟列表状态挤在一起 ⇒ 见 `FilesUi` 的说明）。
    let f_xfer_status = gtk::Label::new(Some(""));
    f_xfer_status.set_xalign(0.0);
    f_xfer_status.set_margin_start(6);
    f_xfer_status.set_margin_end(6);
    f_xfer_status.set_margin_bottom(2);
    f_xfer_status.add_css_class("dim-label");

    let f_root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    f_root.set_margin_start(6);
    f_root.set_margin_end(6);
    f_root.set_margin_bottom(6);
    f_root.append(&f_bar);
    f_root.append(&f_actions);
    f_root.append(&f_scroll);
    f_root.append(&f_xfer_status);
    f_root.append(&f_status);

    // ⚠️ **回调要用**（控件要搬进 `DockerUi`）⇒ 先留一份句柄。
    let f_path_cb = f_path.clone();
    let f_view_cb = f_view_tbl.clone();

    // ---- 详情区：概览 / 日志 / 监控 三页（**Stack**）----
    //
    // ⚠️ 用 **Stack**（不是两个盒子轮流 `set_visible`）⇒ 页面切换是 GTK
    //    自己的事 ⇒ 不会出现「两个都显示」或「两个都不显示」。
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    // ⚠️ 页面的**标题**（页签上显示的字）要单独设 ⇒ 页名是内部标识
    //    （`overview` / `logs`），不能直接拿来做界面文案。
    let page_overview = stack.add_named(&grid, Some(PAGE_OVERVIEW));
    page_overview.set_title(tr("概览"));
    let page_logs = stack.add_named(&log_root, Some(PAGE_LOGS));
    page_logs.set_title(tr("日志"));
    // ⚠️ **阶段 114**：监控页（4 个指标卡 ＋ 4 张折线图）。
    let page_monitor = stack.add_named(&mon_root, Some(PAGE_MONITOR));
    page_monitor.set_title(tr("监控"));

    // ⚠️ **阶段 116**：文件页（浏览 ＋ 上传 ＋ 下载）。
    let page_files = stack.add_named(&f_root, Some(PAGE_FILES));
    page_files.set_title(tr("文件"));

    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_halign(gtk::Align::Start);

    let d_root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    d_root.set_margin_start(6);
    d_root.set_margin_end(6);
    d_root.set_margin_bottom(6);
    d_root.append(&d_row);
    d_root.append(&switcher);
    d_root.append(&stack);
    // ⚠️ **没选容器时不显示**（Paned 里隐藏的子控件不占地方 ⇒ 列表拿满）。
    d_root.set_visible(false);

    // ---- compose 项目详情（阶段 111：选中项目行时显示）----
    //
    // ⚠️ 与**容器详情互斥**：两块都在 Paned 的下半区，谁显示谁。
    let p_title = gtk::Label::new(Some(""));
    p_title.set_xalign(0.0);
    p_title.set_hexpand(true);
    p_title.add_css_class("heading");

    // 「编辑 compose 文件」+「启动」「停止」「销毁」「重建」。
    //
    // ⚠️ **图标按钮统一走 `a11y::icon_button`**（AGENTS.md 的 lint）——
    //    这里用的是**带文字的按钮**（不是图标）⇒ 不受那条约束。
    let p_edit = gtk::Button::with_label(tr("编辑 compose 文件"));
    p_edit.add_css_class("flat");
    let p_start = mk_btn(tr("启动"));
    let p_stop = mk_btn(tr("停止"));
    let p_down = mk_btn(tr("销毁"));
    let p_up = mk_btn(tr("重建"));

    let p_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    p_row.set_margin_start(8);
    p_row.set_margin_end(8);
    p_row.set_margin_top(4);
    p_row.append(&p_title);
    p_row.append(&p_edit);
    p_row.append(&p_start);
    p_row.append(&p_stop);
    p_row.append(&p_down);
    p_row.append(&p_up);

    let p_info = gtk::Label::new(Some(""));
    p_info.set_xalign(0.0);
    p_info.set_yalign(0.0);
    p_info.set_wrap(true);
    p_info.set_selectable(true);
    let p_scroll = gtk::ScrolledWindow::new();
    p_scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    p_scroll.set_child(Some(&p_info));
    p_scroll.set_vexpand(true);

    let p_root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    p_root.set_margin_start(6);
    p_root.set_margin_end(6);
    p_root.set_margin_bottom(6);
    p_root.append(&p_row);
    p_root.append(&p_scroll);
    p_root.set_visible(false);

    // 两块详情**装在同一个盒子里**（Paned 的下半区只有一块）。
    let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    detail_box.append(&d_root);
    detail_box.append(&p_root);

    // ---- 状态行 ----
    let status = gtk::Label::new(Some(tr("正在读取远程容器…")));
    status.set_xalign(0.0);
    status.set_margin_start(6);
    status.set_margin_end(6);
    status.set_margin_top(2);
    status.set_margin_bottom(2);

    // ---- 页面：上（工具条 + 表格）｜下（详情区）----
    //
    // ⚠️ 用 **Paned**（可拖）：详情区矮了就把它拖矮，别把列表挤没。
    let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
    top.append(&bar);
    // ⚠️ **用 Overlay 包一层**：大转圈要**在列表正中间**（loading 动画）。
    top.append(&table_overlay);

    let paned = gtk::Paned::new(gtk::Orientation::Vertical);
    paned.set_vexpand(true);
    paned.set_start_child(Some(&top));
    paned.set_end_child(Some(&detail_box));
    paned.set_position(360);

    // ---- 阶段 115：顶层四页（容器 / 镜像 / 卷 / 网络）----
    //
    // ⚠️ **容器那一整套**（工具条 + 表格 + 详情区）整体变成「容器」子页。
    let seg_stack = gtk::Stack::new();
    seg_stack.set_vexpand(true);
    seg_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    let _ = seg_stack.add_named(&paned, Some(SEG_CONTAINERS));
    let _ = seg_stack.add_named(&images_root, Some(SEG_IMAGES));
    let _ = seg_stack.add_named(&volumes_root, Some(SEG_VOLUMES));
    let _ = seg_stack.add_named(&networks_root, Some(SEG_NETWORKS));

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&seg_bar);
    page.append(&seg_stack);
    page.append(&status);

    // ---- 外层：给**编辑区**留一个落点（阶段 113 之后）----
    //
    // ⚠️ **为什么要有这一层**（用户 2026-11-16 报的）：
    //   「编辑 docker-compose 文件」按钮点了以后，文件编辑器打开了，
    //   **却不在容器界面**，用户会找不到 ⇒ 应该打开之后**用户马上能看到**，
    //   **不要跑到 term 界面**。
    //
    // ⇒ 根因：编辑区（阶段 107）是**一个共享的 widget**，它住在**终端列**
    //   下面 ⇒ 用户在**容器页**打开编辑器 ⇒ 编辑器建好了，却待在终端页里
    //   ⇒ **眼前什么也没变**。
    //
    // ⇒ 修法：容器页也给编辑区一个落点（就是这一列）⇒ 切到容器页时把
    //   编辑区搬过来（见 `SubTabs::host_editor_area_for_page_num`）。
    let column = gtk::Paned::new(gtk::Orientation::Vertical);
    column.set_vexpand(true);
    column.set_hexpand(true);
    column.set_start_child(Some(&page));
    column.set_end_child(None::<&gtk::Widget>);

    // ---- 结果通道（后台任务 → 主线程）----
    //
    // ⚠️ **为什么用通道而不是 `idle_add_local_once`**：后台任务跑在 tokio
    //    的**多线程**运行时上 ⇒ 它 spawn 的 future 必须是 `Send` ⇒
    //    **不能把 `Rc`/`Weak` 抓进去**（`Rc` 不是 `Send`）。
    // ⇒ 后台只发**纯数据**（`Msg`），主线程这边收。
    let (tx, rx) = async_channel::unbounded::<Msg>();

    let ui = Rc::new(DockerUi {
        page: column.clone().upcast(),
        column,
        tab_id: Cell::new(0),
        model,
        view: view.clone(),
        status,
        last,
        auto: auto.clone(),
        filter: Cell::new(Filter::All),
        search: RefCell::new(String::new()),
        busy: Cell::new(false),
        spin,
        spin_big,
        timer: Cell::new(None),
        tx,
        containers: RefCell::new(Vec::new()),
        version: RefCell::new(String::new()),
        name_renderer,
        detail: DetailUi {
            root: d_root,
            title: d_title,
            config: card_config,
            net: card_net,
            mount: card_mount,
            env: card_env,
            btn_start: d_start.clone(),
            btn_stop: d_stop.clone(),
            btn_restart: d_restart.clone(),
            btn_term: d_term.clone(),
            proj_root: p_root,
            proj_title: p_title,
            proj_info: p_info,
            btn_edit: p_edit.clone(),
            btn_cstart: p_start.clone(),
            btn_cstop: p_stop.clone(),
            btn_cdown: p_down.clone(),
            btn_cup: p_up.clone(),
        },
        log: LogUi {
            root: log_root,
            view: log_view,
            buffer: log_buffer,
            scroll: log_scroll,
            search: log_search.clone(),
            btn_pause: log_pause.clone(),
            btn_clear: log_clear.clone(),
            follow: log_follow.clone(),
            status: log_status,
        },
        stack: stack.clone(),
        log_name: RefCell::new(String::new()),
        log_lines: RefCell::new(Vec::new()),
        log_partial: RefCell::new(String::new()),
        log_paused: Cell::new(false),
        log_search: RefCell::new(String::new()),
        log_stop: RefCell::new(None),
        log_running: Cell::new(false),
        monitor: MonitorUi {
            root: mon_root,
            val_cpu,
            val_mem,
            val_net,
            val_blk,
            chart_cpu,
            chart_mem,
            chart_net,
            chart_blk,
            status: mon_status,
        },
        monitor_data: monitor_data.clone(),
        stat_name: RefCell::new(String::new()),
        stat_stop: RefCell::new(None),
        stat_running: Cell::new(false),
        stat_last_raw: RefCell::new(None),
        seg: seg_stack.clone(),
        seg_buttons: RefCell::new(seg_buttons.clone()),
        seg_current: Cell::new(SEG_CONTAINERS),
        images_store: images_store.clone(),
        images_view: images_view.clone(),
        volumes_store: volumes_store.clone(),
        volumes_view: volumes_view.clone(),
        networks_store: networks_store.clone(),
        networks_view: networks_view.clone(),
        images_busy: Cell::new(false),
        files: FilesUi {
            root: f_root,
            path: f_path,
            store: f_store,
            view: f_view_tbl,
            status: f_status,
            xfer_status: f_xfer_status,
        },
        files_name: RefCell::new(String::new()),
        files_path: RefCell::new(String::new()),
        files_xfer: Cell::new(false),
        files_seq: Cell::new(0),
        files_xfer_done: Cell::new(0),
        volumes_busy: Cell::new(false),
        networks_busy: Cell::new(false),
        term_chan: RefCell::new(None),
        term_running: Cell::new(false),
        term_win: RefCell::new(None),
        term_closing: Cell::new(false),
        term_view: RefCell::new(None),
        selected: RefCell::new(String::new()),
        selected_project: RefCell::new(String::new()),
        restoring: Cell::new(false),
        last_action: RefCell::new(String::new()),
    });

    // 主线程接收循环（**弱引用** ⇒ 面板关了这条循环自己会结束）。
    {
        let u = Rc::downgrade(&ui);
        let stw = Rc::downgrade(st);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(r) = rx.recv().await {
                let Some(u) = u.upgrade() else { break };
                match r {
                    Msg::List(Ok(info)) => u.apply(&info),
                    Msg::List(Err(msg)) => u.fail(&msg),
                    // ⚠️ **阶段 115**：三个新分段的结果。
                    //    ⚠️ **失败时只改状态行**（不清容器表）⇒ 切回容器页
                    //    还能看到原来的列表。
                    Msg::Images(Ok(v)) => {
                        u.images_busy.set(false);
                        apply_images(&u, &v);
                    }
                    Msg::Images(Err(msg)) => {
                        u.images_busy.set(false);
                        u.status.set_text(&msg);
                    }
                    Msg::Volumes(Ok(v)) => {
                        u.volumes_busy.set(false);
                        apply_volumes(&u, &v);
                    }
                    Msg::Volumes(Err(msg)) => {
                        u.volumes_busy.set(false);
                        u.status.set_text(&msg);
                    }
                    Msg::Networks(Ok(v)) => {
                        u.networks_busy.set(false);
                        apply_networks(&u, &v);
                    }
                    Msg::Networks(Err(msg)) => {
                        u.networks_busy.set(false);
                        u.status.set_text(&msg);
                    }
                    // ⚠️ **阶段 116**：文件列表 / 传输 / 查看。
                    Msg::Files { name, path, seq, r } => {
                        // ⚠️ **只认最新一次请求**（连点目录时旧结果会晚到
                        //   ⇒ 不挡一下就会盖掉新结果）。
                        if seq != u.files_seq.get() {
                            continue;
                        }
                        // ⚠️ **只认「现在这个容器的这个目录」**：换了容器或
                        //   换了目录以后，旧结果必须丢掉（不然界面上是上一个
                        //   目录的内容）。
                        if u.files_name.borrow().as_str() != name
                            || u.files_path.borrow().as_str() != path
                        {
                            continue;
                        }
                        match r {
                            Ok(v) => apply_files(&u, &v),
                            Err(msg) => u.files.status.set_text(&msg),
                        }
                    }
                    Msg::FileProgress { msg } => {
                        u.files.status.set_text(&msg);
                    }
                    Msg::FileXferDone { msg } => {
                        u.files_xfer.set(false);
                        u.files_xfer_done.set(u.files_xfer_done.get() + 1);
                        u.files.xfer_status.set_text(&msg);
                        // ⚠️ **传完重新列一遍**（远端已经变了）。
                        let name = u.selected.borrow().clone();
                        if !name.is_empty() {
                            let path = u.files_path.borrow().clone();
                            if let Some(st) = stw.upgrade() {
                                files_list(&st, &u, &name, &path);
                            }
                        }
                    }
                    Msg::FileView { path, r } => match r {
                        Ok(text) => show_file_window(stw.upgrade().as_ref(), &path, &text),
                        Err(msg) => {
                            if let Some(st) = stw.upgrade() {
                                crate::panel::dialogs::show_error(
                                    st.topwin().as_ref(),
                                    &tr("查看文件失败"),
                                    &msg,
                                );
                            }
                        }
                    },
                    Msg::Detail { name, r } => {
                        // ⚠️ **只认当前选中的那个**：用户连着点了两个容器 ⇒
                        //    先回来的那个可能已经过时了 ⇒ 丢掉。
                        if u.selected.borrow().as_str() != name {
                            continue;
                        }
                        match r {
                            Ok(d) => {
                                u.apply_detail(&d);
                                // ⚠️ **日志页正开着** ⇒ 这时容器才定下来 ⇒
                                //    现在起流（切页时还没选中容器，起不了）。
                                if u.log_page_visible() && !u.log_running.get() {
                                    if let Some(st) = stw.upgrade() {
                                        start_logs(&st, &u, &name);
                                    }
                                }
                                // ⚠️ **监控页正开着** ⇒ 同理（**阶段 114**）。
                                if u.stat_page_visible() && !u.stat_running.get() {
                                    if let Some(st) = stw.upgrade() {
                                        start_stats(&st, &u, &name);
                                    }
                                }
                                // ⚠️ **文件页正开着** ⇒ 同理（**阶段 116**）。
                                if files_page_visible(&u) {
                                    if let Some(st) = stw.upgrade() {
                                        files_ensure(&st, &u, &name);
                                    }
                                }
                            }
                            Err(msg) => u.detail_error(&msg),
                        }
                    }
                    Msg::LogChunk { name, data } => {
                        // ⚠️ **只认现在这条流的**（换了容器 ⇒ 旧流的数据丢掉）。
                        if u.log_name.borrow().as_str() != name {
                            continue;
                        }
                        u.log_push_text(&data);
                    }
                    Msg::LogEnd { name, code } => {
                        if u.log_name.borrow().as_str() != name {
                            continue;
                        }
                        u.log_running.set(false);
                        let msg = if code == 0 {
                            tr("日志流已结束").to_string()
                        } else {
                            trf!("日志流已结束（退出码 {c}）", c = code)
                        };
                        u.log.status.set_text(&msg);
                    }
                    Msg::LogError { name, msg } => {
                        if u.log_name.borrow().as_str() != name {
                            continue;
                        }
                        u.log_running.set(false);
                        u.log.status.set_text(&msg);
                    }
                    Msg::StatPoint { name, pt, raw } => {
                        // ⚠️ **只认现在这条流的**（换了容器 ⇒ 旧流的数据丢掉）。
                        if u.stat_name.borrow().as_str() != name {
                            continue;
                        }
                        // ⚠️ **留着原始行**（测试要拿它跟指标卡对照）。
                        *u.stat_last_raw.borrow_mut() = Some(raw);
                        u.push_stat_point(pt);
                    }
                    Msg::StatEnd { name, code } => {
                        if u.stat_name.borrow().as_str() != name {
                            continue;
                        }
                        u.stat_running.set(false);
                        let msg = if code == 0 {
                            tr("监控采样已结束").to_string()
                        } else {
                            trf!("监控采样已结束（退出码 {c}）", c = code)
                        };
                        u.monitor.status.set_text(&msg);
                    }
                    Msg::StatError { name, msg } => {
                        if u.stat_name.borrow().as_str() != name {
                            continue;
                        }
                        u.stat_running.set(false);
                        u.monitor.status.set_text(&msg);
                    }
                    Msg::Action(msg) => {
                        *u.last_action.borrow_mut() = msg.clone();
                        u.status.set_text(&msg);
                        // 动作完了 ⇒ 列表刷新（状态变了）＋ 详情重取。
                        if let Some(st) = stw.upgrade() {
                            refresh(&st, &u);
                            let name = u.selected.borrow().clone();
                            if !name.is_empty() {
                                load_detail(&st, &u, &name);
                            }
                        }
                    }
                }
            }
        });
    }

    // ---- 事件 ----
    // **选中一行 ⇒ 取它的详情**（阶段 110）
    //
    // ⚠️ 只对**容器行**响应（项目行是父行 ⇒ 选中它不取详情）。
    // ⚠️ `restoring` 是**程序性**恢复选中（刷新后恢复）⇒ 那时**不重新取**
    //    （不然每 7 秒自动刷新就多一条 docker 命令）。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        view.connect_cursor_changed(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            if u.restoring.get() {
                return;
            }
            // ⚠️ **项目行优先**：项目行是父行 ⇒ 选中它显示「项目详情」
            //    （编辑 / 启动 / 停止 / 销毁 / 重建），不是容器详情。
            if let Some(p) = u.selected_project_name() {
                u.show_project(&p);
                return;
            }
            match u.selected_container_name() {
                Some(n) => {
                    // ⚠️ **先记下选中的容器**（阶段 112 修的 bug）。
                    //
                    // ⚠️ **为什么必须在这儿记**：详情消息回来时要拿它比对
                    //    「这条结果是不是现在选的那个」⇒ 之前**只有测试钩子**
                    //    会写它 ⇒ **真实流程里详情消息全被当成过时的丢掉**
                    //    （详情区永远不显示、启停按钮点了没反应）。
                    *u.selected.borrow_mut() = n.clone();
                    // ⚠️ **换容器了** ⇒ 停掉上一条日志流（别让远端白挂着）。
                    if u.log_name.borrow().as_str() != n {
                        stop_logs(&u);
                        u.log_reset();
                    }
                    // ⚠️ **换容器了** ⇒ 监控流也得换（**阶段 114**）⇒
                    //    否则曲线还是上一个容器的（用户会以为看错了）。
                    if u.stat_name.borrow().as_str() != n {
                        stop_stats(&u);
                        u.monitor_data.borrow_mut().clear();
                        // 监控页正开着 ⇒ **立刻**起新流（不用等详情回来）。
                        if u.stat_page_visible() {
                            start_stats(&st, &u, &n);
                        }
                    }
                    load_detail(&st, &u, &n);
                }
                None => u.clear_detail(),
            }
        });
    }
    // **启停 / 重启**（**写操作 ⇒ 二次确认**）
    {
        for (btn, action) in [
            (d_start.clone(), Action::Start),
            (d_stop.clone(), Action::Stop),
            (d_restart.clone(), Action::Restart),
        ] {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            btn.connect_clicked(move |_| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                let name = u.selected.borrow().clone();
                if name.is_empty() {
                    return;
                }
                // ⚠️ **二次确认**：停掉用户服务器上的容器是**不可逆**的副作用 ⇒
                //    先问一句（默认焦点落在「取消」上 ⇒ 手快按回车不会误停）。
                let msg = trf!(
                    "确定要{act}容器「{name}」吗？",
                    act = action.label(),
                    name = name
                );
                let (st2, u2) = (st.clone(), u.clone());
                crate::panel::dialogs::confirm_title(
                    st.topwin().as_ref(),
                    tr("请确认"),
                    &msg,
                    action.label(),
                    move |ok| {
                        if !ok {
                            return;
                        }
                        do_action(&st2, &u2, action);
                    },
                );
            });
        }
    }
    // **容器内终端**（阶段 113）
    //
    // ⚠️ **不是写操作**（只是进容器里跑个 shell）⇒ 不用二次确认。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        d_term.connect_clicked(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            let name = u.selected.borrow().clone();
            if name.is_empty() {
                return;
            }
            open_container_terminal(&st, &u, &name);
        });
    }
    // **compose 项目的 5 个操作**（阶段 111）
    {
        // 「编辑 compose 文件」⇒ **打开远程文件**（不是写操作 ⇒ 不必确认）。
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            p_edit.connect_clicked(move |_| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                open_compose_file(&st, &u);
            });
        }
        // 启动 / 停止 / 销毁 / 重建（**写操作 ⇒ 二次确认**）。
        for (btn, action) in [
            (p_start.clone(), ComposeAction::Start),
            (p_stop.clone(), ComposeAction::Stop),
            (p_down.clone(), ComposeAction::Down),
            (p_up.clone(), ComposeAction::Up),
        ] {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            btn.connect_clicked(move |_| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                let name = u.selected_project.borrow().clone();
                if name.is_empty() {
                    return;
                }
                // ⚠️ **二次确认**：在用户服务器上动 compose 项目是**不可逆**的
                //    副作用（销毁还会删掉容器与网络）⇒ 先问一句（默认焦点在
                //    「取消」上 ⇒ 手快按回车不会误操作）。
                let msg = if action.is_dangerous() {
                    trf!(
                        "确定要{act} compose 项目「{name}」吗？（容器和网络会被删除）",
                        act = action.label(),
                        name = name
                    )
                } else {
                    trf!(
                        "确定要{act} compose 项目「{name}」吗？",
                        act = action.label(),
                        name = name
                    )
                };
                let (st2, u2) = (st.clone(), u.clone());
                crate::panel::dialogs::confirm_title(
                    st.topwin().as_ref(),
                    tr("请确认"),
                    &msg,
                    action.label(),
                    move |ok| {
                        if !ok {
                            return;
                        }
                        do_compose_action(&st2, &u2, action);
                    },
                );
            });
        }
    }
    // ================= 阶段 112：日志页 =================
    // **切到「日志」页 ⇒ 起流；切走 ⇒ 停流**（监控页同理，**阶段 114**）。
    //
    // ⚠️ **为什么切走就停**：`docker logs -f` / `docker stats` 是长跑的 ⇒
    //    一直挂着就是在用户的服务器上白占一个进程（还占着一条 SSH 通道）
    //    ⇒ 离开这页就该把它收掉。
    //
    // ⚠️ **两个流互斥**：日志页与监控页**只有一个可见** ⇒ 同一时刻只有
    //    一条流在跑（切页时另一个被停掉）⇒ 不会白占两条通道。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        stack.connect_visible_child_name_notify(move |s| {
            let Some(u) = u.upgrade() else { return };
            let page = s.visible_child_name();
            let on_logs = page.as_deref() == Some(PAGE_LOGS);
            let on_monitor = page.as_deref() == Some(PAGE_MONITOR);
            let on_files = page.as_deref() == Some(PAGE_FILES);
            if !on_logs {
                stop_logs(&u);
            }
            if !on_monitor {
                stop_stats(&u);
            }
            // ⚠️ **还没选中容器** ⇒ 等详情到了再起（见消息循环里的处理）。
            let name = u.selected.borrow().clone();
            if name.is_empty() {
                return;
            }
            let Some(st) = stw.upgrade() else { return };
            if on_logs && !u.log_running.get() {
                start_logs(&st, &u, &name);
            }
            if on_monitor && !u.stat_running.get() {
                start_stats(&st, &u, &name);
            }
            // ⚠️ **阶段 116**：切到「文件」页 ⇒ 列一次目录（换了容器 ⇒
            //   回到根目录重新列）。
            if on_files {
                files_ensure(&st, &u, &name);
            }
        });
    }
    // ================= 阶段 116：文件页 =================
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        // **双击目录 ⇒ 进入；双击文件 ⇒ 看内容**。
        f_view_cb.connect_row_activated(move |_, _, _| {
            let Some(u) = u.upgrade() else { return };
            let Some(st) = stw.upgrade() else { return };
            let container = u.selected.borrow().clone();
            if container.is_empty() {
                return;
            }
            let Some((name, kind)) = files_selected(&u) else { return };
            let remote = files_remote_path(&u, &name);
            if kind == "dir" {
                files_goto(&st, &u, &container, &remote);
            } else if kind == "file" {
                files_view(&st, &u, &container, &remote);
            }
        });
        // **「上一级」**
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            f_up.connect_clicked(move |_| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    return;
                }
                let cur = u.files_path.borrow().clone();
                if cur.is_empty() {
                    return;
                }
                let parent = crate::util::path_parent(&cur);
                files_goto(&st, &u, &container, &parent);
            });
        }
        // **「刷新」**
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            f_refresh.connect_clicked(move |_| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    return;
                }
                let cur = u.files_path.borrow().clone();
                files_goto(&st, &u, &container, &cur);
            });
        }
        // **路径栏回车 ⇒ 去那个目录**
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            f_path_cb.connect_activate(move |e| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    return;
                }
                let p = e.text().trim().to_string();
                if p.is_empty() {
                    return;
                }
                files_goto(&st, &u, &container, &p);
            });
        }
        // **「上传文件…」/「上传目录…」**
        for (btn, is_dir) in [(f_upload, false), (f_uploaddir, true)] {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            btn.connect_clicked(move |_| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    crate::panel::dialogs::show_error(
                        st.topwin().as_ref(),
                        &tr("先选一个容器"),
                        &tr("选中一个容器之后才能上传文件"),
                    );
                    return;
                }
                let top = st.topwin();
                let mut b = gtk::FileChooserNative::builder()
                    .modal(true)
                    .title(if is_dir {
                        tr("选要上传的目录")
                    } else {
                        tr("选要上传的文件")
                    })
                    .action(if is_dir {
                        gtk::FileChooserAction::SelectFolder
                    } else {
                        gtk::FileChooserAction::Open
                    })
                    .accept_label(tr("上传"))
                    .cancel_label(tr("取消"));
                if let Some(p) = top.as_ref() {
                    b = b.transient_for(p);
                }
                let ch = b.build();
                let st2 = Rc::clone(&st);
                let u2 = Rc::clone(&u);
                let container2 = container.clone();
                ch.connect_response(move |c, r| {
                    if r == gtk::ResponseType::Accept {
                        if let Some(f) = c.file() {
                            if let Some(p) = f.path() {
                                let local = p.clone();
                                let st3 = Rc::clone(&st2);
                                let u3 = Rc::clone(&u2);
                                let cname = container2.clone();
                                let remote_dir = u2.files_path.borrow().clone();
                                let fname = local
                                    .file_name()
                                    .map(|s| s.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                // ⚠️ **上传是写操作** ⇒ 二次确认（默认焦点在「取消」）。
                                let msg = trf!(
                                    "确定要把「{n}」上传到容器「{c}」的 {d} 吗？",
                                    n = fname,
                                    c = cname,
                                    d = remote_dir
                                );
                                crate::panel::dialogs::confirm_title(
                                    st3.topwin().as_ref(),
                                    tr("请确认"),
                                    &msg,
                                    tr("上传"),
                                    move |ok| {
                                        if !ok {
                                            return;
                                        }
                                        files_upload(&st3, &u3, &cname, &local);
                                    },
                                );
                            }
                        }
                    }
                    c.destroy();
                });
                ch.show();
            });
        }
        // **「下载…」**
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            f_download.connect_clicked(move |_| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    crate::panel::dialogs::show_error(
                        st.topwin().as_ref(),
                        &tr("先选一个容器"),
                        &tr("选中一个容器之后才能下载文件"),
                    );
                    return;
                }
                let Some((name, kind)) = files_selected(&u) else {
                    crate::panel::dialogs::show_error(
                        st.topwin().as_ref(),
                        &tr("先选一个条目"),
                        &tr("先在列表里选中一个条目，再点「下载…」"),
                    );
                    return;
                };
                let remote = files_remote_path(&u, &name);
                let is_dir = kind == "dir";
                let top = st.topwin();
                let mut b = gtk::FileChooserNative::builder()
                    .modal(true)
                    .title(tr("选下载到哪个目录"))
                    .action(gtk::FileChooserAction::SelectFolder)
                    .accept_label(tr("下载"))
                    .cancel_label(tr("取消"));
                if let Some(p) = top.as_ref() {
                    b = b.transient_for(p);
                }
                let ch = b.build();
                let st2 = Rc::clone(&st);
                let u2 = Rc::clone(&u);
                let container2 = container.clone();
                ch.connect_response(move |c, r| {
                    if r == gtk::ResponseType::Accept {
                        if let Some(f) = c.file() {
                            if let Some(p) = f.path() {
                                files_download(&st2, &u2, &container2, &remote, &p);
                            }
                        }
                    }
                    c.destroy();
                });
                ch.show();
            });
        }
        // **「查看…」**
        {
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            f_view.connect_clicked(move |_| {
                let Some(u) = u.upgrade() else { return };
                let Some(st) = stw.upgrade() else { return };
                let container = u.selected.borrow().clone();
                if container.is_empty() {
                    return;
                }
                let Some((name, kind)) = files_selected(&u) else {
                    crate::panel::dialogs::show_error(
                        st.topwin().as_ref(),
                        &tr("先选一个条目"),
                        &tr("先在列表里选中一个文件，再点「查看…」"),
                    );
                    return;
                };
                if kind == "dir" {
                    crate::panel::dialogs::show_error(
                        st.topwin().as_ref(),
                        &tr("看不了目录"),
                        &tr("目录请用「下载…」整个拉下来"),
                    );
                    return;
                }
                let remote = files_remote_path(&u, &name);
                files_view(&st, &u, &container, &remote);
            });
        }
    }
    // **暂停 / 继续**
    //
    // ⚠️ **暂停不是「不收」**：数据照收进 `log_lines`，只是**不往显示里
    //    加** ⇒ 恢复时一次性补上（用户没丢日志）。
    {
        let u = Rc::downgrade(&ui);
        log_pause.connect_clicked(move |_| {
            let Some(u) = u.upgrade() else { return };
            let paused = !u.log_paused.get();
            u.log_paused.set(paused);
            u.log
                .btn_pause
                .set_label(if paused { tr("继续") } else { tr("暂停") });
            if !paused {
                // 恢复 ⇒ 把暂停期间收到的行补上。
                u.log_rebuild();
            }
        });
    }
    // **搜索日志**
    {
        let u = Rc::downgrade(&ui);
        log_search.connect_search_changed(move |s| {
            let Some(u) = u.upgrade() else { return };
            *u.log_search.borrow_mut() = s.text().to_string();
            u.log_rebuild();
        });
    }
    // **清空日志**
    {
        let u = Rc::downgrade(&ui);
        log_clear.connect_clicked(move |_| {
            let Some(u) = u.upgrade() else { return };
            u.log_clear();
        });
    }
    // **自动跟随**：打开时立刻滚到底。
    {
        let u = Rc::downgrade(&ui);
        log_follow.connect_state_set(move |_s, on| {
            let Some(u) = u.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if on {
                u.log_follow_end();
            }
            glib::Propagation::Proceed
        });
    }
    // 搜索框一变就重新过滤（**不重新连服务器**）
    {
        let u = Rc::downgrade(&ui);
        search.connect_search_changed(move |s| {
            let Some(u) = u.upgrade() else { return };
            *u.search.borrow_mut() = s.text().to_string();
            u.refilter();
        });
    }
    // 三个过滤按钮
    {
        let u = Rc::downgrade(&ui);
        for (btn, f) in [
            (f_all, Filter::All),
            (f_run, Filter::Running),
            (f_stop, Filter::Stopped),
        ] {
            let u = u.clone();
            btn.connect_toggled(move |_| {
                let Some(u) = u.upgrade() else { return };
                u.filter.set(f);
                u.refilter();
            });
        }
    }
    // 「刷新」按钮
    //
    // ⚠️ **抓弱引用**（AGENTS.md 引用环红线）：界面控件在 `PanelState` 里，
    // 回调里再抓 `PanelState` 的**强引用** ⇒ 控件 → PanelState → 控件
    // 成环 ⇒ 面板永远释放不掉。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        refresh_btn.connect_clicked(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            refresh(&st, &u);
        });
    }
    // 自动刷新开关
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        auto.connect_state_set(move |_s, on| {
            let Some(u) = u.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if on {
                if let Some(st) = stw.upgrade() {
                    start_auto(&st, &u);
                }
            } else {
                stop_auto(&u);
            }
            glib::Propagation::Proceed
        });
    }
    // **双击项目行 = 展开/收起**（点箭头是 GTK 自带的，这里多给一种手势）。
    //
    // ---- 阶段 115：分段切换 ----
    //
    // ⚠️ **切过去就取一次数据**（懒加载）⇒ 用户不用先点「刷新」。
    // ⚠️ **group 里切换会发两次 `toggled`**（旧的变 false、新的变 true）
    //   ⇒ 只在 `is_active()` 时才动手。
    {
        for (btn, key) in seg_buttons.clone() {
            // ⚠️ **每个按钮各自一份弱引用**：弱引用不是 Copy ⇒ 在循环里
            //   clone 一份（循环外的那份 move 进第一个闭包就废了）。
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            // ⚠️ **闭包捕获一份克隆**：按钮本身还要留着去 `connect_toggled`
            //   ⇒ 直接 move 进闭包就把它借走了（编译不过）。
            let btn_c = btn.clone();
            btn.connect_toggled(move |_| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                if !btn_c.is_active() {
                    return;
                }
                u.seg_current.set(key);
                u.seg.set_visible_child_name(key);
                refresh_segment(&st, &u);
            });
        }
        // ⚠️ **刷新按钮**：刷**当前分段**（不是永远刷容器）。
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        seg_refresh.connect_clicked(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            match u.seg_current.get() {
                SEG_IMAGES => refresh_images(&st, &u),
                SEG_VOLUMES => refresh_volumes(&st, &u),
                SEG_NETWORKS => refresh_networks(&st, &u),
                _ => {}
            }
        });
    }
    // ⚠️ GTK4 的 `row_activated` 是**双击**才发的（单击不发）⇒
    //    单击项目行不会误触发。
    {
        let u = Rc::downgrade(&ui);
        view.connect_row_activated(move |v, path, _col| {
            let Some(u) = u.upgrade() else { return };
            // 只有**有子行的行**（项目行）才响应。
            let Some(it) = u.model.iter(path) else {
                return;
            };
            if u.model.iter_children(Some(&it)).is_none() {
                return;
            }
            if v.row_expanded(path) {
                v.collapse_row(path);
            } else {
                v.expand_row(path, false);
            }
        });
    }
    ui
}

/// 开/关自动刷新定时器。
fn start_auto(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    stop_auto(ui);
    let stw = Rc::downgrade(st);
    let u = Rc::downgrade(ui);
    let id = glib::timeout_add_local(std::time::Duration::from_secs(
        AUTO_REFRESH_SECS as u64,
    ), move || {
        let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        if u.auto.is_active() {
            // ⚠️ **阶段 115**：自动刷新刷的是**当前分段**（在镜像页就别
            //   去刷容器了 ⇒ 少一条远端命令）。
            refresh_segment(&st, &u);
        }
        glib::ControlFlow::Continue
    });
    ui.timer.set(Some(id));
}

/// 停掉自动刷新（页签关掉时也要停）。
pub fn stop_auto(ui: &Rc<DockerUi>) {
    if let Some(id) = ui.timer.take() {
        id.remove();
    }
}

/// 在**共享连接**上取一次容器列表（后台任务里跑）。
///
/// ⚠️ 这个函数**不许碰任何 GTK 控件**（后台线程）⇒ 只带可 Send 的数据
/// （连接参数 + 密码）进异步任务，结果回主线程再更新界面。
async fn fetch_all(
    opts: &crate::args::ConnOptions,
    pw: Option<String>,
) -> Result<DockerInfo, String> {
    let session = shared_session(opts, pw).await?;
    let version = docker::probe(&session).await.map_err(|e| e.0)?;
    let containers = docker::list(&session).await.map_err(|e| e.0)?;
    Ok(DockerInfo {
        containers,
        version,
    })
}

/// 在**共享连接**上拿一个 session（⚠️ **不新增连接**）。
///
/// ⚠️ 同一台服务器（host+port+user）整个程序只有一条 SSH 连接 ⇒ 容器视图的
///    列表、详情、启停**全都走这一条**（各开各的 exec 通道）。
async fn shared_session(
    opts: &crate::args::ConnOptions,
    pw: Option<String>,
) -> Result<
    std::sync::Arc<russh::client::Handle<crate::backend::conn::HostKeyHandler>>,
    String,
> {
    let creds = crate::backend::hub::Creds {
        password: pw,
        keyfile: opts.keyfile.clone().filter(|k| !k.is_empty()),
        proxycmd: opts.proxycmd.clone().filter(|c| !c.is_empty()),
        keepalive_sec: opts.keepalive_sec,
        auth_method: opts.auth_method,
        auth_order: opts.auth_order,
    };
    let key = crate::backend::hub::ServerKey::new(&opts.host, opts.port, &opts.user);
    // ⚠️ **共享连接**：同一台服务器在整个程序里只有一条 SSH 连接 ⇒
    //    容器视图**不新增连接**，只是在这条连接上开 exec 通道。
    let shared = crate::backend::hub::acquire(key, &creds)
        .await
        .map_err(|e| e.0)?;
    Ok(shared.session().clone())
}

/// 取一个容器的详情（后台任务里跑，结果回主线程）。
fn load_detail(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => docker::fetch_detail(&session, &name)
                .await
                .map_err(|e| e.0),
            Err(e) => Err(e),
        };
        let _ = tx.send_blocking(Msg::Detail { name, r });
    });
}

/// 起一条**日志流**（后台任务读，增量发回主线程）。
///
/// ⚠️ **不新增 SSH 连接**：走**共享连接**上的**独立 exec 通道**
///    （通道很轻 ⇒ 一台服务器仍然只有一条连接）。
/// ⚠️ **能停**：用户切走日志页 ⇒ 发停止信号 ⇒ 后台任务关掉通道 ⇒
///    远端的 `docker logs -f` 退出（服务器上不留进程）。
fn start_logs(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    stop_logs(ui);
    ui.log_reset();
    *ui.log_name.borrow_mut() = name.to_string();
    ui.log_running.set(true);
    ui.log.status.set_text(tr("正在接收日志…"));

    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let tx = ui.tx.clone();
    // ⚠️ **停止信号**：`oneshot` ⇒ 发一次就醒 ⇒ 后台任务不用等下一块数据
    //    就能收尾。
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    *ui.log_stop.borrow_mut() = Some(stop_tx);

    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let session = match shared_session(&opts, pw).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send_blocking(Msg::LogError { name, msg: e });
                return;
            }
        };
        let ch = match docker::open_logs(&session, &name, LOG_TAIL).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send_blocking(Msg::LogError { name, msg: e.0 });
                return;
            }
        };
        // ⚠️ **停止信号要能被反复问**：`select!` 会把 future 移走 ⇒ 用
        //    `&mut` 问它（`oneshot::Receiver` 是 Unpin ⇒ 取消安全）。
        let mut stop_rx = stop_rx;
        loop {
            tokio::select! {
                ev = ch.next_event() => {
                    match ev {
                        Some(ShellEvent::Data(b)) => {
                            let data = String::from_utf8_lossy(&b).into_owned();
                            if tx
                                .send_blocking(Msg::LogChunk { name: name.clone(), data })
                                .is_err()
                            {
                                // 界面没了 ⇒ 关通道（别让远端白挂着）。
                                ch.close();
                                break;
                            }
                        }
                        Some(ShellEvent::Exit(code)) => {
                            let _ = tx.send_blocking(Msg::LogEnd { name, code });
                            break;
                        }
                        None => {
                            let _ = tx.send_blocking(Msg::LogEnd { name, code: 0 });
                            break;
                        }
                    }
                }
                _ = &mut stop_rx => {
                    // ⚠️ **主动关通道** ⇒ 远端 `docker logs -f` 退出
                    //    （看门狗收尾 ⇒ 服务器上不留进程）。
                    ch.close();
                    break;
                }
            }
        }
    });
}

/// 停掉日志流（**关通道** ⇒ 远端的 `docker logs -f` 退出）。
pub fn stop_logs(ui: &Rc<DockerUi>) {
    if let Some(tx) = ui.log_stop.borrow_mut().take() {
        let _ = tx.send(());
    }
    ui.log_running.set(false);
}

/// 起一条**监控采样**流（`docker stats` 持续模式，**阶段 114**）。
///
/// ⚠️ **走共享连接**上的独立 exec 通道 ⇒ 一台服务器仍然只有一条 SSH 连接。
/// ⚠️ **后台做节流**：docker stats 每 500ms 刷一次 ⇒ 每 `STATS_INTERVAL`
///   秒才出一个点（曲线才好看、消息才不多）。
/// ⚠️ **半行缓冲**：一次 `Data` 里可能只有半行 ⇒ 拼起来再解析。
fn start_stats(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    stop_stats(ui);
    ui.monitor_data.borrow_mut().clear();
    *ui.stat_name.borrow_mut() = name.to_string();
    ui.stat_running.set(true);
    ui.monitor.status.set_text(tr("正在采样…"));

    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let tx = ui.tx.clone();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    *ui.stat_stop.borrow_mut() = Some(stop_tx);

    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let session = match shared_session(&opts, pw).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send_blocking(Msg::StatError { name, msg: e });
                return;
            }
        };
        let ch = match docker::open_stats(&session, &name).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send_blocking(Msg::StatError { name, msg: e.0 });
                return;
            }
        };
        let mut sampler = docker::StatsSampler::default();
        let mut partial = String::new();
        let t0 = std::time::Instant::now();
        // ⚠️ **停止信号要能被反复问**：`select!` 会把 future 移走 ⇒ 用
        //    `&mut` 问它（`oneshot::Receiver` 是 Unpin ⇒ 取消安全）。
        let mut stop_rx = stop_rx;
        loop {
            tokio::select! {
                ev = ch.next_event() => {
                    match ev {
                        Some(ShellEvent::Data(b)) => {
                            partial.push_str(&String::from_utf8_lossy(&b));
                            // ⚠️ **按行切**（半行留着 ⇒ 下次拼上）。
                            while let Some(i) = partial.find('\n') {
                                let line: String = partial.drain(..=i).collect();
                                let line = line.trim_end_matches(['\r', '\n']);
                                let Some(sample) = docker::parse_stats_line(line) else {
                                    continue;
                                };
                                let now = t0.elapsed().as_secs_f64();
                                let Some(pt) = sampler.push(&sample, now, STATS_INTERVAL) else {
                                    continue;
                                };
                                if tx
                                    .send_blocking(Msg::StatPoint {
                                        name: name.clone(),
                                        pt,
                                        raw: line.to_string(),
                                    })
                                    .is_err()
                                {
                                    // 界面没了 ⇒ 关通道（别让远端白挂着）。
                                    ch.close();
                                    break;
                                }
                            }
                        }
                        Some(ShellEvent::Exit(code)) => {
                            let _ = tx.send_blocking(Msg::StatEnd { name, code });
                            break;
                        }
                        None => {
                            let _ = tx.send_blocking(Msg::StatEnd { name, code: 0 });
                            break;
                        }
                    }
                }
                _ = &mut stop_rx => {
                    // ⚠️ **主动关通道** ⇒ 远端 `docker stats` 退出
                    //    （看门狗收尾 ⇒ 服务器上不留进程）。
                    ch.close();
                    break;
                }
            }
        }
    });
}

/// 停掉监控流（**关通道** ⇒ 远端的 `docker stats` 退出）。
pub fn stop_stats(ui: &Rc<DockerUi>) {
    if let Some(tx) = ui.stat_stop.borrow_mut().take() {
        let _ = tx.send(());
    }
    ui.stat_running.set(false);
}

/// 做一次**写操作**（启停 / 重启）。
///
/// ⚠️ **界面必须先二次确认**（见 `build()` 里的按钮接线）⇒ 这里只管执行。
fn do_action(st: &Rc<PanelState>, ui: &Rc<DockerUi>, action: Action) {
    let name = ui.selected.borrow().clone();
    if name.is_empty() {
        return;
    }
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name2 = name.clone();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => docker::run_action(&session, &name2, action)
                .await
                .map_err(|e| e.0),
            Err(e) => Err(e),
        };
        let msg = match r {
            Ok(m) => trf!("{n}：{m}", n = name2, m = m),
            Err(e) => trf!("{act}失败：{e}", act = action.label(), e = e),
        };
        let _ = tx.send_blocking(Msg::Action(msg));
    });
}

/// 做一次 **compose 项目的写操作**（启动 / 停止 / 销毁 / 重建）。
///
/// ⚠️ **界面必须先二次确认**（见 `build()` 里的按钮接线）⇒ 这里只管执行。
fn do_compose_action(st: &Rc<PanelState>, ui: &Rc<DockerUi>, action: ComposeAction) {
    let name = ui.selected_project.borrow().clone();
    if name.is_empty() {
        return;
    }
    // 配置文件与工作目录取自**容器标签**（项目里每个容器都带一份）。
    let Some(sum) = ui.project_summary(&name) else {
        ui.status.set_text(tr("找不到这个 compose 项目的容器"));
        return;
    };
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name2 = name.clone();
    let files = sum.files.clone();
    let dir = sum.dir.clone();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => {
                docker::run_compose_action(&session, &name2, &files, &dir, action)
                    .await
                    .map_err(|e| e.0)
            }
            Err(e) => Err(e),
        };
        let msg = match r {
            Ok(m) => trf!("{n}：{m}", n = name2, m = m),
            Err(e) => trf!("{act}失败：{e}", act = action.label(), e = e),
        };
        let _ = tx.send_blocking(Msg::Action(msg));
    });
}

/// 打开**容器内终端**（`docker exec -it`；阶段 113）。
///
/// ⚠️ **走共享连接**上的独立 exec 通道（不新增 SSH 连接）。
/// ⚠️ **容器里可能没有 shell** ⇒ 挨个试 `/bin/sh` → `/bin/bash` → … ⇒
///    全都没有才报错（说清楚试了哪些）。
/// ⚠️ **终端另开一个窗口**：容器详情区只有一小块 ⇒ 终端塞进去只有几行
///    （用户没法用）⇒ 单独一个窗口（跟「外观窗口」「凭据库管理器」一样）。
fn open_container_terminal(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    // 已经在开着 ⇒ 别开第二个（远端会多一个孤儿 shell）。
    if ui.term_running.get() {
        ui.status.set_text(tr("容器终端已经开着（在另一个窗口里）"));
        return;
    }

    // ⚠️ **后端跟主终端一致**（用户在首选项里选了 ghostty ⇒ 容器终端也
    //    是 ghostty）⇒ 行为统一。
    let term = crate::term::new_term_backend();
    let view = term.widget().clone();
    term.set_expand(true, true);

    let win = gtk::Window::new();
    win.set_title(Some(&trf!("容器终端：{n}", n = name)));
    win.set_default_size(900, 560);
    let status = gtk::Label::new(Some(tr("正在打开容器终端…")));
    status.set_xalign(0.0);
    status.set_margin_start(6);
    status.set_margin_end(6);
    status.set_margin_bottom(4);
    status.add_css_class("dim-label");
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&view);
    vbox.append(&status);
    win.set_child(Some(&vbox));
    win.present();
    // ⚠️ **记下窗口句柄**（测试钩子要能关掉它）。
    *ui.term_win.borrow_mut() = Some(win.clone());

    // ⚠️ **窗口关掉 ⇒ 远端那条 `docker exec` 也要收掉**（否则容器里会留下
    //    一个孤儿 shell 一直占着）。
    {
        let u = Rc::downgrade(ui);
        win.connect_close_request(move |_| {
            if let Some(u) = u.upgrade() {
                stop_terminal(&u);
            }
            glib::Propagation::Proceed
        });
    }

    // ⚠️ **尺寸同步不用自己写**：两个后端都自带 ——
    //    VTE 后端每 300ms 看一眼行列数（变了才发 window-change），
    //    ghostty 后端在 `set_term_size` 里发（内核与 pty 一起）。

    // ---- 后台：找 shell → 开通道 → 读循环 ----
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let name2 = name.clone();
    // 事件通道：后台 → 主线程（终端控件**只能**在主线程上碰）。
    let (ev_tx, ev_rx) = async_channel::unbounded::<ShellEvent>();
    let (res_tx, res_rx) = async_channel::bounded::<
        Result<crate::backend::shell::ShellChannel, String>,
    >(1);
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let session = match shared_session(&opts, pw).await {
            Ok(s) => s,
            Err(e) => {
                let _ = res_tx.send_blocking(Err(e));
                return;
            }
        };
        let shell = match docker::find_shell(&session, &name).await {
            Ok(s) => s,
            Err(e) => {
                let _ = res_tx.send_blocking(Err(e.0));
                return;
            }
        };
        let ch = match docker::open_exec_shell(&session, &name, &shell, 80, 24).await {
            Ok(c) => c,
            Err(e) => {
                let _ = res_tx.send_blocking(Err(e.0));
                return;
            }
        };
        let _ = res_tx.send_blocking(Ok(ch.clone()));
        // 读循环：远程字节 → 主线程（终端控件在主线程 ⇒ 必须这么走）。
        while let Some(ev) = ch.next_event().await {
            if ev_tx.send(ev).await.is_err() {
                break;
            }
        }
    });

    // ---- 主线程：拿到通道 ⇒ 交给终端控件 ----
    {
        let u = Rc::downgrade(ui);
        let status2 = status.clone();
        let term2 = term.clone();
        glib::MainContext::default().spawn_local(async move {
            match res_rx.recv().await {
                Ok(Ok(chan)) => {
                    let Some(u) = u.upgrade() else { return };
                    let chan2 = chan.clone();
                    let st2 = status2.clone();
                    let ok = term2.attach_shell(
                        chan,
                        ev_rx,
                        Box::new(move |code| {
                            let msg = if code == 0 {
                                tr("容器终端已退出").to_string()
                            } else {
                                trf!("容器终端已退出（退出码 {c}）", c = code)
                            };
                            st2.set_text(&msg);
                        }),
                    );
                    if !ok {
                        status2.set_text(tr("当前终端后端不支持容器终端"));
                        return;
                    }
                    *u.term_chan.borrow_mut() = Some(chan2);
                    *u.term_view.borrow_mut() = Some(term2.clone());
                    u.term_running.set(true);
                    status2.set_text(&trf!(
                        "容器终端：{n}（输入 exit 可以退出）",
                        n = name2
                    ));
                }
                Ok(Err(msg)) => {
                    status2.set_text(&msg);
                }
                Err(_) => {}
            }
        });
    }
}

/// 停掉**容器内终端**（关通道 ⇒ 容器里那个 shell 退出）。
fn stop_terminal(ui: &Rc<DockerUi>) {
    // ⚠️ **重入保护**：窗口的 `close_request` 也会调这里 ⇒ 自己调自己
    //   （关窗口 ⇒ close_request ⇒ stop_terminal ⇒ 又关窗口…）。
    if ui.term_closing.get() {
        return;
    }
    ui.term_closing.set(true);
    if let Some(ch) = ui.term_chan.borrow_mut().take() {
        ch.close();
    }
    // ⚠️ **控件句柄也要清掉**：窗口已经关了 ⇒ 那个控件已经被销毁 ⇒
    //    攥着它再用会炸。
    ui.term_view.borrow_mut().take();
    // ⚠️ **窗口也要关掉**（阶段 125 顺手修的真 bug）：
    //    以前只「把句柄 take 掉」**不关窗口** ⇒
    //    - 用户**关掉容器页签** ⇒ 容器终端窗口**留在屏幕上**（里面是
    //      一个已经死掉的终端）⇒ 还**盖住**别的窗口；
    //    - 截图也拍不到该拍的东西（盖在上面）。
    //    ⇒ 现在由 `stop_terminal` 负责把窗口收掉。
    //
    // ⚠️ **窗口句柄留着**（不 take）：留着才能回答「那个窗口**还在屏幕上
    //    吗**」（测试钩子要问这个）⇒ 窗口关掉了它只是不在屏幕上了 ⇒
    //    攥着也安全（我们只对它调 `close`，不再用里面的控件）。
    if let Some(w) = ui.term_win.borrow().as_ref() {
        w.close();
    }
    ui.term_running.set(false);
    ui.term_closing.set(false);
}

/// 「编辑 compose 文件」⇒ **用编辑器打开远程文件**。
///
/// ⚠️ 多个 compose 文件时**只打开第一个**（标签里是逗号分隔的）。
/// ⚠️ 没有文件路径 ⇒ 说清楚为什么（别静默）。
fn open_compose_file(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    let name = ui.selected_project.borrow().clone();
    if name.is_empty() {
        return;
    }
    let Some(sum) = ui.project_summary(&name) else {
        ui.status.set_text(tr("找不到这个 compose 项目的容器"));
        return;
    };
    let Some(path) = sum
        .files
        .split(',')
        .map(str::trim)
        .find(|f| !f.is_empty())
        .map(|f| f.to_string())
    else {
        ui.status
            .set_text(tr("这个 compose 项目没有记录配置文件路径，没法编辑"));
        return;
    };
    ui.status
        .set_text(&trf!("正在打开 {p}…", p = path));
    // ⚠️ **先确保编辑区就挂在这一页**（用户 2026-11-16：「打开之后用户马上
    //   能看到，不要跑到 term 界面」）。
    //
    //   编辑区是**一个共享的 widget** ⇒ 它同一时刻只能待在一页里 ⇒ 切页
    //   时会跟着搬；但**保险起见**这里再钉一次（万一切页回调没赶上）。
    if let Some(sub) = st.sub_tabs() {
        let tab = ui.tab_id.get();
        if tab != 0 {
            sub.host_editor_area_for_page(tab);
        }
    }
    // ⚠️ **远程文件** ⇒ 走 SFTP 打开（编辑区里另开一个编辑器标签）。
    crate::panel::fileedit::open_remote(st, &path, false);
}

/// 触发一次刷新。
///
/// ⚠️ **后台任务里不许碰 GTK** ⇒ 只把「连接参数 + 密码」带进去，
/// 结果回来再回主线程更新界面。
fn refresh(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    if ui.busy.get() {
        return; // 上一次还没回来 ⇒ 不叠加
    }
    ui.busy.set(true);
    // ⚠️ **loading 动画**（用户 2026-11-16）：开始取了 ⇒ 转起来。
    ui.set_loading(true);
    ui.status.set_text(&tr("正在读取远程容器…"));

    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());

    // ⚠️ **后台只带 Send 的东西**：`Rc`/`Weak` 不是 `Send` ⇒ 不能进
    //    tokio 的多线程任务 ⇒ 结果走通道回主线程（见 `build` 里的接收循环）。
    let tx = ui.tx.clone();
    // ⚠️ **全局运行时**：连接中枢在那儿 ⇒ russh 的 Handle 必须绑在同一个
    //    runtime 上（Handle 跨 runtime 用不了）。
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = fetch_all(&opts, pw).await;
        // 通道断了 = 界面已经没了 ⇒ 忽略。
        let _ = tx.send_blocking(Msg::List(r));
    });
}

/// 建界面（**只建一次**）。
pub fn ensure(st: &Rc<PanelState>) {
    if st.docker_ui.borrow().is_none() {
        let ui = build(st);
        st.docker_ui.borrow_mut().replace(ui);
    }
}

/// 把容器页**挂上去**（子标签）并切过去 + 拉第一次数据。
pub fn mount(st: &Rc<PanelState>) {
    ensure(st);
    let Some(sub) = st.sub_tabs() else { return };
    let ui = st.docker_ui.borrow().clone();
    let Some(ui) = ui else { return };
    // 页签可能已经被用户 × 掉了 ⇒ 不在 notebook 里就重新挂一个。
    let already = ui.tab_id.get() != 0 && sub.nb.page_num(&ui.page).is_some();
    if !already {
        let id = sub.add_page_icon_kind(
            // ⚠️ **图标名必须是主题里真有的**：`docker-symbolic` 来自
            //    docker-icon-theme 包，**大多数系统没装** ⇒ 会显示成「缺图」。
            //    ⇒ 用 Adwaita（GTK4 基础主题）自带的**箱子**图标。
            Some("package-x-generic-symbolic"),
            tr("容器"),
            crate::panel::subtabs::PageKind::Docker,
            ui.page.clone(),
            None,
        );
        ui.tab_id.set(id);
        // ⚠️ **登记编辑区的落点**（阶段 113 之后）：用户在容器页点
        //   「编辑 compose 文件」⇒ 编辑器要**就在容器界面下面**，
        //   马上能看见（用户 2026-11-16 报的「跑到 term 界面去了」）。
        sub.set_page_editor_host(id, ui.column.clone());
        // 关掉这个页签 ⇒ 停掉自动刷新（否则定时器还在跑，白费连接）。
        let u = Rc::downgrade(&ui);
        sub.add_close_hook(move |cid| {
            let Some(u) = u.upgrade() else { return };
            if cid == u.tab_id.get() {
                stop_auto(&u);
                // ⚠️ **日志流也得停**（否则远端的 `docker logs -f` 一直挂着）。
                stop_logs(&u);
                // ⚠️ **监控流也得停**（否则远端的 `docker stats` 一直挂着）。
                stop_stats(&u);
                // ⚠️ **容器内终端也得停**（否则容器里留着孤儿 shell）。
                stop_terminal(&u);
            }
        });
    }
    if let Some(num) = sub.nb.page_num(&ui.page) {
        sub.nb.set_current_page(Some(num));
    }
    // ⚠️ **把编辑区搬到这个页面来**（用户 2026-11-16：「编辑器打开了，却
    //   不在容器界面，用户会找不到 ⇒ 应该打开之后用户马上能看到，
    //   不要跑到 term 界面」）。
    //
    //   编辑区是**一个共享的 widget** ⇒ 它同一时刻只能待在一页里 ⇒ 之前
    //   它住在**终端列**下面 ⇒ 用户在容器页打开编辑器时，编辑器建好了
    //   却**藏在 term 界面里**。
    //
    //   ⚠️ **为什么这里再做一遍**（tabs.rs 的切页回调也会做）：那个回调
    //   只在**真的连接页**里才有 ⇒ 这里自己再做一次（幂等，重复调用
    //   没有副作用）。
    let tab = ui.tab_id.get();
    if tab != 0 {
        sub.host_editor_area_for_page(tab);
    }
    refresh(st, &ui);
}

/* ---------------- 测试钩子（冒烟用） ---------------- */

/// 测试钩子：容器页的表格。
pub fn view_for_test(st: &Rc<PanelState>) -> Option<gtk::TreeView> {
    st.docker_ui.borrow().as_ref().map(|u| u.view.clone())
}

/// 测试钩子：容器页的**整个控件**（页面）。
///
/// ⚠️ 冒烟要问 GTK「屏幕上哪一行在哪儿」（`path_at_pos`）⇒ 表格必须**真的
///    挂到窗口上、真的画出来** ⇒ 挂**整页**（表格在里面已经有滚动容器当父了
///    ⇒ 不能直接再当窗口的子控件）。
pub fn page_for_test(st: &Rc<PanelState>) -> Option<gtk::Widget> {
    st.docker_ui.borrow().as_ref().map(|u| u.page.clone())
}

/// 测试钩子：**名称列的渲染器**（冒烟用它验「字真的画出来了」）。
///
/// ⚠️ GTK4 里 `pack_start` **只把渲染器塞进列**，不绑 `add_attribute` 就
///    **一片空白**（列头还在 ⇒ 看着像「查出来是空的」）⇒ 画完之后这个渲染器
///    的 "text" 属性里**有值**才算真的画出来了。
pub fn name_renderer_for_test(st: &Rc<PanelState>) -> Option<gtk::CellRendererText> {
    st.docker_ui
        .borrow()
        .as_ref()
        .and_then(|u| u.name_renderer.clone())
}

/// 测试钩子：最近一次取到的容器。
pub fn containers_for_test(st: &Rc<PanelState>) -> Vec<ContainerInfo> {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.containers.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：状态行的文字（冒烟要看错误提示）。
pub fn status_for_test(st: &Rc<PanelState>) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.status.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：表格**模型**里的行（`(名字, 是不是项目行, 是不是顶层行)`）。
pub fn model_rows_for_test(st: &Rc<PanelState>) -> Vec<(String, bool, bool)> {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.model_rows())
        .unwrap_or_default()
}

/// 测试钩子：**灌一批容器进表格**（**不连服务器**）。///
/// 冒烟用它验界面的树形形状（compose 项目是父行、独立容器是顶层行、
/// 默认收起、点开才显示……）⇒ 不用真的连一台有 docker 的机器。
pub fn set_rows_for_test(st: &Rc<PanelState>, rows: Vec<ContainerInfo>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.busy.set(false);
    *ui.containers.borrow_mut() = rows.clone();
    ui.rebuild(&rows);
    // ⚠️ **loading 动画停掉**（测试里不连服务器 ⇒ 没有「正在加载」这回事）。
    ui.set_loading(false);
}

/// 测试钩子：**灌一份详情**进界面（**不连服务器**）＋ 把它当成选中的容器。
///
/// 冒烟用它验详情区（标题 / 四张卡片 / 按钮可用性）⇒ 不用真的连服务器。
pub fn set_detail_for_test(st: &Rc<PanelState>, d: ContainerDetail) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    *ui.selected.borrow_mut() = d.name.clone();
    ui.apply_detail(&d);
}

/// 测试钩子：详情区是不是显示着（**没选容器时应当是收起的**）。
pub fn detail_visible_for_test(st: &Rc<PanelState>) -> bool {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail.root.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：详情标题文字（「详情：web（运行中）」）。
pub fn detail_title_for_test(st: &Rc<PanelState>) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail.title.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：某张卡片的文字（`config` / `net` / `mount` / `env`）。
pub fn detail_card_for_test(st: &Rc<PanelState>, which: &str) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| match which {
            "config" => u.detail.config.text().to_string(),
            "net" => u.detail.net.text().to_string(),
            "mount" => u.detail.mount.text().to_string(),
            "env" => u.detail.env.text().to_string(),
            _ => String::new(),
        })
        .unwrap_or_default()
}

/// 测试钩子：**最近一次写操作的结果**（空 = 没执行过写操作）。
pub fn last_action_for_test(st: &Rc<PanelState>) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.last_action.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：当前选中的容器名。
pub fn selected_for_test(st: &Rc<PanelState>) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.selected.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：动作按钮**能不能点**（`start` / `stop` / `restart`）。
pub fn action_sensitive_for_test(st: &Rc<PanelState>, which: &str) -> bool {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| match which {
            "start" => u.detail.btn_start.is_sensitive(),
            "stop" => u.detail.btn_stop.is_sensitive(),
            "restart" => u.detail.btn_restart.is_sensitive(),
            _ => false,
        })
        .unwrap_or(false)
}

/// 测试钩子：**点一下动作按钮**（冒烟要验「二次确认拦在前面」）。
///
/// ⚠️ 冒烟里点的是**不存在的容器名** ⇒ 就算确认逻辑坏了，命令也只会被
///    docker 拒绝 ⇒ **碰不到用户机器上的真容器**。
pub fn click_action_for_test(st: &Rc<PanelState>, which: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let btn = match which {
        "start" => ui.detail.btn_start.clone(),
        "stop" => ui.detail.btn_stop.clone(),
        "restart" => ui.detail.btn_restart.clone(),
        _ => return,
    };
    btn.emit_clicked();
}

/// 测试钩子：**选中**某个 compose 项目行（冒烟要验「项目详情 + 5 个操作」）。
pub fn select_project_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(it) = ui.project_iter(name) else {
        return false;
    };
    let path = ui.model.path(&it);
    gtk::prelude::TreeViewExt::set_cursor(&ui.view, &path, None, false);
    true
}

/// 测试钩子：当前选中的 **compose 项目名**（空 = 没选项目）。
pub fn selected_project_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.selected_project.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：**项目详情**是不是显示着（没选项目行时应当是隐藏的）。
pub fn project_detail_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.detail.proj_root.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：项目详情的标题（「compose 项目：xxx」）。
pub fn project_title_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.detail.proj_title.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：项目详情的正文（配置文件 / 工作目录 / 容器清单）。
pub fn project_info_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.detail.proj_info.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：某个项目操作按钮**能不能点**（`edit`/`start`/`stop`/`down`/`up`）。
pub fn project_action_sensitive_for_test(st: &Rc<PanelState>, which: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let btn = match which {
        "edit" => &ui.detail.btn_edit,
        "start" => &ui.detail.btn_cstart,
        "stop" => &ui.detail.btn_cstop,
        "down" => &ui.detail.btn_cdown,
        "up" => &ui.detail.btn_cup,
        _ => return false,
    };
    btn.is_sensitive()
}

/// 测试钩子：**点一下**某个项目操作按钮。
///
/// ⚠️ 冒烟里点的是**不存在的项目名** ⇒ 就算确认逻辑坏了，命令也只会被
///    docker 拒绝 ⇒ **碰不到用户机器上的真项目**。
pub fn click_project_action_for_test(st: &Rc<PanelState>, which: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let btn = match which {
        "edit" => ui.detail.btn_edit.clone(),
        "start" => ui.detail.btn_cstart.clone(),
        "stop" => ui.detail.btn_cstop.clone(),
        "down" => ui.detail.btn_cdown.clone(),
        "up" => ui.detail.btn_cup.clone(),
        _ => return,
    };
    btn.emit_clicked();
}

/// 测试钩子：**展开**某个 compose 项目（冒烟要验证「点开才显示」）。
pub fn expand_project_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(it) = ui.project_iter(name) else {
        return false;
    };
    let path = ui.model.path(&it);
    ui.view.expand_row(&path, false);
    true
}

/* ---------------- 阶段 113：容器内终端的测试钩子 ---------------- */

/// 测试钩子：**打开容器内终端**（真连服务器 ⇒ 真进容器里跑 shell）。
pub fn open_terminal_for_test(st: &Rc<PanelState>, name: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    open_container_terminal(st, ui, name);
}

/// 测试钩子：容器内终端**是不是开着**。
pub fn term_running_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.term_running.get()).unwrap_or(false)
}

/// 测试钩子：容器内终端**屏幕上**的字（冒烟用它验「真的有回显」）。
pub fn term_screen_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return String::new();
    };
    let view = ui.term_view.borrow();
    view.as_ref()
        .and_then(|t| t.visible_text())
        .unwrap_or_default()
}

/// 测试钩子：**往容器内终端里打字**（模拟用户敲键）。
pub fn term_send_for_test(st: &Rc<PanelState>, bytes: &[u8]) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let view = ui.term_view.borrow();
    if let Some(t) = view.as_ref() {
        t.feed_child(bytes);
    }
}

/// 测试钩子：**停掉**容器内终端（窗口关掉时走的就是它）。
pub fn stop_terminal_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    stop_terminal(ui);
}

/// 测试钩子：容器详情区的**「终端」按钮**能不能点。
pub fn term_button_sensitive_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.detail.btn_term.is_sensitive())
        .unwrap_or(false)
}

/// 测试钩子：**容器内终端的窗口还开着吗**（阶段 125）。
///
/// ⚠️ **为什么要有**：以前「停终端」只把窗口句柄 take 掉、**不关窗口**
///   ⇒ 用户关掉容器页签后，屏幕上留着个死终端窗口 ⇒ 断言抓得住这个。
///
/// ⚠️ **看的是「窗口还在不在屏幕上」**（`is_visible`）⇒ 不是「句柄还在
///   不在」⇒ 句柄总会清掉，拿它判断等于什么都没断言。
pub fn term_window_open_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.term_win
        .borrow()
        .as_ref()
        .map(|w| w.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：**loading 动画**的状态（用户 2026-11-16）。
///
/// 返回 `(工具条小转圈可见, 列表正中大转圈可见, 小转圈在转)`。
pub fn loading_spinners_for_test(st: &Rc<PanelState>) -> (bool, bool, bool) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (false, false, false) };
    (
        ui.spin.is_visible(),
        ui.spin_big.is_visible(),
        ui.spin.is_spinning(),
    )
}

/// 测试钩子：**编辑区是不是挂在容器页这一列**。
///
/// ⚠️ **干什么用**（用户 2026-11-16）：「编辑 docker-compose 文件」点了
///   以后，编辑器打开了**却不在容器界面** ⇒ 用户会找不到 ⇒ 断言「编辑区
///   的宿主是容器页那根 Paned」⇒ 钉住「编辑器就在容器界面下面」。
pub fn editor_area_in_docker_page_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(sub) = st.sub_tabs() else { return false };
    let area = sub.editor_area_widget();
    area.parent().and_downcast::<gtk::Paned>() == Some(ui.column.clone())
}

/// 测试钩子：**切到监控页**（**阶段 114**）。
pub fn monitor_show_page_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.stack.set_visible_child_name(PAGE_MONITOR);
}

/// 测试钩子：**切回概览页**（**阶段 114**）。
pub fn monitor_show_overview_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.stack.set_visible_child_name(PAGE_OVERVIEW);
}

/// 测试钩子：监控流**是不是在跑**（**阶段 114**）。
pub fn stat_running_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.stat_running.get()).unwrap_or(false)
}

/// 测试钩子：曲线上**有多少个点**（**阶段 114**）。
///
/// ⚠️ 返回 `(CPU点数, 网络接收点数)` —— 两条线代表「曲线真的在画」。
pub fn stat_points_for_test(st: &Rc<PanelState>) -> (usize, usize) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (0, 0) };
    let d = ui.monitor_data.borrow();
    (d.cpu.len(), d.net_rx.len())
}

/// 测试钩子：四个指标卡的**当前值**（**阶段 114**）。
pub fn stat_values_for_test(st: &Rc<PanelState>) -> (String, String, String, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        );
    };
    (
        ui.monitor.val_cpu.text().to_string(),
        ui.monitor.val_mem.text().to_string(),
        ui.monitor.val_net.text().to_string(),
        ui.monitor.val_blk.text().to_string(),
    )
}

/// 测试钩子：**最后一个采样点的原始行**（**带 ANSI**，阶段 114）。
///
/// ⚠️ **为什么留它**：冒烟要拿它跟指标卡**对照** ⇒ 能抓到「忘了剥 ANSI
///   ⇒ 解析出来的值全是 0」这类退化（光看指标卡抓不到）。
pub fn stat_last_raw_for_test(st: &Rc<PanelState>) -> Option<String> {
    let slot = st.docker_ui.borrow();
    let ui = slot.as_ref()?;
    ui.stat_last_raw.borrow().clone()
}

/// 测试钩子：监控页的**状态行**（**阶段 114**）。
pub fn stat_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.monitor.status.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：灌一个采样点进曲线（**不连服务器**）。
pub fn push_stat_for_test(st: &Rc<PanelState>, pt: docker::StatPoint) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.push_stat_point(pt);
}

/// 测试钩子：**关掉容器内终端的窗口**（冒烟截图用）。
pub fn close_terminal_window_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if let Some(w) = ui.term_win.borrow_mut().take() {
        w.close();
    }
}

/// 测试钩子：**展开所有 compose 项目**。///
/// ⚠️ **为什么需要它**：GTK 的 `set_cursor` 对**看不见的行**（收着的项目里
///    的子行）**不生效** ⇒ 冒烟要选 compose 组里的容器 ⇒ 得先展开。
pub fn expand_all_projects_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let mut names: Vec<String> = Vec::new();
    let mut it = match ui.model.iter_first() {
        Some(i) => i,
        None => return,
    };
    loop {
        if ui.model.iter_children(Some(&it)).is_some() {
            let name: String = ui.model.get(&it, COL_NAME as i32);
            if !name.is_empty() {
                names.push(name);
            }
        }
        let mut next = it;
        if !ui.model.iter_next(&mut next) {
            break;
        }
        it = next;
    }
    for n in names {
        if let Some(pit) = ui.project_iter(&n) {
            let path = ui.model.path(&pit);
            ui.view.expand_row(&path, false);
        }
    }
}

/// 测试钩子：容器页在子标签里的 id（冒烟要切到那一页去截图）。
pub fn tab_id_for_test(st: &Rc<PanelState>) -> u64 {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.tab_id.get()).unwrap_or(0)
}

/// 测试钩子：某个 compose 项目**是不是展开着**。
pub fn project_expanded_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(it) = ui.project_iter(name) else {
        return false;
    };
    let path = ui.model.path(&it);
    ui.view.row_expanded(&path)
}

/* ---------------- 阶段 112：日志的测试钩子 ---------------- */

/// 测试钩子：**假装在看某个容器的日志**（**不连服务器**）。
///
/// ⚠️ 真实流程里 `log_name` 是 `start_logs` 写的 ⇒ 冒烟要验「消息进到
///    界面」这条路径，就得有办法先把「在看谁」设上。
pub fn log_watch_for_test(st: &Rc<PanelState>, name: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.log_reset();
    *ui.log_name.borrow_mut() = name.to_string();
}

/// 测试钩子：**灌一段日志**（**走真实的消息通道** ⇒ 和真流一样处理）。
pub fn log_push_for_test(st: &Rc<PanelState>, name: &str, data: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let _ = ui.tx.try_send(Msg::LogChunk {
        name: name.to_string(),
        data: data.to_string(),
    });
}

/// 测试钩子：**灌一个「流结束」**（走真实的消息通道）。
pub fn log_end_for_test(st: &Rc<PanelState>, name: &str, code: i32) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let _ = ui.tx.try_send(Msg::LogEnd {
        name: name.to_string(),
        code,
    });
}

/// 测试钩子：日志区**显示着**的文字。
pub fn log_text_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return String::new();
    };
    let b = &ui.log.buffer;
    b.text(&b.start_iter(), &b.end_iter(), false).to_string()
}

/// 测试钩子：**已收到**的行数（暂停时也照收 ⇒ 这个数只增不减）。
pub fn log_line_count_for_test(st: &Rc<PanelState>) -> usize {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.log_lines.borrow().len())
        .unwrap_or(0)
}

/// 测试钩子：日志**是不是暂停**中。
pub fn log_paused_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log_paused.get()).unwrap_or(false)
}

/// 测试钩子：**点一下「暂停 / 继续」**（走真实按钮 ⇒ 触发真实逻辑）。
pub fn log_click_pause_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.log.btn_pause.emit_clicked();
}

/// 测试钩子：**点一下「清空」**（走真实按钮）。
pub fn log_click_clear_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.log.btn_clear.emit_clicked();
}

/// 测试钩子：**设搜索词**（走真实搜索框 ⇒ 触发真实的重建）。
pub fn log_set_search_for_test(st: &Rc<PanelState>, q: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.log.search.set_text(q);
}

/// 测试钩子：**切到日志页**（走真实的 Stack 切换 ⇒ 会起流）。
pub fn log_show_page_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.stack.set_visible_child_name(PAGE_LOGS);
    true
}

/// 测试钩子：**切回概览页**（走真实的 Stack 切换 ⇒ 日志流停）。
pub fn log_show_overview_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.stack.set_visible_child_name(PAGE_OVERVIEW);
    true
}

/// 测试钩子：日志页**是不是开着**。
pub fn log_page_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log_page_visible()).unwrap_or(false)
}

/// 测试钩子：日志流**是不是在跑**。
pub fn log_running_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log_running.get()).unwrap_or(false)
}

/// 测试钩子：**自动跟随**是不是开着。
pub fn log_follow_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.log.follow.is_active())
        .unwrap_or(false)
}

/// 测试钩子：正文**是不是滚到底了**（自动跟随的判据）。
pub fn log_at_bottom_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let adj = ui.log.scroll.vadjustment();
    let lo = adj.lower();
    let hi = (adj.upper() - adj.page_size()).max(lo);
    // ⚠️ 差 1 像素以内就算到底（浮点误差）。
    adj.value() >= hi - 1.0
}

/// 测试钩子：日志正文**是不是等宽、不自动换行**（看日志要舒服）。
pub fn log_view_is_monospace_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.log.view.is_monospace())
        .unwrap_or(false)
}

/// 测试钩子：日志区**是不是显示着**（在日志页里才显示）。
pub fn log_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log.root.is_visible()).unwrap_or(false)
}

/// 测试钩子：**设「自动跟随」开关**（走真实开关 ⇒ 触发真实逻辑）。
pub fn log_set_follow_for_test(st: &Rc<PanelState>, on: bool) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.log.follow.set_active(on);
}

/// 测试钩子：把日志正文**滚到顶**（模拟用户往上翻）。
pub fn log_scroll_to_top_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let adj = ui.log.scroll.vadjustment();
    adj.set_value(adj.lower());
}

/// 测试钩子：**选中**某个容器行（走真实的 `set_cursor` ⇒ 触发真实的
/// 「选中 ⇒ 取详情」）。
pub fn select_container_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(path) = ui.find_path(name) else {
        return false;
    };
    gtk::prelude::TreeViewExt::set_cursor(&ui.view, &path, None, false);
    true
}

/// 测试钩子：**触发一次刷新**（真连服务器取容器列表）。
pub fn refresh_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    refresh(st, ui);
}

/// 测试钩子：**光标**现在指着哪一行（空 = 没有光标）。
/// ⚠️ 这是「表格自己的光标」⇒ 和 `selected_for_test`（程序记的选中）
///    是**两回事** ⇒ 两者不一致就说明中间出了问题。
pub fn cursor_name_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return String::new();
    };
    let (path, _col) = gtk::prelude::TreeViewExt::cursor(&ui.view);
    let Some(path) = path else {
        return String::new();
    };
    let Some(it) = ui.model.iter(&path) else {
        return String::new();
    };
    let name: String = ui.model.get(&it, COL_NAME as i32);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, compose: &str) -> ContainerInfo {
        ContainerInfo {
            id: format!("{name}-id"),
            name: name.to_string(),
            image: "img".to_string(),
            status: "Up 1 hour".to_string(),
            state: "running".to_string(),
            ports: String::new(),
            compose: compose.to_string(),
            compose_files: if compose.is_empty() {
                String::new()
            } else {
                "/srv/app/docker-compose.yml".to_string()
            },
            compose_dir: if compose.is_empty() {
                String::new()
            } else {
                "/srv/app".to_string()
            },
            cpu: "1.0%".to_string(),
            mem: "10MiB".to_string(),
        }
    }

    /// compose 的容器**聚到一个项目行**里，独立容器**各占一行**。
    #[test]
    fn 按compose项目分组() {
        let rows = vec![
            c("solo", ""),
            c("web", "proj"),
            c("other", ""),
            c("db", "proj"),
        ];
        let g = DockerUi::group(&rows);
        assert_eq!(g.len(), 3, "顶层行数不对");
        // 第 1 行：独立容器
        assert!(matches!(&g[0], Node::Solo(x) if x.name == "solo"));
        // 第 2 行：compose 项目（带 2 个容器）
        match &g[1] {
            Node::Project { name, items } => {
                assert_eq!(name, "proj");
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].name, "web");
                assert_eq!(items[1].name, "db");
            }
            _ => panic!("第 2 行应当是项目行"),
        }
        // 第 3 行：另一个独立容器
        assert!(matches!(&g[2], Node::Solo(x) if x.name == "other"));
    }

    /// 没有 compose 的容器**不该**被塞进项目行。
    #[test]
    fn 全是独立容器时不产生项目行() {
        let rows = vec![c("a", ""), c("b", "")];
        let g = DockerUi::group(&rows);
        assert_eq!(g.len(), 2);
        assert!(g.iter().all(|n| matches!(n, Node::Solo(_))));
    }

    /// 同一个项目的容器**聚到一起**（不按出现顺序散开）。
    #[test]
    fn 同一项目的容器不散开() {
        let rows = vec![c("a", "p1"), c("b", "p2"), c("c", "p1")];
        let g = DockerUi::group(&rows);
        assert_eq!(g.len(), 2);
        match &g[0] {
            Node::Project { name, items } => {
                assert_eq!(name, "p1");
                assert_eq!(items.len(), 2);
            }
            _ => panic!("第 1 行应当是项目行"),
        }
    }
}


/* ---------------- 阶段 115：镜像 / 卷 / 网络（前端） ---------------- */

/// 建一张**平表**（列标题 + 列宽）⇒ 返回 `(ListStore, TreeView)`。
///
/// ⚠️ **为什么不用树表**：镜像 / 卷 / 网络都没有层级 ⇒ 平表就够。
/// ⚠️ **第一列 expand**：名字列拉宽，其余给下限（跟容器表一个脾气）。
fn flat_table(
    titles: &[&str],
    widths: &[i32],
    expand_col: usize,
) -> (gtk::TreeStore, gtk::TreeView) {
    let types: Vec<glib::Type> = titles.iter().map(|_| glib::Type::STRING).collect();
    let store = gtk::TreeStore::new(&types);
    let view = gtk::TreeView::new();
    view.set_model(Some(&store));
    for (i, t) in titles.iter().enumerate() {
        let r = gtk::CellRendererText::new();
        r.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let col = gtk::TreeViewColumn::new();
        col.set_title(t);
        col.set_resizable(true);
        col.pack_start(&r, false);
        // ⚠️ **必须显式绑 "text"**（跟容器表同一个坑：不绑就是一片空白）。
        col.add_attribute(&r, "text", i as i32);
        col.set_min_width(widths[i]);
        // ⚠️ **哪一列展开**：镜像表是「创建时间」（最容易被截断的那列）⇒
        //   展开它，别让「仓库」占满宽度把时间挤成「2026-11-15 19:33…」。
        if i == expand_col {
            col.set_expand(true);
        }
        view.append_column(&col);
    }
    (store, view)
}

/// 把一张表包进**能滚动的页面**（表格占满 + 上下留白）。
fn flat_page(view: &gtk::TreeView) -> gtk::Box {
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_child(Some(view));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_margin_bottom(6);
    root.append(&scroll);
    root
}

/// 刷新**当前分段**（阶段 115）。
///
/// ⚠️ **自动刷新也走这里** ⇒ 在镜像页就别去刷容器（少一条远端命令）。
fn refresh_segment(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    match ui.seg_current.get() {
        SEG_IMAGES => refresh_images(st, ui),
        SEG_VOLUMES => refresh_volumes(st, ui),
        SEG_NETWORKS => refresh_networks(st, ui),
        _ => refresh(st, ui),
    }
}

/// 刷新**镜像**（阶段 115）。
fn refresh_images(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    if ui.images_busy.get() {
        return; // 上一次还没回来 ⇒ 不叠加
    }
    ui.images_busy.set(true);
    ui.status.set_text(&tr("正在读取远程镜像…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = fetch_images(&opts, pw).await;
        let _ = tx.send_blocking(Msg::Images(r));
    });
}

/// 刷新**卷**（阶段 115）。
fn refresh_volumes(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    if ui.volumes_busy.get() {
        return;
    }
    ui.volumes_busy.set(true);
    ui.status.set_text(&tr("正在读取远程卷…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = fetch_volumes(&opts, pw).await;
        let _ = tx.send_blocking(Msg::Volumes(r));
    });
}

/// 刷新**网络**（阶段 115）。
fn refresh_networks(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    if ui.networks_busy.get() {
        return;
    }
    ui.networks_busy.set(true);
    ui.status.set_text(&tr("正在读取远程网络…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = fetch_networks(&opts, pw).await;
        let _ = tx.send_blocking(Msg::Networks(r));
    });
}

/// 后台取**镜像列表**（走**共享连接** ⇒ 不新增 SSH 连接）。
async fn fetch_images(
    opts: &crate::args::ConnOptions,
    pw: Option<String>,
) -> Result<Vec<docker::ImageInfo>, String> {
    let session = shared_session(opts, pw).await?;
    docker::list_images(&session).await.map_err(|e| e.0)
}

/// 后台取**卷列表**（走**共享连接**）。
async fn fetch_volumes(
    opts: &crate::args::ConnOptions,
    pw: Option<String>,
) -> Result<Vec<docker::VolumeInfo>, String> {
    let session = shared_session(opts, pw).await?;
    docker::list_volumes(&session).await.map_err(|e| e.0)
}

/// 后台取**网络列表**（走**共享连接**）。
async fn fetch_networks(
    opts: &crate::args::ConnOptions,
    pw: Option<String>,
) -> Result<Vec<docker::NetInfo>, String> {
    let session = shared_session(opts, pw).await?;
    docker::list_networks(&session).await.map_err(|e| e.0)
}

/// 填**镜像表** ＋ 更新状态行（阶段 115）。
fn apply_images(ui: &Rc<DockerUi>, v: &[docker::ImageInfo]) {
    ui.images_store.clear();
    for i in v {
        let row = ui.images_store.append(None);
        ui.images_store.set(
            &row,
            &[
                (0u32, &i.repository),
                (1u32, &i.tag),
                (2u32, &i.id),
                (3u32, &i.size),
                (4u32, &i.created),
            ],
        );
    }
    ui.status.set_text(&trf!("共 {n} 个镜像", n = v.len()));
}

/// 填**卷表** ＋ 更新状态行（阶段 115）。
fn apply_volumes(ui: &Rc<DockerUi>, v: &[docker::VolumeInfo]) {
    ui.volumes_store.clear();
    for x in v {
        let row = ui.volumes_store.append(None);
        ui.volumes_store.set(&row, &[(0u32, &x.name), (1u32, &x.driver)]);
    }
    ui.status.set_text(&trf!("共 {n} 个卷", n = v.len()));
}

/// 填**网络表** ＋ 更新状态行（阶段 115）。
fn apply_networks(ui: &Rc<DockerUi>, v: &[docker::NetInfo]) {
    ui.networks_store.clear();
    for x in v {
        let row = ui.networks_store.append(None);
        ui.networks_store
            .set(&row, &[(0u32, &x.name), (1u32, &x.driver), (2u32, &x.scope)]);
    }
    ui.status.set_text(&trf!("共 {n} 个网络", n = v.len()));
}

/* ---------------- 阶段 116：文件页 ---------------- */

/// 文件类型 → **稳定的键**（内部用；显示用 `tr(kind.label())`）。
///
/// ⚠️ **为什么不直接用中文标签**：标签会随语言变（`tr` 会翻成英文）⇒
///   拿它做判断 ⇒ 英文界面下判断全错 ⇒ 这里用**不翻译的键**。
fn file_kind_key(k: docker::FileKind) -> &'static str {
    match k {
        docker::FileKind::Dir => "dir",
        docker::FileKind::File => "file",
        docker::FileKind::Link => "link",
        docker::FileKind::Other => "other",
        docker::FileKind::Unknown => "unknown",
    }
}

/// 填**文件表** ＋ 更新状态行（阶段 116）。
fn apply_files(ui: &Rc<DockerUi>, v: &[docker::ContainerFile]) {
    ui.files.store.clear();
    for f in v {
        let name = f.name.clone();
        let kind_label = tr(f.kind.label()).to_string();
        let size = crate::util::human_size(f.size);
        let perms = f.perms.clone();
        let key = file_kind_key(f.kind).to_string();
        let row = ui.files.store.append(None);
        ui.files.store.set(
            &row,
            &[
                (0u32, &name),
                (1u32, &kind_label),
                (2u32, &size),
                (3u32, &perms),
                (4u32, &key),
            ],
        );
    }
    ui.files.status.set_text(&trf!("{n} 个条目", n = v.len()));
}

/// 当前选中的行（**名字 ＋ 类型键**）。
fn files_selected(ui: &Rc<DockerUi>) -> Option<(String, String)> {
    let sel = ui.files.view.selection();
    let (model, iter) = sel.selected()?;
    let name: String = model.get(&iter, 0);
    let kind: String = model.get(&iter, 4);
    if name.is_empty() {
        return None;
    }
    Some((name, kind))
}

/// 选中条目的**远程绝对路径**。
fn files_remote_path(ui: &Rc<DockerUi>, name: &str) -> String {
    let dir = ui.files_path.borrow().clone();
    if dir.is_empty() {
        return format!("/{name}");
    }
    format!("{}/{name}", dir.trim_end_matches('/'))
}

/// **文件页是不是正开着**。
fn files_page_visible(ui: &Rc<DockerUi>) -> bool {
    ui.stack.visible_child_name().as_deref() == Some(PAGE_FILES)
}

/// 切到文件页时：**没列过 / 换了容器** ⇒ 列一次（**阶段 116**）。
fn files_ensure(st: &Rc<PanelState>, ui: &Rc<DockerUi>, container: &str) {
    if container.is_empty() {
        return;
    }
    let same = ui.files_name.borrow().as_str() == container
        && !ui.files_path.borrow().is_empty();
    if same {
        return;
    }
    files_goto(st, ui, container, "/");
}

/// 去某个目录（**改路径栏 ＋ 重新列**）。
fn files_goto(st: &Rc<PanelState>, ui: &Rc<DockerUi>, container: &str, path: &str) {
    *ui.files_name.borrow_mut() = container.to_string();
    *ui.files_path.borrow_mut() = path.to_string();
    ui.files.path.set_text(path);
    files_list(st, ui, container, path);
}

/// 列目录（**后台** ⇒ 结果回主线程）。
///
/// ⚠️ **不靠 busy 标志挡重复请求**：用户连点目录时，后到的旧结果必须
///   能被认出来丢掉（见消息循环里的「路径一样吗」判断）⇒ 请求照发，
///   旧结果自己会被扔掉。
fn files_list(st: &Rc<PanelState>, ui: &Rc<DockerUi>, container: &str, path: &str) {
    ui.files.status.set_text(&tr("正在读取远程目录…"));
    // ⚠️ **请求序号**：只认最新一次（旧的晚到 ⇒ 盖掉新的 ⇒ 看着像刷新坏了）。
    let seq = ui.files_seq.get() + 1;
    ui.files_seq.set(seq);
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let container = container.to_string();
    let path = path.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => docker::list_dir(&session, &container, &path)
                .await
                .map_err(|e| e.0),
            Err(e) => Err(e),
        };
        let _ = tx.send_blocking(Msg::Files {
            name: container,
            path,
            seq,
            r,
        });
    });
}

/// **上传**（后台 ⇒ 进度 / 结果回主线程）。
fn files_upload(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    container: &str,
    local: &std::path::Path,
) {
    if ui.files_xfer.get() {
        return;
    }
    let remote_dir = ui.files_path.borrow().clone();
    if remote_dir.is_empty() {
        return;
    }
    ui.files_xfer.set(true);
    ui.files.xfer_status.set_text(&tr("正在上传…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let container = container.to_string();
    let local = local.to_path_buf();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => {
                let tx2 = tx.clone();
                // ⚠️ **进度限流 100ms**（每 32KB 一块 ⇒ 不限流会把消息
                //   通道塞爆 ⇒ 主线程被拖死）。
                let last = std::sync::Mutex::new(std::time::Instant::now());
                let prog: docker::XferProgress = std::sync::Arc::new(move |n: u64| {
                    {
                        let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
                        if g.elapsed() < std::time::Duration::from_millis(100) {
                            return;
                        }
                        *g = std::time::Instant::now();
                    }
                    let _ = tx2.send_blocking(Msg::FileProgress {
                        msg: trf!("正在上传… 已传 {n} 字节", n = n),
                    });
                });
                docker::upload_from(&session, &container, &local, &remote_dir, &prog)
                    .await
                    .map_err(|e| e.0)
            }
            Err(e) => Err(e),
        };
        let msg = match r {
            Ok(()) => tr("上传完成").to_string(),
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::FileXferDone { msg });
    });
}

/// **下载**（后台 ⇒ 进度 / 结果回主线程）。
fn files_download(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    container: &str,
    remote: &str,
    local_dir: &std::path::Path,
) {
    if ui.files_xfer.get() {
        return;
    }
    ui.files_xfer.set(true);
    ui.files.xfer_status.set_text(&tr("正在下载…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let container = container.to_string();
    let remote = remote.to_string();
    let local_dir = local_dir.to_path_buf();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => {
                let tx2 = tx.clone();
                let last = std::sync::Mutex::new(std::time::Instant::now());
                let prog: docker::XferProgress = std::sync::Arc::new(move |n: u64| {
                    {
                        let mut g = last.lock().unwrap_or_else(|e| e.into_inner());
                        if g.elapsed() < std::time::Duration::from_millis(100) {
                            return;
                        }
                        *g = std::time::Instant::now();
                    }
                    let _ = tx2.send_blocking(Msg::FileProgress {
                        msg: trf!("正在下载… 已收 {n} 字节", n = n),
                    });
                });
                docker::download_to(&session, &container, &remote, &local_dir, &prog)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.0)
            }
            Err(e) => Err(e),
        };
        let msg = match r {
            Ok(()) => tr("下载完成").to_string(),
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::FileXferDone { msg });
    });
}

/// **查看**文件内容（**只读**，最多 256 KB）。
fn files_view(st: &Rc<PanelState>, ui: &Rc<DockerUi>, container: &str, remote: &str) {
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let container = container.to_string();
    let remote = remote.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let r = match shared_session(&opts, pw).await {
            Ok(session) => {
                docker::read_file(&session, &container, &remote, 256 * 1024)
                    .await
                    .map_err(|e| e.0)
            }
            Err(e) => Err(e),
        };
        let _ = tx.send_blocking(Msg::FileView { path: remote, r });
    });
}

/// 弹一个**只读**的文本窗口（看文件内容用）。
fn show_file_window(st: Option<&Rc<PanelState>>, path: &str, content: &str) {
    let win = gtk::Window::builder()
        .title(tr("查看文件"))
        .default_width(760)
        .default_height(520)
        .build();
    if let Some(st) = st {
        if let Some(top) = st.topwin().as_ref() {
            win.set_transient_for(Some(top));
        }
    }
    let title = gtk::Label::new(Some(path));
    title.set_xalign(0.0);
    title.set_margin_start(8);
    title.set_margin_end(8);
    title.set_margin_top(6);
    title.add_css_class("heading");
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_monospace(true);
    view.buffer().set_text(content);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&view));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.append(&title);
    root.append(&scroll);
    win.set_child(Some(&root));
    win.present();
}

/* ---------------- 阶段 116：测试钩子 ---------------- */

/// 测试钩子：切到「文件」页（**真的切** ＋ 列目录）。
pub fn files_show_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.stack.set_visible_child_name(PAGE_FILES);
    let name = ui.selected.borrow().clone();
    if name.is_empty() {
        return;
    }
    files_ensure(st, ui, &name);
}

/// 测试钩子：去**某个目录**（阶段 116；冒烟要传到容器的 /tmp 里）。
pub fn files_goto_for_test(st: &Rc<PanelState>, path: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let name = ui.selected.borrow().clone();
    if name.is_empty() {
        return;
    }
    files_goto(st, ui, &name, path);
}

/// 测试钩子：文件表的行（**名字 / 类型 / 大小 / 权限**）。
pub fn files_rows_for_test(st: &Rc<PanelState>) -> Vec<(String, String, String, String)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    let model = ui.files.store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let g = |c: i32| -> String { model.get(&iter, c) };
            out.push((g(0), g(1), g(2), g(3)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：文件页**当前目录**。
pub fn files_path_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.files_path.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：文件页**已经完成了几次传输**（每完成一次 +1）。
pub fn files_xfer_done_for_test(st: &Rc<PanelState>) -> u64 {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.files_xfer_done.get()).unwrap_or(0)
}

/// 测试钩子：文件页**传输状态行**（上传 / 下载的进度与结果）。
pub fn files_xfer_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.files.xfer_status.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：文件页**状态行**。
pub fn files_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.files.status.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：**上传**一个本地文件（阶段 116；冒烟用真上传）。
pub fn files_upload_for_test(st: &Rc<PanelState>, local: &std::path::Path) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let container = ui.selected.borrow().clone();
    if container.is_empty() {
        return;
    }
    files_upload(st, ui, &container, local);
}

/// 测试钩子：**下载**一个远程条目（阶段 116；冒烟用真下载）。
pub fn files_download_for_test(
    st: &Rc<PanelState>,
    remote: &str,
    local_dir: &std::path::Path,
) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let container = ui.selected.borrow().clone();
    if container.is_empty() {
        return;
    }
    files_download(st, ui, &container, remote, local_dir);
}

/* ---------------- 阶段 115：测试钩子 ---------------- */

/// 测试钩子：切到某个**顶层分段**（阶段 115）。
///
/// ⚠️ **真的切**（改可见子页 ＋ 触发取数）⇒ 冒烟能验「切过去就有数据」。
pub fn seg_show_for_test(st: &Rc<PanelState>, seg: &'static str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.seg_current.set(seg);
    ui.seg.set_visible_child_name(seg);
    // ⚠️ **同时把按钮状态对上**（不然界面上高亮的还是「容器」）。
    for (btn, key) in ui.seg_buttons.borrow().iter() {
        btn.set_active(*key == seg);
    }
    refresh_segment(st, ui);
}

/// 测试钩子：当前是哪个**顶层分段**（阶段 115）。
pub fn seg_current_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.seg_current.get().to_string())
        .unwrap_or_default()
}

/// 测试钩子：**镜像表**的行（阶段 115）。
pub fn images_rows_for_test(st: &Rc<PanelState>) -> Vec<(String, String, String, String, String)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    // ⚠️ **ListStore 自己没有 `iter()`/`value()`** ⇒ 走 **TreeModel**
    //   接口（ListStore 也是 TreeModel）⇒ `iter_first` + `iter_next`。
    let model = ui.images_store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let g = |c: i32| -> String { model.get(&iter, c) };
            out.push((g(0), g(1), g(2), g(3), g(4)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：**卷表**的行（阶段 115）。
pub fn volumes_rows_for_test(st: &Rc<PanelState>) -> Vec<(String, String)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    let model = ui.volumes_store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let g = |c: i32| -> String { model.get(&iter, c) };
            out.push((g(0), g(1)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：**网络表**的行（阶段 115）。
pub fn networks_rows_for_test(st: &Rc<PanelState>) -> Vec<(String, String, String)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    let model = ui.networks_store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let g = |c: i32| -> String { model.get(&iter, c) };
            out.push((g(0), g(1), g(2)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}
