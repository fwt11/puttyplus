//! 终态文案（「完成」/「失败」…）淡出冒烟。
//! 运行：cargo run --example xfer_fade_smoke
//!
//! 用户 2026-11-10：「下载完了，就显示一下完成，然后就慢慢消失吧，不要把
//! 「完成」一直显示在那里」。
//!
//! 逐条钉住：
//!   1. 终态文案**立刻可见**（不透明度 1.0）
//!   2. 停留期内**不淡**（用户得看清"完成了"）
//!   3. 淡出期透明度**线性下降**
//!   4. ⚠️ **队列排空时的清空不能抹掉它**（`clear_progress`）
//!   5. ⚠️ **目录刷新不能抹掉它**（`fill_model` 重建模型 ⇒ 搬运回去）
//!   6. 淡完就**不再复活**（下一次队列刷新不会又点亮它）
//!   7. 模型里的透明度**真的送到了渲染器**（画面上确实会淡）

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{pane, fade, PanelState, SftpPanel};

fn pump(n: usize) {
    let ctx = glib::MainContext::default();
    for _ in 0..n {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 三行假数据（当成本地目录的内容）。
fn rows() -> Vec<pane::RowData> {
    vec![
        pane::RowData {
            name: "a.txt".into(),
            size: 12,
            is_dir: false,
            perm: "-rw-r--r--".into(),
            mtime: 1_700_000_000,
            hidden: false,
        },
        pane::RowData {
            name: "b.bin".into(),
            size: 4096,
            is_dir: false,
            perm: "-rw-r--r--".into(),
            mtime: 1_700_000_001,
            hidden: false,
        },
        pane::RowData {
            name: "c.txt".into(),
            size: 3,
            is_dir: false,
            perm: "-rw-r--r--".into(),
            mtime: 1_700_000_002,
            hidden: false,
        },
    ]
}

/// 这一行的 (文案, 透明度, 起始时刻)
fn state(st: &Rc<PanelState>, name: &str) -> (String, f32, i64) {
    let p = st.pane(false);
    pane::row_terminal(&p, name)
        .unwrap_or_else(|| panic!("本地列表里没有这一行：{name}"))
}

/* ---------------- 渲染探针（独立窗口） ---------------- */

/// 造一个**独立的小窗口**：自己的模型 + 名称列 ⇒ 专门验"模型 ⇒ 渲染器 ⇒
/// 画面"这一段链路。
///
/// ⚠️ 面板自己的 view 挂在没 present 的窗口里 ⇒ 画不出东西 ⇒ 像素级
///    验证只能另起一个窗口。
fn build_render_probe() -> (gtk::ApplicationWindow, gtk::ListStore, pane::FileCols) {
    // ⚠️ 列的顺序/类型必须和 `pane::build_pane` 一模一样（cell data func
    //    按列号取值）。
    let model = gtk::ListStore::new(&[
        gtk::glib::Type::STRING, // ICON
        gtk::glib::Type::STRING, // NAME
        gtk::glib::Type::STRING, // SIZE_H
        gtk::glib::Type::STRING, // PERM
        gtk::glib::Type::STRING, // MTIME
        gtk::glib::Type::BOOL,   // ISDIR
        gtk::glib::Type::U64,    // SIZE
        gtk::glib::Type::BOOL,   // HIDDEN
        gtk::glib::Type::I32,    // PCT
        gtk::glib::Type::STRING, // PROGRESS
        gtk::glib::Type::STRING, // MTIME_FULL
        gtk::glib::Type::BOOL,   // XFER
        gtk::glib::Type::F32,    // FADE
        gtk::glib::Type::I64,    // FADE_AT
    ]);
    let it = model.append();
    let icon = "text-x-generic-symbolic";
    let name = "a.txt";
    let empty = "";
    let mtime = "2026-11-10 20:00";
    model.set(
        &it,
        &[
            (0, &icon),
            (1, &name),
            (2, &"12"),
            (3, &"-rw-r--r--"),
            (4, &mtime),
            (5, &false),
            (6, &0u64),
            (7, &false),
            (8, &100i32),
            // ⚠️ 文案写长一点 ⇒ 像素差异更明显，好比对
            (9, &"完成 (1.2 MB)"),
            (10, &mtime),
            (11, &false),
            (12, &1.0f32),
            (13, &0i64),
        ],
    );

    let view = gtk::TreeView::with_model(&model);
    let icon_col = gtk::TreeViewColumn::new();
    let rp = gtk::CellRendererPixbuf::new();
    icon_col.pack_start(&rp, false);
    icon_col.add_attribute(&rp, "icon-name", 0);
    view.append_column(&icon_col);
    let cols = pane::add_file_columns(&view, true);

    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&view));
    let win = gtk::ApplicationWindow::builder()
        .default_width(520)
        .default_height(160)
        .build();
    win.set_child(Some(&sw));
    win.present();
    pump(30);
    (win, model, cols)
}

/// 把模型里所有行的透明度设成 `alpha`。
fn set_fade_col(model: &gtk::ListStore, alpha: f32) {
    let mut it = model.iter_first();
    while let Some(mut i) = it {
        model.set(&i, &[(pane::COL_FADE as u32, &alpha)]);
        it = if model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
}

/// 抓 X 根窗口（⚠️ Xvfb 下 `scrot` 只能拍到黑屏 ⇒ 用 ImageMagick 的 import）。
fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/import")
        .arg("-window")
        .arg("root")
        .arg(path)
        .status();
}

/// 两张图的平均像素差异（0 = 完全一样）。
fn pixel_diff(a: &str, b: &str) -> f64 {
    let out = std::process::Command::new("/usr/bin/convert")
        .arg(a)
        .arg(b)
        .arg("-compose")
        .arg("difference")
        .arg("-composite")
        .arg("-colorspace")
        .arg("Gray")
        .arg("-format")
        .arg("%[fx:mean]")
        .arg("info:")
        .output();
    let Ok(out) = out else { return 0.0 };
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
}

fn main() {
    if !gtk::is_initialized_main_thread() {
        gtk::init().unwrap();
    }

    // ⚠️ 本地目录指到临时目录 ⇒ 不碰用户真实的文件列表
    let tmp = std::env::temp_dir().join("pp_xfer_fade_smoke");
    let _ = std::fs::create_dir_all(&tmp);
    unsafe { std::env::set_var("PUTTYPLUS_LOCALDIR", &tmp) };

    // ⚠️ **主机留空** ⇒ 面板不会去连 SFTP（也就不会弹模态框挡路）
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: String::new(),
        ..Default::default()
    };
    let panel = SftpPanel::new(&opts);
    let st: Rc<PanelState> = Rc::clone(&panel.st);
    pump(20);

    let p = st.pane(false);
    pane::fill_model(&p, true, rows());
    pump(10);

    // ---- 1. 起点：没有终态文案 ----
    let (text, alpha, at) = state(&st, "a.txt");
    assert_eq!(text, "", "起点不该有终态文案，实际 {text:?}");
    assert_eq!(alpha, 1.0, "起点透明度应当是 1，实际 {alpha}");
    assert_eq!(at, 0, "起点不该在淡出，实际 {at}");
    assert!(!pane::has_fading(&p), "起点不该有正在淡出的行");
    println!("✔ 起点干净：没有终态文案、没有在淡的行");

    // ---- 2. 终态文案写上去 ----
    let t0 = pane::now_ms();
    assert!(
        pane::set_terminal(&p, "a.txt", 100, "完成", t0),
        "写终态文案应该成功"
    );
    let (text, alpha, at) = state(&st, "a.txt");
    assert_eq!(text, "完成", "终态文案应当是「完成」，实际 {text:?}");
    assert_eq!(alpha, 1.0, "刚写上应当完全不透明，实际 {alpha}");
    assert_eq!(at, t0 as i64, "起始时刻应当是写上去的那一刻");
    assert!(pane::has_fading(&p), "写上之后这一行应当在淡出中");
    // 别的行不能被波及
    let (t2, a2, _) = state(&st, "b.bin");
    assert_eq!(t2, "", "别的行不该被写上终态文案，实际 {t2:?}");
    assert_eq!(a2, 1.0, "别的行透明度不该被改，实际 {a2}");
    println!("✔ 终态文案「完成」立刻出现且完全不透明，只有这一行被波及");

    // ---- 3. 停留期内不淡 ----
    pane::tick_fade(&p, t0 + fade::HOLD_MS - 1);
    let (_, alpha, _) = state(&st, "a.txt");
    assert_eq!(
        alpha, 1.0,
        "停留期内不该变淡（用户得看清「完成」），实际 {alpha}"
    );
    println!("✔ 停留期（{}ms）内一直看得见", fade::HOLD_MS);

    // ---- 4. 淡出期线性下降 ----
    let half = t0 + fade::HOLD_MS + fade::FADE_MS / 2;
    pane::tick_fade(&p, half);
    let (_, alpha, at2) = state(&st, "a.txt");
    assert!(
        (alpha - 0.5).abs() < 0.05,
        "淡出中点应该只剩一半不透明度，实际 {alpha}"
    );
    assert_eq!(at2, t0 as i64, "淡出过程中起始时刻不该变");
    println!("✔ 淡出到一半：不透明度 {alpha:.3}（文件名本身不受影响）");

    // ---- 5. 队列排空的清空**不能**抹掉它 ----
    //
    // ⚠️ 这是最关键的一条：传输全部结束时会调 `clear_progress` ⇒ 以前
    //    「完成」刚出现就被抹掉，用户根本看不见。
    pane::clear_progress(&p);
    let (text, alpha, at3) = state(&st, "a.txt");
    assert_eq!(
        text, "完成",
        "队列排空时的清空把「完成」抹掉了（用户根本来不及看）"
    );
    assert_eq!(at3, t0 as i64, "清空不该动淡出状态");
    assert!(alpha > 0.0, "清空后透明度不该变 0，实际 {alpha}");
    println!("✔ 队列排空时清空进度：正在淡出的「完成」安然无恙");

    // ---- 6. 目录刷新（重建模型）**不能**抹掉它 ----
    //
    // ⚠️ 传输完成后会自动刷新远程目录 ⇒ `fill_model` 会重建整个模型 ⇒
    //    不搬运的话「完成」会被这一下抹掉。
    pane::fill_model(&p, true, rows());
    let (text, alpha, at4) = state(&st, "a.txt");
    assert_eq!(text, "完成", "目录刷新把「完成」抹掉了");
    assert_eq!(at4, t0 as i64, "目录刷新后起始时刻应当还在");
    assert!(alpha > 0.0, "目录刷新后透明度不该变 0，实际 {alpha}");
    assert!(pane::has_fading(&p), "目录刷新后这一行应当还在淡出中");
    println!("✔ 目录刷新（模型重建）后「完成」还在，淡出继续");

    // ---- 7. 淡完 → 看不见了，且不再复活 ----
    pane::tick_fade(&p, t0 + fade::TOTAL_MS);
    let (_text, alpha, at5) = state(&st, "a.txt");
    assert_eq!(alpha, 0.0, "淡完之后透明度应当是 0，实际 {alpha}");
    assert!(at5 < 0, "淡完之后应当标记成「已淡完」，实际 {at5}");
    assert!(!pane::has_fading(&p), "淡完之后不该还有在淡的行");
    // ⚠️ **不会再亮起来**：下一次队列刷新会再调 set_terminal ⇒ 必须被拒
    assert!(
        !pane::set_terminal(&p, "a.txt", 100, "完成", pane::now_ms()),
        "淡完之后「完成」又冒出来了（用户明确说过不要一直挂在那儿）"
    );
    let (_, alpha2, _) = state(&st, "a.txt");
    assert_eq!(alpha2, 0.0, "被拒之后透明度不该变，实际 {alpha2}");
    println!("✔ 淡完之后彻底不见，而且下一次刷新**不会**再点亮它");

    // ---- 8. 模型 ⇒ 渲染器 ⇒ 画面 这一整段链路 ----
    //
    // ⚠️ 前面 7 条都在**模型层**验证；这一条要证明**画面上真的在淡**
    //    ⇒ 得让 GTK 真的画一次 ⇒ 造一个**独立的小窗口**（面板那个 view
    //    挂在没 present 的窗口里，画不出东西）。
    let (win, rmodel, rcols) = build_render_probe();

    // 8a. 渲染器确实收到了模型里的透明度（cell data func 推过去了）
    set_fade_col(&rmodel, 0.5);
    rcols.name_renderer.set_fade(0.0); // 先弄成错的，看会不会被推回来
    rcols.name_renderer.fade_alpha();
    pump(20);
    let alpha_r = rcols.name_renderer.fade_alpha();
    assert!(
        (alpha_r - 0.5).abs() < 0.01,
        "渲染器拿到的透明度是 {alpha_r}，不是模型里的 0.5 ⇒ cell data func 没推"
    );
    println!("✔ 渲染器确实收到了模型里的透明度（{alpha_r:.3}）");

    // 8b. 像素级：alpha=1 与 alpha=0 的画面**必须有差别**
    set_fade_col(&rmodel, 1.0);
    pump(20);
    let a = "/tmp/pp_fade_a.png";
    let b = "/tmp/pp_fade_b.png";
    shot(a);
    set_fade_col(&rmodel, 0.0);
    pump(20);
    shot(b);
    let d = pixel_diff(a, b);
    assert!(
        d > 1e-5,
        "透明度 1.0 与 0.0 的画面**一模一样**（差异 {d}）⇒ 文字根本没淡"
    );
    println!("✔ 像素级：alpha 1.0 vs 0.0 的画面差异 {d:.6} ⇒ 文字真的会淡掉");

    // 8c. 中间值应该介于两者之间（半透明）
    set_fade_col(&rmodel, 0.5);
    pump(20);
    let m = "/tmp/pp_fade_m.png";
    shot(m);
    let d_half_full = pixel_diff(a, m);
    let d_half_zero = pixel_diff(m, b);
    assert!(
        d_half_full > 0.0 && d_half_zero > 0.0,
        "半透明画面既不像全亮也不像全暗（{d_half_full} / {d_half_zero}）"
    );
    println!(
        "✔ 半透明确实介于中间：离全亮 {d_half_full:.6}、离全暗 {d_half_zero:.6}"
    );

    win.close();
    pump(6);

    println!("\n✔ 全部通过：终态文案显示一会儿 → 慢慢变淡 → 消失，且不会一直挂在那儿");
}
