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

use crate::backend::docker::{self, ContainerInfo, DockerInfo};
use crate::i18n::tr;
use crate::panel::PanelState;
use crate::trf;

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
    pub page: gtk::Widget,
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
    /// 自动刷新的定时器（页签关掉时要停掉）。
    timer: Cell<Option<glib::SourceId>>,
    /// **后台任务 → 主线程**的结果通道（⚠️ 见 [`refresh`] 的说明）。
    tx: async_channel::Sender<Result<DockerInfo, String>>,
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
    fn rebuild(&self, rows: &[ContainerInfo]) {
        let expanded = self.expanded_projects();
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
        self.busy.set(false);
        *self.containers.borrow_mut() = info.containers.clone();
        *self.version.borrow_mut() = info.version.clone();
        let shown = self.filter_rows(&info.containers);
        self.rebuild(&shown);
        let now = chrono::Local::now().format("%H:%M:%S").to_string();
        self.last.set_text(&trf!("上次刷新 {t}", t = now));
        let msg = trf!(
            "共 {n} 个容器（远程 Docker {v}）",
            n = info.containers.len(),
            v = info.version
        );
        self.status.set_text(&msg);
    }

    /// 失败：清空表格 + 说清楚为什么（**中文 + 怎么办**）。
    fn fail(&self, msg: &str) {
        self.busy.set(false);
        self.model.clear();
        self.containers.borrow_mut().clear();
        self.status.set_text(msg);
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

    // ---- 状态行 ----
    let status = gtk::Label::new(Some(tr("正在读取远程容器…")));
    status.set_xalign(0.0);
    status.set_margin_start(6);
    status.set_margin_end(6);
    status.set_margin_top(2);
    status.set_margin_bottom(2);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&bar);
    page.append(&scroll);
    page.append(&status);

    // ---- 结果通道（后台任务 → 主线程）----
    //
    // ⚠️ **为什么用通道而不是 `idle_add_local_once`**：后台任务跑在 tokio
    //    的**多线程**运行时上 ⇒ 它 spawn 的 future 必须是 `Send` ⇒
    //    **不能把 `Rc`/`Weak` 抓进去**（`Rc` 不是 `Send`）。
    // ⇒ 后台只发**纯数据**（`DockerInfo` / 错误串），主线程这边收。
    let (tx, rx) = async_channel::unbounded::<Result<DockerInfo, String>>();

    let ui = Rc::new(DockerUi {
        page: page.upcast(),
        tab_id: Cell::new(0),
        model,
        view: view.clone(),
        status,
        last,
        auto: auto.clone(),
        filter: Cell::new(Filter::All),
        search: RefCell::new(String::new()),
        busy: Cell::new(false),
        timer: Cell::new(None),
        tx,
        containers: RefCell::new(Vec::new()),
        version: RefCell::new(String::new()),
        name_renderer,
    });

    // 主线程接收循环（**弱引用** ⇒ 面板关了这条循环自己会结束）。
    {
        let u = Rc::downgrade(&ui);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(r) = rx.recv().await {
                let Some(u) = u.upgrade() else { break };
                match r {
                    Ok(info) => u.apply(&info),
                    Err(msg) => u.fail(&msg),
                }
            }
        });
    }

    // ---- 事件 ----
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
            refresh(&st, &u);
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
    let session = shared.session().clone();
    let version = docker::probe(&session).await.map_err(|e| e.0)?;
    let containers = docker::list(&session).await.map_err(|e| e.0)?;
    Ok(DockerInfo {
        containers,
        version,
    })
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
        let _ = tx.send_blocking(r);
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
        let id = sub.add_page_icon(
            // ⚠️ **图标名必须是主题里真有的**：`docker-symbolic` 来自
            //    docker-icon-theme 包，**大多数系统没装** ⇒ 会显示成「缺图」。
            //    ⇒ 用 Adwaita（GTK4 基础主题）自带的**箱子**图标。
            Some("package-x-generic-symbolic"),
            tr("容器"),
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
            }
        });
    }
    if let Some(num) = sub.nb.page_num(&ui.page) {
        sub.nb.set_current_page(Some(num));
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

/// 测试钩子：**灌一批容器进表格**（**不连服务器**）。
///
/// 冒烟用它验界面的树形形状（compose 项目是父行、独立容器是顶层行、
/// 默认收起、点开才显示……）⇒ 不用真的连一台有 docker 的机器。
pub fn set_rows_for_test(st: &Rc<PanelState>, rows: Vec<ContainerInfo>) {
    let slot = st.docker_ui.borrow();
    let Some(ui) = slot.as_ref() else { return };
    ui.busy.set(false);
    *ui.containers.borrow_mut() = rows.clone();
    ui.rebuild(&rows);
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
