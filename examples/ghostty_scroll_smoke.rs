//! 阶段 118 冒烟：ghostty 自绘终端的**滚动历史 + 选历史里的字**。
//!
//! **背景**（用户 2026-11-15）：
//! > 「ghostty 下面还不能查看和选择屏幕当前显示的内容**之前**的内容」
//!
//! 根因：VTE 后端是**控件自己滚**的（`vte4::Terminal` 内置回滚缓冲 +
//! 滚轮），而 ghostty 后端是**我们自绘**的 ⇒ 之前**压根没装滚轮**、
//! 也没有「视口」概念 ⇒ 只能看屏幕底部那几行。
//!
//! **断言**（全是**外部量**，不查内部状态）：
//! 1. **真滚轮**（xdotool 发真的滚轮事件）⇒ 视口离开底部；
//! 2. 滚上去之后**画面真的变了**（渲到内存 surface 做像素比对）；
//! 3. 滚上去之后能**拖选历史里的字**（xdotool 真拖 ⇒ 选出来的文本对）；
//! 4. `Shift+PageUp/PageDown` 也能滚，且**不发**给远端（VTE 同款）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example ghostty_scroll_smoke

use gtk::gdk;
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

/// 窗口的**缩放倍率**（xdotool 的坐标是物理像素，GTK 的是逻辑像素）。
fn surface_scale(w: &gtk::Widget) -> f64 {
    w.native()
        .and_then(|n| n.surface())
        .map(|s| s.scale())
        .unwrap_or(1.0)
}

/// 把视图渲到内存 surface，顺手**存一张图**（`out` 给了路径就存）。
///
/// ⚠️ `ImageSurface::write_to_png` 在这个 cairo 绑定里**没有** ⇒ 自己写
///    **PPM**（零依赖）⇒ 再用 ImageMagick 转成 PNG。
fn snapshot_png(view: &GhosttyView, w: i32, h: i32, out: Option<&str>) {
    let mut surf = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
        .expect("建内存 surface");
    {
        let cr = gtk::cairo::Context::new(&surf).expect("建 cairo 上下文");
        view.render_to(&cr, w as f64, h as f64);
    }
    let Some(p) = out else { return };
    surf.flush();
    let stride = surf.stride() as usize;
    let data = surf.data().expect("读 surface 像素");
    let ppm = format!("{}.ppm", std::path::Path::new(p).display());
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::File::create(&ppm) {
            let _ = write!(f, "P6\n{w} {h}\n255\n");
            let mut buf = Vec::with_capacity((w * h * 3) as usize);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let o = y * stride + x * 4;
                    buf.extend_from_slice(&[data[o + 2], data[o + 1], data[o]]);
                }
            }
            let _ = f.write_all(&buf);
        }
    }
    // ImageMagick 转 PNG（没有就只留 PPM）。
    let _ = std::process::Command::new("convert")
        .arg(&ppm)
        .arg(p)
        .status();
    println!("[ok] 画面已存：{p}（原始 {ppm}）");
}

/// 把视图渲到内存 surface，返回像素（用来比对「画面变了没」）。
fn snapshot_pixels(view: &GhosttyView, w: i32, h: i32) -> Vec<u8> {    let mut surf = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
        .expect("建内存 surface");
    {
        let cr = gtk::cairo::Context::new(&surf).expect("建 cairo 上下文");
        view.render_to(&cr, w as f64, h as f64);
    }
    // ⚠️ 读像素前：Context 要先 drop（data() 要求独占访问）；flush 要先做。
    surf.flush();
    let stride = surf.stride() as usize;
    let data = surf.data().expect("读 surface 像素");
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = y * stride + x * 4;
            out.extend_from_slice(&data[o..o + 4]);
        }
    }
    out
}

/// **真的**拖一次（按下 → 拖 → 抬起）。
///
/// ⚠️ 坐标是**窗口内**的（xdotool 的 `--window`）⇒ 要乘缩放倍率。
///
/// ⚠️⚠️ **别把 y 写成 x**（2026-11-15 踩过）：`m(from)` 取的是 `.0`（x），
///    所以 y 必须显式用 `from.1` —— 一开始写成 `m((from.0, from.1))`
///    ⇒ 指针被放到 `(x, x)` ⇒ 按下点跑到第一行上 ⇒ 看着像「拖选没选中」。
fn real_drag(winid: &str, from: (f64, f64), to: (f64, f64), scale: f64) {
    let mx = |v: f64| format!("{:.0}", v * scale);
    xdo(&[
        "mousemove",
        "--window",
        winid,
        &mx(from.0),
        &mx(from.1),
    ]);
    pump(120);
    xdo(&["mousedown", "1"]);
    pump(120);
    for i in 1..=6 {
        let x = from.0 + (to.0 - from.0) * i as f64 / 6.0;
        let y = from.1 + (to.1 - from.1) * i as f64 / 6.0;
        xdo(&[
            "mousemove",
            "--window",
            winid,
            &mx(x),
            &mx(y),
        ]);
        pump(40);
    }
    pump(150);
    xdo(&["mouseup", "1"]);
    pump(200);
}

/// 「line 42」⇒ 42（用来判滚动方向）。
fn line_no(row: &str) -> Option<i64> {
    row.trim().strip_prefix("line ")?.trim().parse().ok()
}

fn main() {
    gtk::init().expect("gtk init");

    let view = GhosttyView::new();
    // 回滚缓冲（历史上限）——app 里是 10000 行，这里给 1000 够用。
    view.set_scrollback_lines(1000);
    // 收集「发出去的字节」（按键 ⇒ 远端）。
    let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let o = out.clone();
        view.set_output_handler(move |b| o.borrow_mut().extend_from_slice(b));
    }
    // 灌 100 行（行号写进内容 ⇒ 滚上去能看出是哪几行）。
    let mut buf = String::new();
    for i in 0..100 {
        buf.push_str(&format!("line {i}\r\n"));
    }
    view.feed(buf.as_bytes());

    let win = gtk::Window::new();
    win.set_title(Some("阶段 118 ghostty 滚动冒烟"));
    win.set_default_size(800, 440);
    // ⚠️ **终端上方要留一条空隙**：阶段 122 要验「指针拖到**控件外面**」
    //    ⇒ 指针必须能跑到终端控件的上方，而**又还在窗口里**
    //    （xdotool 的坐标是窗口内的；跑到窗口外会被 X 服务器夹回屏幕边缘
    //    ⇒ 指针根本到不了「控件上方」）。
    //    有了这条空隙，指针停在空隙里 = 停在终端控件**外面** ✓。
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Label::new(Some("↑ 这一条是终端上方的空隙（阶段 122 要用）"));
    root.append(&header);
    root.append(view.widget());
    win.set_child(Some(&root));
    win.present();
    pump(800);

    let (w, h) = (800i32, 400i32);
    let before = snapshot_pixels(&view, w, h);
    assert!(view.viewport_active(), "刚建好时视口应当贴着底部");
    println!("[ok] 初始：视口贴着底部（能看到最后那几行）");

    let winid = win_id("阶段 118 ghostty 滚动冒烟");
    assert!(!winid.is_empty(), "找不到窗口 id（xdotool search 没结果）");
    let scale = surface_scale(view.widget());
    let (vx, vy) = view
        .widget()
        .translate_coordinates(&win, 0.0, 0.0)
        .expect("换算坐标失败");
    println!("窗口缩放倍率 = {scale}");

    /* ---------------- ① 真滚轮往上滚 ---------------- */
    //
    // ⚠️ **必须把指针放在终端上**：滚轮事件发给指针底下的控件。
    xdo(&[
        "mousemove",
        "--window",
        &winid,
        &format!("{:.0}", (vx + 200.0) * scale),
        &format!("{:.0}", (vy + 120.0) * scale),
    ]);
    pump(200);
    for _ in 0..3 {
        xdo(&["click", "4"]); // 4 = 滚轮上
        pump(150);
    }
    pump(400);
    assert!(
        !view.viewport_active(),
        "滚轮往上滚了 3 格，视口还是贴着底部 ⇒ 滚轮根本没接上"
    );
    println!("[ok] 滚轮往上滚 ⇒ 视口离开了底部");

    /* ---------------- ② 画面真的变了 ---------------- */
    let after = snapshot_pixels(&view, w, h);
    assert_ne!(
        before, after,
        "滚上去之后画面一模一样 ⇒ 内容没换（用户还是只能看屏幕那几行）"
    );
    // 留张图（人能直接看「滚上去之后是旧内容」）。
    snapshot_png(
        &view,
        w,
        h,
        std::env::var("PP_GHOSTTY_SHOT").ok().as_deref(),
    );
    println!("[ok] 滚上去之后画面变了（像素级比对）");

    /* ---------------- ③ 在滚上去的画面上拖选历史里的字 ---------------- */
    let (cw, ch) = view.cell_size();
    assert!(cw > 1.0 && ch > 1.0, "格子尺寸不对：({cw}, {ch})");
    // 起点：第 0 行第 0 格；终点：第 0 行第 9 格（够盖住 "line NN"）。
    let x0 = vx + 1.0;
    let y0 = vy + ch / 2.0;
    let x1 = vx + cw * 9.0;
    println!(
        "拖选（滚上去之后的第一行）：({x0:.0}, {y0:.0}) → ({x1:.0}, {y0:.0})"
    );
    real_drag(&winid, (x0, y0), (x1, y0), scale);
    let text = view.selection_text().expect("应当选到文字");
    println!("[ok] 选到的文本 = {text:?}");
    assert!(
        text.starts_with("line "),
        "选到的应当是**历史里**那一行（内容是 line NN），实际：{text:?}"
    );
    println!("[ok] 滚上去之后能选中历史里的字：{text:?}");

    /* ---------------- ③b) 拖选到**边缘外** ⇒ 自动滚动 + 选区扩大（阶段 122） ------ */
    //
    // 用户 2026-11-15：「libvte 中，如果选中文字，然后鼠标一直向上或者向下，
    // 屏幕会跟着滚动，选择范围会一直扩大。但 ghostty 中还不行」。
    //
    // ⚠️ **必须按住不放**（`mousedown` → `mousemove` → **不 `mouseup`**）：
    //    真实拖拽会抬手 ⇒ 拖选结束 ⇒ 自动滚动就停了 ⇒ 那样验不到「持续
    //    滚动」这个行为。
    //
    // ⚠️ 指针要停在**终端控件上方的那条空隙里**（窗口顶部那条 Label）
    //    ⇒ 相对终端控件就是「出界」⇒ 边缘自动滚动该接手了。
    let x0 = vx + cw * 2.0;
    let y0 = vy + ch * 2.0;
    let y_up = vy - 30.0; // ⚠️ **控件上方之外**（相对控件 y 为负 ⇒ 往上滚）
    xdo(&[
        "mousemove",
        "--window",
        &winid,
        &format!("{:.0}", x0 * scale),
        &format!("{:.0}", y0 * scale),
    ]);
    pump(120);
    xdo(&["mousedown", "1"]);
    pump(150);
    let sel_before = view.selection_text().unwrap_or_default();
    let rows_before = view.screen_rows();
    // ⚠️ 拖到**控件上方之外** ⇒ 边缘自动滚动该接手了。
    xdo(&[
        "mousemove",
        "--window",
        &winid,
        &format!("{:.0}", x0 * scale),
        &format!("{:.0}", y_up * scale),
    ]);
    pump(700); // 自动滚动的节拍是 40ms ⇒ 700ms 能滚十几行
    let sel_after = view.selection_text().unwrap_or_default();
    let rows_after = view.screen_rows();
    // 留张图（人能直接看「滚上去了 + 选区扩大了」）。
    snapshot_png(
        &view,
        w,
        h,
        std::env::var("PP_GHOSTTY_SHOT_AUTOSCROLL")
            .ok()
            .as_deref(),
    );
    xdo(&["mouseup", "1"]);
    pump(150);
    assert_ne!(
        rows_before.first(),
        rows_after.first(),
        "指针停在控件上方不动 ⇒ 屏幕**没有继续滚**（阶段 122 的边缘自动滚动没接上）"
    );
    // ⚠️ 方向也要对：指针在**上方** ⇒ 往**更早**的内容滚 ⇒ 首行的行号
    //    必须**变小**（滚反了的话这里会红）。
    let no_before = line_no(rows_before.first().map(|s| s.as_str()).unwrap_or(""));
    let no_after = line_no(rows_after.first().map(|s| s.as_str()).unwrap_or(""));
    assert!(
        no_before.is_some() && no_after.is_some() && no_after < no_before,
        "往上滚的方向不对：滚之前首行是 {rows_before:?}，滚之后是 {rows_after:?}"
    );
    assert!(
        sel_after.len() > sel_before.len(),
        "选区**没有扩大**：拖之前 {:?}，拖之后 {:?}",
        sel_before,
        sel_after
    );
    println!(
        "[ok] 拖到边缘外 ⇒ 屏幕继续滚（首行 {} → {}）+ 选区扩大（{b} → {a} 字符）",
        no_before.unwrap_or(-1),
        no_after.unwrap_or(-1),
        b = sel_before.len(),
        a = sel_after.len()
    );

    /* ---------------- ④ Shift+PageUp/PageDown 也能滚，且不发远端 ------------ */
    out.borrow_mut().clear();
    view.press_key(gdk::Key::Page_Up, gdk::ModifierType::SHIFT_MASK);
    pump(300);
    assert!(
        out.borrow().is_empty(),
        "Shift+PageUp 不该发给远端（VTE 也是自己吃掉）：{:?}",
        out.borrow()
    );
    let after2 = snapshot_pixels(&view, w, h);
    assert_ne!(
        after, after2,
        "Shift+PageUp 之后画面没变 ⇒ 滚动键没接上"
    );
    println!("[ok] Shift+PageUp 能滚、且没发给远端");

    // 往下滚回底部 ⇒ 视口重新贴底。
    for _ in 0..6 {
        view.press_key(gdk::Key::Page_Down, gdk::ModifierType::SHIFT_MASK);
        pump(80);
    }
    pump(300);
    assert!(
        view.viewport_active(),
        "滚了这么多页还没回到底部 ⇒ 视口回不去"
    );
    println!("[ok] Shift+PageDown 滚回了底部");

    println!("ghostty_scroll_smoke: OK");
}
