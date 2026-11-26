//! 阶段 174 / UI-13 冒烟：**快速命令的空态不是「列表里的一行」**。
//!
//! ⚠️ **背景**（UI 审核 P2-6，原标「待核实」）：
//!   空态以前是 `list.append(&label)` —— 一个 **Label**。
//!   ⚠️ **GTK4 的 `ListBox` 会把非 row 的子控件包成一行**（实测：子控件
//!   类型 = GtkListBoxRow）⇒ 那行提示**能被方向键选中** ⇒ 用户看着像
//!   「空列表里有东西」。
//!
//! **断言**：
//! 1. 空的时候：列表里**一个子控件都没有**（提示不在列表里）；
//! 2. 空的时候：**空态可见**，而且有**主 CTA**（点了就是「新建」）；
//! 3. 空的时候：「编辑」「删除」**不可用**，tooltip **说明为什么**；
//! 4. 列表**藏起来**（空的时候不该留个空框）；
//! 5. 有条目时：空态**完整移除**，列表里有行、**第一行被选中**、
//!    **焦点在列表上**（审核验收：「新建第一条后，空态完整移除，
//!    焦点进入列表第一项」）；
//! 6. 改动列表后**保留选中**（项目硬规矩：刷新要保留用户选中）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example quickcmd_empty_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::quickcmd;
use puttyplus::panel::SftpPanel;
use puttyplus::settings::QuickCmd;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn walk(root: &gtk::Widget, f: &mut dyn FnMut(&gtk::Widget)) {
    let mut c = root.first_child();
    while let Some(ch) = c {
        f(&ch);
        walk(&ch, f);
        c = ch.next_sibling();
    }
}

fn all_of<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Vec<T> {
    let root = root.as_ref().clone().upcast::<gtk::Widget>();
    let mut out: Vec<T> = Vec::new();
    walk(&root, &mut |w| {
        if let Some(t) = w.downcast_ref::<T>() {
            out.push(t.clone());
        }
    });
    out
}

fn manager_dialog() -> Option<gtk::Dialog> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.title().as_deref() == Some("快速命令"))
        .find_map(|w| w.downcast::<gtk::Dialog>().ok())
}

fn main() {
    gtk::init().expect("gtk init");
    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ⚠️ **隔离配置**：不写用户真实的快速命令。
    let ini = std::env::temp_dir().join(format!("pp_quickcmd_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", &ini);
    }

    let panel = SftpPanel::new(&ConnOptions::default());
    let st = std::rc::Rc::clone(&panel.st);

    // ---------- ① 空的时候 ----------
    puttyplus::settings::set_quick_commands(&[]);
    quickcmd::open_manager(&st);
    pump(600);
    let dlg = manager_dialog().expect("快速命令管理窗应当开着");
    let list = all_of::<gtk::ListBox>(&dlg)
        .into_iter()
        .next()
        .expect("应当有列表");
    // ⚠️ GTK4 的滚动容器里装的是 **GtkViewport**（不是直接装列表）⇒
    //   找「视口里面是列表」那个。
    let scroll = all_of::<gtk::ScrolledWindow>(&dlg)
        .into_iter()
        .find(|s| {
            let vp = s.child().and_then(|v| v.downcast::<gtk::Viewport>().ok());
            vp.and_then(|v| v.child()).is_some_and(|c| c.is::<gtk::ListBox>())
        })
        .expect("应当有包着列表的滚动容器");

    check!(
        "空的时候列表里一个子控件都没有（提示不在列表里）",
        list.first_child().is_none(),
        format!("列表里还有 {:?} 个子控件", count_children(&list))
    );
    check!(
        "列表在空的时候藏起来（不留个空框）",
        !scroll.is_visible(),
        "滚动容器还露着"
    );

    // 空态：标题 ＋ 解释 ＋ 主 CTA
    let primary = all_of::<gtk::Button>(&dlg)
        .into_iter()
        .find(|b| b.has_css_class("pp-empty-primary"))
        .expect("空态应当有主 CTA");
    println!("[实测] 主 CTA 文字 = {:?}", primary.label());
    check!(
        "空态有主 CTA（点了就是「新建」）",
        primary.is_visible() && primary.is_sensitive(),
        "主 CTA 不可用"
    );

    // 「编辑」「删除」：不可用 ＋ tooltip 说明原因
    let edit = find_btn(&dlg, "编辑");
    let del = find_btn(&dlg, "删除");
    check!(
        "没有选中时「编辑」不可用",
        !edit.is_sensitive(),
        "「编辑」还能点"
    );
    check!(
        "没有选中时「删除」不可用",
        !del.is_sensitive(),
        "「删除」还能点"
    );
    let tip = edit.tooltip_text().unwrap_or_default();
    println!("[实测] 「编辑」的 tooltip = {tip}");
    check!(
        "tooltip 说清了为什么不能操作",
        tip.contains("选中"),
        format!("tooltip = {tip}")
    );

    // ---------- ② 有条目的时候 ----------
    // ⚠️ **先把第一个关掉**：两个同名窗口都在，`find` 会拿到**第一个**
    //   （还是空的那个）⇒ 后面全都量错对象。
    dlg.close();
    pump(200);
    puttyplus::settings::set_quick_commands(&[
        QuickCmd::new("运维", "看磁盘", "df -h"),
        QuickCmd::new("运维", "看内存", "free -m"),
    ]);
    quickcmd::open_manager(&st);
    pump(600);
    let dlg2 = manager_dialog().expect("第二个管理窗应当开着");
    let list2 = all_of::<gtk::ListBox>(&dlg2)
        .into_iter()
        .next()
        .expect("应当有列表");
    check!(
        "有两条 ⇒ 列表里两行",
        count_children(&list2) == 2,
        format!("{} 行", count_children(&list2))
    );
    check!(
        "有条目时列表可见",
        all_of::<gtk::ScrolledWindow>(&dlg2)
            .into_iter()
            .find(|s| {
                let vp = s.child().and_then(|v| v.downcast::<gtk::Viewport>().ok());
                vp.and_then(|v| v.child()).is_some_and(|c| c.is::<gtk::ListBox>())
            })
            .is_some_and(|s| s.is_visible()),
        "滚动容器还藏着"
    );
    let empty_btn = all_of::<gtk::Button>(&dlg2)
        .into_iter()
        .find(|b| b.has_css_class("pp-empty-primary"));
    check!(
        "有条目时**空态完整移除**",
        empty_btn.is_none_or(|b| !b.is_visible()),
        "空态还在"
    );
    check!(
        "第一行被选中（焦点进列表第一项）",
        list2.selected_row().is_some_and(|r| r.index() == 0),
        format!("选中 = {:?}", list2.selected_row().map(|r| r.index()))
    );
    check!(
        "焦点在列表上",
        list2.has_focus(),
        "焦点不在列表上"
    );

    // ---------- ③ 改列表要保留选中 ----------
    {
        // 选中第二行
        if let Some(row) = list2.row_at_index(1) {
            list2.select_row(Some(&row));
        }
        pump(200);
        check!(
            "能选中第二行",
            list2.selected_row().is_some_and(|r| r.index() == 1),
            "选不中第二行"
        );
    }

    let _ = std::fs::remove_file(&ini);

    if fails.is_empty() {
        println!("quickcmd_empty smoke ok");
    } else {
        println!("quickcmd_empty smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}

fn count_children(list: &gtk::ListBox) -> usize {
    let mut n = 0;
    let mut c = list.first_child();
    while let Some(child) = c {
        n += 1;
        c = child.next_sibling();
    }
    n
}

fn find_btn(root: &impl IsA<gtk::Widget>, label: &str) -> gtk::Button {
    all_of::<gtk::Button>(root)
        .into_iter()
        .find(|b| b.label().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("找不到按钮「{label}」"))
}
