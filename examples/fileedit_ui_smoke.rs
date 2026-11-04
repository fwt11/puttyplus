//! 内置查看/编辑器 GUI 冒烟：构造一个真实的 SFTP 面板（不连接远端），
//! 在本地列表里选中临时文件，走文件面板的「打开」入口，确认编辑器窗口
//! 能建出来且不崩溃；并核对**行号栏**与**语法高亮**确实生效。
//!
//! **含一个"卡死检测"用例**：打开带代码围栏的 `.md`（2026-10-26 办公机
//! 冻结的复现样本）。主线程一旦原地打转，GTK 主循环就再也回不来——
//! 所以例子带一个**看门狗线程**：超时自己报错退出，把"冻住"变成"失败"。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example fileedit_ui_smoke
//!   xvfb-run -a cargo run --example fileedit_ui_smoke -- --hold   # 停留 20 秒供截图
//!   xvfb-run -a cargo run --example fileedit_ui_smoke -- light    # 浅色主题
//!
//! 它不断言内容编辑/保存（那需要真实键盘事件）；远端读写的正确性由
//! `fileedit_smoke` 对真实 sshd 验证，纯逻辑由 `cargo test` 覆盖。

#![allow(deprecated)] // 文件面板沿用 TreeView/ListStore（与主程序一致）

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel, fileedit};
use puttyplus::theme::{self, Theme};

/// 带中文注释的样例：中文行走字体回退、行高与 ASCII 行不同，
/// 正是行号栏行高对齐（影子字符）要解决的场景。
const SAMPLE: &str = r#"// 示例文件：内置查看/编辑器的行号与语法高亮
use std::collections::HashMap;

/// 一个普通的中文注释，测试行号会不会歪
fn main() {
    // 字符串与数字
    let name = "PuTTYPlus";
    let count = 42;
    let mut map: HashMap<String, i32> = HashMap::new();
    map.insert(name.to_string(), count);
    println!("{name} -> {count}");

    /* 块注释
       跨好几行 */
    let list = vec![1, 2, 3];
    let total: i32 = list.iter().sum();
    println!("total = {total}");

    // 再来一段中文，确认行号在长文件里也不歪
    let 名字 = "中文变量也能看";
    println!("{名字}");
}
"#;

/// Python 样例：开头是中文文档字符串——用来验「中文字符会让字节偏移与
/// 字符偏移错位」这个坑，以及 Python 关键字真的上了色。
const SAMPLE_PY: &str = r#""""WiFi 推送文件到电脑 — 绕开串口瓶颈。

用法:
  1. 改下面的 SSID / PWD
"""
import socket, time, os
from network import WLAN, STA_IF

SSID = "TPLINK6377"
PORT = 8266

wlan = WLAN(STA_IF)
wlan.active(True)
if not wlan.isconnected():
    wlan.connect(SSID, "pwd")
"#;

/// Markdown 样例：**含代码围栏**。
///
/// 这是 2026-10-26 办公机"编辑文件时整个软件冻住"的复现样本：围栏分支
/// 把游标停在 '\n' 上 → 下一轮取行尾又回到同一个下标 → 原地打转的无限
/// 循环；而高亮跑在 GTK 主线程上，一进去窗口就冻住（CPU 100%、零
/// syscall，用户只能杀进程）。修复前这个冒烟会**卡死在这一步**。
const SAMPLE_MD: &str = r#"# 标题

正文 `code` 与 **粗体**。

```rust
fn main() {
    println!("围栏里的代码不该把程序冻住");
}
```

结尾一行。
"#;

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

/// 看门狗：万一编辑器又把 GTK 主线程卡死（这个冒烟要防的正是这个），
/// 进程不能无限挂着——超时自己报错退出，而不是让人干等。
///
/// 看门狗跑在**另一个线程**上，所以主线程原地打转时它照样醒着。
fn watchdog(seconds: u64) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&done);
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        eprintln!("[看门狗] {seconds} 秒没跑完 —— 多半是主线程又原地打转卡死了");
        std::process::exit(2);
    });
    done
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hold = args.iter().any(|a| a == "--hold");
    // --view：以只读模式打开（只读窗口不该有保存按钮）
    let view = args.iter().any(|a| a == "--view");
    let th = if args.iter().any(|a| a == "light") {
        Theme::Light
    } else if args.iter().any(|a| a == "contrast") {
        Theme::HighContrast
    } else {
        Theme::Dark
    };
    let dir = std::env::temp_dir().join(format!("pp_fileedit_ui_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("sample.rs");
    std::fs::write(&file, SAMPLE).expect("sample file");
    let py_file = dir.join("sample.py");
    std::fs::write(&py_file, SAMPLE_PY).expect("sample py file");
    let md_file = dir.join("sample.md");
    std::fs::write(&md_file, SAMPLE_MD).expect("sample md file");
    // 阶段 23：双击打开编辑器的样本（**单独一个文件**，双击它必然新开一扇窗）
    let dbl_file = dir.join("dblclick.txt");
    std::fs::write(&dbl_file, "双击我，应当打开编辑器。\n").expect("dblclick file");
    // 长文件：超过一屏（用户报的「在底部输入就跳到顶部」就是它）
    let long_file = dir.join("long.txt");
    let long_text: String = (1..=400)
        .map(|i| format!("第 {i} 行：这是一份很长的文件，用来复现跳顶。\n"))
        .collect();
    std::fs::write(&long_file, &long_text).expect("long file");

    // 看门狗：主线程要是原地打转（历史 bug），这里会替我们喊出来
    let done = watchdog(if hold { 90 } else { 60 });

    // 面板启动时读这个环境变量作为本地起始目录
    unsafe { std::env::set_var("PUTTYPLUS_LOCALDIR", &dir) };

    gtk::init().expect("gtk init");
    theme::apply(th);

    let panel = SftpPanel::new(&ConnOptions::default());
    let win = gtk::Window::new();
    win.set_title(Some("fileedit ui smoke"));
    win.set_default_size(980, 640);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(300);

    // 在本地面板里选中某个文件（首行是 ".."）
    // 注意：面板是多选，`select_path` 不会取消之前的选中——先全取消，
    // 否则「打开」会因为"选中了两个文件"而拒绝打开。
    let pane = panel.st.pane(false);
    let select = |name: &str| -> bool {
        if let Some(mut it) = pane.model.iter_first() {
            loop {
                let n: String = pane.model.get(&it, puttyplus::panel::pane::COL_NAME);
                if n == name {
                    let path = pane.model.path(&it);
                    pane.view.selection().unselect_all();
                    pane.view.selection().select_path(&path);
                    return true;
                }
                if !pane.model.iter_next(&mut it) {
                    break;
                }
            }
        }
        false
    };
    assert!(select("sample.rs"), "本地面板里没找到 sample.rs");
    pump(100);

    panel::on_open_file(&panel.st, false, view);
    pump(500);
    let n = panel.st.file_edits.borrow().len();
    println!("editor windows: {n}");
    assert_eq!(n, 1, "「打开」没有打开编辑器窗口");

    // 只读窗口（查看模式）里不该摆保存按钮：看的东西存不回去
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器窗口");
        if view {
            assert!(
                !fileedit::save_btn_visible(e),
                "查看模式里还摆着保存按钮"
            );
        } else {
            assert!(
                fileedit::save_btn_visible(e),
                "编辑模式里保存按钮不见了"
            );
        }
    }

    // 同一路径再点一次应聚焦原窗，而不是开第二个（两个窗口会互相覆盖）
    if !view {
        panel::on_open_file(&panel.st, false, true);
        pump(300);
        assert_eq!(
            panel.st.file_edits.borrow().len(),
            1,
            "同一文件被打开了两个窗口"
        );
    }

    // —— 行号与语法高亮 ——
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器窗口");
        assert_eq!(
            fileedit::lang_of(e),
            puttyplus::panel::syntax::Lang::Rust,
            "没认出 Rust"
        );
        let gtext = fileedit::gutter_text(e);
        let glines: Vec<&str> = gtext.split('\n').collect();
        let total = SAMPLE.split('\n').count();
        assert_eq!(glines.len(), total, "行号行数与正文行数对不上");
        // 行号是定宽右对齐的，后面可能跟着透明的影子字符，只取行首数字
        let leading_num = |l: &str| -> String {
            let t = l.trim_start();
            let end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
            t[..end].to_string()
        };
        assert_eq!(leading_num(glines[0]), "1", "第一行行号不对");
        assert_eq!(
            leading_num(glines[total - 1]),
            total.to_string(),
            "最后一行行号不对"
        );
        assert_eq!(
            leading_num(glines[3]),
            "4",
            "含中文的行行号不对"
        );
        println!("gutter lines: {total}，语言：{:?}", fileedit::lang_of(e));
    }

    // —— 长文件：在底部敲字**不能跳到顶部** ——
    //
    // 用户实测：文件超过一屏时，在底部输入会突然跳到文件头 ⇒ 没法继续改。
    // 根因：行号栏每次敲字都会重建（`rebuild_gutter` → `set_text`），而
    // `set_text` 把行号栏的滚动条归零；行号栏 ↔ 正文是**双向**同步的
    // ⇒ 归零顺带把正文也拽到顶部。
    {
        assert!(select("long.txt"), "本地面板里没找到 long.txt");
        pump(100);
        panel::on_open_file(&panel.st, false, view);
        pump(500);
        let edits = panel.st.file_edits.borrow();
        let e = edits
            .iter()
            .find(|e| fileedit::path_of(e).ends_with("long.txt"))
            .expect("长文件编辑器窗口");
        let e = e.clone();
        // 光标 + 视图都挪到文件尾部（模拟用户已经滚到下面在改）
        fileedit::jump_to_line(&e, 380);
        pump(200);
        let before = fileedit::scroll_offset(&e);
        assert!(
            before > 0.0,
            "跳到第 380 行后视图没跟着滚下去（scroll={before}）——这一步就没法测跳顶"
        );
        // 在底部敲一个字（走真的 buffer 变更 ⇒ 真的会重建行号栏）
        assert!(
            fileedit::type_into(&panel.st, &fileedit::path_of(&e).to_string(), "X"),
            "往长文件里插字失败"
        );
        // 等防抖 + 重建（行号栏重建有 80ms 防抖）
        pump(600);
        let after = fileedit::scroll_offset(&e);
        println!("长文件底部敲字前后的滚动偏移：{before} → {after}");
        // ⚠ 断言要**卡得死**：跳顶后 scroll 会变成 ~8（不是 0），光判
        //   「大于 0」会把它放过（踩过：修复前 8475 → 8 竟然过了）。
        //   ⇒ 判「位置基本没变」：允许 GTK 自动滚到光标的那点微调。
        assert!(
            after > 0.0,
            "在底部敲一个字，视图跳到顶部了（scroll={before} → {after}）——没法继续改"
        );
        assert!(
            (after - before).abs() < 200.0,
            "在底部敲一个字，视图跳到了别处（scroll={before} → {after}）"
        );
        println!("长文件底部敲字没有跳顶 ✓");
    }

    // —— 阶段 23：双击文件 = **编辑**（不是上传 / 下载）——
    //
    // 双击走的是 `row-activated` 信号 → `on_row_activated` → 编辑器，
    // 跟右键菜单里的「打开」是同一个入口。这里发真信号，验的是
    // **真·双击那条路**（不是直接调函数）。
    {
        // 找 dblclick.txt 那一行（单独一个文件，必然新开一扇编辑器窗）
        let mut target = None;
        if let Some(mut it) = pane.model.iter_first() {
            loop {
                let n: String = pane.model.get(&it, puttyplus::panel::pane::COL_NAME);
                if n == "dblclick.txt" {
                    target = Some(pane.model.path(&it));
                    break;
                }
                if !pane.model.iter_next(&mut it) {
                    break;
                }
            }
        }
        let path = target.expect("本地面板里没找到 dblclick.txt");
        let before = panel.st.file_edits.borrow().len();
        // 真·双击（row-activated）
        pane.view.emit_row_activated(&path, None);
        pump(600);
        let after = panel.st.file_edits.borrow().len();
        println!("双击后编辑器窗口数：{before} → {after}");
        assert!(
            after > before,
            "双击文件没有打开编辑器（应当是编辑，不是上传 / 下载）"
        );
    }

    // —— Python：语言识别 + 关键字真的上了色 ——
    // 「识别出语言」不等于「高亮生效」：tag 没进 GTK 的 tag 表时
    // apply_tag 会被拒掉，界面上一点颜色都没有（用户报的就是这个）。
    {
        assert!(select("sample.py"), "本地面板里没找到 sample.py");
        pump(100);
        panel::on_open_file(&panel.st, false, view);
        pump(500);
        let edits = panel.st.file_edits.borrow();
        let py = edits
            .iter()
            .find(|e| fileedit::path_of(e).ends_with("sample.py"))
            .expect("Python 编辑器窗口");
        assert_eq!(
            fileedit::lang_of(py),
            puttyplus::panel::syntax::Lang::Python,
            "没认出 Python"
        );
        // "import socket" 在中文文档字符串之后：字节偏移 ≠ 字符偏移，
        // 正好验一遍字符偏移换算
        let byte = SAMPLE_PY.find("import socket").expect("样例里没有 import");
        let char_off = SAMPLE_PY[..byte].chars().count() as i32;
        let fg = fileedit::fg_at(py, char_off).expect("Python 关键字没有高亮颜色");
        assert_ne!(fg, "#000000", "关键字颜色是黑的，等于没高亮");
        println!("Python 关键字 import 的高亮色：{fg}");
    }

    // —— Markdown（含代码围栏）：曾经把整个窗口冻住的那一类 ——
    // 围栏分支的游标原地打转 → 高亮在 GTK 主线程上无限循环 → 窗口冻住、
    // CPU 100%、零 syscall。修复前走到这一步就再也回不来了（看门狗会喊）。
    {
        assert!(select("sample.md"), "本地面板里没找到 sample.md");
        pump(100);
        panel::on_open_file(&panel.st, false, view);
        pump(500);
        let edits = panel.st.file_edits.borrow();
        let md = edits
            .iter()
            .find(|e| fileedit::path_of(e).ends_with("sample.md"))
            .expect("Markdown 编辑器窗口");
        assert_eq!(
            fileedit::lang_of(md),
            puttyplus::panel::syntax::Lang::Markdown,
            "没认出 Markdown"
        );
        // 围栏行上了色（Meta 类）——说明高亮真的跑完了，不是被守卫挡下了
        let byte = SAMPLE_MD.find("```rust").expect("样例里没有围栏");
        let char_off = SAMPLE_MD[..byte].chars().count() as i32;
        let fg = fileedit::fg_at(md, char_off).expect("围栏没有高亮颜色");
        assert_ne!(fg, "#000000", "围栏颜色是黑的，等于没高亮");
        println!("Markdown 围栏的高亮色：{fg}");
    }

    if hold {
        println!("holding 20s for screenshot…");
        pump(20_000);
    }

    panel::shutdown(&panel.st);
    win.destroy();
    let _ = std::fs::remove_dir_all(&dir);
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    println!("fileedit ui smoke ok");
}
