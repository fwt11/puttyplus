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

/// 窗口的**缩放倍率**（GDK_SCALE=2 / Windows 的 150% ⇒ 2）。
///
/// ⚠️ **xdotool 的坐标是物理像素，GTK 的坐标是逻辑像素** ⇒ 缩放 ≠ 1 时
/// 必须把逻辑坐标乘上倍率，否则**鼠标走的路只有看起来的一半**
/// ⇒ 拖选断言会以「选区只有一半」的形式假红（2026-11-14 踩过）。
fn surface_scale(w: &gtk::Widget) -> f64 {
    w.native()
        .and_then(|n| n.surface())
        .map(|s| s.scale())
        .unwrap_or(1.0)
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
    let scale = surface_scale(view.widget());
    println!("窗口缩放倍率 = {scale}");
    let (vx, vy) = view
        .widget()
        .translate_coordinates(&win, 0.0, 0.0)
        .expect("换算坐标失败");
    // 起点：第 0 行第 0 格；终点：第 0 行第 10 格（world 的最后一个字母）
    let x0 = vx + 1.0;
    let y0 = vy + ch / 2.0;
    let x1 = vx + cw * 10.0 + cw / 2.0;
    println!("拖选：({x0:.0}, {y0:.0}) → ({x1:.0}, {y0:.0})，格子 {cw:.1}×{ch:.1}");

    xdo(&[
        "mousemove",
        "--window",
        &winid,
        &format!("{:.0}", x0 * scale),
        &format!("{:.0}", y0 * scale),
    ]);
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
            &format!("{:.0}", x * scale),
            &format!("{:.0}", y0 * scale),
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
        &format!("{:.0}", (vx + cw * 20.0) * scale),
        &format!("{:.0}", (vy + ch * 3.5) * scale),
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

    /* ---------------- ⑦ 选区范围必须跟得上鼠标（阶段 89） ---------------- */
    //
    // ⚠️ **为什么单开一段**：上面只断言「选出来的**文本**对」⇒ 选区终点
    // 比鼠标远得多也可能看不出来（后面是空格子，`trim` 之后一样）⇒
    // 这个 bug 漏了很久（用户 2026-11-14 报：「拖选的范围和鼠标移动的
    // 范围不一致，鼠标稍微移动拖选的范围就很大」）。
    //
    // ⇒ 这里用一串**可区分**的字符，断言「拖到第 N 格 ⇒ 选区就止于第 N 格」。
    {
        let view2 = GhosttyView::new();
        view2.feed(b"0123456789\r\n");
        let win2 = gtk::Window::new();
        win2.set_title(Some("阶段 89 ghostty 拖选范围冒烟"));
        win2.set_default_size(420, 160);
        win2.set_child(Some(view2.widget()));
        win2.present();
        pump(700);

        let (cw, ch) = view2.cell_size();
        assert!(cw > 1.0 && ch > 1.0, "格子尺寸不对：({cw}, {ch})");
        let winid2 = win_id("阶段 89 ghostty 拖选范围冒烟");
        assert!(!winid2.is_empty(), "找不到窗口 id（xdotool search 没结果）");
        let scale2 = surface_scale(view2.widget());
        let (vx, vy) = view2
            .widget()
            .translate_coordinates(&win2, 0.0, 0.0)
            .expect("换算坐标失败");
        let x0 = vx + 1.0;
        let y0 = vy + ch / 2.0;
        // 终点：第 4 格的**中部** ⇒ 选区应当是第 0..=4 格（5 个字符）
        let x1 = vx + cw * 4.5;
        println!("拖选范围：({x0:.0}, {y0:.0}) → ({x1:.0}, {y0:.0})，格子 {cw:.1}");

        xdo(&[
            "mousemove",
            "--window",
            &winid2,
            &format!("{:.0}", x0 * scale2),
            &format!("{:.0}", y0 * scale2),
        ]);
        pump(150);
        xdo(&["mousedown", "1"]);
        pump(80);
        for step in 1..=4 {
            let x = x0 + (x1 - x0) * (step as f64 / 4.0);
            xdo(&[
                "mousemove",
                "--window",
                &winid2,
                &format!("{:.0}", x * scale2),
                &format!("{:.0}", y0 * scale2),
            ]);
            pump(80);
        }
        xdo(&["mouseup", "1"]);
        pump(400);

        let text = view2.selection_text().expect("拖完了应该有选区");
        println!("[ok] 拖到第 5 格 ⇒ 选区文本 = {text:?}");
        assert_eq!(
            text.trim(),
            "01234",
            "选区范围跟不上了：鼠标停在第 5 格，选区却盖到了 {text:?}"
        );
    }

    println!("\nghostty_select_smoke: 全部通过");
}
