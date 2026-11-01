//! 快速命令（3.3）：把常用命令做成一排按钮，点了就发到本标签的终端。
//!
//! 两件事让这东西值得做：
//! - **分组**：命令按 group 归类，弹出面板里一屏能扫完
//! - **参数占位符**：`tail -n ${行数:100} ${路径}` 这类模板，点下去先问参数
//!
//! 能力边界：命令是**往终端里敲**（feed_child + 换行），不是 exec 通道。
//! 好处是不用给 worker 加一条 exec 链路，代价是终端里正在跑前台程序时
//! 这行字会进到那个程序的 stdin —— 所以按钮的 tooltip 与文档都写明了。
//!
//! 存 settings.ini 的 [quick_commands]，全局共用（序列化见 settings.rs）。

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use vte4::prelude::*;

use crate::settings::{self, QuickCmd};

use super::PanelState;

/* ---------------- 占位符 ---------------- */

/// 命令里按出现顺序解析出的占位符（去重后的首次出现）。
/// 语法 `${名字}` 或 `${名字:默认值}`；`$${` 转义成字面 `${`。
///
/// 名字里不允许 `:` `}` 与空白——`${a b}` 不是占位符，原样留在命令里。
pub fn parse_placeholders(cmd: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let b: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < b.len() {
        // `$${` → 字面 `${`
        if b[i] == '$' && i + 2 < b.len() && b[i + 1] == '$' && b[i + 2] == '{' {
            i += 3;
            continue;
        }
        if b[i] != '$' || i + 1 >= b.len() || b[i + 1] != '{' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let mut name = String::new();
        while j < b.len() && b[j] != '}' && b[j] != ':' {
            name.push(b[j]);
            j += 1;
        }
        if j >= b.len() || b[j] == '}' && name.trim().is_empty() {
            i += 1; // 没闭合 / 空名字，当普通字符
            continue;
        }
        let mut default = String::new();
        if j < b.len() && b[j] == ':' {
            j += 1;
            while j < b.len() && b[j] != '}' {
                default.push(b[j]);
                j += 1;
            }
        }
        if j < b.len() && !name.trim().is_empty() && !name.contains(char::is_whitespace) {
            let name = name.trim().to_string();
            if !out.iter().any(|(n, _)| n == &name) {
                out.push((name, default));
            }
        }
        i = j + 1;
    }
    out
}

/// 用填好的值替换占位符。`values` 与 `parse_placeholders` 顺序一一对应。
pub fn fill_placeholders(cmd: &str, values: &[String]) -> String {
    let ph = parse_placeholders(cmd);
    let mut out = String::with_capacity(cmd.len());
    let b: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let mut vi = 0;
    while i < b.len() {
        if b[i] == '$' && i + 2 < b.len() && b[i + 1] == '$' && b[i + 2] == '{' {
            out.push_str("${");
            i += 3;
            continue;
        }
        if b[i] != '$' || i + 1 >= b.len() || b[i + 1] != '{' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let mut name = String::new();
        while j < b.len() && b[j] != '}' && b[j] != ':' {
            name.push(b[j]);
            j += 1;
        }
        while j < b.len() && b[j] != ':' && b[j] != '}' {
            j += 1;
        }
        while j < b.len() && b[j] != '}' {
            j += 1;
        }
        if j >= b.len() {
            out.extend(&b[i..]);
            break;
        }
        let name = name.trim();
        match ph.get(vi).map(|(n, _)| n.as_str()) {
            Some(n) if n == name => {
                let v = values.get(vi).map(String::as_str).unwrap_or_default();
                // 引号兜底：用户填了带空格的路径也不至于把命令拆散
                out.push_str(&quote_arg(v));
                vi += 1;
            }
            _ => out.extend(&b[i..=j]),
        }
        i = j + 1;
    }
    out
}

/// 参数含空格或 shell 元字符时加引号；空值给一对空引号，保持参数个数。
fn quote_arg(v: &str) -> String {
    if v.is_empty() {
        return "''".into();
    }
    if v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        return v.to_string();
    }
    format!("'{}'", v.replace('\'', r"'\''"))
}

/* ---------------- 发送 ---------------- */

/// 把一行命令发到指定终端（末尾补换行）。
fn send_to_terminal(term: &vte4::Terminal, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    term.feed_child(format!("{line}\n").as_bytes());
}

/// 点一条命令：没占位符直接发，有占位符先问参数。
fn run(st: &Rc<PanelState>, term: &vte4::Terminal, c: &QuickCmd) {
    let ph = parse_placeholders(&c.cmd);
    if ph.is_empty() {
        send_to_terminal(term, &c.cmd);
        return;
    }
    let parent = st.topwin();
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        &c.label,
        parent.as_ref(),
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("发送", gtk::ResponseType::Ok),
        ],
    );
    dlg.set_default_size(420, -1);
    let grid = gtk::Grid::new();
    grid.set_margin_top(12);
    grid.set_margin_bottom(12);
    grid.set_margin_start(12);
    grid.set_margin_end(12);
    grid.set_row_spacing(6);
    grid.set_column_spacing(8);

    let mut entries: Vec<gtk::Entry> = Vec::new();
    for (i, (name, def)) in ph.iter().enumerate() {
        let e = gtk::Entry::new();
        e.set_text(def);
        e.set_hexpand(true);
        if def.is_empty() {
            e.set_placeholder_text(Some("必填"));
        }
        let l = gtk::Label::new(Some(&format!("{name}")));
        l.set_halign(gtk::Align::End);
        grid.attach(l.upcast_ref::<gtk::Widget>(), 0, i as i32, 1, 1);
        grid.attach(e.upcast_ref::<gtk::Widget>(), 1, i as i32, 1, 1);
        entries.push(e);
    }
    let tip = gtk::Label::new(Some(&format!(
        "模板：{}\n\n带空格或 shell 元字符的参数会自动加引号。",
        c.cmd
    )));
    tip.add_css_class("dim-label");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    grid.attach(tip.upcast_ref::<gtk::Widget>(), 0, ph.len() as i32, 2, 1);
    dlg.content_area().append(&grid);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
    // 回车即发送（默认按钮就是「发送」，这里等价 activates_default）
    let st2 = Rc::clone(st);
    let term2 = term.clone();
    let cmd = c.cmd.clone();
    let label = c.label.clone();
    dlg.connect_response(move |dlg, resp| {
        let vals: Vec<String> = entries.iter().map(|e| e.text().to_string()).collect();
        // 必填项（无默认值）留空就不发，别把半截命令敲进终端
        let missing = ph.iter().enumerate().any(|(i, (_, def))| {
            def.is_empty() && vals.get(i).map(|v| v.trim().is_empty()).unwrap_or(true)
        });
        dlg.destroy();
        if resp != gtk::ResponseType::Ok {
            return;
        }
        if missing {
            super::dialogs::show_error(
                st2.topwin().as_ref(),
                "参数没填全",
                &format!("「{label}」有必填参数是空的，已取消发送。"),
            );
            return;
        }
        send_to_terminal(&term2, &fill_placeholders(&cmd, &vals));
    });
}

/* ---------------- 弹出面板 ---------------- */

/// 弹出按分组排好的命令列表。
///
/// `term` 是命令最终要敲进去的终端（分屏后是当前活动那一块），
/// `anchor` 只是弹层的锚点控件（标签头按钮 / 终端本身）。
pub fn open_popover(st: &Rc<PanelState>, term: &vte4::Terminal, anchor: &gtk::Widget) {
    let pop = crate::menu::popover(anchor);
    // 快捷键（Ctrl+Shift+K）的锚点是整个终端。不指定位置的话 GTK 会按终端
    // 底边摆，弹层跑到窗口最下面；固定到终端左上角，视觉上仍贴着标签栏。
    if anchor.downcast_ref::<vte4::Terminal>().is_some() {
        pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(8, 8, 1, 1)));
    }
    let vbox = crate::menu::body();

    let cmds = settings::quick_commands();
    if cmds.is_empty() {
        let empty = gtk::Label::new(Some("还没有快速命令"));
        empty.add_css_class("pp-menu-empty");
        empty.set_xalign(0.0);
        vbox.append(&empty);
    } else {
        // 按 group 归拢，组内保持配置里的顺序
        let mut groups: Vec<(String, Vec<&QuickCmd>)> = Vec::new();
        for c in &cmds {
            match groups.iter_mut().find(|(g, _)| g == &c.group) {
                Some((_, v)) => v.push(c),
                None => groups.push((c.group.clone(), vec![c])),
            }
        }
        for (g, list) in &groups {
            vbox.append(&crate::menu::section(g));
            for c in list {
                let b = crate::menu::item("utilities-terminal-symbolic", &c.label, None);
                b.set_tooltip_text(Some(&format!(
                    "{}\n\n点一下把这行敲进本标签的终端。\n终端里正在跑前台程序时，这行字会进到那个程序。",
                    c.cmd
                )));
                let st2 = Rc::clone(st);
                let term2 = term.clone();
                let cmd = (*c).clone();
                let pop2 = pop.clone();
                b.connect_clicked(move |_| {
                    run(&st2, &term2, &cmd);
                    pop2.popdown();
                });
                vbox.append(&b);
            }
        }
    }

    vbox.append(&crate::menu::sep());
    let manage = crate::menu::item(
        "preferences-system-symbolic",
        if cmds.is_empty() {
            "新建快速命令…"
        } else {
            "管理命令…"
        },
        None,
    );
    {
        let st2 = Rc::clone(st);
        let pop2 = pop.clone();
        manage.connect_clicked(move |_| {
            pop2.popdown();
            open_manager(&st2);
        });
    }
    vbox.append(&manage);

    pop.set_child(Some(&vbox));
    pop.popup();
}

/* ---------------- 管理对话框 ---------------- */

/// 增删改快速命令。改完立刻落 settings.ini（全局共用一份）。
pub fn open_manager(st: &Rc<PanelState>) {
    let parent = st.topwin();
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        "快速命令",
        parent.as_ref(),
        &[("关闭", gtk::ResponseType::Close)],
    );
    dlg.set_default_size(560, 420);
    dlg.connect_response(|dlg, _| dlg.close());

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(10);
    root.set_margin_end(10);

    let hint = gtk::Label::new(None);
    hint.set_markup(
        "命令点下去会<b>敲进本标签的终端</b>（等同你在终端里敲一遍回车）。\n\
         分组按名字归拢；参数占位符写 ${名字} 或 ${名字:默认值}，\n\
         没有默认值的参数在发送前必填。",
    );
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    root.append(&hint);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let mk = |label: &str, tip: &str| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        b
    };
    let b_new = mk("新建", "新增一条快速命令");
    let b_edit = mk("编辑", "修改选中的命令");
    let b_del = mk("删除", "删除选中的命令");
    btns.append(&b_new);
    btns.append(&b_edit);
    btns.append(&b_del);
    root.append(&btns);
    dlg.content_area().append(&root);

    // 列表内容（增删改后重画）
    let items: Rc<RefCell<Vec<QuickCmd>>> = Rc::new(RefCell::new(settings::quick_commands()));
    let fill = {
        let list = list.clone();
        let items = Rc::clone(&items);
        move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let v = items.borrow();
            if v.is_empty() {
                let l = gtk::Label::new(Some("（空）点「新建」加第一条"));
                l.add_css_class("dim-label");
                l.set_margin_top(12);
                list.append(&l);
            }
            for c in v.iter() {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let title = gtk::Label::new(Some(&format!("[{}] {}", c.group, c.label)));
                title.set_xalign(0.0);
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                let cmd = gtk::Label::new(Some(&c.cmd));
                cmd.set_xalign(0.0);
                cmd.add_css_class("dim-label");
                cmd.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&title);
                row.append(&cmd);
                list.append(row.upcast_ref::<gtk::Widget>());
            }
        }
    };
    fill();

    let selected = move |items: &Rc<RefCell<Vec<QuickCmd>>>, list: &gtk::ListBox| -> Option<usize> {
        let row = list.selected_row()?;
        let i = row.index() as usize;
        // 「（空）」占位行不算条目
        if i < items.borrow().len() { Some(i) } else { None }
    };

    {
        let items = Rc::clone(&items);
        let fill = fill.clone();
        b_new.connect_clicked(move |_| {
            let items = Rc::clone(&items);
            let fill = fill.clone();
            edit_dialog(None, move |opt| {
                items.borrow_mut().push(opt.clone());
                let v = items.borrow().clone();
                settings::set_quick_commands(&v);
                fill();
            });
        });
    }
    {
        let items = Rc::clone(&items);
        let list2 = list.clone();
        let fill = fill.clone();
        b_edit.connect_clicked(move |_| {
            let Some(i) = selected(&items, &list2) else { return };
            let cur = items.borrow()[i].clone();
            let items2 = Rc::clone(&items);
            let fill2 = fill.clone();
            edit_dialog(Some(&cur), move |opt| {
                items2.borrow_mut()[i] = opt.clone();
                let v = items2.borrow().clone();
                settings::set_quick_commands(&v);
                fill2();
            });
        });
    }
    {
        let items = Rc::clone(&items);
        let list2 = list.clone();
        let fill = fill.clone();
        b_del.connect_clicked(move |_| {
            let Some(i) = selected(&items, &list2) else { return };
            items.borrow_mut().remove(i);
            let v = items.borrow().clone();
            settings::set_quick_commands(&v);
            fill();
        });
    }

    crate::panel::dialogs::style_dialog(&dlg);

    dlg.present();
}

/// 新建/编辑一条命令。`cur` 为 None 是新建。
fn edit_dialog<F: Fn(QuickCmd) + 'static>(cur: Option<&QuickCmd>, done: F) {
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        if cur.is_some() {
            "编辑快速命令"
        } else {
            "新建快速命令"
        },
        None::<&gtk::Window>,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("保存", gtk::ResponseType::Ok),
        ],
    );
    dlg.set_default_size(480, -1);
    let root = crate::panel::dialogs::form_box();

    let w_group = gtk::Entry::new();
    w_group.set_text(cur.map(|c| c.group.as_str()).unwrap_or(""));
    w_group.set_placeholder_text(Some("未分组"));
    let w_label = gtk::Entry::new();
    w_label.set_text(cur.map(|c| c.label.as_str()).unwrap_or(""));
    w_label.set_placeholder_text(Some("按钮上显示的名字"));
    let w_cmd = gtk::Entry::new();
    w_cmd.set_text(cur.map(|c| c.cmd.as_str()).unwrap_or(""));
    w_cmd.set_placeholder_text(Some("tail -n ${行数:100} ${路径:/var/log/syslog}"));
    w_cmd.set_hexpand(true);

    // 命令一改就把识别到的占位符报出来，省得填完保存才发现没认出来
    let ph_lbl = gtk::Label::new(None);
    ph_lbl.add_css_class("dim-label");
    ph_lbl.add_css_class("pp-hint");
    ph_lbl.set_xalign(0.0);
    ph_lbl.set_wrap(true);
    {
        let lbl = ph_lbl.clone();
        let cmd = w_cmd.clone();
        cmd.connect_changed(move |e| {
            let ph = parse_placeholders(&e.text());
            lbl.set_text(&if ph.is_empty() {
                "没有占位符：点按钮直接发送".to_string()
            } else {
                format!(
                    "参数：{}",
                    ph.iter()
                        .map(|(n, d)| if d.is_empty() {
                            format!("{n}（必填）")
                        } else {
                            format!("{n}={d}")
                        })
                        .collect::<Vec<_>>()
                        .join("、")
                )
            });
        });
        cmd.emit_by_name::<()>("changed", &[]);
    }

    root.append(&crate::panel::dialogs::form_row("分组", &w_group));
    root.append(&crate::panel::dialogs::form_row("名称", &w_label));
    root.append(&crate::panel::dialogs::form_row("命令", &w_cmd));
    root.append(&ph_lbl);

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
    let done = RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        let cmd = w_cmd.text().trim().to_string();
        let label = w_label.text().trim().to_string();
        let group = w_group.text().trim().to_string();
        dlg.destroy();
        if resp != gtk::ResponseType::Ok || cmd.is_empty() || label.is_empty() {
            return;
        }
        if let Some(f) = done.borrow_mut().take() {
            f(QuickCmd::new(&group, &label, &cmd));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 识别占位符与默认值() {
        let ph = parse_placeholders("tail -n ${行数:100} ${路径}");
        assert_eq!(ph, vec![("行数".into(), "100".into()), ("路径".into(), String::new())]);
        // 重复的只留第一次
        let ph = parse_placeholders("${a:1} ${b:2} ${a:9}");
        assert_eq!(ph, vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
        // 没有占位符
        assert!(parse_placeholders("ls -la /tmp").is_empty());
    }

    #[test]
    fn 坏占位符当普通字符() {
        // 没闭合
        assert!(parse_placeholders("echo ${abc").is_empty());
        // 空名字
        assert!(parse_placeholders("echo ${}").is_empty());
        // 名字带空格：不算占位符
        assert!(parse_placeholders("echo ${a b}").is_empty());
    }

    #[test]
    fn 转义_dollar_不误认() {
        let ph = parse_placeholders("echo $${literal}");
        assert!(ph.is_empty());
        assert_eq!(fill_placeholders("echo $${literal}", &[]), "echo ${literal}");
    }

    #[test]
    fn 填值替换() {
        assert_eq!(
            fill_placeholders("tail -n ${行数:100} ${路径}", &["50".into(), "/tmp/x.log".into()]),
            "tail -n 50 /tmp/x.log"
        );
        // 没填就用传入的值（UI 传的是默认值文本）
        assert_eq!(fill_placeholders("echo ${x}", &["hi".into()]), "echo hi");
    }

    #[test]
    fn 含空格的参数自动加引号() {
        assert_eq!(
            fill_placeholders("tail ${路径}", &["/var/log/my log".into()]),
            "tail '/var/log/my log'"
        );
        // 单引号按 shell 规则转义
        assert_eq!(fill_placeholders("echo ${x}", &["a'b".into()]), r"echo 'a'\''b'");
        // 空值给空引号，保持参数个数不被 shell 吃掉
        assert_eq!(fill_placeholders("ls ${x}", &["".into()]), "ls ''");
        // 普通路径不加引号
        assert_eq!(fill_placeholders("ls ${x}", &["/tmp".into()]), "ls /tmp");
    }

    #[test]
    fn 值里带占位符记号不会被二次展开() {
        // 填进去的 "$x" 只是字面量，不该被当占位符再解析
        assert_eq!(
            fill_placeholders("echo ${a}", &["${b}".into()]),
            "echo '${b}'"
        );
    }
}
