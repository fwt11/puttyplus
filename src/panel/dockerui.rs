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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::docker::{
    self, Action, ComposeAction, ContainerDetail, ContainerInfo, DockerInfo, LOG_TAIL,
};
use crate::backend::shell::ShellEvent;
use crate::i18n::tr;
use crate::panel::PanelState;
use crate::trf;

/// **顶层分段**（阶段 115）：容器 / 镜像 / 卷 / 网络。
///
/// ⚠️ **跟详情区的三个窗口不是一回事**：详情区那三个（概览 / 日志 / 文件）
///   是**选中某个容器之后**才有的；这四个是**顶层**（先选看什么，
///   再看细节）。
pub const SEG_CONTAINERS: &str = "containers";
pub const SEG_IMAGES: &str = "images";
pub const SEG_VOLUMES: &str = "volumes";
pub const SEG_NETWORKS: &str = "networks";

/// 表格的列（**顺序 = `TreeStore` 的列顺序**）。
///
/// ⚠️ **阶段 155**：用户说「去掉**详情**列，没啥用」＋「去掉 **CPU 和内存**列」
///    ⇒ 这三列**退役**（连同它们的模型列号一起收掉），
///    换来一列**「操作」**（每行一排图标按钮，见 [`crate::panel::actioncell`]）。
///
/// ⚠️ **模型列号 ≠ 视图列下标**了 ⇒ 建列时**必须按模型列号绑属性**
///    （`col.add_attribute(&r, "text", model_col)`），
///    别再拿循环下标当模型列号（以前两者恰好相等 ⇒ 删列就错位）。
const COL_NAME: usize = 0;
const COL_ID: usize = 1;
const COL_IMAGE: usize = 2;
const COL_PORTS: usize = 3;
const COL_STATE: usize = 4;
/// **这一行是什么**（`"container"` / `"project"`）。
///
/// 干什么用：**「操作」列要给不同类型的行不同的按钮**
///   （容器：详情/进容器/启停；compose 项目：编辑/启停/销毁/重建）
///   ⇒ 行类型得能从模型里读到。
const COL_KIND: usize = 5;
/// **docker 的原始状态**（`running` / `exited` / …；项目行是空串）。
///
/// 干什么用：「操作」列里「启动 / 停止」哪个能点 ⇒ 得知道**原始状态**
///   （界面上的状态是中文标签，英文界面下文字变了 ⇒ 不能拿它判断）。
const COL_RAW_STATE: usize = 6;
/// **状态列的 markup**（带色点的那一份；**界面上已不再显示**）。
///
/// 历史包袱：UI 审查 D8 时代状态列靠 Pango markup 画「绿点 + 文字」，
/// 文字（`COL_STATE`）与 markup（`COL_STATE_MK`）分成两列。美化·地基 2/5
/// 之后状态列改用**药丸渲染器**（颜色由 `pill_state_of` 现取，跟主题走）
/// ⇒ 这一列**没人读了**，但 TreeStore 的列结构是按位置取值的
/// ⇒ 摘掉这一列要挪 N_COLS 后面所有列号，没那份必要 ⇒ 先留着，
/// 注释说明它已退役。
const COL_STATE_MK: usize = 7;
/// **名称列的 markup**（阶段 162：让 compose 项目行**一眼就认出来**）。
///
/// ⚠️ **为什么单独一列**：`COL_NAME` 存的是**纯名字**（按名字找行、排序、
///   拼路径都靠它）⇒ **不能**往里塞 `<b>` 之类标签 ⇒ 另开一列放**要画的
///   那份**（容器行 = 空 ⇒ 画纯名字；项目行 = 粗体 ＋「compose」小字）。
const COL_NAME_MK: usize = 8;
/// **资源列**（阶段 164：CPU 百分比 ＋ 内存占用）。
///
/// ⚠️ **为什么单独一列**（用户 2026-11-24：「如果地方确实大的话，我们可以
///   考虑把 cpu 占用、内存占用加回来，**只用 1 列**，cpu 写百分比，内存只写
///   占了多少内存，不用百分比」）⇒ **一列两样**，不拆成两列。
const COL_RES: usize = 9;
const N_COLS: usize = 10;

/// 行类型：**容器**（`COL_KIND` 的值）。
const KIND_CONTAINER: &str = "container";
/// 行类型：**compose 项目**（父行）。
const KIND_PROJECT: &str = "project";

/* ---------------- 阶段 155：「操作」列 ---------------- */

/// 「操作」列里一颗图标 = **干什么**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    /// 看详情（弹窗；页签停在**概览**）。
    Detail,
    /// 看容器里的文件（文件页）。
    Files,
    /// 看容器日志（日志页）。
    Logs,
    /// 进容器（开容器内终端）。
    Enter,
    /// 编辑 compose 文件。
    Edit,
    /// 启动。
    Start,
    /// 停止。
    Stop,
    /// 重启。
    Restart,
    /// 销毁 compose 项目（`docker compose down`）。
    ///
    /// ⚠️ **阶段 163：只在右键菜单里**（操作列没有「销毁」按钮）——
    ///   用户 2026-11-24：「down 了之后容器界面就看不到了 ⇒ 那 up 也没意义」
    ///   ⇒ down 会**删掉容器** ⇒ **项目行整行消失** ⇒ 没法再「拉回来」。
    Down,
    /// 重建 compose 项目（`docker compose up -d --force-recreate`）。
    ///
    /// ⚠️ **阶段 163：操作列里 compose 项目行只留这一颗**（替掉了原来的
    ///   up / down 两颗）⇒ 按 compose 文件**一律重建**。
    Recreate,
    /// 复制容器 ID（复制到剪贴板）。
    CopyId,
}

/// 一颗操作图标（**干什么** ＋ **画哪个图** ＋ **能不能点**）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowAction {
    /// 干什么（派发时按它分派）。
    pub kind: ActionKind,
    /// 图标名（**Adwaita 真有的** ⇒ 写错就显示成空白方块）。
    pub icon: String,
    /// 能不能点（不能 ⇒ 画暗的，tooltip 说为什么）。
    pub enabled: bool,
}

/// 图标名：**详情**。
///
/// ⚠️ **不是 `dialog-information-symbolic`**（阶段 157）：Adwaita 把它画成
///   一个**灯泡** ⇒ 看着像「提示 / 灵感」，不像「看详情」。
///   ⇒ 换成 **ⓘ**（`help-about-symbolic`）⇒ 一眼就是「信息 / 详情」。
const ICON_DETAIL: &str = "help-about-symbolic";
/// 图标名：**进容器**（终端）。
const ICON_TERMINAL: &str = "utilities-terminal-symbolic";
/// 图标名：**启动**（**右键菜单**里用的）。
///
/// ⚠️ **菜单不能用 `pp-play`**：那是**表格自绘**的图标（`panel::actioncell`
///   自己画出来的）⇒ **图标主题里根本没有这个名字** ⇒ 菜单里会显示成
///   **缺图占位符**（实测截图：菜单里「启动 / 停止」是两块灰糊）。
/// ⇒ 菜单用 Adwaita **真有的** `media-playback-start-symbolic`
///   （实心 ▶ 在**图标＋文字**的菜单行里不扎眼 ⇒ 可以用）。
const ICON_MENU_START: &str = "media-playback-start-symbolic";
/// 图标名：**停止**（**右键菜单**里用的；理由同 [`ICON_MENU_START`]）。
const ICON_MENU_STOP: &str = "media-playback-stop-symbolic";
/// 图标名：**启动**。
///
/// ⚠️ **不是 `media-playback-start-symbolic`**（阶段 157）：Adwaita 那个是
///   **实心**三角 ⇒ 混在一排线性图标里**特别扎眼**。
///   ⇒ 改成**自绘的描边三角**（`pp-play` ⇒ 见 `panel::actioncell`）。
const ICON_START: &str = "pp-play";
/// 图标名：**停止**。
///
/// ⚠️ 同上：Adwaita 的 `media-playback-stop-symbolic` 是**实心**方块
///   ⇒ 改成自绘的**描边方块**（`pp-stop`）。
const ICON_STOP: &str = "pp-stop";
/// 图标名：**重启**。
const ICON_RESTART: &str = "view-refresh-symbolic";
/// 图标名：**重建**（**操作列**里 compose 项目行的第 4 颗，阶段 164）。
///
/// ⚠️ **为什么是「两个叠起来的方框」**（用户 2026-11-24：「这几个图标实在
///   不够精致，尤其是 recreate 图标，**看不出来是什么意思**」）：
/// - 阶段 162 用的是 `go-up-symbolic`（一个向上的箭头）⇒ **看不出是啥**
///   （「向上」跟「重建」没关系，跟「上一级目录」倒是撞了）；
/// - 试过一堆自绘方案（圆弧绕着容器方框 / 容器方框＋向下的箭头 / 方形循环
///   箭头 / 两个叠方框…）⇒ **自己画的都不如主题的清楚**（16px 下自己画的
///   圆弧**看不出箭头**、叠方框**糊成一团**）⇒ 16px 的图标必须**用主题里
///   按 16px 设计过的**那一个；
/// - ⇒ `edit-copy-symbolic`（两个叠起来的方块 =「多一个 / 再来一份」）
///   ⇒ 意思对得上「按 compose 文件**再造一批**容器」；
/// - ⇒ **跟右键菜单里的「重建」用同一个图标**（同一个动作 ⇒ 同一个图标）。
const ICON_RECREATE: &str = "edit-copy-symbolic";
/// 图标名：**编辑**。
const ICON_EDIT: &str = "document-edit-symbolic";
/// 图标名：**销毁**（垃圾桶）。
///
/// ⚠️ **只在右键菜单里用**（阶段 163）—— 操作列不放「销毁」：
///   `docker compose down` 会**删掉容器** ⇒ **项目行整行消失** ⇒ 用户
///   再也点不到那一行的任何按钮（用户 2026-11-24 的原话：「down 了之后
///   容器界面就看不到了」）⇒ 那种按钮不该放在「一眼就点得到」的位置。
const ICON_DOWN: &str = "user-trash-symbolic";
/// 图标名：**文件**（容器里的文件）。
const ICON_FILES: &str = "folder-symbolic";
/// 图标名：**日志**。
const ICON_LOGS: &str = "text-x-generic-symbolic";

/// **动作的短名字**（测试钩子用；跟界面上没关系）。
pub fn action_key(k: ActionKind) -> &'static str {
    match k {
        ActionKind::Detail => "detail",
        ActionKind::Files => "files",
        ActionKind::Logs => "logs",
        ActionKind::Enter => "enter",
        ActionKind::Edit => "edit",
        ActionKind::Start => "start",
        ActionKind::Stop => "stop",
        ActionKind::Restart => "restart",
        ActionKind::Down => "down",
        ActionKind::Recreate => "recreate",
        ActionKind::CopyId => "copyid",
    }
}

/// **compose 项目行的名字**（阶段 162：加粗 ＋ 灰色小字「compose」）。
///
/// ⚠️ **为什么要有**（用户 2026-11-24：「compose 项目应该显著的区别于容器」）：
///   光靠「有展开箭头」不够 ⇒ 一眼看不出那是个**项目**（还是一堆容器）。
///   ⇒ 名字**加粗** ＋ 后面跟一个**灰色小字「compose」**。
/// ⚠️ **名字要转义**（markup 里 `&` `<` 会坏事）。
/// ⚠️ **「compose」是 docker 的产品名** ⇒ 中英文界面都这么写（不翻译）。
fn project_name_markup(name: &str) -> String {
    format!(
        "<b>{n}</b> <span foreground=\"{c}\">compose</span>",
        n = glib::markup_escape_text(name),
        c = crate::theme::dim_hex()
    )
}

/// **这一行的操作按钮**（**纯函数** ⇒ 单测能验，不用开 GTK）。
///
/// ⚠️ **阶段 160：操作列只留「启动 / 停止 / 重启」**（用户 2026-11-24：
///   「操作列只保留启动、停止、重启，把其他的都移到右键菜单里」）
///   ⇒ **容器行 3 颗**。
///
/// ⚠️ **阶段 162：compose 项目行还要 up / down**（用户 2026-11-24：
///   「compose 项目的操作列中应该还有与 up 和 down 对应的按钮」）
///   ⇒ 当时是**项目行 5 颗**（启动 / 停止 / 重启 ＋ `docker compose up` ＋
///   `docker compose down`）。
/// ⚠️ **阶段 163：up / down 两颗换成一颗「重建」**（用户 2026-11-24：
///   「docker compose down 了之后，是不是容器界面就看不到了？那 up 也没
///   意义了」⇒「那就不要 up 和 down 了，就来一个 recreate 吧，就是
///   docker compose up -d --force-recreate」）：
/// - **为什么不能留 down**：`down` 会**删掉容器** ⇒ compose 项目行是
///   **从容器推导出来的** ⇒ 容器没了**那一行整行消失** ⇒ 用户再也点不到
///   那一行的任何按钮（包括「拉回来」的那个）⇒ 是个**死路**；
/// - **为什么 up 也要一起换掉**：光 `up` **只补缺的**（配置没变的容器
///   不重建）⇒ 用户要的「按新文件重建一遍」达不到 ⇒ 一律
///   `up -d --force-recreate`；
/// ⇒ **项目行 4 颗**（启动 / 停止 / 重启 / 重建）⇒ **容器行 3 颗**。
///
/// ⚠️ **能不能点看的是 `raw_state`**（docker 的**原始**状态），不是界面上的
///   中文标签 ⇒ 切到英文界面也不会判错。
pub fn row_actions(kind: &str, raw_state: &str) -> Vec<RowAction> {
    // ⚠️ **项目行有三种状态**（阶段 155）：`running`（全在跑）/ `exited`
    //   （全停了）/ `mixed`（一半在跑）⇒ 「启动 / 停止」哪个能点得看这个。
    let all_running = raw_state == "running";
    let none_running = raw_state == "exited";
    // ⚠️ **「重启」只在「有东西在跑」时能点**（全停着的项目重启它也没用）。
    let some_running = !none_running;
    let mut acts = vec![
        RowAction {
            kind: ActionKind::Start,
            icon: ICON_START.into(),
            enabled: !all_running,
        },
        RowAction {
            kind: ActionKind::Stop,
            icon: ICON_STOP.into(),
            enabled: !none_running,
        },
        RowAction {
            kind: ActionKind::Restart,
            icon: ICON_RESTART.into(),
            enabled: some_running,
        },
    ];
    // ⚠️ **阶段 163：只有 compose 项目行才有「重建」**
    //   （容器没有「项目」这层 ⇒ 这条命令对它没意义）。
    if kind == KIND_PROJECT {
        // ⚠️ **一直能点**：项目停了（exited）也能重建（`up` 会把它拉起来）；
        //   项目在跑也能重建（`--force-recreate` 换一批新容器）⇒
        //   灰掉反而挡路。
        acts.push(RowAction {
            kind: ActionKind::Recreate,
            icon: ICON_RECREATE.into(),
            enabled: true,
        });
    }
    acts
}

/// **右键菜单里有哪些项**（**纯函数** ⇒ 单测能验）。
///
/// ⚠️ **阶段 160**：操作列瘦身后，其余动作全搬到右键菜单 ⇒
///   菜单**按行类型**给不同的项：
/// - **容器行**：进容器 / 文件 / 日志 / 属性 ＋ 启停 ＋ 复制容器 ID；
/// - **compose 项目行**：编辑 compose 文件 / 属性 ＋ 启停 ＋ 销毁 / 重建。
///
/// ⚠️ **阶段 163**：
/// - **「重建」＝ `docker compose up -d --force-recreate`**（跟操作列那颗
///   按钮是**同一件事** ⇒ 一律按 compose 文件重建）；
/// - **「销毁」留着**（`docker compose down`）⇒ 它是**唯一能拆掉整个项目**
///   的入口（改完卷 / 换目录之后要清场）⇒ 但确认框会明说「**这个项目也会
///   从列表里消失**」（用户 2026-11-24：「down 了之后容器界面就看不到了」）
///   ⇒ 不再是「按下去才发现」。
///
/// ⚠️ **分三段**（用分隔线隔开）：「看」→「启停」→「危险动作」。
pub fn menu_actions(kind: &str, raw_state: &str) -> Vec<MenuAction> {
    // ⚠️ **启停的可点性看原始状态**（跟「操作」列**同一套算术** ⇒ 菜单与列一致；
    //   否则运行中的容器在菜单里还能点「启动」⇒ 点了只是空转）。
    let op_enabled = |k: ActionKind| -> bool {
        row_actions(kind, raw_state)
            .into_iter()
            .find(|a| a.kind == k)
            .map(|a| a.enabled)
            .unwrap_or(true)
    };
    if kind == KIND_PROJECT {
        vec![
            MenuAction {
                kind: ActionKind::Edit,
                icon: ICON_EDIT.into(),
                label: tr("编辑 compose 文件"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Detail,
                icon: ICON_DETAIL.into(),
                label: tr("属性"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Start,
                icon: ICON_MENU_START.into(),
                label: tr("启动"),
                group: 1,
                enabled: op_enabled(ActionKind::Start),
            },
            MenuAction {
                kind: ActionKind::Stop,
                icon: ICON_MENU_STOP.into(),
                label: tr("停止"),
                group: 1,
                enabled: op_enabled(ActionKind::Stop),
            },
            MenuAction {
                kind: ActionKind::Restart,
                icon: ICON_RESTART.into(),
                label: tr("重启"),
                group: 1,
                enabled: op_enabled(ActionKind::Restart),
            },
            MenuAction {
                kind: ActionKind::Down,
                icon: ICON_DOWN.into(),
                label: tr("销毁"),
                group: 2,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Recreate,
                icon: ICON_RECREATE.into(),
                label: tr("重建"),
                group: 2,
                enabled: true,
            },
        ]
    } else {
        vec![
            MenuAction {
                kind: ActionKind::Enter,
                icon: ICON_TERMINAL.into(),
                label: tr("进入容器"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Files,
                icon: ICON_FILES.into(),
                label: tr("文件"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Logs,
                icon: ICON_LOGS.into(),
                label: tr("日志"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Detail,
                icon: ICON_DETAIL.into(),
                label: tr("属性"),
                group: 0,
                enabled: true,
            },
            MenuAction {
                kind: ActionKind::Start,
                icon: ICON_MENU_START.into(),
                label: tr("启动"),
                group: 1,
                enabled: op_enabled(ActionKind::Start),
            },
            MenuAction {
                kind: ActionKind::Stop,
                icon: ICON_MENU_STOP.into(),
                label: tr("停止"),
                group: 1,
                enabled: op_enabled(ActionKind::Stop),
            },
            MenuAction {
                kind: ActionKind::Restart,
                icon: ICON_RESTART.into(),
                label: tr("重启"),
                group: 1,
                enabled: op_enabled(ActionKind::Restart),
            },
            MenuAction {
                kind: ActionKind::CopyId,
                icon: "edit-copy-symbolic".into(),
                label: tr("复制容器 ID"),
                group: 2,
                enabled: true,
            },
        ]
    }
}

/// 右键菜单里的一项（**干什么** ＋ **画哪个图** ＋ **写什么字** ＋ **第几段**）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuAction {
    /// 干什么（派发时按它分派）。
    pub kind: ActionKind,
    /// 图标名（**Adwaita 真有的**）。
    pub icon: String,
    /// 菜单上的文字。
    pub label: &'static str,
    /// 第几段（**同段之间不插分隔线** ⇒ 分隔线只在段号变化时插）。
    pub group: u8,
    /// 能不能点（**启停看原始状态** ⇒ 菜单与「操作」列一致）。
    pub enabled: bool,
}

/// **操作列最多几颗图标**（列宽按它算 ⇒ 两种行都放得下）。
pub fn action_col_icons_max() -> usize {
    row_actions(KIND_PROJECT, "running")
        .len()
        .max(row_actions(KIND_CONTAINER, "running").len())
}

/// 一颗操作图标的 **tooltip**（禁用时说明「为什么不能点」）。
///
/// ⚠️ **灰图标要说明为什么**（UI 审查 D15 的老规矩）：以前是灰按钮配一句
///   说明 ⇒ 现在灰的是图标 ⇒ 说明也得跟过来。
pub fn action_tooltip(a: &RowAction, is_project: bool, raw_state: &str) -> &'static str {
    let all_running = raw_state == "running";
    let none_running = raw_state == "exited";
    match a.kind {
        ActionKind::Detail => {
            if is_project {
                tr("看这个 compose 项目的详情")
            } else {
                tr("看这个容器的详情（概览 / 日志 / 监控 / 文件）")
            }
        }
        ActionKind::Files => tr("看容器里的文件"),
        ActionKind::Logs => tr("看容器的日志"),
        ActionKind::CopyId => tr("复制这个容器的 ID"),
        ActionKind::Enter => tr("在这个容器里开一个 shell"),
        ActionKind::Edit => tr("编辑这个 compose 项目的 compose 文件"),
        ActionKind::Start => {
            if all_running {
                if is_project {
                    tr("项目正在运行，不用再启动（先「停止」才能启动）")
                } else {
                    tr("容器正在运行，不用再启动（先「停止」才能启动）")
                }
            } else if is_project {
                tr("启动这个 compose 项目的容器")
            } else {
                tr("启动这个容器")
            }
        }
        ActionKind::Stop => {
            if none_running {
                if is_project {
                    tr("项目没有在运行，不用停（先「启动」才能停）")
                } else {
                    tr("容器没有在运行，不用停（先「启动」才能停）")
                }
            } else if is_project {
                tr("停止这个 compose 项目的容器")
            } else {
                tr("停止这个容器")
            }
        }
        ActionKind::Restart => tr("重启这个容器"),
        // ⚠️ **阶段 163**：重建在「操作」列里**只有图标没有字** ⇒
        //   tooltip 得**写清楚跑的是哪条命令**（不然 ⬆ 看不出是啥）。
        ActionKind::Down => tr("停掉并删除这个 compose 项目的容器与网络（docker compose down）"),
        ActionKind::Recreate => {
            tr("按 compose 文件重新创建并启动这个项目（docker compose up -d --force-recreate）")
        }
    }
}

/* ---------------- 文件表的列（阶段 140） ---------------- */

/// **图标名**（`folder-symbolic` / `text-x-generic-symbolic` / …）。
///
/// 干什么用：给名称列前面那条**窄图标列**用 ⇒ 一眼分得清目录 / 文件 /
///   链接（用户 2026-11-19：「文件、目录之类的要有图标吧，不然看不出来是啥」）。
const FC_ICON: usize = 0;
/// 名称。
const FC_NAME: usize = 1;
/// 大小。
const FC_SIZE: usize = 2;
/// 权限（**通用写法**，不翻译 —— 见 [`perm_text`]）。
const FC_PERM: usize = 3;
/// **修改时间**（用户 2026-11-27：「容器文件管理中没有显示修改时间」）。
const FC_MTIME: usize = 4;
/// **符号链接指向哪里**（用户 2026-11-27：「最好能显示符号链接指向的文件」）。
const FC_LINK: usize = 5;
/// 类型**文字**（「目录」/「文件」/「链接」）。
///
/// ⚠️ **界面上不显示**（图标已经表达了）⇒ 留着给 **tooltip** 和测试用。
const FC_TYPE: usize = 6;
/// 类型**键**（`"dir"` / `"file"` / …；**界面上不显示**）。
const FC_KIND: usize = 7;
/// 文件表的列数。
const FC_N_COLS: usize = 8;

/// **跟随符号链接时最多追几层**（链接套链接）。
///
/// ⚠️ **为什么要限层**：两个链接**互相指**（`a -> b`、`b -> a`）时，
///   不限层就会**一直追下去**（永远追不到头）⇒ 界面上看着像卡死了。
///   ⇒ 追够 [`LINK_FOLLOW_MAX`] 层就放弃（报「链接指向的文件不存在」）。
const LINK_FOLLOW_MAX: usize = 8;

/// 纯图标按钮（**文字挪进 tooltip**）。
///
/// ⚠️ **为什么要这个**：用户 2026-10-28：「主界面尽量少用文字」⇒ 文件面板
///   的按钮早就是纯图标了 ⇒ 容器文件页也得同一口径（以前是两行**大文字
///   按钮**，白占一整行高度）。
fn icon_btn(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    b.set_child(Some(&img));
    // 纯图标按钮 ⇒ 可访问名称跟着 tooltip 走
    crate::a11y::watch_tooltip(&b);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b
}

/// 条目类型 ⇒ **图标名**（阶段 140）。
///
/// ⚠️ **图标名必须是 Adwaita 真有的**（实测：`emblem-symbolic-link-symbolic`
///   在 Adwaita 里**不存在** ⇒ 只能用真有的）。
fn file_icon(kind: docker::FileKind) -> &'static str {
    match kind {
        docker::FileKind::Dir => "folder-symbolic",
        docker::FileKind::File => "text-x-generic-symbolic",
        // ⚠️ Adwaita 没有「软链徽标」的 symbolic 版 ⇒ 用「链接」图标。
        docker::FileKind::Link => "insert-link-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

/// 权限列的**通用写法**（阶段 140）。
///
/// ⚠️ **为什么不翻成中文**（用户 2026-11-19：「权限好像一般没有说还要翻译成
///   汉语的」）：Linux / Docker / `chmod` 用的都是同一套写法（`drwxr-xr-x`）
///   ⇒ 翻译成中文反而跟命令对不上，也没法复制粘贴。
fn perm_text(perms: &str) -> String {
    perms.to_string()
}

/// 文件页的**错误分类**（阶段 172 / UI-07，治 P1-4）。
///
/// ⚠️ **为什么要有分类**：Docker / OCI 的报错原文对普通用户是天书
///   （`permission denied`、`OCI runtime exec failed`……）⇒ 用户既看不懂
///   **发生了什么**，也不知道**下一步该干什么**。
///
/// ⚠️ **两条硬规矩**（审核 P1-4）：
/// 1. **说人话与保留原文必须同时成立** ⇒ 分类只给界面看，**原文照留**
///    （详情 / tooltip 里能看到）；
/// 2. **未知就保守** ⇒ 拿不准就归「未知」，**不瞎猜**是权限还是网络。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilesErrKind {
    /// 权限不足。
    Permission,
    /// 容器没在跑。
    NotRunning,
    /// 只读文件系统。
    ReadOnly,
    /// 路径不存在。
    NoSuchPath,
    /// 容器里缺命令（镜像太精简）。
    MissingCommand,
    /// 超时。
    Timeout,
    /// 连不上 Docker 守护进程。
    DaemonUnreachable,
    /// 认不出来。
    Unknown,
}

/// 一条文件页错误：**分类 ＋ 给用户看的话 ＋ 原文**。
#[derive(Clone, Debug)]
pub struct FilesErr {
    pub kind: FilesErrKind,
    /// 界面上显示的那一句（中文分类 ＋ 下一步建议）。
    pub brief: String,
    /// 原始错误（详情里看，别丢）。
    pub raw: String,
}

/// 把 Docker / OCI 的报错原文归类。
///
/// ⚠️ **归类只看「明确的特征」**（关键词 ＋ 退出码），**不猜**：
///   认不出来就归 [`FilesErrKind::Unknown`]，界面显示「操作未完成」＋ 原文。
pub fn classify_files_error(raw: &str) -> FilesErr {
    let low = raw.to_ascii_lowercase();
    let kind = if low.contains("permission denied")
        || low.contains("operation not permitted")
        || low.contains("not permitted")
    {
        FilesErrKind::Permission
    } else if low.contains("read-only file system")
        || low.contains("read only file system")
        || low.contains("readonly file system")
    {
        FilesErrKind::ReadOnly
    } else if low.contains("is not running")
        || low.contains("container is stopped")
        || low.contains("is stopped")
        || low.contains("no such container")
        || low.contains("container not found")
    {
        FilesErrKind::NotRunning
    } else if low.contains("no such file or directory") {
        FilesErrKind::NoSuchPath
    } else if low.contains("executable file not found")
        || low.contains("command not found")
        || low.contains("no such file or directory: ls")
    {
        FilesErrKind::MissingCommand
    } else if low.contains("timed out") || low.contains("timeout") || low.contains("deadline exceeded")
    {
        FilesErrKind::Timeout
    } else if low.contains("cannot connect to the docker daemon")
        || low.contains("docker daemon")
        || low.contains("error during connect")
        || low.contains("is the docker daemon running")
    {
        FilesErrKind::DaemonUnreachable
    } else {
        FilesErrKind::Unknown
    };
    let brief = match kind {
        FilesErrKind::Permission => tr(
            "权限不足 ⇒ 读不了这个目录。检查容器里这个文件的属主和权限，或换一个权限更高的账号进容器。",
        ),
        FilesErrKind::NotRunning => tr(
            "容器没在运行 ⇒ 进不去。先把这个容器启动起来（或切到正在运行的容器）再试。",
        ),
        FilesErrKind::ReadOnly => tr(
            "文件系统是只读的 ⇒ 改不了。检查容器的挂载与权限；别用「提权重试」硬来。",
        ),
        FilesErrKind::NoSuchPath => tr(
            "路径不存在 ⇒ 刷新一下目录再试，或者检查路径拼写（容器里的路径和本地不一样）。",
        ),
        FilesErrKind::MissingCommand => tr(
            "这个容器里没有 ls 命令 ⇒ 列不了目录。换个带 ls 的镜像（常见的都有），或者用「终端」页进容器自己看。",
        ),
        FilesErrKind::Timeout => tr(
            "操作超时了 ⇒ 稍后重试；如果一直这样，先看看容器是不是卡住了。",
        ),
        FilesErrKind::DaemonUnreachable => tr(
            "连不上 Docker 服务 ⇒ 检查这台机器上的 Docker 是否在跑，以及当前账号有没有权限访问它。",
        ),
        FilesErrKind::Unknown => tr("操作没完成 ⇒ 详情里是原始报错。"),
    };
    FilesErr {
        kind,
        brief: brief.to_string(),
        raw: raw.to_string(),
    }
}

/// 文件页的**报错 ⇒ 说人话**（阶段 140 → 阶段 172 / UI-07）。
///
/// ⚠️ **原始错误不能丢**（审核 P1-4）：这句只是「给人看的分类」，
///   原文在详情 / tooltip 里 ⇒ 「说人话」与「保留原文」同时成立。
fn files_err_zh(msg: &str) -> String {
    classify_files_error(msg).brief
}

/// 过滤：全部 / 运行中 / 已停止。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Running,
    Stopped,
}

/// **容器表格的响应式分档**（阶段 171 / UI-02，治 P0-1）。
///
/// ⚠️ **为什么要有分档**：默认主窗 1200×780 扣掉侧栏只剩 ~960px，而六列
///   加上「操作」列的**自然宽度合计 1139px** ⇒ 「操作」列被挤出可视区
///   ⇒ 启动 / 停止 / 重启 / 详情 这些**核心操作用户根本看不见**
///   （UI 审核 P0-1：已确认）。
///
/// ⚠️ **分档不只看「显不显示」，还要看「最小宽度」**：
///   GTK4 的 TreeView 宽度下限 = **可见列的 min_width 之和** ⇒ 只藏列、
///   不降 min_width 的话，表格**根本缩不下去** ⇒ 分档永远换不到
///   （这正是第一版实现的坑：`1024×700` 那一档永远进不去）。
///   ⇒ 每一档都自带一套「更紧的 min_width」。
///
/// ⚠️ **退让顺序是按信息量排的**：
///   名称 / 状态 / 操作 **永远保留**（操作是核心动作，名称与状态是识别依据）；
///   ID 最先让位（本来就只显示前 8 位，完整值在详情弹窗里）；
///   然后是端口、资源；镜像比名称更早让位（窄屏优先看名字）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnDensity {
    /// 七列全留（内容区 ≥ 1170px）。
    Wide,
    /// 收起「ID」——其余全留（内容区 ≥ 940px）。
    Medium,
    /// 再收起「端口」「资源」（内容区 ≥ 530px）。
    Narrow,
    /// 只留「名称」「状态」（再窄就只剩这个了）。
    Compact,
}

impl ColumnDensity {
    /// 这一档的**稳定区间**：宽度落在区间里就**不换档**（滞回，防抖）。
    ///
    /// ⚠️ **四段必须无缝拼满**：否则会出现「落在缝里 ⇒ 反复横跳」。
    fn stable_band(self) -> (i32, i32) {
        match self {
            Self::Wide => (1170, i32::MAX),
            Self::Medium => (940, 1169),
            Self::Narrow => (530, 939),
            Self::Compact => (0, 529),
        }
    }

    /// 按**实际分配宽度**选档（进入阈值）。
    ///
    /// ⚠️ **阈值必须比这一档的最小宽度总和高**：否则表格会被 min_width
    ///   撑住、永远量不到那个宽度（见 struct 注释）。
    fn for_width(width: i32) -> Self {
        if width >= 1170 {
            Self::Wide
        } else if width >= 940 {
            Self::Medium
        } else if width >= 530 {
            Self::Narrow
        } else {
            Self::Compact
        }
    }

    /// 带滞回的换档：先看当前档的稳定区间，不在区间里才按进入阈值重选。
    fn for_resize(self, width: i32) -> Self {
        let (lo, hi) = self.stable_band();
        if width >= lo && width <= hi {
            self
        } else {
            Self::for_width(width)
        }
    }

    /// 这一档里某一列的 `(是否展开, 最小宽度)`；`None` = 这一列不显示。
    ///
    /// ⚠️ **展开列的分布也是分档的一部分**（UI 审核验收：「宽窗口不出现
    ///   大片空谷」）：只留两三列却都展开 ⇒ 富余全堆到一两列上 ⇒ 空谷。
    ///   ⇒ 窄档让**名称**也参与展开（名字是最容易长的那列）。
    fn column_spec(self, model_col: usize) -> Option<(bool, i32)> {
        match self {
            Self::Wide => Some(match model_col {
                COL_NAME => (false, 200),
                COL_ID => (false, 84),
                COL_IMAGE => (true, 240),
                COL_PORTS => (true, 180),
                COL_STATE => (true, 125),
                COL_RES => (true, 190),
                _ => return None,
            }),
            Self::Medium => Some(match model_col {
                COL_NAME => (false, 175),
                COL_IMAGE => (true, 200),
                COL_PORTS => (true, 140),
                COL_STATE => (true, 110),
                COL_RES => (true, 165),
                _ => return None,
            }),
            Self::Narrow => Some(match model_col {
                COL_NAME => (true, 135),
                COL_IMAGE => (true, 150),
                COL_STATE => (true, 100),
                _ => return None,
            }),
            Self::Compact => Some(match model_col {
                COL_NAME => (true, 115),
                COL_STATE => (true, 95),
                _ => return None,
            }),
        }
    }
}

fn apply_column_density(ui: &DockerUi, density: ColumnDensity) {
    for (model_col, col) in &ui.data_cols {
        match density.column_spec(*model_col) {
            Some((expand, min_w)) => {
                col.set_visible(true);
                col.set_expand(expand);
                // ⚠️ **必须一起降 min_width**：否则表格缩不到下一档去
                //   （TreeView 的宽度下限 = 可见列 min_width 之和）。
                col.set_min_width(min_w);
            }
            None => {
                col.set_visible(false);
            }
        }
    }
    ui.column_density.set(density);
}

/// 自动刷新的间隔（秒）——跟 dbx 一致。
/// ⚠️ **公开**（阶段 176 / UI-14）：性能回归要**盯住**这个间隔
///   （太密就是忙轮询 ⇒ 主线程白烧）⇒ 写死就盯不住。
pub const AUTO_REFRESH_SECS: u32 = 7;

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
    /// **每个分段各自**的状态文字（UI 审查 D7）。
    ///
    /// ⚠️ **为什么不能只有一行**：状态行是**整页共用**的 ⇒ 切到「文件」页
    ///   底部还挂着「正在读取远程容器…」⇒ **上一段的状态污染下一段**。
    ///   ⇒ 每个分段各存一份，底部那行**只显示当前分段**的。
    status_text: RefCell<HashMap<&'static str, String>>,
    /// **空态 / 错态**那块（UI 审查 D6）。
    ///
    /// ⚠️ **为什么不能只有「一张空表 + 底部一行小灰字」**：远端没装 docker、
    ///   当前账号不在 docker 组、docker 守护进程没起 —— 这三个**长得
    ///   一模一样** ⇒ 用户只能猜 ⇒ 这里给**分类过的标题 + 解释 + 重试**。
    empty: crate::emptystate::DynamicEmptyState,
    /// 镜像 / 卷 / 网络三页**各自的错态**（阶段 135）。
    ///
    /// ⚠️ **为什么以前只有容器页有**（用户 2026-11-19 报的）：这三页失败时
    ///   **只写底部一行小灰字** ⇒ 界面上就是「一张空表 + 角落一行小字」
    ///   ⇒ 用户以为是自己没镜像/没卷，**根本看不出是没权限**。
    ///   ⇒ 三页也各挂一块**大号错态**（标题按错误分类 + 解释 + 重试）。
    images_empty: crate::emptystate::DynamicEmptyState,
    volumes_empty: crate::emptystate::DynamicEmptyState,
    networks_empty: crate::emptystate::DynamicEmptyState,
    /// 「没权限」的提示**弹过了没有**（**每个连接只弹一次**；阶段 135）。
    ///
    /// ⚠️ **为什么记在这儿**：这是**每个连接**的状态（`DockerUi` 挂在
    ///   `PanelState` 上 ⇒ 一条连接一份）⇒ 换个连接就重新提示一次。
    denied_notified: Cell<bool>,
    /// 搜索框（**键盘 `/` 要能聚焦到它** ⇒ 留个句柄）。
    search_entry: gtk::SearchEntry,
    /// 底部那行**右侧的提示**（「双击容器行可进入 shell」）。
    hint: gtk::Label,
    /// **行的右键菜单**（UI 审查 D5）。
    row_menu: RefCell<Option<gtk::Popover>>,
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
    /// **上一次点「操作」列里的图标**是什么时刻（微秒，单调钟）。
    ///
    /// ⚠️ **干什么用**：双击图标时，表格自己的「激活」（进 shell）也会触发
    ///   ⇒ 一次点击干两件事 ⇒ 记下时刻 ⇒ **激活时看一眼**（太近就当是
    ///   这一次图标点击的延续 ⇒ 跳过）。
    last_action_click: Cell<Option<i64>>,
    /// **「操作」列**（阶段 155）。
    ///
    /// ⚠️ **为什么留着句柄**：命中判定要认出「点的是不是这一列」
    ///   （`path_at_pos` 会把列一起返回 ⇒ 拿它跟这一列比）。
    action_col: gtk::TreeViewColumn,
    /// 数据列句柄：响应式退让只改可见性，不重建模型或列。
    data_cols: Vec<(usize, gtk::TreeViewColumn)>,
    column_density: Cell<ColumnDensity>,
    column_layout_pending: Cell<bool>,
    column_resize_active: Cell<bool>,
    /// **视口宽度的来源**（响应式分档按它换档）。
    ///
    /// ⚠️ **为什么不能靠 `notify::width`**（阶段 171 实测）：
    ///   GTK4 **不会**在控件重新分配时发 `notify::width` —— 窗口从 1200
    ///   缩到 900，`notify::width` **一次都不触发** ⇒ 响应式列永远不重排
    ///   （第一版实现就栽在这：窗口变窄了，操作列还是被挤在外面）。
    ///   ⇒ 改看 **ScrolledWindow 的 `hadjustment.page-size`**（= 视口宽度）
    ///   —— 实测每次变宽变窄都会触发。
    viewport_adj: gtk::Adjustment,
    /// **详情区的内容盒**（容器详情 ＋ compose 项目详情）。
    ///
    /// ⚠️ **阶段 155**：它住在**弹窗**里（不是页面下半区）⇒ 建弹窗时要把它
    ///   装进去。
    detail_box: gtk::Box,
    /// **日志窗的内容盒**（标题行 ＋ 日志正文；阶段 165）。
    logs_box: gtk::Box,
    /// **文件窗的内容盒**（标题行 ＋ 文件区；阶段 165）。
    files_box: gtk::Box,
    /// **详情弹窗**（阶段 165：**概览**单独一扇窗）。
    ///
    /// ⚠️ **为什么是弹窗**：用户说「不要下面那部分了，整个页面都改成列表」
    ///   ⇒ 页面只剩一张满高的列表 ⇒ 详情（概览 / 日志 / 文件 ＋
    ///   compose 项目）只能**另开一扇窗**。
    /// ⚠️ **建一次、复用**：关掉只是**隐藏**（不销毁）⇒ 控件状态不丢。
    detail_window: RefCell<Option<gtk::Window>>,
    /// **日志弹窗**（阶段 165：日志**独立一扇窗**）。
    ///
    /// ⚠️ **用户 2026-11-24**：「其他这几个 tab 都单独出来吧，每次点鼠标
    ///   右键就只出来一个窗口就行了」⇒ 概览 / 日志 / 文件**各一扇窗**，
    ///   同一时刻只显示一扇。
    logs_window: RefCell<Option<gtk::Window>>,
    /// **文件弹窗**（阶段 165：文件**独立一扇窗**）。
    files_window: RefCell<Option<gtk::Window>>,
    /// **「查看文件」窗**（阶段 167：双击文件弹出的只读文本窗）。
    ///
    /// ⚠️ **为什么存着**：冒烟要能量它的**正文高度**（用户 2026-11-25
    ///   「查看容器里面的文件，现在只能显示1行」）⇒ 没有句柄就量不到。
    file_view_window: RefCell<Option<gtk::Window>>,
    /// 「查看文件」窗里的**正文控件**（同上，量高度用）。
    file_view_text: RefCell<Option<gtk::TextView>>,
    /// 详情弹窗里的**「关闭」按钮**（阶段 156：容器详情 / 项目详情各一个）。
    ///
    /// ⚠️ **为什么内容里必须有它**（用户 2026-11-23 报的「弹出对话框没法
    ///   关闭」）：详情弹窗是**普通 `gtk::Window`** ⇒ GTK4 **不替它画标题栏**
    ///   ⇒ 窗口管理器给不给装饰都不确定 ⇒ **用户可能一个关闭入口都看不到**
    ///   ⇒ 就在内容里放一个，**永远看得见**。
    /// ⚠️ **一个 widget 只能有一个 parent** ⇒ 两块详情各要一个。
    detail_close_btns: RefCell<Vec<gtk::Button>>,
    /// **日志页**（阶段 112）。
    log: LogUi,
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
    /// 容器内终端的**落点**（**独立窗口**；懒建）。
    ///
    /// ⚠️ **为什么又改回弹窗**（用户 2026-11-23）：UI 审查 D19 把它从
    ///   「另开窗口」改成「详情区里的一个页签」⇒ 用户觉得**弹出一个
    ///   终端窗口**更合适（跟 Docker Desktop 的 Exec 一样）⇒ 改回来。
    ///   ⚠️ **D19 提的老坑这次补上**：那个窗口**没有 transient parent**
    ///   （可能落在主窗后面，用户还以为没弹出来）⇒ 现在挂到主窗口下面。
    term_window: RefCell<Option<gtk::Window>>,
    /// 终端**窗口**建好了没有（建过就复用 ⇒ 滚动缓冲不丢）。
    term_window_ready: Cell<bool>,
    /// **正在关**容器内终端（**重入保护**，阶段 125）。
    ///
    /// ⚠️ **为什么需要**：「关闭终端」按钮与页签关闭钩子都会调
    ///   `stop_terminal` ⇒ 不挡一下就是「自己调自己」。
    term_closing: Cell<bool>,
    /// 容器终端的**工具条**（阶段 119：切 shell / 重连 / 清屏 / 搜索）。
    ///
    /// ⚠️ **为什么放在终端页里**：这几个操作**只对容器终端**有意义
    ///   ⇒ 放到容器页的工具条上会让人以为「能对选中的容器做」
    ///   （其实那个容器可能压根没开终端）。
    term_bar: RefCell<Option<gtk::Box>>,
    /// 工具条上的 **shell 下拉**（阶段 119）。
    ///
    /// ⚠️ **第 0 项是「自动」**（挨个试 `/bin/sh` → …）⇒ 跟原来的行为
    ///   一模一样；后面几项是**点名**（用不了就报错，**不悄悄换**）。
    term_shell_combo: RefCell<Option<gtk::ComboBoxText>>,
    /// 容器终端的**搜索条**（阶段 119）。
    term_search_bar: RefCell<Option<gtk::SearchBar>>,
    /// 容器终端的**搜索输入框**（阶段 119；测试钩子要往里填词）。
    term_find_entry: RefCell<Option<gtk::Entry>>,
    /// 容器终端的 **shell 下拉里都有啥**（阶段 119）。
    ///
    /// ⚠️ **为什么自己记一份**：`gtk::ComboBoxText` **没有「列出全部选项」
    ///   的接口**（只有 `active_text()`）⇒ 记一份自己 append 过的 ⇒
    ///   测试能看见「用户到底能选什么」。
    term_shell_items: RefCell<Vec<String>>,
    /// 容器终端的**状态标签**（阶段 119：重连 / 换 shell 时要改它）。
    term_status: RefCell<Option<gtk::Label>>,
    /// 「重连」按钮（阶段 119）。
    term_reconnect_btn: RefCell<Option<gtk::Button>>,
    /// 「清屏」按钮（阶段 119）。
    term_clear_btn: RefCell<Option<gtk::Button>>,
    /// 「搜索」开关按钮（阶段 119）。
    term_search_btn: RefCell<Option<gtk::ToggleButton>>,
    /// 容器终端**当前是哪个容器**（阶段 119：重连要用）。
    term_name: RefCell<String>,
    /// 容器终端**当前用的 shell**（阶段 119；空 = 还没建起来）。
    term_shell: RefCell<String>,
    /// 容器终端的**代次号**（阶段 119）。
    ///
    /// ⚠️ **干什么用**：重连时**旧会话**可能还有数据在队列里 ⇒ 那些数据
    ///   会喂进**同一个**终端控件（新旧混在一起）⇒ 每次起会话**加一代**，
    ///   转发事件时**只认当前代** ⇒ 过期的一律丢掉。
    term_gen: Cell<u64>,
    /// 容器终端**重连过几次**（测试钩子：断言「重连真的发生了」）。
    term_reconnects: Cell<u32>,
    /// 容器终端的**请求序号**（阶段 119）。
    ///
    /// ⚠️ **为什么和 `term_gen` 分开**：`term_gen` 是「**已经挂上去的会话**」
    ///   的代号（数据过滤用）⇒ **只有真的换会话时才加** ⇒ 探 shell 失败时
    ///   **不动它**（旧会话的输出照旧显示）；而 `term_req` 是「**请求**」的
    ///   序号（每次点「重连」都加）⇒ 用来认出「这个请求已经过期了」
    ///   （用户连点两次 ⇒ 后一个说了算，前一个的通道要关掉）。
    term_req: Cell<u64>,
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
    /// 镜像的**操作条**（阶段 120：推送 / 导出 / 打标签 / 删除）。
    images_bar: gtk::Box,
    /// 「推送」按钮（阶段 120）。
    images_push_btn: gtk::Button,
    /// 「导出…」按钮（阶段 120）。
    images_export_btn: gtk::Button,
    /// 「打标签…」按钮（阶段 120）。
    images_tag_btn: gtk::Button,
    /// 「删除」按钮（阶段 120）。
    images_remove_btn: gtk::Button,
    /// 操作条上的**提示标签**（当前选中的镜像）。
    images_hint: gtk::Label,
    /// **当前选中的镜像**（`仓库:标签`；空 = 没选）。
    images_selected: RefCell<String>,
    /// **镜像操作**的结果文字（**等列表刷新完再贴回状态行**）。
    /// **三个分段的完整列表**（筛选时要用 ⇒ 表格只放**筛出来的**）。
    ///
    /// ⚠️ **为什么留着完整列表**：UI 审查 D12 指出镜像 / 卷 / 网络三页
    ///   **一点筛选都没有** ⇒ 本机 44 个镜像里 30 个是同一个 tag 的重复。
    images_all: RefCell<Vec<docker::ImageInfo>>,
    volumes_all: RefCell<Vec<docker::VolumeInfo>>,
    networks_all: RefCell<Vec<docker::NetInfo>>,
    /// **卷页的操作条**（阶段 167：搜索框 ＋ 删除按钮 ＋ 提示）。
    volumes_bar: gtk::Box,
    /// **网络页的操作条**（阶段 167：搜索框 ＋ 删除按钮 ＋ 提示）。
    networks_bar: gtk::Box,
    /// 三个分段**各自的搜索词**。
    images_query: RefCell<String>,
    volumes_query: RefCell<String>,
    networks_query: RefCell<String>,
    /// ⚠️ **为什么留着**：操作做完会**重新列一遍镜像** ⇒ 刷新那条消息
    ///   （「共 45 个镜像」）会把操作结果**冲掉** ⇒ 用户看不到「打标签
    ///   成功」⇒ 结果得**刷新之后再贴回去**。
    images_op_msg: RefCell<String>,
    /// **卷表**（阶段 115）。
    volumes_store: gtk::TreeStore,
    volumes_view: gtk::TreeView,
    /// **网络表**（阶段 115）。
    networks_store: gtk::TreeStore,
    networks_view: gtk::TreeView,
    /// 卷页的「删除」按钮（阶段 166）。
    volumes_remove_btn: gtk::Button,
    /// 网络页的「删除」按钮（阶段 166）。
    networks_remove_btn: gtk::Button,
    /// 卷 / 网络页的**提示标签**（当前选中了谁；没选就提示怎么才能操作）。
    volumes_hint: gtk::Label,
    networks_hint: gtk::Label,
    /// **当前选中的卷 / 网络名**（空 = 没选）。
    ///
    /// ⚠️ **自己记一份**（跟 `images_selected` 一个套路）：GTK 的「选中」只是
    ///   一行 ⇒ 得把它翻译成 docker 认的**名字** ⇒ 后面的命令才用得上。
    volumes_selected: RefCell<String>,
    networks_selected: RefCell<String>,
    /// **删卷 / 删网络**的结果文字（**等列表刷新完再贴回状态行**）。
    ///
    /// ⚠️ **跟 `images_op_msg` 同一个坑**：操作做完会**重新列一遍** ⇒ 刷新
    ///   那条消息（「共 N 个卷」）会把操作结果**冲掉** ⇒ 用户看不到
    ///   「卷已删除」⇒ 结果得**刷新之后再贴回去**。
    volumes_op_msg: RefCell<String>,
    networks_op_msg: RefCell<String>,
    /// 三个分段**各自**的「正在取」标志（阶段 115）。
    ///
    /// ⚠️ **为什么不跟容器共用一个 `busy`**：容器正在刷新时，用户切到
    ///   「镜像」页 ⇒ 镜像那次取数会被 `busy` 挡掉 ⇒ 用户干等 ⇒
    ///   ⇒ 每个分段各管各的。
    images_busy: Cell<bool>,
    volumes_busy: Cell<bool>,
    networks_busy: Cell<bool>,
    /// **「待会儿再刷一次」**的标志（阶段 161）。
    ///
    /// ⚠️ **为什么需要**：刷新有「不叠加」的守卫（正在飞就直接返回）
    ///   ⇒ 但**写操作之后的那次刷新**（打标签 / 删除镜像 / 启停容器）
    ///   **正好撞上正在飞的那次** ⇒ 那次刷新被**丢掉** ⇒
    ///   状态行说「已删除」而**表里那行还在**（像程序坏了）；
    ///   镜像段更糟：**自动刷新默认关着** ⇒ 一直不更新（实测踩过）。
    /// ⇒ 撞上了就**记一笔** ⇒ 这次回来后**立刻再刷一次**。
    busy_pending: Cell<bool>,
    images_pending: Cell<bool>,
    volumes_pending: Cell<bool>,
    networks_pending: Cell<bool>,
    /// **「文件」页**（阶段 116）。
    files: FilesUi,
    /// 文件页**正在列的是哪个容器**（空 = 没选容器）。
    files_name: RefCell<String>,
    /// 文件页**当前目录**（远程 POSIX 路径）。
    ///
    /// ⚠️ **只认「成功列出来过」的目录**（阶段 159）：它是
    ///   - 双击目录时**拼路径的基准**（[`files_remote_path`]）
    ///   - 表格里**正在显示**的那个目录
    ///   - 上传时的落点
    /// ⇒ **列失败时绝不能推进它**（推进了 ⇒ 下一次双击在失败路径上再拼一层
    ///   ⇒ 地址栏越拼越长；用户 2026-11-24 报的正是这个）。
    files_path: RefCell<String>,
    /// 文件页**正在请求的目录**（远程 POSIX 路径）。
    ///
    /// ⚠️ **跟 [`Self::files_path`] 分开**（阶段 159）：
    /// - `files_path` = **已经成功列出来**的目录（界面显示的就是它）；
    /// - `files_req` = **这次正在请求**的目录（用来认「这个回包是不是我要的」）。
    /// ⇒ 请求失败时**只动 `files_req`**，`files_path` 原地不动 ⇒
    ///   路径栏 / 表格 / 拼接基准三者始终**指同一个目录**。
    files_req: RefCell<String>,
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
    ///
    /// ⚠️ **阶段 166：三个平表（镜像 / 卷 / 网络）的重建也用它** ——
    ///   重建要 `store.clear()` ⇒ GTK 会**逐行**发 `cursor-changed`
    ///   （每删掉一行发一次，最后一次是「没选中」）⇒ 光标处理器会把
    ///   「程序记着的选中」**清成空** ⇒ 自动刷新（每 7 秒）一来，用户的
    ///   选中就没了 ⇒ 删除按钮跟着变灰 ⇒ **用户点不到**。
    /// ⇒ 重建那一段算「程序性」⇒ 用这个标志挡掉那些假的光标变化。
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
    /// **日志下载**的结果（阶段 167：存成本地文件）。
    LogSave {
        /// 中文结果（成功路径 / 失败原因）。
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
    /// **跟随符号链接**：问出**目标**是目录还是文件了（用户 2026-11-27）。
    ///
    /// ⚠️ 带上**目标绝对路径**与**容器名** ⇒ 拿到结果就知道该干什么
    ///   （目标是目录 ⇒ 进去；是文件 ⇒ 看内容）。
    LinkKind {
        container: String,
        /// **链接目标的绝对路径**（不是链接自己）。
        path: String,
        /// **链接自己**（出错时要说清楚是哪个链接坏了）。
        link: String,
        /// Ok = 类型键（`"dir"` / `"file"` …）。
        r: Result<String, String>,
    },
    /// **镜像操作**的进度 / 结果（阶段 120）。
    ///
    /// ⚠️ **`done`**：进度消息**不刷新**列表（每 100ms 刷一次 ⇒ 远端被
    ///    hammering）；只有**最后那条**才刷新。
    ImageOp { msg: String, done: bool },
    /// **卷 / 网络写操作**的结果（阶段 166）。
    ///
    /// ⚠️ **为什么单开一个而不是复用 `ImageOp`**：镜像那条消息**固定**贴到
    ///   镜像分段的状态行 ⇒ 卷 / 网络得**自己带分段**（`seg`）⇒ 否则卷的
    ///   结果会贴到镜像那一行去。
    SegOp {
        /// 目标分段（`SEG_VOLUMES` / `SEG_NETWORKS`）。
        seg: &'static str,
        /// 中文结果文字。
        msg: String,
    },
}

/// 详情区的控件（阶段 110）。
struct DetailUi {
    /// 整块（**没选容器时不显示**）。
    root: gtk::Box,
    /// 标题（「详情：web」；**状态单独用药丸**，见下）。
    title: gtk::Label,
    /// 状态药丸（美化·地基 2/5）：胶囊底 + 状态档位色，
    /// 替代标题里的「（运行中）」——形状比括号里的字好扫。
    pill: gtk::Label,
    /// 四张卡片的正文（**键值表**：每张一个 `Grid`）。
    ///
    /// ⚠️ **为什么是 Grid 不是 Label**（UI 审查 D9）：一坨自动换行的文本
    ///   ⇒ 键与值对不齐、内容一多就互相裁。
    config: gtk::Grid,
    net: gtk::Grid,
    mount: gtk::Grid,
    env: gtk::Grid,
    /// **compose 项目详情**那块（阶段 111：选中项目行时显示，与容器详情互斥）。
    proj_root: gtk::Box,
    /// 项目标题（「compose 项目：postgres」）。
    proj_title: gtk::Label,
    /// 项目正文（配置文件 / 工作目录 / 容器清单）。
    proj_info: gtk::Label,
    // ⚠️ **阶段 155**：原来这里还有九个按钮（容器的启动 / 停止 / 重启 /
    //   进入容器 ＋ 项目的编辑 / 启动 / 停止 / 销毁 / 重建）⇒ 全部搬进
    //   **「操作」列**（每行一排图标按钮）⇒ 详情区**只管看**。
}

/// 「文件」页的控件（**阶段 116**）。
///
/// ⚠️ **容器里没有 SFTP** ⇒ 浏览 / 上传 / 下载全靠 `docker exec`
///   （命令与解析见 [`crate::backend::docker`]）。
struct FilesUi {
    /// **路径栏**（当前远程目录；回车 ⇒ 去这个目录）。
    path: gtk::Entry,
    /// 文件表（**平表**：图标 / 名字 / 大小 / 权限 ＋ 两个隐藏列）。
    store: gtk::TreeStore,
    /// 文件表。
    view: gtk::TreeView,
    /// **工具条上的按钮**（阶段 140：纯图标 ＋ tooltip；一行）。
    ///
    /// ⚠️ **留着是为了冒烟能断言**：用户 2026-11-19：「大按钮、框框占太多
    ///   空间」⇒ 得钉住「按钮**没有文字**、每个都有 tooltip」。
    btns: Vec<gtk::Button>,
    /// 状态行（**列表**的状态：几个条目 / 出错原因）。
    status: gtk::Label,
    /// **传输状态行**（上传 / 下载的进度与结果）。
    ///
    /// ⚠️ **为什么不跟列表状态挤在一行**：列表结果**随时**会到（刷新、切
    ///   目录）⇒ 挤在一行 ⇒ 传输刚报完「上传完成」就被「N 个条目」冲掉
    ///   ⇒ 用户（和冒烟）根本看不到传完了没有。
    xfer_status: gtk::Label,
}

/// 端口列：docker 给的是**逗号分隔的一长串** ⇒ 单行放不下就被省略号
/// 截掉（`0.0.0.0:3306->330…`）⇒ 改成**一个端口一行**。
///
/// ⚠️ **怎么做到多行**：把 `", "` 换成 `\n` ⇒ GTK 的 CellRendererText 用
///   `pango_layout_set_text` 渲染 ⇒ 文本里的换行符就是换行（**不是**
///   markup，所以换行符是安全的）。
fn ports_multiline(raw: &str) -> String {
    raw.replace(", ", "\n")
}

/// 端口串 ⇒ **给人看的样子**（阶段 157 / 162）。
///
/// ⚠️ **阶段 162：格式是「主机端口 : 容器端口」**（用户 2026-11-24：
///   「端口应该显示为 主机端口：容器端口，但现在只有一个端口，不知道是什么」）
///   ⇒ **两个端口都留着**（以前 `0.0.0.0:5432->5432/tcp` 被压成 `5432/tcp`
///   ⇒ 用户根本不知道那个 5432 是主机上的还是容器里的）。
///
/// ⚠️ **写法跟 `docker run -p` 一样**（好认）：
/// - `0.0.0.0:5432->5432/tcp` ⇒ `5432:5432/tcp`（通配地址丢掉，但**两个
///   端口都留着**）；
/// - `127.0.0.1:9001->9001/tcp` ⇒ `127.0.0.1:9001:9001/tcp`
///   （**只绑本机**是有用信息，不能丢 ⇒ 地址留着）；
/// - `9000->9001/tcp` ⇒ `9000:9001/tcp`（端口不同 ⇒ 映射关系要留）；
/// - `5432/tcp` ⇒ `5432/tcp`（**只 EXPOSE、没发布** ⇒ 没有主机端口）。
///
/// ⚠️ **完整端口串在详情弹窗里看得到**（这里只是给人扫一眼的摘要）。
fn ports_pretty(raw: &str) -> String {
    /// 通配地址（IPv4 全 0 / IPv6 全 0）⇒ 不占地方。
    fn is_wildcard(host: &str) -> bool {
        matches!(host, "0.0.0.0" | "::" | "[::]" | "*")
    }
    let mut out: Vec<String> = Vec::new();
    // ⚠️ **模型里存的是「一行一个」的串**（`ports_multiline` 把 `, ` 换成
    //   换行）⇒ 这里**逗号和换行都当分隔符**。
    for item in raw.split([',', '\n']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let pretty = match item.split_once("->") {
            Some((left, right)) => {
                // 左边 = `host:port` 或 `port`；右边 = `port/proto`。
                let (host, lport) = match left.rsplit_once(':') {
                    Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                        (Some(h), p)
                    }
                    _ => (None, left),
                };
                let (rport, proto) = match right.split_once('/') {
                    Some((p, pr)) => (p, Some(pr)),
                    None => (right, None),
                };
                let proto_txt = proto.map(|p| format!("/{p}")).unwrap_or_default();
                // ⚠️ **阶段 162：主机端口 : 容器端口**（两个都留着）。
                match host {
                    Some(h) if !h.is_empty() && !is_wildcard(h) => {
                        format!("{h}:{lport}:{rport}{proto_txt}")
                    }
                    _ => format!("{lport}:{rport}{proto_txt}"),
                }
            }
            None => item.to_string(),
        };
        if !out.contains(&pretty) {
            out.push(pretty);
        }
    }
    out.join("\n")
}

/* ---------------- 阶段 164：资源列（CPU / 内存） ---------------- */

/// **内存：只写「占了多少」**（用户 2026-11-24：「内存只写占了多少内存，
/// 不用百分比」）。
///
/// ⚠️ **docker stats 给的是「用了 / 上限」**（`52.84MiB / 63.00GiB`）⇒
///   这里**只取前面那段**（上限对「这个容器占了多少」没用 ⇒ 反而占地方）。
fn mem_used(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    s.split('/').next().unwrap_or(s).trim().to_string()
}

/// **CPU 百分比**（`"2.30%"` ⇒ `2.30`；认不出来就是 `0.0`）。
fn cpu_pct(raw: &str) -> f64 {
    let s = raw.trim().trim_end_matches('%').trim();
    s.parse::<f64>().unwrap_or(0.0)
}

/// **二进制单位**（跟 docker 的 `MiB` / `GiB` 同款 ⇒ 父子两行能直接比）。
///
/// ⚠️ **两位小数**（docker 的 `52.84MiB` / `63.00GiB` 就是两位）⇒ 同一列里
///   父行「1.00GiB」、子行「52.84MiB」不会看着像两个不同的量。
fn human_size_bin(n: u64) -> String {
    const KIB: f64 = 1024.0;
    let v = n as f64;
    if v < KIB {
        return format!("{n} B");
    }
    // ⚠️ **阈值与除数别错**（连踩两次：阈值错一档 ⇒ 10MiB 变成 0.01GiB；
    //   除数也错 ⇒ 10MiB 变成 10240.00MiB）⇒ 下面有专门的断言。
    if v < KIB * KIB * KIB {
        return format!("{:.2}MiB", v / (KIB * KIB));
    }
    if v < KIB * KIB * KIB * KIB {
        return format!("{:.2}GiB", v / (KIB * KIB * KIB));
    }
    format!("{:.2}TiB", v / (KIB * KIB * KIB * KIB))
}

/// **资源列的文案**（一行：`CPU 2.30% · 内存 52.84MiB`）。
///
/// ⚠️ **两个指标写在一列里**（用户 2026-11-24：「只用 1 列，cpu 写百分比，
///   内存只写占了多少内存」）⇒ 一眼能看，不用在两列之间来回扫。
/// ⚠️ **没采到就是「—」**（`docker stats` **只报运行中的容器** ⇒ 已停止的
///   容器没有这一行 ⇒ 不能显示成 0%）。
fn res_text(cpu: &str, mem: &str) -> String {
    let c = cpu.trim();
    let m = mem_used(mem);
    // ⚠️ **两个都没有 ⇒ 就一个「—」**（已停止的容器：`docker stats` 不报它）
    //   ⇒ 写成「CPU — · 内存 —」是废话。
    if c.is_empty() && m.is_empty() {
        return tr("—").to_string();
    }
    let c_txt = if c.is_empty() {
        tr("—").to_string()
    } else {
        // ⚠️ **阶段 165：小数位也要统一**（跟内存同一个道理）。
        //   docker 原样给 `1.0%`（一位小数）⇒ 而项目行是**合计** ⇒ 走
        //   `format!("{:.2}%")` ⇒ **两位小数** ⇒ 父子两行一个模子。
        format!("{:.2}%", cpu_pct(c))
    };
    // ⚠️ **阶段 165：内存的单位要跟「项目行的合计」一致**。
    //   docker 原样给的是 `67.9MiB`（**一位小数**）⇒ 而项目行是合计值
    //   ⇒ 用 `human_size_bin` 统一成**两位小数**（`67.90MiB`）
    //   ⇒ 同一列里父子两行**一个模子**（不然看着像两个不同的量）。
    //   ⚠️ 解析不了 ⇒ 保留原样（别把真信息弄丢）。
    let m_owned = match crate::backend::docker::parse_size_bytes(&m) {
        Some(b) => human_size_bin(b),
        None => m.clone(),
    };
    let m_txt = if m_owned.is_empty() { tr("—") } else { m_owned.as_str() };
    trf!("CPU {c} · 内存 {m}", c = c_txt, m = m_txt)
}
/// **项目行的资源**（CPU 百分比合计 ＋ 内存合计）。
///
/// ⚠️ **合计**（不是平均）⇒ 父行显示的是「这个项目一共占了多少」。
fn project_res_text(items: &[crate::backend::docker::ContainerInfo]) -> String {
    let cpu_sum: f64 = items.iter().map(|c| cpu_pct(&c.cpu)).sum();
    let mem_sum: u64 = items
        .iter()
        .filter_map(|c| crate::backend::docker::parse_size_bytes(&mem_used(&c.mem)))
        .sum();
    // ⚠️ **`trf!` 不支持格式说明**（它只做「把 `{名字}` 换成值」）⇒
    //   `{c:.2}` 这种写法**原样留在界面上**（踩过）⇒ 先自己格式化好再传进去。
    let cpu_txt = format!("{cpu_sum:.2}%");
    let mem_txt = human_size_bin(mem_sum);
    trf!("CPU {c} · 内存 {m}", c = cpu_txt, m = mem_txt)
}

/// **资源列排序用的 CPU 值**（从列里的文案取 ⇒ 排序按 CPU 百分比）。
fn res_cpu_value(text: &str) -> f64 {
    // 文案形如 `CPU 2.30% · 内存 52.84MiB` ⇒ 取 `CPU ` 后面那一段。
    let Some(rest) = text.strip_prefix("CPU ") else {
        return 0.0;
    };
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse::<f64>().unwrap_or(0.0)
}


/// 后端给的中文错误 ⇒ **分类**（决定空态的标题）。
///
/// ⚠️ **为什么要有**（UI 审查 D6）：最常见的三个「第一分钟错误」——
///   远端没装 docker、当前账号不在 docker 组、docker 守护进程没起——
///   在界面上**长得一模一样** ⇒ 用户只能猜 ⇒ 按分类给不同标题。
fn classify_docker_error(msg: &str) -> crate::emptystate::DockerErrorKind {
    use crate::emptystate::DockerErrorKind as K;
    // ⚠️ **顺序要紧**（阶段 135）：「守护进程没起」的那句话里**也带
    //   「权限」两个字**（「服务没起，或当前账号没权限」）⇒ 先认它 ⇒
    //   否则会把「守护进程没起」说成「没权限」⇒ **弹出来的对话框就是错的**。
    if msg.contains("守护进程") || msg.contains("服务没有运行") || msg.contains("连不上") {
        K::NotRunning
    } else if msg.contains("权限") {
        K::NoPermission
    } else if msg.contains("没有找到 docker") || msg.contains("没装 Docker") {
        K::NoDocker
    } else if msg.contains("太老") {
        K::TooOld
    } else {
        K::Other
    }
}

/// 远端账号**没有 Docker 权限** ⇒ **弹一次对话框**说清楚（每个连接只弹一次）。
///
/// ⚠️ **为什么是模态对话框**（用户 2026-11-19 报的）：以前这件事只写在
///   **底部一行小灰字**里 ⇒ 用户根本看不见（他截图里就是「一张空表 +
///   角落一行小字」）⇒ 现在直接弹窗，说明原因**和怎么办**。
///
/// ⚠️ **为什么只弹一次**：弹窗是**用户点的**入口触发的 ⇒ 连着点几次
///   就是骚扰 ⇒ 每个连接只弹一次。
///   ⚠️ **但页面上的大号错态一直都在** ⇒ **每次点「容器」看到的都一样**
///   （不会「这次弹、下次不弹」让用户诧异）。
///
/// ⚠️ **「重试」是用户明确要求的** ⇒ 允许再弹一次（告诉他「还是不行」）。
fn warn_no_permission(st: Option<&Rc<PanelState>>, ui: &Rc<DockerUi>, msg: &str) {
    if classify_docker_error(msg) != crate::emptystate::DockerErrorKind::NoPermission {
        return;
    }
    if ui.denied_notified.get() {
        return;
    }
    ui.denied_notified.set(true);
    let parent = st.and_then(|s| s.topwin());
    let st2 = st.cloned();
    let ui2 = ui.clone();
    crate::panel::dialogs::show_error_with_retry(
        parent.as_ref(),
        tr("没有访问 Docker 的权限"),
        &docker_denied_help(),
        tr("重试"),
        move || {
            ui2.denied_notified.set(false);
            let Some(st) = st2.clone() else { return };
            refresh_segment(&st, &ui2);
        },
    );
}

/// 「没有 Docker 权限」时给用户看的**怎么办**（对话框正文）。
///
/// ⚠️ **为什么单出一个公开函数**：这段话是**用户可见文案** ⇒ 得能
///   **单测**（断言它真说了「怎么办」）⇒ 别埋在闭包里。
///
/// ⚠️ **必须是「一行」的 `tr()`**：i18n 的守门扫描器**不折叠行续接**
///   （`\` + 换行 + 缩进）⇒ 折行排版会让查表 key 对不上 ⇒ 守门误报
///   「没译文」（见 `src/i18n.rs` 的 `unescape` 注释）。
///
/// ⚠️ **不许用 Markdown 的 `**`**：GTK 不渲染 Markdown ⇒ 用户会看到
///   **字面的星号**（这个坑踩过，见 `panel/fwdmgr.rs`）。
pub fn docker_denied_help() -> String {
    tr("当前账号没有访问 Docker 的权限。\n这台服务器的 docker 守护进程只让 docker 组的账号访问。要用容器功能，得在服务器上把当前账号加进 docker 组（改完要重新登录才生效）：\n\nsudo usermod -aG docker 你的用户名\n\n或者改用有权限的账号登录。\nPuTTYPlus 不会自动 sudo（那会要密码、也会留痕迹）——这里只把原因说清楚，怎么改由你决定。").to_string()
}

/// 一张卡片的**键值对**（UI 审查 D9）。
///
/// ⚠️ **为什么不是一坨文本**：概览页以前是「标题 Label + 一个可换行的纯
///   文本 Label」⇒ 一行里挤着 `镜像：xxx 状态：xxx …` ⇒ **键与值对不
///   齐**，内容一多就**互相裁**（截图里「运行用户：root」被下面那行压掉
///   一半）⇒ 现在是**真的键值表**（每行：键 | 值）。
type Kv = Vec<(String, String)>;

/// 键值表 ⇒ **纯文本**（测试钩子与空态用）。
fn kv_text(kv: &Kv) -> String {
    kv.iter()
        .map(|(k, v)| {
            if k.is_empty() {
                v.clone()
            } else {
                trf!("{k}：{v}", k = k, v = v)
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// 从键值表里**读回**纯文本（测试钩子用：界面是 Grid ⇒ 没有 `text` 属性）。
fn grid_kv_text(grid: &gtk::Grid) -> String {
    let mut cells: Vec<String> = Vec::new();
    let mut child = grid.first_child();
    while let Some(c) = child {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            cells.push(l.text().to_string());
        }
        child = c.next_sibling();
    }
    // 每两格一组：键 | 值。
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < cells.len() {
        let k = cells[i].clone();
        let v = cells.get(i + 1).cloned().unwrap_or_default();
        if k.is_empty() {
            lines.push(v);
        } else {
            lines.push(trf!("{k}：{v}", k = k, v = v));
        }
        i += 2;
    }
    lines.join("\n")
}

/// 建一张**键值表**卡片（标题 ＋ 「键 | 值」的表）。
///
/// 美化·地基 3/5：卡片挂 `.pp-card`（圆角 + 边框 + 卡片面色）⇒
/// 概览页从「四坨文字」变成真正意义上的**四张卡**。
fn mk_kv_card(title: &str) -> (gtk::Box, gtk::Grid) {
    let head = gtk::Label::new(Some(title));
    head.set_xalign(0.0);
    head.add_css_class("heading");
    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(4);
    grid.set_hexpand(true);
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_child(Some(&grid));
    sw.set_vexpand(true);
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.set_hexpand(true);
    b.set_vexpand(true);
    b.add_css_class("pp-card");
    b.append(&head);
    b.append(&sw);
    (b, grid)
}

/// 往键值表里**填内容**（先清空再填 ⇒ 不会越填越多）。
fn fill_kv(grid: &gtk::Grid, kv: &Kv) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    for (i, (k, v)) in kv.iter().enumerate() {
        let kl = gtk::Label::new(Some(k));
        kl.set_xalign(0.0);
        kl.add_css_class("dim-label");
        kl.set_valign(gtk::Align::Start);
        let vl = gtk::Label::new(Some(v));
        vl.set_xalign(0.0);
        vl.set_yalign(0.0);
        vl.set_wrap(true);
        vl.set_selectable(true);
        vl.set_hexpand(true);
        grid.attach(&kl, 0, i as i32, 1, 1);
        grid.attach(&vl, 1, i as i32, 1, 1);
    }
}


/// docker 状态原文 ⇒ **药丸档位**（**纯函数**，便于单测）。
///
/// - 运行中 ⇒ 绿；已暂停 / 重启中 ⇒ 黄；已失效 ⇒ 红；
/// - 已停止 / 已创建 ⇒ **灰**（不是错误，只是没在跑）；
/// - 其它（compose 父行的「1/3 运行中」摘要、空串）⇒ 灰（不假装有状态）。
///
/// ⚠️ `state_markup` 与状态药丸渲染器**共用这一个映射**——两处各写一份
///    迟早漂移（点的颜色跟药丸的底色对不上）。
fn pill_state_of(state: &str) -> crate::panel::pillcell::PillState {
    use crate::panel::pillcell::PillState;
    match state {
        "running" => PillState::Ok,
        "paused" | "restarting" => PillState::Warn,
        "dead" => PillState::Err,
        _ => PillState::Dim,
    }
}

/// 药丸要显示的**文字**（**纯函数**，便于单测）。
///
/// docker 的六种状态原文翻成中文（[`docker::state_label`]）；**翻不出的**
/// （compose 父行的「1/3 运行中」摘要）**原样显示**——那本身就是中文，
/// 比「状态未知」有信息量得多。
fn pill_text_of(state: &str) -> String {
    const RAW: [&str; 6] = ["running", "exited", "created", "paused", "restarting", "dead"];
    if RAW.contains(&state) {
        docker::state_label(state).to_string()
    } else {
        state.to_string()
    }
}

/// 状态列的**带色点 markup**（UI 审查 D8）。
///
/// ⚠️ **颜色走 `theme::state_hex`**（项目规矩：语义色不许写死 RGB）。
/// - 运行中 ⇒ 绿点；
/// - 已暂停 / 重启中 ⇒ 黄点；
/// - 已失效 ⇒ 红点；
/// - 已停止 / 已创建 ⇒ **灰点**（不是错误，只是没在跑）。
fn state_markup(state: &str) -> String {
    use crate::theme::{state_hex, State};
    let color: String = match pill_state_of(state) {
        crate::panel::pillcell::PillState::Ok => state_hex(State::Ok).to_string(),
        crate::panel::pillcell::PillState::Warn => state_hex(State::Warn).to_string(),
        crate::panel::pillcell::PillState::Err => state_hex(State::Err).to_string(),
        crate::panel::pillcell::PillState::Dim => crate::theme::dim_hex(),
    };
    let label = docker::state_label(state);
    format!("<span foreground=\"{c}\">●</span> {t}", c = color, t = label)
}

/// 图例里的**色块**（**跟着主题走** ⇒ 切主题不用重建图例）。

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
    /// **「折行」开关**（UI 审查 D10；默认关 ⇒ 用横向滚动看全文）。
    wrap: gtk::ToggleButton,
    /// **「下载…」按钮**（阶段 167：把日志存成本地文件）。
    save: gtk::Button,
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
        // ⚠️ **展开状态等**整表建完**再恢复**：开了排序以后，追加一行会
        //   **重排** ⇒ 中途算出来的行号会变 ⇒ 展开会落到别的行上。
        let mut to_expand: Vec<String> = Vec::new();
        for node in Self::group(rows) {
            match node {
                Node::Solo(c) => {
                    let it = self.model.append(None);
                    self.set_container_row(&it, &c);
                }
                Node::Project { name, items } => {
                    // 父行：项目名 + 摘要。
                    let it = self.model.append(None);
                    self.set_project_row(&it, &name, &items);
                    for c in items {
                        let sub = self.model.append(Some(&it));
                        self.set_container_row(&sub, &c);
                    }
                    // 之前是展开的 ⇒ 等会儿展开回来。
                    if expanded.contains(&name) {
                        to_expand.push(name);
                    }
                }
            }
        }
        // ⚠️ **整表建完了再展开**（见上面的说明）。
        for name in to_expand {
            if let Some(it) = self.project_iter(&name) {
                let path = self.model.path(&it);
                self.view.expand_row(&path, false);
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
        // ⚠️ **阶段 158：状态列的文案带上了时长**（「运行 2 小时」）。
        let state = state_pill_text(&c.state, c.state_label(), c.duration);
        let mk = state_markup(&c.state);
        // ⚠️ **端口一个一行**（用户 2026-11-23）：docker 的端口串是逗号
        //   分隔的长串 ⇒ 单行会被省略号截掉 ⇒ 换成换行符。
        let ports = ports_multiline(&c.ports);
        // ⚠️ **阶段 164：资源列**（CPU 百分比 ＋ 内存占用；一列两样）。
        let res = res_text(&c.cpu, &c.mem);
        self.model.set(
            it,
            &[
                (COL_NAME as u32, &c.name),
                (COL_ID as u32, &c.id),
                (COL_IMAGE as u32, &c.image),
                (COL_PORTS as u32, &ports),
                (COL_STATE as u32, &state),
                (COL_KIND as u32, &KIND_CONTAINER),
                // ⚠️ **阶段 162：容器行**没有特别标记 ⇒ 空（画纯名字）。
                (COL_NAME_MK as u32, &String::new()),
                // ⚠️ **原始状态**（阶段 155）：「操作」列里「启动 / 停止」哪个
                //   能点 ⇒ 得知道 docker 说的原始状态（界面上的中文标签在
                //   英文界面下会变 ⇒ 不能拿它判断）。
                (COL_RAW_STATE as u32, &c.state),
                (COL_STATE_MK as u32, &mk),
                (COL_RES as u32, &res),
            ],
        );
    }

    /// 写一个项目行（父行）。
    ///
    /// ⚠️ **父行不是一行空格**（UI 审查 D16）：以前 ID / 镜像 / 端口 /
    ///   CPU / 内存 全空 ⇒ 一行里 5 个空格 + 2 个词 ⇒ 扫起来很跳 ⇒
    ///   现在给一份**摘要**（镜像个数 / 几个在跑）。
    /// ⚠️ **阶段 155**：CPU / 内存列退役 ⇒ 父行也不再有合计。
    fn set_project_row(&self, it: &gtk::TreeIter, name: &str, items: &[ContainerInfo]) {
        let n = items.len();
        let running = items.iter().filter(|c| c.state == "running").count();
        // 镜像：去重后列出（最多两个，多了写「N 个镜像」）。
        let mut imgs: Vec<String> = Vec::new();
        for c in items {
            if !c.image.is_empty() && !imgs.contains(&c.image) {
                imgs.push(c.image.clone());
            }
        }
        let img_text = if imgs.is_empty() {
            String::new()
        } else if imgs.len() == 1 {
            imgs[0].clone()
        } else {
            trf!("{n} 个镜像", n = imgs.len())
        };
        let cnt = trf!("{r}/{n} 运行中", r = running, n = n);
        // ⚠️ **阶段 164：项目行的资源**（CPU 合计 ＋ 内存合计）。
        let res = project_res_text(items);
        // ⚠️ **父行没有自己的状态** ⇒ 色点用「灰」（不假装它在跑）。
        let mk = format!(
            "<span foreground=\"{c}\">●</span> {t}",
            c = crate::theme::dim_hex(),
            t = trf!("{n} 个容器", n = n)
        );
        self.model.set(
            it,
            &[
                (COL_NAME as u32, &name.to_string()),
                // ⚠️ **阶段 162：项目行**要**一眼就跟容器行分得开**
                //   ⇒ 名字**加粗** ＋ 后面跟一个**灰色小字「compose」**
                //   （这是 docker 的产品名 ⇒ 中英文界面都这么写）。
                //   ⚠️ 名字本身要**转义**（markup 里 `&` `<` 会出事）。
                (
                    COL_NAME_MK as u32,
                    &project_name_markup(name),
                ),
                (COL_ID as u32, &String::new()),
                (COL_IMAGE as u32, &img_text),
                (COL_PORTS as u32, &String::new()),
                (COL_STATE as u32, &cnt),
                (COL_KIND as u32, &KIND_PROJECT),
                // ⚠️ **原始状态**（阶段 155）：项目行没有自己的状态 ⇒ 用
                //   「容器们跑得怎么样」⇒ 三种：`running`（全在跑）/
                //   `exited`（全停了）/ `mixed`（一半在跑）。
                //   ⇒ 「启动 / 停止」哪个能点就看得出来了。
                (
                    COL_RAW_STATE as u32,
                    &match (running, n) {
                        (0, _) => "exited".to_string(),
                        (r, t) if r == t => "running".to_string(),
                        _ => "mixed".to_string(),
                    },
                ),
                (COL_STATE_MK as u32, &mk),
                (COL_RES as u32, &res),
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
        // ⚠️ **数字要对**（UI 审查 D7a）：以前写的是**过滤前**的总数 ⇒
        //   点「运行中」只剩 1 行，底栏还写着「共 2 个容器」⇒ 现在写成
        //   「显示 N / 共 M」。
        let total = info.containers.len();
        let msg = if shown.len() == total {
            trf!(
                "共 {n} 个容器（远程 Docker {v}）",
                n = total,
                v = info.version
            )
        } else {
            trf!(
                "显示 {s} / 共 {n} 个容器（远程 Docker {v}）",
                s = shown.len(),
                n = total,
                v = info.version
            )
        };
        self.set_segment_status(SEG_CONTAINERS, &msg);
        // ⚠️ **空态**：一个容器都没有 ⇒ 给「这台服务器上没有容器」；
        //   有东西 ⇒ 藏起来。
        self.sync_empty_state(shown.is_empty());
        // ⚠️ **放最后**：大转圈要不要消失，取决于「列表空不空」⇒ 得等
        //   列表填好了再判断（见 `set_loading` 的说明）。
        self.set_loading(false);
    }

    /// **空态**：列表空了 ⇒ 显示「这台服务器上没有容器」；不空 ⇒ 藏起来。
    ///
    /// ⚠️ **为什么单独一个函数**：填表有三条路（刷新 / 过滤 / 测试灌数据）
    ///   ⇒ 空态得**三处都对** ⇒ 别复制三份。
    fn sync_empty_state(&self, empty: bool) {
        if empty {
            let st = crate::emptystate::docker_empty();
            self.empty.set_texts(&st.title, &st.desc);
            self.empty.root.set_visible(true);
        } else {
            self.empty.root.set_visible(false);
        }
    }

    /// 失败：清空表格 + 说清楚为什么（**中文 + 怎么办**）。
    fn fail(&self, msg: &str) {
        self.model.clear();
        self.containers.borrow_mut().clear();
        self.set_loading(false);
        self.set_segment_status(SEG_CONTAINERS, msg);
        // ⚠️ **错态**：不是「一张空表 + 底部一行小灰字」，而是
        //   **标题 + 解释 + 重试**（UI 审查 D6）⇒ 三种最常见的错误
        //   （没装 docker / 没权限 / 守护进程没起）**长得不一样**。
        let kind = classify_docker_error(msg);
        let st = crate::emptystate::docker_error(kind, msg);
        self.empty.set_texts(&st.title, &st.desc);
        self.empty.root.set_visible(true);
    }

    /// **某个分段取数失败** ⇒ **统一**的处理（阶段 135）。
    ///
    /// ⚠️ **为什么抽成一个函数**：真路径（消息循环）与**测试钩子**都得
    ///   走它 ⇒ 走**同一条路** ⇒ 断言才抓得住回归。
    ///   （之前钩子自己实现了一遍 ⇒ 把真路径删了测试照样绿 ⇒ 等于没测。）
    ///
    /// 三件事：状态行写一句 ＋ **大号错态** ＋ 必要时**弹一次对话框**。
    fn seg_failed(self: &Rc<Self>, st: Option<&Rc<PanelState>>, seg: &'static str, msg: &str) {
        self.set_segment_status(seg, msg);
        self.show_seg_error(seg, msg);
        warn_no_permission(st, self, msg);
    }

    /// **容器页取数失败** ⇒ 清表 + 大号错态 + 必要时弹框（同一条路）。
    fn containers_failed(self: &Rc<Self>, st: Option<&Rc<PanelState>>, msg: &str) {
        self.fail(msg);
        warn_no_permission(st, self, msg);
    }

    /// 某个分段的**错态**（阶段 135）：显示大号空态（标题按错误分类）。
    ///
    /// ⚠️ **为什么镜像/卷/网络也要有**（用户 2026-11-19 报的）：这三页
    ///   以前失败时**只写底部一行小灰字** ⇒ 用户看到的是「一张空表」，
    ///   根本分不清「没权限」与「真的没有镜像」⇒ 跟容器页一样给大号说明。
    fn show_seg_error(&self, seg: &'static str, msg: &str) {
        let empty = self.seg_empty(seg);
        let kind = classify_docker_error(msg);
        let st = crate::emptystate::docker_error(kind, msg);
        empty.set_texts(&st.title, &st.desc);
        empty.root.set_visible(true);
    }

    /// 某个分段的错态**收起来**（取到数据了）。
    fn hide_seg_error(&self, seg: &'static str) {
        self.seg_empty(seg).root.set_visible(false);
    }

    /// 某个分段的**错态空态**（容器页是 `empty`，其余三页各有各的）。
    fn seg_empty(&self, seg: &str) -> &crate::emptystate::DynamicEmptyState {
        match seg {
            SEG_IMAGES => &self.images_empty,
            SEG_VOLUMES => &self.volumes_empty,
            SEG_NETWORKS => &self.networks_empty,
            _ => &self.empty,
        }
    }

    /// 设置**某个分段**的状态文字（UI 审查 D7）。
    ///
    /// ⚠️ **底部那行只显示当前分段的** ⇒ 切到「镜像」页就不会还挂着
    ///   「正在读取远程容器…」（上一段的状态污染下一段）。
    fn set_segment_status(&self, seg: &'static str, text: &str) {
        self.status_text
            .borrow_mut()
            .insert(seg, text.to_string());
        if self.seg_current.get() == seg {
            self.status.set_text(text);
        }
    }

    /// **切换分段** ⇒ 底部那行跟着换成新分段的状态。
    fn sync_status_line(&self) {
        let seg = self.seg_current.get();
        let text = self
            .status_text
            .borrow()
            .get(seg)
            .cloned()
            .unwrap_or_default();
        self.status.set_text(&text);
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
        // ⚠️ **正在加载时藏掉空态**（UI 审查 D6）：大转圈与空态**同时**
        //   出现会打架（一个说「正在加载」、一个说「没有容器」）。
        if loading {
            self.empty.root.set_visible(false);
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
        // ⚠️ **数字要对**（UI 审查 D7a）：过滤后写「显示 N / 共 M」。
        let total = rows.len();
        let msg = if shown.len() == total {
            trf!("共 {n} 个容器", n = total)
        } else {
            trf!("显示 {s} / 共 {n} 个容器", s = shown.len(), n = total)
        };
        self.set_segment_status(SEG_CONTAINERS, &msg);
        // ⚠️ **过滤后空了** ⇒ 也给空态（不然又是一块空白）。
        self.sync_empty_state(shown.is_empty());
    }

    /// **关掉行的右键菜单**（点完菜单项之后）。
    fn close_row_menu(&self) {
        if let Some(p) = self.row_menu.borrow().as_ref() {
            p.popdown();
        }
    }

    /// 当前选中容器的 **ID**（复制 ID 那个菜单项用）。
    fn selected_container_id(&self) -> String {
        let (path, _col) = gtk::prelude::TreeViewExt::cursor(&self.view);
        let Some(path) = path else {
            return String::new();
        };
        let Some(it) = self.model.iter(&path) else {
            return String::new();
        };
        // 项目行（有子行）不是容器 ⇒ 没有自己的 ID。
        if self.model.iter_children(Some(&it)).is_some() {
            return String::new();
        }
        self.model.get(&it, COL_ID as i32)
    }

    /// 表格**模型**里的全部行（**冒烟测试读它**）。
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

    /// 找到某一行（**容器名 / 项目名**）的迭代器。
    fn find_iter(&self, name: &str) -> Option<gtk::TreeIter> {
        let path = self.find_path(name)?;
        self.model.iter(&path)
    }

    /// 详情取回来了 ⇒ 填四张卡片。
    fn apply_detail(&self, d: &ContainerDetail) {
        // ⚠️ **顺手记下选中的容器**（阶段 112 修的 bug）：消息循环拿它比对
        //    「这条结果是不是现在选的那个」⇒ 它必须**一直是对的**。
        *self.selected.borrow_mut() = d.name.clone();
        self.detail.title.set_text(&trf!("详情：{n}", n = d.name));
        // 状态药丸：文字 + 档位类（同一个 label 反复用 ⇒ 先摘干净再挂）
        let pill = &self.detail.pill;
        pill.set_text(d.state_label());
        pill.set_visible(true);
        for c in ["pp-pill-ok", "pp-pill-warn", "pp-pill-err", "pp-pill-dim"] {
            pill.remove_css_class(c);
        }
        pill.add_css_class(
            match pill_state_of(&d.status) {
                crate::panel::pillcell::PillState::Ok => "pp-pill-ok",
                crate::panel::pillcell::PillState::Warn => "pp-pill-warn",
                crate::panel::pillcell::PillState::Err => "pp-pill-err",
                crate::panel::pillcell::PillState::Dim => "pp-pill-dim",
            },
        );
        fill_kv(&self.detail.config, &detail_config_kv(d));
        fill_kv(&self.detail.net, &detail_net_kv(d));
        fill_kv(&self.detail.mount, &detail_mount_kv(d));
        fill_kv(&self.detail.env, &detail_env_kv(d));
        // ⚠️ **阶段 155**：原来这里会按状态设按钮的可用性（运行中不能启动…）
        //   ⇒ 现在按钮在**「操作」列**里 ⇒ 可用性由那一列自己算
        //   （`row_actions` 看 `COL_RAW_STATE`）⇒ 这里只管**看**。
        self.detail.root.set_visible(true);
    }

    /// 详情取失败 ⇒ 说清楚为什么（**别静默**）。
    fn detail_error(&self, msg: &str) {
        self.detail.title.set_text(tr("取不到容器详情"));
        // 取不到详情 ⇒ 没有状态可说 ⇒ 药丸收起来（别挂着上一个容器的状态）
        self.detail.pill.set_visible(false);
        // ⚠️ **错态也放进键值表**（第一行是原因，其余留空）。
        fill_kv(&self.detail.config, &vec![(String::new(), msg.to_string())]);
        fill_kv(&self.detail.net, &Kv::new());
        fill_kv(&self.detail.mount, &Kv::new());
        fill_kv(&self.detail.env, &Kv::new());
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
        // ⚠️ **阶段 155**：原来这里会把「编辑 compose 文件」按钮按有没有
        //   配置文件设成灰的 ⇒ 现在那个入口在**「操作」列**里 ⇒ 点它的时候
        //   再判断（见 `dispatch_row_action` 的 `ActionKind::Edit`）。
        self.detail.proj_root.set_visible(true);
        self.detail.root.set_visible(false);
    }

    /* ---------------- 阶段 112：日志 ---------------- */

    /// **日志窗是不是开着**（用户在看日志）。
    ///
    /// ⚠️ **阶段 165**：日志是**独立窗口**（不再是页签）⇒ 判据变成
    ///   「那扇窗可见吗」。
    fn log_page_visible(&self) -> bool {
        self.logs_window
            .borrow()
            .as_ref()
            .map(|w| w.is_visible())
            .unwrap_or(false)
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
/// 「运行配置」卡片（**键值对**）。
fn detail_config_kv(d: &ContainerDetail) -> Kv {
    let mut v: Kv = Vec::new();
    v.push((tr("镜像").to_string(), d.image.clone()));
    let mut st = d.state_label().to_string();
    if !d.health.is_empty() && d.health != "none" {
        st = trf!("{s}（{h}）", s = st, h = d.health);
    }
    v.push((tr("状态").to_string(), st));
    v.push((tr("创建时间").to_string(), fmt_time(&d.created)));
    v.push((tr("启动时间").to_string(), fmt_time(&d.started_at)));
    v.push((tr("退出时间").to_string(), fmt_time(&d.finished_at)));
    if !d.exit_code.is_empty() {
        v.push((tr("退出码").to_string(), d.exit_code.clone()));
    }
    v.push((tr("重启策略").to_string(), d.restart_policy.clone()));
    if !d.restart_count.is_empty() {
        v.push((tr("重启次数").to_string(), d.restart_count.clone()));
    }
    v.push((
        tr("运行用户").to_string(),
        if d.user.is_empty() {
            tr("root").to_string()
        } else {
            d.user.clone()
        },
    ));
    v.push((tr("工作目录").to_string(), d.workdir.clone()));
    if !d.command.is_empty() {
        v.push((tr("命令").to_string(), d.command.clone()));
    }
    v
}

/// 「网络与端口」卡片（**键值对**）。
fn detail_net_kv(d: &ContainerDetail) -> Kv {
    let mut v: Kv = Vec::new();
    v.push((tr("网络模式").to_string(), d.network_mode.clone()));
    for n in d.networks.iter() {
        v.push((
            tr("网络").to_string(),
            trf!(
                "{name}（{ip}，网关 {gw}）",
                name = n.name,
                ip = n.ip,
                gw = n.gateway
            ),
        ));
    }
    for p in d.ports.iter() {
        if p.host_port.is_empty() {
            v.push((
                tr("端口").to_string(),
                trf!("{c}（未发布）", c = p.container),
            ));
        } else {
            v.push((
                tr("端口").to_string(),
                trf!(
                    "{c} → {ip}:{hp}",
                    c = p.container,
                    ip = p.host_ip,
                    hp = p.host_port
                ),
            ));
        }
    }
    if v.len() == 1 {
        v.push((String::new(), tr("（没有网络信息）").to_string()));
    }
    v
}

/// 「挂载」卡片（**键值对**）。
fn detail_mount_kv(d: &ContainerDetail) -> Kv {
    if d.mounts.is_empty() {
        return vec![(String::new(), tr("（没有挂载）").to_string())];
    }
    let mut v: Kv = Vec::new();
    for m in d.mounts.iter() {
        v.push((
            m.kind.clone(),
            trf!(
                "{src} → {dst}（{rw}）",
                src = m.source,
                dst = m.dest,
                rw = if m.rw == "true" {
                    tr("可写")
                } else {
                    tr("只读")
                }
            ),
        ));
    }
    v
}

/// 「环境变量」卡片（**键值对**：`KEY` | `VALUE`）。
fn detail_env_kv(d: &ContainerDetail) -> Kv {
    if d.env.is_empty() {
        return vec![(String::new(), tr("（没有环境变量）").to_string())];
    }
    d.env
        .iter()
        .map(|e| match e.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (e.clone(), String::new()),
        })
        .collect()
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

/// 一段时长 ⇒ **给人看的样子**（"2 小时" / "不到 1 秒"）。
///
/// ⚠️ **单位走 `tr()`**（界面有中英两版 ⇒ 单位也得翻）。
/// ⚠️ **数字与单位之间留一个空格**（中文排版的习惯，也跟英文对齐）。
fn duration_text(d: docker::DockerDuration) -> String {
    use docker::{DockerDuration, DurUnit};
    match d {
        DockerDuration::LessThanSecond => tr("不到 1 秒").to_string(),
        DockerDuration::Num(n, u) => {
            let unit = match u {
                DurUnit::Second => tr("秒"),
                DurUnit::Minute => tr("分钟"),
                DurUnit::Hour => tr("小时"),
                DurUnit::Day => tr("天"),
                DurUnit::Week => tr("周"),
                DurUnit::Month => tr("个月"),
                DurUnit::Year => tr("年"),
            };
            format!("{n} {unit}")
        }
    }
}

/// 状态列的**药丸文字**（阶段 158）。
///
/// ⚠️ **用户 2026-11-24 提的**：「状态列要显示已经运行了多久，如果只是已经
///   创建，就显示已经创建了多久，如果是正在运行，就显示已经运行了多久，
///   **尽量简洁**」。
/// ⇒ 在跑 ⇒ 「运行 2 小时」；只创建 ⇒ 「创建 3 分钟」；已停止 ⇒「停止 9 天」；
///   **拿不到时长就退回原来的状态标签**（"运行中" / "已创建" / "已停止"）。
///
/// ⚠️ **药丸底色不变**（绿 / 黄 / 灰 / 红）⇒ 状态一眼能分，时长是补充。
fn state_pill_text(state: &str, label: &str, duration: Option<docker::DockerDuration>) -> String {
    let Some(d) = duration else {
        return label.to_string();
    };
    let dur = duration_text(d);
    match state {
        "running" => trf!("运行 {d}", d = dur),
        "created" => trf!("创建 {d}", d = dur),
        "exited" => trf!("停止 {d}", d = dur),
        _ => label.to_string(),
    }
}

/// 表格**实际可用**的宽度（响应式分档的判据）。
///
/// ⚠️ **优先用视口宽度**（`hadjustment.page-size`）：它就是**用户能看到的
///   那部分**；`view.width()` 在表格被最小宽度撑住时会偏大。
///   视口宽度拿不到时（比如还没布局）才退回 `view.width()`。
fn available_width(ui: &DockerUi) -> i32 {
    let page = ui.viewport_adj.page_size();
    if page > 0.0 {
        page.round() as i32
    } else {
        ui.view.width()
    }
}

/// 按 TreeView 的**实际分配宽度**重排列；拖动列宽或菜单开着时先不排。
fn schedule_column_layout(ui: &Rc<DockerUi>) {
    if ui.column_layout_pending.replace(true) {
        return;
    }
    let weak = Rc::downgrade(ui);
    glib::idle_add_local_once(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.column_layout_pending.set(false);
        let menu_open = ui
            .row_menu
            .borrow()
            .as_ref()
            .is_some_and(|menu| menu.is_visible());
        if ui.column_resize_active.get() || menu_open {
            return;
        }
        let density = ui.column_density.get().for_resize(available_width(&ui));
        apply_column_density(&ui, density);
    });
}

/// 建界面（**只建一次**；挂在 [`PanelState::docker_ui`] 上）。
fn build(st: &Rc<PanelState>) -> Rc<DockerUi> {
    // ---- 工具条 ----
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(tr("搜索容器")));
    // ⚠️ **阶段 157：搜索框不扩展，但给一个像样的宽度**（18~28 字符）。
    //   GTK4 里 `max_width_chars` **限不住分配宽度**（Box 会照旧把富余
    //   塞给它）⇒ 真要限宽，得让它**不扩展**，再由旁边的弹性空隙吸收。
    search.set_hexpand(false);
    search.set_width_chars(18);
    search.set_max_width_chars(28);

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

    let spin = gtk::Spinner::new();
    spin.set_size_request(16, 16);
    spin.set_valign(gtk::Align::Center);
    spin.set_visible(false);

    // ⚠️ **阶段 155**：工具条上原来那个「进入容器」按钮**撤了** ⇒ 它搬进
    //   **「操作」列**（每行一颗终端图标）⇒ 不用先选中容器就能直接进那一个。
    //   （以前「选中容器 + 工具条按钮」是两步 ⇒ 现在一步。）

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    // ⚠️ **工具条拆成两组**（UI 审查 D13）：以前一行塞了 11 个控件
    //   ⇒ 搜索框 `hexpand` 把右边的全推走 ⇒ 窗口一窄就溢出（Box 不换行）。
    //   ⇒ 左组「筛选」可伸缩、右组「操作」保持自然宽度 ⇒ 窄窗口时
    //   被压扁的是**搜索框**，操作按钮永远看得见。
    let filter_group = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // ⚠️ **阶段 157：这一组**不**吸富余宽度**。
    //   GTK4 的 Box 会把富余宽度**全给扩展子控件** ⇒ 以前搜索框一个人
    //   吃掉了整条工具条的富余 ⇒ 「全部 / 运行中 / 已停止」被顶到最右边，
    //   跟「自动刷新」挤成一团 ⇒ 整条栏看着是歪的。
    //   ⇒ 富余改由**后面的弹性空隙**吸收（见下面 `spacer`）。
    filter_group.set_hexpand(false);
    filter_group.append(&search);
    // 筛选组收进**分段控件**（美化·二批 7）：三颗互斥 ToggleButton 本来就
    //   是"分段"的语义 ⇒ 套上 `.pp-segmented`（圆角胶囊容器 + 段内 dim、
    //   选中 accent_soft）⇒ 跟表设计器、顶层分段条一个样式，不再是三颗
    //   散着的默认按钮。
    let f_seg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    f_seg.add_css_class("pp-segmented");
    f_seg.set_valign(gtk::Align::Center);
    f_seg.append(&f_all);
    f_seg.append(&f_run);
    f_seg.append(&f_stop);
    filter_group.append(&f_seg);

    let action_group = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    action_group.append(&auto_lbl);
    action_group.append(&auto);
    // ⚠️ loading 动画的小转圈（用户 2026-11-16）。
    action_group.append(&spin);

    bar.append(&filter_group);
    // ⚠️ **阶段 157：弹性空隙** ⇒ 左边（搜索 + 筛选）聚在一起，
    //   右边（自动刷新）靠右 ⇒ 工具条的两头各归各位。
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    // ⚠️ **分组之间画一根竖线**（一眼看得出哪里是筛选、哪里是操作）。
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.set_valign(gtk::Align::Center);
    sep.set_margin_start(2);
    sep.set_margin_end(2);
    bar.append(&sep);
    bar.append(&action_group);
    // ⚠️ **不再放第二个刷新按钮**（UI 审查 D13）：分段栏右边那个 ↻ 刷的
    //   就是当前分段（在容器段里跟这里那个是同一件事）⇒ 两个刷新按钮
    //   干同一件事，用户不知道该点哪个 ⇒ 只留分段栏那个。

    // ---- 表格（**树形**）----
    let types: Vec<glib::Type> = (0..N_COLS).map(|_| glib::Type::STRING).collect();
    let model = gtk::TreeStore::new(&types);
    let view = gtk::TreeView::new();
    view.set_model(Some(&model));
    // ⚠️ **阶段 157：必须显式开 `has-tooltip`**！
    //
    // GTK4 的树视图**默认不查 tooltip** ⇒ 不开的话 `query-tooltip`
    // **永远不会来** ⇒ 「操作」列那套自绘 tooltip **一次都没显示过**
    // （用户 2026-11-23：「按钮增加 tooltip」）。
    view.set_has_tooltip(true);
    // ⚠️ **阶段 157：挂 `.view` 类** ⇒ 主题里那整套「表格节奏」规则
    //   （`treeview.view cell { padding: 4px 7px }` ⇒ 行高有呼吸感、
    //   `treeview.view:selected` ⇒ 选中行是**淡 accent** 而不是 GTK 默认的
    //   **实心蓝**、表头降半调加粗）**才生效**。
    //
    // ⚠️ **GTK4 的 `GtkTreeView` 不自带 `.view` 类**（那是 `GtkListView` 的）
    //   ⇒ 全项目以前都**没挂** ⇒ 这些规则**从来没生效过**（用户
    //   2026-11-23：「整个界面很不美」）。
    view.add_css_class("view");
    // 树形表格 ⇒ 画树线（父行与子行的连线），一眼看得出谁是谁的。
    view.set_enable_tree_lines(true);
    // 名称列的渲染器（**冒烟用它验「字真的画出来了」**）。
    let mut name_renderer: Option<gtk::CellRendererText> = None;
    // ⚠️ **阶段 155**：「操作」列的渲染器（每行一排图标按钮）。
    let action_renderer = crate::panel::actioncell::ActionCellRenderer::new();
    // ⚠️ **操作列的句柄要留在外面**（命中判定要认出「点的是不是这一列」）。
    let action_col = gtk::TreeViewColumn::new();
    let mut data_cols = Vec::new();
    {
        // ⚠️ **模型列号 ≠ 视图列下标**（阶段 155）：CPU / 内存 / 详情三列
        //    退役了 ⇒ 视图下标跟模型列号**不再相等** ⇒ 这里**必须带模型列号**
        //    走，别再拿循环下标当模型列号（以前两者恰好相等 ⇒ 删列就错位）。
        let cols: [(usize, &str); 6] = [
            (COL_NAME, tr("名称")),
            (COL_ID, tr("ID")),
            (COL_IMAGE, tr("镜像")),
            (COL_PORTS, tr("端口（主机:容器）")),
            (COL_STATE, tr("状态")),
            // ⚠️ **阶段 164**：资源列回来了（CPU 百分比 ＋ 内存占用）。
            (COL_RES, tr("资源（CPU / 内存）")),
        ];
        for (model_col, t) in cols {
            let col = gtk::TreeViewColumn::new();
            col.set_title(t);
            col.set_resizable(true);
            // ⚠️ **渲染器按列挑**：状态列用「药丸」（美化·地基 2/5）——胶囊底
            //    + 色点 + 文字，一屏扫下来先看到的是**形状**；其余列用文字。
            //
            // ⚠️ **必须显式喂数据**：GTK4 的 `pack_start` 只把渲染器塞进列，
            //    **不会**自己把这一列的文本喂给它 ⇒ 不绑就是一片空白（列头
            //    还在，看着像「查出来是空的」）。项目里 `panel/dbgrid.rs`
            //    踩过同一个坑。药丸走 `cell_data_func`，文字走 `add_attribute`。
            if model_col == COL_STATE {
                let pr = crate::panel::pillcell::PillCellRenderer::new();
                col.pack_start(&pr, false);
                col.set_cell_data_func(&pr, move |_col, cell, model, iter| {
                    // ⚠️ **文字用显示列**（阶段 158 起，这列已经是**成好的**
                    //   药丸文案：「运行 2 小时」/「2/2 运行中」……），
                    //   **颜色档位用原始状态列**（docker 的 `running` 等）。
                    let text: String = model.get(iter, COL_STATE as i32);
                    let raw: String = model.get(iter, COL_RAW_STATE as i32);
                    let kind: String = model.get(iter, COL_KIND as i32);
                    let p = cell
                        .downcast_ref::<crate::panel::pillcell::PillCellRenderer>()
                        .expect("状态列的渲染器必须是 PillCellRenderer");
                    p.set_text(text);
                    // ⚠️ **阶段 157（修 bug）**：以前这里拿**中文标签**
                    //   （`COL_STATE` = "运行中"）去判档位 ⇒ `pill_state_of`
                    //   认不出来 ⇒ **永远是「灰」** ⇒ 整个状态列一片死灰
                    //   （用户 2026-11-23：「很不美」）。
                    //   ⇒ 档位必须看 **原始状态**（`COL_RAW_STATE`）。
                    // ⚠️ **项目行仍然是灰的**：项目本身没有「运行 / 停止」，
                    //   灰点不假装它在跑（跟 `state_markup` 的口径一致）。
                    p.set_state(if kind == KIND_PROJECT {
                        crate::panel::pillcell::PillState::Dim
                    } else {
                        pill_state_of(&raw)
                    });
                });
            } else {
                let r = gtk::CellRendererText::new();
                r.set_ellipsize(gtk::pango::EllipsizeMode::End);
                // ⚠️ **技术值等宽**（美化·地基 3/5）：ID/端口等宽 ⇒
                //    `200f9d6b` 这种十六进制值在比例字体里数错位。
                match model_col {
                    COL_ID => {
                        r.set_family(Some("monospace"));
                    }
                    COL_PORTS => {
                        r.set_family(Some("monospace"));
                        // ⚠️ **端口允许多行**（用户 2026-11-23）：一个端口
                        //   一行 ⇒ 整串不再被省略号截掉。
                        r.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                        r.set_ellipsize(gtk::pango::EllipsizeMode::None);
                    }
                    _ => {}
                }
                col.pack_start(&r, false);
                // ⚠️ **阶段 157：ID / 端口两列改走 `cell_data_func`**
                //   （要「收拾一下再显示」：ID 只留前 8 位、端口去重压缩）。
                match model_col {
                    COL_NAME => {
                        // ⚠️ **阶段 162：名称列要能画 markup** ⇒ compose 项目行
                        //   能**加粗** ＋ 后面跟一个灰色小字「compose」
                        //   ⇒ 一眼就分得出「项目」和「容器」。
                        //
                        // ⚠️ **GTK4 没有 `use-markup` 属性**（那是 GTK3 的）
                        //   ⇒ GTK4 是**两个属性**：`text`（**纯文字**，内部会
                        //   自己转义）＋ `markup`（**原始 markup**）。
                        col.set_cell_data_func(&r, move |_col, cell, model, iter| {
                            let name: String = model.get(iter, COL_NAME as i32);
                            let mk: String = model.get(iter, COL_NAME_MK as i32);
                            let r = cell
                                .downcast_ref::<gtk::CellRendererText>()
                                .expect("名称列的渲染器必须是 CellRendererText");
                            if mk.is_empty() {
                                // ⚠️ **纯文字**：走 `text`（GTK4 自己转义
                                //   ⇒ 名字里有 `&` 也不会把 markup 弄坏）。
                                r.set_text(Some(&name));
                            } else {
                                r.set_markup(Some(&mk));
                            }
                        });
                    }
                    COL_ID => {
                        col.set_cell_data_func(&r, move |_col, cell, model, iter| {
                            let full: String = model.get(iter, COL_ID as i32);
                            let r = cell
                                .downcast_ref::<gtk::CellRendererText>()
                                .expect("ID 列的渲染器必须是 CellRendererText");
                            // ⚠️ **12 位 hex 占 130px 太奢侈**（用户 2026-11-23：
                            //   「很不美」）⇒ 只显示**前 8 位**（够认容器了）
                            //   ⇒ 完整 ID 在详情弹窗里看得到。
                            let short: String = full.chars().take(8).collect();
                            r.set_property("text", &short);
                        });
                    }
                    COL_PORTS => {
                        col.set_cell_data_func(&r, move |_col, cell, model, iter| {
                            let raw: String = model.get(iter, COL_PORTS as i32);
                            let r = cell
                                .downcast_ref::<gtk::CellRendererText>()
                                .expect("端口列的渲染器必须是 CellRendererText");
                            r.set_property("text", &ports_pretty(&raw));
                        });
                    }
                    _ => {
                        col.add_attribute(&r, "text", model_col as i32);
                    }
                }
                if model_col == COL_NAME {
                    name_renderer = Some(r.clone());
                }
            }
            // ⚠️ **列宽**（阶段 158，用户 2026-11-24：「容器名称列还是太宽了，
            //   看看怎么给每列设置宽度合理些，既要尽量显示全所有的信息，又要
            //   各列之间有合理的比例」）。
            //
            // ⚠️ **以前名称列 `expand(true)`** ⇒ 它一个人吃掉整条表格的富余
            //   宽度（截图里 1420px 的窗口它占了 820px ⇒ 名字只占一小条，
            //   别的列全挤在一起）⇒ **改成不展开**（按内容给宽度）。
            // ⚠️ **展开的是「镜像」列**（跟本文件其它表一个套路：`flat_table`
            //   里「展开**最容易被截断的那列**」）⇒ `emqx/emqx-enterprise:…`
            //   这种长镜像名**完整显示** ✓。
            // ⚠️ **阶段 160：每列的宽度按「内容该有多宽」给**（用户 2026-11-24：
            //   「容器名称列还是太宽了……既要尽量显示全所有的信息，又要各列之间
            //   有合理的比例」）⇒ **都不展开** ⇒ 富余宽度全给**最后一列**
            //   （操作列，图标右对齐）⇒ **中间的列之间不会隔出空洞**。
            //
            // ⚠️ **阶段 164：富余怎么分**（用户 2026-11-24：「操作列太宽了，
            //   按钮们太靠右了，没有一点留白」）：
            // - **实测 GTK4 的规矩**：**一列都不展开 ⇒ 富余全给最后一列**
            //   （临时实验：6 列各 200、窗口 1600 ⇒ 最后一列拿到 600）；
            //   **多列一起展开 ⇒ 富余在它们之间平分**（实测 333/333/334）⇒
            //   所以「操作」列**必须**显式 `set_expand(false)`，而且**得有几列
            //   展开**不然 GTK 又会把富余塞回最后一列；
            // - ⇒ **镜像 / 端口 / 状态 / 资源 四列一起展开** ⇒ 富余被摊开
            //   ⇒ 没有哪一列变成空谷，**按钮那边也留得下白**。
            match model_col {
                COL_NAME => {
                    // ⚠️ **不展开**（用户明说「太宽了」）⇒ 200（够放 20 来个字符）。
                    col.set_min_width(200);
                    col.set_expand(false);
                }
                // ⚠️ **ID 只显示前 8 位**（阶段 158）⇒ 84 足够。
                COL_ID => {
                    col.set_min_width(84);
                    col.set_expand(false);
                }
                COL_IMAGE => {
                    // ⚠️ **镜像名最长**（`emqx/emqx-enterprise:latest` 约 28 字）
                    //   ⇒ 240（阶段 157 时只有 230 ⇒ 会被省略号截掉）。
                    col.set_min_width(240);
                    // ⚠️ **阶段 164：这一列参与分富余**（见下面「富余怎么分」）。
                    col.set_expand(true);
                }
                // ⚠️ **端口可能有两个**（`3306/tcp, 33060/tcp`）⇒ 180。
                COL_PORTS => {
                    col.set_min_width(180);
                    col.set_expand(true);
                }
                // ⚠️ **阶段 158**：药丸文字变成「运行 2 小时」⇒ 比「运行中」宽。
                COL_STATE => {
                    col.set_min_width(125);
                    col.set_expand(true);
                }
                // ⚠️ **阶段 164：资源列**（`CPU 2.30% · 内存 52.84MiB`）⇒ 190。
                COL_RES => {
                    col.set_min_width(190);
                    col.set_expand(true);
                }
                _ => {}
            }
            // ⚠️ **每一列都能排序**（UI 审查 D4）：给列头一个排序 id ⇒
            //   用户点列头就能排序（GTK 会自己画排序箭头）。
            //   ⚠️ **默认不排**（不调 `set_sort_column_id`）⇒ 保持
            //   `docker ps` 原来的顺序，不擅自改用户的视图。
            col.set_sort_column_id(model_col as i32);
            view.append_column(&col);
            data_cols.push((model_col, col));
        }
        // ---- **「操作」列**（阶段 155）----
        //
        // ⚠️ **这一列没有模型列** ⇒ 图标是**现算**的（按行类型 + 原始状态）
        //    ⇒ 走 `cell_data_func`，不走 `add_attribute`。
        // ⚠️ **这一列不排序**（图标没法比大小）⇒ 不给 `set_sort_column_id`。
        action_col.set_title(tr("操作"));
        action_col.set_resizable(false);
        // ⚠️ **阶段 164：这一列**不再**吸收富余宽度**（用户 2026-11-24：
        //   「操作列太宽了，按钮们太靠右了，没有一点留白」）。
        //
        //   **为什么以前是「吸收」**：阶段 158 的想法是「富余全给最后一列 ⇒
        //   图标右对齐 ⇒ 中间的列之间不会隔出空洞」⇒ 实测**GTK4 就是这么
        //   干的**（临时实验：6 列各 200、窗口 1600 ⇒ **不设 expand 时
        //   富余 400 全给了最后一列**）⇒ 结果就是**操作列 480px 宽、里面只有
        //   4 颗 16px 的图标贴在最右边** ⇒ 一大片空白 ＋ 按钮贴死窗口右缘。
        //
        //   ⇒ 现在让**中间几列（镜像 / 端口 / 状态 / 资源）一起展开**
        //   ⇒ GTK4 会把富余**平分**给它们（实测：3 列展开 ⇒ 400 变成
        //   133/133/134）⇒ **没有任何一列变成空谷**，按钮那边也留得下白。
        action_col.set_expand(false);
        // ⚠️ **表头跟图标组一起左对齐**（图标组现在也是左对齐 ⇒ 标题正好
        //   压在第一颗图标上面；右对齐的话标题会跑到图标右边，对不上）。
        action_col.set_alignment(0.0);
        // ⚠️ **宽度按最多的那种行算**（compose 项目 4 颗）＋ **右边留一截
        //   空白**（⇒ 按钮不贴着窗口右缘）。
        action_col.set_min_width(
            (crate::panel::actioncell::SLOT_W * action_col_icons_max() as i32) + 24,
        );
        action_col.pack_start(&action_renderer, false);
        action_col.set_cell_data_func(&action_renderer, move |_col, cell, model, iter| {
            let kind: String = model.get(iter, COL_KIND as i32);
            let raw: String = model.get(iter, COL_RAW_STATE as i32);
            let r = cell
                .downcast_ref::<crate::panel::actioncell::ActionCellRenderer>()
                .expect("操作列的渲染器必须是 ActionCellRenderer");
            r.set_icons(
                row_actions(&kind, &raw)
                    .into_iter()
                    .map(|a| crate::panel::actioncell::Icon::new(&a.icon, a.enabled))
                    .collect(),
            );
        });
        view.append_column(&action_col);
    }
    // ---- 排序函数（UI 审查 D4）----
    //
    // ⚠️ **每个列都要注册**：GTK 不知道没注册的列该怎么排 ⇒ 点了没反应。
    // ⚠️ **阶段 155**：CPU / 内存列退役 ⇒ 只剩文字列 ⇒ 全部按字典序排。
    // ⚠️ **阶段 164：资源列回来了** ⇒ 它**按 CPU 百分比排**（不是按字符串
    //   ⇒ `"9.50%"` 排在 `"50.80%"` 前面 ⇒ 找「最占 CPU 的容器」不会看错）。
    {
        for col in [COL_NAME, COL_ID, COL_IMAGE, COL_PORTS, COL_STATE] {
            model.set_sort_func(
                gtk::SortColumn::Index(col as u32),
                move |m, a, b| {
                    let sa: String = m.get(a, col as i32);
                    let sb: String = m.get(b, col as i32);
                    // ⚠️ **端口列现在是多行**（带 `\n`）⇒ 直接按原字符串
                    //   比，`\n`（0x0A）会排在所有字符前面 ⇒ 多端口的
                    //   容器全挤到最前面 ⇒ 比较前**摊平成一行**再比。
                    match sa.replace('\n', " ").cmp(&sb.replace('\n', " ")) {
                        std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                        std::cmp::Ordering::Greater => gtk::Ordering::Larger,
                        std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                    }
                },
            );
        }
        model.set_sort_func(
            gtk::SortColumn::Index(COL_RES as u32),
            move |m, a, b| {
                let sa: String = m.get(a, COL_RES as i32);
                let sb: String = m.get(b, COL_RES as i32);
                let va = res_cpu_value(&sa);
                let vb = res_cpu_value(&sb);
                match va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal) {
                    std::cmp::Ordering::Less => gtk::Ordering::Smaller,
                    std::cmp::Ordering::Greater => gtk::Ordering::Larger,
                    std::cmp::Ordering::Equal => gtk::Ordering::Equal,
                }
            },
        );
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
        &[150, 110, 100, 80, 190],
        // ⚠️ **阶段 167：仓库 / 标签 / 创建时间 一起展开** ⇒ 富余被摊开
        //   （实测 GTK4 只展开一列 ⇒ 富余全给它 ⇒ 右边一大片空谷）。
        &[0, 1, 4],
        // ID 是十六进制 ⇒ 等宽；大小右对齐（美化·地基 3/5）
        &[ColStyle::Normal, ColStyle::Normal, ColStyle::Mono, ColStyle::Num, ColStyle::Normal],
    );
    let (volumes_store, volumes_view) = flat_table(
        &[tr("名称"), tr("驱动")],
        &[260, 140],
        // ⚠️ **两列都展开** ⇒ 富余平分 ⇒ 「驱动」不会孤零零地被推到最右边。
        &[0, 1],
        &[ColStyle::Normal; 2],
    );
    let (networks_store, networks_view) = flat_table(
        &[tr("名称"), tr("驱动"), tr("范围")],
        &[260, 140, 110],
        &[0, 1, 2],
        &[ColStyle::Normal; 3],
    );

    // ---- 阶段 120：镜像的**操作条**（推送 / 导出 / 打标签 / 删除）----
    //
    // ⚠️ **放在表格正上方**（不是详情区）⇒ 镜像段里也能一眼看到。
    let images_push_btn = gtk::Button::with_label(tr("推送"));
    images_push_btn.set_tooltip_text(Some(tr("把这个镜像推到仓库（docker push）")));
    let images_export_btn = gtk::Button::with_label(tr("导出…"));
    images_export_btn.set_tooltip_text(Some(tr("把镜像导出成本地的 tar 文件")));
    let images_tag_btn = gtk::Button::with_label(tr("打标签…"));
    images_tag_btn.set_tooltip_text(Some(tr("给这个镜像另打一个标签")));
    let images_remove_btn = gtk::Button::with_label(tr("删除"));
    images_remove_btn.set_tooltip_text(Some(tr("删除这个镜像（docker rmi）")));
    let images_hint = gtk::Label::new(Some(tr("选中一个镜像才能操作")));
    images_hint.set_xalign(0.0);
    images_hint.add_css_class("dim-label");
    // ⚠️ **搜索框**（UI 审查 D12）：容器页有，镜像 / 卷 / 网络三页以前
    //   **一点筛选都没有** ⇒ 镜像一多就只能肉眼扫。
    //
    // ⚠️ **阶段 167：搜索框**不**吸富余宽度**（用户 2026-11-25
    //   「搜索框吃满富余宽度」）⇒ 跟容器页（阶段 157）**同一个口径**：
    //   `hexpand(false)` ＋ 固定字符宽度 ⇒ 按钮不会被顶到天边。
    let images_search = gtk::SearchEntry::new();
    images_search.set_placeholder_text(Some(tr("搜索镜像")));
    images_search.set_hexpand(false);
    images_search.set_width_chars(16);
    images_search.set_max_width_chars(26);
    let images_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    images_bar.set_margin_start(6);
    images_bar.set_margin_end(6);
    images_bar.set_margin_top(4);
    images_bar.append(&images_search);
    images_bar.append(&images_push_btn);
    images_bar.append(&images_export_btn);
    images_bar.append(&images_tag_btn);
    images_bar.append(&images_remove_btn);
    images_bar.append(&images_hint);

    // 卷 / 网络两页：搜索栏 ＋ **删除按钮**（阶段 166）。
    let volumes_search = gtk::SearchEntry::new();
    volumes_search.set_placeholder_text(Some(tr("搜索卷")));
    volumes_search.set_hexpand(false);
    volumes_search.set_width_chars(16);
    volumes_search.set_max_width_chars(26);
    let volumes_remove_btn = gtk::Button::with_label(tr("删除"));
    volumes_remove_btn.set_tooltip_text(Some(tr("删除这个卷（docker volume rm；卷里的数据会一起没了）")));
    let volumes_hint = gtk::Label::new(Some(tr("选中一个卷才能删除")));
    volumes_hint.set_xalign(0.0);
    volumes_hint.add_css_class("dim-label");
    let volumes_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    volumes_bar.set_margin_start(6);
    volumes_bar.set_margin_end(6);
    volumes_bar.set_margin_top(4);
    volumes_bar.append(&volumes_search);
    volumes_bar.append(&volumes_remove_btn);
    volumes_bar.append(&volumes_hint);

    let networks_search = gtk::SearchEntry::new();
    networks_search.set_placeholder_text(Some(tr("搜索网络")));
    networks_search.set_hexpand(false);
    networks_search.set_width_chars(16);
    networks_search.set_max_width_chars(26);
    let networks_remove_btn = gtk::Button::with_label(tr("删除"));
    networks_remove_btn.set_tooltip_text(Some(tr("删除这个网络（docker network rm；用着它的容器会受影响）")));
    let networks_hint = gtk::Label::new(Some(tr("选中一个网络才能删除")));
    networks_hint.set_xalign(0.0);
    networks_hint.add_css_class("dim-label");
    let networks_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    networks_bar.set_margin_start(6);
    networks_bar.set_margin_end(6);
    networks_bar.set_margin_top(4);
    networks_bar.append(&networks_search);
    networks_bar.append(&networks_remove_btn);
    networks_bar.append(&networks_hint);

    let (images_root, images_empty) = flat_page_with_bar(&images_view, Some(&images_bar));
    let (volumes_root, volumes_empty) = flat_page_with_bar(&volumes_view, Some(&volumes_bar));
    let (networks_root, networks_empty) = flat_page_with_bar(&networks_view, Some(&networks_bar));

    // ---- 阶段 115：顶层的四个分段（容器 / 镜像 / 卷 / 网络）----
    //
    // ⚠️ **用 ToggleButton ＋ group**：点一个别的自动取消 ⇒ 表现像
    //   「分段按钮」而不是「四个复选框」。
    // 美化·二批 7：整条收进 `.pp-segmented` 胶囊 ⇒ 跟容器页筛选组、
    //   表设计器的分段一个样式；刷新图标按钮作为**尾段**留在胶囊里。
    let seg_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    seg_bar.add_css_class("pp-segmented");
    // ⚠️ 胶囊**贴内容**（不 expand）⇒ 不然整幅白条里只有四个小段，
    //    看着像一条空工具栏。
    seg_bar.set_halign(gtk::Align::Start);
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
    // ⚠️ **空态 / 错态**（UI 审查 D6）：列表空的时候不是「一块空白」，
    //   而是「标题 + 解释 + 重试」⇒ 远端没装 docker / 没权限 / 守护进程
    //   没起，**三者的文案不一样** ⇒ 用户一眼知道该做什么。
    let empty = crate::emptystate::build_dynamic(&crate::emptystate::docker_empty());
    empty.root.set_visible(false);
    table_overlay.add_overlay(&empty.root);

    // ---- 详情区（阶段 155：**搬进弹窗**了）----
    //
    // 布局：标题行（容器名 + 状态药丸）＋ 四张卡片（2×2 网格）。
    //
    // ⚠️ **阶段 155**：这块以前在页面**下半区**（Paned 的下半格）⇒ 用户说
    //   「不要下面那部分了，整个页面都改成列表」⇒ 现在它**装在弹窗里**
    //   （见 `ensure_detail_window`）⇒ 页面只剩**一张满高的列表**。
    // ⚠️ **标题行里的四个按钮也没了**（启动 / 停止 / 重启 / 进入容器）
    //   ⇒ 全部搬进**「操作」列**（每行一排图标按钮）⇒ 这里只管**看**。
    let d_title = gtk::Label::new(Some(""));
    d_title.set_xalign(0.0);
    d_title.set_hexpand(true);
    d_title.add_css_class("heading");

    let d_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    d_row.set_margin_start(8);
    d_row.set_margin_end(8);
    d_row.set_margin_top(4);
    // 状态药丸（美化·地基 2/5）：状态从标题的括号里搬出来，给个胶囊形状。
    // 档位类（pp-pill-ok/-warn/-err/-dim）由 apply_detail 按状态现挂。
    let d_pill = gtk::Label::new(None);
    d_pill.add_css_class("pp-pill");
    d_pill.set_valign(gtk::Align::Center);
    d_row.append(&d_title);
    d_row.append(&d_pill);
    // ⚠️ **阶段 156**：末尾放一个**「关闭」** ⇒ 弹窗里永远有一个看得见的
    //   关闭入口（普通 `GtkWindow` 没有自带标题栏 ⇒ 外面那个 X 不一定在）。
    let d_close = detail_close_button();
    d_row.append(&d_close);

    // 一张卡片 = 标题 + **键值表**（UI 审查 D9：不是一坨自动换行的文本）。
    let (c_config, card_config) = mk_kv_card(tr("运行配置"));
    let (c_net, card_net) = mk_kv_card(tr("网络与端口"));
    let (c_mount, card_mount) = mk_kv_card(tr("挂载"));
    let (c_env, card_env) = mk_kv_card(tr("环境变量"));

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
    // ⚠️ **「折行」开关**（UI 审查 D10）：默认**不折行**（折行会把时间戳
    //   和正文拆开）⇒ 但长行的尾巴就这么没了 ⇒ 给个开关，想看全就打开。
    let log_wrap = gtk::ToggleButton::with_label(tr("折行"));
    log_wrap.set_tooltip_text(Some(tr("长行折成两行显示（默认关闭，用横向滚动看全文）")));
    // ⚠️ **「复制全部」**（UI 审查 D10）：长行看不见 ⇒ 至少能整段复制走。
    let log_copy = gtk::Button::with_label(tr("复制全部"));
    log_copy.set_tooltip_text(Some(tr("把当前显示的全部日志复制到剪贴板")));
    log_copy.add_css_class("flat");
    // ⚠️ **「下载…」**（阶段 167：用户 2026-11-25「日志要增加一个下载按钮」）
    //   ⇒ 把**容器里的日志**存成本地文件（不是「另存当前显示的那几行」）
    //   ⇒ 走 `docker logs --tail N`（**不带 `-f`**）一次性取回。
    let log_save = gtk::Button::with_label(tr("下载…"));
    log_save.set_tooltip_text(Some(tr("把容器日志保存到本地文件")));
    log_save.add_css_class("flat");

    let log_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    log_bar.set_margin_start(6);
    log_bar.set_margin_end(6);
    log_bar.set_margin_top(4);
    log_bar.append(&log_search);
    log_bar.append(&log_pause);
    log_bar.append(&log_follow_lbl);
    log_bar.append(&log_follow);
    log_bar.append(&log_wrap);
    log_bar.append(&log_save);
    log_bar.append(&log_copy);
    log_bar.append(&log_clear);

    let log_scroll = gtk::ScrolledWindow::new();
    // ⚠️ **横向滚动条常显**（UI 审查 D10）：以前 `Automatic` ⇒ 内容不够长
    //   时滚动条**整个消失** ⇒ 用户根本不知道右边还有内容 ⇒ 长行尾巴
    //   看着像「被截断了」。
    log_scroll.set_policy(gtk::PolicyType::Always, gtk::PolicyType::Automatic);
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

    // ---- 文件页（阶段 116）：路径栏 ＋ 文件表 ＋ 上传 / 下载 / 查看 ----
    //
    // ⚠️ **容器里没有 SFTP** ⇒ 一切靠 `docker exec`（见 `backend/docker.rs`）。
    //
    // ⚠️ **阶段 140：工具条**压成**一行**（以前是「路径行」＋「按钮行」两行
    //   大按钮 ⇒ 白占一整行高度），按钮改成**纯图标**（文字挪进 tooltip）。
    //   ⇒ 跟文件面板的工具条同一口径（用户 2026-10-28：「主界面尽量少用文字」）。
    let f_path = gtk::Entry::new();
    f_path.set_placeholder_text(Some(tr("容器里的目录")));
    f_path.set_hexpand(true);
    f_path.set_width_chars(8);
    f_path.add_css_class("pp-path");
    // ⚠️ **阶段 167：去掉「回到上一级目录」**（用户 2026-11-25：
    //   「就去掉『回到上一级目录』吧，因为已经有『..』完成相同的功能了」）。
    //
    //   **为什么去掉更好**：↑ / ↓ 这两个箭头**本来就不对称**（↑ 看着像
    //   「上一级」、↓ 看着像「下载」⇒ 放在一排里谁也猜不出谁是谁）
    //   ⇒ 用户的记忆负担全靠 tooltip 顶着 ⇒ 干脆去掉一个。
    //
    // ⚠️ **剩下的图标按「一眼看得出意思」挑**：
    //   - 刷新 ⇒ **环形箭头**（`view-refresh`）⇒ 一眼就是「重新读一遍」；
    //   - 上传文件 ⇒ **纸飞机**（`send-to`）⇒ 一眼就是「发出去」；
    //   - 上传目录 ⇒ **文件夹 + 加号**（`folder-new`）⇒ 一眼就是「多一个目录」；
    //   - 下载 ⇒ **文件夹 + 下箭头**（`folder-download`）⇒ 一眼就是「取下来」；
    //   - 查看 ⇒ **放大镜**（`edit-find`）⇒ 一眼就是「看看内容」。
    //   ⇒ **全是 Adwaita 真有的图标**（已核对 `/usr/share/icons/Adwaita/symbolic`）。
    let f_refresh = icon_btn("view-refresh-symbolic", tr("重新读取这个目录"));
    let f_upload = icon_btn("send-to-symbolic", tr("把本地文件上传到容器当前目录"));
    let f_uploaddir = icon_btn("folder-new-symbolic", tr("把本地整个目录上传到容器当前目录"));
    let f_download = icon_btn("folder-download-symbolic", tr("把容器里的文件下载到本地"));
    let f_view = icon_btn("edit-find-symbolic", tr("查看容器里这个文件的内容"));

    let f_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    f_bar.set_margin_start(6);
    f_bar.set_margin_end(6);
    f_bar.set_margin_top(2);
    f_bar.set_margin_bottom(2);
    f_bar.append(&f_path);
    for b in [&f_refresh, &f_upload, &f_uploaddir, &f_download, &f_view] {
        f_bar.append(b);
    }

    // ⚠️ **阶段 140**：文件表用**自己的**建法（`files_table`）⇒ 名称列前面
    //   有**图标**（目录 / 文件 / 链接一眼分得清），而且**没有「类型」列**
    //   （图标已经表达了 ⇒ 省下的宽度给文件名）。
    let (f_store, f_view_tbl) = files_table();
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
    f_root.append(&f_scroll);
    f_root.append(&f_xfer_status);
    f_root.append(&f_status);

    // ⚠️ **回调要用**（控件要搬进 `DockerUi`）⇒ 先留一份句柄。
    let f_path_cb = f_path.clone();
    let f_view_cb = f_view_tbl.clone();

    // ---- 阶段 165：概览 / 日志 / 文件**各一扇独立窗口** ----
    //
    // ⚠️ **用户 2026-11-24**：「其他这几个 tab 都单独出来吧，每次点鼠标
    //   右键就只出来一个窗口就行了」
    //   ⇒ 以前是「一个弹窗里放页签（Stack ＋ StackSwitcher）」⇒ 现在
    //   **拆成三扇窗**（概览 / 日志 / 文件），同一时刻只显示一扇。
    // ⚠️ **「监控」页整个删掉**（同一天：「如果真需要监控，我肯定会搭建
    //   监控的基础设施，而不是从这里看」）⇒ 连它的采样流与折线图一起删。

    // ---- 概览：标题行 ＋ 四张键值卡（原来 `grid` 是 stack 里的那一页）----
    let d_root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    d_root.set_margin_start(6);
    d_root.set_margin_end(6);
    d_root.set_margin_bottom(6);
    d_root.append(&d_row);
    d_root.append(&grid);
    // ⚠️ **没选容器时不显示**（`apply_detail` / `show_project` 会打开它）。
    d_root.set_visible(false);

    // ---- 日志窗：标题行 ＋ 日志正文 ----
    //
    // ⚠️ **标题行里放一个「关闭」**（跟概览窗一个口径）⇒ 普通 `GtkWindow`
    //   没有自带标题栏 ⇒ 用户可能一个关闭入口都看不到。
    let log_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    log_head.set_margin_start(8);
    log_head.set_margin_end(8);
    log_head.set_margin_top(4);
    let log_head_title = gtk::Label::new(Some(tr("容器日志")));
    log_head_title.set_xalign(0.0);
    log_head_title.set_hexpand(true);
    log_head_title.add_css_class("heading");
    let log_close = detail_close_button();
    log_head.append(&log_head_title);
    log_head.append(&log_close);
    let logs_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    logs_box.append(&log_head);
    logs_box.append(&log_root);

    // ---- 文件窗：标题行 ＋ 文件区 ----
    let files_head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    files_head.set_margin_start(8);
    files_head.set_margin_end(8);
    files_head.set_margin_top(4);
    let files_head_title = gtk::Label::new(Some(tr("容器文件")));
    files_head_title.set_xalign(0.0);
    files_head_title.set_hexpand(true);
    files_head_title.add_css_class("heading");
    let files_close = detail_close_button();
    files_head.append(&files_head_title);
    files_head.append(&files_close);
    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.append(&files_head);
    files_box.append(&f_root);

    // ---- compose 项目详情（阶段 155：**也搬进弹窗**了）----
    //
    // ⚠️ 与**容器详情互斥**：两块在同一个弹窗里，谁显示谁。
    let p_title = gtk::Label::new(Some(""));
    p_title.set_xalign(0.0);
    p_title.set_hexpand(true);
    p_title.add_css_class("heading");

    // ⚠️ **阶段 155**：原来这行有五个按钮（编辑 / 启动 / 停止 / 销毁 /
    //   重建）⇒ 全部搬进**「操作」列**（项目行那几颗图标）⇒ 这里只留标题。
    let p_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    p_row.set_margin_start(8);
    p_row.set_margin_end(8);
    p_row.set_margin_top(4);
    p_row.append(&p_title);
    // ⚠️ **阶段 156**：项目详情也得有**「关闭」**（一个 widget 只能有一个
    //   parent ⇒ 不能跟容器详情那个共用同一个按钮）。
    let p_close = detail_close_button();
    p_row.append(&p_close);

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

    // 两块详情**装在同一个盒子里**（**阶段 155**：这个盒子进了**弹窗**）。
    //
    // ⚠️ **为什么还是两个盒子**（不合成一个）：容器详情与 compose 项目详情
    //   **互斥**（看容器时不该露出别的项目的信息）⇒ 两块各自 `set_visible`。
    let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    detail_box.append(&d_root);
    detail_box.append(&p_root);

    // ---- 状态行 ----
    //
    // ⚠️ **一行三样**（UI 审查 D7 / D13）：
    // - 左边：当前分段的**状态文字**（每个分段各存一份 ⇒ 不互相污染）；
    // - 右边：「上次刷新 hh:mm:ss」（**从工具条挪过来** ⇒ 工具条不再
    //   挤成一团，D13 说的「上次刷新」与「自动刷新」连成一片的问题消失）；
    // - 再右边：**双击提示**（双击进 shell 以前是个**没有提示的隐藏交互**）。
    let status = gtk::Label::new(Some(tr("正在读取远程容器…")));
    status.set_xalign(0.0);
    status.set_margin_start(6);
    status.set_margin_end(6);
    status.set_margin_top(2);
    status.set_margin_bottom(2);
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);

    last.set_margin_end(6);
    last.set_valign(gtk::Align::Center);

    let hint = gtk::Label::new(Some(tr("双击容器行可进入 shell")));
    hint.set_xalign(1.0);
    hint.set_margin_end(6);
    hint.set_margin_top(2);
    hint.set_margin_bottom(2);
    hint.add_css_class("dim-label");
    hint.set_valign(gtk::Align::Center);

    // ⚠️ **阶段 157：「上次刷新」和「双击提示」之间加一个「·」**。
    //   以前两段 dim 文字**紧挨着** ⇒ 读起来像一句病句
    //   （「还没刷新 双击容器行可进入 shell」）⇒ 一个分隔点就分开了。
    let status_dot = gtk::Label::new(Some("·"));
    status_dot.add_css_class("dim-label");
    status_dot.set_valign(gtk::Align::Center);

    let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status_bar.append(&status);
    status_bar.append(&last);
    status_bar.append(&status_dot);
    status_bar.append(&hint);

    // ---- 页面：**满高的列表**（阶段 155）----
    //
    // ⚠️ **阶段 155**：以前这里是「上（工具条 + 表格）｜下（详情区）」的
    //   **竖向 Paned** ⇒ 用户说「不要下面那部分了，整个页面都改成列表」
    //   ⇒ **Paned 撤掉** ⇒ 页面只剩工具条 + 满高的表格。
    //   详情（概览 / 日志 / 监控 / 文件 ＋ compose 项目）**搬进弹窗**
    //   （见 `ensure_detail_window`）⇒ 点「操作」列里的 💡 打开。
    let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
    top.append(&bar);
    // ⚠️ **用 Overlay 包一层**：大转圈要**在列表正中间**（loading 动画）。
    top.append(&table_overlay);

    // ---- 阶段 115：顶层四页（容器 / 镜像 / 卷 / 网络）----
    //
    // ⚠️ **容器那一整套**（工具条 + 表格）整体变成「容器」子页。
    let seg_stack = gtk::Stack::new();
    seg_stack.set_vexpand(true);
    seg_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    let _ = seg_stack.add_named(&top, Some(SEG_CONTAINERS));
    let _ = seg_stack.add_named(&images_root, Some(SEG_IMAGES));
    let _ = seg_stack.add_named(&volumes_root, Some(SEG_VOLUMES));
    let _ = seg_stack.add_named(&networks_root, Some(SEG_NETWORKS));

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&seg_bar);
    page.append(&seg_stack);
    page.append(&status_bar);

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
        status_text: RefCell::new(HashMap::new()),
        empty,
        images_empty,
        volumes_empty,
        networks_empty,
        denied_notified: Cell::new(false),
        search_entry: search.clone(),
        hint,
        row_menu: RefCell::new(None),
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
        last_action_click: Cell::new(None),
        action_col: action_col.clone(),
        data_cols,
        column_density: Cell::new(ColumnDensity::Wide),
        column_layout_pending: Cell::new(false),
        column_resize_active: Cell::new(false),
        viewport_adj: scroll.hadjustment(),
        detail_box: detail_box.clone(),
        logs_box: logs_box.clone(),
        files_box: files_box.clone(),
        detail_window: RefCell::new(None),
        logs_window: RefCell::new(None),
        files_window: RefCell::new(None),
        file_view_window: RefCell::new(None),
        file_view_text: RefCell::new(None),
        detail_close_btns: RefCell::new(vec![
            d_close.clone(),
            p_close.clone(),
            log_close.clone(),
            files_close.clone(),
        ]),
        detail: DetailUi {
            root: d_root,
            title: d_title,
            pill: d_pill,
            config: card_config,
            net: card_net,
            mount: card_mount,
            env: card_env,
            proj_root: p_root,
            proj_title: p_title,
            proj_info: p_info,
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
            wrap: log_wrap.clone(),
            save: log_save.clone(),
            status: log_status,
        },
        log_name: RefCell::new(String::new()),
        log_lines: RefCell::new(Vec::new()),
        log_partial: RefCell::new(String::new()),
        log_paused: Cell::new(false),
        log_search: RefCell::new(String::new()),
        log_stop: RefCell::new(None),
        log_running: Cell::new(false),
        seg: seg_stack.clone(),
        seg_buttons: RefCell::new(seg_buttons.clone()),
        seg_current: Cell::new(SEG_CONTAINERS),
        images_store: images_store.clone(),
        images_view: images_view.clone(),
        images_bar: images_bar.clone(),
        images_push_btn: images_push_btn.clone(),
        images_export_btn: images_export_btn.clone(),
        images_tag_btn: images_tag_btn.clone(),
        images_remove_btn: images_remove_btn.clone(),
        images_hint: images_hint.clone(),
        images_selected: RefCell::new(String::new()),
        images_op_msg: RefCell::new(String::new()),
        images_all: RefCell::new(Vec::new()),
        volumes_all: RefCell::new(Vec::new()),
        networks_all: RefCell::new(Vec::new()),
        volumes_bar: volumes_bar.clone(),
        networks_bar: networks_bar.clone(),
        images_query: RefCell::new(String::new()),
        volumes_query: RefCell::new(String::new()),
        networks_query: RefCell::new(String::new()),
        volumes_store: volumes_store.clone(),
        volumes_view: volumes_view.clone(),
        networks_store: networks_store.clone(),
        networks_view: networks_view.clone(),
        volumes_remove_btn: volumes_remove_btn.clone(),
        networks_remove_btn: networks_remove_btn.clone(),
        volumes_hint: volumes_hint.clone(),
        networks_hint: networks_hint.clone(),
        volumes_selected: RefCell::new(String::new()),
        networks_selected: RefCell::new(String::new()),
        volumes_op_msg: RefCell::new(String::new()),
        networks_op_msg: RefCell::new(String::new()),
        images_busy: Cell::new(false),
        busy_pending: Cell::new(false),
        images_pending: Cell::new(false),
        volumes_pending: Cell::new(false),
        networks_pending: Cell::new(false),
        files: FilesUi {
            path: f_path,
            store: f_store,
            view: f_view_tbl,
            btns: vec![
                f_refresh.clone(),
                f_upload.clone(),
                f_uploaddir.clone(),
                f_download.clone(),
                f_view.clone(),
            ],
            status: f_status,
            xfer_status: f_xfer_status,
        },
        files_name: RefCell::new(String::new()),
        files_path: RefCell::new(String::new()),
        files_req: RefCell::new(String::new()),
        files_xfer: Cell::new(false),
        files_seq: Cell::new(0),
        files_xfer_done: Cell::new(0),
        volumes_busy: Cell::new(false),
        networks_busy: Cell::new(false),
        term_chan: RefCell::new(None),
        term_running: Cell::new(false),
        term_window: RefCell::new(None),
        term_window_ready: Cell::new(false),
        term_closing: Cell::new(false),
        term_view: RefCell::new(None),
        term_bar: RefCell::new(None),
        term_shell_combo: RefCell::new(None),
        term_search_bar: RefCell::new(None),
        term_find_entry: RefCell::new(None),
        term_shell_items: RefCell::new(Vec::new()),
        term_status: RefCell::new(None),
        term_reconnect_btn: RefCell::new(None),
        term_clear_btn: RefCell::new(None),
        term_search_btn: RefCell::new(None),
        term_name: RefCell::new(String::new()),
        term_shell: RefCell::new(String::new()),
        term_gen: Cell::new(0),
        term_reconnects: Cell::new(0),
        term_req: Cell::new(0),
        selected: RefCell::new(String::new()),
        selected_project: RefCell::new(String::new()),
        restoring: Cell::new(false),
        last_action: RefCell::new(String::new()),
    });

    {
        // ⚠️ **视口宽度变了就重排列**（阶段 171）。
        //
        //   **为什么听 `hadjustment.page-size` 而不是 `notify::width`**：
        //   GTK4 **不会**在重新分配时发 `notify::width`（实测：窗口
        //   1200 → 900 一次都不触发）⇒ 靠它做响应式 = 永远不重排。
        //   `hadjustment.page-size` 才是**视口宽度**，每次变宽变窄都触发。
        let weak = Rc::downgrade(&ui);
        ui.viewport_adj.connect_notify_local(Some("page-size"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                schedule_column_layout(&ui);
            }
        });
        // ⚠️ **保险丝**：万一某些布局下视口宽度不变、但表格本身被重新分配
        //   （例如列显隐引起的内容变化），也顺手重排一次。
        let weak = Rc::downgrade(&ui);
        ui.view.connect_notify_local(Some("width"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                schedule_column_layout(&ui);
            }
        });

        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&ui);
        legacy.connect_event(move |_, event| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress => ui.column_resize_active.set(true),
                gtk::gdk::EventType::ButtonRelease => {
                    ui.column_resize_active.set(false);
                    schedule_column_layout(&ui);
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        ui.view.add_controller(legacy);
    }
    schedule_column_layout(&ui);

    // ---- 阶段 120：镜像的**操作条**接线 ----
    {
        // ⚠️ **选中变了** ⇒ 记住选中的是哪个镜像 ＋ 按钮跟着变。
        {
            let u = Rc::downgrade(&ui);
            let view = images_view.clone();
            view.connect_cursor_changed(move |v| {
                let Some(u) = u.upgrade() else { return };
                on_images_cursor_changed(&u, v);
            });
        }
        // ⚠️ **四个动作**（**写操作都有二次确认**；导出不动远端 ⇒ 不问）。
        {
            let stw = Rc::downgrade(st);
            let uw = Rc::downgrade(&ui);
            ui.images_push_btn.connect_clicked(move |_| {
                let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
                let image = u.images_selected.borrow().clone();
                if image.is_empty() {
                    return;
                }
                images_push(&st, &u, &image);
            });
        }
        {
            let stw = Rc::downgrade(st);
            let uw = Rc::downgrade(&ui);
            ui.images_export_btn.connect_clicked(move |_| {
                let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
                let image = u.images_selected.borrow().clone();
                if image.is_empty() {
                    return;
                }
                images_export(&st, &u, &image);
            });
        }
        {
            let stw = Rc::downgrade(st);
            let uw = Rc::downgrade(&ui);
            ui.images_tag_btn.connect_clicked(move |_| {
                let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
                let image = u.images_selected.borrow().clone();
                if image.is_empty() {
                    return;
                }
                images_tag(&st, &u, &image);
            });
        }
        {
            let stw = Rc::downgrade(st);
            let uw = Rc::downgrade(&ui);
            ui.images_remove_btn.connect_clicked(move |_| {
                let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
                let image = u.images_selected.borrow().clone();
                if image.is_empty() {
                    return;
                }
                images_remove(&st, &u, &image);
            });
        }
        update_images_actions(&ui);
    }
    // ---- 阶段 166：卷 / 网络的**删除**接线 ----
    //
    // ⚠️ **跟镜像页一个套路**：选中变了 ⇒ 记住选中的是谁 ＋ 按钮跟着变。
    {
        let u = Rc::downgrade(&ui);
        let view = volumes_view.clone();
        view.connect_cursor_changed(move |v| {
            let Some(u) = u.upgrade() else { return };
            on_volumes_cursor_changed(&u, v);
        });
    }
    {
        let u = Rc::downgrade(&ui);
        let view = networks_view.clone();
        view.connect_cursor_changed(move |v| {
            let Some(u) = u.upgrade() else { return };
            on_networks_cursor_changed(&u, v);
        });
    }
    {
        let stw = Rc::downgrade(st);
        let uw = Rc::downgrade(&ui);
        ui.volumes_remove_btn.connect_clicked(move |_| {
            let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
            let name = u.volumes_selected.borrow().clone();
            if name.is_empty() {
                return;
            }
            volumes_remove(&st, &u, &name);
        });
    }
    {
        let stw = Rc::downgrade(st);
        let uw = Rc::downgrade(&ui);
        ui.networks_remove_btn.connect_clicked(move |_| {
            let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
            let name = u.networks_selected.borrow().clone();
            if name.is_empty() {
                return;
            }
            networks_remove(&st, &u, &name);
        });
    }
    update_volumes_actions(&ui);
    update_networks_actions(&ui);

    // ---- 三个分段的**搜索框**（UI 审查 D12）----
    {
        let u = Rc::downgrade(&ui);
        images_search.connect_search_changed(move |s| {
            let Some(u) = u.upgrade() else { return };
            *u.images_query.borrow_mut() = s.text().to_string();
            refilter_images(&u);
        });
    }
    {
        let u = Rc::downgrade(&ui);
        volumes_search.connect_search_changed(move |s| {
            let Some(u) = u.upgrade() else { return };
            *u.volumes_query.borrow_mut() = s.text().to_string();
            refilter_volumes(&u);
        });
    }
    {
        let u = Rc::downgrade(&ui);
        networks_search.connect_search_changed(move |s| {
            let Some(u) = u.upgrade() else { return };
            *u.networks_query.borrow_mut() = s.text().to_string();
            refilter_networks(&u);
        });
    }

    // 主线程接收循环（**弱引用** ⇒ 面板关了这条循环自己会结束）。
    {
        let u = Rc::downgrade(&ui);
        let stw = Rc::downgrade(st);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(r) = rx.recv().await {
                let Some(u) = u.upgrade() else { break };
                match r {
                    Msg::List(Ok(info)) => {
                        u.apply(&info);
                        // ⚠️ **阶段 161**：撞上了「待会儿再刷」⇒ 立刻再刷一次
                        //   （不然写操作之后的那次刷新被丢掉 ⇒ 表里还是旧状态）。
                        if u.busy_pending.get() {
                            u.busy_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh(&st, &u);
                            }
                        }
                    }
                    Msg::List(Err(msg)) => {
                        // ⚠️ **没权限 ⇒ 弹一次对话框**（阶段 135）。
                        u.containers_failed(stw.upgrade().as_ref(), &msg);
                    }
                    // ⚠️ **阶段 115**：三个新分段的结果。
                    //    ⚠️ **失败时只改状态行**（不清容器表）⇒ 切回容器页
                    //    还能看到原来的列表。
                    Msg::Images(Ok(v)) => {
                        u.images_busy.set(false);
                        apply_images(&u, &v);
                        // ⚠️ **取到数据了 ⇒ 错态收起来**（阶段 135）。
                        u.hide_seg_error(SEG_IMAGES);
                        // ⚠️ **阶段 161：撞上了「待会儿再刷」⇒ 立刻再刷一次**
                        //   （不然打标签/删除之后的那次刷新被丢掉 ⇒ 表里还是旧数据）。
                        if u.images_pending.get() {
                            u.images_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_images(&st, &u);
                            }
                            // ⚠️ **操作结果先留着** ⇒ 等**最后一次**刷完再贴
                            //   （不然会被「正在读取…」冲掉）。
                        } else {
                            // ⚠️ **把操作结果贴回去**（刷新那条消息会冲掉它）。
                            let op = u.images_op_msg.borrow().clone();
                            if !op.is_empty() {
                                u.set_segment_status(SEG_IMAGES, &op);
                                u.images_op_msg.borrow_mut().clear();
                            }
                        }
                    }
                    Msg::Images(Err(msg)) => {
                        u.images_busy.set(false);
                        // ⚠️ **阶段 161**：同样要把「待会儿再刷」兑现掉。
                        if u.images_pending.get() {
                            u.images_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_images(&st, &u);
                            }
                        }
                        // ⚠️ **大号错态**（阶段 135）：不能只剩底部那行小字。
                        u.seg_failed(stw.upgrade().as_ref(), SEG_IMAGES, &msg);
                    }
                    Msg::Volumes(Ok(v)) => {
                        u.volumes_busy.set(false);
                        apply_volumes(&u, &v);
                        u.hide_seg_error(SEG_VOLUMES);
                        // ⚠️ **阶段 161**：同上 ⇒ 撞上了就再刷一次。
                        if u.volumes_pending.get() {
                            u.volumes_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_volumes(&st, &u);
                            }
                        } else {
                            // ⚠️ **阶段 166**：把删除结果贴回去（刷新那条
                            //   「共 N 个卷」会把结果冲掉）。
                            let op = u.volumes_op_msg.borrow().clone();
                            if !op.is_empty() {
                                u.set_segment_status(SEG_VOLUMES, &op);
                                u.volumes_op_msg.borrow_mut().clear();
                            }
                        }
                    }
                    Msg::Volumes(Err(msg)) => {
                        u.volumes_busy.set(false);
                        if u.volumes_pending.get() {
                            u.volumes_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_volumes(&st, &u);
                            }
                        }
                        u.seg_failed(stw.upgrade().as_ref(), SEG_VOLUMES, &msg);
                    }
                    Msg::Networks(Ok(v)) => {
                        u.networks_busy.set(false);
                        apply_networks(&u, &v);
                        u.hide_seg_error(SEG_NETWORKS);
                        // ⚠️ **阶段 161**：同上 ⇒ 撞上了就再刷一次。
                        if u.networks_pending.get() {
                            u.networks_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_networks(&st, &u);
                            }
                        } else {
                            // ⚠️ **阶段 166**：把删除结果贴回去。
                            let op = u.networks_op_msg.borrow().clone();
                            if !op.is_empty() {
                                u.set_segment_status(SEG_NETWORKS, &op);
                                u.networks_op_msg.borrow_mut().clear();
                            }
                        }
                    }
                    Msg::Networks(Err(msg)) => {
                        u.networks_busy.set(false);
                        if u.networks_pending.get() {
                            u.networks_pending.set(false);
                            if let Some(st) = stw.upgrade() {
                                refresh_networks(&st, &u);
                            }
                        }
                        u.seg_failed(stw.upgrade().as_ref(), SEG_NETWORKS, &msg);
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
                        // ⚠️ **阶段 159：比的是「正在请求的目录」**
                        //   （`files_req`）⇒ 当前目录（`files_path`）**只在成功
                        //   之后才推进** ⇒ 失败时它还是上一个成功的目录。
                        if u.files_name.borrow().as_str() != name
                            || u.files_req.borrow().as_str() != path
                        {
                            continue;
                        }
                        match r {
                            Ok(v) => {
                                // ⚠️ **成功才推进「当前目录」**（阶段 159）：
                                //   路径栏显示的内容、表格里的行、双击时拼路径的
                                //   基准 —— 三者必须**指同一个目录**。
                                *u.files_path.borrow_mut() = path.clone();
                                u.files.path.set_text(&path);
                                apply_files(&u, &v);
                            }
                            // ⚠️ **说人话**（阶段 140）：容器里没有 `ls` 时
                            //   docker 的原话是「OCI runtime exec failed …」
                            //   ⇒ 用户看不懂「这个容器列不了目录」。
                            Err(msg) => {
                                // ⚠️ **阶段 159：失败 ⇒ 路径栏回到上一个成功的目录**
                                //   （表格也还是那个目录的行）⇒ 用户不会拿着
                                //   「旧表格 + 新路径」继续点 ⇒ 路径不再累积。
                                let cur = u.files_path.borrow().clone();
                                if !cur.is_empty() {
                                    u.files.path.set_text(&cur);
                                }
                                // ⚠️ **阶段 172 / UI-07**：界面上给**中文分类
                                //   ＋ 下一步建议**，**原文放进 tooltip** ⇒
                                //   「说人话」与「保留原文」同时成立。
                                let e = classify_files_error(&msg);
                                u.files.status.set_text(&e.brief);
                                u.files.status.set_tooltip_text(Some(e.raw.as_str()));
                            }
                        }
                    }
                    // ⚠️ **阶段 120**：镜像操作的结果（进度 / 成功 / 失败）。
                    Msg::ImageOp { msg, done } => {
                        u.set_segment_status(SEG_IMAGES, &msg);
                        if done {
                            // ⚠️ **结果先存着** ⇒ 等列表刷新完再贴回去
                            //   （刷新那条「共 N 个镜像」会把结果冲掉）。
                            *u.images_op_msg.borrow_mut() = msg;
                            // ⚠️ **操作完了重新列一遍**（打标签 / 删除都改了远端）。
                            if let Some(st) = stw.upgrade() {
                                refresh_images(&st, &u);
                            }
                        }
                    }
                    // ⚠️ **阶段 166**：卷 / 网络写操作的结果。
                    Msg::SegOp { seg, msg } => {
                        u.set_segment_status(seg, &msg);
                        // ⚠️ **结果先存着** ⇒ 等列表刷新完再贴回去
                        //   （刷新那条「共 N 个卷」会把结果冲掉）。
                        match seg {
                            SEG_VOLUMES => *u.volumes_op_msg.borrow_mut() = msg,
                            SEG_NETWORKS => *u.networks_op_msg.borrow_mut() = msg,
                            _ => {}
                        }
                        // ⚠️ **操作完了重新列一遍**（远端已经变了）。
                        if let Some(st) = stw.upgrade() {
                            match seg {
                                SEG_VOLUMES => refresh_volumes(&st, &u),
                                SEG_NETWORKS => refresh_networks(&st, &u),
                                _ => {}
                            }
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
                        Ok(text) => show_file_window(stw.upgrade().as_ref(), &path, &text, Some(&u)),
                        Err(msg) => {
                            if let Some(st) = stw.upgrade() {
                                let parent = host_window(&u, &st);
                                crate::panel::dialogs::show_error(
                                    parent.as_ref(),
                                    &tr("查看文件失败"),
                                    &msg,
                                );
                            }
                        }
                    },
                    Msg::LinkKind {
                        container,
                        path,
                        link,
                        r,
                    } => match r {
                        Ok(kind) => {
                            let Some(st) = stw.upgrade() else { continue };
                            // ⚠️ **目标是目录 ⇒ 进去；是文件 ⇒ 看内容**
                            //   （用户 2026-11-27：「双击符号链接不能跟随」）。
                            if kind == "dir" {
                                files_goto(&st, &u, &container, &path);
                            } else {
                                files_view(&st, &u, &container, &path);
                            }
                        }
                        Err(msg) => {
                            if let Some(st) = stw.upgrade() {
                                let parent = host_window(&u, &st);
                                crate::panel::dialogs::show_error(
                                    parent.as_ref(),
                                    &tr("打开符号链接失败"),
                                    &trf!("{link}：{msg}", link = link, msg = msg),
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
                                // ⚠️ **阶段 165：「监控」页整个删了**（用户
                                //   2026-11-24：「如果真需要监控，我肯定会搭建监控
                                //   的基础设施，而不是从这里看」）⇒ 采样流与折线图
                                //   一并退场。
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
                        let empty = u.log_lines.borrow().is_empty();
                        u.log.status.set_text(&log_end_status(empty, code));
                    }
                    Msg::LogError { name, msg } => {
                        if u.log_name.borrow().as_str() != name {
                            continue;
                        }
                        u.log_running.set(false);
                        u.log.status.set_text(&msg);
                    }
                    Msg::LogSave { msg } => {
                        u.log.status.set_text(&msg);
                    }
                    Msg::Action(msg) => {
                        *u.last_action.borrow_mut() = msg.clone();
                        u.set_segment_status(SEG_CONTAINERS, &msg);
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
                    // ⚠️ **阶段 139：选中的是「容器行」⇒ compose 项目那块要
                    //   **收起来**。
                    //
                    //   干什么用（用户 2026-11-19 报的截图）：选中的是**独立
                    //   容器** some-postgres（它没有 compose）⇒ 底下却显示着
                    //   **别的项目** eng_tut 的 compose 信息（连「配置文件
                    //   /opt/eng_tut/docker-compose.yml」都写着）⇒ 用户根本
                    //   分不清那是哪个容器的。
                    //
                    //   ⚠️ **项目块在详情框里是「容器详情」的兄弟**（不是子页
                    //      签里的东西）⇒ 不藏起来的话，它在**每个页签**
                    //      （概览 / 日志 / 监控 / 文件 / 终端）都露着。
                    //
                    //   ⇒ 规则：**点项目只显示 compose 内容；点容器只显示
                    //     容器内容**（两种详情互斥，跟项目详情藏容器详情一个
                    //     道理）。
                    *u.selected_project.borrow_mut() = String::new();
                    u.detail.proj_root.set_visible(false);
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
                    // ⚠️ **换容器了** ⇒ 容器终端也得换（它显示的是**这个
                    //    容器**的 shell）。
                    if u.term_name.borrow().as_str() != n && u.term_running.get() {
                        // ⚠️ **只停通道，不关窗口**（阶段 152）⇒ 用户看到
                        //   的是同一个窗口里换了个容器（而不是窗口闪一下
                        //   没了又出来）。
                        close_term_channel(&u);
                        u.term_running.set(false);
                        // 窗口开着 ⇒ 给新容器起一条。
                        if u.term_window.borrow().is_some() {
                            open_container_terminal(&st, &u, &n);
                        }
                    }
                    load_detail(&st, &u, &n);
                }
                None => u.clear_detail(),
            }
        });
    }
    // ================= 阶段 155：「操作」列的点击 =================
    //
    // ⚠️ **为什么自己画图标按钮**（而不是每行放真按钮）：GTK4 **没有**
    //   `GtkCellRendererWidget` ⇒ 单元格里放不下真 widget ⇒「操作」列是
    //   一个**自绘的 CellRenderer**（见 `panel::actioncell`）⇒ 命中判定
    //   也得自己做（`path_at_pos` 给格内坐标 ⇒ 算命中第几颗图标）。
    //
    // ⚠️ **GTK4 没有 `button-press-event` 信号** ⇒ 用 `GestureClick`。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        let click = gtk::GestureClick::new();
        click.set_button(1);
        // ⚠️ **捕获阶段** ⇒ 我们的手势先于表格自己的点击手势跑 ⇒ 命中图标
        //   时可以**独占**这次点击（表格不会跟着选中 / 激活那一行）。
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |g, n_press, x, y| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            // ⚠️ **双击只算第一次**（否则一次双击派发两遍动作）。
            if n_press > 1 {
                return;
            }
            let Some(hit) = hit_action_column(&u, x as i32, y as i32) else {
                return;
            };
            // 命中了 ⇒ 独占这次点击（表格不再选中 / 激活那一行）。
            //
            // ⚠️ GTK4 的 `GtkGestureSingle::claim()` **gtk4-rs 没暴露**
            //   ⇒ 用 `set_state(Claimed)`（同一个意思）。
            g.set_state(gtk::EventSequenceState::Claimed);
            // ⚠️ **记住点过图标** ⇒ 双击图标时表格的「激活」（进 shell）要
            //   拦下来（否则点一下图标、双击一下就跑进容器了）。
            u.last_action_click.set(Some(glib::monotonic_time()));
            dispatch_row_action(&st, &u, &hit);
        });
        view.add_controller(click);
    }
    // ---- **「操作」列的 tooltip**（阶段 155）----
    //
    // ⚠️ **为什么自己写**：图标是画上去的 ⇒ GTK 不知道每颗图标叫什么 ⇒
    //   悬停说明也得自己给（禁用时说明「为什么不能点」）。
    {
        let u = Rc::downgrade(&ui);
        view.connect_query_tooltip(move |view, x, y, _kbd, tooltip| {
            let Some(u) = u.upgrade() else { return false };
            let Some(hit) = hit_action_column(&u, x, y) else {
                return false;
            };
            // GTK4 的树视图 tooltip 要自己指定行（默认处理器被我们接管了）。
            view.set_tooltip_row(tooltip, &hit.ctx.path);
            let is_project = hit.ctx.kind == KIND_PROJECT;
            tooltip.set_text(Some(action_tooltip(&hit.action, is_project, &hit.ctx.raw)));
            true
        });
    }
    // ================= 阶段 112：日志 =================
    // ⚠️ **阶段 165：日志是独立窗口** ⇒ 起流 / 停流都挂在**窗口**上，
    //   不再挂在「页签切换」上：
    // - 打开日志窗 ⇒ 起流（`open_logs_window` 里显式调）；
    // - 关掉日志窗 ⇒ 停流（`ensure_logs_window` 的 `connect_hide`）。
    // ⚠️ **为什么关窗就停**：`docker logs -f` 是长跑的 ⇒ 一直挂着就是在
    //   用户的服务器上白占一个进程（还占着一条 SSH 通道）。
    // ================= 阶段 116：文件页 =================
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        // **双击目录 ⇒ 进入；双击文件 ⇒ 看内容**。
        f_view_cb.connect_row_activated(move |_, path, _| {
            let Some(u) = u.upgrade() else { return };
            let Some(st) = stw.upgrade() else { return };
            let container = u.selected.borrow().clone();
            if container.is_empty() {
                return;
            }
            // ⚠️ **阶段 162：按 path 取「被双击的那一行」**
            //   （以前问的是「当前选中行」⇒ 两者不一定一致 ⇒ 会动错行）。
            let Some((name, kind)) = files_row_at(&u, path) else { return };
            // ⚠️ **阶段 162：「..」⇒ 回上一级**（不是「进一个叫 `..` 的
            //   目录」⇒ 那会拼出 `/media/..` 这种怪路径）。
            if kind == "up" {
                let cur = u.files_path.borrow().clone();
                if cur.is_empty() {
                    return;
                }
                let parent = crate::util::path_parent(&cur);
                files_goto(&st, &u, &container, &parent);
                return;
            }
            let remote = files_remote_path(&u, &name);
            if kind == "dir" {
                files_goto(&st, &u, &container, &remote);
            } else if kind == "file" {
                files_view(&st, &u, &container, &remote);
            } else if kind == "link" {
                // ⚠️ **符号链接要跟随**（用户 2026-11-27：「双击符号链接不能
                //   跟随。最好能显示符号链接指向的文件」）⇒ 问一次目标的类型
                //   ⇒ 目标是目录就进去、是文件就看内容。
                let target = files_row_link(&u, path);
                if target.is_empty() {
                    return;
                }
                let abs = files_link_target_abs(&u, &name, &target);
                files_follow_link(&st, &u, &container, &remote, &abs);
            }
        });
        // **悬停 ⇒ tooltip 里写清「这是什么」**（阶段 140）。
        //
        // ⚠️ **为什么**：表里**没有「类型」列**了（图标表达了）⇒ 想看
        //   「目录 / 文件 / 链接」这种**文字**时，悬停一下就有。
        {
            let view = f_view_cb.clone();
            view.connect_query_tooltip(|view, x, y, _kbd, tooltip| {
                // ⚠️ **坐标换算**（阶段 156）：tooltip 给的是 widget 坐标，
                //   `path_at_pos` 要 bin window 坐标 ⇒ 不换算会显示**下一行**。
                let (bx, by) = view.convert_widget_to_bin_window_coords(x, y);
                let Some((Some(path), _col, _cx, _cy)) = view.path_at_pos(bx, by) else {
                    return false;
                };
                // GTK4 的树视图 tooltip 要自己指定行（默认处理器被我们接管了）
                view.set_tooltip_row(tooltip, &path);
                let Some(model) = view.model() else { return false };
                let Some(mut it) = model.iter_first() else { return false };
                loop {
                    if model.path(&it) == path {
                        let name: String = model.get(&it, FC_NAME as i32);
                        let kind: String = model.get(&it, FC_TYPE as i32);
                        let size: String = model.get(&it, FC_SIZE as i32);
                        let perm: String = model.get(&it, FC_PERM as i32);
                        let mtime: String = model.get(&it, FC_MTIME as i32);
                        let link: String = model.get(&it, FC_LINK as i32);
                        let mut lines = vec![name];
                        if !kind.is_empty() {
                            lines.push(trf!("类型：{v0}", v0 = kind));
                        }
                        if !size.is_empty() {
                            lines.push(trf!("大小：{size}", size = size));
                        }
                        if !perm.is_empty() {
                            lines.push(trf!("权限：{p}", p = perm));
                        }
                        // ⚠️ **修改时间**（用户 2026-11-27）⇒ tooltip 里也写
                        //   一份（列窄了会被截断 ⇒ 这里给全的）。
                        if !mtime.is_empty() {
                            lines.push(trf!("修改时间：{m}", m = mtime));
                        }
                        // ⚠️ **符号链接指向**（用户 2026-11-27）。
                        if !link.is_empty() {
                            lines.push(trf!("指向：{l}", l = link));
                        }
                        tooltip.set_text(Some(&lines.join("\n")));
                        return true;
                    }
                    if !model.iter_next(&mut it) {
                        return false;
                    }
                }
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
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("先选一个容器"),
                        &tr("选中一个容器之后才能上传文件"),
                    );
                    return;
                }
                let top = host_window(&u, &st);
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
                // ⚠️ **弹窗的 transient parent**（UI 审查 D20）⇒ 传进闭包。
                let parent2 = top.clone();
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
                                    parent2.as_ref(),
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
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("先选一个容器"),
                        &tr("选中一个容器之后才能下载文件"),
                    );
                    return;
                }
                let Some((name, kind)) = files_selected(&u) else {
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("先选一个条目"),
                        &tr("先在列表里选中一个条目，再点「下载…」"),
                    );
                    return;
                };
                // ⚠️ **阶段 162：「..」是上一级目录**，不是文件 ⇒ 说清楚。
                if kind == "up" {
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("这是上一级目录"),
                        &tr("双击列表里的「..」就能回到上一级"),
                    );
                    return;
                }
                let remote = files_remote_path(&u, &name);
                let is_dir = kind == "dir";
                let top = host_window(&u, &st);
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
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("先选一个条目"),
                        &tr("先在列表里选中一个文件，再点「查看…」"),
                    );
                    return;
                };
                if kind == "dir" {
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("看不了目录"),
                        &tr("目录请用「下载…」整个拉下来"),
                    );
                    return;
                }
                // ⚠️ **阶段 162：「..」是上一级目录**，不是文件 ⇒ 说清楚。
                if kind == "up" {
                    let parent = host_window(&u, &st);
                    crate::panel::dialogs::show_error(
                        parent.as_ref(),
                        &tr("这是上一级目录"),
                        &tr("双击列表里的「..」就能回到上一级"),
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
    // **「折行」开关**（UI 审查 D10）
    {
        let u = Rc::downgrade(&ui);
        log_wrap.connect_toggled(move |b| {
            let Some(u) = u.upgrade() else { return };
            // ⚠️ **开着就折行，不开就横向滚动**（折行会把时间戳与正文拆开）。
            u.log
                .view
                .set_wrap_mode(if b.is_active() {
                    gtk::WrapMode::WordChar
                } else {
                    gtk::WrapMode::None
                });
        });
    }
    // **「复制全部」**（UI 审查 D10）
    {
        let u = Rc::downgrade(&ui);
        log_copy.connect_clicked(move |_| {
            let Some(u) = u.upgrade() else { return };
            let text = u.log.buffer.text(&u.log.buffer.start_iter(), &u.log.buffer.end_iter(), false);
            copy_to_clipboard(&text);
            u.log
                .status
                .set_text(&trf!("已复制 {n} 个字符到剪贴板", n = text.chars().count()));
        });
    }
    // **「下载…」**（阶段 167：把容器日志存成本地文件）
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        log_save.connect_clicked(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            logs_save(&st, &u);
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
                // ⚠️ **底部那行跟着换**（UI 审查 D7）：否则切到「镜像」页
                //   底栏还挂着「正在读取远程容器…」（上一段污染下一段）。
                u.sync_status_line();
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
        // ⚠️ **阶段 117**：双击顶层容器行 ⇒ 进 shell ⇒ 要用到 `PanelState`
        //   （开终端要它）⇒ 这里补一份**弱引用**（AGENTS.md 红线：不能抓强
        //   引用 ⇒ 会成环）。
        let stw = Rc::downgrade(st);
        view.connect_row_activated(move |v, path, _col| {
            let Some(u) = u.upgrade() else { return };
            // ⚠️ **阶段 155**：刚点过「操作」列的图标 ⇒ 这次「激活」是那次
            //   点击的延续（双击图标）⇒ 跳过（不然点图标会顺带进 shell）。
            if let Some(t) = u.last_action_click.get() {
                let now = glib::monotonic_time();
                if now - t < 700_000 {
                    u.last_action_click.set(None);
                    return;
                }
                u.last_action_click.set(None);
            }
            // 只有**有子行的行**（项目行）才响应。
            let Some(it) = u.model.iter(path) else {
                return;
            };
            if u.model.iter_children(Some(&it)).is_none() {
                // ⚠️ **阶段 117**：**没有子行的行 = 独立容器** ⇒ 双击 ⇒
                //   进容器 shell（`docker exec -it`）。
                //   ⚠️ **原来这里直接 return** ⇒ 顶层容器行双击**什么也
                //   不发生** ⇒ 入口少了一个 ⇒ 用户说「没看到在哪开」。
                let name: String = u.model.get(&it, COL_NAME as i32);
                if name.is_empty() {
                    return;
                }
                let Some(st) = stw.upgrade() else { return };
                open_container_terminal(&st, &u, &name);
                return;
            }
            if v.row_expanded(path) {
                v.collapse_row(path);
            } else {
                v.expand_row(path, false);
            }
        });
    }
    // ================= UI 审查 D5：右键菜单 ＋ 键盘 =================
    {
        // ---- **右键菜单**（按行类型给不同的项）----
        //
        // ⚠️ **为什么要有**：写操作（启停 / 重启 / 进 shell）以前**只能**
        //   「选中行 → 看下半区详情 → 点按钮」⇒ 在 20 个容器里停掉一个要
        //   滚动、选中、够到下半区、再点 ⇒ 右键直接就在手边。
        //
        // ⚠️ **阶段 160：操作列瘦身后（只留启动 / 停止 / 重启）**，
        //   「进容器 / 文件 / 日志 / 属性 / 编辑 compose / 销毁 / 重建」
        //   全搬到这儿 ⇒ 菜单**按行类型**给不同的项（容器行 vs 项目行）
        //   ⇒ **每次弹出前按这一行重建**（见 `build_row_menu`）。
        //
        // ⚠️ **GTK4 没有 `connect_button_press_event`**（那是 GTK3 的写法）
        //   ⇒ 右键要用 `GestureClick`（`set_button(3)`）。
        let pop = crate::menu::popover(&view);
        let menu_layout_ui = Rc::downgrade(&ui);
        // 右键 ⇒ 挪光标 ＋ 按这一行的类型重建菜单 ＋ 弹出来。
        {
            let view2 = view.clone();
            let pop2 = pop.clone();
            let stw = Rc::downgrade(st);
            let u = Rc::downgrade(&ui);
            let click = gtk::GestureClick::new();
            click.set_button(3);
            click.connect_pressed(move |_g, _n, x, y| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                // ⚠️ **坐标换算**（阶段 156）：手势是 widget 坐标，
                //   `path_at_pos` 要 bin window 坐标 ⇒ 不换算会选中**下一行**。
                let (bx, by) = view2.convert_widget_to_bin_window_coords(x as i32, y as i32);
                let mut ctx: Option<RowCtx> = None;
                if let Some((Some(path), _c, _cx, _cy)) = view2.path_at_pos(bx, by) {
                    gtk::prelude::TreeViewExt::set_cursor(&view2, &path, None, false);
                    ctx = row_ctx_at(&u, &path);
                }
                build_row_menu(&st, &u, &pop2, ctx.as_ref());
                pop2.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    x as i32,
                    y as i32,
                    1,
                    1,
                )));
                pop2.popup();
            });
            view.add_controller(click);
        }
        pop.connect_hide(move |_| {
            if let Some(ui) = menu_layout_ui.upgrade() {
                schedule_column_layout(&ui);
            }
        });
        *ui.row_menu.borrow_mut() = Some(pop);
    }
    // ---- **键盘**（↑↓ 选行 / Enter 进 shell / Del 停止 / `/` 搜）----
    //
    // ⚠️ **↑↓ 是 GTK 自带的**（表格获得焦点就能上下选行）⇒ 这里只补
    //   三个**没有**的：Enter / Del / `/`。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_k, keyval, _code, mods| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            match keyval {
                // ⚠️ **Enter** ⇒ 进 shell（跟双击一样）。
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    if let Some(name) = u.selected_container_name() {
                        open_container_terminal(&st, &u, &name);
                        return glib::Propagation::Stop;
                    }
                }
                // ⚠️ **Delete** ⇒ 停止容器（写操作 ⇒ 照样二次确认）。
                gtk::gdk::Key::Delete => {
                    if let Some(name) = u.selected_container_name() {
                        confirm_container_action(&st, &u, &name, Action::Stop);
                        return glib::Propagation::Stop;
                    }
                }
                // ⚠️ **`/`** ⇒ 跳到搜索框（跟大部分列表界面一样）。
                gtk::gdk::Key::slash if !ctrl => {
                    u.search_entry.grab_focus();
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        view.add_controller(key);
    }
    // ---- **空态的「重试」按钮**（UI 审查 D6）----
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        ui.empty.primary.connect_clicked(move |_| {
            let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                return;
            };
            refresh(&st, &u);
        });
    }
    // ---- **镜像 / 卷 / 网络三页的「重试」**（阶段 135）----
    //
    // ⚠️ **每一页重刷自己那一段**（别去刷容器页）⇒ 跟 `refresh_segment`
    //   一个口径（自动刷新也是这个口径）。
    {
        let stw = Rc::downgrade(st);
        let u = Rc::downgrade(&ui);
        for (empty, seg) in [
            (&ui.images_empty, SEG_IMAGES),
            (&ui.volumes_empty, SEG_VOLUMES),
            (&ui.networks_empty, SEG_NETWORKS),
        ] {
            let stw = stw.clone();
            let u = u.clone();
            empty.primary.connect_clicked(move |_| {
                let (Some(u), Some(st)) = (u.upgrade(), stw.upgrade()) else {
                    return;
                };
                match seg {
                    SEG_IMAGES => refresh_images(&st, &u),
                    SEG_VOLUMES => refresh_volumes(&st, &u),
                    _ => refresh_networks(&st, &u),
                }
            });
        }
    }
    ui
}

/// 把一段文字**复制到剪贴板**。
fn copy_to_clipboard(text: &str) {
    if let Some(d) = gtk::gdk::Display::default() {
        d.clipboard().set_text(text);
    }
}

/// **把容器日志存成本地文件**（阶段 167：用户「日志要增加一个下载按钮」）。
///
/// ⚠️ **取的是「容器里的全部日志」**（`docker logs --tail N`）⇒ 不是
///   「界面上现在显示的那几行」⇒ 用户存下来的东西是有用的。
///
/// ⚠️ **不带 `-f`**：跟随流是**通道**（`ShellChannel`）⇒ 拿不到「结尾」；
///   下载要的是一个**完整的静态文本** ⇒ 另开一条命令取一次。
fn logs_save(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    let name = ui.log_name.borrow().clone();
    if name.is_empty() {
        crate::panel::dialogs::show_error(
            host_window(ui, st).as_ref(),
            &tr("先打开一个容器的日志"),
            &tr("没有正在看的日志 ⇒ 不知道要下载哪个容器的"),
        );
        return;
    }
    let top = host_window(ui, st);
    let mut b = gtk::FileChooserNative::builder()
        .modal(true)
        .title(tr("把日志保存到哪个文件"))
        .action(gtk::FileChooserAction::Save)
        .accept_label(tr("保存"))
        .cancel_label(tr("取消"));
    if let Some(p) = top.as_ref() {
        b = b.transient_for(p);
    }
    let ch = b.build();
    // ⚠️ **默认文件名**：`<容器名>-<日期>.log`（省得自己想名字）。
    let mut def = format!("{}-{}.log", name.replace('/', "_"), local_date_stamp());
    if def.starts_with('-') {
        def.remove(0);
    }
    ch.set_current_name(&def);
    let (st2, u2) = (st.clone(), ui.clone());
    ch.connect_response(move |c, r| {
        if r == gtk::ResponseType::Accept {
            if let Some(f) = c.file() {
                if let Some(p) = f.path() {
                    run_logs_save(&st2, &u2, &name, &p);
                }
            }
        }
        c.destroy();
    });
    ch.show();
}

/// **今天的日期**（`YYYYMMDD`）⇒ 当默认文件名的一部分。
fn local_date_stamp() -> String {
    // ⚠️ **不引新依赖**：`chrono` 之类没必要 ⇒ 用系统时间自己算
    //   （UTC 的年月日就够了 ⇒ 文件名只要能区分天）。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // ⚠️ ** civil-from-days**（Howard Hinnant 的算法）⇒ 不引依赖也能算日期。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

/// **下载日志**的后台部分（远端取一次 ⇒ 写本地文件）。
fn run_logs_save(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    name: &str,
    dest: &std::path::Path,
) {
    ui.log.status.set_text(tr("正在取日志…"));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let dest = dest.to_path_buf();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::dump_logs(&session, &name, docker::LOG_DUMP_TAIL).await {
                Ok(text) => match std::fs::write(&dest, text.as_bytes()) {
                    Ok(()) => trf!(
                        "日志已保存到 {p}（{n} 字节）",
                        p = dest.display().to_string(),
                        n = text.len()
                    ),
                    Err(e) => trf!("保存失败：{e}", e = e),
                },
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::LogSave { msg });
    });
}

/// 启停 / 重启的**二次确认**（写操作 ⇒ 一律要问一句）。
///
/// ⚠️ **为什么单独一个函数**：详情区的按钮与右键菜单、键盘 Delete
///   都要走这同一套确认 ⇒ 别复制三份（改一处漏两处的经典坑）。
fn confirm_container_action(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    name: &str,
    action: Action,
) {
    // ⚠️ **二次确认**：停掉用户服务器上的容器是**不可逆**的副作用 ⇒
    //   先问一句（默认焦点落在「取消」上 ⇒ 手快按回车不会误停）。
    let msg = trf!(
        "确定要{act}容器「{name}」吗？",
        act = action.label(),
        name = name
    );
    let (st2, u2) = (st.clone(), ui.clone());
    let name2 = name.to_string();
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
        tr("请确认"),
        &msg,
        action.label(),
        move |ok| {
            if !ok {
                return;
            }
            do_action(&st2, &u2, action, &name2);
        },
    );
}

/// compose 项目写操作的**二次确认**（启动 / 停止 / 重启 / 销毁 / 重建）。
///
/// ⚠️ **阶段 160**：操作列瘦身后，项目的启停也走这里 ⇒ 项目行点「启动」
///   跑的是 `docker compose start`（不是 `docker start <项目名>`）。
fn confirm_compose_action(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    name: &str,
    action: ComposeAction,
) {
    // ⚠️ `do_compose_action` 从这里取项目名 ⇒ 先记上。
    *ui.selected_project.borrow_mut() = name.to_string();
    // ⚠️ **二次确认**：在用户服务器上动 compose 项目是**不可逆**的
    //   副作用（重建会**换掉容器**、销毁会**删掉容器与网络**）⇒ 先问一句
    //   （默认焦点在「取消」上 ⇒ 手快按回车不会误操作）。
    // ⚠️ **阶段 163：每种动作补一句「会发生什么」**（`confirm_hint`）——
    //   尤其销毁：**这个项目会从列表里消失**（项目行是从容器推导出来的）
    //   ⇒ 用户 2026-11-24 担心的正是这个 ⇒ 现在**按下之前**就说清楚。
    let msg = match action.confirm_hint() {
        Some(hint) => trf!(
            "确定要{act} compose 项目「{name}」吗？（{hint}）",
            act = action.label(),
            name = name,
            hint = hint
        ),
        None => trf!(
            "确定要{act} compose 项目「{name}」吗？",
            act = action.label(),
            name = name
        ),
    };
    let (st2, u2) = (st.clone(), ui.clone());
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
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
}

/* ---------------- 阶段 155：「操作」列 ---------------- */

/// **一行容器表的基本信息**（右键菜单、命中判定都要它）。
///
/// ⚠️ **阶段 160**：从 [`ActionHit`] 里拆出来 ⇒ 右键菜单不必伪造一个
///   「命中的图标」就能拿到「是哪一行」。
#[derive(Clone, Debug)]
struct RowCtx {
    /// 那一行的路径（tooltip / 挪光标要用它定位）。
    path: gtk::TreePath,
    /// 行名（容器名 / compose 项目名）。
    name: String,
    /// 行类型（`COL_KIND`）。
    kind: String,
    /// 原始状态（`COL_RAW_STATE`）。
    raw: String,
}

/// **命中结果**（点中了哪一行、哪颗图标）。
#[derive(Clone, Debug)]
struct ActionHit {
    /// 命中的那一行。
    ctx: RowCtx,
    /// 命中的那颗图标。
    action: RowAction,
}

/// 某一行在模型里的**基本信息**（**纯查表** ⇒ 菜单与命中判定共用）。
fn row_ctx_at(ui: &Rc<DockerUi>, path: &gtk::TreePath) -> Option<RowCtx> {
    let it = ui.model.iter(path)?;
    let name: String = ui.model.get(&it, COL_NAME as i32);
    let kind: String = ui.model.get(&it, COL_KIND as i32);
    let raw: String = ui.model.get(&it, COL_RAW_STATE as i32);
    Some(RowCtx {
        path: path.clone(),
        name,
        kind,
        raw,
    })
}

/// **按这一行的类型重建右键菜单**（阶段 160）。
///
/// ⚠️ **为什么每次弹出都重建**：菜单项**按行类型**不同（容器行有「文件 /
///   日志 / 复制 ID」，项目行有「编辑 compose / 销毁 / 重建」）
///   ⇒ 一次建好没法两用 ⇒ 弹出前按行重建（菜单很少弹 ⇒ 重建不心疼）。
///
/// ⚠️ **每一项都把「哪一行」记在闭包里** ⇒ 点它的时候动的是**右键的那一行**
///   （不是「当前选中那一行」）⇒ 免得「右键 A 实际动到 B」。
fn build_row_menu(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    pop: &gtk::Popover,
    ctx: Option<&RowCtx>,
) {
    // ⚠️ **旧内容扔掉**（换行类型 ⇒ 换内容）。
    pop.set_child(None::<&gtk::Widget>);
    let body = crate::menu::body();
    let Some(ctx) = ctx else {
        pop.set_child(Some(&body));
        return;
    };
    let acts = menu_actions(&ctx.kind, &ctx.raw);
    let mut last_group = u8::MAX;
    for a in acts {
        // ⚠️ **段号变了才插分隔线**（同一段里不插）。
        if a.group != last_group {
            if last_group != u8::MAX {
                body.append(&crate::menu::sep());
            }
            last_group = a.group;
        }
        let b = crate::menu::item(&a.icon, a.label, None);
        // ⚠️ **危险动作标红**（销毁项目会删掉容器与网络）。
        if a.kind == ActionKind::Down {
            crate::menu::danger(&b);
        }
        // ⚠️ **不能点的项要真的点不了**（跟「操作」列同一套算术）——
        //   运行中的容器在菜单里点「启动」只是空转 ⇒ 直接灰掉。
        if !a.enabled {
            b.set_sensitive(false);
        }
        let st2 = st.clone();
        let u2 = ui.clone();
        let ctx2 = ctx.clone();
        let kind = a.kind;
        let icon = a.icon.clone();
        b.connect_clicked(move |_| {
            u2.close_row_menu();
            dispatch_row_action(
                &st2,
                &u2,
                &ActionHit {
                    ctx: ctx2.clone(),
                    action: RowAction {
                        kind,
                        icon: icon.clone(),
                        enabled: true,
                    },
                },
            );
        });
        body.append(&b);
    }
    pop.set_child(Some(&body));
}

/// **点中了「操作」列里的第几颗图标**（没点中就 `None`）。
///
/// ⚠️ **`cell_x` 是格内坐标**（相对列的左边缘）⇒ 除以槽宽就是第几颗。
///   （实测确认：GTK4 的 `path_at_pos` 给的 `cell_x` 就是格内坐标。）
fn hit_action_column(ui: &Rc<DockerUi>, x: i32, y: i32) -> Option<ActionHit> {
    // ⚠️ **坐标必须换算**（阶段 156）：手势给的是 **widget 坐标**（含列头），
    //   而 `path_at_pos` 收的是 **bin window 坐标**（不含列头）⇒ 不换算就
    //   **整体下移一行** ⇒ 点 compose 项目行的「编辑」会命中它**下面那个
    //   容器行**的「进容器」⇒ 弹出 term 窗口（用户 2026-11-23 报的）。
    //   ⚠️ 老坑见 `src/panel/pane.rs:1881`（「右键 A 实际选中/上传 B」）。
    let (bx, by) = ui.view.convert_widget_to_bin_window_coords(x, y);
    let (path, col, cell_x, _cell_y) = ui.view.path_at_pos(bx, by)?;
    let path = path?;
    let col = col?;
    // ⚠️ **不是「操作」列** ⇒ 不算命中（点别的列就是普通点击）。
    if col != ui.action_col {
        return None;
    }
    let ctx = row_ctx_at(ui, &path)?;
    let acts = row_actions(&ctx.kind, &ctx.raw);
    // ⚠️ **阶段 158：图标组右对齐** ⇒ 第几颗要减去它在格内的左边缘。
    //   ⇒ **问渲染器**（`hit_index`）⇒ **画和判是同一套算术**
    //   （以前两边各写一份 ⇒ 迟早漂移）。
    let cell_w = ui.view.cell_area(Some(&path), Some(&ui.action_col)).width();
    // ⚠️ **`n` 用「这一行」的图标数**（不是渲染器里存着的那个 ——
    //   那是**最后绘制的那一行**的 ⇒ 拿它算会错位）。
    let idx = crate::panel::actioncell::hit_index_at(cell_x, cell_w, acts.len())?;
    let action = acts.get(idx)?.clone();
    Some(ActionHit { ctx, action })
}

/// **派发一次操作**（点中了「操作」列里的某颗图标）。
///
/// ⚠️ **写操作仍然二次确认**（跟以前那些按钮一个待遇）⇒ 启停 / 销毁 /
///   重建都走 `confirm_*`。
fn dispatch_row_action(st: &Rc<PanelState>, ui: &Rc<DockerUi>, hit: &ActionHit) {
    let is_project = hit.ctx.kind == KIND_PROJECT;
    let name = hit.ctx.name.clone();
    match hit.action.kind {
        ActionKind::Detail => {
            // ⚠️ **先把光标挪到那一行**（用户看得见自己点的是谁）。
            //   `restoring` 是「程序性恢复选中」⇒ 顺带**跳过**表格自己的
            //   选中回调（不然详情会被取两次）。
            ui.restoring.set(true);
            if let Some(path) = ui.find_path(&name) {
                gtk::prelude::TreeViewExt::set_cursor(&ui.view, &path, None, false);
            }
            ui.restoring.set(false);
            if is_project {
                ui.show_project(&name);
            } else {
                *ui.selected.borrow_mut() = name.clone();
                *ui.selected_project.borrow_mut() = String::new();
                ui.detail.proj_root.set_visible(false);
                load_detail(st, ui, &name);
            }
            // 概览窗（阶段 165：概览 / 日志 / 文件各一扇窗）。
            let title = if is_project {
                trf!("compose 项目：{n}", n = name)
            } else {
                trf!("容器：{n}", n = name)
            };
            open_detail_window(st, ui, &title);
        }
        ActionKind::Enter => {
            // ⚠️ **不是写操作**（只是进容器里跑个 shell）⇒ 不用二次确认。
            open_container_terminal(st, ui, &name);
        }
        ActionKind::Edit => {
            // 「编辑 compose 文件」⇒ **打开远程文件**（不是写操作 ⇒ 不必确认）。
            *ui.selected_project.borrow_mut() = name.clone();
            let has_file = ui
                .project_summary(&name)
                .map(|s| !s.files.is_empty())
                .unwrap_or(false);
            if !has_file {
                ui.set_segment_status(
                    SEG_CONTAINERS,
                    tr("这个项目没有记录 compose 配置文件路径 ⇒ 没法编辑"),
                );
                return;
            }
            open_compose_file(st, ui);
        }
        ActionKind::Start | ActionKind::Stop | ActionKind::Restart => {
            // ⚠️ **阶段 160：项目行要走 compose**（以前一律走容器路径
            //   ⇒ `docker start <项目名>` ⇒ 报「no such container」⇒
            //   **项目行的启动/停止其实是坏的**）。
            if is_project {
                let action = match hit.action.kind {
                    ActionKind::Start => ComposeAction::Start,
                    ActionKind::Stop => ComposeAction::Stop,
                    _ => ComposeAction::Restart,
                };
                confirm_compose_action(st, ui, &name, action);
            } else {
                let action = match hit.action.kind {
                    ActionKind::Start => Action::Start,
                    ActionKind::Stop => Action::Stop,
                    _ => Action::Restart,
                };
                // ⚠️ **二次确认**：停掉用户服务器上的容器是**不可逆**的副作用 ⇒
                //   先问一句（默认焦点落在「取消」上 ⇒ 手快按回车不会误停）。
                confirm_container_action(st, ui, &name, action);
            }
        }
        ActionKind::Files => {
            // ⚠️ **文件窗看的是「选中容器的目录」** ⇒ 打开时就把它设成选中。
            open_files_window(st, ui, &name);
        }
        ActionKind::Logs => {
            // ⚠️ **阶段 165**：日志**独立一扇窗**，打开就起流（不再靠
            //   「页签变了」这种隐式触发 ⇒ 换一个容器再点也照样起流）。
            open_logs_window(st, ui, &name);
        }
        ActionKind::CopyId => {
            let id = ui.selected_container_id();
            copy_to_clipboard(&id);
        }
        ActionKind::Down | ActionKind::Recreate => {
            let action = if hit.action.kind == ActionKind::Down {
                ComposeAction::Down
            } else {
                ComposeAction::Recreate
            };
            confirm_compose_action(st, ui, &name, action);
        }
    }
}

/// **详情弹窗里的「关闭」按钮**（阶段 156）。
///
/// ⚠️ **为什么非有不可**（用户 2026-11-23 报的「弹出对话框没法关闭」）：
///   详情弹窗是**普通 `gtk::Window`** ⇒ GTK4 **不替它画标题栏**（项目自己的
///   主窗口 / 编辑器窗口都是**显式** `set_titlebar(HeaderBar)` 才有的，
///   见 `src/main.rs:189`、`src/panel/fileedit.rs:1126`）⇒ 外面那个 X
///   **不一定存在** ⇒ **用户可能一个关闭入口都看不到**。
///   ⇒ 就在内容里放一个，**永远看得见**。
///
/// ⚠️ **只藏，不销毁**：弹窗里装着**共享的**详情控件（`detail_box`）
///   ⇒ 销毁了就再也开不出来（控件全成悬空句柄）。
fn detail_close_button() -> gtk::Button {
    let b = gtk::Button::with_label(tr("关闭"));
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    // ⚠️ **阶段 157：图标按钮/动作按钮都要有 tooltip**（用户 2026-11-23：
    //   「按钮增加 tooltip」）⇒ 这里是纯文字按钮 ⇒ 补一句说明它做什么。
    b.set_tooltip_text(Some(tr("关掉这个详情窗口（内容留着，下次打开还在）")));
    b.connect_clicked(|b| {
        // ⚠️ 往上找那扇窗（按钮是弹窗内容的一部分）⇒ **藏起来**，不销毁。
        if let Some(w) = b
            .ancestor(gtk::Window::static_type())
            .and_downcast::<gtk::Window>()
        {
            w.set_visible(false);
        }
    });
    b
}

/// **三扇详情窗里的哪一扇**（阶段 165）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailWin {
    /// 概览（容器详情 ＋ compose 项目详情）。
    Overview,
    /// 日志。
    Logs,
    /// 文件。
    Files,
}

/// **建一扇工具窗**（阶段 165：概览 / 日志 / 文件各一扇）。
///
/// ⚠️ **三扇窗的规矩一样**（记尺寸 / 挂宿主窗口 / Esc 关闭 / 只藏不销毁）
///   ⇒ 写一份，三处共用（免得各写各的、行为慢慢漂移）。
///
/// ⚠️ **关掉只是隐藏，不销毁**（AGENTS.md 的老坑）：窗里装的是**共享的**
///   控件 ⇒ 销毁了就再也开不出来（控件全成悬空句柄）。
fn new_tool_window(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    size_key: &str,
    title: &str,
    child: &gtk::Box,
) -> gtk::Window {
    let win = gtk::Window::new();
    win.set_title(Some(title));
    // ⚠️ **阶段 156**：**记住尺寸**（AGENTS.md：工具窗要记尺寸）⇒ 下次按它开。
    let (mut dw, mut dh) = (880, 620);
    if let Some((w, h)) = crate::settings::window_size(size_key) {
        if w >= 240 && h >= 180 {
            dw = w;
            dh = h;
        }
    }
    win.set_default_size(dw, dh);
    // ⚠️ **必须挂到宿主窗口下面**（transient parent）⇒ 不然弹窗可能落在
    //   主窗**后面**（UI 审查 D19 的老坑）。
    if let Some(parent) = host_window(ui, st) {
        win.set_transient_for(Some(&parent));
    }
    win.set_child(Some(child));
    // ⚠️ **这里必须 `hide` + `Stop`，不能 `Proceed`**：`Proceed` = 继续走
    //   GTK 的默认处理 ⇒ 它会把窗口**销毁**（见 `src/panel/dbui.rs:5616-5618`
    //   的老坑）⇒ 而窗里装的是**共享控件** ⇒ 一关内容就没了 ⇒ 下次打开是坏的。
    {
        let key = size_key.to_string();
        win.connect_close_request(move |w| {
            crate::settings::save_window_size(&key, w);
            w.set_visible(false);
            glib::Propagation::Stop
        });
    }
    // ⚠️ **Esc 关闭**（普通 `GtkWindow` 不像 `GtkDialog` 那样 Esc 自动关）。
    {
        let w = win.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                // ⚠️ **光标在输入框里时不抢 Esc**（日志窗的搜索框要靠 Esc
                //   清空）⇒ 只在焦点**不在可编辑控件**上时才关窗。
                if let Some(f) = gtk::prelude::GtkWindowExt::focus(&w) {
                    if f.is::<gtk::Entry>() {
                        return glib::Propagation::Proceed;
                    }
                }
                w.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(key);
    }
    win
}

/// **概览弹窗**（容器详情 ＋ compose 项目详情）。
fn ensure_detail_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>) -> gtk::Window {
    if let Some(w) = ui.detail_window.borrow().as_ref() {
        return w.clone();
    }
    let win = new_tool_window(
        st,
        ui,
        "docker_detail",
        tr("容器详情"),
        &ui.detail_box,
    );
    // ⚠️ **万一还是被销毁了**（将来谁又引入了 destroy）⇒ 把句柄摘掉
    //   ⇒ 下次**重建**，而不是拿一个野句柄去 `present()`。
    {
        let u = Rc::downgrade(ui);
        win.connect_destroy(move |_| {
            if let Some(u) = u.upgrade() {
                *u.detail_window.borrow_mut() = None;
            }
        });
    }
    *ui.detail_window.borrow_mut() = Some(win.clone());
    win
}

/// **日志弹窗**（阶段 165：日志**独立一扇窗**）。
fn ensure_logs_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>) -> gtk::Window {
    if let Some(w) = ui.logs_window.borrow().as_ref() {
        return w.clone();
    }
    let win = new_tool_window(st, ui, "docker_logs", tr("容器日志"), &ui.logs_box);
    // ⚠️ **藏起来 ⇒ 日志流停掉**（不然远端一直挂着 `docker logs -f`）。
    {
        let u = Rc::downgrade(ui);
        win.connect_hide(move |_| {
            if let Some(u) = u.upgrade() {
                stop_logs(&u);
            }
        });
    }
    {
        let u = Rc::downgrade(ui);
        win.connect_destroy(move |_| {
            if let Some(u) = u.upgrade() {
                *u.logs_window.borrow_mut() = None;
            }
        });
    }
    *ui.logs_window.borrow_mut() = Some(win.clone());
    win
}

/// **文件弹窗**（阶段 165：文件**独立一扇窗**）。
fn ensure_files_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>) -> gtk::Window {
    if let Some(w) = ui.files_window.borrow().as_ref() {
        return w.clone();
    }
    let win = new_tool_window(st, ui, "docker_files", tr("容器文件"), &ui.files_box);
    {
        let u = Rc::downgrade(ui);
        win.connect_destroy(move |_| {
            if let Some(u) = u.upgrade() {
                *u.files_window.borrow_mut() = None;
            }
        });
    }
    *ui.files_window.borrow_mut() = Some(win.clone());
    win
}

/// **只留一扇详情窗**（阶段 165：用户 2026-11-24「每次点鼠标右键就只出来
///   一个窗口就行了」）。
///
/// ⚠️ **关掉日志窗会停掉日志流**（`connect_hide`）⇒ 这正是我们要的：
///   同一时刻只有一扇窗 ⇒ 只有一条流。
fn show_only(ui: &Rc<DockerUi>, keep: DetailWin) {
    if keep != DetailWin::Overview {
        if let Some(w) = ui.detail_window.borrow().as_ref() {
            w.set_visible(false);
        }
    }
    if keep != DetailWin::Logs {
        if let Some(w) = ui.logs_window.borrow().as_ref() {
            w.set_visible(false);
        }
    }
    if keep != DetailWin::Files {
        if let Some(w) = ui.files_window.borrow().as_ref() {
            w.set_visible(false);
        }
    }
}

/// **关掉全部详情窗**（面板关掉时走的就是它）。
fn close_detail_window(ui: &Rc<DockerUi>) {
    // ⚠️ **三扇全关**（不是只关概览）⇒ 阶段 165 之后它们是**三扇独立的窗**。
    for w in [
        ui.detail_window.borrow().as_ref().cloned(),
        ui.logs_window.borrow().as_ref().cloned(),
        ui.files_window.borrow().as_ref().cloned(),
    ]
    .into_iter()
    .flatten()
    {
        w.set_visible(false);
    }
}

/// **打开概览窗**（标题带上是谁）。
fn open_detail_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>, title: &str) {
    let win = ensure_detail_window(st, ui);
    win.set_title(Some(&format!("{} · {}", tr("容器详情"), title)));
    show_only(ui, DetailWin::Overview);
    win.present();
}

/// **打开日志窗**（并给这个容器起流）。
///
/// ⚠️ **为什么显式起流**（阶段 165 修的坑）：以前靠「切到日志页」这个
///   **隐式事件**起流 ⇒ **已经开着日志页时再点另一个容器的「日志」**
///   ⇒ 页签没变 ⇒ **不起流** ⇒ 看到的还是上一个容器的日志（或一片空白）。
///   ⇒ 现在**打开窗口就起流**，跟「页签有没有变」无关。
fn open_logs_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    *ui.selected.borrow_mut() = name.to_string();
    *ui.selected_project.borrow_mut() = String::new();
    ui.detail.proj_root.set_visible(false);
    let win = ensure_logs_window(st, ui);
    win.set_title(Some(&format!("{} · {}", tr("容器日志"), name)));
    show_only(ui, DetailWin::Logs);
    win.present();
    start_logs(st, ui, name);
}

/// **打开文件窗**（并列出这个容器的目录）。
fn open_files_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    *ui.selected.borrow_mut() = name.to_string();
    *ui.selected_project.borrow_mut() = String::new();
    ui.detail.proj_root.set_visible(false);
    let win = ensure_files_window(st, ui);
    win.set_title(Some(&format!("{} · {}", tr("容器文件"), name)));
    show_only(ui, DetailWin::Files);
    win.present();
    files_ensure(st, ui, name);
}

/// 容器页的**宿主窗口**（弹窗的 transient parent）。
///
/// ⚠️ **为什么不用 `st.topwin()`**（UI 审查 D20）：`topwin()` 从
///   `PanelState::root` 往上找祖先窗口 ⇒ 容器页**还没挂进窗口**时（或
///   单独放进一个窗口时）它返回 `None` ⇒ 弹窗**没有 transient parent**
///   ⇒ GTK 会警告「GtkDialog mapped without a transient parent」⇒
///   弹窗可能落在屏幕任意位置、甚至在主窗**后面**。
///   ⇒ 改成从**容器页自己**往上找 ⇒ 拿到的就是**真正显示它的那扇窗**。
fn host_window(ui: &Rc<DockerUi>, st: &Rc<PanelState>) -> Option<gtk::Window> {
    ui.page
        .ancestor(gtk::Window::static_type())
        .and_downcast::<gtk::Window>()
        .or_else(|| st.topwin())
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

/// 日志流**结束时的状态文案**（**纯函数** ⇒ 好测）。
///
/// ⚠️ **阶段 165：一条日志都没有 ⇒ 说清楚**（用户 2026-11-24：「这个日志
///   窗口我从来没看到日志」）。
///
/// ⚠️ **为什么以前那句不够**：以前只写「日志流已结束」⇒ 用户分不清是
///   「功能坏了」还是「这个容器本来就不写日志」⇒ 白白地怀疑功能。
///   ⇒ **实测**：大多数容器 `docker logs` 就是空的（实测本机 4 个容器里
///   有 2 个零输出）⇒ 这句话**很常见** ⇒ 必须说人话。
///
/// ⚠️ **退出码非 0 时不这么说**：那种情况多半是命令本身出了错（容器没了
///   / 没权限）⇒ 错误原文已经落在日志正文里了 ⇒ 只补一句退出码。
pub fn log_end_status(empty: bool, code: i32) -> String {
    if empty && code == 0 {
        tr("这个容器没有日志输出（docker logs 是空的）").to_string()
    } else if empty {
        trf!("这个容器没有日志输出（退出码 {c}）", c = code)
    } else if code == 0 {
        tr("日志流已结束").to_string()
    } else {
        trf!("日志流已结束（退出码 {c}）", c = code)
    }
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

    // ⚠️ **阶段 165：等了几秒还没输出 ⇒ 说清楚**（用户 2026-11-24：
    //   「这个日志窗口我从来没看到日志」）。
    //
    // ⚠️ **为什么要这句**：容器**运行中**时 `docker logs -f` **不会自己结束**
    //   ⇒ 状态一直停在「正在接收日志…」⇒ 而正文区**一片空白** ⇒ 用户看着
    //   像坏了。⇒ 3 秒后如果**一行都没来**，就把话说透：「这个容器暂时没有
    //   输出（它可能根本不往 stdout/stderr 写东西）」。
    {
        let u = Rc::downgrade(ui);
        glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
            let Some(u) = u.upgrade() else { return };
            // ⚠️ **已经有输出了**（或流已经停了）⇒ 不用改状态。
            if !u.log_running.get() || !u.log_lines.borrow().is_empty() {
                return;
            }
            u.log.status.set_text(tr(
                "正在接收日志…（暂时还没有输出；这个容器可能根本不写日志）",
            ));
        });
    }

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

/// 做一次**写操作**（启停 / 重启）。
///
/// ⚠️ **界面必须先二次确认**（见 `build()` 里的按钮接线）⇒ 这里只管执行。
fn do_action(st: &Rc<PanelState>, ui: &Rc<DockerUi>, action: Action, name: &str) {
    if name.is_empty() {
        return;
    }
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name2 = name.to_string();
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
        ui.set_segment_status(SEG_CONTAINERS, tr("找不到这个 compose 项目的容器"));
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

/// 容器终端**用的 shell**：自动（挨个试）还是**点名**。
///
/// ⚠️ **为什么分两种**（阶段 119）：界面上可以让用户**点名**要某个 shell
///   （比如 `/bin/bash`）⇒ 点名的那个**用不了**时得**说清楚**
///   （「这个容器里没有 /bin/bash」），**不能悄悄换一个**
///   （悄悄换 = 用户以为自己在用 bash，其实是 sh ⇒ 脚本行为不一样）。
#[derive(Clone, Debug)]
enum ShellChoice {
    /// 自动（挨个试 `/bin/sh` → `/bin/bash` → …）。
    Auto,
    /// 点名要某个 shell。
    Named(String),
}

/// 打开**容器内终端**（`docker exec -it`；阶段 113）。
///
/// ⚠️ **走共享连接**上的独立 exec 通道（不新增 SSH 连接）。
/// ⚠️ **容器里可能没有 shell** ⇒ 挨个试 `/bin/sh` → `/bin/bash` → … ⇒
///    全都没有才报错（说清楚试了哪些）。
/// ⚠️ **终端现在在容器页里**（UI 审查 D19）：以前是**另开一个窗口**
///   ⇒ 编辑器都搬进容器页了（阶段 124），终端还留在外面不一致；
///   而且那个窗口**没有 transient parent**（可能落在主窗后面）。
///   ⇒ 现在终端是详情区的**一个子页**（「终端」页签）。
/// ⚠️ **阶段 119**：终端控件**只建一次** ⇒ 「重连 / 换 shell」
///   **复用**它（不重建控件 ⇒ 滚动缓冲也还在）。
fn open_container_terminal(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    // 已经在开着 ⇒ 别开第二个（远端会多一个孤儿 shell）。
    if ui.term_running.get() && ui.term_name.borrow().as_str() == name {
        // ⚠️ **已经在看着它** ⇒ 只需把窗口拉到前面。
        if let Some(w) = ui.term_window.borrow().as_ref() {
            w.present();
        }
        return;
    }
    // ⚠️ **换容器了** ⇒ 旧的会话要收掉（不然容器里留着孤儿 shell）
    //   ⇒ 但**窗口与终端控件留着**（`start_term_session` 自己会停旧通道）
    //   ⇒ 用户看到的是同一个窗口里换了个容器。
    if ui.term_running.get() {
        close_term_channel(ui);
    }
    *ui.term_name.borrow_mut() = name.to_string();
    ensure_term_window(st, ui, name);
    // ⚠️ **窗口标题跟着容器名走**（关掉窗口后重开也是对的）。
    if let Some(w) = ui.term_window.borrow().as_ref() {
        w.set_title(Some(&format!("{} · {}", tr("进入容器"), name)));
        w.present();
    }
    start_term_session(st, ui, name, ShellChoice::Auto);
}

/// **建容器终端的窗口**（第一次打开时建；以后**复用**）。
///
/// ⚠️ **为什么复用**：重连 / 换 shell 时**不该**重建终端控件（用户以为
///   界面出问题了，而且滚动缓冲会丢）⇒ 工具条、搜索条、终端控件**都留着**。
fn ensure_term_window(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    // 建过了 ⇒ 复用。
    if ui.term_window_ready.get() {
        return;
    }
    ui.term_window_ready.set(true);
    // ⚠️ **旧控件已经销毁了**（`stop_terminal` 清过）⇒ 句柄**全丢掉**，
    //    别攥着一个已销毁的控件（再用会炸）。
    ui.term_view.borrow_mut().take();
    *ui.term_bar.borrow_mut() = None;
    *ui.term_shell_combo.borrow_mut() = None;
    *ui.term_search_bar.borrow_mut() = None;
    *ui.term_find_entry.borrow_mut() = None;
    ui.term_shell_items.borrow_mut().clear();
    *ui.term_status.borrow_mut() = None;
    *ui.term_reconnect_btn.borrow_mut() = None;
    *ui.term_clear_btn.borrow_mut() = None;
    *ui.term_search_btn.borrow_mut() = None;

    // ⚠️ **后端跟主终端一致**（用户在首选项里选了 ghostty ⇒ 容器终端也
    //    是 ghostty）⇒ 行为统一。
    let term = crate::term::new_term_backend();
    let view = term.widget().clone();
    term.set_expand(true, true);

    let status = gtk::Label::new(Some(tr("正在打开容器终端…")));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_margin_start(6);
    status.set_margin_end(6);
    status.set_margin_bottom(4);
    status.add_css_class("dim-label");
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);

    // ---- 工具条（阶段 119：切 shell / 重连 / 清屏 / 搜索）----
    let shell_lbl = gtk::Label::new(Some(tr("Shell")));
    let shell_combo = gtk::ComboBoxText::new();
    // ⚠️ **第 0 项是「自动」**（挨个试）⇒ 默认行为跟以前一模一样。
    let mut shell_items: Vec<String> = Vec::new();
    shell_combo.append_text(tr("自动"));
    shell_items.push(tr("自动").to_string());
    for sh in docker::SHELL_CANDIDATES {
        shell_combo.append_text(sh);
        shell_items.push(sh.to_string());
    }
    shell_combo.set_active(Some(0));

    let reconnect_btn = gtk::Button::with_label(tr("重连"));
    reconnect_btn.set_tooltip_text(Some(tr("断开当前会话，用上面选的 shell 重新进入容器")));
    let clear_btn = gtk::Button::with_label(tr("清屏"));
    clear_btn.set_tooltip_text(Some(tr("清掉屏幕上的输出")));
    let search_btn = gtk::ToggleButton::with_label(tr("搜索"));
    search_btn.set_tooltip_text(Some(tr("在容器终端的输出里查找")));
    // ⚠️ **「关闭」**：终端现在在页面里 ⇒ 没有窗口的 ✕ 了 ⇒ 给一颗
    //   「关闭终端」（收掉远端那条 `docker exec` ＋ 回概览页）。
    let close_btn = gtk::Button::with_label(tr("关闭终端"));
    close_btn.set_tooltip_text(Some(tr("关掉这个容器里的 shell（回到概览）")));
    close_btn.add_css_class("flat");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(2);
    bar.append(&shell_lbl);
    bar.append(&shell_combo);
    bar.append(&reconnect_btn);
    bar.append(&clear_btn);
    bar.append(&search_btn);
    bar.append(&close_btn);
    // ⚠️ **状态标签占满剩下的宽度**（终端名 + 当前状态）。
    bar.append(&status);

    // ---- 搜索条（阶段 119）----
    //
    // ⚠️ 用的是**共用构件** `findbar`（跟主终端、编辑器同一套外观与按键
    //    约定：Enter 下一个 / Shift+Enter 上一个 / Esc 关闭）。
    let fb = crate::findbar::build(crate::findbar::FindOpts::minimal());
    let search_entry = fb.find_entry.clone();
    search_entry.set_placeholder_text(Some(tr("在容器终端中查找…")));
    {
        let term2 = term.clone();
        search_entry.connect_changed(move |e| {
            // ⚠️ 传**普通文本**（不是正则）：转义由后端做。
            term2.search_set_pattern(Some(&e.text()));
        });
    }
    {
        let term2 = term.clone();
        search_entry.connect_activate(move |_| {
            if !term2.search_find_next() {
                // 单结果时 next 失败再回绕上一个
                term2.search_find_previous();
            }
        });
    }
    {
        let term2 = term.clone();
        fb.prev_btn.connect_clicked(move |_| {
            term2.search_find_previous();
        });
        let term2 = term.clone();
        fb.next_btn.connect_clicked(move |_| {
            if !term2.search_find_next() {
                term2.search_find_previous();
            }
        });
    }
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&fb.root));
    search_bar.set_show_close_button(true);
    search_bar.connect_entry(&search_entry);

    // 搜索条开关 ⇄ 工具条上的「搜索」按钮（两边状态要一致）。
    {
        let sb = search_bar.clone();
        search_btn.connect_toggled(move |b| {
            sb.set_search_mode(b.is_active());
        });
    }
    {
        let tb = search_btn.clone();
        search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            // 搜索条自带的 ✕ 关掉时 ⇒ 按钮也要跟着取消（不然按钮还亮着）。
            if !bar.is_search_mode() && tb.is_active() {
                tb.set_active(false);
            }
        });
    }
    {
        // Esc 关闭后焦点回终端（不然键盘输入还落在搜索框里）。
        let term2 = term.clone();
        search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
            if !bar.is_search_mode() {
                term2.grab_focus();
            }
        });
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&bar);
    vbox.append(&search_bar);
    vbox.append(&view);
    // ⚠️ **装进独立窗口**（用户 2026-11-23：弹一个 term 窗口更合适）。
    let win = gtk::Window::new();
    win.set_title(Some(&format!("{} · {}", tr("进入容器"), name)));
    win.set_default_size(900, 560);
    // ⚠️ **必须挂到主窗口下面**（transient parent）：UI 审查 D19 提过的
    //   老坑 —— 没有它，这个窗口**可能落在主窗后面** ⇒ 用户还以为
    //   没弹出来 ⇒ 还要永远置顶（GTK 会把它钉在主窗之上）。
    if let Some(parent) = crate::windows::parent_of(None) {
        win.set_transient_for(Some(&parent));
    }
    win.set_child(Some(&vbox));
    // ⚠️ **用户点 ✕ 关窗口 ⇒ 也要把远端那条 `docker exec` 收掉**
    //   （不然容器里留着孤儿 shell）⇒ 走 `teardown_terminal`。
    {
        let u = Rc::downgrade(ui);
        win.connect_close_request(move |_| {
            if let Some(u) = u.upgrade() {
                teardown_terminal(&u);
            }
            // `Proceed` = 继续关闭（`Stop` 才是拦住）。
            glib::Propagation::Proceed
        });
    }
    win.present();
    // ⚠️ **焦点给终端**（不然键盘还落在别的控件上 ⇒ 打不进字）。
    view.grab_focus();
    *ui.term_window.borrow_mut() = Some(win);
    *ui.term_bar.borrow_mut() = Some(bar);
    *ui.term_shell_combo.borrow_mut() = Some(shell_combo.clone());
    *ui.term_shell_items.borrow_mut() = shell_items;
    *ui.term_search_bar.borrow_mut() = Some(search_bar.clone());
    *ui.term_find_entry.borrow_mut() = Some(search_entry.clone());
    *ui.term_status.borrow_mut() = Some(status.clone());
    *ui.term_reconnect_btn.borrow_mut() = Some(reconnect_btn.clone());
    *ui.term_clear_btn.borrow_mut() = Some(clear_btn.clone());
    *ui.term_search_btn.borrow_mut() = Some(search_btn.clone());
    *ui.term_view.borrow_mut() = Some(term);

    // ⚠️ **「关闭终端」按钮** ⇒ 收掉远端那条 `docker exec` ＋ 回概览页。
    {
        let u = Rc::downgrade(ui);
        close_btn.connect_clicked(move |_| {
            let Some(u) = u.upgrade() else { return };
            // ⚠️ **不再切页**：终端在**独立窗口**里（阶段 152）⇒ 详情区
            //   停在哪一页跟它无关。
            stop_terminal(&u);
        });
    }

    // ---- 工具条上的三个动作 ----
    {
        let u = Rc::downgrade(ui);
        shell_combo.connect_changed(move |c| {
            let Some(u) = u.upgrade() else { return };
            let idx = c.active().unwrap_or(0);
            if idx == 0 {
                set_term_status(&u, tr("Shell：自动（挨个试 /bin/sh、/bin/bash…）"));
            } else if let Some(sh) = docker::SHELL_CANDIDATES.get(idx as usize - 1) {
                set_term_status(
                    &u,
                    &trf!("Shell：{s}（点「重连」生效）", s = *sh),
                );
            }
        });
    }
    {
        let stw = Rc::downgrade(st);
        let uw = Rc::downgrade(ui);
        reconnect_btn.connect_clicked(move |_| {
            let (Some(st), Some(u)) = (stw.upgrade(), uw.upgrade()) else { return };
            reconnect_terminal(&st, &u);
        });
    }
    {
        let uw = Rc::downgrade(ui);
        clear_btn.connect_clicked(move |_| {
            let Some(u) = uw.upgrade() else { return };
            clear_term_screen(&u);
        });
    }

    // ⚠️ **尺寸同步不用自己写**：两个后端都自带 ——
    //    VTE 后端每 300ms 看一眼行列数（变了才发 window-change），
    //    ghostty 后端在 `set_term_size` 里发（内核与 pty 一起）。
}

/// 起（重起）一条**容器内终端**通道。
///
/// ⚠️ **阶段 119 的顺序很重要**：**先把 shell 探出来**（探不到就报错，
///    **旧的那条会话不受影响**）⇒ 再开新通道 ⇒ 再关旧通道。
///    ⇒ 用户点错了 shell（容器里没有）⇒ 正在用的终端**不会被弄死**。
fn start_term_session(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    name: &str,
    choice: ShellChoice,
) {
    // ⚠️ **请求序号**（每次请求都加）：用来认出「这个请求已经过期了」
    //   （用户连点两次「重连」⇒ 后一个说了算 ⇒ 前一个的通道要关掉）。
    //   ⚠️ **会话代号 `term_gen` 这里不动** —— 它是「已经挂上去的会话」的
    //   编号，只有**真的换会话**时才加 ⇒ 探 shell 失败时旧会话照旧显示。
    let req = ui.term_req.get() + 1;
    ui.term_req.set(req);

    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let name_bg = name.clone();
    // 事件通道：后台 → 主线程（终端控件**只能**在主线程上碰）。
    let (ev_tx, ev_rx) = async_channel::unbounded::<ShellEvent>();
    let (res_tx, res_rx) = async_channel::bounded::<
        Result<(crate::backend::shell::ShellChannel, String), String>,
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
        // ⚠️ **选 shell**：自动 ⇒ 挨个试；点名 ⇒ 只探那一个（用不了就报错）。
        let shell = match choice {
            ShellChoice::Auto => docker::find_shell(&session, &name_bg).await.map_err(|e| e.0),
            ShellChoice::Named(sh) => {
                docker::verify_shell(&session, &name_bg, &sh).await.map_err(|e| e.0)
            }
        };
        let shell = match shell {
            Ok(s) => s,
            Err(e) => {
                let _ = res_tx.send_blocking(Err(e));
                return;
            }
        };
        let ch = match docker::open_exec_shell(&session, &name_bg, &shell, 80, 24).await {
            Ok(c) => c,
            Err(e) => {
                let _ = res_tx.send_blocking(Err(e.0));
                return;
            }
        };
        let _ = res_tx.send_blocking(Ok((ch.clone(), shell)));
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
        glib::MainContext::default().spawn_local(async move {
            match res_rx.recv().await {
                Ok(Ok((chan, shell))) => {
                    let Some(u) = u.upgrade() else { return };
                    // ⚠️ **过期了**（用户连点了两次「重连」）⇒ 把这条通道
                    //    关掉（别在容器里留一个孤儿 shell）⇒ 忽略。
                    if u.term_req.get() != req {
                        chan.close();
                        return;
                    }
                    // ⚠️ **旧的那条先关掉**（重连 ⇒ 容器里那个 shell 退出）。
                    let was_running = u.term_running.get();
                    close_term_channel(&u);
                    let Some(view) = u.term_view.borrow().clone() else {
                        // 没有控件（窗口已经被关了）⇒ 关掉这条通道。
                        chan.close();
                        return;
                    };
                    // ⚠️ **会话代号**（**真的换会话了**才加）⇒ 旧会话残留的
                    //    数据从这一刻起全部丢掉（见 `term_gen` 的说明）。
                    let generation = u.term_gen.get() + 1;
                    u.term_gen.set(generation);
                    // ⚠️ **清屏**：旧会话的输出对新会话**没有意义**
                    //    （留着会让人以为「刚才那些命令是在新会话里跑的」）。
                    view.reset(true, true);
                    // 事件转发（**带代次检查** ⇒ 旧会话残留的数据丢掉）。
                    let (out_tx, out_rx) = async_channel::unbounded::<ShellEvent>();
                    {
                        let uw = Rc::downgrade(&u);
                        glib::MainContext::default().spawn_local(async move {
                            while let Ok(ev) = ev_rx.recv().await {
                                let Some(uu) = uw.upgrade() else { break };
                                // 过期的一代 ⇒ 丢弃（并退出这条转发）。
                                if uu.term_gen.get() != generation {
                                    break;
                                }
                                if out_tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    let status2 = u.term_status.borrow().clone();
                    let ok = view.attach_shell(
                        chan.clone(),
                        out_rx,
                        Box::new(move |code| {
                            let msg = if code == 0 {
                                tr("容器终端已退出").to_string()
                            } else {
                                trf!("容器终端已退出（退出码 {c}）", c = code)
                            };
                            if let Some(s) = status2.as_ref() {
                                s.set_text(&msg);
                            }
                        }),
                    );
                    if !ok {
                        if let Some(s) = u.term_status.borrow().as_ref() {
                            s.set_text(tr("当前终端后端不支持容器终端"));
                        }
                        return;
                    }
                    *u.term_chan.borrow_mut() = Some(chan);
                    u.term_running.set(true);
                    *u.term_shell.borrow_mut() = shell.clone();
                    *u.term_name.borrow_mut() = name.clone();
                    if was_running {
                        u.term_reconnects.set(u.term_reconnects.get() + 1);
                    }
                    // ⚠️ **标题带上 shell**（一眼看得出现在用的是哪个）。
                    //   ⚠️ 终端现在是**页面**（UI 审查 D19）⇒ 没有窗口标题
                    //   了 ⇒ 标题信息落到**状态行**上（见下面的 `set_term_status`）。
                    set_term_status(
                        &u,
                        &trf!(
                            "容器终端：{n}（shell：{s}，输入 exit 可以退出）",
                            n = name,
                            s = shell
                        ),
                    );
                }
                Ok(Err(msg)) => {
                    let Some(u) = u.upgrade() else { return };
                    // ⚠️ **失败时旧会话还在**（探 shell 失败发生在关旧通道
                    //    之前）⇒ 用户的终端**没被弄死**。
                    set_term_status(&u, &msg);
                }
                Err(_) => {}
            }
        });
    }
}

/// **只关通道**（**不动窗口 / 控件**）—— 重连、换 shell 时用。
fn close_term_channel(ui: &Rc<DockerUi>) {
    if let Some(ch) = ui.term_chan.borrow_mut().take() {
        ch.close();
    }
    ui.term_running.set(false);
}

/// **重连**：断开当前会话，用下拉里选的 shell 重新进去。
fn reconnect_terminal(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    let name = ui.term_name.borrow().clone();
    if name.is_empty() {
        set_term_status(ui, tr("容器终端还没打开"));
        return;
    }
    let choice = current_shell_choice(ui);
    set_term_status(ui, tr("正在重新连接…"));
    start_term_session(st, ui, &name, choice);
}

/// 工具条下拉当前选的 shell（0 = 自动）。
fn current_shell_choice(ui: &Rc<DockerUi>) -> ShellChoice {
    let combo = ui.term_shell_combo.borrow();
    let Some(c) = combo.as_ref() else {
        return ShellChoice::Auto;
    };
    let idx = c.active().unwrap_or(0) as usize;
    if idx == 0 {
        return ShellChoice::Auto;
    }
    match docker::SHELL_CANDIDATES.get(idx - 1) {
        Some(s) => ShellChoice::Named((*s).to_string()),
        None => ShellChoice::Auto,
    }
}

/// **清屏**：把终端画面擦干净（不动会话）。
fn clear_term_screen(ui: &Rc<DockerUi>) {
    let view = ui.term_view.borrow();
    if let Some(t) = view.as_ref() {
        // ⚠️ **软复位 + 清屏**（`hard=false`）⇒ 只是擦画面，
        //    不用把整台终端重新初始化。
        t.reset(false, true);
    }
}

/// 改容器终端的状态文字（**没开窗口时什么都不做**）。
fn set_term_status(ui: &Rc<DockerUi>, text: &str) {
    if let Some(s) = ui.term_status.borrow().as_ref() {
        s.set_text(text);
    }
}

/// 停掉**容器内终端**（关通道 ⇒ 容器里那个 shell 退出）。
fn teardown_terminal(ui: &Rc<DockerUi>) {
    close_term_channel(ui);
    // ⚠️ **代次 +1** ⇒ 旧会话残留的事件全部丢掉（别再喂进终端控件）。
    ui.term_gen.set(ui.term_gen.get() + 1);
    // ⚠️ **请求序号 +1** ⇒ 还在路上的请求作废（它到了也没地方挂）。
    ui.term_req.set(ui.term_req.get() + 1);
    // ⚠️ **控件句柄清掉**：会话已经断了 ⇒ 那个控件没有后端了 ⇒
    //    攥着它再用会炸。
    ui.term_view.borrow_mut().take();
    // ⚠️ **窗口先从字段里摘掉，再关** ⇒ `close-request` 里再进一次
    //   `teardown_terminal` 时不会重复关（也就不会递归下去）。
    let win = ui.term_window.borrow_mut().take();
    ui.term_window_ready.set(false);
    if let Some(w) = win {
        w.close();
    }
    ui.term_running.set(false);
}

/// **关掉容器内终端**（「关闭终端」按钮 / 页签关闭钩子用）。
///
/// ⚠️ **为什么拆成两个函数**：`stop_terminal` 有**重入保护**（正在关就直接
///   返回），而**窗口的 `close-request` 回调**里也要做同一件事 ⇒ 如果
///   close-request 也去调 `stop_terminal`，就会因为「正在关」这个标志而
///   **啥也不做** ⇒ 通道不会停 ⇒ 容器里留一个孤儿 shell。
///   ⇒ 拆出 `teardown_terminal`（**不看重入标志**）⇒ 两边都调它。
fn stop_terminal(ui: &Rc<DockerUi>) {
    // ⚠️ **重入保护**：「关闭终端」按钮与页签关闭钩子都会调这里 ⇒
    //   不挡一下就是「自己调自己」。
    if ui.term_closing.get() {
        return;
    }
    ui.term_closing.set(true);
    teardown_terminal(ui);
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
    open_compose_file_at(st, ui, &path, &sum.dir);
}

/// 「编辑 compose 文件」的**真动作**（状态栏 + 打开编辑器）。
///
/// ⚠️ **状态栏开完就清**（用户 2026-11-27：「状态栏会显示『正在打开 xxx
///   文件』，然后打开了编辑器，但是编辑器关掉之后，状态栏的文字还在，
///   这不合理，应该是编辑器只要打开，状态栏文字就可以消失了」）。
fn open_compose_file_at(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    path: &str,
    work_dir: &str,
) -> String {
    open_compose_file_at_with(st, ui, path, work_dir, open_compose_path)
}

/// 同 [`open_compose_file_at`]，但**「打开编辑器」那一步可以换掉**。
///
/// ⚠️ **为什么留这个口子**（冒烟要能进 CI）：
///   「打开编辑器」那步在**没连上服务器**时会弹一个**模态**错误框 ⇒ 冒烟
///   会卡在那儿等人点 ⇒ CI 上没法跑。
///   ⇒ 把那一步做成**可注入**的 ⇒ 冒烟注入一个「什么都不干」的开法 ⇒
///   验的**仍然是真函数**里「写状态 → 开 → 清状态」这三步（不是把逻辑
///   抄一份到测试里）。
///
/// ⚠️ **返回刚才写的那句**（"正在打开 …"）⇒ 冒烟能证明「**确实写过**
///   又**确实清掉了**」（不然「从一开始就是空的」也会让断言变绿）。
fn open_compose_file_at_with(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    path: &str,
    work_dir: &str,
    mut opener: impl FnMut(&Rc<PanelState>, &str, &str),
) -> String {
    let msg = trf!("正在打开 {p}…", p = path);
    // ⚠️ **写进「容器」分段的状态**（底部那行只显示当前分段的）⇒ 用
    //   `set_segment_status`（它顺手把分段里的那份也记上）⇒ 切分段回来
    //   不会又冒出这句过期的话。
    ui.set_segment_status(SEG_CONTAINERS, &msg);
    opener(st, path, work_dir);
    // ⚠️ **编辑器已经打开了** ⇒ 这句「正在打开…」就是过期信息了 ⇒ 清掉
    //   （空标签 GTK 会自己收起来 ⇒ 状态栏不会留一条空白行）。
    ui.set_segment_status(SEG_CONTAINERS, "");
    msg
}

/// 打开一个 compose 配置文件（阶段 138）。
///
/// 干什么用：① 在**主栏页签**里开这个文件（与其它文件一样）；② **把文件
///   面板跳到它所在的项目目录**并**选中它**（用户 2026-11-19：「sftp 最好
///   能跳转到 docker-compose.yml 文件所在目录」）。
///
/// ⚠️ **为什么抽成独立函数**：冒烟要能走**同一条路**验这件事 ⇒ 如果把逻辑
///   留在 `open_compose_file` 里，测试只能去点按钮（要真 docker + 真容器
///   标签 ⇒ 测不了）；复制一份逻辑到测试里 ⇒ **那种测试是假的**
///   （把真路径删了测试照样绿）。
///
/// `work_dir` = compose 的**工作目录**（容器标签
/// `com.docker.compose.project.working_dir` ⇒ 那**就是**
/// docker-compose.yml 所在目录）；取不到才退回从文件路径推。
pub fn open_compose_path(st: &Rc<PanelState>, path: &str, work_dir: &str) {
    // ⚠️ **阶段 138：编辑器默认停「主栏页签」**（与终端并列）⇒ **不要**再
    //   把底部编辑区搬到容器页。
    //
    //   那是**阶段 113 的老做法**（那时编辑器住在编辑区里 ⇒ 编辑区得跟着
    //   当前页走）；阶段 137 之后编辑器是**自己的页签** ⇒ 这时候再搬编辑
    //   区，只会把**终端页的编辑区**抢走（用户没要求过的副作用）。
    //
    // ⚠️ **先把文件面板跳到 compose 所在目录** ⇒ 编辑器左边就是**项目文件
    //   树**（docker-compose.yml / .env / 其它配置）⇒ 点一下就能开兄弟
    //   文件；再把那个文件**选中** ⇒ 一眼看到「编辑的就是这一个」。
    let dir = if work_dir.is_empty() {
        crate::util::path_parent(path)
    } else {
        work_dir.to_string()
    };
    let file_name = crate::util::path_basename(path);
    if !dir.is_empty() {
        crate::panel::goto_remote_dir(st, &dir, Some(&file_name));
    }
    // ⚠️ **远程文件** ⇒ 走 SFTP 打开（另开一个编辑器页签）。
    crate::panel::fileedit::open_remote(st, path, false);
}

/// 触发一次刷新。
///
/// ⚠️ **后台任务里不许碰 GTK** ⇒ 只把「连接参数 + 密码」带进去，
/// 结果回来再回主线程更新界面。
fn refresh(st: &Rc<PanelState>, ui: &Rc<DockerUi>) {
    if ui.busy.get() {
        // ⚠️ **阶段 161：记一笔「待会儿再刷」**——上一次还没回来时，
        //   **写操作之后的那次刷新会被丢掉** ⇒ 表里还是旧数据
        //   （用户看着像「删了没删」）。
        ui.busy_pending.set(true);
        return; // 上一次还没回来 ⇒ 不叠加
    }
    ui.busy_pending.set(false);
    ui.busy.set(true);
    // ⚠️ **loading 动画**（用户 2026-11-16）：开始取了 ⇒ 转起来。
    ui.set_loading(true);
    ui.set_segment_status(SEG_CONTAINERS, tr("正在读取远程容器…"));

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
        // 关掉这个页签 ⇒ 停掉自动刷新（否则定时器还在跑，白费连接）。
        let u = Rc::downgrade(&ui);
        sub.add_close_hook(move |cid| {
            let Some(u) = u.upgrade() else { return };
            if cid == u.tab_id.get() {
                stop_auto(&u);
                // ⚠️ **日志流也得停**（否则远端的 `docker logs -f` 一直挂着）。
                stop_logs(&u);
                // ⚠️ **容器内终端也得停**（否则容器里留着孤儿 shell）。
                stop_terminal(&u);
                // ⚠️ **详情弹窗也得关**（否则关掉页签后屏幕上还留着一扇窗）。
                close_detail_window(&u);
            }
        });
    }
    if let Some(num) = sub.nb.page_num(&ui.page) {
        sub.nb.set_current_page(Some(num));
    }
    // ⚠️ **阶段 151：不再有「编辑区」** ⇒ 不用把编辑区搬到容器页。
    //   容器页的 compose 文件**在格子页签栏里打开**（阶段 138 起就是这么
    //   做的 ⇒ `open_compose_path` 走 `fileedit::open_remote`）⇒ 用户
    //   点完就看到编辑器，**不需要**「搬到容器页下面」这套。
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

/// 测试钩子：表格的**当前可见列标题**（窄窗口下次要列会让位）。
pub fn column_titles_for_test(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    for i in 0..ui.view.n_columns() {
        if let Some(c) = ui.view.column(i as i32) {
            if c.is_visible() {
                out.push(c.title().to_string());
            }
        }
    }
    out
}

/// 测试钩子：当前响应式档位与布局暂停状态。
pub fn column_layout_for_test(st: &Rc<PanelState>) -> (&'static str, bool) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return ("none", false) };
    let name = match ui.column_density.get() {
        ColumnDensity::Wide => "wide",
        ColumnDensity::Medium => "medium",
        ColumnDensity::Narrow => "narrow",
        ColumnDensity::Compact => "compact",
    };
    (
        name,
        ui.column_resize_active.get()
            || ui
                .row_menu
                .borrow()
                .as_ref()
                .is_some_and(|menu| menu.is_visible()),
    )
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
    // ⚠️ **空态也得对上**（UI 审查 D6）⇒ 灌一份空数据进去也该看到空态。
    ui.sync_empty_state(rows.is_empty());
    // ⚠️ **loading 动画停掉**（测试里不连服务器 ⇒ 没有「正在加载」这回事）。
    ui.set_loading(false);
}

/// 测试钩子：**按某一列排序**（UI 审查 D4）⇒ 返回排序后的**顶层行名字**。
///
/// ⚠️ **为什么要有这个钩子**：排序是「点列头」触发的 ⇒ 冒烟点不到列头
///   ⇒ 得能**直接**把模型排一遍 ⇒ 才能验「CPU 列真的按**数值**排」
///   （`9.5%` 必须排在 `50.8%` 前面 ⇒ 字符串排序会看错）。
pub fn sort_by_for_test(st: &Rc<PanelState>, col: usize, asc: bool) -> Vec<String> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.model.set_sort_column_id(
        gtk::SortColumn::Index(col as u32),
        if asc {
            gtk::SortType::Ascending
        } else {
            gtk::SortType::Descending
        },
    );
    let mut out = Vec::new();
    if let Some(mut it) = ui.model.iter_first() {
        loop {
            let name: String = ui.model.get(&it, COL_NAME as i32);
            out.push(name);
            if !ui.model.iter_next(&mut it) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：**空态 / 错态**是不是显示着（UI 审查 D6）。
pub fn empty_state_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.empty.root.is_visible()
}

/// 测试钩子：**空态的标题**（错态分类要看它）。
pub fn empty_state_title_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.empty.title.text().to_string()
}

/// 测试钩子：让某个分段进入**错态**（**不连服务器**）。
///
/// ⚠️ **为什么要有**：镜像 / 卷 / 网络三页以前失败时**只写底部一行小灰字**
///   ⇒ 用户根本看不出「是没权限」⇒ 这个钩子让冒烟能**不连服务器**就
///   验「错态是不是真的显示出来了、标题对不对」（阶段 135）。
///
/// ⚠️ **必须走真路径那条**（[`DockerUi::seg_failed`]）⇒ 否则钩子自己
///   实现一遍 ⇒ 把真路径删了测试照样绿 ⇒ 等于没测。
///
/// `seg` 取 `SEG_IMAGES` / `SEG_VOLUMES` / `SEG_NETWORKS` / `SEG_CONTAINERS`。
pub fn set_seg_error_for_test(st: &Rc<PanelState>, seg: &str, msg: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    match seg {
        SEG_IMAGES => ui.seg_failed(Some(st), SEG_IMAGES, msg),
        SEG_VOLUMES => ui.seg_failed(Some(st), SEG_VOLUMES, msg),
        SEG_NETWORKS => ui.seg_failed(Some(st), SEG_NETWORKS, msg),
        _ => ui.containers_failed(Some(st), msg),
    }
}

/// 测试钩子：某个分段的**错态**是不是显示着。
pub fn seg_empty_visible_for_test(st: &Rc<PanelState>, seg: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    ui.seg_empty(seg).root.is_visible()
}

/// 测试钩子：某个分段**错态的标题**。
pub fn seg_empty_title_for_test(st: &Rc<PanelState>, seg: &str) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.seg_empty(seg).title.text().to_string()
}

/// 测试钩子：「没权限」的提示**弹过了没有**（每个连接只弹一次）。
pub fn denied_notified_for_test(st: &Rc<PanelState>) -> bool {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.denied_notified.get())
        .unwrap_or(false)
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

/// 测试钩子：详情**弹窗**是不是开着（阶段 155：详情搬进弹窗了）。
///
/// ⚠️ **看的是「弹窗在不在屏幕上」**（`is_visible`）⇒ 不是「控件在不在」
///   ⇒ 控件一直都在（弹窗是复用的）⇒ 拿控件判断等于什么都没断言。
pub fn detail_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .and_then(|u| u.detail_window.borrow().as_ref().cloned())
        .map(|w| w.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：**详情弹窗**的标题（阶段 155）。
pub fn detail_window_title_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .and_then(|u| u.detail_window.borrow().as_ref().cloned())
        .and_then(|w| w.title())
        .unwrap_or_default()
        .to_string()
}

/// 测试钩子：容器详情的**内容块**是不是显示着（项目行时应当是收起的）。
pub fn detail_body_visible_for_test(st: &Rc<PanelState>) -> bool {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail.root.property::<bool>("visible"))
        .unwrap_or(false)
}

/// 测试钩子：详情标题文字（「详情：web」）。
pub fn detail_title_for_test(st: &Rc<PanelState>) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail.title.text().to_string())
        .unwrap_or_default()
}

/// 测试钩子：**按 widget 坐标**命中「操作」列 ⇒ 命中的动作短名（阶段 156）。
///
/// ⚠️ **为什么要有它**（阶段 156）：`dispatch_action_for_test` /
///   `click_row_action_for_test` 都**直接构造命中结果** ⇒ **完全绕开坐标**
///   ⇒ 「点项目行的『编辑』却弹出 term 窗口」（命中**差一行**）这种 bug
///   **测不出来**。
///   ⇒ 这个钩子走的是**真·命中判定那条路**（含 widget → bin window 换算）。
///
/// ⚠️ 坐标是 **widget 坐标**（跟手势给的一样）。
pub fn hit_action_at_for_test(st: &Rc<PanelState>, x: i32, y: i32) -> Option<String> {
    let slot = st.docker_ui.borrow();
    let ui = slot.as_ref()?;
    let hit = hit_action_column(ui, x, y)?;
    Some(action_key(hit.action.kind).to_string())
}

/// 测试钩子：悬停「操作」列 ⇒ **tooltip 文案**（阶段 157）。
///
/// ⚠️ **用户 2026-11-23 提的**：「按钮增加 tooltip」。
///   ⇒ 这里走的是**跟真·tooltip 处理器同一条路**（`hit_action_column` ＋
///   `action_tooltip`）⇒ 「命中差一行」/「文案对不上」都能测出来。
///
/// ⚠️ 坐标是 **widget 坐标**（跟手势给的一样）。
pub fn action_tooltip_at_for_test(st: &Rc<PanelState>, x: i32, y: i32) -> Option<String> {
    let slot = st.docker_ui.borrow();
    let ui = slot.as_ref()?;
    let hit = hit_action_column(ui, x, y)?;
    Some(
        action_tooltip(
            &hit.action,
            hit.ctx.kind == KIND_PROJECT,
            &hit.ctx.raw,
        )
        .to_string(),
    )
}

/// 测试钩子：详情弹窗里**「关闭」按钮**的个数（阶段 156：容器 / 项目各一个）。
pub fn detail_close_button_count_for_test(st: &Rc<PanelState>) -> usize {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail_close_btns.borrow().len())
        .unwrap_or(0)
}

/// 测试钩子：**点一下**详情弹窗里的「关闭」按钮（阶段 156）。
///
/// ⚠️ 走的是**按钮自己的点击处理**（不是直接藏窗口）⇒ 跟用户真点一下是
///   同一条路。
pub fn close_detail_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let btns = ui.detail_close_btns.borrow();
    if let Some(b) = btns.first() {
        b.emit_clicked();
    }
}

/// 测试钩子：**概览窗**开着吗（阶段 165：三扇窗之一）。
pub fn overview_window_open_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .and_then(|u| u.detail_window.borrow().as_ref().cloned())
        .map(|w| w.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：**日志窗**开着吗（阶段 165：三扇窗之一）。
pub fn logs_window_open_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .and_then(|u| u.logs_window.borrow().as_ref().cloned())
        .map(|w| w.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：**文件窗**开着吗（阶段 165：三扇窗之一）。
pub fn files_window_open_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .and_then(|u| u.files_window.borrow().as_ref().cloned())
        .map(|w| w.is_visible())
        .unwrap_or(false)
}

/// 测试钩子：详情弹窗**还活着**吗（没被销毁）（阶段 156）。
///
/// ⚠️ 验的是「句柄还在」⇒ 抓「关一次就把共享内容销毁了」那种回归。
pub fn detail_window_alive_for_test(st: &Rc<PanelState>) -> bool {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| u.detail_window.borrow().is_some())
        .unwrap_or(false)
}

/// 测试钩子：详情**内容**还挂在弹窗里吗（阶段 156）。
///
/// ⚠️ **这是「内容有没有被销毁」最直接的判据**：GTK4 销毁窗口时会把子控件
///   **解除父子关系**（`parent()` 变 `None`）⇒ 内容还挂在窗上 ⇒ 没被销毁。
pub fn detail_content_attached_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(u) = slot.as_ref() else { return false };
    let win = u.detail_window.borrow();
    match win.as_ref() {
        Some(w) => u.detail_box.is_ancestor(w),
        None => false,
    }
}

/// 测试钩子：详情区（页签栈）**在屏幕上吗**（阶段 165）。
///
/// ⚠️ **为什么要有这个判据**（阶段 165 踩出来的）：`is_visible()` 只看控件
///   自己的「可见」标记 ⇒ **祖先被藏起来时它照样返回 true** ⇒ 光看它抓不到
///   「整块内容其实没画出来」那种 bug ⇒ 得看 `is_mapped()`（**真的映射到
///   屏幕上了**）。
pub fn detail_stack_mapped_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    // ⚠️ **阶段 165**：概览窗里没有页签栈了 ⇒ 改看**概览内容盒**。
    slot.as_ref().map(|u| u.detail_box.is_mapped()).unwrap_or(false)
}

/// 测试钩子：日志正文**在屏幕上吗**（阶段 165）。
pub fn log_view_mapped_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log.view.is_mapped()).unwrap_or(false)
}

/// 测试钩子：日志正文**自己的可见标记**（不查祖先）。
pub fn log_view_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log.view.is_visible()).unwrap_or(false)
}

/// 测试钩子：详情区外框（装着页签栈的那个盒子）**可见吗**（阶段 165）。
pub fn detail_box_mapped_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.detail.root.is_mapped()).unwrap_or(false)
}

/// 测试钩子：**模拟点窗口的 X**（走 `close-request` 那条路）（阶段 156）。
pub fn request_close_detail_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    if let Some(w) = ui.detail_window.borrow().as_ref() {
        w.close();
    }
}

/// 测试钩子：详情头的**状态药丸**（文字 + 档位类）。
pub fn detail_pill_for_test(st: &Rc<PanelState>) -> (String, Vec<String>) {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| {
            let classes: Vec<String> = u
                .detail
                .pill
                .css_classes()
                .iter()
                .map(|s| s.to_string())
                .collect();
            (u.detail.pill.text().to_string(), classes)
        })
        .unwrap_or_default()
}

/// 测试钩子：某张卡片的文字（`config` / `net` / `mount` / `env`）。
pub fn detail_card_for_test(st: &Rc<PanelState>, which: &str) -> String {
    st.docker_ui
        .borrow()
        .as_ref()
        .map(|u| match which {
            "config" => grid_kv_text(&u.detail.config),
            "net" => grid_kv_text(&u.detail.net),
            "mount" => grid_kv_text(&u.detail.mount),
            "env" => grid_kv_text(&u.detail.env),
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

/// 测试钩子：**某一行某个操作能不能点**（阶段 155：按钮搬进「操作」列了）。
///
/// `name` = 行名（容器名 / compose 项目名）；`action` = `detail` / `enter` /
/// `edit` / `start` / `stop` / `restart` / `down` / `up`。
pub fn row_action_enabled_for_test(st: &Rc<PanelState>, name: &str, action: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let Some(it) = ui.find_iter(name) else {
        return false;
    };
    let kind: String = ui.model.get(&it, COL_KIND as i32);
    let raw: String = ui.model.get(&it, COL_RAW_STATE as i32);
    row_actions(&kind, &raw)
        .into_iter()
        .find(|a| action_key(a.kind) == action)
        .map(|a| a.enabled)
        .unwrap_or(false)
}

/// 测试钩子：某一行「操作」列的**图标**（图标名 + 能不能点）。
///
/// ⚠️ **为什么要有**：图标是**自绘**的 ⇒ 冒烟能验「图标名对不对、几个、
///   禁用态对不对」⇒ 免得图标名写错 ⇒ 界面上是空白方块。
pub fn row_action_icons_for_test(st: &Rc<PanelState>, name: &str) -> Vec<(String, bool)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let Some(it) = ui.find_iter(name) else {
        return Vec::new();
    };
    let kind: String = ui.model.get(&it, COL_KIND as i32);
    let raw: String = ui.model.get(&it, COL_RAW_STATE as i32);
    row_actions(&kind, &raw)
        .into_iter()
        .map(|a| (a.icon, a.enabled))
        .collect()
}

/// 测试钩子：**在某一行上弹出右键菜单**（阶段 160）。
///
/// ⚠️ **菜单是「按行类型」现建的** ⇒ 要先给它一行 ⇒ 冒烟才能截图 / 断言。
pub fn show_row_menu_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let pop = ui.row_menu.borrow().clone();
    let Some(pop) = pop else { return false };
    let Some(it) = ui.find_iter(name) else { return false };
    let path = ui.model.path(&it);
    let Some(ctx) = row_ctx_at(ui, &path) else { return false };
    build_row_menu(st, ui, &pop, Some(&ctx));
    // ⚠️ **要像真右键一样定位在那一行旁边**（阶段 160 补）——
    //   不定位的话弹层会落在表格左下角 ⇒ 截图里根本看不到菜单。
    let y = row_widget_y(ui, &path);
    pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(24, y, 1, 1)));
    pop.popup();
    true
}

/// 这一行在**表格 widget 坐标**里的 y（找不到就 0）。
///
/// ⚠️ **`path_at_pos` 收的是 bin window 坐标**（不含列头）⇒ 得先换算。
fn row_widget_y(ui: &Rc<DockerUi>, path: &gtk::TreePath) -> i32 {
    let view = &ui.view;
    let mut y = 0;
    while y < 800 {
        let (bx, by) = view.convert_widget_to_bin_window_coords(0, y);
        if let Some((Some(p), _, _, _)) = view.path_at_pos(bx, by) {
            if &p == path {
                return y;
            }
        }
        y += 6;
    }
    0
}

/// 测试钩子：**收起右键菜单**（阶段 160；冒烟截图后要收起来）。
pub fn close_row_menu_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    if let Some(ui) = slot.as_ref() {
        ui.close_row_menu();
    }
}

/// 测试钩子：**右键菜单里每一项的文字**（阶段 160）。
pub fn row_menu_items_for_test(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let pop = ui.row_menu.borrow();
    let Some(pop) = pop.as_ref() else { return Vec::new() };
    let Some(child) = pop.child() else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur = child.first_child();
    while let Some(c) = cur {
        if let Some(b) = c.downcast_ref::<gtk::Button>() {
            if let Some(t) = crate::menu::item_label(b) {
                out.push(t);
            }
        }
        cur = c.next_sibling();
    }
    out
}

/// 测试钩子：**右键菜单里每一项能不能点**（**键是菜单文字**）。
///
/// ⚠️ **为什么要有**：菜单里的「启动 / 停止」要跟「操作」列**一样**看状态
///   ⇒ 冒烟得能验「运行中的容器，菜单里的『启动』是灰的」。
pub fn row_menu_item_sensitive_for_test(st: &Rc<PanelState>, label: &str) -> Option<bool> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return None };
    let pop = ui.row_menu.borrow();
    let Some(pop) = pop.as_ref() else { return None };
    let Some(child) = pop.child() else { return None };
    let mut cur = child.first_child();
    while let Some(c) = cur {
        if let Some(b) = c.downcast_ref::<gtk::Button>() {
            if crate::menu::item_label(b).as_deref() == Some(label) {
                return Some(b.is_sensitive());
            }
        }
        cur = c.next_sibling();
    }
    None
}

/// 测试钩子：**右键菜单里有哪些项**（阶段 160）。
///
/// 返回 `(动作短名, 菜单文字)` ⇒ 冒烟能验「文件 / 日志 / 属性」都在菜单里
/// （用户 2026-11-24 提的）。
pub fn menu_actions_for_test(kind: &str, raw_state: &str) -> Vec<(String, String)> {
    menu_actions(kind, raw_state)
        .into_iter()
        .map(|m| (action_key(m.kind).to_string(), m.label.to_string()))
        .collect()
}

/// 测试钩子：**右键菜单里每一项的图标名**（阶段 160 补）。
///
/// ⚠️ **为什么要有**：菜单图标名写错 ⇒ 界面上是**缺图占位符**（用户一眼就看见）
///   ⇒ 冒烟要能验「菜单里的图标名**系统主题里真有**」。
pub fn menu_action_icons_for_test(kind: &str, raw_state: &str) -> Vec<String> {
    menu_actions(kind, raw_state).into_iter().map(|m| m.icon).collect()
}

/// 测试钩子：**右键菜单里每一项**能不能点（**键是菜单文字**）。
///
/// ⚠️ **为什么要有**：菜单里的「启动 / 停止」要跟「操作」列**一样**
///   看容器状态 ⇒ 冒烟得能验「运行中的容器，菜单里的『启动』是灰的」。
pub fn menu_action_enabled_for_test(kind: &str, raw_state: &str, label: &str) -> Option<bool> {
    menu_actions(kind, raw_state)
        .into_iter()
        .find(|m| m.label == label)
        .map(|m| m.enabled)
}

/// 测试钩子：**直接派发一次操作**（**不查模型** ⇒ 不受后台刷新影响）。
///
/// ⚠️ **为什么要有这个**：界面组（不连服务器）会往模型里灌**假数据**
///   ⇒ 但后台的**真实刷新**可能刚好落下来 ⇒ 把假数据换掉 ⇒ 按名字查行
///   就查不到了 ⇒ 断言变成「偶发红」（实测踩过：截图多花 1 秒就红了）。
///   ⇒ 这个钩子**直接派发** ⇒ 与模型里现在是什么无关。
///
/// ⚠️ **但它绕过了「按行找」那一段** ⇒ 「行 ⇒ 类型/原始状态 ⇒ 动作」
///   这条链路由**单测**（`row_actions`）与**界面组**（在真数据上点
///   「进容器」图标）一起守着。
pub fn dispatch_action_for_test(st: &Rc<PanelState>, name: &str, kind: &str, action: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    // ⚠️ **阶段 160**：动作可能在「操作」列里，也可能在**右键菜单**里
    //   （详情 / 文件 / 日志 / 编辑…）⇒ 两处都找。
    let Some(a) = row_actions(kind, "running")
        .into_iter()
        .chain(menu_actions(kind, "running").into_iter().map(|m| RowAction {
            kind: m.kind,
            icon: m.icon,
            enabled: true,
        }))
        .find(|a| action_key(a.kind) == action)
    else {
        return;
    };
    let hit = ActionHit {
        ctx: RowCtx {
            path: gtk::TreePath::new_first(),
            name: name.to_string(),
            kind: kind.to_string(),
            raw: "running".to_string(),
        },
        action: a,
    };
    dispatch_row_action(st, ui, &hit);
}

/// 测试钩子：**点一下某一行某个操作**（走的是「操作」列的同一条派发链路）。
///
/// ⚠️ 冒烟里点的是**不存在的容器名** ⇒ 就算确认逻辑坏了，命令也只会被
///    docker 拒绝 ⇒ **碰不到用户机器上的真容器**。
pub fn click_row_action_for_test(st: &Rc<PanelState>, name: &str, action: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let Some(it) = ui.find_iter(name) else { return };
    let kind: String = ui.model.get(&it, COL_KIND as i32);
    let raw: String = ui.model.get(&it, COL_RAW_STATE as i32);
    // ⚠️ **阶段 160**：动作可能在「操作」列里，也可能在**右键菜单**里
    //   （进容器 / 详情 / 文件…）⇒ 两处都找。
    let Some(a) = row_actions(&kind, &raw)
        .into_iter()
        .chain(menu_actions(&kind, &raw).into_iter().map(|m| RowAction {
            kind: m.kind,
            icon: m.icon,
            enabled: true,
        }))
        .find(|a| action_key(a.kind) == action)
    else {
        return;
    };
    let path = ui.model.path(&it);
    let hit = ActionHit {
        ctx: RowCtx {
            path,
            name: name.to_string(),
            kind,
            raw,
        },
        action: a,
    };
    dispatch_row_action(st, ui, &hit);
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
///
/// ⚠️ **用 `visible()` 而不是 `is_visible()`**：阶段 155 之后详情住在
///   **弹窗**里 ⇒ 弹窗没开时整棵子树的 `is_visible()` **恒为 false**
///   ⇒ 那样就分不清「没有项目详情」和「弹窗没开」了。
pub fn project_detail_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.detail.proj_root.property::<bool>("visible"))
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

// ⚠️ **阶段 155**：原来这里有 `project_action_sensitive_for_test` /
//   `click_project_action_for_test`（项目那五个按钮）⇒ 那些按钮搬进
//   **「操作」列**了 ⇒ 跟容器行**共用**上面那两个钩子
//   （`row_action_enabled_for_test` / `click_row_action_for_test`）。

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

// ⚠️ **阶段 155**：原来这里有 `enter_button_for_test` / `enter_click_for_test`
//   （工具条上的「进入容器」按钮）⇒ 那个按钮**撤了** ⇒ 入口搬进
//   **「操作」列**（每行一颗终端图标）⇒ 冒烟改用
//   `click_row_action_for_test(st, 容器名, "enter")`。

/// 测试钩子：**双击**某个容器行（阶段 117 ⇒ 顶层容器行双击 ⇒ 进 shell）。
pub fn double_click_container_for_test(st: &Rc<PanelState>, name: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let model = ui.model.clone();
    let Some(path) = ui.find_path(name) else { return };
    if model.iter(&path).is_none() {
        return;
    }
    ui.view.emit_row_activated(&path, None);
}

/// 测试钩子：容器内终端的**尺寸**（列, 行）—— 用来对照容器里的 `stty size`。
pub fn term_size_for_test(st: &Rc<PanelState>) -> (i64, i64) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (0, 0) };
    let view = ui.term_view.borrow();
    match view.as_ref() {
        Some(t) => (t.column_count(), t.row_count()),
        None => (0, 0),
    }
}

/// 测试钩子：**停掉**容器内终端（窗口关掉时走的就是它）。
pub fn stop_terminal_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    stop_terminal(ui);
}

// ⚠️ **阶段 155**：原来这里有 `term_button_sensitive_for_test`（详情区的
//   「终端」按钮）⇒ 那个按钮**撤了** ⇒ 入口在**「操作」列**里。

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
    // ⚠️ **终端现在在独立窗口里**（阶段 152）⇒ 「开着」＝
    //   会话还在跑 **且** 那个窗口还在屏幕上。
    ui.term_running.get()
        && ui
            .term_window
            .borrow()
            .as_ref()
            .map(|w| w.is_visible())
            .unwrap_or(false)
}

/* ---------------- 阶段 119：容器终端工具条的测试钩子 ---------------- */

/// 测试钩子：容器终端**工具条**上的东西（阶段 119）。
///
/// 返回 `(下拉里的选项, 工具条在不在屏幕上)`。
///
/// ⚠️ **为什么连选项一起返回**：光说「有个下拉」没用 ⇒ 得看得见**里面
///   有什么**（自动 ＋ 四个候选）⇒ 用户才知道能选什么。
pub fn term_toolbar_for_test(st: &Rc<PanelState>) -> (Vec<String>, bool, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return (Vec::new(), false, String::new());
    };
    let opts = ui.term_shell_items.borrow().clone();
    let visible = ui
        .term_bar
        .borrow()
        .as_ref()
        .map(|b| b.is_visible())
        .unwrap_or(false);
    // ⚠️ **当前选中的那一项**从**真的下拉**里读（不是从我们自己记的
    //    列表里读）⇒ 验的是「用户看见的那个」。
    let active = ui
        .term_shell_combo
        .borrow()
        .as_ref()
        .and_then(|c| c.active_text())
        .map(|s| s.to_string())
        .unwrap_or_default();
    (opts, visible, active)
}

/// 测试钩子：**选**工具条下拉里的第几项（阶段 119；0 = 自动）。
pub fn term_select_shell_for_test(st: &Rc<PanelState>, idx: u32) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let combo = ui.term_shell_combo.borrow();
    if let Some(c) = combo.as_ref() {
        c.set_active(Some(idx));
    }
}

/// 测试钩子：点「重连」（阶段 119）。
pub fn term_reconnect_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let btn = ui.term_reconnect_btn.borrow();
    if let Some(b) = btn.as_ref() {
        b.emit_clicked();
    }
}

/// 测试钩子：点「清屏」（阶段 119）。
pub fn term_clear_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let btn = ui.term_clear_btn.borrow();
    if let Some(b) = btn.as_ref() {
        b.emit_clicked();
    }
}

/// 测试钩子：点「搜索」（阶段 119）⇒ 返回**搜索条现在是不是开着**。
pub fn term_search_toggle_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let btn = ui.term_search_btn.borrow();
    if let Some(b) = btn.as_ref() {
        b.emit_clicked();
    }
    ui.term_search_bar
        .borrow()
        .as_ref()
        .map(|s| s.is_search_mode())
        .unwrap_or(false)
}

/// 测试钩子：在容器终端的**搜索框里填词并找下一个**（阶段 119）。
///
/// 返回**找到没有**。
pub fn term_search_for_test(st: &Rc<PanelState>, text: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    // ⚠️ **走真的输入框**（不是直接调后端）⇒ 顺带验「输入框 → 后端」
    //    那根线**真的接上了**。
    if let Some(e) = ui.term_find_entry.borrow().as_ref() {
        e.set_text(text);
    }
    let view = ui.term_view.borrow();
    match view.as_ref() {
        // ⚠️ **跟界面上的行为一样**：next 找不到就回绕找 previous
        //   （输出在**光标前面**时 next 找不到 ⇒ 界面也是这么兜的）。
        Some(t) => t.search_find_next() || t.search_find_previous(),
        None => false,
    }
}

/// 测试钩子：容器终端**当前用的 shell**（阶段 119；空 = 还没建起来）。
pub fn term_shell_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.term_shell.borrow().clone()
}

/// 测试钩子：容器终端**重连过几次**（阶段 119）。
///
/// ⚠️ **为什么要有**：光看「屏幕上还有没有输出」分不清「重连了」与
///   「没重连但输出还在」⇒ 要一个**外部可观察**的量。
pub fn term_reconnects_for_test(st: &Rc<PanelState>) -> u32 {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.term_reconnects.get()).unwrap_or(0)
}

/// 测试钩子：容器终端的**状态文字**（阶段 119）。
pub fn term_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    ui.term_status
        .borrow()
        .as_ref()
        .map(|l| l.text().to_string())
        .unwrap_or_default()
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

/// 测试钩子：**关掉容器内终端的窗口**（冒烟截图用）。
pub fn close_terminal_window_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    // ⚠️ **终端现在在独立窗口里**（阶段 152）⇒ 「关掉」＝ 停掉会话 ＋
    //   **关掉那个窗口**（`stop_terminal` 已经会关 ⇒ 不会留在屏幕上）。
    stop_terminal(ui);
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

/// 测试钩子：**打开日志窗**（走真实的打开流程 ⇒ 会起流）。
///
/// ⚠️ **阶段 165**：日志是独立窗口（不再是页签）⇒ 这里开的是**那扇窗**。
pub fn log_show_page_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let name = ui.selected.borrow().clone();
    if name.is_empty() {
        return false;
    }
    open_logs_window(st, ui, &name);
    true
}

/// 测试钩子：**只开日志窗**（**不起流**）。
///
/// ⚠️ **为什么单独给一个**：纯界面行为（搜索 / 暂停 / 清空）要在**假面板**
///   上测 ⇒ 不能真的去连服务器 ⇒ 不能走 `open_logs_window`（它会起流）。
///   ⇒ 这里只把窗打开（界面行为照样是真的）⇒ 不碰网络。
pub fn log_window_show_only_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let win = ensure_logs_window(st, ui);
    win.present();
    true
}

/// 测试钩子：**关掉日志窗**（⇒ 日志流停）。
pub fn log_show_overview_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    if let Some(w) = ui.logs_window.borrow().as_ref() {
        w.set_visible(false);
    }
    true
}

/// 测试钩子：日志页**是不是开着**。
pub fn log_page_visible_for_test(st: &Rc<PanelState>) -> bool {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.log_page_visible()).unwrap_or(false)
}

/// 测试钩子：日志区的**状态行**文字（阶段 165）。
pub fn log_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.log.status.text().to_string())
        .unwrap_or_default()
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

    /// 分档阈值：跟 [`ColumnDensity::for_width`] 的进入阈值一致。
    ///
    /// ⚠️ **默认主窗 1200×780 扣掉侧栏 230 ⇒ 内容区约 960px**
    ///   ⇒ 落在「中」档（七列里的 ID 让位，其余全留）。
    #[test]
    fn 容器列按实际宽度分档() {
        assert_eq!(ColumnDensity::for_width(1200), ColumnDensity::Wide);
        assert_eq!(ColumnDensity::for_width(960), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::for_width(800), ColumnDensity::Narrow);
        assert_eq!(ColumnDensity::for_width(500), ColumnDensity::Compact);
        assert_eq!(ColumnDensity::Wide.for_resize(1200), ColumnDensity::Wide);
        assert_eq!(ColumnDensity::Wide.for_resize(1169), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::Medium.for_resize(960), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::Medium.for_resize(939), ColumnDensity::Narrow);
        assert_eq!(ColumnDensity::Narrow.for_resize(800), ColumnDensity::Narrow);
        assert_eq!(ColumnDensity::Narrow.for_resize(529), ColumnDensity::Compact);
    }

    #[test]
    fn 列档位切换有双向滞回() {
        // 往下：掉出稳定区才换档，换完就稳住。
        assert_eq!(ColumnDensity::Wide.for_resize(1180), ColumnDensity::Wide);
        assert_eq!(ColumnDensity::Medium.for_resize(950), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::Narrow.for_resize(600), ColumnDensity::Narrow);
        assert_eq!(ColumnDensity::Compact.for_resize(400), ColumnDensity::Compact);
        // 往上：多给一点才升档（防抖）。
        assert_eq!(ColumnDensity::Medium.for_resize(1160), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::Medium.for_resize(1170), ColumnDensity::Wide);
        assert_eq!(ColumnDensity::Narrow.for_resize(930), ColumnDensity::Narrow);
        assert_eq!(ColumnDensity::Narrow.for_resize(940), ColumnDensity::Medium);
        assert_eq!(ColumnDensity::Compact.for_resize(520), ColumnDensity::Compact);
        assert_eq!(ColumnDensity::Compact.for_resize(530), ColumnDensity::Narrow);
    }

    /// ⚠️ **这一条是 P0-1 的命门**：分档要成立，两个不等式都得满足。
    ///
    /// ① **够得着**：每一档的「可见列最小宽度之和」必须 ≤ 它的进入阈值
    ///    —— 否则表格被 min_width 撑住、分配宽度永远达不到阈值 ⇒
    ///    **这一档根本进不去**。
    /// ② **缩得下去**：表格被撑到本档的最小宽度时，那个宽度必须能落到
    ///    **更窄的一档** —— 否则就卡死在宽档、再也退不了。
    ///
    /// （第一版实现就栽在 ①：只藏列、不降 min_width ⇒ `1024×700` 那一档
    ///  进不去 ⇒ 操作列仍然被挤出可视区。）
    #[test]
    fn 分档的最小宽度既够得着也缩得下去() {
        let action_min = crate::panel::actioncell::SLOT_W * 4 + 24;
        let total = |d: ColumnDensity| -> i32 {
            [
                COL_NAME,
                COL_ID,
                COL_IMAGE,
                COL_PORTS,
                COL_STATE,
                COL_RES,
            ]
            .iter()
            .filter_map(|c| d.column_spec(*c))
            .map(|(_, w)| w)
            .sum::<i32>()
                + action_min
        };
        // ① 够得着：阈值就是进入这一档所需的最小视口宽度。
        for (d, threshold) in [
            (ColumnDensity::Wide, 1170),
            (ColumnDensity::Medium, 940),
            (ColumnDensity::Narrow, 530),
            // 极窄档没有自己的下界（它是「再窄就只能是它」）
            (ColumnDensity::Compact, 530),
        ] {
            assert!(
                total(d) <= threshold,
                "{d:?} 的最小宽度合计 {} 超过进入阈值 {threshold} ⇒ 这一档进不去",
                total(d)
            );
        }
        // ② 缩得下去：撑到本档最小宽度时，必须能落到更窄（或就是本）的一档。
        for d in [
            ColumnDensity::Wide,
            ColumnDensity::Medium,
            ColumnDensity::Narrow,
            ColumnDensity::Compact,
        ] {
            let t = total(d);
            let next = ColumnDensity::for_width(t);
            assert!(
                total(next) <= t,
                "{d:?} 撑到最小宽度 {t} 后落到 {next:?}，而后者要 {} ⇒ 缩不下去",
                total(next)
            );
        }
    }

    /// 核心三列（名称 / 状态 / 操作）**任何一档都不能丢**。
    #[test]
    fn 核心列任何一档都保留() {
        for density in [
            ColumnDensity::Wide,
            ColumnDensity::Medium,
            ColumnDensity::Narrow,
            ColumnDensity::Compact,
        ] {
            assert!(density.column_spec(COL_NAME).is_some(), "{density:?} 丢了名称列");
            assert!(density.column_spec(COL_STATE).is_some(), "{density:?} 丢了状态列");
        }
    }

    /// 错误分类：三种「第一分钟错误」要**长得不一样**（阶段 135 加了顺序断言）。
    ///
    /// ⚠️ **顺序这条最要紧**：后端那句「连不上远程的 Docker（服务没起，
    ///   或当前账号没权限）」**同时**含「连不上」与「权限」⇒ 先认哪个
    ///   决定标题 ⇒ 也决定**弹不弹那个「没权限」的对话框** ⇒ 认错就是
    ///   **弹一个错的对话框**给用户。
    #[test]
    fn docker错误分类() {
        use crate::emptystate::DockerErrorKind as K;
        // 用户截图里那句 ⇒ 必须是「没权限」。
        assert_eq!(
            classify_docker_error("当前账号没有访问 Docker 的权限（需要加入 docker 组，或改用有权限的账号登录）"),
            K::NoPermission
        );
        // ⚠️ **守护进程没起**：那句话里也有「权限」两个字 ⇒ 不能认成没权限。
        assert_eq!(
            classify_docker_error("连不上远程的 Docker（服务没起，或当前账号没权限）"),
            K::NotRunning
        );
        assert_eq!(
            classify_docker_error("远程的 Docker 服务没有运行（守护进程没起来）"),
            K::NotRunning
        );
        assert_eq!(
            classify_docker_error("远程没有找到 docker（这台服务器上可能没装 Docker）"),
            K::NoDocker
        );
        assert_eq!(classify_docker_error("随便什么错"), K::Other);
    }

    /// 「没权限」的说明：**得说清楚怎么办**（阶段 135）。
    #[test]
    fn 没权限的说明说了怎么办() {
        let help = docker_denied_help();
        assert!(help.contains("docker 组"), "没说要加入 docker 组：{help}");
        assert!(help.contains("usermod"), "没给具体命令：{help}");
        // ⚠️ **不许出现 Markdown 星号**（GTK 不渲染 ⇒ 用户看到字面星号）。
        // ⚠️ **星号要「拼出来」**：写成字面量的话，lint 会**扫到自己**
        //    （源码里那个 `"**"` 自己就命中了）——同一个坑，见 AGENTS.md。
        let stars = ["*", "*"].concat();
        assert!(!help.contains(&stars), "说明里有 Markdown 星号：{help}");
    }

    /// docker 状态 ⇒ 药丸档位（美化·地基 2/5）。
    ///
    /// ⚠️ 这条钉的是「`state_markup` 与药丸渲染器共用一个映射」——
    ///    谁改了分支没同步另一处，这里先红。
    #[test]
    fn 状态映射到药丸档位() {
        use crate::panel::pillcell::PillState;
        assert_eq!(pill_state_of("running"), PillState::Ok);
        assert_eq!(pill_state_of("paused"), PillState::Warn);
        assert_eq!(pill_state_of("restarting"), PillState::Warn);
        assert_eq!(pill_state_of("dead"), PillState::Err);
        // 已停止/已创建不是错误 ⇒ 灰
        assert_eq!(pill_state_of("exited"), PillState::Dim);
        assert_eq!(pill_state_of("created"), PillState::Dim);
        // compose 父行摘要 / 空串 ⇒ 灰（不假装有状态）
        assert_eq!(pill_state_of("1/3 运行中"), PillState::Dim);
        assert_eq!(pill_state_of(""), PillState::Dim);
    }

    /// 药丸文字：六种状态原文翻中文；摘要原样显示（不是「状态未知」）。
    #[test]
    fn 药丸文字翻中文() {
        assert_eq!(pill_text_of("running"), tr("运行中"));
        assert_eq!(pill_text_of("exited"), tr("已停止"));
        assert_eq!(pill_text_of("1/3 运行中"), "1/3 运行中");
        assert_eq!(pill_text_of(""), "");
    }

    /// **阶段 158：状态列的文案带上时长**（用户 2026-11-24 提的）。
    #[test]
    fn 状态列带时长() {
        use crate::backend::docker::{DockerDuration, DurUnit};
        let h2 = DockerDuration::Num(2, DurUnit::Hour);
        let m3 = DockerDuration::Num(3, DurUnit::Minute);
        let d9 = DockerDuration::Num(9, DurUnit::Day);
        // 在跑 ⇒ 「运行 2 小时」
        assert_eq!(
            state_pill_text("running", tr("运行中"), Some(h2)),
            trf!("运行 {d}", d = "2 小时")
        );
        // 只创建 ⇒ 「创建 3 分钟」
        assert_eq!(
            state_pill_text("created", tr("已创建"), Some(m3)),
            trf!("创建 {d}", d = "3 分钟")
        );
        // 已停止 ⇒ 「停止 9 天」
        assert_eq!(
            state_pill_text("exited", tr("已停止"), Some(d9)),
            trf!("停止 {d}", d = "9 天")
        );
        // ⚠️ **拿不到时长 ⇒ 退回原来的状态标签**（不能显示成空）。
        assert_eq!(state_pill_text("running", tr("运行中"), None), tr("运行中"));
        assert_eq!(state_pill_text("exited", tr("已停止"), None), tr("已停止"));
        // ⚠️ **暂停 / 重启中不给时长**（那两小时是「暂停了多久」）。
        assert_eq!(state_pill_text("paused", tr("已暂停"), Some(h2)), tr("已暂停"));
    }

    /// **阶段 158：时长 ⇒ 给人看的样子**。
    #[test]
    fn 时长文案() {
        use crate::backend::docker::{DockerDuration, DurUnit};
        assert_eq!(
            duration_text(DockerDuration::Num(45, DurUnit::Second)),
            format!("45 {}", tr("秒"))
        );
        assert_eq!(
            duration_text(DockerDuration::Num(3, DurUnit::Minute)),
            format!("3 {}", tr("分钟"))
        );
        assert_eq!(
            duration_text(DockerDuration::Num(2, DurUnit::Hour)),
            format!("2 {}", tr("小时"))
        );
        assert_eq!(
            duration_text(DockerDuration::Num(9, DurUnit::Day)),
            format!("9 {}", tr("天"))
        );
        assert_eq!(
            duration_text(DockerDuration::LessThanSecond),
            tr("不到 1 秒")
        );
    }

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
            duration: Some(crate::backend::docker::DockerDuration::Num(
                1,
                crate::backend::docker::DurUnit::Hour,
            )),
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

    /* ---------------- 阶段 155：「操作」列的纯函数 ---------------- */

    /// **阶段 164：资源列的取值与格式化**（CPU 百分比 ＋ 内存占用）。
    ///
    /// ⚠️ **这几条是踩过坑的**：
    /// - **`trf!` 不支持格式说明**（`{c:.2}` 会**原样显示**在界面上）⇒
    ///   数字得**先格式化好**再传进去；
    /// - **GiB 的除数写错**（拿 MiB 当 GiB）⇒ 20MiB 显示成「20.00GiB」
    ///   ⇒ **截图复检**才看出来 ⇒ 下面有专门的断言。
    #[test]
    fn 资源列的文案是cpu百分比加内存占用() {
        // ⚠️ **内存只取「用了多少」**（`docker stats` 给的是「用了 / 上限」）。
        assert_eq!(mem_used("52.84MiB / 63.00GiB"), "52.84MiB");
        assert_eq!(mem_used("10MiB"), "10MiB");
        assert_eq!(mem_used(""), "");
        // CPU 百分比。
        assert!((cpu_pct("2.30%") - 2.3).abs() < 0.001);
        assert!((cpu_pct("0.00%") - 0.0).abs() < 0.001);
        assert!((cpu_pct("") - 0.0).abs() < 0.001);
        // ⚠️ **单位不能搞错**（曾把 20MiB 写成 20.00GiB ⇒ 除数拿错了）。
        assert_eq!(human_size_bin(10 * 1024 * 1024), "10.00MiB");
        assert_eq!(human_size_bin(20 * 1024 * 1024), "20.00MiB");
        assert_eq!(human_size_bin(1024 * 1024 * 1024), "1.00GiB");
        assert_eq!(human_size_bin(512), "512 B");
        // 文案：一行，两个指标。
        let t = res_text("2.30%", "52.84MiB / 63.00GiB");
        assert!(t.contains("2.30%"), "资源列里要有 CPU 百分比：{t}");
        assert!(t.contains("52.84MiB"), "资源列里要有内存占用：{t}");
        assert!(!t.contains("63.00GiB"), "资源列里不该有内存上限：{t}");
        // ⚠️ **没采样到（已停止的容器）⇒ 就一个「—」**。
        assert_eq!(res_text("", ""), "—");
    }

    /// **阶段 165：容器行与项目行的小数位/单位都要一致**（截图里发现的）。
    ///
    /// ⚠️ **踩过的坑**：docker 原样给的是 `67.9MiB`（**一位小数**）⇒ 而
    ///   项目行是**合计** ⇒ 走 `human_size_bin` ⇒ **两位小数**
    ///   （`67.90MiB`）⇒ 同一列里父子两行**一个模子**（不然看着像两个
    ///   不同的量 ⇒ 用户会以为项目占得比容器还多）。
    #[test]
    fn 资源列的单位父子一致() {
        // docker 原样：一位小数。
        let raw = res_text("0.00%", "67.9MiB");
        // 项目行合计：两位小数。
        let sum = human_size_bin(67 * 1024 * 1024 + 942_080);
        assert!(
            raw.contains(&sum),
            "容器行应当也是 {sum}（docker 原样是 67.9MiB ⇒ 少一位小数）：{raw}"
        );
        // ⚠️ **两边的小数位要一样**（都是两位）。
        assert!(raw.contains("67.90MiB"), "{raw}");
        // ⚠️ **CPU 也一样**（docker 原样 `1.0%` ⇒ 统一成 `1.00%`）。
        let cpu = res_text("1.0%", "10MiB");
        assert!(cpu.contains("1.00%"), "CPU 小数位要统一：{cpu}");
        assert!(!cpu.contains("1.0% ·"), "CPU 小数位要统一：{cpu}");
    }

    /// **阶段 164：项目行的资源是合计**（CPU 相加 ＋ 内存按字节相加）。
    #[test]
    fn 项目行的资源是合计() {
        let mk = |name: &str, cpu: &str, mem: &str| crate::backend::docker::ContainerInfo {
            id: format!("{name}-id"),
            name: name.to_string(),
            image: "img:1".into(),
            status: "Up".into(),
            state: "running".into(),
            ports: String::new(),
            compose: "proj".into(),
            compose_files: String::new(),
            compose_dir: String::new(),
            cpu: cpu.into(),
            mem: mem.into(),
            duration: None,
        };
        let items = vec![mk("a", "1.00%", "10MiB"), mk("b", "1.00%", "10MiB")];
        let t = project_res_text(&items);
        assert!(t.contains("2.00%"), "CPU 应当相加：{t}");
        // ⚠️ **20MiB 不能写成 20.00GiB**（除数拿错过 ⇒ 截图才发现）。
        assert!(t.contains("20.00MiB"), "内存应当按字节相加：{t}");
        assert!(!t.contains("GiB"), "不该出现 GiB：{t}");
    }

    /// **阶段 164：资源列排序按 CPU 百分比**（不是按字符串 ⇒ 「9.50%」要排在
    ///   「50.80%」前面）。
    #[test]
    fn 资源列排序取cpu百分比() {
        assert!((res_cpu_value("CPU 9.50% · 内存 10MiB") - 9.5).abs() < 0.001);
        assert!((res_cpu_value("CPU 50.80% · 内存 10MiB") - 50.8).abs() < 0.001);
        assert!((res_cpu_value("—") - 0.0).abs() < 0.001);
    }

    /// **阶段 164：操作列不再吸收富余**（用户 2026-11-24：「操作列太宽了，
    ///   按钮们太靠右了，没有一点留白」）。
    ///
    /// ⚠️ **GTK4 的规矩**（实测）：**一列都不展开 ⇒ 富余全给最后一列**
    ///   ⇒ 「操作」列必须显式 `set_expand(false)`；而且**得有几列展开**，
    ///   不然 GTK 又把富余塞回最后一列。
    #[test]
    fn 操作列不吸收富余() {
        // 图标组**左对齐** ⇒ 按钮不再贴着窗口右缘。
        assert_eq!(crate::panel::actioncell::group_offset(400, 4), 0);
        // 列宽按最多的那种行算 ＋ 右边留一截空白。
        let w = (crate::panel::actioncell::SLOT_W * action_col_icons_max() as i32) + 24;
        // ⚠️ **图标组必须在列宽之内**（否则最后一颗被挤出去 ⇒ 点不到）。
        let group = crate::panel::actioncell::SLOT_W * action_col_icons_max() as i32
            - crate::panel::actioncell::ICON_GAP;
        assert!(
            group < w,
            "图标组（{group}）比列宽（{w}）还宽 ⇒ 最后一颗会被挤出格子"
        );
    }

    /// ⚠️ **阶段 160：操作列只留 3 颗**（启动 / 停止 / 重启）——
    ///   **容器行**（用户 2026-11-24：「操作列只保留启动、停止、重启，
    ///   把其他的都移到右键菜单里」）。
    /// ⚠️ **阶段 162：compose 项目行多 up / down 两颗**（用户 2026-11-24：
    ///   「compose 项目的操作列中应该还有与 up 和 down 对应的按钮」）
    ///   ⇒ 项目行 5 颗、容器行 3 颗 ⇒ **两种行一眼就分得开**。
    /// ⚠️ **阶段 163：up / down 换成一颗「重建」**（用户 2026-11-24：
    ///   「down 了之后容器界面就看不到了 ⇒ 那 up 也没意义 ⇒ 就来一个
    ///   recreate 吧」）⇒ **项目行 4 颗**。
    #[test]
    fn 操作列只有启动停止重启三颗() {
        let a = row_actions(KIND_CONTAINER, "running");
        assert_eq!(a.len(), 3, "容器行的操作列应当只有 3 颗");
        let kinds: Vec<ActionKind> = a.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ActionKind::Start,
                ActionKind::Stop,
                ActionKind::Restart
            ],
            "容器行的操作列应当只有 启动/停止/重启"
        );
        // 项目行：前 3 颗一样，后面多一颗「重建」。
        let p = row_actions(KIND_PROJECT, "running");
        let pk: Vec<ActionKind> = p.iter().map(|x| x.kind).collect();
        assert_eq!(
            pk,
            vec![
                ActionKind::Start,
                ActionKind::Stop,
                ActionKind::Restart,
                ActionKind::Recreate
            ],
            "项目行的操作列应当是 启动/停止/重启/重建"
        );
        assert_eq!(action_col_icons_max(), 4);
    }

    /// ⚠️ **阶段 160：其它动作都进右键菜单了** ⇒ 操作列里**不该**再有它们。
    #[test]
    fn 操作列里不该有详情和编辑() {
        let a = row_actions(KIND_CONTAINER, "running");
        for x in &a {
            assert!(
                !matches!(
                    x.kind,
                    ActionKind::Detail
                        | ActionKind::Enter
                        | ActionKind::Edit
                        | ActionKind::Files
                        | ActionKind::Logs
                        | ActionKind::Down
                        | ActionKind::Recreate
                        | ActionKind::CopyId
                ),
                "这些动作应当只出现在右键菜单里：{:?}",
                x.kind
            );
        }
    }

    /// ⚠️ **阶段 160：右键菜单按行类型给不同的项**（用户：新增文件 / 日志 /
    ///   属性）。
    #[test]
    fn 右键菜单按行类型给项() {
        let c = menu_actions(KIND_CONTAINER, "running");
        let ck: Vec<ActionKind> = c.iter().map(|x| x.kind).collect();
        assert!(
            ck.contains(&ActionKind::Files),
            "容器行的菜单里要有「文件」：{ck:?}"
        );
        assert!(
            ck.contains(&ActionKind::Logs),
            "容器行的菜单里要有「日志」：{ck:?}"
        );
        assert!(
            ck.contains(&ActionKind::Detail),
            "容器行的菜单里要有「属性」：{ck:?}"
        );
        assert!(
            ck.contains(&ActionKind::Enter),
            "容器行的菜单里要有「进入容器」：{ck:?}"
        );
        // 项目行：编辑 compose / 销毁 / 重建 ⇒ **不该**有「文件 / 日志」
        // （那是容器级的东西）。
        let p = menu_actions(KIND_PROJECT, "running");
        let pk: Vec<ActionKind> = p.iter().map(|x| x.kind).collect();
        assert!(pk.contains(&ActionKind::Edit), "项目行的菜单里要有「编辑 compose」：{pk:?}");
        assert!(pk.contains(&ActionKind::Down), "项目行的菜单里要有「销毁」：{pk:?}");
        assert!(pk.contains(&ActionKind::Recreate), "项目行的菜单里要有「重建」：{pk:?}");
        assert!(!pk.contains(&ActionKind::Files), "项目行不该有「文件」：{pk:?}");
        assert!(!pk.contains(&ActionKind::Logs), "项目行不该有「日志」：{pk:?}");
        // 两种行都有启停三项。
        for k in [ActionKind::Start, ActionKind::Stop, ActionKind::Restart] {
            assert!(ck.contains(&k), "容器行菜单缺 {k:?}");
            assert!(pk.contains(&k), "项目行菜单缺 {k:?}");
        }
    }

    /// **阶段 160 补：菜单里的启停项也要看状态**（跟「操作」列同一套算术）。
    ///
    /// ⚠️ **为什么要有**：运行中的容器，菜单里点「启动」只是空转
    ///   ⇒ 那一项得**真的点不了**（灰的）⇒ 免得用户以为程序坏了。
    #[test]
    fn 菜单里的启停看状态() {
        let en = |acts: &[MenuAction], k: ActionKind| -> bool {
            acts.iter()
                .find(|a| a.kind == k)
                .map(|a| a.enabled)
                .unwrap_or(false)
        };
        // 运行中 ⇒ 启动点不了、停止能点、重启能点。
        let run = menu_actions(KIND_CONTAINER, "running");
        assert!(!en(&run, ActionKind::Start), "运行中不该能点「启动」");
        assert!(en(&run, ActionKind::Stop), "运行中应当能点「停止」");
        assert!(en(&run, ActionKind::Restart), "运行中应当能点「重启」");
        // 已停止 ⇒ 反过来。
        let stop = menu_actions(KIND_CONTAINER, "exited");
        assert!(en(&stop, ActionKind::Start), "已停止应当能点「启动」");
        assert!(!en(&stop, ActionKind::Stop), "已停止不该能点「停止」");
        assert!(!en(&stop, ActionKind::Restart), "全停着不该能点「重启」");
        // 项目行同理（状态是 running / exited / mixed）。
        let prun = menu_actions(KIND_PROJECT, "running");
        assert!(!en(&prun, ActionKind::Start), "运行中的项目不该能点「启动」");
        assert!(en(&prun, ActionKind::Stop), "运行中的项目应当能点「停止」");
        // ⚠️ **跟「操作」列完全一致**（同一套算术 ⇒ 不会漂）。
        for raw in ["running", "exited", "mixed"] {
            let row = row_actions(KIND_PROJECT, raw);
            let menu = menu_actions(KIND_PROJECT, raw);
            for k in [ActionKind::Start, ActionKind::Stop, ActionKind::Restart] {
                let r = row.iter().find(|a| a.kind == k).unwrap();
                assert_eq!(
                    en(&menu, k),
                    r.enabled,
                    "菜单与操作列的 {k:?} 不一致（状态 {raw}）"
                );
            }
        }
    }

    /// **第几颗图标 ⇒ 哪个动作**（阶段 156）。
    ///
    /// ⚠️ **为什么要单测这一层**：图标**画出来**的顺序（`row_actions`）和
    ///   **点中**的顺序（`cell_x ÷ 槽宽`）必须**一模一样** ⇒ 错位一格就是
    ///   「点 compose 项目行的『编辑』却弹出了 term 窗口」（用户
    ///   2026-11-23 报的）⇒ 把这条对应关系钉死。
    ///
    /// ⚠️ **阶段 160**：操作列只剩 3 颗 ⇒ 第 2 颗是**停止**。
    #[test]
    fn 第几颗图标对应哪个动作() {
        use crate::panel::actioncell::SLOT_W;
        let p = row_actions(KIND_PROJECT, "running");
        assert_eq!(p[1].kind, ActionKind::Stop);
        assert_eq!(action_key(p[1].kind), "stop");
        let c = row_actions(KIND_CONTAINER, "running");
        assert_eq!(c[1].kind, ActionKind::Stop);
        assert_eq!(action_key(c[1].kind), "stop");
        // 第 3 颗 = 重启（两种行都一样）。
        assert_eq!(c[2].kind, ActionKind::Restart);
        // 槽宽 ⇒ 第 2 颗的**格内**坐标落在 [SLOT_W, 2*SLOT_W) ⇒ 索引 = 1。
        for x in [SLOT_W, SLOT_W + 8, 2 * SLOT_W - 1] {
            assert_eq!(x / SLOT_W, 1, "第 2 颗图标的格内坐标 {x} 应当落在索引 1");
        }
    }

    /// **运行中 ⇒ 启动点不了、停止能点**（跟以前那两个按钮一个待遇）。
    #[test]
    fn 运行中启动点不了停止能点() {
        let a = row_actions(KIND_CONTAINER, "running");
        let en = |k: ActionKind| a.iter().find(|x| x.kind == k).map(|x| x.enabled);
        assert_eq!(en(ActionKind::Start), Some(false));
        assert_eq!(en(ActionKind::Stop), Some(true));
        // ⚠️ **停了反过来**（这条抓得住「判反了」的退化）。
        let b = row_actions(KIND_CONTAINER, "exited");
        let en = |k: ActionKind| b.iter().find(|x| x.kind == k).map(|x| x.enabled);
        assert_eq!(en(ActionKind::Start), Some(true));
        assert_eq!(en(ActionKind::Stop), Some(false));
    }

    /// **阶段 163：compose 项目行比容器行多一颗「重建」**（用户
    /// 2026-11-24：「down 了之后容器界面就看不到了 ⇒ 那 up 也没意义 ⇒
    /// 就来一个 recreate 吧，就是 docker compose up -d --force-recreate」）。
    ///
    /// ⚠️ **容器行不能有**（容器没有「项目」这层 ⇒ 这条命令对它没意义）。
    /// ⚠️ **项目行也不能有「销毁」**：down 会**删掉容器** ⇒ 项目行整行消失
    ///   ⇒ 用户再也点不到那一行的任何按钮 ⇒ 是个死路（销毁只在右键菜单里）。
    #[test]
    fn 项目行多出重建一颗按钮() {
        let c = row_actions(KIND_CONTAINER, "running");
        assert_eq!(c.len(), 3, "容器行应当只有 启动/停止/重启：{c:?}");
        assert!(
            !c.iter()
                .any(|a| a.kind == ActionKind::Recreate || a.kind == ActionKind::Down),
            "容器行不该有 重建/销毁：{c:?}"
        );
        // 项目行：启动 / 停止 / 重启 ＋ 重建。
        let p = row_actions(KIND_PROJECT, "running");
        assert_eq!(p.len(), 4, "项目行应当 4 颗：{p:?}");
        let kinds: Vec<ActionKind> = p.iter().map(|a| a.kind).collect();
        assert_eq!(kinds[0], ActionKind::Start);
        assert_eq!(kinds[1], ActionKind::Stop);
        assert_eq!(kinds[2], ActionKind::Restart);
        assert_eq!(kinds[3], ActionKind::Recreate, "第 4 颗是重建");
        // ⚠️ **操作列里不该有「销毁」**（down 会让整行消失 ⇒ 死路）。
        assert!(
            !p.iter().any(|a| a.kind == ActionKind::Down),
            "项目行的操作列不该有「销毁」：{p:?}"
        );
        // ⚠️ **重建一直能点**（项目停着也能重建；改了 compose 文件也能
        //   重建 ⇒ 灰掉反而挡路）。
        let en = |k: ActionKind| p.iter().find(|x| x.kind == k).map(|x| x.enabled);
        assert_eq!(en(ActionKind::Recreate), Some(true));
        let stopped = row_actions(KIND_PROJECT, "exited");
        let en = |k: ActionKind| stopped.iter().find(|x| x.kind == k).map(|x| x.enabled);
        assert_eq!(en(ActionKind::Recreate), Some(true));
        // ⚠️ **列宽按最多的那种行算** ⇒ 4 颗放得下。
        assert_eq!(action_col_icons_max(), 4);
    }

    /// **阶段 163：重建的 tooltip 得写明跑的是哪条命令**（图标没字 ⇒
    ///   「⬆」看不出是「重建」还是「启停」）。
    #[test]
    fn 重建的tooltip写明强制重建() {
        let p = row_actions(KIND_PROJECT, "running");
        let a = p
            .iter()
            .find(|x| x.kind == ActionKind::Recreate)
            .expect("项目行有重建");
        let tip = action_tooltip(a, true, "running");
        assert!(tip.contains("compose up"), "tooltip 得写明是 compose up：{tip}");
        assert!(
            tip.contains("--force-recreate"),
            "tooltip 得写明是强制重建：{tip}"
        );
    }

    /// **阶段 162：compose 项目行的名字要一眼认出来**（粗体 ＋ 小字
    /// 「compose」）。
    #[test]
    fn 项目行名字有标记() {
        let mk = project_name_markup("proj");
        assert!(mk.contains("<b>proj</b>"), "项目名要加粗：{mk}");
        assert!(mk.contains("compose"), "要带「compose」标记：{mk}");
        // ⚠️ **名字要转义**（markup 里 `&` `<` 会坏事）。
        let mk2 = project_name_markup("a&b<c");
        assert!(!mk2.contains("<c>"), "名字里的尖括号得转义：{mk2}");
        assert!(mk2.contains("&amp;"), "名字里的 & 得转义：{mk2}");
    }

    /// **阶段 162：端口列是「主机端口 : 容器端口」**（用户 2026-11-24：
    /// 「端口应该显示为 主机端口：容器端口，但现在只有一个端口，不知道
    /// 是什么」）。
    #[test]
    fn 端口列显示主机端口和容器端口() {
        // ⚠️ **两个端口都要在**（以前压成 `5432/tcp` ⇒ 用户不知道那是
        //   主机上的还是容器里的）。
        assert_eq!(ports_pretty("0.0.0.0:5432->5432/tcp"), "5432:5432/tcp");
        assert_eq!(ports_pretty("127.0.0.1:9001->9001/tcp"), "127.0.0.1:9001:9001/tcp");
        assert_eq!(ports_pretty("9000->9001/tcp"), "9000:9001/tcp");
        // ⚠️ **只 EXPOSE、没发布** ⇒ 没有主机端口 ⇒ 原样。
        assert_eq!(ports_pretty("5432/tcp"), "5432/tcp");
        // IPv4 ＋ IPv6 两条同一个映射 ⇒ **去重**（只留一条）。
        assert_eq!(
            ports_pretty("0.0.0.0:3306->3306/tcp, [::]:3306->3306/tcp"),
            "3306:3306/tcp"
        );
        // 多个端口 ⇒ **一个一行**。
        assert_eq!(
            ports_pretty("0.0.0.0:80->80/tcp, 0.0.0.0:443->443/tcp"),
            "80:80/tcp\n443:443/tcp"
        );
        // 没有端口 ⇒ 空。
        assert_eq!(ports_pretty(""), "");
    }

    /// **项目三种状态**（全跑 / 全停 / 一半跑）⇒ 启停的可用性跟着变。
    #[test]
    fn 项目三态决定启停能不能点() {
        let en = |raw: &str, k: ActionKind| {
            row_actions(KIND_PROJECT, raw)
                .iter()
                .find(|x| x.kind == k)
                .map(|x| x.enabled)
        };
        // 全在跑 ⇒ 启动没得启（灰），停止能点。
        assert_eq!(en("running", ActionKind::Start), Some(false));
        assert_eq!(en("running", ActionKind::Stop), Some(true));
        // 全停了 ⇒ 启动能点，停止没得停（灰）。
        assert_eq!(en("exited", ActionKind::Start), Some(true));
        assert_eq!(en("exited", ActionKind::Stop), Some(false));
        // 一半在跑 ⇒ 两个都能点（停掉跑着的、把停着的起起来）。
        assert_eq!(en("mixed", ActionKind::Start), Some(true));
        assert_eq!(en("mixed", ActionKind::Stop), Some(true));
    }

    /// **图标名必须是 Adwaita 真有的**（写错 ⇒ 界面上是空白方块）。
    ///
    /// ⚠️ **这条只是「长得像」**：真正的判据是问图标主题（冒烟里问）
    ///   ⇒ 这里防的是「拼错 / 用了不存在的名字」这种低级错。
    ///
    /// ⚠️ **阶段 157：自绘图标（`pp-` 开头）不在主题里** ⇒ 单独放行
    ///   （Adwaita 只有**实心**的 ▶ / ■ ⇒ 混在一排线性图标里太扎眼
    ///   ⇒ 自己画描边版，见 `panel::actioncell::custom_node`）。
    #[test]
    fn 图标名都是已知的Adwaita符号() {
        for raw in ["running", "exited", "mixed"] {
            for a in row_actions(KIND_CONTAINER, raw)
                .into_iter()
                .chain(row_actions(KIND_PROJECT, raw))
            {
                assert!(!a.icon.is_empty());
                if a.icon.starts_with("pp-") {
                    continue;
                }
                assert!(
                    a.icon.ends_with("-symbolic"),
                    "图标名应当是 symbolic 的：{}",
                    a.icon
                );
            }
        }
    }

    /// **自绘图标只有两个**，且**必须真的自绘**（阶段 157）。
    ///
    /// ⚠️ 为什么要单独钉住：自绘图标**不在图标主题里** ⇒ 冒烟里那条
    ///   「图标名在主题里真的存在」只能跳过它们 ⇒ 万一有人把 `pp-play`
    ///   写成了 `media-playback-start-symbolic`（主题里**有** ⇒ 冒烟绿了）
    ///   ⇒ 视觉上又变回那个扎眼的实心三角 ⇒ 没人会发现。
    #[test]
    fn 自绘图标只有播放和停止两个() {
        let mut custom: Vec<String> = Vec::new();
        for raw in ["running", "exited", "mixed"] {
            for a in row_actions(KIND_CONTAINER, raw)
                .into_iter()
                .chain(row_actions(KIND_PROJECT, raw))
            {
                if a.icon.starts_with("pp-") {
                    custom.push(a.icon.clone());
                }
            }
        }
        custom.sort_unstable();
        custom.dedup();
        assert_eq!(
            custom,
            vec!["pp-play".to_string(), "pp-stop".to_string()],
            "自绘图标多了或少了：{custom:?}"
        );
    }

    /// **同一行里的短名字不能重样**（测试钩子靠它派发 ⇒ 重了就派错）。
    #[test]
    fn 同一行里动作短名不重样() {
        for raw in ["running", "exited", "mixed"] {
            for kind in [KIND_CONTAINER, KIND_PROJECT] {
                let mut seen: Vec<&str> = Vec::new();
                for a in row_actions(kind, raw) {
                    let k = action_key(a.kind);
                    assert!(!seen.contains(&k), "{kind} 这一行里短名重了：{k}");
                    seen.push(k);
                }
            }
        }
        // ⚠️ **八种动作各有一个短名**（详情在两种行里都有 ⇒ 按行判重）。
        let all = [
            ActionKind::Detail,
            ActionKind::Enter,
            ActionKind::Edit,
            ActionKind::Start,
            ActionKind::Stop,
            ActionKind::Restart,
            ActionKind::Down,
            ActionKind::Recreate,
        ];
        let mut names: Vec<&str> = all.iter().map(|k| action_key(*k)).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 8, "八种动作各有一个短名");
    }

    /// **禁用时要说明「为什么不能点」**（UI 审查 D15 的老规矩）。
    #[test]
    fn 禁用时有话说清楚为什么() {
        let running = row_actions(KIND_CONTAINER, "running");
        let start = running
            .iter()
            .find(|x| x.kind == ActionKind::Start)
            .expect("有启动");
        assert!(!start.enabled);
        let tip = action_tooltip(start, false, "running");
        assert!(tip.contains("运行"), "tooltip 得说清为什么不能点：{tip}");
        // ⚠️ **英文界面也得有话说**（不能是空串 / 不能是中文）。
        assert!(!tip.is_empty());
    }

    /* ---------------- UI 审查 2026-11-18 的纯函数 ---------------- */

    /// 端口列：**一个端口一行**（用户 2026-11-23）。
    ///
    /// ⚠️ **为什么**：docker 给的端口串是逗号分隔的长串 ⇒ 单行放不下
    ///   ⇒ 被省略号截成 `0.0.0.0:3306->330…` ⇒ 端口号看不全。
    #[test]
    fn 端口列一个端口一行() {
        let s = ports_multiline("0.0.0.0:3306->3306/tcp, :::3306->3306/tcp");
        assert_eq!(s, "0.0.0.0:3306->3306/tcp\n:::3306->3306/tcp");
        // ⚠️ **换行后不能把端口内容弄丢**（闸门：两个端口都在）
        assert_eq!(s.lines().count(), 2);
        // 没有端口 ⇒ 空字符串（不是一堆空行）
        assert_eq!(ports_multiline(""), "");
        // ⚠️ **排序用的摊平版**：多行的端口串比较时不能被 `\n` 带偏
        //   （`", "` 里的逗号+空格 ⇒ 换成换行 ⇒ 摊平回来还是**一个**空格）
        assert_eq!(
            ports_multiline("a, b").replace('\n', " "),
            "a b"
        );
    }

    /// 内存列**不带百分比**（用户 2026-11-23）。
    ///
    /// 权限列 ⇒ **通用写法，不翻成中文**（阶段 140）。
    ///
    /// ⚠️ **为什么改**（用户 2026-11-19：「权限好像一般没有说还要翻译成汉语
    ///   的」）：Linux / Docker / `chmod` 用的都是同一套写法（`drwxr-xr-x`）
    ///   ⇒ 翻译成中文反而跟命令对不上，也没法复制粘贴。
    #[test]
    fn 权限列用通用写法不翻成中文() {
        assert_eq!(perm_text("drwxr-xr-x"), "drwxr-xr-x");
        assert_eq!(perm_text("-rw-r--r--"), "-rw-r--r--");
        assert_eq!(perm_text("lrwxrwxrwx"), "lrwxrwxrwx");
        // ⚠️ **认不出来也原样给**（别吞信息）。
        assert_eq!(perm_text("???"), "???");
        assert_eq!(perm_text(""), "");
        // ⚠️ **不许出现中文**（这条是被用户的话钉死的）。
        for p in ["drwxr-xr-x", "-rw-r--r--", "lrwxrwxrwx", "???"] {
            let s = perm_text(p);
            assert!(
                !s.chars().any(|c| c as u32 >= 0x4e00 && c as u32 <= 0x9fff),
                "权限里不该有中文：{s}"
            );
        }
    }

    /// 文件类型 ⇒ **图标**（阶段 140：目录 / 文件 / 链接一眼分得清）。
    #[test]
    fn 文件类型各有图标() {
        assert_eq!(file_icon(docker::FileKind::Dir), "folder-symbolic");
        assert_eq!(file_icon(docker::FileKind::File), "text-x-generic-symbolic");
        assert_eq!(file_icon(docker::FileKind::Link), "insert-link-symbolic");
        // ⚠️ **三种类型不能撞成同一个图标**（不然还是分不清）。
        assert_ne!(
            file_icon(docker::FileKind::Dir),
            file_icon(docker::FileKind::File)
        );
        assert_ne!(
            file_icon(docker::FileKind::File),
            file_icon(docker::FileKind::Link)
        );
    }

    /// **阶段 165：空日志要说清楚**（纯函数 ⇒ 单测能守住）。
    ///
    /// ⚠️ **为什么值钱**：用户 2026-11-24 报「这个日志窗口我从来没看到
    ///   日志」⇒ 根因是**反馈没写清楚**（容器本来就没日志）⇒ 这句话是
    ///   这次改动的核心 ⇒ 钉死它。
    #[test]
    fn 空日志的状态文案说清楚() {
        // 一条都没有 ＋ 正常退出 ⇒ **明确说「没有日志输出」**。
        let s = log_end_status(true, 0);
        assert!(
            s.contains("没有日志输出"),
            "空日志要说清楚：{s}"
        );
        // ⚠️ **不能跟「正常结束」混为一谈**（那会让用户以为功能坏了）。
        assert!(
            !s.contains("日志流已结束"),
            "空日志不该说成「日志流已结束」：{s}"
        );
        // 有日志 ＋ 正常退出 ⇒ 照旧说「日志流已结束」。
        let s2 = log_end_status(false, 0);
        assert!(s2.contains("日志流已结束"), "{s2}");
        assert!(!s2.contains("没有日志输出"), "{s2}");
        // 退出码非 0 ⇒ 两边都要带退出码（**别把错误吃掉**）。
        assert!(log_end_status(true, 3).contains("3"), "得带退出码");
        assert!(log_end_status(false, 3).contains("3"), "得带退出码");
    }

    /// 文件页的报错 ⇒ **说人话**（阶段 140 → 阶段 172 / UI-07）。
    ///
    /// ⚠️ **老断言「别的错原样给」已经过时**：现在**每类错都有中文分类**
    ///   （审核 P1-4），但**原文必须保留**（在 `raw` 里）。
    #[test]
    fn 文件页报错说人话() {
        // ⚠️ **容器里没有 ls** ⇒ docker 的原话用户看不懂。
        let raw = "OCI runtime exec failed: exec: \"ls\": executable file not found in $PATH";
        let e = classify_files_error(raw);
        assert_eq!(e.kind, FilesErrKind::MissingCommand);
        assert!(e.brief.contains("ls"), "得说清是缺 ls：{}", e.brief);
        assert!(!e.brief.contains("OCI runtime"), "不该再摆 docker 的原话：{}", e.brief);
        // ⚠️ **原文必须一字不丢**（「说人话」与「保留原文」同时成立）。
        assert_eq!(e.raw, raw);
    }

    /// UI-07：六类常见错误各有**自己的中文提示**，且**互不串味**。
    #[test]
    fn 文件页错误分六类() {
        let cases: &[(&str, FilesErrKind)] = &[
            (
                "Error response from daemon: permission denied while trying to open /data",
                FilesErrKind::Permission,
            ),
            (
                "Error response from daemon: Container abc123 is not running",
                FilesErrKind::NotRunning,
            ),
            (
                "Error: failed to create task: read-only file system",
                FilesErrKind::ReadOnly,
            ),
            (
                "ls: cannot access '/nope': No such file or directory",
                FilesErrKind::NoSuchPath,
            ),
            (
                "OCI runtime exec failed: exec: \"ls\": executable file not found in $PATH",
                FilesErrKind::MissingCommand,
            ),
            (
                "error during connect: Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
                FilesErrKind::DaemonUnreachable,
            ),
            ("context deadline exceeded", FilesErrKind::Timeout),
        ];
        let mut briefs = Vec::new();
        for (raw, want) in cases {
            let e = classify_files_error(raw);
            assert_eq!(e.kind, *want, "「{raw}」归错类了");
            assert!(!e.brief.is_empty(), "「{raw}」没有中文提示");
            assert_eq!(e.raw.as_str(), *raw, "原文被改了");
            briefs.push(e.brief);
        }
        // ⚠️ **每类的提示都要不一样**（不能都变成同一句「操作失败」）。
        for i in 0..briefs.len() {
            for j in i + 1..briefs.len() {
                assert_ne!(briefs[i], briefs[j], "两类错误的提示撞车了");
            }
        }
    }

    /// ⚠️ **认不出来就保守**（审核 P1-4：「未知错误不瞎猜权限或网络原因」）。
    #[test]
    fn 认不出来的错误归未知() {
        let e = classify_files_error("something totally unexpected happened");
        assert_eq!(e.kind, FilesErrKind::Unknown);
        assert!(e.brief.contains("详情"), "未知错误要提示去看详情：{}", e.brief);
        assert_eq!(e.raw.as_str(), "something totally unexpected happened");
    }

    /// ⚠️ **Docker 29 的多行报错 ＋ `exit status N` 尾行**也要归对类
    ///   （审核 P1-4 明确点名了这个真实形态）。
    #[test]
    fn 多行报错也归得对() {
        let raw = "OCI runtime exec failed: exec: \"ls\": executable file not found in $PATH\n\
                   (standard input): line 1: ls: not found\nexit status 127";
        let e = classify_files_error(raw);
        assert_eq!(e.kind, FilesErrKind::MissingCommand);
        assert_eq!(e.raw.as_str(), raw);
    }

    /// 空目录是**成功**，不是错误（审核 P1-4：「空目录仍视为成功」）。
    #[test]
    fn 空目录不是错误() {
        // 退出码 0 的 0 行 ⇒ 后端不会报错；这里只是确认分类函数不会把
        // 「没有条目」这种正常返回当成错误。
        let e = classify_files_error("");
        assert_eq!(e.kind, FilesErrKind::Unknown);
        assert_eq!(e.raw, "");
    }
}


/* ---------------- 阶段 115：镜像 / 卷 / 网络（前端） ---------------- */

/// 建一张**平表**（列标题 + 列宽）⇒ 返回 `(ListStore, TreeView)`。
///
/// ⚠️ **为什么不用树表**：镜像 / 卷 / 网络都没有层级 ⇒ 平表就够。
/// ⚠️ **第一列 expand**：名字列拉宽，其余给下限（跟容器表一个脾气）。
/// 一列的**样式**（美化·地基 3/5）：普通 / 数字（右对齐）/ 等宽（技术值）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColStyle {
    Normal,
    Num,
    Mono,
}

/// 建一张**平表**（ListStore + TreeView；镜像 / 卷 / 网络 / 容器文件共用）。
///
/// `styles` 按列给样式（缺省 = Normal）：ID 这类十六进制技术值给
/// [`ColStyle::Mono`]（比例字体数错位），大小这类数值给
/// [`ColStyle::Num`]（右对齐，数量级一眼对齐）。
fn flat_table(
    titles: &[&str],
    widths: &[i32],
    expand_cols: &[usize],
    styles: &[ColStyle],
) -> (gtk::TreeStore, gtk::TreeView) {
    let types: Vec<glib::Type> = titles.iter().map(|_| glib::Type::STRING).collect();
    let store = gtk::TreeStore::new(&types);
    let view = gtk::TreeView::new();
    view.set_model(Some(&store));
    // ⚠️ **阶段 167：表格要铺满整页**（用户 2026-11-25「表格没铺满」）。
    //   ⇒ `TreeView` 自己也要 `hexpand` ⇒ 否则它在 `ScrolledWindow` 里
    //   **只占自然宽度**（= 各列宽度之和）⇒ 右边空一大块。
    view.set_hexpand(true);
    for (i, t) in titles.iter().enumerate() {
        let r = gtk::CellRendererText::new();
        r.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let col = gtk::TreeViewColumn::new();
        col.set_title(t);
        col.set_resizable(true);
        match styles.get(i).copied().unwrap_or(ColStyle::Normal) {
            ColStyle::Num => {
                gtk::prelude::CellRendererExt::set_alignment(&r, 1.0, 0.5);
                col.set_alignment(1.0);
            }
            ColStyle::Mono => {
                r.set_family(Some("monospace"));
            }
            ColStyle::Normal => {}
        }
        col.pack_start(&r, false);
        // ⚠️ **必须显式绑 "text"**（跟容器表同一个坑：不绑就是一片空白）。
        col.add_attribute(&r, "text", i as i32);
        col.set_min_width(widths[i]);
        // ⚠️ **哪几列展开**（阶段 167，改成**可以多列**）：
        //
        //   **实测 GTK4 的规矩**（`examples/_tmp_layout_probe.rs` 验过）：
        //   - **一列都不展开** ⇒ 富余宽度**全给最后一列**（实测最后一列
        //     拿到 870px，其余纹丝不动）⇒ 页面右边出现一大片空谷；
        //   - **一列展开** ⇒ 富余**全给它那一列**（同样 870px）；
        //   - **多列一起展开** ⇒ 富余**在它们之间平分**（实测 305/275/265/265）。
        //
        //   ⇒ 所以「哪个列展开」不是审美问题而是**算术问题**：想不出现空谷，
        //   就得**让好几列一起分**（跟容器页阶段 164 同一个配方）。
        if expand_cols.contains(&i) {
            col.set_expand(true);
        }
        view.append_column(&col);
    }
    (store, view)
}

/// 建**文件表**（阶段 140）：**名称列前面有图标** ＋ 权限用**通用写法**。
///
/// ⚠️ **为什么不用 [`flat_table`]**：那个表是「一列一个 `CellRendererText`」
///   ⇒ 放不了图标（`CellRendererText` **没有** `icon-name` 属性）⇒ 这里
///   单独给名称列前面加一条**窄图标列**（跟文件面板 `pane.rs` 同一套路）。
///
/// ⚠️ **没有「类型」列**：图标已经表达了（用户 2026-11-19：「文件、目录之类
///   的要有图标吧，不然看不出来是啥」）⇒ 省下的宽度全给文件名。
fn files_table() -> (gtk::TreeStore, gtk::TreeView) {
    let types: Vec<glib::Type> = vec![glib::Type::STRING; FC_N_COLS];
    let store = gtk::TreeStore::new(&types);
    let view = gtk::TreeView::new();
    view.set_model(Some(&store));

    // ---- 图标列（窄窄的一条，只放 16px 的图标）----
    let col_icon = gtk::TreeViewColumn::new();
    let r_pix = gtk::CellRendererPixbuf::new();
    col_icon.pack_start(&r_pix, false);
    col_icon.add_attribute(&r_pix, "icon-name", FC_ICON as i32);
    // ⚠️ **空模型时量出来的宽度会一直是 0** ⇒ 整列被压没（跟文件面板同一个坑）
    //   ⇒ 给个下限把位置先占住。
    col_icon.set_min_width(24);
    col_icon.set_resizable(false);
    view.append_column(&col_icon);

    // ---- 名称（展开列：文件名最长 ⇒ 宽度给它）----
    let col_name = gtk::TreeViewColumn::new();
    col_name.set_title(tr("名称"));
    col_name.set_resizable(true);
    col_name.set_min_width(220);
    col_name.set_expand(true);
    {
        let r = gtk::CellRendererText::new();
        r.set_ellipsize(gtk::pango::EllipsizeMode::End);
        col_name.pack_start(&r, false);
        // ⚠️ **必须显式绑 "text"**（不绑就是一片空白）。
        col_name.add_attribute(&r, "text", FC_NAME as i32);
    }
    view.append_column(&col_name);

    // ---- 大小（右对齐；跟文件面板的大列同一口径）----
    let col_size = gtk::TreeViewColumn::new();
    col_size.set_title(tr("大小"));
    col_size.set_resizable(true);
    col_size.set_min_width(90);
    {
        let r = gtk::CellRendererText::new();
        gtk::prelude::CellRendererExt::set_alignment(&r, 1.0, 0.5);
        col_size.set_alignment(1.0);
        col_size.pack_start(&r, false);
        col_size.add_attribute(&r, "text", FC_SIZE as i32);
    }
    view.append_column(&col_size);

    // ---- 权限（**通用写法**，十来个字符 ⇒ 不用给 250px）----
    let col_perm = gtk::TreeViewColumn::new();
    col_perm.set_title(tr("权限"));
    col_perm.set_resizable(true);
    col_perm.set_min_width(110);
    {
        let r = gtk::CellRendererText::new();
        r.set_family(Some("monospace"));
        col_perm.pack_start(&r, false);
        col_perm.add_attribute(&r, "text", FC_PERM as i32);
    }
    view.append_column(&col_perm);

    // ---- 修改时间（用户 2026-11-27：「容器文件管理中没有显示修改时间」）----
    //
    // ⚠️ **口径跟文件面板一样**（`util::mtime_str`）：今天只写时间、今年
    //   写「月-日 时:分」、别的年写全 ⇒ 列窄一点也放得下。
    let col_mtime = gtk::TreeViewColumn::new();
    col_mtime.set_title(tr("修改时间"));
    col_mtime.set_resizable(true);
    col_mtime.set_min_width(120);
    {
        let r = gtk::CellRendererText::new();
        r.set_ellipsize(gtk::pango::EllipsizeMode::End);
        col_mtime.pack_start(&r, false);
        col_mtime.add_attribute(&r, "text", FC_MTIME as i32);
    }
    view.append_column(&col_mtime);

    // ---- 符号链接指向（用户 2026-11-27：「最好能显示符号链接指向的文件」）----
    let col_link = gtk::TreeViewColumn::new();
    col_link.set_title(tr("指向"));
    col_link.set_resizable(true);
    col_link.set_min_width(140);
    {
        let r = gtk::CellRendererText::new();
        r.set_ellipsize(gtk::pango::EllipsizeMode::End);
        col_link.pack_start(&r, false);
        col_link.add_attribute(&r, "text", FC_LINK as i32);
    }
    view.append_column(&col_link);

    (store, view)
}

/// 把一张表包进**能滚动的页面**（表格占满 + 上下留白）。
fn flat_page(view: &gtk::TreeView) -> gtk::Box {
    flat_page_with_bar(view, None).0
}

/// 把一张表包进**能滚动的页面**（可选：表格上面加一条**操作条**）。
///
/// ⚠️ **为什么要有操作条**（阶段 120）：镜像段**没有详情区** ⇒ 推送 /
///    导出 / 打标签 / 删除这些**行操作**得有个落脚的地方 ⇒ 放在表格
///    正上方（选中哪一行就作用在哪一行）。
/// 一页「平表 + 工具条」，**外加一块错态空态**（镜像 / 卷 / 网络三页）。
///
/// ⚠️ **为什么也要空态**（阶段 135，用户 2026-11-19 报的）：这三页以前
///   失败时**只写底部一行小灰字** ⇒ 界面上是「一张空表 + 角落一行小字」
///   ⇒ 用户根本看不出「是没权限」还是「真的没有镜像」⇒ 这里给同样的
///   **大号错态**（标题按错误分类 + 解释 + 重试）。
///
/// 返回 `(整页, 错态空态)`——空态由调用方塞进 [`DockerUi`]（重试按钮
/// 要接到「重刷这一段」的回调上）。
fn flat_page_with_bar(
    view: &gtk::TreeView,
    bar: Option<&gtk::Box>,
) -> (gtk::Box, crate::emptystate::DynamicEmptyState) {
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_child(Some(view));
    // ⚠️ **空态盖在表格上**（跟容器页一样）⇒ 出错时是「一块说明」，
    //   不是「一张空表」。
    let empty = crate::emptystate::build_dynamic(&crate::emptystate::docker_empty());
    empty.root.set_visible(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroll));
    overlay.add_overlay(&empty.root);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_margin_bottom(6);
    if let Some(b) = bar {
        root.append(b);
    }
    root.append(&overlay);
    (root, empty)
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
        // ⚠️ **阶段 161**：同上 ⇒ 记一笔，待会儿再刷一次。
        ui.images_pending.set(true);
        return; // 上一次还没回来 ⇒ 不叠加
    }
    ui.images_pending.set(false);
    ui.images_busy.set(true);
    ui.set_segment_status(SEG_IMAGES, tr("正在读取远程镜像…"));
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
        // ⚠️ **阶段 161**：同上 ⇒ 记一笔，待会儿再刷一次。
        ui.volumes_pending.set(true);
        return;
    }
    ui.volumes_pending.set(false);
    ui.volumes_busy.set(true);
    ui.set_segment_status(SEG_VOLUMES, tr("正在读取远程卷…"));
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
        // ⚠️ **阶段 161**：同上 ⇒ 记一笔，待会儿再刷一次。
        ui.networks_pending.set(true);
        return;
    }
    ui.networks_pending.set(false);
    ui.networks_busy.set(true);
    ui.set_segment_status(SEG_NETWORKS, tr("正在读取远程网络…"));
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
    *ui.images_all.borrow_mut() = v.to_vec();
    refilter_images(ui);
}

/// 重新按搜索词填**镜像表**（UI 审查 D12）。
///
/// ⚠️ **为什么镜像页最需要筛选**：本机 44 个镜像里 30 个是同一个 tag 的
///   重复 ⇒ 仓库列重复 30 遍 ⇒ 真实服务器上镜像轻松上百个 ⇒ 没有筛选
///   就只能肉眼扫。
fn refilter_images(ui: &Rc<DockerUi>) {
    // ⚠️ **保住选中**（阶段 120）：自动刷新每 7 秒来一次 ⇒ 每次都把选中
    //    清掉的话，用户**根本点不到操作按钮**（按钮会跟着变灰）⇒
    //    重建以后**按名字找回来**。
    let keep = ui.images_selected.borrow().clone();
    // ⚠️ **重建那一段算「程序性」**（阶段 166）：`store.clear()` 会逐行发
    //   `cursor-changed` ⇒ 光标处理器会把选中清空 ⇒ 先挡掉。
    ui.restoring.set(true);
    let q = ui.images_query.borrow().to_ascii_lowercase();
    let all = ui.images_all.borrow().clone();
    ui.images_store.clear();
    let mut found: Option<gtk::TreeIter> = None;
    let mut shown = 0usize;
    for i in all.iter() {
        if !q.is_empty() {
            let hit = i.repository.to_ascii_lowercase().contains(&q)
                || i.tag.to_ascii_lowercase().contains(&q)
                || i.id.to_ascii_lowercase().contains(&q);
            if !hit {
                continue;
            }
        }
        shown += 1;
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
        if !keep.is_empty() && image_full_name(i) == keep {
            found = Some(row);
        }
    }
    if let Some(it) = found {
        ui.images_view.selection().select_iter(&it);
    }
    ui.restoring.set(false);
    // ⚠️ **自己把选中写回去**（阶段 166）：实测 GTK4 **单选模式**下
    //   `select_iter()` **不发** `cursor-changed` ⇒ 只靠信号的话，选中
    //   记不回来 ⇒ 按钮还是灰的 ⇒ 用户点不到。
    *ui.images_selected.borrow_mut() = if found.is_some() {
        keep
    } else {
        String::new()
    };
    update_images_actions(ui);
    let total = all.len();
    let msg = if shown == total {
        trf!("共 {n} 个镜像", n = total)
    } else {
        trf!("显示 {s} / 共 {n} 个镜像", s = shown, n = total)
    };
    ui.set_segment_status(SEG_IMAGES, &msg);
}

/// 镜像的**完整名字**（`仓库:标签`；悬空镜像只有仓库）。
///
/// ⚠️ **为什么单独一个函数**：表格里是**仓库**与**标签**两列 ⇒ 命令要的是
///    **一个**名字 ⇒ 拼法得**只有一处**（拼错了 docker 就说不认）。
fn image_full_name(i: &docker::ImageInfo) -> String {
    if i.tag.is_empty() || i.tag == "<none>" {
        i.repository.clone()
    } else {
        format!("{}:{}", i.repository, i.tag)
    }
}

/// 填**卷表** ＋ 更新状态行（阶段 115）。
fn apply_volumes(ui: &Rc<DockerUi>, v: &[docker::VolumeInfo]) {
    *ui.volumes_all.borrow_mut() = v.to_vec();
    refilter_volumes(ui);
}

/// 重新按搜索词填**卷表**（UI 审查 D12：这三页以前**一点筛选都没有**）。
fn refilter_volumes(ui: &Rc<DockerUi>) {
    // ⚠️ **保住选中**（阶段 166）：自动刷新每 7 秒来一次 ⇒ 每次都把选中
    //    清掉的话，用户**根本点不到删除按钮**（按钮会跟着变灰）⇒
    //    重建以后**按名字找回来**（跟 `refilter_images` 一个套路）。
    let keep = ui.volumes_selected.borrow().clone();
    // ⚠️ **重建那一段算「程序性」**（阶段 166）：`store.clear()` 会逐行发
    //   `cursor-changed` ⇒ 光标处理器会把选中清空 ⇒ 先挡掉。
    ui.restoring.set(true);
    let q = ui.volumes_query.borrow().to_ascii_lowercase();
    let all = ui.volumes_all.borrow().clone();
    ui.volumes_store.clear();
    let mut found: Option<gtk::TreeIter> = None;
    let mut shown = 0usize;
    for x in all.iter() {
        if !q.is_empty() {
            let hit = x.name.to_ascii_lowercase().contains(&q)
                || x.driver.to_ascii_lowercase().contains(&q);
            if !hit {
                continue;
            }
        }
        shown += 1;
        let row = ui.volumes_store.append(None);
        ui.volumes_store.set(&row, &[(0u32, &x.name), (1u32, &x.driver)]);
        if !keep.is_empty() && x.name == keep {
            found = Some(row);
        }
    }
    // ⚠️ **把选中找回来**（重建前记的名字）⇒ 自动刷新不会把用户的选择顶掉。
    if let Some(it) = found {
        ui.volumes_view.selection().select_iter(&it);
    }
    ui.restoring.set(false);
    // ⚠️ **自己把选中写回去**（阶段 166）：GTK4 单选模式下 `select_iter()`
    //   **不发** `cursor-changed` ⇒ 只靠信号的话记不回来 ⇒ 按钮还是灰的。
    *ui.volumes_selected.borrow_mut() = if found.is_some() {
        keep
    } else {
        String::new()
    };
    update_volumes_actions(ui);
    let total = all.len();
    let msg = if shown == total {
        trf!("共 {n} 个卷", n = total)
    } else {
        trf!("显示 {s} / 共 {n} 个卷", s = shown, n = total)
    };
    ui.set_segment_status(SEG_VOLUMES, &msg);
}

/// 填**网络表** ＋ 更新状态行（阶段 115）。
fn apply_networks(ui: &Rc<DockerUi>, v: &[docker::NetInfo]) {
    *ui.networks_all.borrow_mut() = v.to_vec();
    refilter_networks(ui);
}

/// 重新按搜索词填**网络表**（UI 审查 D12）。
fn refilter_networks(ui: &Rc<DockerUi>) {
    // ⚠️ **保住选中**（阶段 166）：跟卷页一个套路 ⇒ 自动刷新不会把用户
    //    选中的网络顶掉（顶掉了删除按钮就变灰）。
    let keep = ui.networks_selected.borrow().clone();
    // ⚠️ **重建那一段算「程序性」**（阶段 166）：`store.clear()` 会逐行发
    //   `cursor-changed` ⇒ 光标处理器会把选中清空 ⇒ 先挡掉。
    ui.restoring.set(true);
    let q = ui.networks_query.borrow().to_ascii_lowercase();
    let all = ui.networks_all.borrow().clone();
    ui.networks_store.clear();
    let mut found: Option<gtk::TreeIter> = None;
    let mut shown = 0usize;
    for x in all.iter() {
        if !q.is_empty() {
            let hit = x.name.to_ascii_lowercase().contains(&q)
                || x.driver.to_ascii_lowercase().contains(&q)
                || x.scope.to_ascii_lowercase().contains(&q);
            if !hit {
                continue;
            }
        }
        shown += 1;
        let row = ui.networks_store.append(None);
        ui.networks_store
            .set(&row, &[(0u32, &x.name), (1u32, &x.driver), (2u32, &x.scope)]);
        if !keep.is_empty() && x.name == keep {
            found = Some(row);
        }
    }
    // ⚠️ **把选中找回来**（重建前记的名字）。
    if let Some(it) = found {
        ui.networks_view.selection().select_iter(&it);
    }
    ui.restoring.set(false);
    // ⚠️ **自己把选中写回去**（阶段 166）：见 `refilter_volumes` 的说明。
    *ui.networks_selected.borrow_mut() = if found.is_some() {
        keep
    } else {
        String::new()
    };
    update_networks_actions(ui);
    let total = all.len();
    let msg = if shown == total {
        trf!("共 {n} 个网络", n = total)
    } else {
        trf!("显示 {s} / 共 {n} 个网络", s = shown, n = total)
    };
    ui.set_segment_status(SEG_NETWORKS, &msg);
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
///
/// ⚠️ **阶段 140**：多填两列 —— **图标名**（名称列前面那条窄列用）与
///   **类型键**（`dir` / `file` …）；权限改成**通用写法**（不翻译）。
fn apply_files(ui: &Rc<DockerUi>, v: &[docker::ContainerFile]) {
    ui.files.store.clear();
    // ⚠️ **阶段 162：先加一行「..」**（回到上一级）。
    //
    // ⚠️ **为什么要有**（用户 2026-11-24：「容器的文件管理，文件夹中没有
    //   ".."，进入文件夹就没法出来了」）：**文件管理器都有这一行** ⇒
    //   双击它就往上走 ⇒ 不用去工具条上找那个不起眼的小箭头按钮。
    // ⚠️ **根目录不加**（`/` 的上一级还是 `/` ⇒ 加了就是死循环）。
    let cur = ui.files_path.borrow().clone();
    if !cur.is_empty() && cur != "/" {
        let row = ui.files.store.append(None);
        ui.files.store.set(
            &row,
            &[
                (FC_ICON as u32, &"go-up-symbolic"),
                (FC_NAME as u32, &".."),
                (FC_SIZE as u32, &tr("—")),
                (FC_PERM as u32, &""),
                (FC_MTIME as u32, &""),
                (FC_LINK as u32, &""),
                (FC_TYPE as u32, &tr("上级目录")),
                // ⚠️ **类型键是 "up"**（不是 "dir"）⇒ 双击时走「回上一级」
                //   而不是「进这个叫 `..` 的目录」（那会拼出 `/media/..`）。
                (FC_KIND as u32, &"up"),
            ],
        );
    }
    for f in v {
        let name = f.name.clone();
        let kind_label = tr(f.kind.label()).to_string();
        // ⚠️ **目录的大小是 inode 大小**（4.0 KB）⇒ 纯噪音 ⇒ 给「—」
        //   （UI 审查 D11）。
        let size = if f.kind == docker::FileKind::Dir {
            tr("—").to_string()
        } else {
            crate::util::human_size(f.size)
        };
        // ⚠️ **权限不翻成中文**（用户 2026-11-19）⇒ 原样给（见 [`perm_text`]）。
        let perms = perm_text(&f.perms);
        let key = file_kind_key(f.kind).to_string();
        let icon = file_icon(f.kind).to_string();
        // ⚠️ **修改时间**（用户 2026-11-27）⇒ 跟文件面板同一口径
        //   （`util::mtime_str`）；解析不出来 ⇒ 空（别显示 1970）。
        let mtime = if f.mtime > 0 {
            crate::util::mtime_str(f.mtime)
        } else {
            String::new()
        };
        // ⚠️ **符号链接指向**（用户 2026-11-27：「最好能显示符号链接指向的
        //   文件」）⇒ 不是链接就空着。
        let link = f.link_target.clone().unwrap_or_default();
        let row = ui.files.store.append(None);
        ui.files.store.set(
            &row,
            &[
                (FC_ICON as u32, &icon),
                (FC_NAME as u32, &name),
                (FC_SIZE as u32, &size),
                (FC_PERM as u32, &perms),
                (FC_MTIME as u32, &mtime),
                (FC_LINK as u32, &link),
                (FC_TYPE as u32, &kind_label),
                (FC_KIND as u32, &key),
            ],
        );
    }
    ui.files.status.set_text(&trf!("{n} 个条目", n = v.len()));
}

/// 当前选中的行（**名字 ＋ 类型键**）。
fn files_selected(ui: &Rc<DockerUi>) -> Option<(String, String)> {
    let sel = ui.files.view.selection();
    let (model, iter) = sel.selected()?;
    let name: String = model.get(&iter, FC_NAME as i32);
    let kind: String = model.get(&iter, FC_KIND as i32);
    if name.is_empty() {
        return None;
    }
    Some((name, kind))
}

/// **某一行**（按 path）的**名字 ＋ 类型键**。
///
/// ⚠️ **为什么要有**（阶段 162）：双击的回调里**光有 path** ⇒
///   以前是去问「**当前选中**的那一行」⇒ **点的是哪一行不一定等于
///   选中的是哪一行**（程序化发信号、或选中被别处改掉时就会错位）
///   ⇒ 现在**按 path 取** ⇒ 动的一定是用户双击的那一行。
fn files_row_at(ui: &Rc<DockerUi>, path: &gtk::TreePath) -> Option<(String, String)> {
    let model = ui.files.store.clone();
    let iter = model.iter(path)?;
    let name: String = model.get(&iter, FC_NAME as i32);
    let kind: String = model.get(&iter, FC_KIND as i32);
    if name.is_empty() {
        return None;
    }
    Some((name, kind))
}

/// 那一行的**符号链接目标**（不是链接 ⇒ 空串）。
///
/// ⚠️ **用户 2026-11-27**：「最好能显示符号链接指向的文件」⇒ 目标存进
///   表里（[`FC_LINK`]）⇒ 双击跟随时从这里取。
fn files_row_link(ui: &Rc<DockerUi>, path: &gtk::TreePath) -> String {
    let model = ui.files.store.clone();
    let Some(iter) = model.iter(path) else {
        return String::new();
    };
    model.get(&iter, FC_LINK as i32)
}

/// 符号链接的**目标绝对路径**。
///
/// ⚠️ **相对目标按「链接所在目录」解**（POSIX 语义）：`ls -l` 给的目标是
///   `../lib/x.so` 这种相对写法 ⇒ 相对**链接自己**的目录，不是相对当前
///   工作目录 ⇒ 不规整就会跳错地方。
fn files_link_target_abs(ui: &Rc<DockerUi>, link_name: &str, target: &str) -> String {
    files_link_target_abs_from(&files_remote_path(ui, link_name), target)
}

/// 同上，但**基准路径**直接给（追链接链时用：下一段的基准是**上一段的目标**）。
fn files_link_target_abs_from(base_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        return crate::util::path_normalize(target);
    }
    // ⚠️ **相对目标按「链接自己所在目录」解**（不是当前工作目录）。
    let link_dir = crate::util::path_parent(base_path);
    crate::util::path_normalize(&crate::util::path_join(&link_dir, target))
}

/// **跟随符号链接**（用户 2026-11-27：「双击符号链接不能跟随」）。
///
/// ⚠️ **为什么要多问一次**：目录列表里的 `ls -l` 给的是**链接自己**的类型
///   （`l`）⇒ 不知道目标是目录还是文件 ⇒ 对**目标路径**再问一次。
///
/// ⚠️ **为什么要追链**（链接套链接）：目标本身可能**还是**一个链接 ⇒
///   那次问回来的类型还是 `link` ⇒ 得**再追一层**（最多追
///   [`LINK_FOLLOW_MAX`] 层 ⇒ 防止两个链接互相指形成死循环）。
fn files_follow_link(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    container: &str,
    link_path: &str,
    target_abs: &str,
) {
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let container = container.to_string();
    let link_path = link_path.to_string();
    let target_abs = target_abs.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let (r, final_path) = match shared_session(&opts, pw).await {
            Ok(session) => {
                // ⚠️ **追链**：每追一层，基准路径就换成**上一段的目标**
                //   （相对目标要相对**它自己所在的那个目录**解）。
                let mut cur = target_abs.clone();
                let mut out: Result<String, String> = Err(String::new());
                for _ in 0..LINK_FOLLOW_MAX {
                    // ⚠️ **每层的基准都是「当前这一段」**（`cur`）。
                    let cmd = docker::stat_kind_cmd(&container, &cur);
                    let res = match docker::run_cmd(&session, &cmd).await {
                        // ⚠️ `ls -ld` 的输出与 `ls -l` 同格式 ⇒ 借 `parse_ls` 解析。
                        Ok((0, text)) => docker::parse_ls(&text).into_iter().next(),
                        Ok((_, text)) => {
                            out = Err(docker::classify_ls_failure(&text));
                            break;
                        }
                        Err(e) => {
                            out = Err(e.0);
                            break;
                        }
                    };
                    let Some(f) = res else {
                        out = Err(crate::i18n::tr("链接指向的文件不存在").to_string());
                        break;
                    };
                    let key = file_kind_key(f.kind);
                    if key != "link" {
                        out = Ok(key.to_string());
                        break;
                    }
                    // ⚠️ **还是链接 ⇒ 再追一层**。
                    let Some(next) = f.link_target.clone() else {
                        out = Err(crate::i18n::tr("链接指向的文件不存在").to_string());
                        break;
                    };
                    cur = files_link_target_abs_from(&cur, &next);
                }
                (out, cur)
            }
            Err(e) => (Err(e), target_abs.clone()),
        };
        let _ = tx.send_blocking(Msg::LinkKind {
            container,
            // ⚠️ **发的是「追完链之后的最终路径」**（链式链接时 ≠ 原来的
            //   目标）⇒ 进去/打开的才是**真正要去的那个东西**。
            path: final_path,
            link: link_path,
            r,
        });
    });
}

/// 选中条目的**远程绝对路径**。
///
/// ⚠️ **基准是「当前目录」**（[`DockerUi::files_path`]）⇒ 那个目录
///   **成功列出来过**才算数（阶段 159）⇒ 拼出来的路径一定指得过去。
fn files_remote_path(ui: &Rc<DockerUi>, name: &str) -> String {
    let dir = ui.files_path.borrow().clone();
    if dir.is_empty() {
        return format!("/{name}");
    }
    // ⚠️ **用 path_join**（远端是 POSIX 路径）：它自己处理结尾的 '/' ⇒
    //   "/" ＋ "media" = "/media"，"/media" ＋ "proc" = "/media/proc"。
    crate::util::path_join(&dir, name)
}

/// **文件页是不是正开着**。
fn files_page_visible(ui: &Rc<DockerUi>) -> bool {
    ui.files_window
        .borrow()
        .as_ref()
        .map(|w| w.is_visible())
        .unwrap_or(false)
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
///
/// ⚠️ **阶段 159：这里**只记「正在请求的目录」**（`files_req`）**，
///   **不动**当前目录（`files_path`）**和路径栏**：
/// - 目录**列不出来**（不存在 / 没权限 / 容器没跑）⇒ 回包是失败 ⇒
///   路径栏与表格**都留在上一个成功的目录**（见 `Msg::Files` 的处理）；
/// - 以前这里**乐观地**先把路径栏改了 ⇒ 失败后路径栏停在那个进不去的目录，
///   而表格还是旧目录的行 ⇒ 用户以为「没进去」⇒ 又在旧表格里点一行
///   ⇒ **地址栏越拼越长**（用户 2026-11-24 报的
///   `/media/proc/root/sbin/srv/tmp/srv/sbin`）。
fn files_goto(st: &Rc<PanelState>, ui: &Rc<DockerUi>, container: &str, path: &str) {
    *ui.files_name.borrow_mut() = container.to_string();
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
    // ⚠️ **阶段 159：登记「正在请求的目录」**（回包靠它认「是不是我要的那个」）
    //   ⇒ **每次发请求都要登记**（不只 `files_goto`：传完之后的重列也走这儿）。
    *ui.files_req.borrow_mut() = path.to_string();
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
///
/// ⚠️ **阶段 167**：`ui` 用来**记住窗与正文控件**（冒烟要量正文高度）。
fn show_file_window(
    st: Option<&Rc<PanelState>>,
    path: &str,
    content: &str,
    ui: Option<&Rc<DockerUi>>,
) {
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
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::Word);
    view.set_left_margin(6);
    view.set_right_margin(6);
    view.set_top_margin(4);
    view.set_bottom_margin(4);
    view.buffer().set_text(content);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    // ⚠️ **阶段 167：必须 `vexpand` ＋ `hexpand`**（用户 2026-11-25：
    //   「查看容器里面的文件，现在只能显示1行」）。
    //
    //   **根因**：`ScrolledWindow` 放在**竖直 Box** 里却**没有设
    //   `vexpand`** ⇒ GTK4 的 Box 只把**富余高度**分给**扩展型**子控件
    //   ⇒ 它只拿到 `TextView` 的**最小高度**（= 1 行）⇒ 正文只显示 1 行，
    //   下面全是空白（用户截图里滚动条正好压在第 1 行下面）。
    //   ⇒ `vexpand` 才是「占满剩余空间」的开关。
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_child(Some(&view));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_bottom(6);
    root.append(&title);
    root.append(&scroll);
    win.set_child(Some(&root));
    // ⚠️ **阶段 167：记住句柄**（冒烟量正文高度用）。
    if let Some(u) = ui {
        *u.file_view_window.borrow_mut() = Some(win.clone());
        *u.file_view_text.borrow_mut() = Some(view.clone());
    }
    win.present();
}

/* ---------------- 阶段 116：测试钩子 ---------------- */

/// 测试钩子：打开「文件」窗（**真的开** ＋ 列目录）。
///
/// ⚠️ **阶段 165**：文件是独立窗口（不再是页签）⇒ 这里开的是**那扇窗**。
pub fn files_show_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let name = ui.selected.borrow().clone();
    if name.is_empty() {
        return;
    }
    open_files_window(st, ui, &name);
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
            let g = |c: usize| -> String { model.get(&iter, c as i32) };
            out.push((
                g(FC_NAME),
                g(FC_TYPE),
                g(FC_SIZE),
                g(FC_PERM),
            ));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：**某一行名字列的 markup**（阶段 162）。
///
/// 干什么用：冒烟要验「compose 项目行的名字**带标记**、容器行的**没有**」。
pub fn row_name_markup_for_test(st: &Rc<PanelState>, name: &str) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return String::new() };
    let Some(it) = ui.find_iter(name) else { return String::new() };
    ui.model.get(&it, COL_NAME_MK as i32)
}

/// 测试钩子：**双击文件表里的某一行**（阶段 162：验「..」能回到上一级）。
///
/// ⚠️ **走的是真的 `row-activated` 信号**（跟双击同一条路）⇒
///   「..」的处理逻辑被真的验到（而不是绕过它）。
pub fn files_activate_row_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let model = ui.files.store.clone();
    // ⚠️ **迭代器要自己推进**（`iter_next` 返回 bool）⇒ 写成
    //   `while let Some(..) = iter` 会**永远停在第一行** ⇒ **死循环**
    //   （2026-11-24 反向验证时踩到：找不到那一行就卡死 ⇒ 冒烟挂住）。
    if let Some(mut it) = model.iter_first() {
        loop {
            let n: String = model.get(&it, FC_NAME as i32);
            if n == name {
                let path = model.path(&it);
                ui.files.view.emit_row_activated(&path, None);
                return true;
            }
            if !model.iter_next(&mut it) {
                break;
            }
        }
    }
    false
}

/// 测试钩子：文件表每一行的**图标名**（阶段 140）。
///
/// 干什么用：冒烟要钉住「目录 / 文件 / 链接各有各的图标」（用户 2026-11-19：
///   「文件、目录之类的要有图标吧，不然看不出来是啥」）。
pub fn files_icons_for_test(st: &Rc<PanelState>) -> Vec<String> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    let model = ui.files.store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let icon: String = model.get(&iter, FC_ICON as i32);
            out.push(icon);
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：文件页**工具条上的按钮**（tooltip ＋ 有没有文字）。
///
/// 干什么用：冒烟要钉住「按钮是**纯图标**（没有文字）＋ 每个都有 tooltip」
///   （用户 2026-11-19：「大按钮、框框占太多空间」）。
pub fn files_toolbar_for_test(st: &Rc<PanelState>) -> Vec<(String, bool)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.files
        .btns
        .iter()
        .map(|b| {
            let tip = b.tooltip_text().unwrap_or_default().to_string();
            // ⚠️ **有文字 = 不是纯图标**（按钮的 child 是 Label 时才有 label）。
            let has_label = b.label().is_some();
            (tip, has_label)
        })
        .collect()
}

/// 测试钩子：文件表**每一列的标题 ＋ 是否可见**（阶段 140）。
///
/// 干什么用：冒烟要钉住「**没有「类型」列**了」（图标已经表达了 ⇒ 省宽度）。
pub fn files_cols_for_test(st: &Rc<PanelState>) -> Vec<(String, bool)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    ui.files
        .view
        .columns()
        .iter()
        .map(|c| {
            (
                c.title().to_string(),
                c.is_visible(),
            )
        })
        .collect()
}

/// 测试钩子：文件表的**修改时间 ＋ 符号链接指向**（每行）。
///
/// ⚠️ **干什么用**（用户 2026-11-27）：「容器文件管理中没有显示修改时间」
///   ＋「最好能显示符号链接指向的文件」⇒ 冒烟要能读到这两列的内容。
///
/// 返回 `(名字, 修改时间, 指向)`。
pub fn files_mtime_link_for_test(st: &Rc<PanelState>) -> Vec<(String, String, String)> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    let model = ui.files.store.clone();
    if let Some(mut iter) = model.iter_first() {
        loop {
            let g = |c: usize| -> String { model.get(&iter, c as i32) };
            out.push((g(FC_NAME), g(FC_MTIME), g(FC_LINK)));
            if !model.iter_next(&mut iter) {
                break;
            }
        }
    }
    out
}

/// 测试钩子：**compose 状态栏**的当前文字。
pub fn compose_status_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref().map(|u| u.status.text().to_string()).unwrap_or_default()
}

/// 测试钩子：走一遍「编辑 compose 文件」的**真动作**（状态栏 + 打开编辑器）。
///
/// ⚠️ **返回 `(写进去的那句, 打开期间状态栏里真的看到的那句)`** ⇒ 冒烟能
///   同时证明「**确实写过**」「**写进去时用户真的看得到**」「**确实清掉
///   了**」（三样缺一，那条「编辑器一打开状态栏就该清空」就没被守住）。
///
/// ⚠️ **「打开编辑器」那步注入成「什么都不干」**：没连服务器时真开会弹
///   **模态**错误框 ⇒ 冒烟卡住 ⇒ CI 上跑不了（见
///   [`open_compose_file_at_with`]）。
pub fn open_compose_file_at_for_test(
    st: &Rc<PanelState>,
    path: &str,
    work_dir: &str,
) -> (String, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref().cloned() else {
        return (String::new(), String::new());
    };
    // ⚠️ **在「打开」那一步偷看一眼状态栏** ⇒ 证明它**真的显示过**。
    let mut seen = String::new();
    let written = open_compose_file_at_with(st, &ui, path, work_dir, |_, _, _| {
        seen = ui.status.text().to_string();
    });
    (written, seen)
}

/// 测试钩子：文件页**当前目录**。
pub fn files_path_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.files_path.borrow().clone())
        .unwrap_or_default()
}

/// 测试钩子：文件页**路径栏上显示的文字**（阶段 159）。
///
/// ⚠️ **跟 [`files_path_for_test`] 分开**：那个是「内部认定的当前目录」，
///   这个是**用户眼前看到的那个输入框**⇒ 用户报的现象（「路径名会在地址栏
///   累积」）就发生在这个输入框里 ⇒ 冒烟要盯的是它。
pub fn files_entry_for_test(st: &Rc<PanelState>) -> String {
    let slot = st.docker_ui.borrow();
    slot.as_ref()
        .map(|u| u.files.path.text().to_string())
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

/* ---------------- 阶段 120：镜像的操作（推送 / 导出 / 打标签 / 删除） ---------------- */

/// 镜像表**选中变了** ⇒ 记住选中的是哪个 ＋ 更新操作条。
///
/// ⚠️ **为什么自己记**：GTK 的「选中」只是一行 ⇒ 得把它翻译成
///    `docker` 认的**镜像名**（`仓库:标签`）⇒ 后面的命令才用得上。
fn on_images_cursor_changed(ui: &Rc<DockerUi>, view: &gtk::TreeView) {
    // ⚠️ **重建表格时**那些「假的光标变化」要挡掉（见 `refilter_images`）。
    if ui.restoring.get() {
        return;
    }
    let mut name = String::new();
    if let Some((model, iter)) = view.selection().selected() {
        let repo: String = model.get(&iter, 0);
        let tag: String = model.get(&iter, 1);
        if !repo.is_empty() {
            name = if tag.is_empty() || tag == "<none>" {
                repo
            } else {
                format!("{repo}:{tag}")
            };
        }
    }
    *ui.images_selected.borrow_mut() = name;
    update_images_actions(ui);
}

/// 刷新**镜像操作条**（选中哪个 ＋ 按钮能不能点）。
fn update_images_actions(ui: &Rc<DockerUi>) {
    let sel = ui.images_selected.borrow().clone();
    let has = !sel.is_empty();
    ui.images_push_btn.set_sensitive(has);
    ui.images_export_btn.set_sensitive(has);
    ui.images_tag_btn.set_sensitive(has);
    ui.images_remove_btn.set_sensitive(has);
    if has {
        ui.images_hint.set_text(&sel);
    } else {
        ui.images_hint.set_text(tr("选中一个镜像才能操作"));
    }
}

/// **推送**镜像（**写操作** ⇒ 二次确认）。
fn images_push(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    let msg = trf!("确定要把镜像「{img}」推到仓库吗？", img = image);
    let (st2, u2) = (st.clone(), ui.clone());
    let image = image.to_string();
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
        tr("请确认"),
        &msg,
        tr("推送"),
        move |ok| {
            if !ok {
                return;
            }
            run_image_push(&st2, &u2, &image);
        },
    );
}

/// **推送**的后台部分（远端跑 `docker push`）。
fn run_image_push(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    ui.set_segment_status(SEG_IMAGES, &trf!("正在推送镜像「{img}」…", img = image));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let image = image.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::push_image(&session, &image).await {
                Ok(_) => trf!("镜像「{img}」推送完成", img = image),
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::ImageOp { msg, done: true });
    });
}

/// **导出**镜像到本地文件（`docker save` 的归档流）。
///
/// ⚠️ **不是写操作**（不动远端）⇒ 不用二次确认；但会**写本地文件**。
fn images_export(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    let top = host_window(ui, st);
    let mut b = gtk::FileChooserNative::builder()
        .modal(true)
        .title(tr("把镜像导出到哪个文件"))
        .action(gtk::FileChooserAction::Save)
        .accept_label(tr("导出"))
        .cancel_label(tr("取消"));
    if let Some(p) = top.as_ref() {
        b = b.transient_for(p);
    }
    let ch = b.build();
    // ⚠️ **默认文件名**：镜像名里有 `/` 与 `:` ⇒ 换成 `_`（省得用户在
    //    文件名里看到奇怪的东西）。
    let mut def = image.replace('/', "_").replace(':', "_");
    if !def.ends_with(".tar") {
        def.push_str(".tar");
    }
    ch.set_current_name(&def);
    let (st2, u2) = (st.clone(), ui.clone());
    let image = image.to_string();
    ch.connect_response(move |c, r| {
        if r == gtk::ResponseType::Accept {
            if let Some(f) = c.file() {
                if let Some(p) = f.path() {
                    run_image_export(&st2, &u2, &image, &p);
                }
            }
        }
        c.destroy();
    });
    ch.show();
}

/// **导出**的后台部分（远端跑 `docker save` ⇒ 归档流直接落本地文件）。
fn run_image_export(
    st: &Rc<PanelState>,
    ui: &Rc<DockerUi>,
    image: &str,
    dest: &std::path::Path,
) {
    ui.set_segment_status(SEG_IMAGES, &trf!("正在导出镜像「{img}」…", img = image));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let image = image.to_string();
    let dest = dest.to_path_buf();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => {
                // ⚠️ **进度限流 100ms**（跟容器文件传输一个套路）⇒ 数字
                //   每次都更新、界面通知变稀。
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
                    let _ = tx2.send_blocking(Msg::ImageOp {
                        msg: trf!("正在导出… 已收 {n} 字节", n = n),
                        done: false,
                    });
                });
                match docker::save_image_to(&session, &image, &dest, &prog).await {
                    Ok(total) => trf!(
                        "镜像「{img}」已导出到 {p}（{n} 字节）",
                        img = image,
                        p = dest.display().to_string(),
                        n = total
                    ),
                    Err(e) => e.0,
                }
            }
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::ImageOp { msg, done: true });
    });
}

/// **打标签**（**写操作** ⇒ 先问标签名、再二次确认）。
fn images_tag(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    let (st2, u2) = (st.clone(), ui.clone());
    let image = image.to_string();
    let parent = host_window(ui, st);
    let parent2 = parent.clone();
    crate::panel::dialogs::ask_text(
        parent.as_ref(),
        tr("给镜像另打一个标签"),
        "",
        move |target| {
            let Some(target) = target else { return };
            let msg = trf!(
                "确定要把镜像「{img}」打成「{t}」吗？",
                img = image,
                t = target
            );
            let (st3, u3) = (st2.clone(), u2.clone());
            let image2 = image.clone();
            crate::panel::dialogs::confirm_title(
                parent2.as_ref(),
                tr("请确认"),
                &msg,
                tr("打标签"),
                move |ok| {
                    if !ok {
                        return;
                    }
                    run_image_tag(&st3, &u3, &image2, &target);
                },
            );
        },
    );
}

/// **打标签**的后台部分（远端跑 `docker tag`）。
fn run_image_tag(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str, target: &str) {
    ui.set_segment_status(SEG_IMAGES, &trf!("正在给镜像「{img}」打标签…", img = image));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let image = image.to_string();
    let target = target.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::tag_image(&session, &image, &target).await {
                Ok(()) => trf!("镜像「{img}」已经打上标签「{t}」", img = image, t = target),
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::ImageOp { msg, done: true });
    });
}

/// **删除**镜像（**写操作** ⇒ 二次确认）。
fn images_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    let msg = trf!(
        "确定要删除镜像「{img}」吗？（删了就没了）",
        img = image
    );
    let (st2, u2) = (st.clone(), ui.clone());
    let image = image.to_string();
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
        tr("请确认"),
        &msg,
        tr("删除"),
        move |ok| {
            if !ok {
                return;
            }
            run_image_remove(&st2, &u2, &image);
        },
    );
}

/// **删除**的后台部分（远端跑 `docker rmi`）。
fn run_image_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, image: &str) {
    ui.set_segment_status(SEG_IMAGES, &trf!("正在删除镜像「{img}」…", img = image));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let image = image.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::remove_image(&session, &image).await {
                Ok(()) => trf!("镜像「{img}」已删除", img = image),
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::ImageOp { msg, done: true });
    });
}

/* ---------------- 阶段 120：镜像操作的测试钩子 ---------------- */

/// 测试钩子：镜像的**操作条**（阶段 120）。
///
/// 返回 `(四个按钮的文案, 能不能点, 当前选中的镜像)`。
pub fn images_actions_for_test(st: &Rc<PanelState>) -> (Vec<String>, bool, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return (Vec::new(), false, String::new());
    };
    let labels = vec![
        ui.images_push_btn.label().unwrap_or_default().to_string(),
        ui.images_export_btn
            .label()
            .unwrap_or_default()
            .to_string(),
        ui.images_tag_btn.label().unwrap_or_default().to_string(),
        ui.images_remove_btn
            .label()
            .unwrap_or_default()
            .to_string(),
    ];
    let sensitive = ui.images_push_btn.is_sensitive();
    (labels, sensitive, ui.images_selected.borrow().clone())
}

/// 测试钩子：**选中**镜像表里的第几行（阶段 120；0 = 第一行）。
///
/// ⚠️ **真的选中**（不是只记一个变量）⇒ 顺带验「选中 ⇒ 按钮变可点」
///    那根线**真的接上了**。
pub fn images_select_for_test(st: &Rc<PanelState>, idx: u32) -> String {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return String::new();
    };
    let path = gtk::TreePath::from_indices(&[idx as i32]);
    gtk::prelude::TreeViewExt::set_cursor(&ui.images_view, &path, None, false);
    ui.images_view.selection().select_path(&path);
    ui.images_selected.borrow().clone()
}

/// 测试钩子：**打标签**（阶段 120；跳过对话框 ⇒ 直接跑）。
pub fn images_tag_for_test(st: &Rc<PanelState>, target: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let image = ui.images_selected.borrow().clone();
    if image.is_empty() {
        return;
    }
    run_image_tag(st, ui, &image, target);
}

/// 测试钩子：**删除**镜像（阶段 120；跳过对话框 ⇒ 直接跑）。
pub fn images_remove_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let image = ui.images_selected.borrow().clone();
    if image.is_empty() {
        return;
    }
    run_image_remove(st, ui, &image);
}

/// 测试钩子：**推送**镜像（阶段 120；跳过对话框 ⇒ 直接跑）。
pub fn images_push_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let image = ui.images_selected.borrow().clone();
    if image.is_empty() {
        return;
    }
    run_image_push(st, ui, &image);
}

/// 测试钩子：**推送**一个**指定的**镜像名（阶段 120）。
///
/// ⚠️ **为什么要「指定名字」**：冒烟不能真往公共仓库推东西 ⇒ 用一个
///    **不存在的镜像名** ⇒ docker 立刻失败 ⇒ 正好验「失败要给中文原因」。
pub fn images_push_named_for_test(st: &Rc<PanelState>, image: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    run_image_push(st, ui, image);
}

/// 测试钩子：**导出**镜像到本地文件（阶段 120；跳过文件选择框 ⇒ 直接跑）。
pub fn images_export_for_test(st: &Rc<PanelState>, dest: &std::path::Path) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    let image = ui.images_selected.borrow().clone();
    if image.is_empty() {
        return;
    }
    run_image_export(st, ui, &image, dest);
}

/* ---------------- 阶段 166：删卷 / 删网络 ---------------- */

/// **卷表**的**选中变了** ⇒ 记住选中的是哪个卷 ＋ 更新操作条。
///
/// ⚠️ **跟镜像页一个套路**（`on_images_cursor_changed`）：GTK 的「选中」只是
///   一行 ⇒ 得把它翻译成 docker 认的**卷名** ⇒ 后面的命令才用得上。
fn on_volumes_cursor_changed(ui: &Rc<DockerUi>, view: &gtk::TreeView) {
    // ⚠️ **重建表格时**那些「假的光标变化」要挡掉（见 `refilter_volumes`）。
    if ui.restoring.get() {
        return;
    }
    let mut name = String::new();
    if let Some((model, iter)) = view.selection().selected() {
        let n: String = model.get(&iter, 0);
        if !n.is_empty() {
            name = n;
        }
    }
    *ui.volumes_selected.borrow_mut() = name;
    update_volumes_actions(ui);
}

/// 刷新**卷页的操作条**（按钮能不能点 ＋ 提示当前选中了谁）。
fn update_volumes_actions(ui: &Rc<DockerUi>) {
    let sel = ui.volumes_selected.borrow().clone();
    let has = !sel.is_empty();
    ui.volumes_remove_btn.set_sensitive(has);
    if has {
        ui.volumes_hint.set_text(&sel);
    } else {
        ui.volumes_hint.set_text(tr("选中一个卷才能删除"));
    }
}

/// **网络表**的**选中变了** ⇒ 记住选中的是哪个网络 ＋ 更新操作条。
fn on_networks_cursor_changed(ui: &Rc<DockerUi>, view: &gtk::TreeView) {
    // ⚠️ **重建表格时**那些「假的光标变化」要挡掉（见 `refilter_networks`）。
    if ui.restoring.get() {
        return;
    }
    let mut name = String::new();
    if let Some((model, iter)) = view.selection().selected() {
        let n: String = model.get(&iter, 0);
        if !n.is_empty() {
            name = n;
        }
    }
    *ui.networks_selected.borrow_mut() = name;
    update_networks_actions(ui);
}

/// 刷新**网络页的操作条**。
///
/// ⚠️ **默认网络（bridge / host / none）删不掉** ⇒ 按钮**置灰**＋**说清
///   为什么**（别让用户点一下再吃一个远端报错）。
fn update_networks_actions(ui: &Rc<DockerUi>) {
    let sel = ui.networks_selected.borrow().clone();
    let has = !sel.is_empty();
    if has && docker::is_default_network(&sel) {
        ui.networks_remove_btn.set_sensitive(false);
        ui.networks_hint.set_text(&trf!(
            "「{name}」是 Docker 默认网络，删不掉",
            name = sel
        ));
        return;
    }
    ui.networks_remove_btn.set_sensitive(has);
    if has {
        ui.networks_hint.set_text(&sel);
    } else {
        ui.networks_hint.set_text(tr("选中一个网络才能删除"));
    }
}

/// **删除卷**（**写操作** ⇒ 二次确认）。
///
/// ⚠️ **卷里是数据** ⇒ 删了就没了（远端**不会**再问一次）⇒ 必须确认，
///   而且确认框里要**说清后果**。
fn volumes_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    let msg = trf!(
        "确定要删除卷「{name}」吗？（卷里的数据会一起没了，撤不回来）",
        name = name
    );
    let (st2, u2) = (st.clone(), ui.clone());
    let name = name.to_string();
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
        tr("请确认"),
        &msg,
        tr("删除"),
        move |ok| {
            if !ok {
                return;
            }
            run_volume_remove(&st2, &u2, &name);
        },
    );
}

/// **删除卷**的后台部分（远端跑 `docker volume rm`）。
fn run_volume_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    ui.set_segment_status(SEG_VOLUMES, &trf!("正在删除卷「{name}」…", name = name));
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::remove_volume(&session, &name).await {
                Ok(()) => trf!("卷「{name}」已删除", name = name),
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::SegOp {
            seg: SEG_VOLUMES,
            msg,
        });
    });
}

/// **删除网络**（**写操作** ⇒ 二次确认）。
fn networks_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    let msg = trf!(
        "确定要删除网络「{name}」吗？（用着它的容器会受影响）",
        name = name
    );
    let (st2, u2) = (st.clone(), ui.clone());
    let name = name.to_string();
    let parent = host_window(ui, st);
    crate::panel::dialogs::confirm_title(
        parent.as_ref(),
        tr("请确认"),
        &msg,
        tr("删除"),
        move |ok| {
            if !ok {
                return;
            }
            run_network_remove(&st2, &u2, &name);
        },
    );
}

/// **删除网络**的后台部分（远端跑 `docker network rm`）。
fn run_network_remove(st: &Rc<PanelState>, ui: &Rc<DockerUi>, name: &str) {
    ui.set_segment_status(
        SEG_NETWORKS,
        &trf!("正在删除网络「{name}」…", name = name),
    );
    let opts = st.opts.clone();
    let pw = st.pw.borrow().as_ref().map(|p| p.to_string());
    let name = name.to_string();
    let tx = ui.tx.clone();
    let rt = crate::backend::hub::runtime();
    rt.spawn(async move {
        let msg = match shared_session(&opts, pw).await {
            Ok(session) => match docker::remove_network(&session, &name).await {
                Ok(()) => trf!("网络「{name}」已删除", name = name),
                Err(e) => e.0,
            },
            Err(e) => e,
        };
        let _ = tx.send_blocking(Msg::SegOp {
            seg: SEG_NETWORKS,
            msg,
        });
    });
}

/* ---------------- 阶段 166：删卷 / 删网络的测试钩子 ---------------- */

/// 测试钩子：**卷页的操作条**（按钮文案、能不能点、当前选中的卷、提示文字）。
pub fn volumes_actions_for_test(st: &Rc<PanelState>) -> (String, bool, String, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return (String::new(), false, String::new(), String::new());
    };
    (
        ui.volumes_remove_btn.label().unwrap_or_default().to_string(),
        ui.volumes_remove_btn.is_sensitive(),
        ui.volumes_selected.borrow().clone(),
        ui.volumes_hint.text().to_string(),
    )
}

/// 测试钩子：**网络页的操作条**（按钮文案、能不能点、当前选中的网络、提示文字）。
pub fn networks_actions_for_test(st: &Rc<PanelState>) -> (String, bool, String, String) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else {
        return (String::new(), false, String::new(), String::new());
    };
    (
        ui.networks_remove_btn.label().unwrap_or_default().to_string(),
        ui.networks_remove_btn.is_sensitive(),
        ui.networks_selected.borrow().clone(),
        ui.networks_hint.text().to_string(),
    )
}

/// 测试钩子：**关掉全部详情窗**（概览 / 日志 / 文件）。
///
/// ⚠️ **给冒烟用**：详情窗是**独立窗口** ⇒ 不关的话它会**盖在**容器页上面
///   ⇒ 截图拍到的是它 ⇒ 看不清容器页长什么样。
pub fn close_detail_windows_for_test(st: &Rc<PanelState>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    close_detail_window(ui);
}

/// 测试钩子：**选中**某个卷行（走真实的 `set_cursor` ⇒ 触发真实的
/// 「选中 ⇒ 更新操作条」）。
pub fn select_volume_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let model = ui.volumes_store.clone();
    let mut it = model.iter_first();
    while let Some(mut iter) = it {
        let n: String = model.get(&iter, 0);
        if n == name {
            let path = model.path(&iter);
            gtk::prelude::TreeViewExt::set_cursor(&ui.volumes_view, &path, None, false);
            ui.volumes_view.selection().select_path(&path);
            return true;
        }
        if !model.iter_next(&mut iter) {
            break;
        }
        it = Some(iter);
    }
    false
}

/// 测试钩子：**选中**某个网络行（走真实的 `set_cursor`）。
pub fn select_network_for_test(st: &Rc<PanelState>, name: &str) -> bool {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return false };
    let model = ui.networks_store.clone();
    let mut it = model.iter_first();
    while let Some(mut iter) = it {
        let n: String = model.get(&iter, 0);
        if n == name {
            let path = model.path(&iter);
            gtk::prelude::TreeViewExt::set_cursor(&ui.networks_view, &path, None, false);
            ui.networks_view.selection().select_path(&path);
            return true;
        }
        if !model.iter_next(&mut iter) {
            break;
        }
        it = Some(iter);
    }
    false
}

/// 测试钩子：**删掉**指定的卷（跳过对话框 ⇒ 直接跑）。
///
/// ⚠️ **给一个不存在的名字**（`pp-nonexistent-volume-120`）⇒ docker 会
///   立刻失败 ⇒ 正好验「失败要给中文原因」，又**不会碰到用户的真数据**。
pub fn volumes_remove_named_for_test(st: &Rc<PanelState>, name: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    run_volume_remove(st, ui, name);
}

/// 测试钩子：**删掉**指定的网络（跳过对话框 ⇒ 直接跑）。
pub fn networks_remove_named_for_test(st: &Rc<PanelState>, name: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    run_network_remove(st, ui, name);
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

/* ---------------- 阶段 167：新增的测试钩子 ---------------- */

/// 测试钩子：**镜像 / 卷 / 网络三张表的列宽**（阶段 167）。
///
/// 干什么用：冒烟要钉住「**表格要铺满**」⇒ 至少有一列**拿到了富余**
/// （GTK4 的规矩：**一列都不展开 ⇒ 富余全给最后一列**；`expand` 的列
/// 才会分到富余 ⇒ 看「有没有列是 `expand`」就知道表格会不会铺满）。
pub fn seg_table_expands_for_test(st: &Rc<PanelState>, seg: &str) -> Vec<bool> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let view = match seg {
        SEG_IMAGES => &ui.images_view,
        SEG_VOLUMES => &ui.volumes_view,
        SEG_NETWORKS => &ui.networks_view,
        _ => return Vec::new(),
    };
    view.columns()
        .iter()
        .map(|c| c.expands())
        .collect()
}

/// 测试钩子：**三页的搜索框**是不是「不吸富余宽度」（阶段 167）。
///
/// ⚠️ **为什么重要**：GTK4 的 Box 会把富余宽度**全给扩展型子控件**
///   ⇒ 搜索框 `hexpand(true)` ⇒ 它一个人吃掉整条工具条 ⇒ 按钮全被顶到
///   最右边（用户 2026-11-25：「搜索框吃满富余宽度」）。
pub fn seg_search_expands_for_test(st: &Rc<PanelState>, seg: &str) -> Vec<bool> {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return Vec::new() };
    let bar = match seg {
        SEG_IMAGES => &ui.images_bar,
        SEG_VOLUMES => &ui.volumes_bar,
        SEG_NETWORKS => &ui.networks_bar,
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut child = bar.first_child();
    while let Some(c) = child {
        if c.is::<gtk::SearchEntry>() {
            // ⚠️ **必须用 `hexpands()`（实际值）**，不能用 `is_hexpand_set()`
            //   —— 后者的语义是「**有没有被显式设过**」⇒ 我们**显式设了
            //   `false`**（这正是我们要的！）⇒ 它照样返回 true ⇒ 断言会假红。
            out.push(c.hexpands());
        }
        child = c.next_sibling();
    }
    out
}

/// 测试钩子：**日志窗的工具条**上有没有「下载…」按钮（阶段 167）。
///
/// 返回 `(按钮文字列表, 「下载…」在不在)`。
pub fn log_toolbar_for_test(st: &Rc<PanelState>) -> (Vec<String>, bool) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return (Vec::new(), false) };
    let mut labels = Vec::new();
    let mut has_save = false;
    let mut child = ui.log.search.parent().and_then(|p| p.parent());
    // ⚠️ **搜索框的父容器就是工具条** ⇒ 从它往上找一层。
    let _ = &mut child;
    let bar = ui.log.search.parent();
    if let Some(bar) = bar {
        let mut c = bar.first_child();
        while let Some(w) = c {
            if let Some(b) = w.downcast_ref::<gtk::Button>() {
                if let Some(l) = b.label() {
                    let t = l.to_string();
                    if t == tr("下载…") {
                        has_save = true;
                    }
                    labels.push(t);
                }
            }
            c = w.next_sibling();
        }
    }
    (labels, has_save)
}

/// 测试钩子：**「查看文件」窗**的正文区是不是铺满了（阶段 167）。
///
/// 返回 `(正文控件的高度, 窗口的高度)`。
///
/// ⚠️ **为什么要有**：用户 2026-11-25「查看容器里面的文件，现在只能显示1行」
///   ⇒ 根因是 `ScrolledWindow` **没设 `vexpand`** ⇒ 正文只拿到 1 行高度。
///   ⇒ 断言「正文高度 > 窗口高度的一半」才能钉住这个修复。
pub fn file_view_window_size_for_test(st: &Rc<PanelState>) -> Option<(i32, i32)> {
    let slot = st.docker_ui.borrow();
    let ui = slot.as_ref()?;
    let w = ui.file_view_window.borrow().clone()?;
    let view = ui.file_view_text.borrow().clone()?;
    Some((view.height(), w.height()))
}

/// 测试钩子：**打开「查看文件」窗**（阶段 167；给冒烟用）。
pub fn file_view_show_for_test(st: &Rc<PanelState>, path: &str, content: &str) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    show_file_window(Some(st), path, content, Some(ui));
}
