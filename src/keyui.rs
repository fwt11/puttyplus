//! 私钥管理器窗口（2.4）：列出 ~/.ssh 里的密钥、生成新密钥、PPK → OpenSSH。
//!
//! 入口在「工具」菜单（阶段 77b 之前在连接管理器底部）——私钥是连接的一部分，
//! 放一起比再开一个菜单顺。
//!
//! 能力边界写在提示里：PPK **只读不写**（ssh-key 0.7 只实现了 PPK 解析），
//! 所以能做的是"把 .ppk 转成 OpenSSH"，不能反过来。

use gtk::prelude::*;

use crate::keymgr::{self, GenAlgo};
use crate::i18n::tr;
use crate::trf;

pub fn open(parent: Option<&gtk::Window>) {
    let parent = parent.cloned();
    // 阶段 67（审计 P2-17）：私钥管理器是**工具窗**——
    // 非模态 + **单例**（连开两次不再多一个）+ 记尺寸。
    crate::windows::open_or_focus(crate::windows::ToolKind::Key, || {
        let dlg = gtk::Window::new();
        dlg.set_title(Some(crate::windows::ToolKind::Key.title()));
        if let Some(p) = parent.as_ref() {
            dlg.set_transient_for(Some(p));
        }
        // ⚠️ **不设 modal**：工具窗不阻塞主窗口
        let (kw, kh) = crate::settings::window_size("key").unwrap_or((620, 440));
        dlg.set_default_size(kw, kh);

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
        vbox.set_margin_top(10);
        vbox.set_margin_bottom(10);
        vbox.set_margin_start(10);
        vbox.set_margin_end(10);

        // ---- 技术说明（**折叠**）----
        //
        // ⚠ 阶段 78：这段技术说明以前**一直摊在窗口顶上**，把真正要用的
        // 列表挤到下面去；用户第一眼看到的是一段实现细节，而不是
        // 「你的密钥在这儿」⇒ 折起来，标题写「技术说明」，想看再点开。
        let exp = gtk::Expander::builder()
            .label(tr("技术说明"))
            .expanded(false)
            .build();
        let hint = gtk::Label::new(Some(
        tr("生成 ed25519 / RSA 密钥，或把 PuTTY 的 .ppk 转成 OpenSSH 格式。\n\
         私钥一律以 0600 写入；口令可选（留空则不加密，文件权限是唯一防线）。\n\
         注：PPK 只能读不能写（底层库只实现了 PPK 解析），转换方向是 .ppk → OpenSSH。\n\
         旧式 PEM（RSA/EC/PKCS#8）可用「导入/转换…」原地转 OpenSSH；带口令的 PEM 需手动 ssh-keygen。"),
    ));
        hint.add_css_class("dim-label");
        hint.set_xalign(0.0);
        // 这行提示原来既用了「\\n」（Rust 里是字面量反斜杠 n，不是换行），
        // 又没开 wrap —— 结果窗口被这段文字撑到 1712px 宽，正文里还挂着 `\n`
        hint.set_wrap(true);
        hint.set_max_width_chars(64);
        exp.set_child(Some(&hint));
        vbox.append(&exp);

        // ---- 空态（独立于列表的区域）----
        //
        // ⚠️ **以前空态是往 ListBox 里塞一个 Label** ⇒ GTK 把它当成
        // ListBoxRow 处理 ⇒ 它**会被选中**（审计 P1-16 说的"被选中的
        // 『~/.ssh 不存在』蓝条"）——用户看着像"有一项被选中了"，其实
        // 那不是一项。
        //
        // 现在空态是**列表外面的一块独立面板**：不参与选择，有话说清，
        // 再给一个**主 CTA**（生成第一对密钥）。
        //
        // ⚠️ 上下各放一个「可伸展的撑子」，内容才**垂直居中**：
        // 只给面板设 valign 没用——Box 里不伸展的子控件只拿到自然高度，
        // 多出来的空间全落在末尾（内容就贴着顶边了）。
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.set_halign(gtk::Align::Center);
        empty.set_vexpand(true);
        let sp_top = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sp_top.set_vexpand(true);
        let sp_bot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sp_bot.set_vexpand(true);
        empty.append(&sp_top);
        // ⚠️ 阶段 80：空态走**共用**的构件（标题 + 解释 + 一个主 CTA），
        // 别的地方的空态也是这个形状 ⇒ 一份规范、一处改。
        let (content, b_first) = {
            let es = crate::emptystate::keys(keymgr::ssh_dir().is_some());
            crate::emptystate::build(&es)
        };
        empty.append(&content);
        empty.append(&sp_bot);

        // ---- 密钥列表 ----
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        let sw = gtk::ScrolledWindow::new();
        sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        sw.set_vexpand(true);
        sw.set_child(Some(&list));

        // 列表与空态**二选一**：空态不占列表的位置（也不参与选择）
        //
        // ⚠️ stack 自己也要 vexpand：不然它只拿到自然高度，
        // 空态面板就**撑不满**（内容贴着顶边，垂直居中不生效）。
        let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.set_vexpand(true);
        stack.append(&empty);
        stack.append(&sw);
        vbox.append(&stack);

        // 记下列表与空态（**弱引用**）：测试钩子要能重画列表。
        // ⚠ 弱引用：抓着强引用就是「窗口 → 登记表 → 列表」，窗口永远回收不掉。
        LIVE.with(|x| {
            *x.borrow_mut() = Some((list.downgrade(), empty.downgrade()));
        });

        // ---- 生成区 ----
        // 两行而不是一行：五个控件挤一行时，620px 宽只够把 placeholder 截成
        // "文件名，…"、"注释，如…"，用户看不出这一格到底是什么
        let algo = gtk::ComboBoxText::new();
        for a in GenAlgo::ALL {
            algo.append(Some(&format!("{:?}", a)), a.label());
        }
        algo.set_active_id(Some("Ed25519"));
        algo.set_tooltip_text(Some(tr("RSA 4096 生成较慢，但对老堡垒机兼容性最好")));
        let stem = gtk::Entry::new();
        stem.set_placeholder_text(Some(tr("文件名（可留空）")));
        stem.set_hexpand(true);
        let comment = gtk::Entry::new();
        comment.set_placeholder_text(Some(tr("注释（可留空）")));
        comment.set_hexpand(true);
        let pw = gtk::Entry::new();
        pw.set_visibility(false);
        pw.set_placeholder_text(Some(tr("口令（可留空）")));
        pw.set_hexpand(true);
        let b_gen = gtk::Button::with_label(tr("生成"));
        b_gen.add_css_class("suggested-action");

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&algo);
        bar.append(&stem);
        bar.append(&b_gen);
        vbox.append(&bar);

        let bar2 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar2.append(&comment);
        bar2.append(&pw);
        vbox.append(&bar2);

        // ---- 导入/转换（阶段 78：三个按钮收成一个）----
        //
        // ⚠ 以前是「选 .ppk…」+「转为 OpenSSH」+「转换 PEM…」三个控件：
        // 用户得先想清楚"我这份文件是哪一种"，再按对应那个按钮——
        // 认错格式就点错按钮，点错就报一句看不懂的错。
        // ⇒ 一个按钮：挑文件，**按文件头自己认**，认出来再问必要的口令。
        let row2 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let conv = gtk::Button::with_label(tr("导入/转换…"));
        conv.set_tooltip_text(Some(
            tr("挑一个私钥文件：PuTTY 的 .ppk 会另存一份 OpenSSH 私钥（同名）；\
             旧式 PEM 原地转 OpenSSH（先备份）；已经是 OpenSSH 的会告诉你不用转。"),
        ));
        row2.append(&conv);
        vbox.append(&row2);

        dlg.set_child(Some(&vbox));
        // 底部：关闭按钮（工具窗没有 Dialog 的 action area）
        //
        // ⚠️ 闭包只抓**弱引用**（强引用 = 引用环 ⇒ 窗口回收不掉）
        {
            let winw = dlg.downgrade();
            let btn_close = gtk::Button::with_label(tr("关闭"));
            btn_close.connect_clicked(move |_| {
                if let Some(w) = winw.upgrade() {
                    w.close();
                }
            });
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            bar.set_halign(gtk::Align::End);
            bar.append(&btn_close);
            vbox.append(&bar);
        }
        // 空态的「生成第一对密钥」⇒ 聚焦生成区的文件名输入框
        //
        // ⚠️ **弱引用**：闭包抓着 Entry 的强引用就是
        // 按钮 → 闭包 → Entry，而 Entry 在窗口里 ⇒ 窗口永远回收不掉。
        {
            let target: std::cell::RefCell<Option<glib::WeakRef<gtk::Entry>>> =
                std::cell::RefCell::new(None);
            *target.borrow_mut() = Some(stem.downgrade());
            b_first.connect_clicked(move |_| {
                if let Some(e) = target
                    .borrow()
                    .as_ref()
                    .and_then(|w| w.upgrade())
                {
                    e.grab_focus();
                }
            });
        }

        // 首次装载：列表 / 空态二选一
        rebuild(list.clone(), &empty);

        // 生成
        {
            let d2 = dlg.clone();
            let l1 = list.clone();
            let e1 = empty.clone();
            b_gen.connect_clicked(move |_| {
            let algo = match algo.active_id().as_deref() {
                Some("Ed25519") => GenAlgo::Ed25519,
                Some("Rsa2048") => GenAlgo::Rsa2048,
                Some("Rsa4096") => GenAlgo::Rsa4096,
                _ => GenAlgo::EcdsaP256,
            };
            let name = stem.text().trim().to_string();
            let name = if name.is_empty() {
                match algo {
                    GenAlgo::Ed25519 => "id_ed25519",
                    GenAlgo::Rsa2048 => "id_rsa",
                    GenAlgo::Rsa4096 => "id_rsa4096",
                    GenAlgo::EcdsaP256 => "id_ecdsa",
                }
                .to_string()
            } else {
                name
            };
            let pw_txt = pw.text().to_string();
            let pw_opt = (!pw_txt.is_empty()).then_some(pw_txt.as_str());
            let comment_txt = comment.text().trim().to_string();
            let fallback_comment = std::env::var("USER").unwrap_or_default();
            let comment_arg: &str = if comment_txt.is_empty() {
                fallback_comment.as_str()
            } else {
                comment_txt.as_str()
            };
            match generate_key(algo, &name, pw_opt, comment_arg) {
                Ok((priv_p, pub_p)) => {
                    show_generated(
                        Some(d2.upcast_ref::<gtk::Window>()),
                        &priv_p,
                        &pub_p,
                    );
                    rebuild(l1.clone(), &e1);
                }
                Err(e) => {
                    crate::panel::dialogs::show_error(Some(d2.upcast_ref::<gtk::Window>()), tr("生成失败"), &e);
                }
            }
        });
        }

        // 导入/转换：挑一个文件 → 按文件头认格式 → 走对应的转换
        {
            let d2 = dlg.clone();
            let l1 = list.clone();
            let e1 = empty.clone();
            conv.connect_clicked(move |_| {
                // ⚠ connect_clicked 的回调是 **Fn**（能触发多次），
                // 所以在里面 clone 一份再交给一次性的回调。
                let d3 = d2.clone();
                let d4 = d3.clone();
                let l2 = l1.clone();
                let e2 = e1.clone();
                pick_file(d3.upcast_ref::<gtk::Window>(), move |p| {
                    import_or_convert(d4.upcast_ref::<gtk::Window>(), p, l2.clone(), e2.clone());
                });
            });
        }

        // ⚠️ 尺寸由 `windows::open_or_focus` 统一记
        dlg
    });
    // ⚠️ 窗口 present **之后**再探一次现状。
    //
    // GTK 的 `is_visible()` 会一路问父控件的 visible 标志，而 build() 跑
    // 的时候窗口**还没 present** ⇒ 那时探到的「空态可见吗」全是 false，
    // 冒烟会误判成"空态没显示"。
    refresh();
}

/// 异步选文件。
///
/// GTK4 没有 `Dialog::run()`：建完必须 `present()`，在 response 回调里取文件。
/// 旧实现建完立刻 `dlg.file()`（此时用户还没选，永远是 None）再 destroy，
/// 所以「选 .ppk…」「转换 PEM…」点了没有任何反应。
fn pick_file<F>(parent: &gtk::Window, done: F)
where
    F: FnOnce(std::path::PathBuf) + 'static,
{
    // ⚠️ 走 `windows::file_chooser_dialog`：统一 **modal + 有父窗口**
    // （非模态的选择器在模态框开着时会**点不动**——用户 2026-11-14 报的 bug）。
    let dlg = crate::windows::file_chooser_dialog(
        tr("选择私钥文件"),
        Some(parent),
        gtk::FileChooserAction::Open,
    );
    dlg.add_button(tr("取消"), gtk::ResponseType::Cancel);
    dlg.add_button(tr("选择"), gtk::ResponseType::Accept);
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(tr("私钥（OpenSSH / PPK / PEM）")));
    filter.add_pattern("id_*");
    filter.add_pattern("*.pem");
    filter.add_pattern("*.ppk");
    dlg.add_filter(&filter);
    if let Some(dir) = keymgr::ssh_dir() {
        dlg.set_current_folder(Some(&gtk::gio::File::for_path(dir)));
    }
    // connect_response 是 Fn（可能多次触发），FnOnce 的回调放进 RefCell 里 take
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        if resp == gtk::ResponseType::Accept {
            if let Some(p) = dlg.file().and_then(|f| f.path()) {
                if let Some(cb) = done.borrow_mut().take() {
                    cb(p);
                }
            }
        }
        dlg.close();
    });
    dlg.present();
}

/// 生成一把密钥（按钮与测试钩子共用同一条路径）。
///
/// ⚠ 目录**总是** `~/.ssh`：`keymgr::ssh_dir()` 只在目录已存在时返回 Some，
/// 拿 home_dir 兜底会把密钥写到 ~ 根目录（不是 ~/.ssh）——
/// 而「~/.ssh 还不存在」恰恰是空态最常见的场景。
fn generate_key(
    algo: GenAlgo,
    stem: &str,
    passphrase: Option<&str>,
    comment: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let dir = crate::platform::paths::home().join(".ssh");
    keymgr::generate_to_files(algo, &dir, stem, passphrase, comment)
}

/// 导入/转换：按**文件头**认格式，再走对应的转换（阶段 78）。
///
/// 用户只挑一个文件，**不用自己先想清楚是哪一种**。
fn import_or_convert(
    parent: &gtk::Window,
    path: std::path::PathBuf,
    list: gtk::ListBox,
    empty: gtk::Box,
) {
    let parent = parent.clone();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            crate::panel::dialogs::show_error(
                Some(&parent),
                tr("读不了这个文件"),
                &trf!("读取 {v0} 失败：{e}", e = e, v0 = path.display()),
            );
            return;
        }
    };
    match keymgr::detect_format(&data) {
        keymgr::KeyFormat::OpenSsh => {
            crate::panel::dialogs::info(
                Some(&parent),
                tr("不用转换"),
                &trf!("{v0} 已经是 OpenSSH 格式的私钥，直接用就行。", v0 = path.display()),
            );
        }
        keymgr::KeyFormat::Ppk => {
            // 另存一份同名 OpenSSH 私钥（原 .ppk 不动）
            let out = path.with_extension("");
            let p2 = path.clone();
            let w = parent.clone();
            crate::panel::dialogs::ask_password(
                Some(&parent),
                tr("PPK 口令"),
                tr("若该 .ppk 有口令保护请输入（没有就留空）："),
                move |pw| {
                    let pw_opt = pw.as_deref().filter(|p| !p.is_empty());
                    match keymgr::ppk_to_openssh(&p2, pw_opt, &out, "") {
                        Ok(p) => {
                            crate::panel::dialogs::info(
                                Some(&w),
                                tr("转换完成"),
                                &trf!("已写出 OpenSSH 私钥：{v0}", v0 = p.display()),
                            );
                            rebuild(list, &empty);
                        }
                        Err(e) => crate::panel::dialogs::show_error(Some(&w), tr("转换失败"), &e),
                    }
                },
            );
        }
        keymgr::KeyFormat::LegacyPem => {
            let p2 = path.clone();
            let w = parent.clone();
            crate::panel::dialogs::confirm_with(
                Some(&parent),
                &trf!("将把\n{v0}\n原地转换成 OpenSSH 格式（先备份为 *.puttyplus.bak）。\n带口令的旧 PEM 无法在应用内转换。继续？", v0 = path.display()),
                tr("转换"),
                move |ok| {
                    if !ok {
                        return;
                    }
                    match keymgr::convert_pem_to_openssh(&p2) {
                        Ok(bak) => {
                            crate::panel::dialogs::info(
                                Some(&w),
                                tr("转换完成"),
                                &trf!("已转成 OpenSSH 格式。\n备份：{v0}", v0 = bak.display()),
                            );
                            rebuild(list, &empty);
                        }
                        Err(e) => crate::panel::dialogs::show_error(Some(&w), tr("转换失败"), &e),
                    }
                },
            );
        }
        keymgr::KeyFormat::Unknown => {
            crate::panel::dialogs::show_error(
                Some(&parent),
                tr("认不出格式"),
                &trf!("{v0} 不像私钥文件（既不是 OpenSSH，也不是 PuTTY .ppk，也不是旧式 PEM）。", v0 = path.display()),
            );
        }
    }
}

/// 生成成功：给出路径 + 两个动作（复制公钥 / 在连接中使用）。
///
/// 阶段 78：以前只弹一个「已生成」的说明框 —— 用户还得自己去找文件、
/// 自己打开文本编辑器复制公钥、再自己去连接里挑私钥，三件事全手动。
fn show_generated(
    parent: Option<&gtk::Window>,
    priv_p: &std::path::Path,
    pub_p: &std::path::Path,
) {
    let dlg = crate::windows::dialog(
        tr("已生成密钥"),
        parent,
        &[
            (tr("复制公钥"), gtk::ResponseType::Other(1)),
            (tr("在连接中使用"), gtk::ResponseType::Other(2)),
            (tr("关闭"), gtk::ResponseType::Close),
        ],
    );
    dlg.set_default_size(500, -1);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
    vbox.set_margin_top(14);
    vbox.set_margin_bottom(14);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    let body = gtk::Label::new(Some(&trf!("私钥：{v0}\n公钥：{v1}\n\n「复制公钥」把公钥内容放进剪贴板，贴到目标机的 authorized_keys；「在连接中使用」打开新建连接窗口，认证方式设成「只用私钥文件」并选中这一把。", v0 = priv_p.display(), v1 = pub_p.display())));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true);
    body.set_max_width_chars(56);
    vbox.append(&body);
    dlg.content_area().append(&vbox);

    let pub_path = pub_p.to_path_buf();
    let key_path = priv_p.to_string_lossy().into_owned();
    let pwin = parent.cloned();
    dlg.connect_response(move |d, resp| {
        match resp {
            gtk::ResponseType::Other(1) => {
                // 复制公钥内容
                if let Ok(txt) = std::fs::read_to_string(&pub_path) {
                    if let Some(cb) = gtk::gdk::Display::default().map(|d| d.clipboard()) {
                        cb.set_text(txt.trim_end());
                    }
                }
            }
            gtk::ResponseType::Other(2) => {
                // 在连接中使用：新建连接窗口，认证页预选这把私钥
                crate::connmgr::ui_new_connection_with_key(pwin.as_ref(), &key_path, move |o| {
                    if let Some(t) = crate::tabs::current_tabs() {
                        t.open(&o);
                    }
                });
            }
            _ => {}
        }
        d.destroy();
    });
    dlg.present();
}

/// 收集一个控件的所有后代（深度优先）。
fn descendants(parent: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut cur = parent.as_ref().first_child();
    while let Some(c) = cur {
        out.push(c.clone());
        out.extend(descendants(&c));
        cur = c.next_sibling();
    }
    out
}

/// 空态里的 Label 们（按顺序：标题、说明）。
fn empty_labels(empty: &gtk::Box) -> Vec<gtk::Label> {
    descendants(empty)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .collect()
}

/// 空态的说明文字（两种情况说不同的话）。
fn set_empty_desc(empty: &gtk::Box) {
    // ⚠️ 文案来自**共用**的构件（阶段 80）⇒ 别处改了这里也跟着改
    let desc = crate::emptystate::keys(keymgr::ssh_dir().is_some()).desc;
    if let Some(l) = empty_labels(empty).get(1) {
        l.set_text(&desc);
    }
}

/* ---------------- 测试钩子 ---------------- */

/// 私钥管理器窗口的探针（冒烟用）。
#[derive(Clone)]
pub struct Probe {
    /// 列表里有多少**行**（空态时必须是 0 —— 空态不是一行）。
    pub list_rows: usize,
    /// 空态面板可见吗。
    pub empty_visible: bool,
    /// 空态标题。
    pub empty_title: String,
    /// 空态说明。
    pub empty_desc: String,
    /// 主 CTA 的文字（`None` = 没有 CTA）。
    pub cta_label: Option<String>,
    /// 列表里每一行的私钥路径（空态时是空的）。
    ///
    /// 冒烟据此断言「生成后能拿到私钥路径」。
    pub key_paths: Vec<String>,
}

thread_local! {
    /// 开着的私钥管理器窗口（弱引用）：给探针用。
    static PROBE: std::cell::RefCell<Option<Probe>> = const { std::cell::RefCell::new(None) };
    /// 窗口里的列表与空态面板（弱引用）：测试钩子要能重画列表。
    ///
    /// ⚠ 弱引用：抓着强引用就是「窗口 → 闭包 → 列表」，窗口永远回收不掉。
    static LIVE: std::cell::RefCell<Option<(glib::WeakRef<gtk::ListBox>, glib::WeakRef<gtk::Box>)>> =
        const { std::cell::RefCell::new(None) };
}

/// 当前私钥管理器窗口的探针（测试钩子）。
pub fn probe() -> Option<Probe> {
    PROBE.with(|p| p.borrow().clone())
}

/// 测试钩子：重画密钥列表（冒烟在生成之后调用）。
pub fn refresh() {
    let pair = LIVE.with(|x| x.borrow().clone());
    let Some((l, e)) = pair else { return };
    let (Some(list), Some(empty)) = (l.upgrade(), e.upgrade()) else {
        return;
    };
    rebuild(list, &empty);
}

/// 测试钩子：走**和「生成」按钮一样的**路径生成一把密钥。
///
/// 不另抄一份生成逻辑 —— 否则测的就不是按钮真正走的那条路。
pub fn generate_for_test(stem: &str) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    generate_key(GenAlgo::Ed25519, stem, None, "puttyplus")
}

fn rebuild(list: gtk::ListBox, empty: &gtk::Box) {
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    // 空态与列表**二选一**：空态不是列表的一行（见上面 open() 的说明）
    let items = match keymgr::ssh_dir() {
        Some(dir) => keymgr::list_dir(&dir),
        None => Vec::new(),
    };
    if items.is_empty() {
        empty.set_visible(true);
        // 列表在下面，但空态时不显示（免得露出一个空框）
        if let Some(parent) = empty.parent() {
            if let Some(sc) = parent.downcast_ref::<gtk::Box>() {
                if let Some(child) = sc.last_child() {
                    if child.is::<gtk::ScrolledWindow>() {
                        child.set_visible(false);
                    }
                }
            }
        }
        set_empty_desc(empty);
        // ⚠ 空态**也要**刷新探针：否则探针里留着上一次的行数，
        // 冒烟会拿旧数据判「空态是不是一行」。
        refresh_probe(&list, empty, &[]);
        return;
    }
    empty.set_visible(false);
    if let Some(parent) = empty.parent() {
        if let Some(sc) = parent.downcast_ref::<gtk::Box>() {
            if let Some(child) = sc.last_child() {
                if child.is::<gtk::ScrolledWindow>() {
                    child.set_visible(true);
                }
            }
        }
    }
    let mut paths: Vec<String> = Vec::new();
    for i in items {
        paths.push(i.path.to_string_lossy().into_owned());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_start(6);
        row.set_margin_end(6);
        row.set_margin_top(3);
        row.set_margin_bottom(3);

        let name = gtk::Label::new(Some(
            &i.path.file_name().unwrap_or_default().to_string_lossy(),
        ));
        name.set_xalign(0.0);
        let mut bits = String::new();
        if let Some(b) = i.bits {
            bits = trf!(" {b}位", b = b);
        }
        let mut meta = if i.pem {
            tr("旧式 PEM").to_string()
        } else {
            format!("{}{}", i.algo, bits)
        };
        if i.encrypted {
            meta.push_str(tr(" · 有口令"));
        }
        if !i.comment.is_empty() {
            meta.push_str(&format!(" · {}", i.comment));
        }
        let meta_lbl = gtk::Label::new(Some(&meta));
        meta_lbl.add_css_class("dim-label");
        meta_lbl.set_xalign(0.0);
        meta_lbl.set_hexpand(true);
        meta_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);

        row.append(&name);
        row.append(&meta_lbl);
        let lbl = if let Some(e) = &i.error {
            let l = gtk::Label::new(Some(if i.pem { tr("PEM 格式") } else { tr("读不了") }));
            l.add_css_class(if i.pem { "dim-label" } else { "error" });
            l.set_tooltip_text(Some(e));
            l
        } else {
            let l = gtk::Label::new(Some(&i.fingerprint));
            l.add_css_class("dim-label");
            l.set_tooltip_text(Some(tr("SHA256 指纹：复制它去目标机核对，避免登错机")));
            l
        };
        row.append(&lbl);
        let lb = gtk::ListBoxRow::new();
        lb.set_child(Some(&row));
        list.append(&lb);
    }
    refresh_probe(&list, empty, &paths);
}

/// 把窗口的现状写进探针（冒烟据此断言）。
fn refresh_probe(list: &gtk::ListBox, empty: &gtk::Box, paths: &[String]) {
    let mut rows = 0;
    let mut cur = list.first_child();
    while let Some(c) = cur {
        if c.is::<gtk::ListBoxRow>() {
            rows += 1;
        }
        cur = c.next_sibling();
    }
    let labels = empty_labels(empty);
    let title = labels
        .first()
        .map(|l| l.text().to_string())
        .unwrap_or_default();
    let desc = labels
        .get(1)
        .map(|l| l.text().to_string())
        .unwrap_or_default();
    // ⚠️ CTA 现在是**图标 + 文字**的自绘按钮 ⇒ `label()` 取不到，
    // 得去它的子 Label 里找（阶段 80 改成共用构件之后）。
    let cta = descendants(empty)
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Button>().ok())
        .and_then(|b| {
            b.label()
                .map(|s| s.to_string())
                .or_else(|| {
                    descendants(&b)
                        .into_iter()
                        .find_map(|w| w.downcast::<gtk::Label>().ok())
                        .map(|l| l.text().to_string())
                })
        });
    PROBE.with(|p| {
        *p.borrow_mut() = Some(Probe {
            list_rows: rows,
            empty_visible: empty.is_visible(),
            empty_title: title,
            empty_desc: desc,
            cta_label: cta,
            key_paths: paths.to_vec(),
        })
    });
}

/// 「工具」菜单里挂的入口。
pub fn open_from_manager(parent: Option<&gtk::Window>) {
    open(parent);
}
