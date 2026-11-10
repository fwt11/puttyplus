//! **hex 模式打开非文本文件**的冒烟。
//! 运行：cargo run --example hex_view_smoke
//!
//! 用户 2026-11-10：「文本编辑器只能打开文本，对于非文本文件提供以 hex
//! 模式打开的选项，首先弹出一个窗口，告诉用户这不是文本，问用户是否以
//! hex 模式打开，如果用户点击同意，才打开编辑器以 hex 模式打开。如果用户
//! 不同意，则无动作。」
//!
//! 逐条钉住：
//!   1. 打开非文本文件 ⇒ **先弹窗**（标题「这不是文本文件」+「以 hex 模式
//!      打开」按钮）
//!   2. 点「以 hex 模式打开」⇒ 编辑器打开，正文是 **hex 转储**，且**只读**
//!   3. 点「取消」⇒ **什么也不发生**（不弹窗报错，也不留编辑器）
//!   4. hex 模式**存不回去**（保存按钮不摆、Ctrl+S 也拒）
//!   5. 文本文件**照旧**直接打开（不弹窗）

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{dialogs, fileedit, PanelState, SftpPanel};

fn pump(n: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..n {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 找到「问要不要 hex 打开」的那个对话框。
fn find_hex_dialog() -> Option<gtk::Dialog> {
    for w in gtk::Window::list_toplevels() {
        let Some(d) = w.downcast_ref::<gtk::Dialog>() else {
            continue;
        };
        let labels = dialogs::button_labels(d);
        if labels.iter().any(|l| l.contains("hex")) {
            return Some(d.clone());
        }
    }
    None
}

/// 造一个**二进制**文件（NUL 字节 ⇒ 一定被判成非文本）。
fn write_binary(dir: &std::path::Path, name: &str) -> String {
    let p = dir.join(name);
    let mut data: Vec<u8> = (0u8..=255u8).collect();
    data.extend_from_slice(b"\x00\x01\x02hello");
    std::fs::write(&p, &data).unwrap();
    p.to_string_lossy().into_owned()
}

/// 造一个**纯文本**文件。
fn write_text(dir: &std::path::Path, name: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, "hello\nworld\n").unwrap();
    p.to_string_lossy().into_owned()
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    let tmp = std::env::temp_dir().join(format!("pp_hex_smoke_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    // ⚠️ 本地目录指到临时目录 ⇒ 不碰用户真实的文件列表
    unsafe { std::env::set_var("PUTTYPLUS_LOCALDIR", &tmp) };

    let opts = ConnOptions {
        proto: "ssh".into(),
        host: String::new(), // ⚠️ 留空 ⇒ 面板不去连 SFTP（不弹模态框挡路）
        ..Default::default()
    };
    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);
    pump(20);

    let bin = write_binary(&tmp, "blob.bin");
    let txt = write_text(&tmp, "plain.txt");

    // ---- 1. 打开二进制 ⇒ 先弹窗 ----
    fileedit::open_local(&st, &bin, false);
    pump(30);
    let dlg = find_hex_dialog();
    assert!(
        dlg.is_some(),
        "打开非文本文件**没有**弹窗问「要不要以 hex 模式打开」"
    );
    let dlg = dlg.unwrap();
    let title = dlg.title().unwrap_or_default();
    assert!(
        title.contains("不是文本文件"),
        "弹窗标题应当告诉用户「这不是文本文件」，实际 {title:?}"
    );
    let labels = dialogs::button_labels(&dlg);
    assert!(
        labels.iter().any(|l| l.contains("hex")),
        "弹窗上应当有「以 hex 模式打开」按钮，实际 {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("取消")),
        "弹窗上应当有「取消」按钮，实际 {labels:?}"
    );
    println!("✔ 打开非文本文件 ⇒ 弹窗问「这不是文本文件」，带「以 hex 模式打开」");

    // ---- 2. 点「以 hex 模式打开」⇒ 进 hex 模式 ----
    dlg.response(gtk::ResponseType::Ok);
    pump(40);
    assert_eq!(
        fileedit::is_hex(&st, &bin),
        Some(true),
        "点了「以 hex 模式打开」之后**没有**进 hex 模式"
    );
    let body = fileedit::text_of(&st, &bin).expect("hex 模式应当有编辑器");
    assert!(
        body.starts_with("00000000  00 01 02 03"),
        "正文应当是 hex 转储，实际开头 {:?}",
        &body[..body.len().min(40)]
    );
    assert!(
        body.contains("|................"),
        "正文应当带右侧 ASCII 列，实际 {:?}",
        &body[..body.len().min(80)]
    );
    println!("✔ 同意 ⇒ 打开 hex 模式，正文是转储（偏移 + 十六进制 + ASCII）");

    // ---- 3. hex 模式**只读**（存不回去）----
    assert_eq!(
        fileedit::is_hex(&st, &bin),
        Some(true),
        "hex 标记丢了"
    );
    assert!(
        !fileedit::save_btn_visible_for(&st, &bin),
        "hex 模式不该摆「保存」按钮（存回去就把原始二进制毁了）"
    );
    assert_eq!(
        fileedit::editable_for(&st, &bin),
        Some(false),
        "hex 模式的正文**居然能编辑**（改了存回去就把原始二进制毁了）"
    );
    // ⚠️ **真的试一次保存** ⇒ 文件字节必须**一个都没变**
    let before = std::fs::read(&bin).unwrap();
    assert!(
        !fileedit::try_save(&st, &bin),
        "hex 模式居然保存成功了（转储文本会覆盖原始二进制）"
    );
    let after = std::fs::read(&bin).unwrap();
    assert_eq!(
        before.len(),
        after.len(),
        "hex 模式保存后**文件大小变了** ⇒ 原始二进制被转储文本覆盖了"
    );
    assert_eq!(before, after, "hex 模式保存后**文件内容变了**");
    println!("✔ hex 模式是只读的：不摆「保存」按钮、不能编辑、保存被拒且文件没变");

    // ---- 4. 另一个二进制文件 ⇒ 点「取消」⇒ 什么也不发生 ----
    let bin2 = write_binary(&tmp, "blob2.bin");
    fileedit::open_local(&st, &bin2, false);
    pump(30);
    let dlg2 = find_hex_dialog().expect("第二个二进制文件也该弹窗");
    dlg2.response(gtk::ResponseType::Cancel);
    pump(30);
    assert_eq!(
        fileedit::is_hex(&st, &bin2),
        None,
        "用户点了「取消」却还是打开了编辑器（用户不同意 ⇒ 应当无动作）"
    );
    println!("✔ 不同意 ⇒ 无动作：没有编辑器、没有 hex 视图");

    // ---- 5. 文本文件照旧直接打开（**不弹窗**）----
    fileedit::open_local(&st, &txt, false);
    pump(30);
    assert!(
        find_hex_dialog().is_none(),
        "文本文件不该弹「要不要 hex」的窗"
    );
    assert_eq!(fileedit::is_hex(&st, &txt), Some(false));
    let body = fileedit::text_of(&st, &txt).expect("文本文件应当直接打开");
    assert_eq!(body, "hello\nworld\n", "文本内容应当原样显示，实际 {body:?}");
    println!("✔ 文本文件照旧直接打开，不弹窗");

    let _ = std::fs::remove_dir_all(&tmp);
    println!("\n✔ 全部通过：非文本文件先问 → 同意才进 hex 模式 → 不同意无动作");
}
