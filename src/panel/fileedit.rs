//! 内置文本查看/编辑器（本地文件 + 远程 SFTP 文件）。
//!
//! 入口在文件面板右键菜单：`打开`（可改并写回）。
//!
//! **行号与语法高亮是自己实现的，没有引入 GtkSourceView**：GTK4 对应的
//! 是 gtksourceview-5，本机（Deepin/Debian 系）只打包了 GTK3 时代的
//! gtksourceview-3/4——要用 5 就得给每台堡垒机源码编译一个系统库，deb 包
//! 也得跟着加依赖。这里用原生 `gtk::TextView` + `TextTag` 自己拼，
//! 换来零新依赖：单二进制照旧、deb 包不用改、装到任何 GTK4 机器上都能用。
//! 代价是高亮不如 gtksourceview 精细（词法级，不做语法分析）。
//!
//! 能力边界（也在 FEATURE_PARITY 里写明，不在这里偷偷放宽）：
//! - **只处理文本**：含 NUL / 大量控制字符的二进制文件拒绝打开，避免
//!   "查看一下" 把图片/压缩包当文本改坏；
//! - **UTF-8**：非法 UTF-8 可以只读预览（非法字节显示为 `�`），但不允许
//!   保存——没有编码检测就无法保证写回后原文件不被改名；
//! - **单文件上限 8 MB**：`TextView` 不是大文件查看器，超过就提示下载后
//!   用外部编辑器；
//! - **保存是原子的**：本地/远端都先写同目录临时文件再 rename，网络中断
//!   不会留下半个正式文件；代价是 inode 被换掉（硬链接断开），远端符号
//!   链接会被换成普通文件、属主变为当前账号（权限位会抄过去；本地先
//!   canonicalize，写的是链接目标）；
//! - 保留 UTF-8 BOM 与 CRLF（加载时归一成 LF，保存时按原样写回）。
//!
//! 线程模型：本地读写直接在 UI 线程（≤ 8 MB，可接受）；远端读写走
//! `WorkerCmd::ReadFile` / `WriteFile`，回包带 `id`，由本模块按 id 分发。

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use super::PanelState;
use super::dialogs;
use super::syntax::{self, Lang, Tok};
use super::worker::WorkerCmd;
use crate::findbar;
use crate::util::{human_size, path_basename};
use crate::i18n::tr;
use crate::trf;

/// 内置查看/编辑器的单文件上限。硬编码，不给用户改——调大只会让
/// `TextView` 卡死，不是"支持大文件"。
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 语法高亮的大小上限：分词 + 上 tag 都跑在主线程上，超过这个就不高亮
/// （状态栏会说明），免得打开一个大文件就卡住。
const HIGHLIGHT_MAX_BYTES: usize = 1024 * 1024;

/// 行号槽里"影子字符"的个数上限：影子是**透明**的原文片段，只为把行号
/// 那一栏的行高撑成和正文一致（见 `rebuild_gutter` 的说明）。
/// 每行最多留 2 个即可——决定行高的是**字体**不是字数。
const SHADOW_CHARS: usize = 2;

/// Pango 的粗体字重（`set_weight` 收的是 CSS 数值，不是枚举）。
const PANGO_WEIGHT_BOLD: i32 = 700;

/// 行号栏底部额外留的空隙（像素）。正文有横向滚动条时它的可视区会比
/// 行号栏矮一截，两边滚动范围差这一点，底部补齐就不会在文件末尾错行。
const GUTTER_BOTTOM_PAD: i32 = 16;

/// 字号档位（CSS 类）。中间那档是默认。
///
/// ⚠️ 正文与行号栏**必须同一档**——行号栏靠"影子字符"把行高撑成和正文
/// 一致，字号不一样行高就对不上，整个文件看着是歪的。
const ZOOM_CLASSES: [&str; 5] = [
    "pp-editor-zoom-0",
    "pp-editor-zoom-1",
    "pp-editor-zoom-2",
    "pp-editor-zoom-3",
    "pp-editor-zoom-4",
];

/// 默认字号档（缩放的中间那档）。
const ZOOM_DEFAULT: usize = 2;

/// 进程内唯一的编辑器 id（远端读写的回包靠它找回窗口）。
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub struct FileEdit {
    pub id: u64,
    /// **宿主窗口**（阶段 12：编辑器是连接页里的子标签，不再是独立窗口）
    /// ——各种对话框挂在它下面。
    pub win: gtk::Window,
    /// 子标签页的内容（连接页的子标签 notebook 里的一页）
    pub page: gtk::Widget,
    /// 页内第一块的标题栏（独立窗口路径下它是窗口的标题栏）
    header: gtk::HeaderBar,
    /// 页内第一块：**工具栏**（阶段 84）——子标签里不再摆内层 HeaderBar，
    /// 改成一行工具栏；独立窗口路径下 HeaderBar 仍在窗口标题栏上。
    toolbar: gtk::Box,
    /// 查找/替换条（阶段 84）。默认隐藏。
    findbar: findbar::FindBar,
    /// 本编辑器在子标签里的页 id
    pub tab_id: Cell<u64>,
    /// 面板（弱引用）：改脏了要更新子标签上的标题（● 标记）。
    ///
    /// 用弱引用：面板持有子标签，子标签持有这个编辑器（回调），
    /// 面板再被编辑器强引用就成了环。
    st: Weak<PanelState>,
    remote: bool,
    path: String,
    /// 用户打开时选的是"查看"还是"编辑"；非法 UTF-8 会把实际可写性降成只读，
    /// 但重新载入后若变成合法 UTF-8，仍回到用户最初的选择。
    base_readonly: bool,
    /// 当前是否只读（= 用户选查看 或 内容不是合法 UTF-8）
    readonly: Cell<bool>,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    /// 行号栏（左边的窄条）
    gutter: gtk::TextView,
    /// 行号栏的滚动容器（宽度定在这里）
    gutter_scroll: gtk::ScrolledWindow,
    gutter_buffer: gtk::TextBuffer,
    /// 行号栏的纵向滚动条（跟正文同步）
    gutter_adj: gtk::Adjustment,
    /// 正文的纵向滚动条
    main_adj: gtk::Adjustment,
    /// **行号栏 ↔ 正文滚动同步的闸门**。
    ///
    /// ⚠️ 重建行号栏（`rebuild_gutter`）时必须把它关上：`set_text` 会把
    ///    行号栏的滚动条**归零**，而两边是**双向**同步的 ⇒ 归零会顺带把
    ///    **正文拽到顶部**（用户实测：长文件在底部敲一个字就跳顶，
    ///    跳完没法继续改）。关上闸门，重建期间行号栏就不牵动正文。
    scroll_guard: Rc<Cell<bool>>,
    /// 行号（普通色）
    num_tag: gtk::TextTag,
    /// 行号（光标所在行，用强调色）
    cur_tag: gtk::TextTag,
    /// 影子字符（透明）：只占位、不显示，用来把行号栏的行高撑成和正文一致
    shadow_tag: gtk::TextTag,
    /// 语法高亮用的 tag（按 `syntax::ALL_TOKS` 的顺序）
    tags: Vec<gtk::TextTag>,
    /// 识别出来的语言
    lang: Cell<Lang>,
    save_btn: gtk::Button,
    reload_btn: gtk::Button,
    /// 撤销
    undo_btn: gtk::Button,
    /// 重做
    redo_btn: gtk::Button,
    /// 「查找」按钮（呼出查找条）
    find_btn: gtk::Button,
    /// 「替换」按钮（呼出查找条 + 替换行）
    replace_btn: gtk::Button,
    /// 「自动换行」开关
    wrap_btn: gtk::ToggleButton,
    /// 「复制」
    copy_btn: gtk::Button,
    /// 「粘贴」
    paste_btn: gtk::Button,
    /// 「跳转到行」
    goto_btn: gtk::Button,
    /// 「放大字号」
    zoom_in_btn: gtk::Button,
    /// 「缩小字号」
    zoom_out_btn: gtk::Button,
    /// 当前字号档（`ZOOM_CLASSES` 的下标）
    zoom_idx: Cell<usize>,
    title_lbl: gtk::Label,
    sub_lbl: gtk::Label,
    notice: gtk::Label,
    status: gtk::Label,
    /// 状态栏右侧的光标位置
    pos_lbl: gtk::Label,
    /// 临时状态提示（查找替换的结果），`update_ui` 把它拼到状态栏后面
    status_note: Cell<Option<String>>,

    /* ---- 查找 / 替换（阶段 84）---- */
    /// 当前所有匹配（**字符偏移**区间）
    matches: RefCell<Vec<(i32, i32)>>,
    /// 当前匹配的下标；-1 = 还没选中任何一个
    match_idx: Cell<i32>,
    /// 「区分大小写」
    case_sensitive: Cell<bool>,
    /// 标题是在**工具栏**里（子标签模式）还是在窗口标题栏里（独立窗口模式）
    title_in_toolbar: Cell<bool>,

    dirty: Cell<bool>,
    loading: Cell<bool>,
    saving: Cell<bool>,
    close_after_save: Cell<bool>,
    valid_utf8: Cell<bool>,
    bom: Cell<bool>,
    crlf: Cell<bool>,
    size: Cell<u64>,
    /// 行号/高亮重建的防抖计时器是否已挂上
    refresh_pending: Cell<bool>,
    hl_pending: Cell<bool>,
    /// 行号栏当前是几位数（决定行号的定宽与高亮范围）
    gutter_digits: Cell<usize>,
    /// 当前被标成"当前行"的是第几行（光标移动时要把上一行恢复成普通色）
    cur_line: Cell<i32>,
    /// 是否真的上了高亮（超大文件会跳过，状态栏要说明）
    hl_on: Cell<bool>,
    /// 扫描异常是不是已经报过了（同一文件别反复刷 stderr）
    hl_warned: Cell<bool>,
    /// 等宽字体一个字符的宽度（像素），用来给行号栏定宽
    gutter_char_w: Cell<i32>,
}

/// 解码后的文本 + 写回时需要还原的格式信息。
#[derive(Debug, PartialEq)]
struct Decoded {
    text: String,
    bom: bool,
    crlf: bool,
    valid_utf8: bool,
    bytes: u64,
}

impl Decoded {
    fn decode(data: &[u8]) -> Decoded {
        let bom = data.starts_with(&[0xEF, 0xBB, 0xBF]);
        let body = if bom { &data[3..] } else { data };
        let valid_utf8 = std::str::from_utf8(body).is_ok();
        let raw = String::from_utf8_lossy(body).into_owned();
        // 只有"所有换行都是 CRLF"才按 CRLF 处理；混合换行不做猜测，
        // 保持原样（新输入的行用 LF），避免把 Unix 文件整体改成 CRLF。
        let crlf_count = raw.as_bytes().windows(2).filter(|w| *w == b"\r\n").count();
        let lf_count = raw.bytes().filter(|b| *b == b'\n').count();
        let crlf = crlf_count > 0 && crlf_count == lf_count;
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Decoded {
            text,
            bom,
            crlf,
            valid_utf8,
            bytes: data.len() as u64,
        }
    }

    fn encode(&self, text: &str) -> Vec<u8> {
        let body = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if self.bom {
            let mut out = Vec::with_capacity(body.len() + 3);
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            out.extend_from_slice(body.as_bytes());
            out
        } else {
            body.into_bytes()
        }
    }
}

/// 可疑二进制：NUL、UTF-16 BOM，或控制字符占比超过 30%。
/// 只看前 8 KiB——看全部反而会被文件中段的一小段二进制误伤。
fn looks_binary(data: &[u8]) -> bool {
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        return true;
    }
    let sample = &data[..data.len().min(8192)];
    if sample.is_empty() {
        return false;
    }
    if sample.contains(&0) {
        return true;
    }
    // 不把 ESC 算进去：终端彩色日志（\x1b[31m…）是纯文本，误判成二进制
    // 就没法看了。阈值放到 30%——真实的二进制文件控制字符占比远高于此。
    let ctrl = sample
        .iter()
        .filter(|&&b| b < 0x09 || (b > 0x0D && b < 0x20 && b != 0x1B) || b == 0x7F)
        .count();
    ctrl * 100 / sample.len() > 30
}

/* ---------------- 打开入口 ---------------- */

/// 打开本地文件。`readonly` = 查看模式。
pub fn open_local(st: &Rc<PanelState>, path: &str, readonly: bool) {
    if focus_existing(st, false, path, readonly) {
        return;
    }
    let id = next_id();
    match read_local(path) {
        Ok(data) => {
            if let Err(msg) = apply_bytes(st, id, false, path, readonly, data) {
                dialogs::show_error(st.topwin().as_ref(), tr("无法打开文件"), &msg);
            }
        }
        Err(msg) => dialogs::show_error(st.topwin().as_ref(), tr("无法打开文件"), &msg),
    }
}

/// 请求远端文件内容；回包由 `on_read` 接住后创建窗口。
pub fn open_remote(st: &Rc<PanelState>, path: &str, readonly: bool) {
    if focus_existing(st, true, path, readonly) {
        return;
    }
    if !st.conn_ok.get() {
        dialogs::show_error(
            st.topwin().as_ref(),
            tr("无法打开文件"),
            tr("SFTP 尚未连接，无法读取远程文件。"),
        );
        return;
    }
    let id = next_id();
    st.pending_file_reads.borrow_mut().insert(id, readonly);
    if !send(
        st,
        WorkerCmd::ReadFile {
            id,
            path: path.to_string(),
            max: MAX_BYTES,
        },
    ) {
        st.pending_file_reads.borrow_mut().remove(&id);
        dialogs::show_error(
            st.topwin().as_ref(),
            tr("无法打开文件"),
            tr("SFTP 尚未连接，无法读取远程文件。"),
        );
    }
}

/// 远端读取结果（主线程）。
pub fn on_read(st: &Rc<PanelState>, id: u64, path: &str, result: Result<Vec<u8>, String>) {
    let Some(readonly) = st.pending_file_reads.borrow_mut().remove(&id) else {
        return; // 窗口已关 / 请求已作废
    };
    let existing = find(st, id);
    if let Some(e) = &existing {
        e.loading.set(false);
        // 重新载入期间视图被临时锁成只读；失败时要恢复原可写性，
        // 不能因为一次读取失败就把编辑器永久锁住
        e.view.set_editable(!e.readonly.get());
        e.view.set_cursor_visible(!e.readonly.get());
        update_ui(e);
    }
    match result {
        Ok(data) => {
            if let Err(msg) = apply_bytes(st, id, true, path, readonly, data) {
                let parent = existing
                    .as_ref()
                    .map(|e| e.win.clone())
                    .or_else(|| st.topwin());
                dialogs::show_error(parent.as_ref(), tr("无法打开文件"), &msg);
            }
        }
        Err(err) => {
            let parent = existing
                .as_ref()
                .map(|e| e.win.clone())
                .or_else(|| st.topwin());
            dialogs::show_error(
                parent.as_ref(),
                tr("读取远程文件失败"),
                &format!("{path}\n{err}"),
            );
        }
    }
}

/// 远端保存结果（主线程）。
pub fn on_written(st: &Rc<PanelState>, id: u64, path: &str, result: Result<(), String>) {
    let Some(e) = find(st, id) else {
        return; // 保存期间窗口被关掉了
    };
    e.saving.set(false);
    // 保存期间视图被临时锁住；无论成功失败都恢复用户原本的可写性
    e.view.set_editable(!e.readonly.get());
    e.view.set_cursor_visible(!e.readonly.get());
    match result {
        Ok(()) => {
            e.dirty.set(false);
            update_ui(&e);
            super::refresh_after_edit_save(st, true);
            if e.close_after_save.get() {
                close_tab(st, &e);
                // 统一关闭流程在等这个回包（阶段 63）
                super::closeguard::note_save_done_deferred();
            }
        }
        Err(err) => {
            e.close_after_save.set(false);
            update_ui(&e);
            dialogs::show_error(Some(&e.win), tr("保存失败"), &format!("{path}\n{err}"));
            // 保存失败 ⇒ 这次关闭作废：页签留着，用户能救数据（阶段 63）
            super::closeguard::note_save_failed();
        }
    }
}

/// 标签关闭时一并销毁编辑器窗口（有未保存修改也直接关，关标签的语义
/// 是用户要结束这个会话；再弹一串询问只会挡住关窗）。
pub fn close_all(st: &Rc<PanelState>) {
    let list: Vec<Rc<FileEdit>> = st.file_edits.borrow_mut().drain(..).collect();
    st.pending_file_reads.borrow_mut().clear();
    for e in list {
        close_tab(st, &e);
    }
}

/// 冒烟 / e2e 用：往编辑器里插一段文字（模拟用户打字）。
///
/// 走的是**真的 buffer 变更** ⇒ 走的是真的 `connect_changed` 回调 ⇒
/// 「有未保存的修改」这个状态是真的（不是手动塞标志位）。
pub fn type_into(st: &Rc<PanelState>, path: &str, text: &str) -> bool {
    let target = st
        .file_edits
        .borrow()
        .iter()
        .find(|e| e.path == path)
        .cloned();
    let Some(e) = target else { return false };
    let buf = e.buffer.clone();
    buf.insert_at_cursor(text);
    true
}

/// 冒烟 / e2e 用：某个编辑器当前是不是"有未保存的修改"。
pub fn is_dirty(st: &Rc<PanelState>, path: &str) -> bool {
    st.file_edits
        .borrow()
        .iter()
        .find(|e| e.path == path)
        .map(|e| e.dirty.get())
        .unwrap_or(false)
}

/* ---------------- 脏状态（阶段 63：统一关闭流程） ---------------- */

/// 有未保存修改的编辑器个数。
pub fn dirty_count(st: &Rc<PanelState>) -> usize {
    st.file_edits
        .borrow()
        .iter()
        .filter(|e| e.dirty.get())
        .count()
}

/// **能存**的编辑器（脏 + 不是只读 + 内容合法 + 现在没在存/在读）。
fn savable_editors(st: &Rc<PanelState>) -> Vec<Rc<FileEdit>> {
    st.file_edits
        .borrow()
        .iter()
        .filter(|e| {
            e.dirty.get()
                && !e.readonly.get()
                && e.valid_utf8.get()
                && !e.saving.get()
                && !e.loading.get()
        })
        .cloned()
        .collect()
}

/// 能存的编辑器个数（"保存并关闭"按钮要不要给，看这个）。
pub fn savable_dirty_count(st: &Rc<PanelState>) -> usize {
    savable_editors(st).len()
}

/// 保存所有能存的编辑器，返回**实际发起了几次**保存。
///
/// 保存完（成功）后编辑器自己会关页签；统一关闭流程在等这些回包
/// （见 `closeguard::SaveBarrier`）。
pub fn start_save_all_dirty(st: &Rc<PanelState>) -> usize {
    let list = savable_editors(st);
    let mut n = 0;
    for e in list {
        // ⚠️ `close_after_save` 要**先**设：本地文件的保存是**同步**完成的
        // （`save()` 内部就写完盘了），后设的话保存早就结束了、没人再报完成。
        e.close_after_save.set(true);
        if save(&e, st) {
            n += 1;
        } else {
            // 没存成（内容超上限等）⇒ 别关页签
            e.close_after_save.set(false);
        }
    }
    n
}

fn find(st: &Rc<PanelState>, id: u64) -> Option<Rc<FileEdit>> {
    st.file_edits.borrow().iter().find(|e| e.id == id).cloned()
}

/// 从面板登记表移除一个编辑器窗口。
///
/// 不能只依赖 `connect_destroy`：实测 GtkWindow 在 close-request 里
/// `destroy()` 后窗口已不可见，但 `destroy` 信号不一定跑到这里，
/// 于是登记表还留着它，下次“编辑”会把旧“查看”窗重新 present 出来。
fn forget(st: &Rc<PanelState>, id: u64) {
    st.file_edits.borrow_mut().retain(|x| x.id != id);
    st.pending_file_reads.borrow_mut().remove(&id);
}

/// 同一路径已经开着一个窗口就聚焦它，返回 true。
/// 两个窗口各改各的会互相覆盖，而用户以为自己看到的只有一份。
///
/// 模式不同时：
/// - 已有可编辑窗、又以只读方式打开：聚焦原窗（不销毁，避免丢掉未保存修改）；
/// - 已有只读窗、又要求可编辑：只读窗没有未保存内容，直接销毁后重开为可编辑窗，
///   否则用户会看到一个标题仍是“查看”的窗口。
fn focus_existing(st: &Rc<PanelState>, remote: bool, path: &str, readonly: bool) -> bool {
    let existing = st
        .file_edits
        .borrow()
        .iter()
        .find(|e| e.remote == remote && e.path == path)
        .cloned();
    let Some(e) = existing else { return false };
    if e.base_readonly == readonly || !e.base_readonly {
        // 聚焦已开的那一页（原来是 present 弹窗）
        if let Some(sub) = st.sub_tabs() {
            sub.switch_to(e.tab_id.get());
        }
        return true;
    }
    // 已有只读查看窗，但这次要编辑：关掉旧的（本来就是只读，没有可丢内容）
    close_tab(st, &e);
    false
}

fn send(st: &Rc<PanelState>, cmd: WorkerCmd) -> bool {
    match st.tx.borrow().as_ref() {
        Some(tx) => tx.send(cmd).is_ok(),
        None => false,
    }
}

/* ---------------- 解码 / 建窗 ---------------- */

fn apply_bytes(
    st: &Rc<PanelState>,
    id: u64,
    remote: bool,
    path: &str,
    base_readonly: bool,
    data: Vec<u8>,
) -> Result<(), String> {
    if data.len() as u64 > MAX_BYTES {
        return Err(too_large_msg(path, data.len() as u64));
    }
    if looks_binary(&data) {
        return Err(trf!("{path}\n\n疑似二进制文件（含 NUL 字节或大量控制字符）。\n内置查看/编辑器只处理文本文件，请下载后用对应的外部程序打开。", path = path));
    }
    let dec = Decoded::decode(&data);

    if let Some(e) = find(st, id) {
        // 重新载入：保留窗口与 id，只换内容
        e.loading.set(false);
        e.saving.set(false);
        e.valid_utf8.set(dec.valid_utf8);
        e.readonly.set(base_readonly || !dec.valid_utf8);
        e.bom.set(dec.bom);
        e.crlf.set(dec.crlf);
        e.view.set_editable(!e.readonly.get());
        e.view.set_cursor_visible(!e.readonly.get());
        e.loading.set(true);
        e.buffer.set_text(&dec.text);
        e.loading.set(false);
        e.dirty.set(false);
        e.size.set(dec.bytes);
        // 重新载入可能换了语言（同一个路径，内容变了）
        e.lang.set(Lang::detect(path, first_line(&dec.text)));
        rebuild_gutter(&e);
        apply_highlight(&e);
        update_cursor(&e);
        update_ui(&e);
        return Ok(());
    }

    // 兜底（正常入口已在读取前挡掉）：两个请求同时在路上时仍只建一个窗
    if focus_existing(st, remote, path, base_readonly) {
        return Ok(());
    }

    let e = build(st, id, remote, path, base_readonly, &dec);
    st.file_edits.borrow_mut().push(Rc::clone(&e));
    attach_tab(st, &e);
    e.buffer.place_cursor(&e.buffer.start_iter());
    Ok(())
}

/// 把编辑器挂到**连接页的子标签**上（阶段 12：不再是独立弹窗）。
fn attach_tab(st: &Rc<PanelState>, e: &Rc<FileEdit>) {
    let Some(sub) = st.sub_tabs() else {
        // 没有子标签容器（老路径 / 单测 / 示例）：退回独立窗口。
        //
        // **为什么必须兜底**：这一页若谁也不持有，它就是「存在但看不见」——
        // 用户点了"打开文件"却毫无反应，比报错还难查。
        e.win.set_title(Some(&tab_title(e)));
        e.win.set_default_size(900, 640);
        // 独立窗口：那块 HeaderBar **就是窗口标题栏**，窗口控件
        // （— ▣ ×）才有地方显示（子标签路径下它们是关的，见 build 里的说明）。
        // ⚠️ 阶段 84 起 HeaderBar **不再塞进页面那个 Box**（子标签里改用一行
        // 工具栏），所以这里不用"先摘再挂"了——同一个 widget 不能既在 Box
        // 里又当标题栏（GTK4 会断言）。
        e.win.set_titlebar(Some(&e.header));
        e.win.set_child(Some(&e.page));
        e.win.present();
        return;
    };
    // 关闭前的检查：返回 true = 可以关，false = 不关（弹了询问，等用户选）。
    //
    // ⚠️ 只能抓**弱引用**：回调存在子标签里，而子标签挂在 PanelState 上，
    // 抓强引用就成环了（PanelState → 子标签 → 回调 → PanelState），面板永远
    // 释放不掉。
    let ew = Rc::downgrade(e);
    let stw = Rc::downgrade(st);
    let on_close: Box<dyn Fn() -> bool> = Box::new(move || {
        let (Some(e), Some(st)) = (ew.upgrade(), stw.upgrade()) else {
            return true;
        };
        if e.saving.get() {
            dialogs::show_error(Some(&e.win), tr("正在保存"), tr("文件正在写入，请稍候再关闭。"));
            return false;
        }
        if e.loading.get() && e.dirty.get() {
            // 载入中的回包会覆盖缓冲区，这时保存按钮是禁用的；
            // 让用户等这一拍，别出现"点了保存却什么都没发生"。
            dialogs::show_error(Some(&e.win), tr("正在载入"), tr("文件正在读取，请稍候再关闭。"));
            return false;
        }
        if e.dirty.get() && !e.readonly.get() {
            confirm_close(&e, &st);
            return false;
        }
        // 没有未保存内容：从登记表摘掉，这一页就可以安全地关掉了
        forget(&st, e.id);
        true
    });
    let tab_id = sub.add_page_icon_kind(
        Some(tab_icon(e)),
        &tab_title(e),
        super::subtabs::PageKind::FileEdit,
        e.page.clone(),
        Some(on_close),
    );
    e.tab_id.set(tab_id);
    sub.switch_to(tab_id);
}

/// 关掉编辑器所在的子标签（**已经确认过**可以直接关的路径用这个）。
fn close_tab(st: &Rc<PanelState>, e: &Rc<FileEdit>) {
    forget(st, e.id);
    match st.sub_tabs() {
        Some(sub) => {
            sub.remove_page(e.tab_id.get());
        }
        // 独立窗口路径（没有子标签容器）：页签没得摘，得把窗口本身关掉，
        // 否则关连接标签后编辑器窗口还飘在桌面上。
        //
        // ⚠️ 子标签路径**不能** destroy 窗口：那时 `e.win` 是宿主窗口
        // （主窗口），destroy 掉等于整个程序没了。
        None => {
            e.win.destroy();
        }
    }
}

/// 子标签上的标题。
///
/// **不要「编辑」二字**（用户要求）：编辑/查看由**图标**表示（见
/// `tab_icon`），标题只留文件名 + 脏标记。省下来的宽度让长文件名
/// 更不容易被压成省略号。
fn tab_title(e: &FileEdit) -> String {
    let base = path_basename(&e.path);
    let star = if e.dirty.get() { "● " } else { "" };
    format!("{star}{base}")
}

/// 子标签上的图标：可编辑是「铅笔」，只读是「文档」。
fn tab_icon(e: &FileEdit) -> &'static str {
    if e.base_readonly {
        "text-x-generic-symbolic"
    } else {
        "document-edit-symbolic"
    }
}

fn build(
    st: &Rc<PanelState>,
    id: u64,
    remote: bool,
    path: &str,
    base_readonly: bool,
    dec: &Decoded,
) -> Rc<FileEdit> {
    // **宿主窗口 = 主窗口**（阶段 12：编辑器不再是独立窗口了）。
    // 对话框（未保存确认、保存失败…）挂在它下面。
    //
    // ⚠️ 没有子标签容器时（老路径 / 单测 / 示例）必须用自己的窗口：那时
    // 这个窗口是这一页唯一的容器，借主窗口的话 `set_child` 会把主窗口
    // 的内容整个换掉。
    let has_sub = st.sub_tabs().is_some();
    let win = if has_sub {
        st.topwin().unwrap_or_else(gtk::Window::new)
    } else {
        gtk::Window::new()
    };

    let header = gtk::HeaderBar::new();
    header.add_css_class("pp-header");
    // **子标签页里不要显示窗口控件**：HeaderBar 默认会画「最小化 / 最大化 /
    // 关闭」三个按钮，而它们操作的是**宿主窗口**——编辑器变成子标签后，宿主
    // 是主窗口，用户点一下 × 会把整个程序关掉（而不是关掉这个页签）。
    // 独立窗口路径（没有子标签容器）才把它们放出来。
    header.set_show_title_buttons(!has_sub);

    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title_lbl = gtk::Label::new(None);
    title_lbl.set_xalign(0.0);
    title_lbl.add_css_class("pp-app-title");
    let who = if remote { tr("远程") } else { tr("本地") };
    let full = format!("{who} · {path}");
    let sub_lbl = gtk::Label::new(Some(&full));
    sub_lbl.set_xalign(0.0);
    sub_lbl.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    sub_lbl.set_max_width_chars(64);
    sub_lbl.set_tooltip_text(Some(&full));
    sub_lbl.add_css_class("pp-hint");
    title_box.append(&title_lbl);
    title_box.append(&sub_lbl);

    // ---- 工具栏（阶段 84）----
    //
    // 子标签里原先摆的是一块**内层 HeaderBar**（阶段 12 的遗留）：它长得
    // 像窗口标题栏，可它操作的其实是子标签页——「×」还一度会关掉整个程序
    // （所以只能把窗口控件藏掉）。现在改成**一行工具栏**：左边是标题，
    // 右边一排纯图标按钮（说明文字在 tooltip 里）。
    //
    // 独立窗口路径（没有子标签容器）**保留 HeaderBar 当窗口标题栏**，
    // 工具栏照样有——按钮在哪儿都一样，用户不用记两套位置。
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    toolbar.add_css_class("pp-toolbar");

    // ⚠️ **纯图标按钮**（用户 2026-10-28：主界面尽量少用文字）：
    // 说明文字挪进 tooltip。
    //
    // ⚠️ 保存**不是特殊功能**（用户 2026-10-31）：原来它挂着
    // `suggested-action`（整排按钮里唯一的蓝底高亮），看着像"主操作"，
    // 实际每一步都点得到 ⇒ 高亮只会误导。现在跟别的按钮一样是普通图标。
    // 图标也从"箭头落进托盘"（document-save）换成**软盘**（media-floppy）——
    // 后者才是"保存"的老图标，一眼能认。
    let save_btn = crate::a11y::icon_button("media-floppy-symbolic");
    save_btn.add_css_class("flat");
    save_btn.set_tooltip_text(Some(tr("保存到原文件 (Ctrl+S)")));

    let reload_btn = crate::a11y::icon_button("view-refresh-symbolic");
    reload_btn.add_css_class("flat");
    reload_btn.set_tooltip_text(Some(tr("放弃未保存的修改，从磁盘/远程重新载入 (F5)")));

    let undo_btn = crate::a11y::icon_button("edit-undo-symbolic");
    undo_btn.add_css_class("flat");
    undo_btn.set_tooltip_text(Some(tr("撤销 (Ctrl+Z)")));

    let redo_btn = crate::a11y::icon_button("edit-redo-symbolic");
    redo_btn.add_css_class("flat");
    redo_btn.set_tooltip_text(Some(tr("重做 (Ctrl+Shift+Z)")));

    let find_btn = crate::a11y::icon_button("edit-find-symbolic");
    find_btn.add_css_class("flat");
    find_btn.set_tooltip_text(Some(tr("查找 (Ctrl+F)")));

    let replace_btn = crate::a11y::icon_button("edit-find-replace-symbolic");
    replace_btn.add_css_class("flat");
    replace_btn.set_tooltip_text(Some(tr("替换 (Ctrl+H)")));

    // 「自动换行」是个开关，用 ToggleButton（按下=开着）
    let wrap_btn = gtk::ToggleButton::new();
    wrap_btn.add_css_class("flat");
    wrap_btn.set_child(Some(&gtk::Image::from_icon_name(
        "format-justify-fill-symbolic",
    )));
    crate::a11y::watch_tooltip(&wrap_btn);
    wrap_btn.set_tooltip_text(Some(tr("自动换行")));

    let copy_btn = crate::a11y::icon_button("edit-copy-symbolic");
    copy_btn.add_css_class("flat");
    copy_btn.set_tooltip_text(Some(tr("复制选中的文字 (Ctrl+C)")));

    let paste_btn = crate::a11y::icon_button("edit-paste-symbolic");
    paste_btn.add_css_class("flat");
    paste_btn.set_tooltip_text(Some(tr("粘贴 (Ctrl+V)")));

    let goto_btn = crate::a11y::icon_button("go-jump-symbolic");
    goto_btn.add_css_class("flat");
    goto_btn.set_tooltip_text(Some(tr("跳转到行 (Ctrl+G)")));

    let zoom_in_btn = crate::a11y::icon_button("zoom-in-symbolic");
    zoom_in_btn.add_css_class("flat");
    zoom_in_btn.set_tooltip_text(Some(tr("放大字号 (Ctrl++)")));

    let zoom_out_btn = crate::a11y::icon_button("zoom-out-symbolic");
    zoom_out_btn.add_css_class("flat");
    zoom_out_btn.set_tooltip_text(Some(tr("缩小字号 (Ctrl+-)")));

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    if !has_sub {
        // 独立窗口：标题进窗口标题栏
        header.set_title_widget(Some(&title_box));
    }
    // ⚠️ **按钮在左、文件名在右**（用户 2026-10-31）：工具栏的入口靠左，
    // 视线先落在"能做什么"上；文件名退到右边当上下文（原来是文件名在左、
    // 按钮在右，视线得先扫过文件名才知道这是哪个文件）。
    toolbar.append(&undo_btn);
    toolbar.append(&redo_btn);
    let sep1 = gtk::Separator::new(gtk::Orientation::Vertical);
    toolbar.append(&sep1);
    toolbar.append(&copy_btn);
    toolbar.append(&paste_btn);
    toolbar.append(&find_btn);
    toolbar.append(&replace_btn);
    toolbar.append(&goto_btn);
    toolbar.append(&wrap_btn);
    toolbar.append(&zoom_out_btn);
    toolbar.append(&zoom_in_btn);
    let sep2 = gtk::Separator::new(gtk::Orientation::Vertical);
    toolbar.append(&sep2);
    toolbar.append(&save_btn);
    toolbar.append(&reload_btn);
    toolbar.append(&spacer);
    if has_sub {
        // 子标签：标题进工具栏（HeaderBar 整个不显示）
        title_lbl.set_xalign(1.0);
        sub_lbl.set_xalign(1.0);
        toolbar.append(&title_box);
    }

    // 查找/替换条（阶段 84）：默认收起，按 Ctrl+F / Ctrl+H 才出来
    let findbar = findbar::build(findbar::FindOpts::full());
    findbar.root.add_css_class("pp-findbar");
    findbar.root.set_visible(false);

    // 阶段 12：标题栏不再是窗口标题栏，而是**子标签页里的第一块**。
    // （原来它是 `win.set_titlebar()`，独立窗口才有标题栏。）
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&toolbar);
    page.append(&findbar.root);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let notice = gtk::Label::new(None);
    notice.set_xalign(0.0);
    notice.set_wrap(true);
    notice.set_selectable(true);
    notice.add_css_class("pp-chip");
    notice.add_css_class("pp-chip-warn");
    notice.set_margin_start(10);
    notice.set_margin_end(10);
    notice.set_margin_top(6);
    notice.set_margin_bottom(2);
    notice.set_visible(false);
    root.append(&notice);

    let view = gtk::TextView::new();
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::None);
    view.set_editable(!(base_readonly || !dec.valid_utf8));
    view.set_cursor_visible(!(base_readonly || !dec.valid_utf8));
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_left_margin(10);
    view.set_right_margin(10);
    view.add_css_class("pp-editor");
    let buffer = view.buffer();
    buffer.set_enable_undo(true);

    // ---- 行号栏 ----
    // 单独一个 TextView，靠"影子字符"把行高撑成和正文一致（见 rebuild_gutter）。
    let gutter = gtk::TextView::new();
    gutter.set_monospace(true);
    gutter.set_wrap_mode(gtk::WrapMode::None);
    gutter.set_editable(false);
    gutter.set_cursor_visible(false);
    gutter.set_can_focus(false);
    gutter.set_top_margin(8);
    // 底部多留一截：正文有横向滚动条时它的可视区比行号栏矮一截，
    // 两边滚动范围差这一点，底部补齐就不会在文件末尾错行。
    gutter.set_bottom_margin(8 + GUTTER_BOTTOM_PAD);
    gutter.set_left_margin(6);
    gutter.set_right_margin(6);
    gutter.add_css_class("pp-gutter");
    // 字号档：正文与行号栏**同一档**（见 `ZOOM_CLASSES` 的说明——行号栏靠
    // 影子字符撑行高，字号不一样整个文件看着是歪的）
    apply_zoom(&view, &gutter, ZOOM_DEFAULT);
    let gutter_buffer = gutter.buffer();

    // 影子字符：看不见，但仍然参与排版，把这行的高度撑到和正文一样。
    //
    // **怎么让它看不见**（踩过两次坑）：
    // - `foreground: "transparent"` 字符串：TextTag 的 foreground 走 GdkRGBA
    //   解析，不认 CSS 的 transparent 关键字，解析失败等于没设 → 字直接露出来；
    // - `foreground_rgba(alpha=0)`：GTK4 **不吃前景色的 alpha**——实测单独给
    //   一段字挂 alpha=0 的前景色，字照样画出来（探针验证）。所以"透明"这条路
    //   走不通。
    // 真正靠谱的办法：**把影子画成行号栏的底色**（同色即隐形），另外行号栏
    // 本身宽度就只够放数字，影子字形会被裁掉——两道保险。
    let shadow_tag = gtk::TextTag::new(Some("pp-shadow"));
    shadow_tag.set_foreground(Some(&crate::theme::panel_hex()));
    gutter_buffer.tag_table().add(&shadow_tag);
    // 行号本体：普通色 / 光标所在行用强调色
    let num_tag = gtk::TextTag::new(Some("pp-num"));
    let cur_tag = gtk::TextTag::new(Some("pp-num-cur"));
    cur_tag.set_weight(PANGO_WEIGHT_BOLD);
    gutter_buffer.tag_table().add(&num_tag);
    gutter_buffer.tag_table().add(&cur_tag);
    // 注意：**不设 tag 优先级**。GTK4 里两个 tag 抢同一个属性时谁赢并不按
    // 文档的"优先级高的赢"来（实测四种优先级组合都是先挂的赢），而
    // `set_priority` 还有两条硬性要求（必须进表之后设、且小于表里 tag 数），
    // 设错就是一条 Gtk-CRITICAL 还白搭。改成**让 tag 不重叠**：
    // 影子那段把行号色摘掉、当前行那段把行号色摘掉，各自只留一个 tag。

    let gutter_sw = gtk::ScrolledWindow::new();
    // 行号栏**不画滚动条**：整屏只该有正文右边那一根。
    //
    // 之前用的是 `Automatic`，行号栏自己长出一根 15px 宽的竖条，夹在行号
    // 和正文之间，看着像"行号是个独立窗口"——Zed / gedit / VS Code 都没有
    // 这东西。CSS 里把 `min-width` 压成 0 压不住 GTK4 的滚动条节点
    // （实测四种选择器都无效），最后靠**策略**解决：
    //
    // - `Never`：不显示滚动条，但 GTK4 认为"内容不能靠滚动条看到"，
    //   ScrolledWindow 的最小高度就变成子控件（行号文本）的整段高度——
    //   长文件直接把窗口撑到几万像素高，X 直接 BadAlloc。**不能用**。
    // - `External`：滚动条由应用自己画，ScrolledWindow 既不显示也不给它留
    //   位置，但 adjustment 照常工作（行号栏靠它跟正文同步）。
    //
    // 实测（探针）：三种策略下窗口高度 300（不炸）、`External` 文本区不被
    // 挤压、`adj.set_value(500)` 视图跟着滚。
    gutter_sw.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
    gutter_sw.set_hexpand(false);
    gutter_sw.set_vexpand(true);
    // 宽度只按行号算，**不能**跟着内容走：内容里有透明的影子字符，
    // 按内容算会把行号栏撑得比需要的宽一截。
    gutter_sw.set_propagate_natural_width(false);
    gutter_sw.set_child(Some(&gutter));
    gutter_sw.add_css_class("pp-gutter-scroll");

    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(&view));
    // **不挂 Adwaita 的 "frame"**：那是一圈 1px 边框 + 圆角，画在正文左边
    // 就成了"行号和正文之间的分隔线"，看着像两个窗口拼的。Zed / gedit /
    // VS Code 的编辑器都是一整块，行号栏和正文之间没有任何边框。
    sw.add_css_class("pp-editor-scroll");
    // 行号栏和正文之间也不留缝：留缝的话浅色主题下那一道窗口底色的灰
    // 条照样像分隔线。行号栏自己的右边距(6) + 正文的左边距(10) 已经
    // 留出了足够的空隙。
    sw.set_margin_start(0);
    sw.set_margin_end(8);

    // **行号栏上的滚轮 → 带着正文一起滚**。
    //
    // 行号栏的滚动条是"跟随"正文的（见 `sync_scroll`），它自己滚了也
    // 不会牵动正文 ⇒ 得在这儿把滚轮事件转给正文，否则在行号上滚滚轮
    // 什么都不会发生（gedit / Zed / VS Code 里都是滚正文）。
    {
        let m = sw.vadjustment();
        let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::empty());
        sc.connect_scroll(move |_, _dx, dy| {
            if dy == 0.0 {
                return gtk::glib::Propagation::Proceed;
            }
            let adj = m.clone();
            // 一个滚轮档 ≈ 60px（跟 GTK 默认的滚动手感接近）
            let delta = -dy * 60.0;
            let lo = adj.lower();
            let hi = (adj.upper() - adj.page_size()).max(lo);
            adj.set_value((adj.value() + delta).clamp(lo, hi));
            // 吃掉它：行号栏自己不滚（它的位置只跟着正文）
            gtk::glib::Propagation::Stop
        });
        gutter.add_controller(sc);
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.append(&gutter_sw);
    row.append(&sw);
    root.append(&row);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_selectable(true);
    status.add_css_class("pp-hint");
    status.set_margin_start(10);
    status.set_margin_end(10);
    status.set_margin_top(4);
    status.set_margin_bottom(8);
    status.set_hexpand(true);

    // 右侧：光标位置（行/列）
    let pos_lbl = gtk::Label::new(None);
    pos_lbl.set_xalign(1.0);
    pos_lbl.set_selectable(true);
    pos_lbl.add_css_class("pp-hint");
    pos_lbl.set_margin_start(8);
    pos_lbl.set_margin_end(10);
    pos_lbl.set_margin_top(4);
    pos_lbl.set_margin_bottom(8);

    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    status_row.append(&status);
    status_row.append(&pos_lbl);
    root.append(&status_row);

    // 子标签页 = [标题栏][正文]
    page.append(&root);

    // 语法高亮的 tag：一类一个，颜色按当前主题定死
    let colors = crate::theme::syn_colors();
    // 行号：普通号用注释色（灰），当前行用强调色
    num_tag.set_foreground(Some(colors.comment));
    cur_tag.set_foreground(Some(colors.func));
    let tags: Vec<gtk::TextTag> = syntax::ALL_TOKS
        .iter()
        .map(|t| {
            let tag = make_tok_tag(*t, &colors);
            buffer.tag_table().add(&tag);
            tag
        })
        .collect();

    let e = Rc::new(FileEdit {
        id,
        win: win.clone(),
        page: page.clone().upcast(),
        header: header.clone(),
        toolbar,
        findbar,
        tab_id: Cell::new(0),
        st: Rc::downgrade(st),
        remote,
        path: path.to_string(),
        base_readonly,
        readonly: Cell::new(base_readonly || !dec.valid_utf8),
        view: view.clone(),
        buffer: buffer.clone(),
        gutter,
        gutter_scroll: gutter_sw.clone(),
        gutter_buffer,
        gutter_adj: gutter_sw.vadjustment(),
        main_adj: sw.vadjustment(),
        scroll_guard: Rc::new(Cell::new(false)),
        num_tag,
        cur_tag,
        shadow_tag,
        tags,
        lang: Cell::new(Lang::Plain),
        save_btn,
        reload_btn,
        undo_btn,
        redo_btn,
        find_btn,
        replace_btn,
        wrap_btn,
        copy_btn,
        paste_btn,
        goto_btn,
        zoom_in_btn,
        zoom_out_btn,
        zoom_idx: Cell::new(ZOOM_DEFAULT),
        title_lbl,
        sub_lbl,
        notice,
        status,
        pos_lbl,
        status_note: Cell::new(None),
        matches: RefCell::new(Vec::new()),
        match_idx: Cell::new(-1),
        case_sensitive: Cell::new(true),
        title_in_toolbar: Cell::new(has_sub),
        dirty: Cell::new(false),
        loading: Cell::new(true),
        saving: Cell::new(false),
        close_after_save: Cell::new(false),
        valid_utf8: Cell::new(dec.valid_utf8),
        bom: Cell::new(dec.bom),
        crlf: Cell::new(dec.crlf),
        size: Cell::new(dec.bytes),
        refresh_pending: Cell::new(false),
        hl_pending: Cell::new(false),
        gutter_digits: Cell::new(1),
        cur_line: Cell::new(0),
        gutter_char_w: Cell::new(0),
        hl_on: Cell::new(false),
        hl_warned: Cell::new(false),
    });

    // 先塞文本，再把 loading 放下，避免 set_text 触发 dirty
    e.buffer.set_text(&dec.text);

    e.loading.set(false);
    // 语言判定 + 行号 + 高亮
    e.lang.set(Lang::detect(path, first_line(&dec.text)));
    rebuild_gutter(&e);
    apply_highlight(&e);
    update_cursor(&e);
    update_ui(&e);
    wire(&e, st);
    sync_scroll(&e.gutter_adj, &e.main_adj, &e.scroll_guard);
    Rc::clone(&e)
}

/// 等宽字体里一个字符占多宽（像素）。
///
/// 行号栏要定宽：GtkTextView 的自然宽度**不含文本宽度**（实测只有左右
/// 边距那点），不定宽的话行号会被裁掉。宽度按"几个 0 排在一起"来量，
/// 用的是控件自己的 pango context，字体跟正文一致。
fn monospace_text_width(view: &gtk::TextView, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    let ctx = view.pango_context();
    let layout = gtk::pango::Layout::new(&ctx);
    let mut fd = gtk::pango::FontDescription::from_string("monospace");
    fd.set_family("monospace");
    layout.set_font_description(Some(&fd));
    layout.set_width(-1);
    layout.set_text(&"0".repeat(n));
    match layout.line(0) {
        Some(line) => {
            let (_, logical) = line.pixel_extents();
            logical.width()
        }
        None => n as i32 * 9,
    }
}

/// 文本第一行（用来判断 shebang）。
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

fn wire(e: &Rc<FileEdit>, st: &Rc<PanelState>) {
    let ew = Rc::downgrade(e);
    let stw = Rc::downgrade(st);

    // 内容变化 → dirty / 标题与状态重画
    {
        let ew = ew.clone();
        e.buffer.connect_changed(move |_| {
            let Some(e) = ew.upgrade() else { return };
            if e.loading.get() {
                return;
            }
            if !e.dirty.get() {
                e.dirty.set(true);
                update_ui(&e);
            }
            // 撤销/重做按钮的可用性跟着内容走（阶段 84）
            update_undo_buttons(&e);
            // 行号与高亮跟着内容走
            schedule_refresh(&e);
        });
    }
    // 光标移动 → 状态栏位置 + 当前行号高亮
    {
        let ew = ew.clone();
        e.buffer.connect_mark_set(move |_, _, _| {
            let Some(e) = ew.upgrade() else { return };
            update_cursor(&e);
        });
    }
    {
        let ew = ew.clone();
        let stw = stw.clone();
        e.save_btn.connect_clicked(move |_| {
            if let (Some(e), Some(st)) = (ew.upgrade(), stw.upgrade()) {
                save(&e, &st);
            }
        });
    }
    {
        let ew = ew.clone();
        let stw = stw.clone();
        e.reload_btn.connect_clicked(move |_| {
            if let (Some(e), Some(st)) = (ew.upgrade(), stw.upgrade()) {
                reload(&e, &st);
            }
        });
    }

    /* ---- 工具栏（阶段 84）---- */
    {
        let ew = ew.clone();
        e.undo_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            e.buffer.undo();
            update_cursor(&e);
        });
    }
    {
        let ew = ew.clone();
        e.redo_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            e.buffer.redo();
            update_cursor(&e);
        });
    }
    {
        let ew = ew.clone();
        e.find_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            open_find(&e, false);
        });
    }
    {
        let ew = ew.clone();
        e.replace_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            open_find(&e, true);
        });
    }
    {
        let ew = ew.clone();
        e.wrap_btn.connect_toggled(move |b| {
            let Some(e) = ew.upgrade() else { return };
            e.view.set_wrap_mode(if b.is_active() {
                gtk::WrapMode::Word
            } else {
                gtk::WrapMode::None
            });
        });
    }
    {
        let ew = ew.clone();
        e.copy_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            copy_selection(&e);
        });
    }
    {
        let ew = ew.clone();
        e.paste_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            paste_clip(&e);
        });
    }
    {
        let ew = ew.clone();
        e.goto_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            goto_line(&e);
        });
    }
    {
        let ew = ew.clone();
        e.zoom_in_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            zoom_by(&e, 1);
        });
    }
    {
        let ew = ew.clone();
        e.zoom_out_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            zoom_by(&e, -1);
        });
    }
    update_undo_buttons(e);

    /* ---- 查找 / 替换条（阶段 84）---- */
    {
        let ew = ew.clone();
        e.findbar.find_entry.connect_changed(move |_| {
            let Some(e) = ew.upgrade() else { return };
            run_search(&e);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.next_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            goto_match(&e, true);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.prev_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            goto_match(&e, false);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.close_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            close_find(&e);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.case_btn.connect_toggled(move |b| {
            let Some(e) = ew.upgrade() else { return };
            e.case_sensitive.set(b.is_active());
            run_search(&e);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.replace_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            replace_current(&e);
        });
    }
    {
        let ew = ew.clone();
        e.findbar.replace_all_btn.connect_clicked(move |_| {
            let Some(e) = ew.upgrade() else { return };
            let n = replace_all(&e);
            e.status_note.set(Some(trf!("已替换 {n} 处", n = n)));
            update_ui(&e);
        });
    }
    // 查找框里的按键约定（Enter 下一个 / Shift+Enter 上一个 / Esc 关闭）
    // ——与终端查找条同一套（见 `findbar::bind_entry_keys`）。
    {
        let ew1 = ew.clone();
        let ew2 = ew.clone();
        let ew3 = ew.clone();
        findbar::bind_entry_keys(
            &e.findbar.find_entry,
            move || {
                if let Some(e) = ew1.upgrade() {
                    goto_match(&e, true);
                }
            },
            move || {
                if let Some(e) = ew2.upgrade() {
                    goto_match(&e, false);
                }
            },
            move || {
                if let Some(e) = ew3.upgrade() {
                    close_find(&e);
                }
            },
        );
    }
    {
        let ew1 = ew.clone();
        let ew2 = ew.clone();
        findbar::bind_entry_keys(
            &e.findbar.replace_entry,
            move || {
                if let Some(e) = ew1.upgrade() {
                    replace_current(&e);
                }
            },
            move || {},
            move || {
                if let Some(e) = ew2.upgrade() {
                    close_find(&e);
                }
            },
        );
    }

    // 编辑器窗口内局部快捷键：
    // Ctrl+S 保存、F5/Ctrl+R 重新载入（编辑器窗口内局部快捷键）
    // Ctrl+F 查找、Ctrl+H 替换、Ctrl+Z 撤销、Ctrl+Shift+Z 重做（阶段 84）
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let ew = ew.clone();
        let stw = stw.clone();
        key.connect_key_pressed(move |_, keyval, _, mods| {
            let (Some(e), Some(st)) = (ew.upgrade(), stw.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let save_key = ctrl && (keyval == gdk::Key::s || keyval == gdk::Key::S);
            let reload_key = keyval == gdk::Key::F5
                || (ctrl && (keyval == gdk::Key::r || keyval == gdk::Key::R));
            // ⚠️ 修饰键要**精确匹配**：Ctrl+F 是"查找"，
            // Ctrl+Shift+F 不是（别把 Shift+F 也当查找，用户是想搜大小写不同的）。
            let find_key = ctrl && !shift && (keyval == gdk::Key::f || keyval == gdk::Key::F);
            let replace_key =
                ctrl && !shift && (keyval == gdk::Key::h || keyval == gdk::Key::H);
            let undo_key = ctrl && !shift && (keyval == gdk::Key::z || keyval == gdk::Key::Z);
            let redo_key =
                ctrl && shift && (keyval == gdk::Key::z || keyval == gdk::Key::Z);
            // 跳转到行（Ctrl+G）
            let goto_key = ctrl && !shift && (keyval == gdk::Key::g || keyval == gdk::Key::G);
            // 字号：Ctrl++ / Ctrl+= 放大，Ctrl+- 缩小
            let zoom_in = ctrl && (keyval == gdk::Key::equal || keyval == gdk::Key::plus);
            let zoom_out = ctrl && (keyval == gdk::Key::minus || keyval == gdk::Key::underscore);
            if save_key {
                save(&e, &st);
                return glib::Propagation::Stop;
            }
            if reload_key {
                reload(&e, &st);
                return glib::Propagation::Stop;
            }
            if find_key {
                open_find(&e, false);
                return glib::Propagation::Stop;
            }
            if replace_key {
                open_find(&e, true);
                return glib::Propagation::Stop;
            }
            if undo_key {
                e.buffer.undo();
                update_cursor(&e);
                return glib::Propagation::Stop;
            }
            if redo_key {
                e.buffer.redo();
                update_cursor(&e);
                return glib::Propagation::Stop;
            }
            if goto_key {
                goto_line(&e);
                return glib::Propagation::Stop;
            }
            if zoom_in {
                zoom_by(&e, 1);
                return glib::Propagation::Stop;
            }
            if zoom_out {
                zoom_by(&e, -1);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    e.page.add_controller(key);

    // 阶段 12：关闭前的检查在**挂到子标签时**挂上去（见 `attach_tab`）——
    // 原来是窗口的 close-request。编辑器不再是独立窗口，没有 close-request 了。
}

fn update_ui(e: &FileEdit) {
    let mode = if e.base_readonly { tr("查看") } else { tr("编辑") };
    let base = path_basename(&e.path);
    let star = if e.dirty.get() { "● " } else { "" };
    let title = format!("{star}{mode} — {base}");
    e.title_lbl.set_text(&title);
    // 子标签上的标题也跟着走（脏文件前面加 ●；**不带「编辑」二字**——
    // 编辑/查看由图标表示，标题只留文件名）
    if let Some(st) = e.st.upgrade() {
        if let Some(sub) = st.sub_tabs() {
            sub.set_title(e.tab_id.get(), &tab_title(e));
        }
    }
    e.sub_lbl.set_text(&format!("{} · {}",
        if e.remote { tr("远程") } else { tr("本地") },
        e.path
    ));
    // 只读（内容不是合法 UTF-8，存不回去）时**不摆保存按钮**：
    // 既然看的东西压根存不回去，摆个置灰的按钮只会让人以为能存。
    e.save_btn.set_visible(!e.readonly.get());
    e.save_btn.set_sensitive(
        !e.readonly.get() && e.valid_utf8.get() && !e.saving.get() && !e.loading.get(),
    );
    e.reload_btn
        .set_sensitive(!e.saving.get() && !e.loading.get());

    let eol = if e.crlf.get() { "CRLF" } else { "LF" };
    let state = if e.loading.get() {
        tr("正在载入…")
    } else if e.saving.get() {
        tr("正在保存…")
    } else if e.readonly.get() {
        // 只读窗口谈不上保存与否，写「已保存」是误导
        tr("只读")
    } else if e.dirty.get() {
        tr("未保存的修改")
    } else {
        tr("已保存")
    };
    let lang = e.lang.get();
    let lang_txt = if lang == Lang::Plain {
        tr("纯文本").to_string()
    } else if e.hl_on.get() {
        trf!("{v0} 高亮", v0 = lang.label())
    } else {
        trf!("{v0}（文件较大，未高亮）", v0 = lang.label())
    };
    e.status.set_text(&format!("{} · {} · UTF-8 · {} · {}",
        human_size(e.size.get()),
        lang_txt,
        eol,
        state
    ));
    // 临时提示（查找替换的结果）：拼在状态栏**后面**，读完就没了
    if let Some(note) = e.status_note.take() {
        let base = e.status.text().to_string();
        e.status.set_text(&format!("{base} · {note}"));
    }

    let mut notes: Vec<&str> = Vec::new();
    if !e.valid_utf8.get() {
        notes.push(tr("文件不是有效 UTF-8：只读预览，非法字节显示为 �，不能保存。"));
    }
    if e.base_readonly && e.valid_utf8.get() {
        notes.push(tr("只读查看：不会写回文件。"));
    }
    e.notice.set_text(&notes.join(" "));
    e.notice.set_visible(!notes.is_empty());
}

/* ---------------- 编辑器工具（阶段 84） ---------------- */

/// 换字号档：正文与行号栏**必须同步**。
fn apply_zoom(view: &gtk::TextView, gutter: &gtk::TextView, idx: usize) {
    let idx = idx.min(ZOOM_CLASSES.len() - 1);
    for w in [view, gutter] {
        for c in ZOOM_CLASSES {
            w.remove_css_class(c);
        }
        w.add_css_class(ZOOM_CLASSES[idx]);
    }
}

/// 设定字号档。
///
/// 字号变了，行号栏的宽度也得重量（一个字符多宽了）——但**不能马上量**：
/// CSS 里的字号这会儿还没生效，量出来的是旧字宽。所以把宽度作废、延后
/// 一拍重建（`schedule_refresh` 走的是定时器，那时 CSS 已经生效）。
fn set_zoom(e: &Rc<FileEdit>, idx: usize) {
    let idx = idx.min(ZOOM_CLASSES.len() - 1);
    e.zoom_idx.set(idx);
    apply_zoom(&e.view, &e.gutter, idx);
    e.gutter_char_w.set(0);
    schedule_refresh(e);
}

/// 放大 / 缩小字号（到头就不动了）。
fn zoom_by(e: &Rc<FileEdit>, delta: i32) {
    let cur = e.zoom_idx.get() as i32;
    let next = (cur + delta).clamp(0, ZOOM_CLASSES.len() as i32 - 1);
    if next as usize != e.zoom_idx.get() {
        set_zoom(e, next as usize);
    }
}

/// 剪贴板（系统剪贴板，不是 X11 那个老 API）。
fn clipboard() -> Option<gdk::Clipboard> {
    gdk::Display::default().map(|d| d.clipboard())
}

/// 复制选中的文字。
fn copy_selection(e: &Rc<FileEdit>) {
    let Some(cb) = clipboard() else { return };
    e.buffer.copy_clipboard(&cb);
}

/// 粘贴（覆盖选区）。
fn paste_clip(e: &Rc<FileEdit>) {
    let Some(cb) = clipboard() else { return };
    e.buffer.paste_clipboard(&cb, None, true);
    update_cursor(e);
}

/// 跳转到指定行（1 起）。
fn goto_line_no(e: &Rc<FileEdit>, line: i32) {
    let total = e.buffer.line_count().max(1);
    let line = line.clamp(1, total);
    let Some(mut it) = e.buffer.iter_at_line_offset(line - 1, 0) else {
        return;
    };
    e.buffer.place_cursor(&it);
    e.view.scroll_to_iter(&mut it, 0.0, true, 0.0, 0.5);
    update_cursor(e);
}

/// 「跳转到行」：弹个输入框问行号。
fn goto_line(e: &Rc<FileEdit>) {
    let (line, _) = cursor_pos(e);
    let def = line.to_string();
    let ew = Rc::downgrade(e);
    dialogs::ask_text(Some(&e.win), tr("跳转到行"), &def, move |res| {
        let Some(txt) = res else { return };
        let Ok(n) = txt.trim().parse::<i32>() else {
            return;
        };
        let Some(e) = ew.upgrade() else { return };
        goto_line_no(&e, n);
    });
}

/* ---------------- 查找 / 替换（阶段 84） ---------------- */
/// 撤销/重做按钮的可用性（buffer 现在能不能撤）。
fn update_undo_buttons(e: &FileEdit) {
    e.undo_btn.set_sensitive(e.buffer.can_undo());
    e.redo_btn.set_sensitive(e.buffer.can_redo());
}

/// 重算匹配并刷新计数（**不动光标**）。
///
/// 查找词每变一个字就跑一次——所以这里只算区间，不选中任何东西
/// （边打字边跳视图会让人晕）。
fn run_search(e: &Rc<FileEdit>) {
    let q = e.findbar.find_entry.text().to_string();
    let text = e
        .buffer
        .text(&e.buffer.start_iter(), &e.buffer.end_iter(), true)
        .to_string();
    let ms = findbar::find_matches(&text, &q, e.case_sensitive.get());
    {
        let mut slot = e.matches.borrow_mut();
        *slot = ms;
    }
    // 内容变了，之前的下标作废
    e.match_idx.set(-1);
    update_match_count(e);
}

/// 刷新「3/12」那个计数标签。
fn update_match_count(e: &FileEdit) {
    let n = e.matches.borrow().len();
    let txt = if n == 0 {
        tr("无匹配").to_string()
    } else {
        let i = e.match_idx.get();
        if i < 0 {
            // 还没选中任何一个：只报"有几处"，别报"1/3"（会让人以为已经选中了）
            trf!("{n} 处", n = n)
        } else {
            format!("{}/{}", i + 1, n)
        }
    };
    e.findbar.count_lbl.set_text(&txt);
}

/// 选中并滚动到第 `idx` 个匹配。
fn select_match(e: &Rc<FileEdit>, idx: usize) {
    // ⚠️ 先把匹配表**拷出来**再借用：下面还要 `borrow_mut` 别的东西，
    // 借用跨着就会 panic（同一个 RefCell 双重借用）。
    let ms = e.matches.borrow().clone();
    let Some(&(s, t)) = ms.get(idx) else {
        return;
    };
    e.match_idx.set(idx as i32);
    let mut start = e.buffer.iter_at_offset(s);
    let end = e.buffer.iter_at_offset(t);
    e.buffer.select_range(&start, &end);
    e.view.scroll_to_iter(&mut start, 0.0, true, 0.0, 0.5);
    update_cursor(e);
    update_match_count(e);
}

/// 下一个 / 上一个匹配（到头回绕）。
fn goto_match(e: &Rc<FileEdit>, forward: bool) {
    let n = e.matches.borrow().len();
    if n == 0 {
        return;
    }
    let cur = e.match_idx.get();
    let next = if cur < 0 {
        // 还没选中过：第一个"下一个"就是第一个匹配
        if forward {
            0
        } else {
            n - 1
        }
    } else if forward {
        (cur as usize + 1) % n
    } else {
        (cur as usize + n - 1) % n
    };
    select_match(e, next);
}

/// 打开查找条（`replace` = true 时同时展开替换行）。
fn open_find(e: &Rc<FileEdit>, replace: bool) {
    // ⚠️ 替换行**跟着这次打开的模式走**：Ctrl+F 只给查找行（用户要的是
    // "找一下"），Ctrl+H 才把替换行也摆出来。
    e.findbar.replace_row.set_visible(replace);
    e.findbar.root.set_visible(true);
    // 预填：当前选中的文字（用户选中一段再按 Ctrl+F，多半就是想找它）
    if e.findbar.find_entry.text().is_empty() {
        if let Some((s, t)) = e.buffer.selection_bounds() {
            let sel = e.buffer.text(&s, &t, false).to_string();
            // 选中一大段（多行）就别拿去当查找词了——填进去一屏字更难用
            if !sel.is_empty() && !sel.contains('\n') {
                e.findbar.find_entry.set_text(&sel);
            }
        }
    }
    e.findbar.find_entry.grab_focus();
    run_search(e);
}

/// 收起查找条，焦点回正文。
fn close_find(e: &Rc<FileEdit>) {
    e.findbar.root.set_visible(false);
    e.view.grab_focus();
}

/// 替换当前这一个匹配。返回有没有真的替换。
fn replace_current(e: &Rc<FileEdit>) -> bool {
    // 还没选中任何匹配：先跳到离光标最近的那一个
    if e.match_idx.get() < 0 {
        let mark = e.buffer.get_insert();
        let it = e.buffer.iter_at_mark(&mark);
        let pos = it.offset();
        select_nearest(e, pos);
    }
    let cur = e.match_idx.get();
    if cur < 0 {
        return false;
    }
    let ms = e.matches.borrow().clone();
    let Some(&(s, t)) = ms.get(cur as usize) else {
        return false;
    };
    let repl = e.findbar.replace_entry.text().to_string();
    let buf = e.buffer.clone();
    // 一删一插要算**一次**撤销（否则用户按一次 Ctrl+Z 只撤掉一半）
    buf.begin_user_action();
    let mut start = buf.iter_at_offset(s);
    let mut end = buf.iter_at_offset(t);
    buf.delete(&mut start, &mut end);
    buf.place_cursor(&buf.iter_at_offset(s));
    buf.insert_at_cursor(&repl);
    buf.end_user_action();
    run_search(e);
    // 替换完接着选中新的那一个（用户可以连点「替换」一路改下去）
    select_nearest(e, s);
    true
}

/// 全部替换。返回替换了几处。
fn replace_all(e: &Rc<FileEdit>) -> usize {
    let ms = e.matches.borrow().clone();
    if ms.is_empty() {
        return 0;
    }
    let repl = e.findbar.replace_entry.text().to_string();
    let buf = e.buffer.clone();
    buf.begin_user_action();
    // ⚠️ **从后往前**替换：前面的偏移才不会因为后面变长而失效
    for &(s, t) in ms.iter().rev() {
        let mut start = buf.iter_at_offset(s);
        let mut end = buf.iter_at_offset(t);
        buf.delete(&mut start, &mut end);
        buf.place_cursor(&buf.iter_at_offset(s));
        buf.insert_at_cursor(&repl);
    }
    buf.end_user_action();
    let n = ms.len();
    run_search(e);
    n
}

/// 选中位置之后最近的那个匹配（没有就取最后一个）。
fn select_nearest(e: &Rc<FileEdit>, pos: i32) {
    let ms = e.matches.borrow().clone();
    if ms.is_empty() {
        return;
    }
    let idx = ms.iter().position(|m| m.0 >= pos).unwrap_or(ms.len() - 1);
    select_match(e, idx);
}

/// **当前活动**的编辑器（子标签里正显示着的那个）。
///
/// 全局 Ctrl+F 是"终端查找"（见 `shortcuts::Id::ToggleSearch`），
/// 但用户此刻可能正停在编辑器的子标签上——那一下该是**编辑器查找**。
/// 这个函数负责把两者分开。
pub fn active_editor(st: &Rc<PanelState>) -> Option<Rc<FileEdit>> {
    let sub = st.sub_tabs()?;
    let id = sub.current_id()?;
    st.file_edits
        .borrow()
        .iter()
        .find(|e| e.tab_id.get() == id)
        .cloned()
}

/// 全局 Ctrl+F 的入口：活动页是编辑器就交给编辑器，返回 true。
pub fn handle_find_key(st: &Rc<PanelState>) -> bool {
    let Some(e) = active_editor(st) else {
        return false;
    };
    open_find(&e, false);
    true
}

/* ---------------- 行号栏 / 语法高亮 ---------------- */

/// 编辑器当前用的语言（状态栏显示的那个）。冒烟/测试用。
pub fn lang_of(e: &FileEdit) -> Lang {
    e.lang.get()
}

/// 编辑器打开的文件路径。冒烟/测试用。
pub fn path_of(e: &FileEdit) -> &str {
    &e.path
}

/// 正文当前的**纵向滚动偏移**（像素）。冒烟/测试用。
///
/// 长文件在底部敲字会不会跳到顶部，就看这个数会不会突然变成 0
/// （用户实测：跳顶后就改不了了）。
pub fn scroll_offset(e: &FileEdit) -> f64 {
    e.main_adj.value()
}

/// 保存按钮当前是否可见。只读（查看）窗口里不该摆保存按钮。
pub fn save_btn_visible(e: &FileEdit) -> bool {
    e.save_btn.is_visible()
}

/* ---- 冒烟 / 测试钩子（阶段 84：工具栏 + 查找/替换） ---- */

/// 工具栏上的按钮（名字 + 真实按钮对象）。
///
/// 冒烟要点**真实按钮**（`emit_clicked`），别直接调内部函数——按钮灰了、没接上
/// 信号，钩子照样"通过"（阶段 42 踩过：e2e 走钩子，从不点真按钮）。
pub fn toolbar_buttons(e: &FileEdit) -> Vec<(&'static str, gtk::Button)> {
    vec![
        (tr("保存"), e.save_btn.clone()),
        (tr("重载"), e.reload_btn.clone()),
        (tr("撤销"), e.undo_btn.clone()),
        (tr("重做"), e.redo_btn.clone()),
        (tr("复制"), e.copy_btn.clone()),
        (tr("粘贴"), e.paste_btn.clone()),
        (tr("查找"), e.find_btn.clone()),
        (tr("替换"), e.replace_btn.clone()),
        (tr("跳转到行"), e.goto_btn.clone()),
        (tr("放大字号"), e.zoom_in_btn.clone()),
        (tr("缩小字号"), e.zoom_out_btn.clone()),
    ]
}

/// 当前字号档（`ZOOM_CLASSES` 的下标）。
pub fn zoom_index(e: &FileEdit) -> usize {
    e.zoom_idx.get()
}

/// 正文当前带的字号类（冒烟用：正文与行号栏必须**同一档**）。
pub fn view_zoom_class(e: &FileEdit) -> Option<String> {
    e.view
        .css_classes()
        .into_iter()
        .find(|c| c.starts_with("pp-editor-zoom-"))
        .map(|c| c.to_string())
}

/// 行号栏当前带的字号类（冒烟用）。
pub fn gutter_zoom_class(e: &FileEdit) -> Option<String> {
    e.gutter
        .css_classes()
        .into_iter()
        .find(|c| c.starts_with("pp-editor-zoom-"))
        .map(|c| c.to_string())
}

/// 跳到第几行（冒烟用；等价于「跳转到行」对话框里填的那个数）。
pub fn jump_to_line(e: &Rc<FileEdit>, line: i32) {
    goto_line_no(e, line);
}

/// 光标现在在第几行。
pub fn cursor_line(e: &Rc<FileEdit>) -> i32 {
    cursor_pos(e).0
}

/// 「自动换行」开关。
pub fn wrap_button(e: &FileEdit) -> gtk::ToggleButton {
    e.wrap_btn.clone()
}

/// 查找条上的按钮（点**真按钮**，别直接调内部函数）。
pub fn findbar_buttons(e: &FileEdit) -> Vec<(&'static str, gtk::Button)> {
    vec![
        (tr("上一个"), e.findbar.prev_btn.clone()),
        (tr("下一个"), e.findbar.next_btn.clone()),
        (tr("替换"), e.findbar.replace_btn.clone()),
        (tr("全部替换"), e.findbar.replace_all_btn.clone()),
        (tr("关闭"), e.findbar.close_btn.clone()),
    ]
}

/// 「区分大小写」开关。
pub fn case_button(e: &FileEdit) -> gtk::ToggleButton {
    e.findbar.case_btn.clone()
}

/// 查找条现在是不是开着。
pub fn find_bar_visible(e: &FileEdit) -> bool {
    e.findbar.root.is_visible()
}

/// 替换行是不是展开着。
pub fn replace_row_visible(e: &FileEdit) -> bool {
    e.findbar.replace_row.is_visible()
}

/// 查找框里的词。
pub fn find_query(e: &FileEdit) -> String {
    e.findbar.find_entry.text().to_string()
}

/// 设查找词（走 Entry 的 changed 信号 ⇒ 真的重算一遍匹配）。
pub fn set_find_query(e: &FileEdit, q: &str) {
    e.findbar.find_entry.set_text(q);
}

/// 设替换词。
pub fn set_replace_text(e: &FileEdit, t: &str) {
    e.findbar.replace_entry.set_text(t);
}

/// 当前匹配个数。
pub fn match_count(e: &FileEdit) -> usize {
    e.matches.borrow().len()
}

/// 计数标签上的文字（「3/12」「无匹配」）。
pub fn count_text(e: &FileEdit) -> String {
    e.findbar.count_lbl.text().to_string()
}

/// 当前选中的文字。
pub fn selected_text(e: &FileEdit) -> String {
    match e.buffer.selection_bounds() {
        Some((s, t)) => e.buffer.text(&s, &t, false).to_string(),
        None => String::new(),
    }
}

/// 正文全文。
pub fn buffer_text(e: &FileEdit) -> String {
    e.buffer
        .text(&e.buffer.start_iter(), &e.buffer.end_iter(), true)
        .to_string()
}

/// 标题是不是在**工具栏**里（子标签模式）还是在窗口标题栏里（独立窗口模式）。
pub fn title_in_toolbar(e: &FileEdit) -> bool {
    e.title_in_toolbar.get()
}

/// 工具栏是不是真的挂在编辑器页面里（不是"建了但没摆上去"）。
pub fn toolbar_in_page(e: &FileEdit) -> bool {
    e.toolbar.parent().is_some()
}

/// 冒烟用：选中一段（**字符**偏移区间）。
pub fn select_chars(e: &FileEdit, s: i32, t: i32) {
    let start = e.buffer.iter_at_offset(s);
    let end = e.buffer.iter_at_offset(t);
    e.buffer.select_range(&start, &end);
}

/// 行号栏的文本（每行一个行号，可能带透明影子字符）。冒烟/测试用。
pub fn gutter_text(e: &FileEdit) -> String {
    let b = &e.gutter_buffer;
    b.text(&b.start_iter(), &b.end_iter(), true).to_string()
}

/// 正文里某个字符位置上的高亮颜色（`#rrggbb`）；没有高亮则 None。
///
/// 冒烟/测试用：「语言识别出来了」并不代表高亮真的上去了——tag 没进
/// GTK 的 tag 表时 `apply_tag` 会被直接拒掉，界面上一点颜色都没有。
/// 只有看**颜色**才测得出这个问题。
pub fn fg_at(e: &FileEdit, offset: i32) -> Option<String> {
    let it = e.buffer.iter_at_offset(offset);
    it.tags()
        .into_iter()
        .find_map(|t| t.foreground_rgba())
        .map(rgba_hex)
}

/// GdkRGBA → `#rrggbb`。
fn rgba_hex(c: gdk::RGBA) -> String {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", f(c.red()), f(c.green()), f(c.blue()))
}

/// 让行号栏的纵向滚动跟着正文走。
///
/// 两边是各自独立的 `ScrolledWindow`（正文横向滚、行号不滚，行号才能
/// 一直留在左边），所以要把正文的滚动值同步给行号栏。行高一致（见
/// `rebuild_gutter`），同一个像素偏移就是同一行。
///
/// ⚠️ **必须是单向的**：行号栏的位置**只由正文决定**，行号栏自己怎么动
///    都**不牵动正文**。
///
///    为什么不能双向：行号栏每敲一个字就要重建（`rebuild_gutter` →
///    `gutter_buffer.set_text`），而 `set_text` 会把行号栏的滚动条
///    **归零**——而且是**异步**的（要等布局那一步才发生，同步的闸门
///    根本挡不住）。双向同步时，这次归零会顺带把**正文拽到文件顶部**
///    ⇒ 长文件在底部敲一个字就跳顶，跳完没法继续改（用户实测）。
///
///    单向就稳：行号栏被归零后，它的回调会把它**拉回正文的位置**，
///    正文一个像素都不动。用户在行号栏上滚滚轮照样能滚正文
///    （见 `build` 里的滚轮转发）。
fn sync_scroll(
    gutter_adj: &gtk::Adjustment,
    main_adj: &gtk::Adjustment,
    guard: &Rc<Cell<bool>>,
) {
    {
        let g = gutter_adj.clone();
        let guard = guard.clone();
        main_adj.connect_value_changed(move |adj| {
            if guard.get() {
                return;
            }
            guard.set(true);
            g.set_value(adj.value());
            guard.set(false);
        });
    }
    {
        let m = main_adj.clone();
        let guard = guard.clone();
        gutter_adj.connect_value_changed(move |adj| {
            if guard.get() {
                return;
            }
            // 行号栏自己动了（重建时被归零 / 用户滚了它）⇒ **拉回正文
            // 的位置**，正文不动（见上面的说明）。
            guard.set(true);
            adj.set_value(m.value());
            guard.set(false);
        });
    }
}

/// 挂 tag，并且**先摘掉冲突的另一个 tag**。
///
/// GTK4 里两个 tag 抢同一个属性时谁赢并不可靠（实测优先级不按文档来，
/// 两种顺序都是先挂的赢），所以行号栏里同一个位置**只留一个 tag**：
/// 影子那段摘掉行号色（影子画成底色才隐形），当前行那几个数字摘掉行号色
/// （换成强调色）。互不重叠就不用去赌 GTK 的裁决规则。
fn tag_exclusive(
    buffer: &gtk::TextBuffer,
    tag: &gtk::TextTag,
    old: &gtk::TextTag,
    start: i32,
    end: i32,
) {
    if end <= start {
        return;
    }
    let s = buffer.iter_at_offset(start);
    let e = buffer.iter_at_offset(end);
    buffer.remove_tag(old, &s, &e);
    buffer.apply_tag(tag, &s, &e);
}

/// 重建行号栏。
///
/// **为什么要塞"影子字符"**：正文里含中文的行，用的字体和纯 ASCII 行
/// 不是一套（字体回退），行高因此不一样（实测 16px vs 19px）。行号栏
/// 是另一个 TextView，它的行高只按自己那点数字算，于是正文里每出现一个
/// 中文行，两边就错开几像素，越往下越歪。
///
/// 解决办法：行号栏的每一行在数字后面接上**该行的非 ASCII 字符**，并把
/// 它们设成透明（`foreground: transparent`）——看不见，但照样参与排版，
/// 行高就和正文完全一致了。行高只取决于**字体**不取决于字数，所以每行
/// 留一两个就够。
///
/// （试过 `GtkTextTag:invisible`——那个字符直接不参与排版，行高对不上；
///  也试过 `line-height` 属性——GTK4 里它对布局没影响。）
fn rebuild_gutter(e: &Rc<FileEdit>) {
    let buf = &e.buffer;
    let text = buf.text(&buf.start_iter(), &buf.end_iter(), true).to_string();
    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len().max(1);
    let digits = n.to_string().len();
    let prev_digits = e.gutter_digits.get();
    e.gutter_digits.set(digits);
    // 行号栏定宽：数字宽度 + 左右边距。影子字符在框外被裁掉，不影响宽度。
    // 位数变了（或还没量过）才重新量字符宽度——字号、主题换了宽度也会变。
    if prev_digits != digits || e.gutter_char_w.get() == 0 {
        let cw = monospace_text_width(&e.gutter, 4).max(4);
        e.gutter_char_w.set((cw + 3) / 4);
    }
    let cw = e.gutter_char_w.get();
    if cw > 0 {
        e.gutter_scroll.set_size_request(cw * digits as i32 + 12, -1);
    }

    let mut gtext = String::with_capacity(n * (digits + 3));
    let mut shadow_spans: Vec<(i32, i32)> = Vec::new();
    let mut pos: i32 = 0; // 字符位置
    for (i, line) in lines.iter().enumerate() {
        // 行号：定宽右对齐（补空格，数字本身在等宽字体里等宽）
        let num = format!("{:>width$}", i + 1, width = digits);
        gtext.push_str(&num);
        pos += num.chars().count() as i32;

        // 影子：这一行里的非 ASCII 字符（决定行高的就是它们）
        let mut shadow = String::new();
        for c in line.chars() {
            if c.is_ascii() {
                continue;
            }
            if shadow.contains(c) {
                continue;
            }
            shadow.push(c);
            if shadow.chars().count() >= SHADOW_CHARS {
                break;
            }
        }
        if !shadow.is_empty() {
            let start = pos;
            gtext.push_str(&shadow);
            pos += shadow.chars().count() as i32;
            shadow_spans.push((start, pos));
        }
        if i + 1 < lines.len() {
            gtext.push('\n');
            pos += 1;
        }
    }

    // ⚠️ **重建期间把同步闸门关上**：`set_text` 会把行号栏的滚动条归零，
    //    而行号栏 ↔ 正文是**双向**同步的 ⇒ 归零会顺带把**正文拽到顶部**
    //    ⇒ 长文件在底部敲一个字，视图就跳到文件头（用户实测：跳完没法
    //    继续改）。闸门关着，行号栏这一轮就不牵动正文；下面再把行号栏
    //    自己的位置还回去。
    e.scroll_guard.set(true);
    e.gutter_buffer.set_text(&gtext);
    e.scroll_guard.set(false);
    let gstart = e.gutter_buffer.start_iter();
    let gend = e.gutter_buffer.end_iter();
    // 行号本体：普通色
    e.gutter_buffer.apply_tag(&e.num_tag, &gstart, &gend);
    // 影子：画成底色（隐形）。这里**先把行号色摘掉再挂影子**——两个 tag
    // 不重叠，谁的颜色一目了然，也不用去赌 GTK 的优先级规则。
    for (s, en) in shadow_spans {
        tag_exclusive(&e.gutter_buffer, &e.shadow_tag, &e.num_tag, s, en);
    }
    mark_current_line(e);
    // set_text 会把滚动条归零，正文却还停在原处——还回去
    e.gutter_adj.set_value(e.main_adj.value());
}

/// 把光标所在行的行号换成强调色。
///
/// 同样**不靠优先级**：当前行那几个数字上只留 `cur_tag` 一个（行号色先摘
/// 掉），光标移走时再把行号色还回去。
fn mark_current_line(e: &Rc<FileEdit>) {
    let digits = e.gutter_digits.get() as i32;
    // 上一行被标成当前行的，先恢复成普通行号色
    let prev = e.cur_line.get();
    if prev >= 1 {
        if let (Some(s), Some(t)) = (
            e.gutter_buffer.iter_at_line_offset(prev - 1, 0),
            e.gutter_buffer.iter_at_line_offset(prev - 1, digits),
        ) {
            tag_exclusive(&e.gutter_buffer, &e.num_tag, &e.cur_tag, s.offset(), t.offset());
        }
        e.cur_line.set(0);
    }
    let (line, _) = cursor_pos(e);
    if line < 1 {
        return;
    }
    let Some(s) = e.gutter_buffer.iter_at_line_offset(line - 1, 0) else {
        return;
    };
    let Some(t) = e.gutter_buffer.iter_at_line_offset(line - 1, digits) else {
        return;
    };
    tag_exclusive(&e.gutter_buffer, &e.cur_tag, &e.num_tag, s.offset(), t.offset());
    e.cur_line.set(line);
}

/// 光标位置（1 起的行、列）。
fn cursor_pos(e: &Rc<FileEdit>) -> (i32, i32) {
    let mark = e.buffer.get_insert();
    let it = e.buffer.iter_at_mark(&mark);
    (it.line() + 1, it.line_offset() + 1)
}

/// 光标移动：更新状态栏位置 + 行号高亮。
fn update_cursor(e: &Rc<FileEdit>) {
    let (line, col) = cursor_pos(e);
    let total = e.buffer.line_count().max(1);
    e.pos_lbl.set_text(&trf!("行 {line}/{total} · 列 {col}", line = line, total = total, col = col));
    mark_current_line(e);
}

/// 建一个语法高亮用的 tag。
///
/// **名字必须一类一个**：GTK 的 tag 表按名字索引，同名的 tag 只有第一个
/// 进得去，后面的会被 GTK 拒收（`gtk_text_tag_table_add` 返回 false），
/// 而 `gtk_text_buffer_apply_tag` 要求 tag 属于本 buffer 的表——被拒的
/// tag 压根上不去，界面就一点颜色都没有（同名 tag 的坑踩过一次）。
/// 高亮 tag（规则与数据库 SQL 编辑器共用一份，见 `syntax::make_tok_tag`）。
fn make_tok_tag(tok: Tok, colors: &crate::theme::SynColors) -> gtk::TextTag {
    syntax::make_tok_tag(tok, colors)
}

/// 按语言重新上色。语言是纯文本时什么都不做。
fn apply_highlight(e: &Rc<FileEdit>) {
    let start = e.buffer.start_iter();
    let end = e.buffer.end_iter();
    for tag in &e.tags {
        e.buffer.remove_tag(tag, &start, &end);
    }
    if e.lang.get() == Lang::Plain {
        e.hl_on.set(false);
        return;
    }
    let text = e.buffer.text(&start, &end, true).to_string();
    if text.len() > HIGHLIGHT_MAX_BYTES {
        // 太大了就不高亮，免得打开文件就卡住
        e.hl_on.set(false);
        return;
    }
    e.hl_on.set(true);
    let hl = syntax::highlight(&text, e.lang.get());
    if hl.stalls > 0 {
        // 扫描器有 bug（某轮忘了推进游标，被 syntax 里的进度守卫挡下），
        // 这次的高亮**不可信**：宁可整个不高亮，也不能把半吊子的着色贴上去。
        // 守卫保证这里最多是"颜色不对"，不会是"程序冻住"。
        if !e.hl_warned.get() {
            e.hl_warned.set(true);
            eprintln!("{}", trf!("[高亮] {v0} 扫描异常（{v1} 处原地打转），本次不高亮", v0 = e.path, v1 = hl.stalls));
        }
        e.hl_on.set(false);
        return;
    }
    for s in &hl.spans {
        if s.end <= s.start {
            continue;
        }
        let Some(tag) = e.tags.get(syntax::tok_index(s.tok)) else {
            continue;
        };
        let si = e.buffer.iter_at_offset(s.start as i32);
        let ei = e.buffer.iter_at_offset(s.end as i32);
        e.buffer.apply_tag(tag, &si, &ei);
    }
}

/// 内容变了：行号立刻跟上，高亮稍微防抖一下（每敲一个字都重排一次太费）。
fn schedule_refresh(e: &Rc<FileEdit>) {
    // 大文件重建一次要几十毫秒，敲字时更频，别把界面拖住
    let big = e.buffer.char_count() > 200_000;
    if !e.refresh_pending.get() {
        e.refresh_pending.set(true);
        let ew = Rc::downgrade(e);
        let delay = if big { 400 } else { 80 };
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let Some(e) = ew.upgrade() else { return };
            e.refresh_pending.set(false);
            rebuild_gutter(&e);
            update_cursor(&e);
        });
    }
    if !e.hl_pending.get() {
        e.hl_pending.set(true);
        let ew = Rc::downgrade(e);
        let delay = if big { 800 } else { 250 };
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let Some(e) = ew.upgrade() else { return };
            e.hl_pending.set(false);
            apply_highlight(&e);
        });
    }
}

/// 主题切换后重画高亮颜色（tag 的颜色是写死的，只能重新设一遍）。
pub fn refresh_theme_colors(st: &Rc<PanelState>) {
    let colors = crate::theme::syn_colors();
    for e in st.file_edits.borrow().iter() {
        for (i, tag) in e.tags.iter().enumerate() {
            let tok = syntax::ALL_TOKS[i];
            // Plain 不着色（跟建 tag 时保持一致）
            if tok != Tok::Plain {
                tag.set_foreground(Some(colors.of(tok)));
            }
        }
        e.num_tag.set_foreground(Some(colors.comment));
        e.cur_tag.set_foreground(Some(colors.func));
        // 影子字符要跟着底色走，否则换主题后会露出底色不对的鬼影
        e.shadow_tag.set_foreground(Some(&crate::theme::panel_hex()));
        apply_highlight(e);
    }
}

/* ---------------- 保存 / 重新载入 ---------------- */

fn save(e: &Rc<FileEdit>, st: &Rc<PanelState>) -> bool {
    if e.readonly.get() || !e.valid_utf8.get() || e.saving.get() || e.loading.get() {
        return false;
    }
    let buf = e.buffer.clone();
    let text = buf
        .text(&buf.start_iter(), &buf.end_iter(), true)
        .to_string();
    let dec = Decoded {
        text: String::new(), // encode 只用 bom/crlf
        bom: e.bom.get(),
        crlf: e.crlf.get(),
        valid_utf8: true,
        bytes: 0,
    };
    let data = dec.encode(&text);
    if data.len() as u64 > MAX_BYTES {
        dialogs::show_error(
            Some(&e.win),
            tr("无法保存"),
            &trf!("编辑后的内容为 {v0}，超过内置查看/编辑上限 {v1}。\n请删掉部分内容，或复制到外部编辑器处理。", v0 = human_size(data.len() as u64), v1 = human_size(MAX_BYTES)),
        );
        return false;
    }
    e.saving.set(true);
    update_ui(e);

    if e.remote {
        // 写回期间不许继续改：回包成功时会把 dirty 清掉，晚到的按键
        // 若没写进去就会变成"看起来保存了、其实没保存"
        e.view.set_editable(false);
        e.view.set_cursor_visible(false);
        e.size.set(data.len() as u64);
        let ok = send(
            st,
            WorkerCmd::WriteFile {
                id: e.id,
                path: e.path.clone(),
                data,
            },
        );
        if !ok {
            e.saving.set(false);
            e.close_after_save.set(false);
            e.view.set_editable(!e.readonly.get());
            e.view.set_cursor_visible(!e.readonly.get());
            update_ui(e);
            dialogs::show_error(
                Some(&e.win),
                tr("保存失败"),
                tr("SFTP 连接已断开，修改没有写回远程。\n可以先把内容复制出来，重连后再粘贴保存。"),
            );
            super::closeguard::note_save_failed();
            return false;
        }
        return true;
    }

    match write_local(&e.path, &data) {
        Ok(()) => {
            e.saving.set(false);
            e.dirty.set(false);
            e.size.set(data.len() as u64);
            update_ui(e);
            super::refresh_after_edit_save(st, false);
            if e.close_after_save.get() {
                close_tab(st, &e);
                // ⚠️ 本地保存是**同步**完成的：报告必须延后一拍，
                // 否则会插进"批量保存"的循环中间（面板先被关掉，
                // 循环还在往已经关掉的编辑器上写）。
                super::closeguard::note_save_done_deferred();
            }
        }
        Err(err) => {
            e.saving.set(false);
            e.close_after_save.set(false);
            update_ui(e);
            dialogs::show_error(Some(&e.win), tr("保存失败"), &err);
            super::closeguard::note_save_failed();
        }
    }
    true
}

fn reload(e: &Rc<FileEdit>, st: &Rc<PanelState>) {
    if e.saving.get() || e.loading.get() {
        return;
    }
    if e.dirty.get() {
        let e2 = Rc::clone(e);
        let st2 = Rc::clone(st);
        dialogs::confirm_with(
            Some(&e.win),
            tr("放弃未保存的修改并重新载入？"),
            tr("放弃并重载"),
            move |ok| {
                if ok {
                    do_reload(&e2, &st2);
                }
            },
        );
    } else {
        do_reload(e, st);
    }
}

fn do_reload(e: &Rc<FileEdit>, st: &Rc<PanelState>) {
    let readonly = e.base_readonly;
    if e.remote {
        if !st.conn_ok.get() {
            dialogs::show_error(
                Some(&e.win),
                tr("无法重新载入"),
                tr("SFTP 尚未连接，无法读取远程文件。"),
            );
            return;
        }
        e.loading.set(true);
        // 回包会整体替换缓冲区：读取期间不许再输入，否则用户刚敲的字
        // 会被静默覆盖
        e.view.set_editable(false);
        e.view.set_cursor_visible(false);
        st.pending_file_reads.borrow_mut().insert(e.id, readonly);
        update_ui(e);
        if !send(
            st,
            WorkerCmd::ReadFile {
                id: e.id,
                path: e.path.clone(),
                max: MAX_BYTES,
            },
        ) {
            st.pending_file_reads.borrow_mut().remove(&e.id);
            e.loading.set(false);
            e.view.set_editable(!e.readonly.get());
            e.view.set_cursor_visible(!e.readonly.get());
            update_ui(e);
            dialogs::show_error(Some(&e.win), tr("无法重新载入"), tr("SFTP 连接已断开。"));
        }
        return;
    }
    match read_local(&e.path) {
        Ok(data) => {
            if let Err(msg) = apply_bytes(st, e.id, false, &e.path, readonly, data) {
                dialogs::show_error(Some(&e.win), tr("无法重新载入"), &msg);
            }
        }
        Err(msg) => dialogs::show_error(Some(&e.win), tr("无法重新载入"), &msg),
    }
}

/* ---------------- 未保存关闭确认 ---------------- */

fn confirm_close(e: &Rc<FileEdit>, st: &Rc<PanelState>) {
    const DISCARD: gtk::ResponseType = gtk::ResponseType::Other(1);
    let dlg = gtk::Dialog::with_buttons(
        Some(tr("未保存的修改")),
        Some(&e.win),
        gtk::DialogFlags::MODAL,
        &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("不保存关闭"), DISCARD),
            (tr("保存并关闭"), gtk::ResponseType::Ok),
        ],
    );
    let lbl = gtk::Label::new(Some(&trf!("「{v0}」有未保存的修改。\n\n保存到{v1}后再关闭吗？", v0 = path_basename(&e.path), v1 = if e.remote { tr("远程") } else { tr("本地") })));
    lbl.set_wrap(true);
    lbl.set_xalign(0.0);
    lbl.set_max_width_chars(48);
    let ca = dlg.content_area();
    ca.set_margin_top(12);
    ca.set_margin_bottom(12);
    ca.set_margin_start(12);
    ca.set_margin_end(12);
    ca.append(&lbl);
    dlg.set_default_response(gtk::ResponseType::Ok);
    dialogs::style_dialog(&dlg);
    if let Some(b) = dlg
        .widget_for_response(DISCARD)
        .and_downcast::<gtk::Button>()
    {
        b.remove_css_class("suggested-action");
        b.add_css_class("destructive-action");
    }
    dlg.present();
    let e2 = Rc::clone(e);
    let st2 = Rc::clone(st);
    dlg.connect_response(move |dlg, resp| {
        dlg.destroy();
        if resp == gtk::ResponseType::Ok {
            e2.close_after_save.set(true);
            save(&e2, &st2);
        } else if resp == DISCARD {
            close_tab(&st2, &e2);
        }
    });
}

/* ---------------- 本地文件读写 ---------------- */

fn read_local(path: &str) -> Result<Vec<u8>, String> {
    let md = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if md.is_dir() {
        return Err(trf!("{path} 是目录，无法用文本查看/编辑器打开", path = path));
    }
    if !md.is_file() {
        return Err(trf!("{path} 不是普通文件，内置查看/编辑器只处理文本文件", path = path));
    }
    if md.len() > MAX_BYTES {
        return Err(too_large_msg(path, md.len()));
    }
    let data = std::fs::read(path).map_err(|e| trf!("读取 {path} 失败: {e}", path = path, e = e))?;
    if data.len() as u64 > MAX_BYTES {
        return Err(too_large_msg(path, data.len() as u64));
    }
    Ok(data)
}

/// 本地保存同样走临时文件 + rename；权限位从原文件抄过去。
/// 先 canonicalize：如果用户打开的是符号链接，写它的目标而不是把链接
/// 替换成一个普通文件。
fn write_local(path: &str, data: &[u8]) -> Result<(), String> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let dir = target
        .parent()
        .ok_or_else(|| trf!("无法确定 {v0} 所在目录", v0 = target.display()))?;
    if let Ok(md) = std::fs::metadata(&target) {
        if !md.is_file() {
            return Err(trf!("{v0} 不是普通文件，无法写入文本内容", v0 = target.display()));
        }
    }
    let tmp = temp_path(dir, "puttyplus-edit");
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let res = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, data)?;
        if let Some(p) = perms {
            std::fs::set_permissions(&tmp, p)?;
        }
        // Unix 上 rename 覆盖已存在的目标，是原子替换
        std::fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if let Err(err) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(trf!("写入 {v0} 失败: {err}", err = err, v0 = target.display()));
    }
    Ok(())
}

fn temp_path(dir: &Path, tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(".{tag}-{}-{n}.tmp", std::process::id()))
}

fn too_large_msg(path: &str, size: u64) -> String {
    trf!("{path}\n\n文件大小 {v0}，超过内置查看/编辑上限 {v1}。\n请先下载到本地，再用外部编辑器打开。", path = path, v0 = human_size(size), v1 = human_size(MAX_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection() {
        assert!(!looks_binary(b"hello\nworld\n"));
        assert!(!looks_binary("中文文本\n第二行\n".as_bytes()));
        assert!(looks_binary(b"abc\0def"));
        assert!(looks_binary(&[0xFF, 0xFE, 0x00, 0x41]));
        // 少量控制字符（终端颜色转义）不算二进制
        assert!(!looks_binary(b"\x1b[31mred\x1b[0m\n"));
    }

    #[test]
    fn decode_utf8_bom_crlf() {
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"a\r\nb\r\n");
        let d = Decoded::decode(&data);
        assert!(d.bom);
        assert!(d.crlf);
        assert!(d.valid_utf8);
        assert_eq!(d.text, "a\nb\n");
        assert_eq!(d.bytes, data.len() as u64);
        // 编码回去必须逐字节还原
        assert_eq!(d.encode(&d.text), data);
    }

    #[test]
    fn decode_lf_and_invalid() {
        let d = Decoded::decode(b"a\nb\n");
        assert!(!d.bom);
        assert!(!d.crlf);
        assert!(d.valid_utf8);
        assert_eq!(d.text, "a\nb\n");

        let d = Decoded::decode(&[0x61, 0xFF, 0x62]);
        assert!(!d.valid_utf8);
        assert_eq!(d.text, "a�b");
        assert_eq!(d.encode(&d.text), [0x61, 0xEF, 0xBF, 0xBD, 0x62]);
    }

    #[test]
    fn mixed_eol_stays_lf() {
        // 混合换行不整体改 CRLF：保留原样，新增行用 LF
        let d = Decoded::decode(b"a\r\nb\n");
        assert!(!d.crlf);
        assert_eq!(d.encode("x\ny\n"), b"x\ny\n");
    }

    #[test]
    fn local_save_is_atomic_and_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("pp_fileedit_local_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_local(path.to_str().unwrap(), b"new\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "保存后权限位没有保留");
        let temp_left = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("puttyplus-edit"));
        assert!(!temp_left, "保存后留下了临时文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 语法 tag 的名字必须互不相同。
    ///
    /// GTK 的 tag 表按名字索引：同名的 tag 只有第一个进得去，后面的被
    /// GTK 拒收；`gtk_text_buffer_apply_tag` 又要求 tag 属于本 buffer
    /// 的表，于是被拒的那些**一个都上不去**——整屏不着色（踩过一次）。
    #[test]
    fn 语法tag名字必须唯一() {
        let mut seen = std::collections::HashSet::new();
        for t in syntax::ALL_TOKS {
            assert!(
                seen.insert(t.slug()),
                "tag 名重复：{:?} → {}",
                t,
                t.slug()
            );
        }
    }

    /// 影子字符必须是**底色**，不能靠 alpha 透明。
    ///
    /// GTK4 不吃前景色的 alpha：实测单独给一段字挂 `foreground_rgba`
    /// (alpha=0)，字照样画出来（字形是黑的）。以前影子"隐形"其实是靠
    /// 行号栏窄宽度把它裁掉了，滚动条一撤就露馅——用户截图里行号后面
    /// 隐约浮着几个汉字。改成画成底色才真的隐形。
    #[test]
    fn 影子字符画成底色而不是透明() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
            eprintln!("跳过 影子字符画成底色而不是透明：本机没有显示服务");
            return;
        }
        let tag = gtk::TextTag::new(Some("pp-shadow"));
        tag.set_foreground(Some(&crate::theme::panel_hex()));
        let c = tag
            .foreground_rgba()
            .expect("影子 tag 没有前景色");
        assert!(
            c.alpha() > 0.99,
            "影子用了半透明（alpha={}）——GTK4 不吃前景色的 alpha，字会露出来",
            c.alpha()
        );
        let want = crate::theme::panel_hex();
        let got = format!("#{:02x}{:02x}{:02x}",
            (c.red() * 255.0).round() as u8,
            (c.green() * 255.0).round() as u8,
            (c.blue() * 255.0).round() as u8
        );
        assert_eq!(got.to_lowercase(), want.to_lowercase(), "影子颜色跟底色不一致");
    }

    /// 同一段文字上不能有两个抢前景色的 tag。
    ///
    /// GTK4 里两个 tag 抢同一属性时谁赢不可靠（实测优先级不按文档来），
    /// 所以行号栏改成"挂新的先摘旧的"，同一位置永远只有一个 tag。
    #[test]
    fn 行号栏同一段只留一个tag() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
            eprintln!("跳过 行号栏同一段只留一个tag：本机没有显示服务");
            return;
        }
        let buffer = gtk::TextBuffer::new(None);
        let num = gtk::TextTag::new(Some("n"));
        let shadow = gtk::TextTag::new(Some("s"));
        buffer.tag_table().add(&num);
        buffer.tag_table().add(&shadow);
        buffer.set_text("12影子");
        let s = buffer.start_iter();
        let e = buffer.end_iter();
        buffer.apply_tag(&num, &s, &e);
        // 影子那段（字符 2..4）换成影子色，行号色必须被摘掉
        tag_exclusive(&buffer, &shadow, &num, 2, 4);
        let it = buffer.iter_at_offset(2);
        assert_eq!(
            it.tags().len(),
            1,
            "影子那段还挂着别的 tag（GTK4 抢属性时谁赢不可靠）"
        );
        // 行号那段仍然只有行号色
        let it = buffer.iter_at_offset(0);
        assert_eq!(it.tags().len(), 1, "行号那段 tag 重叠了");
    }

    /// 真上一下 GTK：tag 进了表、`apply_tag` 能生效、颜色读得回来。
    ///
    /// 这是「高亮看起来没生效」那类问题的唯一可靠防线——只看"语言识别
    /// 出来了"是发现不了 tag 被 GTK 拒掉的。没有显示服务就跳过。
    #[test]
    fn 高亮tag真的能贴到缓冲区上() {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        if !gtk::is_initialized_main_thread() || gdk::Display::default().is_none() {
            eprintln!("跳过 高亮tag真的能贴到缓冲区上：本机没有显示服务");
            return;
        }
        let colors = crate::theme::syn_colors();
        let buffer = gtk::TextBuffer::new(None);
        let tags: Vec<gtk::TextTag> = syntax::ALL_TOKS
            .iter()
            .map(|t| {
                let tag = make_tok_tag(*t, &colors);
                assert!(
                    buffer.tag_table().add(&tag),
                    "tag {:?} 没进 tag 表（名字重复？）",
                    t.slug()
                );
                tag
            })
            .collect();
        // 全部 17 个 tag 都该在表里（同名的话只有第一个进得去）
        assert_eq!(
            buffer.tag_table().size(),
            syntax::ALL_TOKS.len() as i32,
            "tag 表里少了 tag：同名会被 GTK 拒收"
        );

        buffer.set_text("import socket");
        let hl = syntax::highlight("import socket", syntax::Lang::Python);
        assert!(!hl.spans.is_empty(), "Python 没分出任何词法单元");
        for s in &hl.spans {
            let tag = &tags[syntax::tok_index(s.tok)];
            let si = buffer.iter_at_offset(s.start as i32);
            let ei = buffer.iter_at_offset(s.end as i32);
            buffer.apply_tag(tag, &si, &ei);
        }
        // "import" 是关键字：颜色应该读得回来
        let it = buffer.iter_at_offset(0);
        let colors_at: Vec<Option<String>> = it
            .tags()
            .into_iter()
            .map(|t| t.foreground_rgba().map(rgba_hex))
            .collect();
        assert!(
            colors_at.iter().any(|c| c.is_some()),
            "关键字没上色：{:?}",
            colors_at
        );
    }
}
