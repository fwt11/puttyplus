//! 阶段 86-④ 冒烟：ghostty 自绘终端的**鼠标拖选 + 复制**。
//!
//! **不连真库、不连 SSH**——纯界面验证。
//!
//! 验四件事：
//! 1. **真拖**能选出文字（**xdotool 真拖**：按下 → 拖 → 抬起）；
//! 2. 选出来的**文本内容对**（"hello world"）；
//! 3. 选区**真的画出来了**（反白高亮——像素级判定，不看内部状态）；
//! 4. 「复制」能写进**系统剪贴板**（读回来比对）。
//!
//! ⚠️ 以前 ghostty 后端的 `copy_selection()` 是**空函数**（内核有
//! selection 模块，我们没接拖选）⇒ Windows 上根本没法复制终端里的文字。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example ghostty_select_smoke

use gtk::prelude::*;
use puttyplus::termview::ghostty_view::GhosttyView;
use std::cell::RefCell;
use std::rc::Rc;

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

fn xdo(args: &[&str]) {
    let _ = std::process::Command::new("xdotool").args(args).status();
}

fn main() {
    gtk::init().expect("gtk init");

    let view = GhosttyView::new();
    // 第 0 行："hello world"（hello = 0..4，world = 6..10）
    view.feed(b"hello world\r\nsecond line\r\n");

    let win = gtk::Window::new();
    win.set_title(Some("阶段 86-④ ghostty 拖选冒烟"));
    win.set_default_size(420, 160);
    win.set_child(Some(view.widget()));
    win.present();
    pump(700);

    /* ---------------- ① 没有选区时复制是**空操作** ---------------- */
    assert!(
        view.selection_text().is_none(),
        "刚建好就有选区了：{:?}",
        view.selection_text()
    );
    println!("[ok] 初始没有选区");

    /* ---------------- ② 真拖（xdotool）：从 "hello" 拖到 "world" 末尾 ------- */
    let (cw, ch) = view.cell_size();
    assert!(cw > 1.0 && ch > 1.0, "格子尺寸不对：({cw}, {ch})");
    let winid = win_id("阶段 86-④ ghostty 拖选冒烟");
    assert!(!winid.is_empty(), "找不到窗口 id（xdotool search 没结果）");
    let (vx, vy) = view
        .widget()
        .translate_coordinates(&win, 0.0, 0.0)
        .expect("换算坐标失败");
    // 起点：第 0 行第 0 格；终点：第 0 行第 10 格（world 的最后一个字母）
    let x0 = vx + 1.0;
    let y0 = vy + ch / 2.0;
    let x1 = vx + cw * 10.0 + cw / 2.0;
    println!("拖选：({x0:.0}, {y0:.0}) → ({x1:.0}, {y0:.0})，格子 {cw:.1}×{ch:.1}");

    xdo(&["mousemove", "--window", &winid, &format!("{x0:.0}"), &format!("{y0:.0}")]);
    pump(150);
    xdo(&["mousedown", "1"]);
    pump(80);
    // 分几步拖（一步到位有时候 GTK 认不出拖动）
    for step in 1..=4 {
        let x = x0 + (x1 - x0) * (step as f64 / 4.0);
        xdo(&[
            "mousemove",
            "--window",
            &winid,
            &format!("{x:.0}"),
            &format!("{y0:.0}"),
        ]);
        pump(80);
    }
    xdo(&["mouseup", "1"]);
    pump(400);

    /* ---------------- ③ 选出来的文本对 ---------------- */
    let text = view.selection_text().expect("拖完了应该有选区");
    println!("[ok] 选区文本 = {text:?}");
    assert_eq!(
        text.trim(),
        "hello world",
        "选区文本不对（拖到的是第 10 格 ⇒ 应含 world）"
    );

    /* ---------------- ④ 选区真的画出来了（像素级） ---------------- */
    //
    // 选区是**反白**（底色画成前景色）⇒ 被选中的格子明显比周围亮。
    // 判据：第 0 行第 1 格的像素**亮度**要显著高于没被选中的第 1 行。
    let (w, h) = (420i32, 160i32);
    let mut surf = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
        .expect("建内存 surface");
    {
        let cr = gtk::cairo::Context::new(&surf).expect("建 cairo 上下文");
        view.render_to(&cr, w as f64, h as f64);
    }
    surf.flush();
    let stride = surf.stride() as usize;
    let data = surf.data().expect("读 surface 像素");
    let px = |x: usize, y: usize| -> (i32, i32, i32) {
        let o = y * stride + x * 4;
        (data[o + 2] as i32, data[o + 1] as i32, data[o] as i32)
    };
    let lum = |c: (i32, i32, i32)| -> f64 {
        0.2126 * c.0 as f64 + 0.7152 * c.1 as f64 + 0.0722 * c.2 as f64
    };
    // 第 0 行第 1 格（在 "hello" 里 ⇒ 已被选中）
    let sel_px = px((cw * 1.0) as usize, (ch * 0.5) as usize);
    // 第 1 行第 1 格（"second line" ⇒ 没被选中）
    let unsel_px = px((cw * 1.0) as usize, (ch * 1.5) as usize);
    println!("[ok] 选中格像素 {sel_px:?}（亮度 {:.0}） / 未选中格 {unsel_px:?}（亮度 {:.0}）",
        lum(sel_px), lum(unsel_px));
    assert!(
        lum(sel_px) > lum(unsel_px) + 30.0,
        "选区没高亮（选中格亮度 {:.0} vs 未选中 {:.0}）⇒ 反白没画出来",
        lum(sel_px),
        lum(unsel_px)
    );

    // ⚠️ **选中时文字也要反色**（真反白）——只反底色的话，默认色的文字
    // 就是「白底白字」⇒ **选中的内容看不见**。
    //
    // 判据：选中区域里必须有**不止一种**颜色（底色 + 文字）⇒ 文字看得见。
    // （踩过的坑：只反底色时，选中区域的颜色**只有一种** ⇒ 文字隐形。）
    {
        let mut kinds = std::collections::BTreeSet::new();
        let x0 = 0usize;
        let x1 = (cw * 6.0) as usize;
        for y in 0..(ch as usize) {
            for x in x0..x1.min(w as usize) {
                kinds.insert(px(x, y));
            }
        }
        println!("[ok] 选中区域的颜色种类 = {}", kinds.len());
        assert!(
            kinds.len() >= 2,
            "选中区域只有 {} 种颜色 ⇒ 文字和底色同色 ⇒ 选中的内容看不见",
            kinds.len()
        );
    }

    /* ---------------- ⑤ 复制进系统剪贴板 ---------------- */
    view.copy_selection();
    pump(300);
    // ⚠️ 读剪贴板是**异步**回调（且要求 'static）⇒ 用 Rc<RefCell<…>> 收
    let got: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    {
        let got = got.clone();
        puttyplus::platform::clipboard::read_text(move |t| {
            *got.borrow_mut() = t;
        });
    }
    // 读剪贴板是异步的 ⇒ 泵一会儿等回调
    for _ in 0..40 {
        pump(50);
        if got.borrow().is_some() {
            break;
        }
    }
    let clip = got.borrow().clone().expect("读剪贴板没回调");
    println!("[ok] 剪贴板内容 = {clip:?}");
    assert_eq!(
        clip.trim(),
        "hello world",
        "剪贴板里的内容不对"
    );

    /* ---------------- ⑥ 单击清掉选区 ---------------- */
    xdo(&[
        "mousemove",
        "--window",
        &winid,
        &format!("{:.0}", vx + cw * 20.0),
        &format!("{:.0}", vy + ch * 3.5),
    ]);
    pump(150);
    xdo(&["click", "1"]);
    pump(300);
    assert!(
        view.selection_text().is_none(),
        "单击别处后选区还在：{:?}",
        view.selection_text()
    );
    println!("[ok] 单击别处 ⇒ 选区清掉");

    println!("\nghostty_select_smoke: 全部通过");
}
