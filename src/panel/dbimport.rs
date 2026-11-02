//! CSV 导入向导的**对话框**（阶段 18：Navicat 的 Import Wizard）。
//!
//! **为什么单独一个模块**：导入有三步（选文件 → 字段映射 → 执行），
//! 每一步都是一堆 GTK 代码，混进 `dbui.rs` 会把那个文件撑得没法看。
//! 这里只管"界面"，流程编排（查表结构、发命令、收结果）在 `dbui.rs`。
//!
//! **三步的分工**：
//! 1. **选文件**（`pick_csv`）：只挑路径，不解析；
//! 2. **字段映射**（`show_mapping`）：左边是表的列，右边选对应的 CSV 列；
//! 3. **执行**：回 `dbui.rs` 编排。
//!
//! **映射为什么必须让用户确认**：CSV 的列顺序跟表的不一样是常事，
//! 按位置猜会把数据写进错的列——那是**静默的数据损坏**，比导入失败严重得多。

use gtk::prelude::*;

use crate::backend::dbcsv;
use crate::i18n::tr;
use crate::trf;

/// 导入选项。
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// 首行是列名（不是数据）
    pub first_row_is_header: bool,
    /// 空字段当作 NULL（否则当作空字符串）
    pub empty_as_null: bool,
    /// 字段分隔符
    pub separator: char,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            first_row_is_header: true,
            empty_as_null: true,
            separator: ',',
        }
    }
}

/// 字段映射结果。
#[derive(Debug, Clone)]
pub struct ImportMapping {
    pub options: ImportOptions,
    /// 每个**表的列**对应哪个 CSV 列（`None` = 不导入这一列，用数据库默认值）
    pub map: Vec<Option<usize>>,
    /// CSV 的列名（表头；没有表头时是「第 N 列」）
    pub csv_header: Vec<String>,
    /// 参与映射的 CSV 行（数据行）
    pub rows: Vec<Vec<String>>,
}

/// 选一个 CSV 文件，**把路径填回输入框**。
///
/// **为什么改成"填回输入框"**（阶段 72）：导入向导现在自己带一个「文件」
/// 输入框（`dbtransfer::open_import`），用户能**看见**挑到的是哪个文件、
/// 也能手动改路径——所以选择器只管"挑一个"，挑完写回输入框。
///
/// GTK4 的 `FileChooserDialog` 是非阻塞的（没有 `run()`），选完要等用户点
/// 「打开」才知道选了哪个文件——所以只能回调。
pub fn pick_csv_into(parent: Option<&gtk::Window>, entry: &gtk::Entry) {
    let mut b = gtk::FileChooserDialog::builder()
        .title(tr("选择要导入的 CSV 文件"))
        .action(gtk::FileChooserAction::Open);
    if let Some(p) = parent {
        b = b.transient_for(p);
    }
    let dlg = b.build();
    dlg.add_button(tr("取消"), gtk::ResponseType::Cancel);
    dlg.add_button(tr("打开"), gtk::ResponseType::Accept);
    dlg.set_default_response(gtk::ResponseType::Accept);
    // 只放行常见的数据文件（用户也可以在对话框里手动输入别的名字）
    let filters = gtk::FileFilter::new();
    filters.set_name(Some(tr("CSV / 文本文件")));
    filters.add_mime_type("text/csv");
    filters.add_mime_type("text/plain");
    filters.add_pattern("*.csv");
    filters.add_pattern("*.tsv");
    filters.add_pattern("*.txt");
    dlg.add_filter(&filters);
    let entry = entry.clone();
    dlg.connect_response(move |dlg, resp| {
        if resp == gtk::ResponseType::Accept {
            if let Some(p) = dlg.file().and_then(|f| f.path()) {
                entry.set_text(&p.to_string_lossy());
            }
        }
        dlg.destroy();
    });
    dlg.present();
}

/// 选分隔符（对话框里的下拉）。
fn separator_combo() -> gtk::ComboBoxText {
    let c = gtk::ComboBoxText::new();
    for (id, label) in [(",", tr("逗号（,）")), ("\t", tr("制表符（Tab）")), (";", tr("分号（;）"))] {
        c.append(Some(id), label);
    }
    c.set_active_id(Some(","));
    c
}

/// 解析选项 → 分隔符。
fn separator_of(c: &gtk::ComboBoxText) -> char {
    match c.active_id().as_deref() {
        Some("\t") => '\t',
        Some(";") => ';',
        _ => ',',
    }
}

/// 打开**字段映射**对话框。
///
/// `table_cols` 是目标表的列（`(列名, 类型名)`）。
/// `csv_text` 是文件原文（对话框里改分隔符后要重新解析）。
///
/// 确定时回调 `ImportMapping`；取消不回调。
pub fn show_mapping(
    parent: Option<&gtk::Window>,
    target: &str,
    csv_text: &str,
    table_cols: &[(String, String)],
    on_ok: impl Fn(ImportMapping) + 'static,
) {
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(&trf!("导入数据到 {target}", target = target), parent, &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("开始导入"), gtk::ResponseType::Ok),
        ]);
    dlg.set_default_response(gtk::ResponseType::Ok);
    crate::panel::dialogs::style_dialog(&dlg);

    // 下面的闭包要塞进 GTK 回调里（要 `'static`），所以参数得转成 owned
    let csv_text = csv_text.to_string();
    let table_cols = table_cols.to_vec();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    // ---- 顶部：文件信息 + 解析选项 ----
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let sep = separator_combo();
    let chk_header = gtk::CheckButton::with_label(tr("首行是列名"));
    chk_header.set_active(true);
    let chk_null = gtk::CheckButton::with_label(tr("空字段当作 NULL"));
    chk_null.set_active(true);
    let btn_reload = gtk::Button::with_label(tr("重新读取"));
    btn_reload.set_tooltip_text(Some(tr("改了分隔符 / 表头设置后重新解析这个文件")));
    top.append(&sep);
    top.append(&chk_header);
    top.append(&chk_null);
    top.append(&btn_reload);
    root.append(&top);

    let info = gtk::Label::new(Some(""));
    info.add_css_class("dim-label");
    info.set_xalign(0.0);
    info.set_wrap(true);
    root.append(&info);

    // ---- 中间：映射表（表列 | 类型 | CSV 列）----
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(8);
    root.append(&grid);

    // ---- 预览 ----
    let preview = gtk::Label::new(Some(""));
    preview.add_css_class("dim-label");
    preview.set_xalign(0.0);
    preview.set_wrap(true);
    preview.set_max_width_chars(80);
    root.append(&preview);

    dlg.content_area().append(&root);

    // 解析状态（可变，由「重新读取」更新）
    let state = std::rc::Rc::new(std::cell::RefCell::new(ImportMapping {
        options: ImportOptions::default(),
        map: Vec::new(),
        csv_header: Vec::new(),
        rows: Vec::new(),
    }));
    // 每行一个下拉（表列 → CSV 列）
    let combos: std::rc::Rc<std::cell::RefCell<Vec<gtk::ComboBoxText>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    // 重新解析并刷新界面
    let reload = {
        let state = state.clone();
        let combos = combos.clone();
        let info = info.clone();
        let preview = preview.clone();
        let grid = grid.clone();
        let sep = sep.clone();
        let chk_header = chk_header.clone();
        move |keep_manual: bool| {
            let opts = ImportOptions {
                first_row_is_header: chk_header.is_active(),
                empty_as_null: chk_null.is_active(),
                separator: separator_of(&sep),
            };
            let parsed = match dbcsv::parse_csv(&csv_text, opts.separator) {
                Ok(t) => t,
                Err(e) => {
                    info.set_text(&trf!("⚠ 解析失败：{e}", e = e));
                    return;
                }
            };
            let (header, rows) = if opts.first_row_is_header && !parsed.is_empty() {
                (parsed[0].clone(), parsed[1..].to_vec())
            } else {
                // 没有表头：列名按位置编
                let width = parsed.first().map(|r| r.len()).unwrap_or(0);
                let header = (1..=width).map(|i| trf!("第 {i} 列", i = i)).collect();
                (header, parsed.clone())
            };
            // 自动映射（不覆盖用户已经手工改过的）
            let auto = dbcsv::auto_map(&header, &table_cols.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>());
            let mut st = state.borrow_mut();
            st.options = opts.clone();
            st.csv_header = header.clone();
            st.rows = rows.clone();
            st.map = if keep_manual && st.map.len() == auto.len() {
                st.map.clone()
            } else {
                auto.clone()
            };
            let map = st.map.clone();
            drop(st);

            // 刷新映射表
            while let Some(c) = grid.first_child() {
                grid.remove(&c);
            }
            combos.borrow_mut().clear();
            for (i, (name, ty)) in table_cols.iter().enumerate() {
                let lbl = gtk::Label::new(Some(name));
                lbl.set_xalign(0.0);
                let tyl = gtk::Label::new(Some(ty));
                tyl.set_xalign(0.0);
                tyl.add_css_class("dim-label");
                let combo = gtk::ComboBoxText::new();
                combo.append(None, tr("不导入"));
                for (j, h) in header.iter().enumerate() {
                    combo.append(Some(&j.to_string()), h);
                }
                // ⚠️ `active_id` 收的是 `Option<&str>`，临时字符串不能直接
                // 借（会被立刻丢掉）——得先落到一个变量里。
                let active = map.get(i).and_then(|v| v.map(|x| x.to_string()));
                combo.set_active_id(active.as_deref());
                grid.attach(&lbl, 0, i as i32, 1, 1);
                grid.attach(&tyl, 1, i as i32, 1, 1);
                grid.attach(&combo, 2, i as i32, 1, 1);
                combos.borrow_mut().push(combo);
            }
            info.set_text(&trf!("文件解析出 {v0} 列 × {v1} 行待导入", v0 = header.len(), v1 = rows.len()));
            let preview_txt = rows
                .iter()
                .take(3)
                .map(|r| r.join(" | "))
                .collect::<Vec<_>>()
                .join("\n");
            preview.set_text(&if preview_txt.is_empty() {
                tr("（没有数据行）").to_string()
            } else {
                trf!("预览（前 3 行）：\n{preview_txt}", preview_txt = preview_txt)
            });
        }
    };
    // 初次解析
    reload(false);

    // 「重新读取」：重新解析 + 重新自动映射
    {
        let reload = reload.clone();
        btn_reload.connect_clicked(move |_| reload(false));
    }
    // 分隔符 / 表头选项变了 → 自动重新解析（保留手工映射）
    {
        let reload = reload.clone();
        sep.connect_changed(move |_| reload(true));
    }
    {
        let reload = reload.clone();
        chk_header.connect_toggled(move |_| reload(true));
    }

    let combos2 = combos.clone();
    let state2 = state.clone();
    dlg.connect_response(move |d, resp| {
        if resp == gtk::ResponseType::Ok {
            // 把下拉的选择收进映射
            let mut map = state2.borrow().map.clone();
            for (i, c) in combos2.borrow().iter().enumerate() {
                if let Some(id) = c.active_id() {
                    if let Ok(v) = id.parse::<usize>() {
                        if let Some(slot) = map.get_mut(i) {
                            *slot = Some(v);
                        }
                    }
                }
            }
            let mut st = state2.borrow_mut();
            st.map = map;
            let result = st.clone();
            drop(st);
            d.destroy();
            on_ok(result);
            return;
        }
        d.destroy();
    });
    dlg.present();
}

/// 读一个文本文件（CSV）。
///
/// **为什么自己读而不是让 GTK 读**：文件选择器只给路径；真读文件要处理
/// 编码与 IO 错误，这些在选择器回调里做不得（会卡住 UI）。
pub fn read_text(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| trf!("读取文件失败：{e}", e = e))?;
    String::from_utf8(bytes).map_err(|_| {
        tr("文件不是 UTF-8 编码。\n\n\
         请在 Excel 里另存为「CSV UTF-8（逗号分隔）」再试。")
            .to_string()
    })
}
