//! ghostty 自绘终端控件冒烟（跨平台版 阶段 C3）。
//!
//! 验五件事（都是**真路径**，不是另写一份模拟）：
//!
//! 1. **屏幕内容对**：喂一段 ANSI，网格里的字符就该是那样；
//! 2. **按键发出的字节对**：回车 / Ctrl+C / 方向键；
//! 3. **中文 commit 发的是 UTF-8 字节**（不是问号、不是半个字）；
//! 4. **画出来的像素对**：文字行必须落在网格行里、光标和文字要对齐
//!    （2026-11-08 用户报「光标跑到提示符上一行」——网格是对的，
//!    文字被画低了一行 ⇒ 网格断言抓不住，渲到内存 surface 上数像素
//!    才抓得住。字形长什么样由字体决定，那个才真的只能人眼比）；
//! 5. **光标会闪**（2026-11-09 用户报「光标是静止的，希望能闪烁」）：
//!    聚焦后实心/灭两个相位轮流出现，敲键立刻回到实心。
//!
//! 另外截一张图（`/tmp/pp_ghostty_view.png`）供人工看画面。
//!
//! 运行：
//! ```text
//! cargo run --example ghostty_view_smoke --features ghostty
//! ```
//! ⚠️ 需要 **Zig 0.17**（编译 libghostty-vt 用）。
//! ⚠️ 第 5 节要**真桌面**（焦点）：Windows 桌面直接跑；Linux 无头环境
//!    得有窗口管理器（裸 xvfb 不行，xvfb + 轻量 WM 才行）。

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use puttyplus::termview::ghostty_view::GhosttyView;

/// 喂给终端的测试数据（带颜色、带定位）。
const SAMPLE: &[u8] = b"\x1b[31mred\x1b[0m \x1b[42mgreenbg\x1b[0m\r\nsecond line\x1b[3;5Hmarker";

/// 泵一会儿主循环。
fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn main() {
    gtk::init().expect("gtk init");

    let view = GhosttyView::new();
    // 收集「发出去的字节」（将来这就是写给子进程的东西）
    let out: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let o = out.clone();
        view.set_output_handler(move |b| o.borrow_mut().extend_from_slice(b));
    }

    let win = gtk::Window::new();
    win.set_title(Some("ghostty 自绘终端 — 阶段 C3"));
    win.set_default_size(900, 560);
    win.set_child(Some(view.widget()));
    win.present();
    pump(600);

    /* ---------------- 1. 屏幕内容 ---------------- */
    view.feed(SAMPLE);
    pump(400);
    let rows = view.screen_rows();
    println!("[ok] 网格行数：{}", rows.len());
    for (i, r) in rows.iter().enumerate().take(4) {
        println!("      第 {i} 行：{r:?}");
    }
    assert_eq!(
        rows.first().map(String::as_str),
        Some("red greenbg"),
        "第 0 行不对"
    );
    assert_eq!(
        rows.get(1).map(String::as_str),
        Some("second line"),
        "第 1 行不对"
    );
    assert_eq!(
        rows.get(2).map(String::as_str),
        Some("    marker"),
        "第 2 行不对（ESC[3;5H 定位 + 4 个空格）"
    );
    println!("[ok] 屏幕内容：red greenbg / second line / '    marker'");

    /* ---------------- 1b. 中文（宽字符） ---------------- */
    view.feed("\r\n中文测试 wide".as_bytes());
    pump(400);
    let rows = view.screen_rows();
    let cn_row = rows
        .iter()
        .find(|r| r.contains("中文"))
        .expect("屏幕上应该有中文");
    println!("[ok] 中文行：{cn_row:?}");
    assert!(
        cn_row.starts_with("中文测试 wide"),
        "中文行不对：{cn_row:?}"
    );
    // ⚠️ 宽字符的「尾巴格」不能变成空格（那会让中文后面多一格）
    assert!(
        !cn_row.contains("文 测"),
        "中文之间多了空格（宽字符尾巴格没处理）：{cn_row:?}"
    );
    println!("[ok] 中文：宽字符占 2 列、尾巴格不占位");

    /* ---------------- 2. 按键 ⇒ 字节 ---------------- */
    let mut check = |name: &str, key: gdk::Key, mods: gdk::ModifierType, want: &[u8]| {
        out.borrow_mut().clear();
        view.press_key(key, mods);
        let got = out.borrow().clone();
        assert_eq!(
            got, want,
            "{name} 发出的字节不对：期望 {want:?}，实际 {got:?}"
        );
        println!("[ok] {name} ⇒ {want:?}");
    };
    check("回车", gdk::Key::Return, gdk::ModifierType::empty(), b"\r");
    check(
        "Ctrl+C",
        gdk::Key::c,
        gdk::ModifierType::CONTROL_MASK,
        b"\x03",
    );
    check("上箭头", gdk::Key::Up, gdk::ModifierType::empty(), b"\x1b[A");
    check("退格", gdk::Key::BackSpace, gdk::ModifierType::empty(), b"\x7f");
    check("空格", gdk::Key::space, gdk::ModifierType::empty(), b" ");

    /* ---------------- 3. 中文 commit ⇒ UTF-8 ---------------- */
    out.borrow_mut().clear();
    view.commit_text("中文");
    let got = out.borrow().clone();
    assert_eq!(
        got,
        "中文".as_bytes(),
        "中文 commit 发出的不是 UTF-8 字节：{got:?}"
    );
    println!("[ok] 中文 commit ⇒ UTF-8 字节（{} 字节）", got.len());

    /* ---------------- 4. 没有输出回调 ⇒ 字节必须写给子进程 ---------------- */
    //
    // ⚠️ 这一条钉的是**阶段 C8 的真 bug**：以前按键字节只走
    // `set_output_handler` 设的回调，而**生产代码路径从来没设过它**
    // （只有这个冒烟设了，用来收集字节）⇒ 真人敲键盘的字节被**直接
    // 丢掉**：程序喂进去的字节能出字（那条路走 `feed_child`），真键盘
    // 没反应——Windows 上表现为「终端打不出字」。
    //
    // ⇒ 这里**刻意不设**输出回调 ⇒ 字节必须走 pty ⇒ 回显出来。
    #[cfg(target_os = "linux")]
    {
        let pty = puttyplus::platform::pty::PtySession::new().expect("开 pty");
        // 起一个会回显的子进程（cat）
        let env: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
        let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
        pty.spawn_async("/tmp", &["cat"], &env_refs, |r| match r {
            Ok(_) => println!("[ok] cat 起来了"),
            Err(e) => println!("[warn] cat 起不来：{e}"),
        });
        pump(600);

        let view2 = GhosttyView::new();
        // ⚠️ **pty 的读线程归 view2**（`attach_pty` 自己起一个）⇒
        //    回显会喂给 ghostty 内核 ⇒ **上屏**。
        //    ⚠️ **不能再另起一个读线程**：两个读者抢同一个 master fd
        //    ⇒ 字节被谁读到是**随机的** ⇒ 断言会时红时绿（实测踩过）。
        view2.attach_pty(&pty);
        pump(400);
        // ⚠️ **没有** set_output_handler ⇒ 只能走 pty
        view2.press_key(gdk::Key::a, gdk::ModifierType::empty());
        pump(1000);
        let rows = view2.screen_rows();
        assert!(
            rows.iter().any(|r| r.contains('a')),
            "没有输出回调时，按键字节必须写给子进程（pty 的回显要上屏）：{rows:?}"
        );
        println!("[ok] 没有输出回调 ⇒ 按键字节写给了子进程（回显上屏：{rows:?}）");
    }

    /* ---------------- 5. 像素对齐：文字行必须落在网格行里 ---------------- */
    //
    // ⚠️ 这一条钉的是**2026-11-08 的真 bug**：`draw_text` 把 pangocairo
    // 的当前点当基线用（实际是排版框左上角）⇒ 每行文字整体被压低约
    // 0.8 个格子，视觉上「光标跑到提示符的上一行」。网格（screen_rows）
    // 是对的 ⇒ 上面的文字断言全抓不住 ⇒ 必须把画面真渲出来数像素。
    //
    // 手法：喂三行细笔画字（`ill`——笔画只有 1~2px 宽，不会和光标的
    // 实心块混淆；光标落在第 2 行），渲到内存 surface，按行扫「非背景」
    // 像素 ⇒ 得到每行字的墨迹带与光标块位置 ⇒ 断言：
    //   a. 第 0 行的墨迹顶在**第一个格子**里（画歪了会掉到 ≈1 行距以下）；
    //   b. 第 1 行墨迹顶 ≈ 一个行距；
    //   c. 光标块顶 ≈ 两个行距（第 2 行）；
    //   d. 第 2 行墨迹与光标顶相差**不到半个行距**（同一个格子里）。
    // 全部用「行距的倍数」表达 ⇒ 不依赖具体字体与 DPI。
    {
        let view2 = GhosttyView::new();
        view2.feed(b"ill\r\nill\r\nill");
        pump(100);

        let (w, h) = (320i32, 140i32);
        let mut surf = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
            .expect("建内存 surface");
        {
            let cr = gtk::cairo::Context::new(&surf).expect("建 cairo 上下文");
            view2.render_to(&cr, w as f64, h as f64);
        }
        // ⚠️ 读像素前：Context 要先 drop（data() 要求**独占**访问，
        //    context 还活着就报 NonExclusive）；flush 要先做（还没冲刷
        //    的绘制不落进 surface）；stride 要在 data() 前取（data() 是
        //    对 surface 的可变借用，活着的时候 surf 不能再碰）。
        surf.flush();
        let stride = surf.stride() as usize;
        let data = surf.data().expect("读 surface 像素");

        // 留个图（PPM，零依赖），人工也能看（断言挂了先看这张图）
        let ppm = std::env::temp_dir().join("pp_ghostty_pixel.ppm");
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

        // 像素读取（cairo ARgb32 在小端机器上的字节序是 b,g,r,a）。
        // 背景色取右下角采样（那里必然没画东西）⇒ 不依赖主题色。
        let px = |x: usize, y: usize| -> (i32, i32, i32) {
            let o = y * stride + x * 4;
            (data[o + 2] as i32, data[o + 1] as i32, data[o] as i32)
        };
        let (bgr, bgg, bgb) = px((w - 1) as usize, (h - 1) as usize);
        let is_bg = |x: usize, y: usize| -> bool {
            let (r, g, b) = px(x, y);
            (r - bgr).abs() < 14 && (g - bgg).abs() < 14 && (b - bgb).abs() < 14
        };

        // 每行统计：非背景像素数 + 最长连续非背景横向 run
        //（光标是实心一整格宽的块，run 比任何细笔画都粗）
        let (mut row_ink, mut row_run) = (vec![0usize; h as usize], vec![0usize; h as usize]);
        for y in 0..h as usize {
            let mut run = 0usize;
            for x in 0..w as usize {
                if is_bg(x, y) {
                    run = 0;
                } else {
                    run += 1;
                    row_ink[y] += 1;
                    row_run[y] = row_run[y].max(run);
                }
            }
        }

        // 光标块：run ≥ 6 且**连续 6 行都是**（块是整格高 ⇒ 竖向连续；
        // 字形再粗也撑不满一格宽，更撑不满连续 6 行）
        let is_cursor_row = |y: usize| -> bool {
            y + 6 <= h as usize && (y..y + 6).all(|yy| row_run[yy] >= 6)
        };
        let cursor_top = (0..h as usize).find(|&y| is_cursor_row(y));

        // 墨迹带（连续的「有字」行）
        let mut bands: Vec<(usize, usize)> = Vec::new();
        let mut start: Option<usize> = None;
        for y in 0..h as usize {
            match (row_ink[y] > 3, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    bands.push((s, y - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            bands.push((s, h as usize - 1));
        }
        let diag = format!(
            "墨迹带 {bands:?}；光标顶 {:?}；每行墨迹 {:?}（图：{}）",
            cursor_top,
            row_ink.iter().enumerate().filter(|(_, c)| **c > 0).map(|(y, c)| format!("{y}:{c}")).collect::<Vec<_>>(),
            ppm.display()
        );
        assert!(bands.len() >= 3, "至少要有三行字的墨迹带：{diag}");
        // 行距从**前两条纯文字带**的顶部差推（第 2 行的带会和光标并成
        // 一条，顶部是光标块的顶边，比文字墨迹高，不能用）
        let pitch = bands[1].0 - bands[0].0;
        assert!(
            (8..60).contains(&pitch),
            "行距 {pitch}px 不像正常的字号：{diag}"
        );

        // a. 第 0 行墨迹必须落在第一个格子里
        assert!(
            (bands[0].0 as f64) < 0.7 * pitch as f64,
            "第 0 行文字墨迹顶在 {}px、行距 {pitch}px ⇒ 文字整体被画低了\
             （应落在第一个格子里，即 < {:.0}px）：{diag}",
            bands[0].0,
            0.7 * pitch as f64
        );

        // b. 第 1 行墨迹顶 ≈ 一个行距
        assert!(
            ((bands[1].0 as f64) - pitch as f64).abs() < 0.4 * pitch as f64,
            "第 1 行墨迹顶在 {}px、行距 {pitch}px ⇒ 没落在第 1 行的格子里：{diag}",
            bands[1].0
        );

        // c. 光标块顶 ≈ 两个行距（在第 2 行）
        let cursor_top = cursor_top.expect("扫不到光标块（连续 6 行的实心块）");
        assert!(
            ((cursor_top as f64) - 2.0 * pitch as f64).abs() < 0.45 * pitch as f64,
            "光标顶在 {cursor_top}px、行距 {pitch}px ⇒ 光标没落在第 2 行的格子里：{diag}"
        );

        // d. 第 2 行墨迹与光标必须同格（相差不到半个行距）
        assert!(
            ((bands[2].0 as f64) - cursor_top as f64).abs() < 0.5 * pitch as f64,
            "第 2 行墨迹顶 {}px 与光标顶 {cursor_top}px 相差超过半个行距（{pitch}px）\
             ⇒ 文字和光标没对齐（老 bug：文字整体低一行）：{diag}",
            bands[2].0
        );
        println!(
            "[ok] 像素对齐：行距 {pitch}px，三行墨迹顶 {} / {} / {}，光标顶 {cursor_top}px",
            bands[0].0, bands[1].0, bands[2].0
        );
    }

    /* ---------------- 6. 光标会闪（默认开启） ---------------- */
    //
    // ⚠️ 这一条钉的是**2026-11-09 的真反馈**：光标常亮不闪。内核内置
    // 默认是「不闪」（`libghostty` 的观感）⇒ 建**内核**时
    // `set_default_cursor_blink(Some(true))`；渲染层加**相位定时器**：
    // 半周期切换一次，敲键/拿回焦点时重置为实心。
    //
    // 闪烁只在**有焦点**时发生 ⇒ 视图塞进窗口、present、抢焦点。
    // 判定用真渲染：实心相位上「连续 6 行 run≥6」的实心块在，
    // 灭相位上它没了。窗口宽裕（±1 个相位都不敏感），时序靠
    // **轮询到状态出现为止**，不做脆弱的「睡固定毫秒」。
    {
        let view2 = GhosttyView::new();
        view2.feed(b"ill\r\nill\r\nill");
        let win = gtk::Window::new();
        win.set_default_size(320, 140);
        win.set_child(Some(view2.widget()));
        win.present();
        pump(300);
        view2.area().grab_focus();
        pump(200);

        // ① 刚抢到焦点 ⇒ 实心（相位从头计）
        assert!(
            cursor_block_present(&view2, 320, 140),
            "聚焦后光标应该先实心"
        );

        // ② 泵着等它灭（两个相位内必灭；「不闪」的话永远等不到 ⇒ 失败）
        assert!(
            wait_cursor_state(&view2, 320, 140, false, 2200),
            "光标该在两个相位内灭一次（不闪 = 一直实心）"
        );

        // ③ 再等它亮回来
        assert!(
            wait_cursor_state(&view2, 320, 140, true, 2200),
            "光标该在下一个相位亮回来"
        );

        // ④ 敲键 ⇒ 相位重置，立刻回实心
        view2.press_key(gdk::Key::a, gdk::ModifierType::empty());
        assert!(
            cursor_block_present(&view2, 320, 140),
            "敲键后光标必须立刻实心（相位重置）"
        );
        println!("[ok] 光标闪烁：实心/灭两个相位轮流出现，敲键重置为实心");
    }

    /* ---------------- 截图（人工看画面） ---------------- */
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg("/tmp/pp_ghostty_view.png")
        .status();
    println!("截图：/tmp/pp_ghostty_view.png");

    println!("\nghostty_view_smoke: 全部通过");
}

/* ---------------- 闪烁判定（第 6 节用） ---------------- */

/// 渲一帧，判断画面上有没有**实心光标块**（连续 6 行、每行最长
/// 非背景横向 run ≥ 6 —— 文字笔画撑不满一格宽，实心块撑得满）。
fn cursor_block_present(view: &GhosttyView, w: i32, h: i32) -> bool {
    let mut surf = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)
        .expect("建内存 surface");
    {
        let cr = gtk::cairo::Context::new(&surf).expect("建 cairo 上下文");
        view.render_to(&cr, w as f64, h as f64);
    }
    surf.flush();
    let stride = surf.stride() as usize;
    let data = surf.data().expect("读 surface 像素");

    // 背景色取右下角采样（那里必然没画东西）⇒ 不依赖主题色
    let px = |x: usize, y: usize| -> (i32, i32, i32) {
        let o = y * stride + x * 4;
        (data[o + 2] as i32, data[o + 1] as i32, data[o] as i32)
    };
    let (bgr, bgg, bgb) = px((w - 1) as usize, (h - 1) as usize);

    let mut consecutive = 0usize;
    for y in 0..h as usize {
        let mut run = 0usize;
        let mut best = 0usize;
        for x in 0..w as usize {
            let (r, g, b) = px(x, y);
            let bg = (r - bgr).abs() < 14 && (g - bgg).abs() < 14 && (b - bgb).abs() < 14;
            if bg {
                run = 0;
            } else {
                run += 1;
                best = best.max(run);
            }
        }
        if best >= 6 {
            consecutive += 1;
        } else {
            consecutive = 0;
        }
        if consecutive >= 6 {
            return true;
        }
    }
    false
}

/// 泵主循环直到画面上光标达到想要的状态（实心 `true` / 灭 `false`）
/// 或超时——闪烁是**相位轮换**，判定用轮询而不是「睡固定毫秒」，
/// 相位边界飘一点也不影响。
fn wait_cursor_state(view: &GhosttyView, w: i32, h: i32, want: bool, timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if cursor_block_present(view, w, h) == want {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        pump(40);
    }
}
