//! 单个文件面板（远程/本地）的 UI 构建、列表模型与拖拽（对应 C 版 pane_build /
//! fill_model / fill_local_pane / pane_selected / drag 系列）。

use std::cell::Cell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::backend::SftpEntry;
use crate::util::{human_size, mtime_full_str, mtime_str, path_join, utf8_sanitize};

use super::worker::{XferKind, XferState};
use super::PanelState;
use crate::i18n::tr;
use crate::trf;

pub const COL_ICON: i32 = 0;
pub const COL_NAME: i32 = 1;
pub const COL_SIZE_H: i32 = 2;
pub const COL_PERM: i32 = 3;
pub const COL_MTIME: i32 = 4;
pub const COL_ISDIR: i32 = 5;
pub const COL_SIZE: i32 = 6;
pub const COL_HIDDEN: i32 = 7;
/// 传输进度（0–100）与文案（"45%"/"完成"/…）；画在名称单元格右侧
pub const COL_PCT: i32 = 8;
pub const COL_PROGRESS: i32 = 9;
/// 修改时间的**完整**写法（"2026-10-27 14:32"）——只给悬停 tooltip 用。
///
/// ⚠️ 列里显示的是**短格式**（`COL_MTIME`）：完整格式在 120px 的列里会被
/// 硬裁成「2026-10-27 0…」（阶段 86 / 复查 §2.2、N2）。
pub const COL_MTIME_FULL: i32 = 10;
/// **这一行正在传输**（上传或下载）⇒ 名称单元格画呼吸背景。
///
/// 干什么用（用户 2026-11-10）：「上传/下载的文件背景色可以呼吸变化，
/// 这样更明显些」——窄面板里进度文字经常被挤掉，光靠那几个字不够显眼。
pub const COL_XFER: i32 = 11;
/// **进度文案的透明度**（1 = 完全可见，0 = 看不见）。
///
/// 干什么用（用户 2026-11-10）：「下载完了，就显示一下完成，然后就慢慢
/// 消失吧，不要把「完成」一直显示在那里」⇒ 终态文案停留一下再**渐隐**。
pub const COL_FADE: i32 = 12;
/// **终态文案的起始时刻**（单调毫秒）。
///
/// ⚠️ 三种状态：
/// - `0`  = 这一行**从没显示过**终态文案（或又开始传了 ⇒ 复位成这个）
/// - `>0` = 终态文案**正在淡出**（值 = 开始淡出的那一刻）
/// - `<0` = **已经淡完**（别再显示；文案还留在模型里，但透明度是 0）
///
/// 干什么用：队列每刷新一次就会把状态写回文件行 ⇒ 没有这个标记的话，
/// 淡完之后下一次刷新又会把「完成」点亮 ⇒ 用户看到「完成」反复冒出来。
pub const COL_FADE_AT: i32 = 13;

/* ---------------- 图标 ---------------- */

/// 目录的图标。
pub const ICON_DIR: &str = "folder-symbolic";
/// 普通文件的图标。
pub const ICON_FILE: &str = "text-x-generic-symbolic";
/// **软链接**的图标（GTK 标准的软链徽标）。
///
/// ⚠️ 为什么单独一个：用户 2026-11-10：「链接用链接的图标，文件用文件的
/// 图标」——列表里一眼能看出哪个是链接。⇒ **只换图标，不加列**（面板本来
/// 就挤，再加一列更挤）。
pub const ICON_LINK: &str = "emblem-symbolic-link-symbolic";

/// 权限串是不是「软链」。
///
/// ⚠️ 权限串的**第一个字符**就是类型位（跟 `ls -l` 一个规矩）：
/// `l` = 软链、`d` = 目录、`-` = 普通文件（见 `backend::perm_string`）。
///
/// ⇒ 面板里**不用为软链单开一列**（用户：本来就挤，再加一列太挤了）⇒
/// 直接从已有的权限列判断。
pub fn is_link_perm(perm: &str) -> bool {
    perm.starts_with('l')
}

/// 列表的统一行数据（远程 SftpEntry 与本地目录项都转成这个再填充）。
pub struct RowData {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub perm: String,
    pub mtime: i64,
    pub hidden: bool,
}

pub struct Pane {
    pub remote: bool,
    pub view: gtk::TreeView,
    pub model: gtk::ListStore,
    pub entry: gtk::Entry,
    pub toolbar: gtk::Box,
    pub show_hidden: Cell<bool>,
    /// 远程状态行（仅远程面板）
    pub status_lbl: gtk::Label,
    /// 可开关列（列显隐设置）；本地无权限列
    pub col_size: gtk::TreeViewColumn,
    pub col_mtime: gtk::TreeViewColumn,
    pub col_perm: Option<gtk::TreeViewColumn>,
    /// 名称列
    pub name_col: gtk::TreeViewColumn,
    /// 名称文字渲染器（横向偏移由名称列下方的窄滚动条驱动）
    pub name_renderer: super::namecell::NameCellRenderer,
    /// 名称列的横向滚动范围
    pub name_adj: gtk::Adjustment,
    /// 名称列下方的窄滚动条（只有文件名超出时才显示）
    pub name_bar: gtk::Scrollbar,
    /// **自己画的横向滚动条**（列表下方，常驻、12px 粗 ⇒ 好抓）。
    ///
    /// ⚠️ 为什么不用 GTK 自带的那条：它是 **overlay**（浮在内容上、鼠标
    ///    蹭到底边才浮出来、只有 4px 粗）⇒ 用户反馈「很难选中并拖动」。
    ///    ⇒ 横向改成 `External`（ScrolledWindow 不画它），自己画一条。
    pub h_bar: gtk::Scrollbar,
    /// 列表视口（算名称列滚动条的宽度/位置用）
    pub sw: gtk::ScrolledWindow,
    /// 上传/下载按钮（**纯图标**，文字在 tooltip 里）。
    ///
    /// 暴露出来是给冒烟断言用的：用户 2026-10-28 要求「主界面尽量少用
    /// 文字」，按钮条一律图标 + tooltip——这条得钉住，别哪天改回文字按钮。
    pub transfer_btn: gtk::Button,
}

/// 文件列表的列（名称之后的那些）。
///
/// 单独抽成函数是为了让布局能被独立验证：`examples/pane_cols_smoke.rs`
/// 直接拿它拼一个窄视图，断言"名称列永远在最左且不被挤没"。
pub struct FileCols {
    pub name: gtk::TreeViewColumn,
    pub size: gtk::TreeViewColumn,
    pub perm: Option<gtk::TreeViewColumn>,
    pub mtime: gtk::TreeViewColumn,
    /// 名称文字渲染器（面板要用它同步横向偏移）
    pub name_renderer: super::namecell::NameCellRenderer,
}

/// 名称列之后的所有列。
///
/// 右侧列只留一个很小的 `min_width` 下限，多出来的宽度自然流向 `expand`
/// 的名称列。名称列**不再省略**：长文件名把列撑宽，列表出现横向滚动条，
/// 拖一下就能看全（此前是靠省略号收尾，长名字永远只剩前半截）。
fn setup_trailing(col: &gtk::TreeViewColumn, min_width: i32, r: &gtk::CellRendererText) {
    col.set_min_width(min_width);
    r.set_ellipsize(gtk::pango::EllipsizeMode::End);
}

/// 大小列的**下限**宽度（列头「大小」两个字 + 内边距装得下）。
pub const SIZE_COL_MIN_W: i32 = 64;

/// 单元格左右内边距（列宽 = 文字宽 + 这个 + 余量）。
const SIZE_CELL_PAD: i32 = 12;

/// 大小列的最终宽度：**内容要装得下的宽度** 与 **用户拉过的宽度**，取更宽的。
///
/// ⚠️ **不许比内容窄**（用户 2026-11-10：「如果文件列表中显示了文件大小列，
///    那么一定要保证最长的大小显示完整，就是列宽按最长的文件大小来定」）
///    ⇒ 窄了 GTK 就把文字省略成「104.4 …」⇒ 用户根本看不出文件多大。
/// ⚠️ **也不许比用户拉过的窄**：用户手动加宽过就尊重它（他自己知道要什么）。
pub fn size_col_width(content_w: i32, user_w: i32) -> i32 {
    content_w.max(user_w).max(SIZE_COL_MIN_W)
}

/// 往 `view` 上挂 名称/大小/权限/修改时间 四列（权限列仅远程面板有）。
///
/// 调用方需已挂好图标列（`COL_ICON`）；包住 view 的 ScrolledWindow 给
/// 横向 `Automatic`，长文件名靠横向滚动看全。
pub fn add_file_columns(view: &gtk::TreeView, remote: bool) -> FileCols {
    let col_name = gtk::TreeViewColumn::new();
    col_name.set_title(tr("名称"));
    col_name.set_resizable(true);
    col_name.set_expand(true);
    // ⚠️ 阶段 76 / 审计 P1-15：原来 90px ⇒ 340px 宽的面板里名称列实际
    // 只剩约 120px，`AndroidStudioProje`、`Calibre 书库` 这些被硬裁。
    // 「修改时间」列默认藏起来之后，名称列能拿到 160px。
    col_name.set_min_width(160);
    // 列宽固定：长文件名**不撑列**，而是在列内左右滑动，右侧列保持可见
    col_name.set_sizing(gtk::TreeViewColumnSizing::Fixed);
    let r_name = super::namecell::NameCellRenderer::new();
    col_name.pack_start(&r_name, true);
    col_name.set_cell_data_func(&r_name, |_col, cell, model, iter| {
        if let Some(r) = cell.downcast_ref::<super::namecell::NameCellRenderer>() {
            r.set_text(model.get::<String>(iter, COL_NAME));
            r.set_progress(
                model.get::<String>(iter, COL_PROGRESS),
                model.get::<i32>(iter, COL_PCT),
            );
            // ⚠️ **正在传输的行 ⇒ 呼吸背景**（用户 2026-11-10）
            r.set_xfer(model.get::<bool>(iter, COL_XFER));
            // ⚠️ **终态文案渐隐**（「完成」别一直挂在那儿）
            r.set_fade(model.get::<f32>(iter, COL_FADE));
        }
    });
    view.append_column(&col_name);

    let col_size = gtk::TreeViewColumn::new();
    col_size.set_title(tr("大小"));
    let r_txt2 = gtk::CellRendererText::new();
    // ⚠️ 阶段 76 / 审计 P1-15：大小**右对齐**（数字按位对齐，好读）。
    gtk::prelude::CellRendererExt::set_alignment(&r_txt2, 1.0, 0.5);
    setup_trailing(&col_size, SIZE_COL_MIN_W, &r_txt2);
    // ⚠️ **列宽由 [`apply_size_col_width`] 按「最长的那个大小」算**
    //    （`min_width` + `fixed_width`），**不靠 sizing**。
    //
    //    GTK4 的规矩（查过 gtk/deprecated/gtktreeviewcolumn.c 与
    //    gtktreeview.c）：`_gtk_tree_view_column_request_width()` 里
    //    **fixed_width != -1 就用它**（**跟 sizing 无关**）⇒ 设了
    //    fixed_width，列宽就是它；而用户拖列宽时 GTK 做的是
    //    `gtk_tree_view_column_set_fixed_width (column, 拖出来的宽度)`
    //    ⇒ 拖完的宽度就存在 fixed_width 里 ⇒ 我们只要**别覆盖**它
    //    （`apply_size_col_width` 取「当前实际宽度」与「内容宽度」的
    //    **更宽**那个 ⇒ 用户拖宽了不会被弹回去）。
    col_size.set_resizable(true);
    col_size.pack_start(&r_txt2, false);
    col_size.add_attribute(&r_txt2, "text", COL_SIZE_H);
    view.append_column(&col_size);

    let mut col_perm: Option<gtk::TreeViewColumn> = None;
    if remote {
        let col = gtk::TreeViewColumn::new();
        col.set_title(tr("权限"));
        let r = gtk::CellRendererText::new();
        setup_trailing(&col, 72, &r);
        col.set_resizable(true);
        col.pack_start(&r, false);
        col.add_attribute(&r, "text", COL_PERM);
        view.append_column(&col);
        col_perm = Some(col);
    }

    let col_mtime = gtk::TreeViewColumn::new();
    col_mtime.set_title(tr("修改时间"));
    let r = gtk::CellRendererText::new();
    // 原来这里写死 min_width(108)：一个不可压缩的下限，正好是把横向滚动条
    // 逼出来的帮凶之一。现在改成"可收缩 + 小下限"。
    setup_trailing(&col_mtime, 120, &r);
    col_mtime.set_resizable(true);
    col_mtime.pack_start(&r, false);
    col_mtime.add_attribute(&r, "text", COL_MTIME);
    // ⚠️ **默认显示**（阶段 86 / 复查 §2.2、N2）。
    //
    // 以前这里写的是 `set_visible(false)` + 注释"默认藏起来"（旧审计 P1-15
    // 的处方），但 `settings::ColumnsCfg::defaults` 里 `mtime: true`
    // （注释明说"默认显示名称+大小+修改时间"），而构建时又按设置恢复显隐
    // ⇒ **两边打架**：刚藏掉的列立刻又被显示回来，注释跟实际行为对不上。
    //
    // 复查的结论是"**缩格式，不是隐藏**"：运维找文件恰恰常看"最近改过什么"，
    // 藏掉 mtime 是把有用信息当包袱 ⇒ 保留显示，把格式缩短（`mtime_str`
    // 短格式：今天只写时间、今年省略年份），完整时间在悬停 tooltip 里。
    //
    // ⇒ 这里**不再**默认隐藏；显隐由用户在设置里自己定（默认显示）。
    view.append_column(&col_mtime);

    FileCols {
        name: col_name,
        size: col_size,
        perm: col_perm,
        mtime: col_mtime,
        name_renderer: r_name,
    }
}

impl Pane {
    pub fn set_cwd_entry(&self, cwd: &str) {
        self.entry.set_text(cwd);
    }

    /// 选中项的完整路径列表。
    pub fn selected(&self, cwd: &str) -> Vec<(String, bool)> {
        let sel = self.view.selection();
        let (rows, _m) = sel.selected_rows();
        let mut out = Vec::new();
        for path in rows {
            if let Some(it) = self.model.iter(&path) {
                let name: String = self.model.get(&it, COL_NAME);
                if name == ".." {
                    continue; // 上级导航条目不参与文件操作
                }
                let isdir: bool = self.model.get(&it, COL_ISDIR);
                // ⚠️ **远端路径恒为 POSIX**，本地路径在 Windows 上是 `C:\dir`
                //    ⇒ 两种语义要分开（阶段 C5）。
                out.push((if self.remote {
                    path_join(cwd, &name)
                } else {
                    crate::platform::localpath::join(cwd, &name)
                }, isdir));
            }
        }
        out
    }
}

/// 当前的**单调**时钟（毫秒）。
///
/// ⚠️ 用单调时钟而不是系统时间：改系统时间/时区不该让正在淡出的「完成」
///    突然跳到别的阶段（甚至倒着走）。
pub fn now_ms() -> u64 {
    (glib::monotonic_time() / 1000).max(0) as u64
}

/// 按文件名在模型里找一行。
fn find_row(pane: &Pane, name: &str) -> Option<gtk::TreeIter> {
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        if pane.model.get::<String>(&i, COL_NAME) == name {
            return Some(i);
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    None
}

/// 更新某一行文件名后面的传输进度。找不到（文件不在当前目录）返回 false。
///
/// `xfer` = 这一行**正在传**（上传或下载）⇒ 名称单元格画呼吸背景
/// （用户 2026-11-10）。
pub fn set_progress(pane: &Pane, name: &str, pct: i32, text: &str, xfer: bool) -> bool {
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        if pane.model.get::<String>(&i, COL_NAME) == name {
            pane.model.set(
                &i,
                &[
                    (COL_PCT as u32, &pct.clamp(0, 100)),
                    (COL_PROGRESS as u32, &text),
                    (COL_XFER as u32, &xfer),
                    // ⚠️ **新文案 ⇒ 透明度复位**（否则上一次淡到一半的
                    //    「完成」会让新文案也半透明）
                    (COL_FADE as u32, &1.0f32),
                    // ⚠️ **又开始传了 ⇒ 终态标记清掉**（这一行又要从头来过）
                    (COL_FADE_AT as u32, &0i64),
                ],
            );
            pane.view.queue_draw();
            return true;
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    false
}

/// 在某一行上写**终态文案**（「完成」/「失败」/…）并开始淡出。
///
/// 干什么用（用户 2026-11-10）：「下载完了，就显示一下完成，然后就慢慢消失吧，
/// 不要把『完成』一直显示在那里」。
///
/// ⚠️ **只写一次**：已经显示过终态文案的行（正在淡出 `>0` / 已经淡完 `<0`）
///    不会被再次点亮 ⇒ 否则队列每刷新一次就会把「完成」重新点亮，用户看到
///    它反复冒出来。
///
/// 返回 true = 这次**真的写上**了（调用方据此决定要不要继续跑动画）。
pub fn set_terminal(pane: &Pane, name: &str, pct: i32, text: &str, now_ms: u64) -> bool {
    let Some(i) = find_row(pane, name) else {
        return false;
    };
    let at: i64 = pane.model.get(&i, COL_FADE_AT);
    if at != 0 {
        // 正在淡出（>0）或已经淡完（<0）⇒ 别再点亮
        return false;
    }
    pane.model.set(
        &i,
        &[
            (COL_PCT as u32, &pct.clamp(0, 100)),
            (COL_PROGRESS as u32, &text),
            (COL_XFER as u32, &false),
            (COL_FADE as u32, &1.0f32),
            (COL_FADE_AT as u32, &(now_ms as i64)),
        ],
    );
    pane.view.queue_draw();
    true
}

/// 推进**所有行**的终态文案淡出（动画定时器每 60ms 调一次）。
///
/// 返回"还有没有在淡的"（没有了就该停定时器了）。
pub fn tick_fade(pane: &Pane, now_ms: u64) -> bool {
    let mut fading = false;
    // ⚠️ **动过就要重画**：淡完那一刻 alpha 变 0 ⇒ 不重画的话「完成」还留在
    //    屏幕上（模型里已经透明了，但界面没刷新）。
    let mut touched = false;
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        let at: i64 = pane.model.get(&i, COL_FADE_AT);
        if at > 0 {
            touched = true;
            let elapsed = now_ms.saturating_sub(at as u64);
            if super::fade::finished(elapsed) {
                // ⚠️ **淡完了**：文案**留着**（但透明）——因为「已经淡完」这个
                //    状态本身要靠它记住（`COL_FADE_AT = -1`），不然目录刷新
                //    重建模型之后下一次队列刷新又会把「完成」点亮。
                pane.model.set(
                    &i,
                    &[
                        (COL_FADE as u32, &0.0f32),
                        (COL_FADE_AT as u32, &-1i64),
                    ],
                );
            } else {
                let a = super::fade::alpha_of(elapsed);
                pane.model.set(&i, &[(COL_FADE as u32, &a)]);
                fading = true;
            }
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    if touched {
        pane.view.queue_draw();
    }
    fading
}

/// 这个面板里**还有没有**正在淡出的终态文案。
pub fn has_fading(pane: &Pane) -> bool {
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        let at: i64 = pane.model.get(&i, COL_FADE_AT);
        if at > 0 {
            return true;
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    false
}

/// 冒烟用：读某一行当前的终态状态（**文案**, **透明度**, **起始时刻**）。
///
/// 找不到这一行返回 None。
pub fn row_terminal(pane: &Pane, name: &str) -> Option<(String, f32, i64)> {
    let i = find_row(pane, name)?;
    Some((
        pane.model.get::<String>(&i, COL_PROGRESS),
        pane.model.get::<f32>(&i, COL_FADE),
        pane.model.get::<i64>(&i, COL_FADE_AT),
    ))
}

/// 收集当前模型里所有**带终态文案**的行（正在淡出的 + 已经淡完的）。
///
/// 干什么用：`fill_model` 重建模型时把这些**搬回去** ⇒ 目录刷新（传输完成
/// 后会自动刷新远程目录）不会把刚出现的「完成」一下抹掉。
fn collect_terminal(pane: &Pane) -> Vec<(String, String, i64, f32)> {
    let mut out = Vec::new();
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        let at: i64 = pane.model.get(&i, COL_FADE_AT);
        if at != 0 {
            out.push((
                pane.model.get::<String>(&i, COL_NAME),
                pane.model.get::<String>(&i, COL_PROGRESS),
                at,
                pane.model.get::<f32>(&i, COL_FADE),
            ));
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    out
}

/// 队列清空后把所有行的进度清掉（目录刷新失败时也不留旧百分比）。
///
/// ⚠️ **正在淡出的行不动**：队列排空时会调它，那时「完成」刚出现 ⇒
///    一抹就没了，用户根本看不见（用户 2026-11-10：得**先看见**它慢慢消失）。
pub fn clear_progress(pane: &Pane) {
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        let at: i64 = pane.model.get(&i, COL_FADE_AT);
        if at == 0 {
            let _ = pane.model.set(
                &i,
                &[
                    (COL_PCT as u32, &0i32),
                    (COL_PROGRESS as u32, &""),
                    (COL_XFER as u32, &false),
                    (COL_FADE as u32, &1.0f32),
                ],
            );
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    pane.view.queue_draw();
}

/// 填充列表（隐藏过滤 + 目录显示“目录”）。对应 C 版 fill_model。
pub fn fill_model(pane: &Pane, show_hidden: bool, rows: Vec<RowData>) {
    // ⚠️ **先收集终态文案**：目录刷新会重建整个模型 ⇒ 正在淡出的「完成」
    //    得搬回去，不然它会被这一下抹掉（用户 2026-11-10：得**看得见**
    //    「完成」慢慢消失，而不是目录一刷新就没了）。
    let terminal = collect_terminal(pane);
    pane.model.clear();
    // ⚠️ **这一批的大小文案**（列宽按最长的那个算 ⇒ 见 `apply_size_col_width`）。
    let mut size_texts: Vec<String> = Vec::new();
    // 首行 ".."：进入上级目录（文件管理器惯例，取代工具栏「上级」按钮）
    {
        let icon = "go-up";
        let name = "..";
        let empty = "";
        let is_dir = true;
        let size: u64 = 0;
        let hidden = false;
        let it = pane.model.append();
        pane.model.set(
            &it,
            &[
                (COL_ICON as u32, &icon),
                (COL_NAME as u32, &name),
                (COL_SIZE_H as u32, &empty),
                (COL_PERM as u32, &empty),
                (COL_MTIME as u32, &empty),
                (COL_ISDIR as u32, &is_dir),
                (COL_SIZE as u32, &size),
                (COL_HIDDEN as u32, &hidden),
                (COL_PCT as u32, &0i32),
                (COL_PROGRESS as u32, &""),
                (COL_XFER as u32, &false),
                (COL_FADE as u32, &1.0f32),
                (COL_FADE_AT as u32, &0i64),
            ],
        );
    }
    for r in rows {
        if r.hidden && !show_hidden {
            continue;
        }
        // ⚠️ **软链用「链接」图标**（用户 2026-11-10：「链接用链接的图标，
        //    文件用文件的图标」）——**不加一列**（面板本来就挤）⇒ 只换图标。
        let icon = if is_link_perm(&r.perm) {
            ICON_LINK
        } else if r.is_dir {
            ICON_DIR
        } else {
            ICON_FILE
        };
        let name = utf8_sanitize(r.name.as_bytes());
        let size_h = if r.is_dir { tr("目录").to_string() } else { human_size(r.size) };
        // ⚠️ **收集大小文案**：列宽按**最长的那个**算（用户 2026-11-10）。
        size_texts.push(size_h.clone());
        let perm = r.perm.clone();
        let mtime = mtime_str(r.mtime);
        let mtime_full = mtime_full_str(r.mtime);
        let is_dir = r.is_dir;
        let size = r.size;
        let hidden = r.hidden;
        let it = pane.model.append();
        pane.model.set(
            &it,
            &[
                (COL_ICON as u32, &icon),
                (COL_NAME as u32, &name),
                (COL_SIZE_H as u32, &size_h),
                (COL_PERM as u32, &perm),
                (COL_MTIME as u32, &mtime),
                (COL_MTIME_FULL as u32, &mtime_full),
                (COL_ISDIR as u32, &is_dir),
                (COL_SIZE as u32, &size),
                (COL_HIDDEN as u32, &hidden),
                (COL_PCT as u32, &0i32),
                (COL_PROGRESS as u32, &""),
                (COL_XFER as u32, &false),
                (COL_FADE as u32, &1.0f32),
                (COL_FADE_AT as u32, &0i64),
            ],
        );
    }
    // ⚠️ **把终态文案搬回新模型**（目录刷新不该抹掉刚出现的「完成」）
    for (name, text, at, alpha) in terminal {
        let Some(i) = find_row(pane, &name) else {
            continue; // 新列表里没这个文件了（换了目录/文件被删）⇒ 放弃
        };
        let _ = pane.model.set(
            &i,
            &[
                (COL_PROGRESS as u32, &text),
                (COL_FADE as u32, &alpha),
                (COL_FADE_AT as u32, &at),
            ],
        );
    }
    // 目录换了：名称列的滚动范围重算，并回到最左
    update_name_scroll(&pane.model, &pane.view, &pane.name_col, &pane.name_adj);
    pane.name_renderer.set_offset(0.0);
    // ⚠️ **大小列按最长的那个大小定宽**（用户 2026-11-10：「如果文件列表中显示了
    //    文件大小列，那么一定要保证最长的大小显示完整，就是列宽按最长的文件大小
    //    来定」）⇒ 窄了 GTK 就把文字省略成「104.4 …」，用户根本看不出文件多大。
    apply_size_col_width(pane, longest_size_width(&pane.view, &size_texts));
    pane.view.queue_draw();
}

/// 这一批里**最长的大小文案**要占多宽（像素，含单元格内边距）。
///
/// ⚠️ **先按字符数粗筛、再用 Pango 实测**：目录里可能有几千个文件，每个都
///    去量一遍 Pango 会拖慢刷新 ⇒ 只量字符数最多的那几个候选（单位字母宽度
///    不同，"1.2 TB" 与 "104.4 KB" 字符数一样多但宽度不同）。
fn longest_size_width(view: &gtk::TreeView, texts: &[String]) -> i32 {
    let Some(max_chars) = texts.iter().map(|t| t.chars().count()).max() else {
        return 0;
    };
    let mut w = 0i32;
    for t in texts {
        if t.chars().count() + 2 < max_chars {
            continue; // 明显更短 ⇒ 不可能是最宽的
        }
        let (tw, _) = view.create_pango_layout(Some(t)).pixel_size();
        w = w.max(tw);
    }
    w + SIZE_CELL_PAD
}

/// 按内容宽度给大小列定宽（并尊重用户手动拉过的宽度）。
fn apply_size_col_width(pane: &Pane, content_w: i32) {
    // 用户拉过的宽度 = 列当前的**实际**宽度（GTK 拖列宽时把它写进了
    // fixed_width ⇒ 这里读到的就是用户拖出来的那个值）
    let user_w = pane.col_size.width().max(0);
    let want = size_col_width(content_w, user_w);
    // ⚠️ `min_width` 一起抬上去 ⇒ 用户**没法**把它拖到比内容还窄
    //    （拖了也会被弹回来）⇒ 「最长的大小一定显示完整」这条是**保证**的，
    //    不是"默认如此"。
    pane.col_size.set_min_width(want);
    pane.col_size.set_fixed_width(want);
}

/// 名字列横向滚动条用的 adjustment。
///
/// ⚠️ **区间必须合法**：`lower + page_size <= upper`。
///    GTK 的 `gtk_adjustment_new` 违反这条就**返回 NULL**（不是只警告！）⇒
///    之后每一次 `set_page_size` / `set_upper` / `get_upper` 都是**对着
///    NULL 做** ⇒ 满屏 GLib 警告，而且**横向滚动条根本不工作**。
///
///    ⚠️ **这是 GTK 版本相关的坑**：新 GTK（Windows 上的 4.24）才有这条
///    断言 ⇒ 旧 GTK（Linux 上）会「建出来一个参数不对的对象」⇒ 症状
///    轻得多（只是范围不对），**但一样是错的** ⇒ 所以断言要自己写
///    （见 `examples/fileops_smoke.rs`），不能指望 GTK 帮我们拦。
///
///    ⇒ 这里给一个**合法但看起来是空的**区间（`upper == page_size`）；
///    真正的范围由 [`update_name_scroll`] 按最长文件名算。
pub fn name_adjustment() -> gtk::Adjustment {
    gtk::Adjustment::new(0.0, 0.0, 10.0, 1.0, 10.0, 10.0)
}

/// 按当前名称列的可视宽度与最长文件名，算出名称列横向滚动范围。
fn update_name_scroll(
    model: &gtk::ListStore,
    view: &gtk::TreeView,
    col: &gtk::TreeViewColumn,
    adj: &gtk::Adjustment,
) {
    let avail = (col.width() - 8).max(40);
    let mut max_w = 0i32;
    let mut it = model.iter_first();
    while let Some(mut i) = it {
        let name: String = model.get(&i, COL_NAME);
        if name != ".." && name.chars().count() as i32 * 7 > avail {
            let (w, _) = view.create_pango_layout(Some(&name)).pixel_size();
            max_w = max_w.max(w);
        }
        it = if model.iter_next(&mut i) { Some(i) } else { None };
    }
    // ⚠️ **顺序有讲究**：GTK 要求任何时刻 `lower + page_size <= upper` ⇒
    //    **先抬 upper、再设 page_size**（反过来会出现「page_size 比整个
    //    区间还大」的瞬间，GTK 会断言）。
    // 没有溢出时 upper == page → 滚动条自动隐藏（多给 4px 会永远露出一截）
    let upper = if max_w > avail { max_w as f64 + 4.0 } else { avail as f64 };
    adj.set_upper(upper);
    adj.set_page_size(avail as f64);
    adj.set_value(0.0);
}

/// 读取本地目录并按名称排序（目录优先由 ListStore 排序函数处理）。
pub fn read_local_dir(cwd: &str) -> Result<Vec<RowData>, String> {
    let rd = std::fs::read_dir(cwd).map_err(|e| format!("{e}"))?;
    let mut rows = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let path = ent.path();
        // ⚠️ **Rust 的 `DirEntry::metadata()` 不跟随软链**（跟 `fs::metadata`
        //    不一样！）⇒ 软链会被当成**文件** ⇒ 双击去开编辑器 ⇒ 打开目录
        //    就会报「是目录，无法用文本查看/编辑器打开」（远端那边同一个毛病，
        //    这次一起修）。
        //
        // ⇒ 分两步拿：
        //    - `symlink_metadata()` = **软链自己**（类型位 `l`、大小=目标
        //      路径的长度、`ls -l` 看到的那一串）；
        //    - `fs::metadata()` = **跟随**软链后的**目标**（判定它是目录
        //      还是文件 ⇒ 双击的行为跟真实目标一致）。
        let Ok(lmd) = path.symlink_metadata() else {
            continue;
        };
        let is_link = lmd.file_type().is_symlink();
        // ⚠️ 断链（目标没了）⇒ 跟随失败 ⇒ 退回软链自己（仍显示，图标是
        //    链接图标；双击时问路径类型会失败 ⇒ 弹一句提示）。
        let md = if is_link {
            std::fs::metadata(&path).unwrap_or_else(|_| lmd.clone())
        } else {
            lmd.clone()
        };
        let is_dir = md.is_dir();
        // 大小/时间：**软链显示它自己的**（跟 `ls -l` 一个规矩：软链的大小
        // 是目标路径那串的长度），目录/文件显示目标的。
        let shown = if is_link { &lmd } else { &md };
        rows.push(RowData {
            perm: crate::backend::perm_string(
                crate::platform::perm::mode_of(shown),
                is_dir,
                is_link,
            ),
            hidden: name.starts_with('.'),
            mtime: crate::platform::perm::mtime_of(shown),
            size: if is_dir { 0 } else { shown.len() },
            name,
            is_dir,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

/// SftpEntry → RowData。
pub fn entry_rows(entries: &[SftpEntry]) -> Vec<RowData> {
    entries
        .iter()
        .map(|e| RowData {
            hidden: e.name.starts_with('.'),
            name: e.name.clone(),
            size: e.size,
            is_dir: e.is_dir,
            perm: if e.perm.is_empty() { "-".into() } else { e.perm.clone() },
            mtime: e.mtime,
        })
        .collect()
}

/* ---------------- 面板构建 ---------------- */

pub fn build_pane(st: &Rc<PanelState>, remote: bool) -> Pane {
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let model = gtk::ListStore::new(&[
        glib::Type::STRING, // ICON
        glib::Type::STRING, // NAME
        glib::Type::STRING, // SIZE_H
        glib::Type::STRING, // PERM
        glib::Type::STRING, // MTIME
        glib::Type::BOOL,   // ISDIR
        glib::Type::U64,    // SIZE
        glib::Type::BOOL,   // HIDDEN
        glib::Type::I32,    // PCT
        glib::Type::STRING, // PROGRESS
        glib::Type::STRING, // MTIME_FULL（tooltip 用，不显示）
        glib::Type::BOOL,   // XFER（正在传输 ⇒ 呼吸背景）
        glib::Type::F32,    // FADE（进度文案的透明度：终态文案渐隐用）
        glib::Type::I64,    // FADE_AT（终态文案的起始时刻：0=没有，>0=正在淡出，<0=已淡完）
    ]);
    let view = gtk::TreeView::new();
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(if remote { tr("远程路径") } else { tr("本地路径") }));
    entry.set_width_chars(8);
    entry.set_hexpand(true);
    entry.add_css_class("pp-path");

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    toolbar.set_margin_start(6);
    toolbar.set_margin_end(4);
    toolbar.set_margin_bottom(4);
    let status_lbl = gtk::Label::new(None);

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
    fn icon_toggle(icon: &str, tooltip: &str) -> gtk::ToggleButton {
        let b = gtk::ToggleButton::new();
        let img = gtk::Image::from_icon_name(icon);
        img.set_pixel_size(16);
        b.set_child(Some(&img));
        // 纯图标开关 ⇒ 同样要有可访问名称
        crate::a11y::watch_tooltip(&b);
        b.set_tooltip_text(Some(tooltip));
        b.add_css_class("flat");
        b
    }

    /* ---- 第一行：面板名 + 连接状态 + 主传输动作 ---- */
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_top(6);
    header.set_margin_bottom(2);
    header.set_margin_start(8);
    header.set_margin_end(6);

    let title = gtk::Label::new(None);
    title.set_markup(if remote { tr("<b>远程</b>") } else { tr("<b>本地</b>") });
    title.set_halign(gtk::Align::Start);
    header.append(&title);

    if remote {
        // 圆点由状态行自己画（带主题色），这里只留文字
        status_lbl.set_text(tr("未连接"));
        status_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status_lbl.set_halign(gtk::Align::Start);
        status_lbl.set_hexpand(true);
        status_lbl.set_xalign(0.0);
        header.append(&status_lbl);
    } else {
        let hint = gtk::Label::new(Some(tr("本机文件")));
        hint.add_css_class("dim-label");
        hint.add_css_class("pp-hint");
        hint.set_halign(gtk::Align::Start);
        hint.set_hexpand(true);
        header.append(&hint);
    }

    // 主传输动作常驻：上传/下载不再只藏在右键菜单里
    //
    // ⚠️ **纯图标**（用户 2026-10-28：主界面尽量少用文字）：下载=向下、
    // 上传=向上，方向本身就是语义；文字说明挪进 tooltip。
    let transfer = icon_btn(
        if remote { "go-down-symbolic" } else { "go-up-symbolic" },
        if remote {
            tr("下载选中的远程文件到本地当前目录（也可以直接拖到本地面板）")
        } else {
            tr("上传选中的本地文件到远程当前目录")
        },
    );
    transfer.set_sensitive(false); // 有选中项才可用
    {
        let st2 = Rc::clone(st);
        transfer.connect_clicked(move |_| super::on_transfer_btn(&st2, remote));
    }
    header.append(&transfer);
    box_.append(&header);

    /* ---- 第二行：路径 + 导航 + 次级操作（收进 ⋯） ---- */
    toolbar.append(&entry);

    if remote {
        let rreconnect = icon_btn("network-transmit-receive-symbolic", tr("重新建立 SFTP 连接"));
        {
            let st2 = Rc::clone(st);
            rreconnect.connect_clicked(move |_| super::on_reconnect(&st2));
        }
        toolbar.append(&rreconnect);
        st.rreconnect.replace(Some(rreconnect));
    }

    let home = icon_btn("go-home-symbolic", tr("主目录"));
    {
        let st2 = Rc::clone(st);
        home.connect_clicked(move |_| super::on_home(&st2, remote));
    }
    toolbar.append(&home);
    let refresh = icon_btn("view-refresh-symbolic", tr("刷新目录"));
    {
        let st2 = Rc::clone(st);
        refresh.connect_clicked(move |_| super::on_refresh(&st2, remote));
    }
    toolbar.append(&refresh);
    let hidden = icon_toggle("view-conceal-symbolic", tr("显示/隐藏 隐藏文件"));
    hidden.set_active(false);
    {
        let st2 = Rc::clone(st);
        hidden.connect_toggled(move |b| {
            st2.set_pane_show_hidden(remote, b.is_active());
            super::on_refresh(&st2, remote);
        });
    }
    toolbar.append(&hidden);

    if remote {
        let follow_btn = icon_toggle(
            "emblem-synchronizing-symbolic",
            tr("跟随终端目录（约 2 秒一次轮询，OSC 7 即时）\n按下 = 开启"),
        );
        follow_btn.set_active(true);
        {
            let st2 = Rc::clone(st);
            follow_btn.connect_toggled(move |b| st2.follow.set(b.is_active()));
        }
        toolbar.append(&follow_btn);
        st.follow_btn.replace(Some(follow_btn));

        // 传输队列：会话级窗口，但传东西时经常要看，直接放按钮不藏进 ⋯。
        // 目录镜像放在**本地面板**（源目录在本地，推送方向本地 → 远端）。
        //
        // ⚠ 按钮上带一个**数量角标**（阶段 102）：传输进行中时角标显示
        // 「还在传几个」，空闲时藏起来。角标是这个按钮**唯一的信息增量**
        // ——图标本身不会变，用户扫一眼就知道"有没有东西在传、还有几件"。
        let queue = gtk::Button::new();
        // ⚠️ 圈建在块**外面** ⇒ 后面要交给 `PanelState`（队列刷新时开关它）。
        let ring = super::ring::Ring::new(11);
        {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            let img = gtk::Image::from_icon_name("view-list-symbolic");
            img.set_pixel_size(16);
            // ⚠️ **传输中 ⇒ 图标上叠一个转动的圈**（用户 2026-11-11：
            //    「上传下载文件时传输队列图标可以改为原来静态的图标上加个
            //    转动的圈」）。
            //    ⇒ 用 `Overlay`：底下还是原来那个队列图标（**不再换成
            //    `content-loading-symbolic` 那个静态的加载图标**），上面
            //    盖一个自己画的小转圈（见 `panel::ring`）。
            ring.widget().set_halign(gtk::Align::End);
            ring.widget().set_valign(gtk::Align::End);
            let ov = gtk::Overlay::new();
            ov.set_child(Some(&img));
            let rw = ring.widget().clone();
            ov.add_overlay(&rw);
            let badge = gtk::Label::new(None);
            badge.add_css_class("pp-xfer-badge");
            // 固定一格宽 ⇒ 空闲时也不占地方，传输中出现/消失时
            // 工具栏不会左右跳
            badge.set_width_chars(1);
            row.append(&ov);
            row.append(&badge);
            queue.set_child(Some(&row));
        }
        queue.add_css_class("flat");
        crate::a11y::watch_tooltip(&queue);
        queue.set_tooltip_text(Some(tr("传输队列")));
        {
            let st2 = Rc::clone(st);
            queue.connect_clicked(move |_| super::on_open_queue(&st2));
        }
        toolbar.append(&queue);
        // 队列刷新时要用它刷忙态与角标 ⇒ 交一份给 PanelState（阶段 102）
        st.queue_btn.replace(Some(queue));
        st.xfer_ring.replace(Some(ring));
    } else {
        // 目录镜像：把本地当前目录单向推到远端当前目录，源在本地，
        // 入口跟着源走（以前挂在远程面板的 ⋯ 里，语义是反的）。
        let mirror = icon_btn(
            "document-send-symbolic",
            tr("目录镜像（本地 → 远程）\n扫描差异、先预览再执行；删除远程多余文件需另行确认"),
        );
        {
            let st2 = Rc::clone(st);
            mirror.connect_clicked(move |_| super::mirrorui::open(&st2));
        }
        toolbar.append(&mirror);
    }

    box_.append(&toolbar);

    // 路径栏：回车直达
    {
        let st2 = Rc::clone(st);
        entry.connect_activate(move |e| {
            super::on_entry_activated(&st2, remote, &e.text());
        });
    }

    // 文件列表：排序 = 目录优先 + 名称
    model.set_sort_func(gtk::SortColumn::Index(COL_NAME as u32), |m, a, b| {
        let da: bool = m.get(a, COL_ISDIR);
        let db: bool = m.get(b, COL_ISDIR);
        let na: String = m.get(a, COL_NAME);
        let nb: String = m.get(b, COL_NAME);
        // ".." 固定在最前
        if na == ".." {
            return gtk::Ordering::Smaller;
        }
        if nb == ".." {
            return gtk::Ordering::Larger;
        }
        if da != db {
            if da { gtk::Ordering::Smaller } else { gtk::Ordering::Larger }
        } else if na < nb {
            gtk::Ordering::Smaller
        } else if na > nb {
            gtk::Ordering::Larger
        } else {
            gtk::Ordering::Equal
        }
    });
    model.set_sort_column_id(
        gtk::SortColumn::Index(COL_NAME as u32),
        gtk::SortType::Ascending,
    );

    view.set_model(Some(&model));
    view.selection().set_mode(gtk::SelectionMode::Multiple);

    // ⚠️ 阶段 76 / 审计 P1-15：名称太长时画**省略号** ⇒ 完整名字得有个
    // 地方能看到（悬停 tooltip）。顺带把大小/权限/修改时间也带上。
    view.connect_query_tooltip(|view, x, y, _kbd, tooltip| {
        let Some((Some(path), _col, _cx, _cy)) = view.path_at_pos(x, y) else {
            return false;
        };
        // GTK4 的树视图 tooltip 要自己指定行（默认处理器被我们接管了）
        view.set_tooltip_row(tooltip, &path);
        let Some(model) = view.model() else { return false };
        let Some(mut it) = model.iter_first() else { return false };
        loop {
            if model.path(&it) == path {
                let name: String = model.get(&it, COL_NAME);
                let size: String = model.get(&it, COL_SIZE_H);
                // ⚠️ tooltip 给**完整**时间（列里是短格式，悬停才看全）
                let mtime: String = model.get(&it, COL_MTIME_FULL);
                let mut lines = vec![name];
                if !size.is_empty() {
                    lines.push(trf!("大小：{size}", size = size));
                }
                if !mtime.is_empty() {
                    lines.push(trf!("修改时间：{mtime}", mtime = mtime));
                }
                tooltip.set_text(Some(&lines.join("\n")));
                return true;
            }
            if !model.iter_next(&mut it) {
                return false;
            }
        }
    });

    // 传输按钮只在有选中项时可用（".." 导航条不算）
    {
        let b = transfer.clone();
        view.selection().connect_changed(move |sel| {
            let (rows, m) = sel.selected_rows();
            let n = rows
                .iter()
                .filter(|p| {
                    m.iter(p)
                        .map(|it| m.get::<String>(&it, COL_NAME) != "..")
                        .unwrap_or(false)
                })
                .count();
            b.set_sensitive(n > 0);
        });
    }

    let col_icon = gtk::TreeViewColumn::new();
    let r_pix = gtk::CellRendererPixbuf::new();
    col_icon.pack_start(&r_pix, false);
    col_icon.add_attribute(&r_pix, "icon-name", COL_ICON);
    // 列表是建好窗口之后才填数据的：列在空模型时量出来的宽度会一直保持 0，
    // 于是图标整列被压没（截图里只有文件名）。给个下限把位置先占住。
    col_icon.set_min_width(24);
    col_icon.set_resizable(false);
    view.append_column(&col_icon);

    let cols = add_file_columns(&view, remote);
    // 恢复上次拖过的列宽（名称列始终 expand，不记）
    let cw = crate::settings::col_widths(remote);
    if let Some(w) = cw.size {
        cols.size.set_fixed_width(w);
    }
    if let Some(w) = cw.perm {
        if let Some(c) = &cols.perm {
            c.set_fixed_width(w);
        }
    }
    if let Some(w) = cw.mtime {
        cols.mtime.set_fixed_width(w);
    }

    let sw = gtk::ScrolledWindow::new();
    // ⚠️ 阶段 76 / 审计 P1-15：横向**允许**滚动了。
    //
    // 以前是 Never（"所有列始终可见"），但名称列下限提到 160px 之后，
    // 窄面板里右侧的「大小/权限」会被挤出视口**再也看不到** ⇒
    // 横向能滚才能看到它们。
    //
    // 长文件名仍然**不撑列**：它在名称列内部滑 + 超宽时画省略号。
    //
    // ⚠️ **横向改成 `External`（2026-11-10，用户：「远程文件列表下面的横向
    //    滚动条很难选中并拖动」）**：GTK 自带那条是 **overlay**——浮在内容
    //    上、鼠标蹭到底边才浮出来、而且只有 4px 粗 ⇒ 窄面板里几乎抓不住。
    //    ⇒ 横向让 ScrolledWindow **别画**，自己在列表下面挂一条**常驻的**
    //    12px 滚动条（见下面的 `h_bar`）。
    //    竖向照旧（Automatic + overlay）：竖条要是常驻的就会实占文件名宽度。
    sw.set_policy(gtk::PolicyType::External, gtk::PolicyType::Automatic);
    // 强制 overlay：有些桌面（KDE 等）在全局关掉 overlay scrolling，
    // 竖向滚动条会实占 14px；文件面板本来就窄，这里不允许它吃文件名宽度
    sw.set_overlay_scrolling(true);
    sw.add_css_class("pp-thin-scrollbar");
    sw.set_child(Some(&view));
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    box_.append(&sw);

    // ---- 自己画的横向滚动条（常驻、12px 粗 ⇒ 好抓）----
    let h_bar = {
        let adj = sw.hadjustment();
        let bar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&adj));
        bar.add_css_class("pp-hscroll");
        bar.set_tooltip_text(Some(tr("拖动查看右侧的列")));
        bar.set_visible(false);
        // ⚠️ **有横向溢出才显示**（没溢出时留一条灰杠反而像坏了）。
        let upd = Rc::new({
            let bar = bar.clone();
            let adj = adj.clone();
            move || {
                let over = adj.upper() - adj.page_size() > 0.5;
                bar.set_visible(over);
            }
        });
        upd();
        {
            let upd = Rc::clone(&upd);
            adj.connect_notify_local(Some("upper"), move |_, _| upd());
        }
        {
            let upd = Rc::clone(&upd);
            adj.connect_notify_local(Some("page-size"), move |_, _| upd());
        }
        box_.append(&bar);
        bar
    };

    // 名称列专用横向滚动条：只滚文件名文字，其他列不动。
    // 放在列表下方（6px 薄条，不压住最后一行），没有超长文件名时隐藏。
    let name_adj = name_adjustment();
    let name_bar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&name_adj));
    name_bar.add_css_class("pp-name-scroll");
    name_bar.set_tooltip_text(Some(tr("拖动查看完整文件名")));
    name_bar.set_visible(false);
    name_bar.set_margin_bottom(1);
    box_.append(&name_bar);

    // 滚动条位置/宽度对齐名称列（图标列 24px + 名称列宽）
    let place_bar = {
        let sw = sw.clone();
        let col = cols.name.clone();
        let bar = name_bar.clone();
        let adj = name_adj.clone();
        Rc::new(move || {
            let full = sw.width();
            if full <= 0 {
                return;
            }
            let start = 24;
            let name_w = col.width().max(0);
            bar.set_margin_start(start);
            bar.set_margin_end((full - start - name_w).max(0));
            bar.set_visible(adj.upper() - adj.page_size() > 0.5);
        })
    };
    place_bar();
    {
        let f = Rc::clone(&place_bar);
        sw.connect_notify_local(Some("width"), move |_, _| f());
    }
    {
        let f = Rc::clone(&place_bar);
        cols.name.connect_notify_local(Some("width"), move |_, _| f());
    }
    {
        let f = Rc::clone(&place_bar);
        name_adj.connect_notify_local(Some("upper"), move |_, _| f());
    }
    // 拖动滚动条 → 只改文字偏移并重绘
    {
        let r = cols.name_renderer.clone();
        let v = view.clone();
        name_adj.connect_value_changed(move |a| {
            r.set_offset(a.value());
            v.queue_draw();
        });
    }
    // 列宽变化时重算可滚范围（可用宽度变了）
    {
        let model = model.clone();
        let view = view.clone();
        let col = cols.name.clone();
        let adj = name_adj.clone();
        cols.name.connect_notify_local(Some("width"), move |_, _| {
            update_name_scroll(&model, &view, &col, &adj);
        });
    }

    // 双击进入目录 / 传输文件
    {
        let st2 = Rc::clone(st);
        let model2 = model.clone();
        view.connect_row_activated(move |_v, path, _col| {
            if let Some(it) = model2.iter(path) {
                let name: String = model2.get(&it, COL_NAME);
                let isdir: bool = model2.get(&it, COL_ISDIR);
                // ⚠️ **软链怎么认**：不新增一列（面板本来就挤）⇒ 看**权限列**
                //    的第一个字符——`l` 开头就是软链（跟 `ls -l` 一个规矩，
                //    后端的 perm_string 就是这么写的）。
                let perm: String = model2.get(&it, COL_PERM);
                let islink = is_link_perm(&perm);
                super::on_row_activated(&st2, remote, &name, isdir, islink);
            }
        });
    }

    box_.set_hexpand(true);
    box_.set_vexpand(true);
    st.attach_pane_widget(remote, box_);

    install_context_menu(st, remote, &view);
    install_drag(st, remote, &view);

    // 列显隐：按用户设置初始化
    let cfg = crate::settings::columns(remote);
    cols.size.set_visible(cfg.size);
    cols.mtime.set_visible(cfg.mtime);
    if let Some(cp) = &cols.perm {
        cp.set_visible(cfg.perm);
    }

    Pane {
        remote,
        view,
        model,
        entry,
        toolbar,
        show_hidden: Cell::new(false),
        status_lbl,
        col_size: cols.size,
        col_mtime: cols.mtime,
        col_perm: cols.perm,
        name_col: cols.name,
        name_renderer: cols.name_renderer,
        name_adj,
        name_bar,
        h_bar,
        sw,
        transfer_btn: transfer,
    }
}

/// 「在此处打开终端」的**目标目录**。
///
/// - 右键的是**目录** → 进它；
/// - 右键的是**文件** → 进它所在的目录；
/// - 什么都没选 → 进当前目录。
///
/// **纯函数**（不吃 widget、不碰状态）：这样能单测，也免得菜单回调里
/// 一堆分支。
pub fn term_target_dir(sel: &[(String, bool)], cwd: &str) -> String {
    match sel.first() {
        Some((p, true)) => p.clone(),
        Some((p, false)) => crate::util::path_parent(p),
        None => cwd.to_string(),
    }
}

/* ---------------- 右键菜单（文件操作都在这里，工具栏只留导航） ---------------- */

fn install_context_menu(st: &Rc<PanelState>, remote: bool, view: &gtk::TreeView) {
    // 文件操作菜单：和终端/侧边栏同一套自绘弹层（图标 + 左对齐 + 分组），
    // 删除单独标红。以前用 gio::Menu 模型，行里放不了图标与危险色。
    let pop = crate::menu::popover(view);
    let vb = crate::menu::body();
    let mut file_btns: Vec<gtk::Button> = Vec::new();
    let mut add = |icon: &str, label: &str, danger: bool| -> gtk::Button {
        let b = crate::menu::item(icon, label, None);
        if danger {
            crate::menu::danger(&b);
        }
        let p = pop.clone();
        b.connect_clicked(move |_| p.popdown());
        vb.append(&b);
        b
    };

    {
        // 「打开」＝内置编辑器（可改可存）。原来这里还有一项「查看…」
        // （只读打开），用户嫌两项重复，已去掉：非法 UTF-8 之类确实读不了
        // 又存不回去的文件，编辑器自己会降级成只读并给出说明。
        let b = add("document-open-symbolic", tr("打开"), false);
        b.set_visible(false);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_open_file(&st2, remote, false));
        file_btns.push(b);
    }
    {
        // 「在此处打开终端」：在**当前标签**里新开一个终端页签，
        // 让它进到右键的那个目录（XTerminal / VS Code 的做法）。
        //
        // **只对远程面板有**：本地目录开终端没意义——本程序的终端全是
        // 远程 SSH 会话，没有本地 shell。
        let b = add("utilities-terminal-symbolic", tr("在此处打开终端"), false);
        if !remote {
            b.set_visible(false);
        }
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| {
            let cwd = st2.pane_cwd(true);
            if cwd.is_empty() {
                return; // 还没连上 SFTP，不知道在哪
            }
            let sel = st2.pane(true).selected(&cwd);
            let dir = term_target_dir(&sel, &cwd);
            crate::tabs::open_term_at(&st2, &dir);
        });
    }

    // 目录/多选时这一行连同分隔线一起隐藏，而不是留下灰掉的项
    let file_sep = crate::menu::sep();
    file_sep.set_visible(false);
    vb.append(&file_sep);

    // 只对“选中项里确实有在排队/在传的源文件”显示；目录递归传输也认
    let cancel_btn = add("process-stop-symbolic", tr("取消传输"), false);
    cancel_btn.set_visible(false);
    {
        let st2 = Rc::clone(st);
        cancel_btn.connect_clicked(move |_| super::on_cancel_transfers(&st2, remote));
    }
    let cancel_sep = crate::menu::sep();
    cancel_sep.set_visible(false);
    vb.append(&cancel_sep);

    {
        // 图标跟**工具栏常驻按钮**一致（上行/下行箭头）：原先这里用的是
        // document-save / document-open，而「打开」那一项也是
        // document-open —— 两个不同的动作顶着同一个图标，用户分不清
        // 哪个是"打开"哪个是"上传"。
        let b = add(
            if remote {
                "go-down-symbolic"
            } else {
                "go-up-symbolic"
            },
            if remote { tr("下载…") } else { tr("上传…") },
            false,
        );
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_transfer_btn(&st2, remote));
    }
    if remote {
        let b = add("send-to-symbolic", tr("发送到其它服务器…"), false);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::relayui::send_to_server(&st2));
    }
    if remote {
        // 「粘贴上传」：把**剪贴板里的本地文件**传到当前远程目录。
        //
        // **为什么要有它**：X11 的拖拽传文件要源程序提供选区数据，而 Deepin
        // 文件管理器（Qt）**挑问法**（时间戳或请求方窗口要对得上，见
        // `super::xdnd11` 的说明）。拖拽这条路万一不通，还有剪贴板这条
        // 更直白的路：`Ctrl+C` 复制 → 这里粘贴上传。
        let b = add("edit-paste-symbolic", tr("粘贴上传"), false);
        b.set_tooltip_text(Some(tr("把剪贴板里的本地文件上传到远程当前目录")));
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_paste_upload(&st2));
    }

    vb.append(&crate::menu::sep());
    {
        let b = add("folder-new-symbolic", tr("新建文件夹"), false);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_mkdir(&st2, remote));
    }
    {
        let b = add("document-edit-symbolic", tr("改名…"), false);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_rename(&st2, remote));
    }
    {
        let b = add("edit-delete-symbolic", tr("删除"), true);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_delete(&st2, remote));
    }

    vb.append(&crate::menu::sep());
    {
        let b = add("view-refresh-symbolic", tr("刷新"), false);
        let st2 = Rc::clone(st);
        b.connect_clicked(move |_| super::on_refresh(&st2, remote));
    }
    pop.set_child(Some(&vb));

    // 「打开」只对"恰好选中一个文件"有意义；目录与多选不显示这一项。
    let update_file_btns: Rc<dyn Fn()> = {
        let st2 = Rc::clone(st);
        let btns = file_btns.clone();
        let sep = file_sep.clone();
        let cancel = cancel_btn.clone();
        let cancel_sep = cancel_sep.clone();
        Rc::new(move || {
            let sel = st2.pane(remote).selected(&st2.pane_cwd(remote));
            let ok = sel.len() == 1 && !sel[0].1;
            for b in &btns {
                b.set_visible(ok);
            }
            sep.set_visible(ok);

            // 源面板 = 上传看本地、下载看远程；只认普通文件传输
            let active = st2.items.borrow().iter().any(|it| {
                if it.kind != XferKind::Transfer || (!it.upload) != remote {
                    return false;
                }
                let g = it.shared.lock().unwrap();
                matches!(g.state, XferState::Wait | XferState::Run)
                    && sel.iter().any(|(p, _)| p == &it.src)
            });
            cancel.set_visible(active);
            cancel_sep.set_visible(active);
        })
    };
    {
        let update = Rc::clone(&update_file_btns);
        view.selection().connect_changed(move |_| update());
    }
    {
        let update = Rc::clone(&update_file_btns);
        pop.connect_show(move |_| update());
    }

    // 列头右键：自绘勾选菜单。
    // 不用 CheckButton：Deepin 主题下其勾选视觉在列 set_visible 触发父窗口
    // 重排后会滞后一次操作（queue_draw 也救不回）；改用统一下拉菜单的右侧
    // 勾选标记（ToggleButton + 图标），点击后收起，下次打开状态正确。
    let cols_pop = {
        let pop = crate::menu::popover(view);
        let vb = crate::menu::body();
        vb.append(&crate::menu::section(tr("显示列")));

        // (key, 行按钮)——右键弹出前同步勾选态用
        let rows: std::cell::RefCell<Vec<(&'static str, gtk::ToggleButton)>> =
            std::cell::RefCell::new(Vec::new());

        let mk = |vb: &gtk::Box,
                  rows: &std::cell::RefCell<Vec<(&'static str, gtk::ToggleButton)>>,
                  label: &'static str,
                  key: &'static str,
                  init: bool| {
            let b = crate::menu::toggle("view-list-symbolic", label, None, init);
            {
                let st2 = Rc::clone(st);
                // popover 内子组件的视觉重绘在 Deepin 主题下不可靠
                // （CheckButton/Label 均滞后），点击后收起菜单：
                // 列变化立即可见，下次打开状态正确
                let pop2 = pop.clone();
                b.connect_clicked(move |b| {
                    // 以 settings 为唯一事实源；点击后 ToggleButton 已翻转
                    let next = b.is_active();
                    crate::settings::set_column(remote, key, next);
                    let pane = st2.pane(remote);
                    match key {
                        "size" => pane.col_size.set_visible(next),
                        "mtime" => pane.col_mtime.set_visible(next),
                        "perm" => {
                            if let Some(cp) = &pane.col_perm {
                                cp.set_visible(next);
                            }
                        }
                        _ => {}
                    }
                    pop2.popdown();
                });
            }
            rows.borrow_mut().push((key, b.clone()));
            vb.append(&b);
        };

        let cfg = crate::settings::columns(remote);
        mk(&vb, &rows, tr("大小"), "size", cfg.size);
        mk(&vb, &rows, tr("修改时间"), "mtime", cfg.mtime);
        if remote {
            mk(&vb, &rows, tr("权限"), "perm", cfg.perm);
        }

        pop.set_child(Some(&vb));
        // 弹出前按 settings 同步勾选态（set_active 只改视觉，不会触发 clicked）
        pop.connect_show(move |_| {
            let cfg = crate::settings::columns(remote);
            for (key, b) in rows.borrow().iter() {
                let on = match *key {
                    "size" => cfg.size,
                    "mtime" => cfg.mtime,
                    _ => cfg.perm,
                };
                b.set_active(on);
            }
        });
        pop
    };

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    // 捕获阶段先记下多选：GtkTreeView 自己的右键处理会把多选收缩成
    // 光标下那一行。不能 Claim 整段事件——那会让弹层里的按钮收不到
    // hover/motion，菜单项就没有高亮；改为弹层弹出后的 idle 里恢复选择。
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let view2 = view.clone();
    let pop2 = pop.clone();
    gesture.connect_pressed(move |g, _n, x, y| {
        if g.current_button() != 3 {
            return;
        }
        // bin window 坐标 y<=0 = 列头区域 → 仅列勾选菜单
        let (bx, by) = view2.convert_widget_to_bin_window_coords(x as i32, y as i32);
        if by <= 0 {
            cols_pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            cols_pop.popup();
            return;
        }

        // 点在未选中的行上时，先把它变成当前选中项（文件管理器惯例）；
        // 点在已选中的行上则记下当前多选，等 TreeView 收缩后用 idle 恢复。
        // 注意 path_at_pos 收的是 **bin window** 坐标（不含列头）：直接用
        // 手势的 widget 坐标会整体下移一行，右键 A 实际选中/上传 B。
        if let Some((Some(path), _, _, _)) = view2.path_at_pos(bx, by) {
            let sel = view2.selection();
            if sel.path_is_selected(&path) {
                let old = sel.selected_rows().0;
                if old.len() > 1 {
                    let sel2 = sel.clone();
                    glib::idle_add_local_once(move || {
                        sel2.unselect_all();
                        for p in old {
                            sel2.select_path(&p);
                        }
                    });
                }
            } else {
                sel.unselect_all();
                sel.select_path(&path);
            }
        }
        let popover = pop2.clone();
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    view.add_controller(gesture);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 软链怎么认：权限串的首字符（`ls -l` 的规矩）。
    #[test]
    fn 权限串首字符_l_就是软链() {
        assert!(is_link_perm("lrwxrwxrwx"));
        assert!(!is_link_perm("drwxr-xr-x"));
        assert!(!is_link_perm("-rw-r--r--"));
        assert!(!is_link_perm(""));
    }

    /// **大小列的宽度：装得下内容，也尊重用户拉过的**（用户 2026-11-10：
    /// 「一定要保证最长的大小显示完整，就是列宽按最长的文件大小来定」）。
    #[test]
    fn 大小列宽_不许比内容窄_也不许比用户拉过的窄() {
        // 内容要 90px ⇒ 至少 90px（哪怕用户只拉了 40px ⇒ 装不下 ⇒ 弹回）
        assert_eq!(size_col_width(90, 40), 90);
        // 用户拉到了 140px ⇒ 尊重（更宽）
        assert_eq!(size_col_width(90, 140), 140);
        // 内容很小 ⇒ 也有下限（列头「大小」两个字要装得下）
        assert_eq!(size_col_width(10, 0), SIZE_COL_MIN_W);
        assert_eq!(size_col_width(0, 0), SIZE_COL_MIN_W);
        // 用户拉得极宽也照旧尊重
        assert_eq!(size_col_width(10, 300), 300);
    }

    /// 本地软链：权限串要是 `l` 开头（图标才能区分），但**行为不变**——
    /// 指向目录的软链仍然算「目录」（双击照样进得去）。
    #[cfg(unix)]
    #[test]
    fn 本地软链标成软链但仍按目标判定目录() {
        let dir = std::env::temp_dir().join(format!("pp_local_link_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target")).expect("建目录");
        std::fs::write(dir.join("target/a.txt"), "x").expect("写文件");
        std::os::unix::fs::symlink(dir.join("target"), dir.join("dirlink")).expect("造软链");
        std::os::unix::fs::symlink(dir.join("target/a.txt"), dir.join("filelink")).expect("造软链");

        let rows = read_local_dir(dir.to_str().unwrap()).expect("读目录");
        let get = |n: &str| {
            rows.iter()
                .find(|r| r.name == n)
                .unwrap_or_else(|| panic!("列表里该有 {n}"))
        };
        // 软链 ⇒ 权限串 `l` 开头（图标能区分）
        assert!(
            is_link_perm(&get("dirlink").perm),
            "目录软链的权限串该是 l 开头：{}",
            get("dirlink").perm
        );
        assert!(
            is_link_perm(&get("filelink").perm),
            "文件软链的权限串该是 l 开头：{}",
            get("filelink").perm
        );
        // ⚠️ 但**判定仍是按目标**：指向目录的软链还是「目录」
        assert!(get("dirlink").is_dir, "指向目录的软链仍该算目录");
        assert!(!get("filelink").is_dir, "指向文件的软链不该算目录");
        // 普通文件/目录不是软链
        assert!(!is_link_perm(&get("target").perm), "真目录不该算软链");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「在此处打开终端」的目标目录：目录进它、文件进它的上级、没选进当前。
    #[test]
    fn 在此处打开终端的目标目录() {
        // 右键目录 → 进它
        assert_eq!(
            term_target_dir(&[("/srv/data".into(), true)], "/home"),
            "/srv/data"
        );
        // 右键文件 → 进它所在的目录（去掉文件名那一段）
        assert_eq!(
            term_target_dir(&[("/srv/data/a.txt".into(), false)], "/home"),
            "/srv/data"
        );
        // 根目录下的文件 → 父目录是 "/"
        assert_eq!(term_target_dir(&[("/a.txt".into(), false)], "/"), "/");
        // 什么都没选 → 当前目录
        assert_eq!(term_target_dir(&[], "/home"), "/home");
        // 多选时用第一项
        assert_eq!(
            term_target_dir(
                &[("/srv/data".into(), true), ("/etc".into(), true)],
                "/home"
            ),
            "/srv/data"
        );
    }
}

/* ---------------- 拖拽 ----------------
 * 远程源：类型化字符串 + application/x-puttyplus-remote + text/plain
 *         （远端路径行，一行一个绝对路径）
 * 本地源：类型化 GdkFileList + text/uri-list（file:// URI，\r\n 分隔）
 * 远程目标：要「本地文件」⇒ 上传
 * 本地目标：要「远端路径行」⇒ 下载；要「本地文件」⇒ 复制进本目录
 *
 * ⚠️ 目标一律走 `super::dnd`（`GtkDropTargetAsync`）而不是
 * `gtk::DropTarget`：同步版一次「没落下的拖拽」会把 `self->drop`
 * 留在控件上，**之后所有拖拽全部哑掉**（详见 `dnd.rs` 的说明）。
 */

/** 拖拽源：本地文件 / 远端路径行。 */
fn install_drag(st: &Rc<PanelState>, remote: bool, view: &gtk::TreeView) {
    // ---- 源 ----
    let src = gtk::DragSource::new();
    src.set_actions(gdk::DragAction::COPY);
    {
        let st2 = Rc::clone(st);
        src.connect_prepare(move |_s, _x, _y| {
            st2.drag_from.set(Some(remote));
            let cwd = st2.pane_cwd(remote);
            let paths: Vec<String> = st2
                .pane(remote)
                .selected(&cwd)
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            if paths.is_empty() {
                return None;
            }
            Some(if remote {
                super::dnd::paths_provider(&paths)
            } else {
                super::dnd::files_provider(&paths)
            })
        });
    }
    {
        // 拖完就把"这是本程序内的拖拽"清掉。
        //
        // ⚠️ **不清会出真 bug**：标记只在 `prepare` 里设、从来不重置，
        // 于是"在程序里拖过一次"之后，再从**文件管理器**拖文件进来会被
        // 当成"同面板自拖"**直接拒掉**——用户看到的就是"拖进来没反应"。
        let st2 = Rc::clone(st);
        src.connect_drag_end(move |_s, _drag, _delete| {
            st2.drag_from.set(None);
        });
    }
    view.add_controller(src);

    // ---- 目标 ----
    //
    // ⚠️ 抓**弱引用**：控制器挂在 view 上，view 属于 PanelState 的控件树；
    // 抓强引用就是「PanelState → view → 控制器 → PanelState」的**引用环**，
    // 面板永远释放不掉（AGENTS.md 的红线）。
    {
        let weak = Rc::downgrade(st);
        let r = remote;
        super::dnd::attach(
            view.upcast_ref::<gtk::Widget>(),
            super::dnd::Want::Files,
            move |res| {
                let Some(st) = weak.upgrade() else { return };
                if st.drag_from.get() == Some(r) {
                    return; // 同面板自拖忽略
                }
                match res {
                    super::dnd::DropResult::Paths(paths) => {
                        if r {
                            // 拖到**远程文件列表** = 上传（跟拖到终端同一个入口）
                            super::drop_upload_paths(&st, paths);
                        } else {
                            // 拖到**本地文件列表** = 复制进当前目录（非递归，与 C 版一致）
                            let cwd = st.pane_cwd(false);
                            super::copy_uris_into_local(&st, &paths, &cwd);
                        }
                    }
                    super::dnd::DropResult::NoData => {
                        // 没拿到数据要说清楚（源端可能根本不给数据）
                        super::on_drop_result(&st, super::dnd::DropResult::NoData);
                    }
                }
            },
        );
    }
    if !remote {
        // 同上：弱引用，别把 PanelState 钉死。
        let weak = Rc::downgrade(st);
        super::dnd::attach(
            view.upcast_ref::<gtk::Widget>(),
            super::dnd::Want::Paths,
            move |res| {
                let Some(st) = weak.upgrade() else { return };
                if st.drag_from.get() == Some(false) {
                    return; // 本地面板自己拖出来的（那是上传方向）
                }
                if let super::dnd::DropResult::Paths(paths) = res {
                    super::start_downloads(&st, paths);
                }
            },
        );
    }
}
