//! 「表数据导入 / 导出」向导（阶段 52 起，阶段 72 按审计 P1-7 / P1-8 重做）。
//!
//! **一个入口，两个去向**：
//!
//! - **导出表…**（[`open_export`]）：把**整张表**写进本地文件（CSV / SQL / JSON）。
//!   ⚠️ 跟结果页签上的「导出」**不是一回事**：那边导的是**当前这一页**
//!   （PG 的服务端游标一页、MySQL 客户端切片上限 2000 行），这边导的是
//!   **整张表**——分页查、分页写，导完一页扔一页。
//! - **导入数据（CSV）…**（[`open_import`]）：选**目标表** + 选 CSV 文件 →
//!   字段映射 → 整批一个事务。
//!
//! **为什么是两个向导、而不是一个带「方向」下拉的对话框**（阶段 72 的改动）：
//! 审计 P1-7 说得很直白——「『数据传输』导入只是给同一个 CSV 向导加了一层
//! 方向选择，是多余的中间步骤」。所以顶层入口改成**菜单**：选哪一项就直接
//! 进那个向导，中间**不再多一个对话框**。
//!
//! **为什么不用「数据传输」这个词**（审计 P1-8）：SFTP 那边「传输」指的是
//! **文件**上传下载，DB 这边「传输」指的是**表数据**。两个完全不同的东西
//! 共用一个词，用户根本分不清点的是哪个 ⇒ DB 这一侧一律叫「表数据导入 /
//! 导出」。
//!
//! **为什么不做「库 → 库」**：那要**同时连两个数据库**。我们的数据库通道
//! 是「一条 SSH 连接一个会话」（阶段 13 定的架构），要支持第二个连接就得
//! 把 worker 改成多会话——那是另一个量级的改动，塞进这一步不合适。
//! 折中办法是**导出成 SQL 文件、在另一端导入**（两趟，但两头都能用）。

use gtk::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::backend::dbexport::ExportFmt;

use super::dbimport;
use super::dbui;
use super::dialogs;
use super::PanelState;
use crate::i18n::tr;
use crate::trf;

/// 最近一次打开的向导（测试钩子）。
///
/// ⚠️ **只在主线程用**（`thread_local!`）：GTK 的控件只能在主线程碰。
struct Last {
    /// 这是**导出**向导（`false`）还是**导入**向导（`true`）
    import: bool,
    dlg: gtk::Dialog,
    path: gtk::Entry,
    /// 提示文字（**存控件不存字符串**：导出 / 导入的提示不一样）
    hint: gtk::Label,
    /// 「对象」下拉的选项（`id`, 显示文字）——**自己记一份**，
    /// 不去问 GTK 的 model（ComboBoxText 的 model API 绕，改一个版本就断）。
    objects: Vec<(String, String)>,
    /// 「格式」下拉的选项（`id`, 显示文字）
    formats: Vec<(String, String)>,
}

thread_local! {
    static LAST: RefCell<Option<Last>> = const { RefCell::new(None) };
}

/// 测试钩子：最近打开的向导里「对象」下拉的选项（`id`, 显示文字）。
pub fn last_objects() -> Vec<(String, String)> {
    LAST.with(|l| {
        let b = l.borrow();
        b.as_ref().map(|x| x.objects.clone()).unwrap_or_default()
    })
}

/// 测试钩子：最近打开的向导里「格式」下拉的选项（`id`, 显示文字）。
pub fn last_formats() -> Vec<(String, String)> {
    LAST.with(|l| {
        let b = l.borrow();
        b.as_ref().map(|x| x.formats.clone()).unwrap_or_default()
    })
}

/// 测试钩子：最近打开的是不是**导入**向导。
pub fn last_is_import() -> bool {
    LAST.with(|l| l.borrow().as_ref().map(|x| x.import).unwrap_or(false))
}

/// 测试钩子：设向导里的文件路径。
pub fn last_set_path(p: &str) {
    LAST.with(|l| {
        if let Some(last) = l.borrow().as_ref() {
            last.path.set_text(p);
        }
    });
}

/// 测试钩子：关掉最近打开的向导（模态窗开着会挡住后面的操作）。
pub fn close_last() {
    LAST.with(|l| {
        let mut b = l.borrow_mut();
        if let Some(last) = b.take() {
            // ⚠️ **必须真的关掉窗口**：这是模态窗，不关的话后面的操作全被挡住
            last.dlg.close();
        }
    });
}

/// 测试钩子：向导窗口的实际尺寸（`宽, 高`）与位置。
pub fn last_geometry() -> (i32, i32, i32, i32) {
    LAST.with(|l| {
        let b = l.borrow();
        match b.as_ref() {
            Some(last) => {
                let (w, h) = last.dlg.default_size();
                (w, h, last.dlg.width(), last.dlg.height())
            }
            None => (0, 0, 0, 0),
        }
    })
}

/// 测试钩子：向导窗口在屏幕上的位置 —— **GTK4 拿不到**。
///
/// GTK4 删掉了 `gtk_window_move` / `gtk_window_get_position`，窗口摆哪儿
/// 由**窗口管理器**决定。没有 WM 的 Xvfb 下新窗口会顶到屏幕外头，
/// 截图只能靠**开一个更大的虚拟屏**（`-screen 0 1920x1200x24`）把它兜住。
pub fn last_position() -> (i32, i32) {
    (0, 0)
}

/// 测试钩子：读向导里的提示文字。
pub fn last_hint() -> String {
    LAST.with(|l| {
        let b = l.borrow();
        b.as_ref().map(|x| x.hint.text().to_string()).unwrap_or_default()
    })
}

/// 测试钩子：读向导里的文件路径。
pub fn last_path() -> String {
    LAST.with(|l| {
        let b = l.borrow();
        b.as_ref().map(|x| x.path.text().to_string()).unwrap_or_default()
    })
}

/// 下拉里用的分隔符（模式名和表名之间）。
///
/// **为什么不用点**：表名里可能有点（`public.my.table` 这种合法名字），
/// 用点分不开模式名和表名。
const SEP: char = '\u{1f}';

/// 打开**导出表**向导。
///
/// `preset` = 预先选中的对象（`模式`, `表名`）——从对象树右键「导出表…」进来时带上。
pub fn open_export(
    st: &Rc<PanelState>,
    parent: Option<&gtk::Window>,
    preset: Option<(String, String)>,
) {
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(tr("导出表数据"), parent, &[
        (tr("取消"), gtk::ResponseType::Cancel),
        (tr("开始"), gtk::ResponseType::Ok),
    ]);
    dlg.set_default_response(gtk::ResponseType::Ok);
    dialogs::style_dialog(&dlg);
    dlg.set_default_size(560, -1);

    let box_ = dialogs::form_box();

    // ---- 当前连接（只读显示） ----
    //
    // **为什么不摆连接下拉**：工作台的连接是**一个**（阶段 13 起连接归
    // 连接层管），换连接要回连接管理器。这里摆下拉框只会让人以为能切。
    let conn_text = dbui::current_conn_label(st).unwrap_or_else(|| tr("（还没连上数据库）").into());
    let conn = gtk::Label::new(Some(&conn_text));
    conn.add_css_class("dim-label");
    conn.set_xalign(0.0);
    box_.append(&dialogs::form_row(tr("连接"), &conn));

    // ---- 对象（表 / 视图） ----
    //
    // **只列对象树上已经加载出来的**：树是懒加载的，用户没展开的模式这里
    // 没有。想导哪张表，就先在左边把它展开出来——这跟 dbx 的习惯一致
    // （它也是跟着对象树用的）。
    let objects = dbui::tree_objects(st);
    let obj = gtk::ComboBoxText::new();
    for (schema, name, is_view) in &objects {
        let id = format!("{schema}{SEP}{name}");
        let text = if *is_view {
            trf!("{schema}.{name}（视图）", schema = schema, name = name)
        } else {
            format!("{schema}.{name}")
        };
        obj.append(Some(&id), &text);
    }
    let objects_empty = objects.is_empty();
    if objects_empty {
        // 一张表都没有 → 摆个占位说明，别留个**空下拉框**（看着像坏了）
        obj.append(Some(""), tr("（左边对象树里还没有表，先展开一个模式）"));
        obj.set_sensitive(false);
    } else {
        // ⚠️ **默认选中第一个**：ComboBoxText 一个都不选的话显示的是**空白**
        // （看着像没内容），用户得自己点一下才知道能选。
        match &preset {
            Some((schema, name)) => {
                obj.set_active_id(Some(&format!("{schema}{SEP}{name}")));
            }
            None => obj.set_active(Some(0)),
        }
    }
    box_.append(&dialogs::form_row(tr("对象"), &obj));

    // ---- 格式 ----
    let fmt = gtk::ComboBoxText::new();
    for (id, f) in ExportFmt::ALL {
        fmt.append(Some(id), f.label());
    }
    fmt.set_active_id(Some("csv"));
    box_.append(&dialogs::form_row(tr("格式"), &fmt));

    // ---- 文件 ----
    let path_entry = gtk::Entry::new();
    path_entry.set_placeholder_text(Some(tr("要写到哪个文件（也可以点「选择…」）")));
    path_entry.set_hexpand(true);
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    path_row.append(&path_entry);
    path_row.append(&file_pick_button(parent, &path_entry));
    box_.append(&dialogs::form_row(tr("文件"), &path_row));

    // ---- 提示 ----
    //
    // ⚠️ **别写 markdown 星号**：这是普通 gtk::Label（不是 markup label），
    // 星号会**原样显示**出来。
    let hint = gtk::Label::new(Some(
        tr("导出会把整张表都写进文件（不止当前这一页）。\
         SQL 格式会连建表语句一起写出去，搬到空库也能跑。"),
    ));
    hint.add_css_class("pp-hint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    box_.append(&hint);

    // ---- 「开始」 ----
    {
        let st2 = Rc::clone(st);
        let parent2 = parent.cloned();
        let obj = obj.clone();
        let fmt = fmt.clone();
        let path_entry = path_entry.clone();
        dlg.connect_response(move |d, r| {
            if r != gtk::ResponseType::Ok {
                d.close();
                return;
            }
            if objects_empty {
                dialogs::show_error(
                    parent2.as_ref(),
                    tr("导出表数据"),
                    tr("左边对象树里还没有表。\n先展开一个模式，选中要导出的表再来。"),
                );
                return;
            }
            let Some(id) = obj.active_id() else {
                dialogs::show_error(parent2.as_ref(), tr("导出表数据"), tr("先选一个要导出的表。"));
                return;
            };
            let (schema, table) = split_id(&id);
            let path = path_entry.text().trim().to_string();
            if path.is_empty() {
                dialogs::show_error(
                    parent2.as_ref(),
                    tr("导出表数据"),
                    tr("先选一个要写出的文件（点「选择…」）。"),
                );
                return;
            }
            let f = ExportFmt::from_id(&fmt.active_id().unwrap_or_default());
            dbui::start_export(&st2, &schema, &table, &path, f);
            d.close();
        });
    }

    // ⚠️ **内容要自己塞进 content_area**：`Dialog::with_buttons` 只给你
    // 标题栏 + 按钮区，中间那块是空的——不 append 的话窗口里什么都没有
    // （看着像"点了没反应"）。
    dlg.content_area().append(&box_);

    // 记下控件（测试钩子用）
    let obj_items: Vec<(String, String)> = objects
        .iter()
        .map(|(schema, name, is_view)| {
            let id = format!("{schema}{SEP}{name}");
            let text = if *is_view {
                trf!("{schema}.{name}（视图）", schema = schema, name = name)
            } else {
                format!("{schema}.{name}")
            };
            (id, text)
        })
        .collect();
    let fmt_items: Vec<(String, String)> = ExportFmt::ALL
        .iter()
        .map(|(id, f)| (id.to_string(), f.label().to_string()))
        .collect();
    LAST.with(|l| {
        *l.borrow_mut() = Some(Last {
            import: false,
            dlg: dlg.clone(),
            path: path_entry.clone(),
            hint: hint.clone(),
            objects: obj_items,
            formats: fmt_items,
        });
    });

    dlg.present();
}

/// 打开**导入数据（CSV）**向导。
///
/// `preset` = 预先选中的**目标表**（`模式`, `表名`）——从对象树右键
/// 「导入数据（CSV）…」进来时带上。
///
/// ⚠️ **为什么导入向导自己带「目标表」下拉**：以前是「数据传输」对话框
/// 先选方向、再进 CSV 向导，**中间多了一层**（审计 P1-7）。现在导入只有
/// 这一层：选表 + 选文件 → 字段映射 → 执行。
pub fn open_import(
    st: &Rc<PanelState>,
    parent: Option<&gtk::Window>,
    preset: Option<(String, String)>,
) {
    let dlg = crate::windows::dialog(tr("导入数据（CSV）…"), parent, &[
        (tr("取消"), gtk::ResponseType::Cancel),
        (tr("开始"), gtk::ResponseType::Ok),
    ]);
    dlg.set_default_response(gtk::ResponseType::Ok);
    dialogs::style_dialog(&dlg);
    dlg.set_default_size(560, -1);

    let box_ = dialogs::form_box();

    // ---- 当前连接（只读显示） ----
    let conn_text = dbui::current_conn_label(st).unwrap_or_else(|| tr("（还没连上数据库）").into());
    let conn = gtk::Label::new(Some(&conn_text));
    conn.add_css_class("dim-label");
    conn.set_xalign(0.0);
    box_.append(&dialogs::form_row(tr("连接"), &conn));

    // ---- 目标表 ----
    let objects = dbui::tree_objects(st);
    let obj = gtk::ComboBoxText::new();
    for (schema, name, is_view) in &objects {
        let id = format!("{schema}{SEP}{name}");
        let text = if *is_view {
            trf!("{schema}.{name}（视图）", schema = schema, name = name)
        } else {
            format!("{schema}.{name}")
        };
        obj.append(Some(&id), &text);
    }
    let objects_empty = objects.is_empty();
    if objects_empty {
        obj.append(Some(""), tr("（左边对象树里还没有表，先展开一个模式）"));
        obj.set_sensitive(false);
    } else {
        match &preset {
            Some((schema, name)) => {
                obj.set_active_id(Some(&format!("{schema}{SEP}{name}")));
            }
            None => obj.set_active(Some(0)),
        }
    }
    box_.append(&dialogs::form_row(tr("目标表"), &obj));

    // ---- 文件 ----
    let path_entry = gtk::Entry::new();
    path_entry.set_placeholder_text(Some(tr("要导入哪个 CSV 文件（也可以点「选择…」）")));
    path_entry.set_hexpand(true);
    let path_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    path_row.append(&path_entry);
    {
        // 导入用**带过滤器**的选择器（只放行 CSV / 文本）——跟老向导一样。
        let pick = gtk::Button::with_label(tr("选择…"));
        let entry = path_entry.clone();
        let parent2 = parent.cloned();
        pick.connect_clicked(move |_| {
            dbimport::pick_csv_into(parent2.as_ref(), &entry);
        });
        path_row.append(&pick);
    }
    box_.append(&dialogs::form_row(tr("文件"), &path_row));

    // ---- 提示 ----
    let hint = gtk::Label::new(Some(
        tr("点「开始」后会打开字段映射：左边是表的列，右边选 CSV 里对应的列。\
         整批数据在一个事务里执行，失败整体回滚。"),
    ));
    hint.add_css_class("pp-hint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    box_.append(&hint);

    // ---- 「开始」 ----
    {
        let st2 = Rc::clone(st);
        let parent2 = parent.cloned();
        let obj = obj.clone();
        let path_entry = path_entry.clone();
        dlg.connect_response(move |d, r| {
            if r != gtk::ResponseType::Ok {
                d.close();
                return;
            }
            if objects_empty {
                dialogs::show_error(
                    parent2.as_ref(),
                    tr("导入数据（CSV）…"),
                    tr("左边对象树里还没有表。\n先展开一个模式，选中要导入的表再来。"),
                );
                return;
            }
            let Some(id) = obj.active_id() else {
                dialogs::show_error(
                    parent2.as_ref(),
                    tr("导入数据（CSV）…"),
                    tr("先选一个要导入到的表。"),
                );
                return;
            };
            let (schema, table) = split_id(&id);
            let path = path_entry.text().trim().to_string();
            if path.is_empty() {
                dialogs::show_error(
                    parent2.as_ref(),
                    tr("导入数据（CSV）…"),
                    tr("先选一个要导入的文件（点「选择…」）。"),
                );
                return;
            }
            dbui::on_import_csv_path(&st2, &schema, &table, &PathBuf::from(path));
            d.close();
        });
    }

    dlg.content_area().append(&box_);

    let obj_items: Vec<(String, String)> = objects
        .iter()
        .map(|(schema, name, is_view)| {
            let id = format!("{schema}{SEP}{name}");
            let text = if *is_view {
                trf!("{schema}.{name}（视图）", schema = schema, name = name)
            } else {
                format!("{schema}.{name}")
            };
            (id, text)
        })
        .collect();
    LAST.with(|l| {
        *l.borrow_mut() = Some(Last {
            import: true,
            dlg: dlg.clone(),
            path: path_entry.clone(),
            hint: hint.clone(),
            objects: obj_items,
            // 导入没有「格式」——CSV 是唯一格式（老向导里那个下拉只在导出时才有）
            formats: Vec::new(),
        });
    });

    dlg.present();
}

/// 导出向导的「选择…」按钮（把挑到的路径填回输入框）。
///
/// **为什么自己造而不是直接摆原生选择器**：原生选择器是**非阻塞**的
/// （选完要等回调），而这里要把选到的路径**填回输入框**给用户看——所以得
/// 自己拿回调把路径写回 `entry`。
fn file_pick_button(parent: Option<&gtk::Window>, entry: &gtk::Entry) -> gtk::Button {
    let pick = gtk::Button::with_label(tr("选择…"));
    let path_entry = entry.clone();
    let parent2 = parent.cloned();
    pick.connect_clicked(move |_| {
        let ch = gtk::FileChooserNative::builder()
            .title(tr("选择要写出的文件"))
            .action(gtk::FileChooserAction::Save)
            .accept_label(tr("选定"))
            .cancel_label(tr("取消"))
            .modal(true)
            .build();
        ch.set_transient_for(parent2.as_ref());
        let entry = path_entry.clone();
        ch.connect_response(move |c, r| {
            if r == gtk::ResponseType::Accept {
                if let Some(f) = c.file() {
                    if let Some(p) = f.path() {
                        entry.set_text(&p.to_string_lossy());
                    }
                }
            }
            c.destroy();
        });
        ch.show();
    });
    pick
}

/// 把下拉的 id 拆回（模式, 表名）。
fn split_id(id: &str) -> (String, String) {
    match id.split_once(SEP) {
        Some((s, t)) => (s.to_string(), t.to_string()),
        None => (String::new(), id.to_string()),
    }
}
