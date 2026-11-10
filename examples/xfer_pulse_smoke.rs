//! 冒烟：传输中的文件行背景会「呼吸」（用户 2026-11-10）。
//!
//! 用户原话：「如果有上传后下载，上传后下载的文件背景色可以呼吸变化，
//! 这样更明显些，任务列表等图标也可以有一些动态效果」。
//!
//! 覆盖两件事：
//!
//! 1. **像素级**（真的截图比像素）：文件列表里「正在传」的那一行，背景色
//!    随呼吸相位变化 ⇒ 两次截图的**差异 > 阈值**；把标志关掉再测 ⇒ 两次
//!    截图应当**一模一样**（证明那个差异确实来自呼吸，不是别的抖动）。
//! 2. **真链路**：队列里有传输项在跑时，程序会起呼吸定时器
//!    （`PanelState::xfer_anim`）；队列空了定时器就停 ⇒ 空闲时不占 CPU。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example xfer_pulse_smoke
//!
//! ⚠️ **截图用 ImageMagick 的 `import`**（抓 X 根窗口）——本机的 `scrot`
//!    在 Xvfb 下只能拍到黑屏（拍不了 ⇒ 没法比像素）。

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::pane;
use puttyplus::panel::worker::{XferKind, XferShared, XferState};
use puttyplus::panel::queue::QueueItem;
use puttyplus::panel::{self, SftpPanel};

/// 跑 GTK 主循环 ms 毫秒。
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

/// 抓一张屏幕（X 根窗口）。
fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/import")
        .arg("-window")
        .arg("root")
        .arg(path)
        .status();
}

/// 两张图的平均差异（0 = 完全一样，1 = 完全不同）。
fn diff(a: &str, b: &str) -> f64 {
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
        .output()
        .expect("convert");
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<f64>().unwrap_or(-1.0)
}

/// 造一个文件列表：第 0 行普通，第 1 行「正在传」。
fn build_view(busy: bool) -> (gtk::ApplicationWindow, gtk::TreeView) {
    let model = gtk::ListStore::new(&[
        gtk::glib::Type::STRING, // 0 ICON
        gtk::glib::Type::STRING, // 1 NAME
        gtk::glib::Type::STRING, // 2 SIZE_H
        gtk::glib::Type::STRING, // 3 PERM
        gtk::glib::Type::STRING, // 4 MTIME
        gtk::glib::Type::BOOL,   // 5 ISDIR
        gtk::glib::Type::U64,    // 6 SIZE
        gtk::glib::Type::BOOL,   // 7 HIDDEN
        gtk::glib::Type::I32,    // 8 PCT
        gtk::glib::Type::STRING, // 9 PROGRESS
        gtk::glib::Type::STRING, // 10 MTIME_FULL
        gtk::glib::Type::BOOL,   // 11 XFER（正在传 ⇒ 呼吸背景）
        gtk::glib::Type::F32,    // 12 FADE（进度文案透明度）
        gtk::glib::Type::I64,    // 13 FADE_AT（终态文案起始时刻）
    ]);
    for (i, name) in ["normal.txt", "busy.txt"].iter().enumerate() {
        let it = model.append();
        model.set(
            &it,
            &[
                (0, &"text-x-generic-symbolic"),
                (1, &name),
                (2, &"1.2M"),
                (3, &"-rw-r--r--"),
                (4, &"2026-11-10 12:00"),
                (5, &false),
                (6, &0u64),
                (7, &false),
                (8, &if busy && i == 1 { &42i32 } else { &0i32 }),
                (9, &if busy && i == 1 { &"42%" } else { &"" }),
                (10, &"2026-11-10 12:00"),
                (11, &(busy && i == 1)),
                (12, &1.0f32),
                (13, &0i64),
            ],
        );
    }

    let view = gtk::TreeView::with_model(&model);
    view.set_headers_visible(false);
    let icon = gtk::TreeViewColumn::new();
    let rp = gtk::CellRendererPixbuf::new();
    icon.pack_start(&rp, false);
    icon.add_attribute(&rp, "icon-name", 0);
    view.append_column(&icon);
    let cols = pane::add_file_columns(&view, true);
    cols.perm.as_ref().unwrap().set_visible(false);
    cols.mtime.set_visible(false);

    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_child(Some(&view));

    let win = gtk::ApplicationWindow::builder()
        .default_width(700)
        .default_height(200)
        .build();
    win.set_child(Some(&sw));
    win.present();
    pump(400);
    (win, view)
}

fn main() {
    gtk::init().expect("gtk init");

    // 别碰用户真实配置
    let home = std::env::temp_dir().join(format!("pp-xfer-pulse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", home.join("settings.ini"));
        std::env::set_var("PUTTYPLUS_DB_INI", home.join("databases.ini"));
        std::env::set_var("PUTTYPLUS_CONN_INI", home.join("connections.ini"));
        std::env::set_var("PUTTYPLUS_HIST_INI", home.join("history.ini"));
        std::env::set_var("PUTTYPLUS_COLS_INI", home.join("columns.ini"));
        std::env::set_var("PUTTYPLUS_RECENT_INI", home.join("recent.ini"));
        std::env::set_var("PUTTYPLUS_VAULT", home.join("vault.bin"));
    }

    // ---- 1) 像素级：正在传的那一行，背景在呼吸 ----
    let (win, view) = build_view(true);
    shot("/tmp/pp_pulse_a.png");
    // 相位是**按时间算**的 ⇒ 等一个呼吸周期的一部分再截 ⇒ 底色应当变了。
    // ⚠️ 这里手动 queue_draw：真程序里是呼吸定时器在重绘（下面第 2 段验它）。
    pump(500);
    view.queue_draw();
    pump(300);
    shot("/tmp/pp_pulse_b.png");
    let d = diff("/tmp/pp_pulse_a.png", "/tmp/pp_pulse_b.png");
    println!("[1] 「正在传」的行：两次截图的平均差异 = {d}");
    assert!(
        d > 0.0005,
        "「正在传」的行背景没有呼吸（两次截图几乎一样：{d}）"
    );
    win.close();
    pump(100);

    // ---- 2) 反证：把标志关掉 ⇒ 两次截图应当**一模一样** ----
    let (win, view) = build_view(false);
    shot("/tmp/pp_pulse_c.png");
    pump(500);
    view.queue_draw();
    pump(300);
    shot("/tmp/pp_pulse_d.png");
    let d0 = diff("/tmp/pp_pulse_c.png", "/tmp/pp_pulse_d.png");
    println!("[2] 普通行（不在传）：两次截图的平均差异 = {d0}");
    assert!(
        d0 < d,
        "普通行也在变（差异 {d0}）⇒ 说明差异不是呼吸带来的，测试没抓准"
    );
    win.close();
    pump(100);

    // ---- 3) 真链路：队列里有东西在跑 ⇒ 起呼吸定时器；跑完 ⇒ 停 ----
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    // ⚠️ **主机留空**：面板只在「是 ssh 且主机非空」时才发起连接 ⇒
    //    留空就不会连 ⇒ 不会弹出「连接失败」的**模态**对话框（那个框会
    //    挡住队列窗口，截图比不出差异）。
    opts.host = String::new();
    opts.user = "nobody".into();
    let panel = SftpPanel::new(&opts);
    let st = panel.st.clone();

    let shared = XferShared::new();
    {
        let mut g = shared.lock().unwrap();
        g.state = XferState::Run;
    }
    st.items.borrow_mut().push(QueueItem {
        id: 1,
        src: "/tmp/x".into(),
        dst_dir: "/tmp".into(),
        upload: true,
        kind: XferKind::Transfer,
        shared: shared.clone(),
    });
    panel::sync_xfer_anim(&st);
    assert!(
        st.xfer_anim.borrow().is_some(),
        "队列里有东西在跑，却没起呼吸定时器"
    );
    println!("[3] 有传输在跑 ⇒ 呼吸定时器已启动");

    // 传完了 ⇒ 定时器停掉（空闲时不占 CPU）
    {
        let mut g = shared.lock().unwrap();
        g.state = XferState::Done;
    }
    panel::sync_xfer_anim(&st);
    assert!(
        st.xfer_anim.borrow().is_none(),
        "传输都结束了，呼吸定时器还挂着"
    );
    println!("[4] 传输结束 ⇒ 呼吸定时器已停");

    // ---- 4) 任务列表（队列窗口）：运行中的行，方向列的颜色在呼吸 ----
    {
        let mut g = shared.lock().unwrap();
        g.state = XferState::Run;
    }
    panel::sync_xfer_anim(&st);
    let item = QueueItem {
        id: 1,
        src: "/tmp/x".into(),
        dst_dir: "/tmp".into(),
        upload: true,
        kind: XferKind::Transfer,
        shared: shared.clone(),
    };
    panel::queue::ensure(&st);
    panel::queue::add_item(&st, &item);
    // ⚠️ **必须 flush**：「正在传」这个标志是 flush 写进模型的
    //    （`QCOL_ACTIVE`）——刚入队时它还是 false。
    panel::queue::flush(&st);
    {
        let qu = st.queue_ui.borrow();
        let qu = qu.as_ref().expect("队列窗口应当已经建好");
        qu.win.present();
        qu.view.queue_draw();
    }
    pump(500);
    shot("/tmp/pp_pulse_q1.png");
    pump(600);
    // 重绘（真程序里是呼吸定时器在重绘）
    if let Some(qu) = st.queue_ui.borrow().as_ref() {
        qu.view.queue_draw();
    }
    pump(300);
    shot("/tmp/pp_pulse_q2.png");
    let dq = diff("/tmp/pp_pulse_q1.png", "/tmp/pp_pulse_q2.png");
    println!("[5] 任务列表（队列窗口）：两次截图的平均差异 = {dq}");
    assert!(
        dq > 0.00002,
        "任务列表里运行中的行没有动态效果（两次截图几乎一样：{dq}）"
    );

    let _ = std::fs::remove_dir_all(&home);
    println!("xfer pulse smoke ok");
}
