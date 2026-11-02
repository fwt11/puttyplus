//! 端口转发的**结构化表**（连接编辑器的「转发」页，阶段 71，审计 P1-3）。
//!
//! 以前连接编辑器里是两个原始文本域（终端转发 / 进程内端口映射），一行一条
//! 规则，格式是 ssh 那套 `L:[addr:]port:host:port` ⇒ 用户得自己拼字符串、
//! 分不清哪条是哪条、也不知道哪条走哪套机制（两套机制摆在一起，长得
//! 一模一样）。
//!
//! 现在是一张表：`类型 | 监听 | 目标 | 机制 | 启用`。
//!
//! ⚠️ **存储格式一点没变**：`forwardings`（交给 spawn 的 ssh -L/-R/-D）与
//! `inprocess_forwardings`（跑在应用自己那条 SSH 连接上）两把键照旧，
//! 只换 UI ⇒ PuTTY 互转、CLI 与兼容基线都不受影响。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::backend::forward::{self, FwdRule};
use crate::i18n::tr;

/// 一条结构化的转发规则。
#[derive(Debug, Clone, PartialEq)]
pub struct FwdRow {
    /// 'L' 本地 / 'R' 远程 / 'D' 动态（SOCKS）
    pub kind: char,
    /// 监听：`port` 或 `addr:port`
    pub listen: String,
    /// 目标：`host:port`（动态/SOCKS 没有目标，空）
    pub target: String,
    /// 走哪套机制：true = 进程内（应用自己那条 SSH 连接），
    /// false = 终端（spawn 出去的 ssh -L/-R/-D）
    pub in_process: bool,
    /// 是否启用（false = 停用，存储里带 `!` 前缀）
    pub enabled: bool,
}

impl FwdRow {
    pub fn new(kind: char, listen: &str, target: &str, in_process: bool) -> Self {
        Self {
            kind,
            listen: listen.trim().to_string(),
            target: target.trim().to_string(),
            in_process,
            enabled: true,
        }
    }

    /// 类型中文名（表里那一列）。
    pub fn kind_name(&self) -> &'static str {
        kind_to_display(self.kind)
    }

    /// 机制中文名（表里那一列）。
    pub fn mech_name(&self) -> &'static str {
        mech_to_display(self.in_process)
    }

    /// 这一行的存储规格体（不含 kind 前缀）。
    ///
    /// ⚠️ 形状必须与 `forward::parse_local` / `parse_socks` 认的一样：
    /// L/R 是 `[addr:]port:host:port`，D 是 `[addr:]port`。
    pub fn spec(&self) -> String {
        match self.kind {
            'D' => self.listen.clone(),
            _ => {
                if self.target.is_empty() {
                    self.listen.clone()
                } else {
                    format!("{}:{}", self.listen, self.target)
                }
            }
        }
    }

    /// 这一行能不能落成一条合法规则（表里填了一半的行要拦住）。
    pub fn is_valid(&self) -> bool {
        let rule = FwdRule::new(self.kind, &self.spec(), self.enabled);
        match self.kind {
            'L' => rule.to_local().is_ok(),
            'R' => rule.to_remote().is_ok(),
            'D' => rule.to_socks().is_ok(),
            _ => false,
        }
    }
}

/// 类型：内部记号 ⇄ 表格里显示的字。
// i18n:keep
// ⚠️ 这两个字**也是写进 fwdtable.ini 的格式**（第 3 列就是它）⇒
// 翻译了就跟老配置对不上（读回来会认成"本地"）⇒ 保持中文。
fn kind_to_display(k: char) -> &'static str {
    match k {
        'L' => "本地",
        'R' => "远程",
        'D' => "动态",
        _ => "未知",
    }
}

fn kind_from_display(s: &str) -> char {
    match s.trim() {
        "远程" => 'R',
        "动态" => 'D',
        "本地" => 'L',
        other => other.chars().next().unwrap_or('L'),
    }
}

/// 机制：表格里显示的字。
fn mech_to_display(in_process: bool) -> &'static str {
    if in_process {
        "进程内"
    } else {
        "终端"
    }
}

fn mech_from_display(s: &str) -> bool {
    s.trim() == "进程内"
}
// i18n:end

/// 内部形的规格体 → 结构化行（拆成「监听」「目标」两列）。
///
/// L/R 的规格是 `[addr:]port:host:port` ⇒ 从右往左拆：最后一个字段是
/// 目标端口，倒数第二个是目标主机，剩下的是监听。
fn split_spec(kind: char, spec: &str) -> (String, String) {
    let spec = spec.trim();
    if kind == 'D' {
        return (spec.to_string(), String::new());
    }
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() >= 4 {
        // addr:port:host:port
        let listen = parts[..parts.len() - 2].join(":");
        let target = parts[parts.len() - 2..].join(":");
        (listen, target)
    } else if parts.len() == 3 {
        // port:host:port
        (parts[0].to_string(), format!("{}:{}", parts[1], parts[2]))
    } else {
        // 拆不动就整条塞进「监听」，目标留空（表里会显出不对，用户能改）
        (spec.to_string(), String::new())
    }
}

/// 两把 ini 键 → 结构化行。
///
/// ⚠️ 顺序：**终端的在前、进程内在后**（与界面上"终端转发在上、进程内
/// 映射在下"的老习惯一致，用户扫一眼不用重新找）。
pub fn rows_from_storage(term: &str, inproc: &str) -> Vec<FwdRow> {
    let (t, i) = rows_split(term, inproc);
    let mut out = t;
    out.extend(i);
    out
}

/// 两把 ini 键 →（终端的规则, 进程内的规则）。
pub fn rows_split(term: &str, inproc: &str) -> (Vec<FwdRow>, Vec<FwdRow>) {
    let one = |value: &str, in_process: bool| -> Vec<FwdRow> {
        forward::parse_rules(value)
            .into_iter()
            .map(|r| {
                let (listen, target) = split_spec(r.kind, &r.spec);
                FwdRow {
                    kind: r.kind,
                    listen,
                    target,
                    in_process,
                    enabled: r.enabled,
                }
            })
            .collect()
    };
    (one(term, false), one(inproc, true))
}

/// 结构化行 → 两把 ini 键（存储格式不变）。
///
/// ⚠️ **停用前缀 `!` 照旧**：停用的规则要留在配置里（用户随时能启用），
/// 只是不生效。
/// ⚠️ **不合法的行直接丢掉**（表里填了一半的行不该进配置）。
pub fn rows_to_storage(rows: &[FwdRow]) -> (String, String) {
    let mut term: Vec<FwdRule> = Vec::new();
    let mut inproc: Vec<FwdRule> = Vec::new();
    for r in rows {
        if !r.is_valid() {
            continue;
        }
        let rule = FwdRule::new(r.kind, &r.spec(), r.enabled);
        if r.in_process {
            inproc.push(rule);
        } else {
            term.push(rule);
        }
    }
    (
        forward::to_storage_value(&term),
        forward::to_storage_value(&inproc),
    )
}

/// 第 idx 行的 TreeIter（ListStore 里行号就是路径下标）。
fn iter_at(store: &gtk::ListStore, idx: u32) -> Option<gtk::TreeIter> {
    let mut it = store.iter_first()?;
    if idx == 0 {
        return Some(it);
    }
    let mut i = 0u32;
    while store.iter_next(&mut it) {
        i += 1;
        if i == idx {
            return Some(it);
        }
    }
    None
}

/* ---------------- 表格构件 ---------------- */

/// 结构化转发表（阶段 71）。
pub struct FwdTable {
    /// 工具栏 + 表格
    pub widget: gtk::Box,
    store: gtk::ListStore,
    view: gtk::TreeView,
    /// 正在编辑的行号（双击改类型/机制时用）
    editing: RefCell<Option<usize>>,
}

impl FwdTable {
    /// 建表（`rows` 是初始内容）。
    pub fn new(rows: Vec<FwdRow>) -> Self {
        let store = gtk::ListStore::new(&[
            glib::Type::STRING, // 类型（显示字）
            glib::Type::STRING, // 监听
            glib::Type::STRING, // 目标
            glib::Type::STRING, // 机制（显示字）
            glib::Type::BOOL,   // 启用
        ]);

        let view = gtk::TreeView::with_model(&store);
        view.set_headers_visible(true);
        view.set_enable_search(false);
        // 表格塞在滚动窗口里：规则多的时候要能滚
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(150);
        scroll.set_child(Some(&view));
        scroll.set_vexpand(true);

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // ⚠️ 滚动窗口**必须**挂进容器，否则表格压根不在控件树里
        // （既不显示，冒烟也找不到它）
        widget.append(&scroll);

        let t = Self {
            widget,
            store,
            view,
            editing: RefCell::new(None),
        };
        t.build_columns();
        t.set_rows(&rows);
        t
    }

    fn build_columns(&self) {
        // 类型 / 监听 / 目标 / 机制：文字列（监听与目标可直接在表里改）
        let add_text = |title: &str, col: u32, editable: bool| {
            let renderer = gtk::CellRendererText::new();
            renderer.set_editable(editable);
            let column = gtk::TreeViewColumn::new();
            column.set_title(title);
            column.set_resizable(true);
            // ⚠️ renderer 必须 pack 进列才会渲染（漏掉 → 整列空白）
            column.pack_start(&renderer, true);
            column.add_attribute(&renderer, "text", col as i32);
            if editable {
                // 监听 / 目标：双击直接在表里改
                let store = self.store.clone();
                renderer.connect_edited(move |_r, path, text| {
                    let Some(idx) = path.indices().first().copied() else {
                        return;
                    };
                    if let Some(iter) = iter_at(&store, idx as u32) {
                        store.set(&iter, &[(col, &text.to_string())]);
                    }
                });
            }
            self.view.append_column(&column);
        };

        add_text(tr("类型"), 0, false);
        add_text(tr("监听"), 1, true);
        add_text(tr("目标"), 2, true);
        add_text(tr("机制"), 3, false);

        // 启用：勾选框（点一下就切）
        let renderer = gtk::CellRendererToggle::new();
        let column = gtk::TreeViewColumn::new();
        column.set_title(tr("启用"));
        column.pack_start(&renderer, true);
        column.add_attribute(&renderer, "active", 4);
        self.view.append_column(&column);
    }

    /// 覆盖全部行。
    pub fn set_rows(&self, rows: &[FwdRow]) {
        self.store.clear();
        for r in rows {
            self.append_row(r);
        }
    }

    fn append_row(&self, r: &FwdRow) {
        let it = self.store.append();
        self.store.set(
            &it,
            &[
                (0, &kind_to_display(r.kind).to_string()),
                (1, &r.listen.clone()),
                (2, &r.target.clone()),
                (3, &mech_to_display(r.in_process).to_string()),
                (4, &r.enabled),
            ],
        );
    }

    /// 表里的行（模型是唯一真相）。
    pub fn rows(&self) -> Vec<FwdRow> {
        let mut out = Vec::new();
        let mut cur = match self.store.iter_first() {
            Some(it) => it,
            None => return out,
        };
        loop {
            let s = |i: i32| -> String {
                self.store
                    .get_value(&cur, i)
                    .get::<String>()
                    .unwrap_or_default()
            };
            let enabled = self.store.get_value(&cur, 4).get::<bool>().unwrap_or(true);
            out.push(FwdRow {
                kind: kind_from_display(&s(0)),
                listen: s(1),
                target: s(2),
                in_process: mech_from_display(&s(3)),
                enabled,
            });
            if !self.store.iter_next(&mut cur) {
                break;
            }
        }
        out
    }

    /// 选中的一行（没有选中返回 None）。
    pub fn selected(&self) -> Option<usize> {
        // ⚠️ 用 `selected_rows()` 拿路径：`selected()` 给的是 TreeIter，
        // 换不出行号
        let (sel, _unsel) = self.view.selection().selected_rows();
        let path = sel.first()?;
        let idx = path.indices().first().copied()?;
        Some(idx as usize)
    }

    /// 加一行。
    pub fn add(&self, row: FwdRow) {
        self.append_row(&row);
    }

    /// 删掉第 idx 行。
    pub fn remove(&self, idx: usize) {
        if let Some(it) = iter_at(&self.store, idx as u32) {
            self.store.remove(&it);
        }
    }

    /// 改第 idx 行（类型/机制那一类整体替换）。
    pub fn update(&self, idx: usize, row: &FwdRow) {
        if let Some(it) = iter_at(&self.store, idx as u32) {
            self.store.set(
                &it,
                &[
                    (0, &kind_to_display(row.kind).to_string()),
                    (1, &row.listen.clone()),
                    (2, &row.target.clone()),
                    (3, &mech_to_display(row.in_process).to_string()),
                    (4, &row.enabled),
                ],
            );
        }
    }

    /// 正在编辑的行号（双击改类型/机制时）。
    pub fn editing(&self) -> Option<usize> {
        *self.editing.borrow()
    }

    pub fn set_editing(&self, idx: Option<usize>) {
        *self.editing.borrow_mut() = idx;
    }

    /// 表格控件（双击等交互要用）。
    pub fn view(&self) -> &gtk::TreeView {
        &self.view
    }
}

/// 类型/机制的编辑对话框（双击一行时弹）。
///
/// ⚠️ **异步**：对话框是模态的，"确定"要等用户点 ⇒ 用回调把结果送回来
/// （同步返回永远是 `None`）。
pub fn edit_dialog<F: FnOnce(Option<FwdRow>) + 'static>(
    parent: Option<&gtk::Window>,
    row: &FwdRow,
    done: F,
) {
    let dlg = crate::windows::dialog(
        tr("转发规则"),
        parent,
        &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("确定"), gtk::ResponseType::Ok),
        ],
    );
    dlg.set_default_size(420, -1);
    let root = crate::panel::dialogs::form_box();

    let w_kind = gtk::ComboBoxText::new();
    for (id, name) in [("L", tr("本地")), ("R", tr("远程")), ("D", tr("动态"))] {
        w_kind.append(Some(id), name);
    }
    w_kind.set_active_id(Some(&row.kind.to_string()));

    let w_mech = gtk::ComboBoxText::new();
    w_mech.append(Some("term"), tr("终端（spawn 的 ssh -L/-R/-D）"));
    w_mech.append(Some("inproc"), tr("进程内（应用自己那条 SSH 连接）"));
    w_mech.set_active_id(Some(if row.in_process { "inproc" } else { "term" }));

    let w_listen = gtk::Entry::new();
    w_listen.set_text(&row.listen);
    w_listen.set_placeholder_text(Some(tr("端口，或 地址:端口")));

    let w_target = gtk::Entry::new();
    w_target.set_text(&row.target);
    w_target.set_placeholder_text(Some(tr("主机:端口")));

    root.append(&crate::panel::dialogs::form_row(tr("类型"), &w_kind));
    root.append(&crate::panel::dialogs::form_row(tr("机制"), &w_mech));
    root.append(&crate::panel::dialogs::form_row(tr("监听"), &w_listen));
    root.append(&crate::panel::dialogs::form_row(tr("目标"), &w_target));

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();

    let done = RefCell::new(Some(done));
    let enabled = row.enabled;
    dlg.connect_response(move |dlg, resp| {
        let out = if resp == gtk::ResponseType::Ok {
            let kind = w_kind
                .active_id()
                .and_then(|s| s.chars().next())
                .unwrap_or('L');
            let in_process = w_mech.active_id().as_deref() == Some("inproc");
            Some(FwdRow {
                kind,
                listen: w_listen.text().trim().to_string(),
                target: w_target.text().trim().to_string(),
                in_process,
                enabled,
            })
        } else {
            None
        };
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(out);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 结构化表 ⇄ 两把 ini 键**往返无损**（阶段 71 的验收点）。
    ///
    /// ⚠️ 停用前缀 `!` 也要保住：停用的规则要留在配置里（用户随时能启用）。
    #[test]
    fn 结构化表与两把键往返无损() {
        let term = "L:9000:host:80,!R:9200:h:22,D:1080";
        let inproc = "L:9100:host:81,!D:1081";
        let rows = rows_from_storage(term, inproc);
        assert_eq!(rows.len(), 5, "{rows:?}");
        // 拆成监听/目标两列
        assert_eq!(rows[0].listen, "9000");
        assert_eq!(rows[0].target, "host:80");
        assert_eq!(rows[0].kind_name(), "本地");
        assert!(!rows[0].in_process);
        assert!(rows[0].enabled);
        // 停用的保住 enabled=false
        assert_eq!(rows[1].kind, 'R');
        assert!(!rows[1].enabled);
        // 动态（SOCKS）没有目标
        assert_eq!(rows[2].kind, 'D');
        assert_eq!(rows[2].listen, "1080");
        assert!(rows[2].target.is_empty());
        // 进程内的那几条在后面
        assert!(rows[3].in_process);
        assert!(!rows[4].enabled);

        // 回去 ⇒ 两把键一字不差
        let (t2, i2) = rows_to_storage(&rows);
        assert_eq!(t2, term, "终端那把键要原样");
        assert_eq!(i2, inproc, "进程内那把键要原样");
    }

    /// 带监听地址的规格也要能拆开再拼回去。
    #[test]
    fn 带地址的规格往返无损() {
        let term = "L:127.0.0.1:9000:host:80";
        let rows = rows_from_storage(term, "");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].listen, "127.0.0.1:9000");
        assert_eq!(rows[0].target, "host:80");
        let (t2, _) = rows_to_storage(&rows);
        assert_eq!(t2, term);
    }

    /// 填了一半的行**不进配置**（表里点了保存也不能写出坏规则）。
    #[test]
    fn 不合法的行不进配置() {
        let rows = vec![
            FwdRow::new('L', "9000", "host:80", false),
            FwdRow::new('L', "9000", "", false),      // 缺目标
            FwdRow::new('D', "abc", "", false),        // 端口不是数字
            FwdRow::new('X', "9000", "host:80", false), // 未知类型
        ];
        let (t, i) = rows_to_storage(&rows);
        assert_eq!(t, "L:9000:host:80", "{t}");
        assert!(i.is_empty());
    }
}
