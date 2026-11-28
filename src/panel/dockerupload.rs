//! 容器文件页的「**上传内容**」选择窗口（阶段 192）。
//!
//! ## 为什么自己画一扇窗
//!
//! 用户 2026-11-28 的原话：「**上传这个目录和上传文件不能用同一个按钮吗？
//! 选择文件或目录，确定，然后上传？**」
//!
//! ⚠️ **GTK4 原生的文件选择器做不到「一个对话框里既选文件又选目录」**：
//! - [`gtk::FileChooserAction::Open`] 只能选**文件** —— 点到目录会**进入**那个
//!   目录（这是 GTK 的标准行为，不是 bug），拿不到目录本身；
//! - [`gtk::FileChooserAction::SelectFolder`] 只能选**目录** —— 文件根本不可选；
//! - GTK 没有「选文件**或**目录」这种动作。
//!
//! ⇒ 这一扇窗就是照用户那句话画的：
//! **上面选本地目录，下面勾选条目（文件与目录都列出来），确定后上传**。
//!
//! ## 后端不用动
//!
//! [`crate::backend::docker::upload_from`] 本来就**文件、目录都能传**
//! （走 `tar` 归档流喂给 `docker cp -` ⇒ 目录会整包上去）⇒ 这里只补「入口」。
//!
//! ## 为什么不复用 `new_tool_window`
//!
//! 容器页那几扇窗（概览 / 日志 / 文件）装的是**共享控件** ⇒ 必须「只藏不销毁」。
//! 这一扇窗的控件是**每次新建**的 ⇒ 复用它只是为了不每次重开一扇（省得窗口堆一排）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::i18n::tr;
use crate::trf;
use gtk::prelude::*;

/// 一条**本地**条目（文件 / 目录 / 软链）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickEntry {
    /// 条目名（**只有名字**，不带路径）。
    pub name: String,
    /// 是**目录**吗（软链**不算**目录 —— 它的目标是目录才算）。
    pub is_dir: bool,
    /// 是**软链**吗。
    pub is_link: bool,
    /// 字节数（目录是 0；软链按**目标**算）。
    pub size: u64,
}

/// **图标名**（Adwaita 真有的那几个，跟容器文件表同一套）。
///
/// ⚠️ 软链用「链接」图标 ⇒ 一眼分得清「这是个链接」而不是「这是个文件」。
pub fn entry_icon(e: &PickEntry) -> &'static str {
    if e.is_link {
        "insert-link-symbolic"
    } else if e.is_dir {
        "folder-symbolic"
    } else {
        "text-x-generic-symbolic"
    }
}

/// **排序键**（纯函数 ⇒ 好测）。
///
/// 规矩：**隐藏项一律沉底** → 非隐藏项里**目录在前** → 名字（不区分大小写）。
///
/// ⚠️ 为什么隐藏项沉底优先于「目录在前」：`.git` / `.env` 这类条目混在
///   中间最容易点错（看着像普通目录/文件，一勾就传错东西）。
fn sort_key(e: &PickEntry) -> (u8, u8, String, String) {
    (
        if e.name.starts_with('.') { 1 } else { 0 },
        if e.is_dir { 0 } else { 1 },
        e.name.to_lowercase(),
        e.name.clone(),
    )
}

/// 列出 `dir` 下的条目（**只一层**，不递归）。
///
/// ⚠️ 读不了（没有权限 / 不是目录）⇒ 返回**空**（界面会另外说「读不了」）。
pub fn list_entries(dir: &Path) -> Vec<PickEntry> {
    let mut out: Vec<PickEntry> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // ⚠️ `file_type()` **不跟随**软链 ⇒ `is_symlink()` 判得准。
        let ft = e.file_type().ok();
        out.push(PickEntry {
            name,
            is_dir: ft.map(|t| t.is_dir()).unwrap_or(false),
            is_link: ft.map(|t| t.is_symlink()).unwrap_or(false),
            // ⚠️ `metadata()` **跟随**软链（断链会报错 ⇒ 记 0）。
            size: e.metadata().ok().map(|m| m.len()).unwrap_or(0),
        });
    }
    out.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    out
}

/// 这个目录**读得懂**吗（不是目录 / 没权限 ⇒ false）。
pub fn can_read(dir: &Path) -> bool {
    dir.is_dir() && std::fs::read_dir(dir).is_ok()
}

/// 窗口的**内部状态**（当前目录 ＋ 列表里的每一行）。
struct PickState {
    /// 当前**本地目录**（用户正在看哪个目录）。
    dir: PathBuf,
    /// 列表里的每一行：**完整路径 ＋ 勾选框**。
    rows: Vec<(PathBuf, gtk::CheckButton)>,
}

/// 一扇「上传内容」选择窗口。
pub struct Picker {
    /// 窗口本体。
    pub win: gtk::Window,
    /// 重新列目录（换目录后要重来一遍）。
    refresh: Rc<dyn Fn()>,
    /// 内部状态（当前目录 ＋ 每一行）。
    st: Rc<RefCell<PickState>>,
    /// 「上传」按钮（测试钩子要**真点**它 ⇒ 得留句柄）。
    upload_btn: gtk::Button,
}

impl Clone for Picker {
    fn clone(&self) -> Self {
        Self {
            win: self.win.clone(),
            refresh: self.refresh.clone(),
            st: self.st.clone(),
            upload_btn: self.upload_btn.clone(),
        }
    }
}

impl Picker {
    /// **打开**（并重新列一遍目录 —— 上次看到的东西可能已经变了）。
    pub fn open(&self) {
        (self.refresh)();
        self.win.set_visible(true);
    }

    /// 当前**勾选了**的完整路径（按列表顺序）。
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        selected_paths(&self.st)
    }

    /// 当前目录下的**条目名**（按列表顺序）。
    pub fn entry_names(&self) -> Vec<String> {
        entry_names(&self.st)
    }

    /// **真点**第 `idx` 行的勾选框（`true` = 勾上，`false` = 取消）。
    ///
    /// 返回**点到了没有**（行号越界 ⇒ false）。
    pub fn check_row(&self, idx: usize, on: bool) -> bool {
        check_row(&self.st, idx, on)
    }

    /// 「上传」按钮**能点**吗（**一个都没勾** ⇒ 灰着）。
    pub fn upload_sensitive(&self) -> bool {
        self.upload_btn.is_sensitive()
    }

    /// **真点**「上传」按钮（走的跟用户点一下是**同一条路**）。
    pub fn click_upload(&self) {
        self.upload_btn.emit_clicked();
    }

    /// **换当前目录**（并重新列一遍）。
    ///
    /// ⚠️ **主要是给冒烟用的**：「浏览…」是**原生**文件对话框 ⇒ 自动化
    ///   点不了 ⇒ 冒烟靠这个钩子把窗口停到某个临时目录。
    pub fn set_dir(&self, dir: &Path) {
        self.st.borrow_mut().dir = dir.to_path_buf();
        (self.refresh)();
    }
}

/// 建一扇「上传内容」选择窗口。
///
/// - `parent`：宿主窗口（弹窗要挂在它下面 ⇒ 不然可能落在主窗**后面**）。
/// - `start_dir`：打开时停在哪个本地目录。
/// - `on_pick`：用户点「上传」时回调（带上**勾选的完整路径**）。
pub fn build_picker(
    parent: Option<&gtk::Window>,
    start_dir: &Path,
    on_pick: Rc<dyn Fn(Vec<PathBuf>)>,
) -> Picker {
    let win = gtk::Window::new();
    win.set_title(Some(tr("选择要上传的内容")));
    win.set_default_size(560, 480);
    if let Some(p) = parent {
        win.set_transient_for(Some(p));
    }

    // ---- 外框 ----
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.set_margin_start(12);
    root.set_margin_end(12);
    root.set_margin_top(10);
    root.set_margin_bottom(10);

    let hint = gtk::Label::new(Some(tr("勾选要上传的文件或目录（目录会整个传上去）")));
    hint.set_xalign(0.0);
    hint.add_css_class("dim-label");
    root.append(&hint);

    // ---- 本地目录行 ----
    let dir_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let dir_label = gtk::Label::new(None);
    dir_label.set_xalign(0.0);
    dir_label.set_hexpand(true);
    dir_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    dir_label.add_css_class("dim-label");
    let browse = gtk::Button::with_label(tr("浏览…"));
    dir_row.append(&dir_label);
    dir_row.append(&browse);
    root.append(&dir_row);

    // ---- 条目列表 ----
    let list = gtk::ListBox::new();
    // ⚠️ **不用 GTK 的「选中行」当事实来源**（阶段 89 的老坑）：
    //   这里的事实来源是**勾选框**，不是行高亮。
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));
    root.append(&scroll);

    // ---- 全选 ----
    let all = gtk::CheckButton::with_label(tr("全选"));
    root.append(&all);

    // ---- 按钮行 ----
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btns.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label(tr("取消"));
    let upload = gtk::Button::with_label(tr("上传"));
    upload.add_css_class("suggested-action");
    upload.set_sensitive(false);
    btns.append(&cancel);
    btns.append(&upload);
    root.append(&btns);

    win.set_child(Some(&root));

    // ---- 状态 ----
    let st = Rc::new(RefCell::new(PickState {
        dir: start_dir.to_path_buf(),
        rows: Vec::new(),
    }));

    /// 勾选了几项 ⇒ 更新「上传」按钮（**没勾就灰着**）。
    fn update_upload_btn(st: &Rc<RefCell<PickState>>, upload: &gtk::Button) {
        let n = st
            .borrow()
            .rows
            .iter()
            .filter(|(_, cb)| cb.is_active())
            .count();
        upload.set_sensitive(n > 0);
        let label = if n > 0 {
            trf!("上传（{n} 个）", n = n)
        } else {
            tr("上传").to_string()
        };
        upload.set_label(&label);
    }

    // ---- 重新列目录 ----
    let refresh: Rc<dyn Fn()> = {
        let st = st.clone();
        let list = list.clone();
        let dir_label = dir_label.clone();
        let upload = upload.clone();
        Rc::new(move || {
            let entries = list_entries(&st.borrow().dir);
            list.remove_all();
            let mut rows: Vec<(PathBuf, gtk::CheckButton)> = Vec::new();
            for e in &entries {
                let row = gtk::ListBoxRow::new();
                let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                let img = gtk::Image::from_icon_name(entry_icon(e));
                img.set_pixel_size(16);
                let cb = gtk::CheckButton::with_label(&e.name);
                // ⚠️ 勾选变化 ⇒ 更新「上传」按钮（**只更新，不改勾选** ⇒ 不会递归）。
                {
                    let st2 = st.clone();
                    let up2 = upload.clone();
                    cb.connect_toggled(move |_| update_upload_btn(&st2, &up2));
                }
                hb.append(&img);
                hb.append(&cb);
                row.set_child(Some(&hb));
                list.append(&row);
                rows.push((st.borrow().dir.join(&e.name), cb));
            }
            st.borrow_mut().rows = rows;
            dir_label.set_text(&st.borrow().dir.to_string_lossy());
            // ⚠️ 目录读不了 ⇒ 说清楚（别让用户以为是空目录）。
            if !can_read(&st.borrow().dir) {
                let row = gtk::ListBoxRow::new();
                let lb = gtk::Label::new(Some(tr("这个目录读不了")));
                lb.set_xalign(0.0);
                lb.add_css_class("dim-label");
                row.set_child(Some(&lb));
                list.append(&row);
            }
            update_upload_btn(&st, &upload);
        })
    };

    // ---- 「全选」 ----
    {
        let st = st.clone();
        let upload = upload.clone();
        all.connect_toggled(move |b| {
            let on = b.is_active();
            for (_, cb) in st.borrow().rows.iter() {
                cb.set_active(on);
            }
            update_upload_btn(&st, &upload);
        });
    }

    // ---- 「浏览…」（换本地目录） ----
    {
        let st = st.clone();
        let refresh = refresh.clone();
        let win2 = win.clone();
        browse.connect_clicked(move |_| {
            let mut b = gtk::FileChooserNative::builder()
                .modal(true)
                .title(tr("选本地目录"))
                .action(gtk::FileChooserAction::SelectFolder)
                .accept_label(tr("选定"))
                .cancel_label(tr("取消"));
            b = b.transient_for(&win2);
            let ch = b.build();
            let st2 = st.clone();
            let refresh2 = refresh.clone();
            ch.connect_response(move |c, r| {
                if r == gtk::ResponseType::Accept {
                    if let Some(f) = c.file() {
                        if let Some(p) = f.path() {
                            st2.borrow_mut().dir = p;
                            refresh2();
                        }
                    }
                }
            });
            ch.show();
        });
    }

    // ---- 「取消」 ----
    {
        let win2 = win.clone();
        cancel.connect_clicked(move |_| win2.set_visible(false));
    }

    // ---- 「上传」 ----
    {
        let st = st.clone();
        let win2 = win.clone();
        let on_pick = on_pick.clone();
        upload.connect_clicked(move |_| {
            let picked = selected_paths(&st);
            if picked.is_empty() {
                return;
            }
            win2.set_visible(false);
            on_pick(picked);
        });
    }

    // ---- 关闭（X / Esc）⇒ **只藏，不销毁** ----
    //
    // ⚠️ 这扇窗会被**复用**（下次点「上传」还是它）⇒ 不能销毁。
    {
        let win2 = win.clone();
        win.connect_close_request(move |w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                win2.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(key);
    }

    let p = Picker {
        win: win.clone(),
        refresh: refresh.clone(),
        st: st.clone(),
        upload_btn: upload.clone(),
    };
    p.open();
    p
}

/// 当前**勾选了**的完整路径（按列表顺序）。
fn selected_paths(st: &Rc<RefCell<PickState>>) -> Vec<PathBuf> {
    st.borrow()
        .rows
        .iter()
        .filter(|(_, cb)| cb.is_active())
        .map(|(p, _)| p.clone())
        .collect()
}

/// 当前目录下的**条目名**（按列表顺序）。
fn entry_names(st: &Rc<RefCell<PickState>>) -> Vec<String> {
    st.borrow()
        .rows
        .iter()
        .map(|(p, _)| {
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

/// **真点**第 `idx` 行的勾选框（`true` = 勾上，`false` = 取消）。
fn check_row(st: &Rc<RefCell<PickState>>, idx: usize, on: bool) -> bool {
    let found = {
        let b = st.borrow();
        b.rows.get(idx).map(|(_, cb)| cb.clone())
    };
    match found {
        Some(cb) => {
            cb.set_active(on);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, is_dir: bool, is_link: bool) -> PickEntry {
        PickEntry {
            name: name.to_string(),
            is_dir,
            is_link,
            size: 0,
        }
    }

    #[test]
    fn 目录排在文件前面() {
        let mut v = vec![
            e("a.txt", false, false),
            e("b_dir", true, false),
            e("c.txt", false, false),
        ];
        v.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        assert_eq!(
            v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["b_dir", "a.txt", "c.txt"]
        );
    }

    #[test]
    fn 隐藏项一律沉底() {
        let mut v = vec![
            e(".git", true, false),
            e("z.txt", false, false),
            e(".env", false, false),
            e("a.txt", false, false),
        ];
        v.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        // ⚠️ 隐藏项（`.git` 目录 / `.env` 文件）都在**最后** ⇒ 不容易点错。
        assert_eq!(
            v.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "z.txt", ".git", ".env"]
        );
    }

    #[test]
    fn 名字排序不区分大小写() {
        let mut v = vec![e("B.txt", false, false), e("a.txt", false, false)];
        v.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        assert_eq!(v[0].name, "a.txt");
    }

    #[test]
    fn 三种类型各有图标() {
        assert_eq!(entry_icon(&e("d", true, false)), "folder-symbolic");
        assert_eq!(entry_icon(&e("f", false, false)), "text-x-generic-symbolic");
        assert_eq!(entry_icon(&e("l", false, true)), "insert-link-symbolic");
        // ⚠️ 软链**优先**按链接画（哪怕它指向目录）。
        assert_eq!(entry_icon(&e("l", true, true)), "insert-link-symbolic");
    }

    #[test]
    fn 列目录能分出文件目录与软链() {
        let dir = std::env::temp_dir().join(format!("pp-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("建目录");
        std::fs::write(dir.join("f.txt"), b"hi").expect("写文件");
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(dir.join("f.txt"), dir.join("ln"));
        }
        let v = list_entries(&dir);
        let by = |n: &str| v.iter().find(|x| x.name == n).cloned().expect("有条目");
        assert!(by("sub").is_dir);
        assert!(!by("sub").is_link);
        assert!(!by("f.txt").is_dir);
        assert_eq!(by("f.txt").size, 2);
        #[cfg(unix)]
        {
            assert!(by("ln").is_link);
            assert!(!by("ln").is_dir);
        }
        // ⚠️ 目录排在文件前面。
        assert_eq!(v[0].name, "sub");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 读不了的目录给空表() {
        let v = list_entries(Path::new("/definitely/not/here/at/all"));
        assert!(v.is_empty());
        assert!(!can_read(Path::new("/definitely/not/here/at/all")));
    }
}
