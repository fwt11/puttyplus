//! 文件编辑器**工具栏 + 查找/替换**冒烟（阶段 84）。
//!
//! 用户 2026-10-30：「编辑器应该有一些编辑器的工具，比如搜索、替换等等。
//! 可以给编辑器弄个工具栏」。这个冒烟钉住的是**用户真能碰到的那一层**：
//!
//! 1. 工具栏真的摆在编辑器页面里（不是"建了但没挂上去"）；
//! 2. 工具栏与查找条上的按钮**点的是真按钮**（走 `click()`，不走内部
//!    函数——按钮灰了、没接上信号，钩子照样"通过"，阶段 42 踩过这个坑）；
//! 3. 查找条能开能关，匹配数算得对（**字符偏移**，中文也不偏）；
//! 4. 「下一个」真的把选区挪到匹配上，「替换 / 全部替换」真的改了正文；
//! 5. 子标签模式下**标题在工具栏里**（内层 HeaderBar 不再出现），
//!    独立窗口模式下 HeaderBar 仍是窗口标题栏；
//! 6. 全局 Ctrl+F（本来是"终端查找"）在编辑器页签里归编辑器管。
//!
//! 运行（无桌面时用 xvfb-run）：
//!
//! ```text
//! xvfb-run -a cargo run --example editor_tools_smoke
//! ```

#![allow(deprecated)]

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, fileedit, subtabs, SftpPanel};
use puttyplus::theme::{self, Theme};

/// 样例：故意让同一段文字出现多次，还混着中文和大小写。
const SAMPLE: &str =
    "abc 中文 ABC\nlet a = \"abc\";\nlet b = \"中文\";\nlet c = \"abc\";\n// 中文 abc\n";

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

/// 按钮是不是**纯图标**：child 是 Image（而不是带文字的 Box）。
fn is_icon_only(b: &gtk::Button) -> bool {
    b.child().and_downcast::<gtk::Image>().is_some()
}

/// 从「名字 → 按钮」里挑一个出来。
fn pick(list: Vec<(&'static str, gtk::Button)>, name: &str) -> gtk::Button {
    list.into_iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("没有「{name}」这个按钮"))
        .1
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Dark);

    let dir = std::env::temp_dir().join(format!("pp_editor_tools_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("sample.txt");
    std::fs::write(&file, SAMPLE).expect("sample");

    let panel = SftpPanel::new(&ConnOptions::default());
    let path = file.to_string_lossy().to_string();

    /* ---- ① 独立窗口模式：工具栏摆出来了 ---- */
    panel::fileedit::open_local(&panel.st, &path, false);
    pump(400);
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        assert!(fileedit::toolbar_in_page(e), "工具栏没有挂在编辑器页面里");
        assert!(
            !fileedit::title_in_toolbar(e),
            "独立窗口模式下标题不该在工具栏里（它在窗口标题栏上）"
        );
        // 工具栏上的按钮：纯图标 + 有 tooltip
        let btns = fileedit::toolbar_buttons(e);
        let names: Vec<&str> = btns.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "保存",
                "重载",
                "撤销",
                "重做",
                "复制",
                "粘贴",
                "查找",
                "替换",
                "跳转到行",
                "放大字号",
                "缩小字号"
            ],
            "工具栏上的按钮不是预期的那几个"
        );
        for (name, b) in &btns {
            assert!(is_icon_only(b), "{name} 应当是纯图标按钮（child 是 Image）");
            let tip = b.tooltip_text().unwrap_or_default();
            assert!(!tip.trim().is_empty(), "{name} 必须有 tooltip");
        }
        // 「自动换行」是个开关
        let wrap = fileedit::wrap_button(e);
        assert!(!wrap.is_active(), "自动换行默认应当是关的");
        assert!(
            !wrap.tooltip_text().unwrap_or_default().trim().is_empty(),
            "自动换行开关必须有 tooltip"
        );
        println!(
            "[1] 工具栏：{} 个纯图标按钮 + 自动换行开关，都带 tooltip",
            btns.len()
        );
    }

    /* ---- ①b 字号缩放：正文与行号栏必须同一档 ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        let z0 = fileedit::zoom_index(e);
        assert_eq!(
            fileedit::view_zoom_class(e),
            fileedit::gutter_zoom_class(e),
            "正文与行号栏的字号档必须一样（不然行高对不上，文件看着是歪的）"
        );
        pick(fileedit::toolbar_buttons(e), "放大字号").emit_clicked();
        pump(120);
        assert_eq!(fileedit::zoom_index(e), z0 + 1, "放大字号没生效");
        assert_eq!(
            fileedit::view_zoom_class(e),
            fileedit::gutter_zoom_class(e),
            "放大后正文与行号栏的字号档还是不一样"
        );
        pick(fileedit::toolbar_buttons(e), "缩小字号").emit_clicked();
        pump(120);
        assert_eq!(fileedit::zoom_index(e), z0, "缩小字号没生效");
        println!("[1b] 字号缩放：正文与行号栏同步换档");
    }

    /* ---- ①c 跳转到行 ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        fileedit::jump_to_line(e, 3);
        pump(120);
        assert_eq!(fileedit::cursor_line(e), 3, "跳转到行没跳到第 3 行");
        println!("[1c] 跳转到行：光标落到第 3 行");
    }

    /* ---- ①d 复制 / 粘贴 / 撤销（走系统剪贴板） ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        let before = fileedit::buffer_text(e);
        // 选中开头三个字符 "abc"，复制，再粘到末尾
        fileedit::select_chars(e, 0, 3);
        pump(60);
        assert_eq!(fileedit::selected_text(e), "abc", "选中不对");
        pick(fileedit::toolbar_buttons(e), "复制").emit_clicked();
        pump(120);
        // 光标挪到最后再粘
        let total = before.chars().count() as i32;
        fileedit::select_chars(e, total, total);
        pick(fileedit::toolbar_buttons(e), "粘贴").emit_clicked();
        pump(120);
        let after = fileedit::buffer_text(e);
        assert_eq!(after, format!("{before}abc"), "粘贴没把复制的内容贴上");
        // 撤销：应当回到原样（顺带验了「撤销」按钮）
        pick(fileedit::toolbar_buttons(e), "撤销").emit_clicked();
        pump(120);
        assert_eq!(
            fileedit::buffer_text(e),
            before,
            "撤销没把粘贴撤掉（撤销按钮是灰的？连上了？）"
        );
        println!("[1d] 复制 / 粘贴 / 撤销：剪贴板往返正常，撤销能撤掉");
    }

    /* ---- ② 查找条：开 → 查 → 跳 → 关 ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        assert!(!fileedit::find_bar_visible(e), "查找条默认不该是开着的");
        // 点**真按钮**
        pick(fileedit::toolbar_buttons(e), "查找").emit_clicked();
        pump(120);
        assert!(fileedit::find_bar_visible(e), "点了「查找」查找条没出来");
        assert!(!fileedit::replace_row_visible(e), "只按查找不该展开替换行");
        println!("[2] 点「查找」→ 查找条打开（替换行没收）");

        // 查 "abc"（区分大小写）：样例里有 4 处
        // （第 1 行行首、第 2 行、第 4 行、第 5 行行尾）
        fileedit::set_find_query(e, "abc");
        pump(120);
        assert_eq!(fileedit::match_count(e), 4, "「abc」的匹配数不对");
        assert_eq!(fileedit::count_text(e), "4 处", "计数标签不对");

        // 「下一个」在查找条里（不是工具栏）——点真按钮
        pick(fileedit::findbar_buttons(e), "下一个").emit_clicked();
        pump(120);
        assert_eq!(fileedit::selected_text(e), "abc", "选区没落在匹配上");
        assert_eq!(fileedit::count_text(e), "1/4", "计数没跟着走");
        pick(fileedit::findbar_buttons(e), "下一个").emit_clicked();
        pump(120);
        assert_eq!(fileedit::count_text(e), "2/4", "第二个匹配没选中");
        println!("[3] 「下一个」把选区挪到了匹配上（1/4 → 2/4）");

        // 大小写不敏感：ABC 也算数（5 处）
        let case = fileedit::case_button(e);
        case.set_active(false);
        pump(120);
        assert_eq!(fileedit::match_count(e), 5, "不区分大小写时匹配数不对");
        case.set_active(true);
        pump(120);
        assert_eq!(fileedit::match_count(e), 4, "区分大小写时匹配数不对");

        // 中文照样能找（字符偏移，不是字节偏移）
        fileedit::set_find_query(e, "中文");
        pump(120);
        assert_eq!(fileedit::match_count(e), 3, "「中文」的匹配数不对");
        pick(fileedit::findbar_buttons(e), "下一个").emit_clicked();
        pump(120);
        assert_eq!(fileedit::selected_text(e), "中文", "中文匹配没选中");
        println!("[4] 中文匹配正常（字符偏移没错位）");
    }

    /* ---- ③ 替换：替换一个 / 全部替换 ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        fileedit::set_find_query(e, "abc");
        fileedit::set_replace_text(e, "XYZ");
        pump(120);
        assert_eq!(fileedit::match_count(e), 4);
        // 替换当前这一个
        pick(fileedit::findbar_buttons(e), "替换").emit_clicked();
        pump(120);
        let text = fileedit::buffer_text(e);
        assert_eq!(
            text.matches("XYZ").count(),
            1,
            "「替换」应当只换一个：现在是 {text:?}"
        );
        assert_eq!(text.matches("abc").count(), 3, "还剩三处没换");
        // 全部替换
        pick(fileedit::findbar_buttons(e), "全部替换").emit_clicked();
        pump(120);
        let text = fileedit::buffer_text(e);
        assert_eq!(text.matches("XYZ").count(), 4, "「全部替换」没换完");
        assert_eq!(text.matches("abc").count(), 0, "还有没换掉的");
        println!("[5] 替换 / 全部替换都真的改了正文");
    }

    /* ---- ④ 关闭查找条 ---- */
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器");
        pick(fileedit::findbar_buttons(e), "关闭").emit_clicked();
        pump(120);
        assert!(!fileedit::find_bar_visible(e), "查找条没收起来");
        println!("[6] 查找条能收起来");
    }

    /* ---- ⑤ 子标签模式：标题进工具栏，内层 HeaderBar 不出现 ---- */
    {
        // 给面板装上子标签容器 ⇒ 之后打开的编辑器走"子标签"那条路
        *panel.st.sub_tabs.borrow_mut() = Some(subtabs::SubTabs::new());
        let file2 = dir.join("second.txt");
        std::fs::write(&file2, "第二个文件\n").expect("second");
        let path2 = file2.to_string_lossy().to_string();
        panel::fileedit::open_local(&panel.st, &path2, false);
        pump(400);
        let edits = panel.st.file_edits.borrow();
        let e = edits
            .iter()
            .find(|e| fileedit::path_of(e).ends_with("second.txt"))
            .expect("第二个编辑器");
        assert!(
            fileedit::title_in_toolbar(e),
            "子标签模式下标题应当在工具栏里"
        );
        assert!(
            fileedit::toolbar_in_page(e),
            "子标签模式下工具栏也该在页面里"
        );
        // 全局 Ctrl+F（终端查找那个键）在这个场景下要交给编辑器
        assert!(
            fileedit::handle_find_key(&panel.st),
            "全局 Ctrl+F 应当被活动编辑器接走"
        );
        pump(120);
        assert!(
            fileedit::find_bar_visible(e),
            "全局 Ctrl+F 之后查找条没打开"
        );
        println!("[7] 子标签模式：标题进工具栏，全局 Ctrl+F 归编辑器");
    }

    panel::shutdown(&panel.st);
    let _ = std::fs::remove_dir_all(&dir);
    println!("editor_tools_smoke: OK");
}
