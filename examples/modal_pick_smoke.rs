//! 冒烟：模态对话框开着时弹出的**文件选择器必须能点**。
//!
//! **用户 2026-11-14 报的 bug**：「数据库管理 → 导入数据库表」弹出的文件
//! 选择对话框**不能点击、选不了文件**（它明明显示出来了，旁边的
//! 「导入数据（CSV）…」却能点）。
//!
//! **根因**（GTK4 实测）：模态对话框会**抓走输入**（grab）⇒ 模态框开着
//! 的时候，后开的**非模态**窗口显示得出来，却**收不到任何点击**。
//! 独立探针对照：选择器非模态 ⇒ 选不中；模态 ⇒ 选得中。
//!
//! **这个冒烟走真路径**：主窗口 → 模态的「导入数据（CSV）…」→
//! 「选择…」→ **真按** Ctrl+L 敲绝对路径 → 回车 → 路径回到输入框。
//!
//! **判据是外部量**（不看内部状态）：
//! 1. 输入框里最后**有路径** ⇒ 选择器收到了输入；
//! 2. 选择器窗口**已经关掉** ⇒ 它接受了这个文件。
//!
//! ⚠️ 判据 2 是**必需的**：选择器要是死的（点不动），按键会落到**向导**
//! 那边（向导是模态的，它抓着 grab）⇒ 向导的输入框也会被填上路径
//! ⇒ 只看判据 1 会**误判成通过**。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example modal_pick_smoke

use gtk::prelude::*;

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

/// 找到窗口 id（xdotool 的 `mousemove --window` 要用）。
fn win_id(title: &str) -> String {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", title])
        .output()
        .expect("xdotool 跑不起来");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 真按键（XTEST ⇒ 送到**当前焦点**窗口，跟真人按键一样）。
fn xdo(args: &[&str]) {
    let _ = std::process::Command::new("xdotool").args(args).status();
}

fn main() {
    gtk::init().expect("gtk init");

    // ---- 临时目录：放一个 CSV（绝对路径后面要用）----
    let tmp = std::env::temp_dir().join("pp_modal_pick_smoke");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let csv = tmp.join("ppsmoke.csv");
    std::fs::write(&csv, "id,name\n1,puTTY\n").expect("写临时 CSV");
    let csv_path = csv.to_string_lossy().to_string();

    // ---- 主窗口（当父窗口）----
    let main_win = gtk::Window::new();
    main_win.set_title(Some("阶段 87 模态文件选择器冒烟"));
    main_win.set_default_size(420, 200);
    main_win.present();
    pump(500);

    // ---- 模态的「导入数据（CSV）…」那一层 ----
    let wizard = puttyplus::windows::dialog(
        "导入数据（CSV）…",
        Some(&main_win),
        &[("取消", gtk::ResponseType::Cancel)],
    );
    wizard.set_default_size(560, -1);
    let entry = gtk::Entry::new();
    wizard.content_area().append(&entry);
    wizard.present();
    pump(600);

    // ---- 点「选择…」⇒ 文件选择器（走应用的真路径）----
    // ⚠️ 父窗口给**向导自己**（这正是修好之后应用里的做法）
    let parent = wizard.upcast_ref::<gtk::Window>().clone();
    puttyplus::panel::dbimport::pick_csv_into(Some(&parent), &entry);
    pump(1200);

    let chooser = win_id("选择要导入的 CSV 文件");
    assert!(!chooser.is_empty(), "文件选择器没弹出来（找不到窗口）");
    println!("[ok] 文件选择器已弹出：{chooser}");

    // ---- 真按 Ctrl+L ⇒ 位置输入框 ⇒ 敲绝对路径 ⇒ 回车 ----
    //（用键盘而不是点某一行：文件行的位置**随主题变**，点坐标不可靠）
    xdo(&["key", "ctrl+l"]);
    pump(500);
    xdo(&["type", &csv_path]);
    pump(500);
    xdo(&["key", "Return"]);
    pump(1200);

    let text = entry.text().to_string();
    println!("[debug] 输入框里的路径 = {text:?}");
    assert!(
        !text.trim().is_empty(),
        "选完文件后输入框还是空的 ⇒ 文件选择器**没收到输入**\
         （模态框开着时，非模态的选择器就是点不动——用户 2026-11-14 报的 bug）"
    );
    assert!(
        std::path::Path::new(&text).exists(),
        "填回去的路径不存在：{text}"
    );

    // ⚠️ 判据 2：选择器必须**已经关掉**（它接受了这个文件）。
    // 选择器要是死的（收不到输入），按键会落到向导那边 ⇒ 输入框也有路径，
    // 但选择器还开着 ⇒ 只看判据 1 会误判成通过。
    let still = win_id("选择要导入的 CSV 文件");
    assert!(
        still.is_empty(),
        "选择器还开着（没接受文件）⇒ 它**没收到输入**：{still}"
    );
    println!("[ok] 选择器已关闭，路径回到了输入框");

    println!("[ok] 模态框开着时，文件选择器能收输入、能选文件、能把路径填回输入框");
    let _ = std::fs::remove_dir_all(&tmp);
}
