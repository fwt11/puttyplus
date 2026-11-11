//! 上传时「界面整体卡住」的复现冒烟（阶段 110）。
//!
//! 用户 2026-11-11 报：在堡垒机上上传文件时，界面突然完全卡住（点什么都不
//! 顶用），进度停在 2%，过了很久突然跳到 80%，然后界面才恢复。
//!
//! ⇒ 本冒烟造出两个可能相关的现场条件，并**量主线程有没有被卡住**：
//! 1. `PP_XFER_RATE` —— 限速 TCP 代理（默认 1.5MB/s）⇒ 慢链路；
//! 2. `PP_XFER_FILES` —— 目录里塞 N 个文件（默认 0 = 不塞）⇒ 大列表；
//!
//! ⚠️ **量的不是「两次迭代的间隔」而是「一次 iteration 花了多久」**：间隔里
//!    含着我们自己的 sleep ⇒ 间隔大不一定是被卡了；单次 iteration 的耗时
//!    才是「主线程真的在忙/在等」的证据。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example xfer_stall_smoke
//!
//! 环境变量：
//! - `PP_XFER_RATE`  限速（字节/秒，默认 1500000；0 =不限速）
//! - `PP_XFER_SIZE`  上传文件大小（字节，默认 20MB）
//! - `PP_XFER_FILES` 压力目录里的文件数（默认 0）
//! - `PP_SSH_PORT`   本地测试 sshd 端口（默认 2233）

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::worker::XferState;
use puttyplus::panel::SftpPanel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 跑 GTK 主循环 ms 毫秒（例子没法进 app.run()，只能自己泵）。
fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 限速转发（自己读写，好控制节奏）。
async fn throttled_copy(
    mut r: tokio::net::tcp::OwnedReadHalf,
    mut w: tokio::net::tcp::OwnedWriteHalf,
    rate: u64,
) {
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if w.write_all(&buf[..n]).await.is_err() {
            break;
        }
        // 限速：这一批 n 字节要占 n/rate 秒
        tokio::time::sleep(Duration::from_secs_f64(n as f64 / rate as f64)).await;
    }
    let _ = w.shutdown().await;
}

/// 起限速代理（两个方向都限速）。`rate = 0` ⇒ 不限速。
fn spawn_slow_proxy(listen: u16, target: String, rate: u64) -> u16 {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("代理运行时");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", listen))
                .await
                .expect("代理监听");
            loop {
                let Ok((c, _)) = listener.accept().await else {
                    break;
                };
                let target = target.to_string();
                tokio::spawn(async move {
                    let Ok(s) = tokio::net::TcpStream::connect(&target).await else {
                        return;
                    };
                    let (cr, cw) = c.into_split();
                    let (sr, sw) = s.into_split();
                    if rate == 0 {
                        let mut a = cr;
                        let mut b = sr;
                        let mut c2 = cw;
                        let mut d2 = sw;
                        tokio::join!(tokio::io::copy(&mut a, &mut c2), tokio::io::copy(&mut b, &mut d2));
                    } else {
                        tokio::join!(
                            throttled_copy(cr, sw, rate),
                            throttled_copy(sr, cw, rate)
                        );
                    }
                });
            }
        });
    });
    std::thread::sleep(Duration::from_millis(400));
    listen
}

/// 建一个塞满小文件的目录（模拟「文件列表很长」）。
fn make_stress_dir(n: usize) -> String {
    let dir = format!("/tmp/pp_xfer_stress_{}", std::process::id());
    let _ = std::fs::create_dir_all(&dir);
    for i in 0..n {
        let p = format!("{dir}/f{i:06}.txt");
        if std::fs::write(&p, b"x").is_err() {
            break;
        }
    }
    dir
}

fn main() {
    gtk::init().expect("gtk init");

    // ⚠️ **不许写用户真实配置**（AGENTS.md 第 6 条）。
    for (k, f) in [
        ("PUTTYPLUS_DB_INI", "/tmp/pp_xfer_stall_db.ini"),
        ("PUTTYPLUS_CONN_INI", "/tmp/pp_xfer_stall_conn.ini"),
        ("PUTTYPLUS_HIST_INI", "/tmp/pp_xfer_stall_hist.ini"),
        ("PUTTYPLUS_COLS_INI", "/tmp/pp_xfer_stall_cols.ini"),
        ("PUTTYPLUS_VAULT", "/tmp/pp_xfer_stall_vault.ini"),
    ] {
        unsafe { std::env::set_var(k, f) };
    }

    let rate: u64 = std::env::var("PP_XFER_RATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_500_000);
    let size: u64 = std::env::var("PP_XFER_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20 * 1024 * 1024);
    let files: usize = std::env::var("PP_XFER_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let ssh_port: u16 = std::env::var("PP_SSH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2233);

    // ① 限速代理挡在 sshd 前面
    let proxy_port = spawn_slow_proxy(2299, format!("127.0.0.1:{ssh_port}"), rate);
    println!("限速代理：127.0.0.1:{proxy_port} → 127.0.0.1:{ssh_port}，限速 {rate} B/s");

    // ② 压力目录（可选）
    let stress = if files > 0 {
        let d = make_stress_dir(files);
        println!("压力目录：{d}（{files} 个文件）");
        Some(d)
    } else {
        None
    };

    // ③ 上传用的本地文件（放在压力目录里 ⇒ 「源文件在当前目录」那条路会走）
    let dir = stress.clone().unwrap_or_else(|| "/tmp".to_string());
    let src = format!("{dir}/pp_xfer_stall_{}.bin", std::process::id());
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&src).expect("建临时文件");
        let chunk = vec![0x5au8; 1024 * 1024];
        let mut left = size;
        while left > 0 {
            let n = chunk.len().min(left as usize);
            f.write_all(&chunk[..n]).expect("写临时文件");
            left -= n as u64;
        }
    }
    println!("待上传文件：{src}（{} 字节）", size);

    // ④ 连上面板
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = proxy_port as i32;
    opts.user = std::env::var("USER").unwrap_or_else(|_| "fwt".into());
    let panel = SftpPanel::new(&opts);
    let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);

    // ⚠️⚠️⚠️ **必须把面板真的显示出来**！
    //    面板的控件没挂到窗口上 ⇒ GTK 直接跳过绘制 ⇒ 量到的「主线程耗时」
    //    是假的（只有模型更新，没有真正的画）⇒ 就找不到「界面卡住」的元凶。
    let win = gtk::Window::new();
    win.set_default_size(1000, 700);
    win.set_child(Some(panel.widget().upcast_ref::<gtk::Widget>()));
    win.present();
    pump(500);

    // 等连上
    let deadline = Instant::now() + Duration::from_secs(30);
    while !st.conn_ok.get() && Instant::now() < deadline {
        pump(100);
    }
    if !st.conn_ok.get() {
        eprintln!("连不上本地 sshd（端口 {ssh_port}）⇒ 本冒烟无法跑");
        std::process::exit(2);
    }
    println!("已连接，开始上传");

    // ⑤ 大列表：本地栏指向压力目录，远端栏也指过去
    if let Some(d) = &stress {
        *st.local_cwd.borrow_mut() = d.clone();
        puttyplus::panel::fill_local_pane(&st);
        *st.remote_cwd.borrow_mut() = d.clone();
        puttyplus::panel::on_refresh(&st, true);
        pump(1500);
        let mut n = 0;
        let mut it = st.pane(false).model.iter_first();
        while let Some(mut cur) = it {
            n += 1;
            if st.pane(false).model.iter_next(&mut cur) {
                it = Some(cur);
            } else {
                it = None;
            }
        }
        println!("本地栏 {n} 行 / 远端栏已切到压力目录");
    }

    // ⑥ 开始上传
    let redraw_before: u64 = st.pane(false).full_redraws.get() + st.pane(true).full_redraws.get();
    let draws_before = puttyplus::panel::namecell::draw_count();
    puttyplus::panel::start_uploads(&st, vec![src.clone()]);

    // ⑦ 量主线程：① 一次 iteration 的最长耗时 ② 主循环定时器的抖动
    let ctx = gtk::glib::MainContext::default();
    let ticks: Rc<RefCell<Vec<Instant>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let ticks = Rc::clone(&ticks);
        glib::timeout_add_local(Duration::from_millis(16), move || {
            ticks.borrow_mut().push(Instant::now());
            glib::ControlFlow::Continue
        });
    }
    let mut max_iter = Duration::ZERO;
    let mut worst: Vec<(Duration, i64)> = Vec::new();
    let mut last = Instant::now();
    let mut trace: Vec<(i64, u64)> = Vec::new();
    let mut last_pct = -1i64;
    let mut measuring = false;
    let mut up_start = Instant::now();
    let mut up_end = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let t0 = Instant::now();
        while ctx.pending() {
            ctx.iteration(false);
        }
        let t1 = Instant::now();
        // 进度
        let (done, total, state) = {
            let items = st.items.borrow();
            match items.first() {
                Some(it) => {
                    let g = it.shared.lock().unwrap();
                    (g.done, g.total, g.state)
                }
                None => (0, 0, XferState::Wait),
            }
        };
        let pct = if total > 0 {
            (done * 100 / total) as i64
        } else {
            -1
        };
        // ⚠️ **上传开始之后才统计**（之前那段是建窗口/列目录，不算数）
        if !measuring && pct >= 0 {
            measuring = true;
            max_iter = Duration::ZERO;
            worst.clear();
            up_start = Instant::now();
        }
        if measuring {
            let d = t1 - t0;
            if d > max_iter {
                max_iter = d;
            }
            if d > Duration::from_millis(8) {
                worst.push((d, pct));
                worst.sort_by(|a, b| b.0.cmp(&a.0));
                worst.truncate(8);
            }
        }
        last = t1;
        let _ = last;

        if pct != last_pct {
            trace.push((pct, done));
            println!("  进度 {pct}%（{done}/{total}）最长单次阻塞 {max_iter:?}");
            last_pct = pct;
        }

        match state {
            XferState::Done | XferState::Err | XferState::Skip | XferState::Cancel => {
                up_end = Instant::now();
                break;
            }
            _ => {}
        }
        if Instant::now() > deadline {
            eprintln!("超时：上传没在 600 秒内结束");
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // 定时器抖动：相邻两次 16ms 定时器之间最长隔了多久
    let ticks = ticks.borrow();
    let mut max_tick_gap = Duration::ZERO;
    for w in ticks.windows(2) {
        if w[1] - w[0] > max_tick_gap {
            max_tick_gap = w[1] - w[0];
        }
    }

    let _ = std::fs::remove_file(&src);
    if let Some(d) = &stress {
        let _ = std::fs::remove_dir_all(d);
    }
    let redraws = (st.pane(false).full_redraws.get() + st.pane(true).full_redraws.get())
        - redraw_before;
    println!();
    println!("—— 结果 ——");
    println!("主线程最长单次阻塞（一次 iteration）：{max_iter:?}");
    println!("主循环定时器最长间隔（正常 ≈16ms）：{max_tick_gap:?}");
    println!("最长的几次迭代（耗时, 当时进度%）：{worst:?}");
    println!("上传期间的**整表重绘**次数：{redraws}");
    // ⚠️⚠️ **最硬的一条断言**：数**单元格真正被画了多少次**。
    //    这个数**跟谁触发的重画无关** ⇒ 谁改回整表重画都逃不掉。
    //    正常：每帧只画「正在传的那一行」⇒ 约 1 次/帧；
    //    整表重画：每帧画**所有可见行**（两个面板加起来几十行）⇒ 涨一个数量级。
    let draws = puttyplus::panel::namecell::draw_count() - draws_before;
    let frames = ticks.len().max(1);
    // ⚠️ **按「每秒画多少行」来卡**（与机器快慢无关）：
    //    - 修复后：呼吸动画每帧只画「正在传的那一行」⇒ **≤ 20 次/秒**；
    //    - 整表重画：每帧画**所有可见行**（两个面板几十行）⇒ **几百次/秒**。
    let elapsed_s = up_end.saturating_duration_since(up_start).as_secs_f64().max(0.1);
    let draw_limit = (elapsed_s * 40.0) as u64 + 100;
    println!(
        "上传期间的**单元格绘制**次数：{draws}（{elapsed_s:.1} 秒，上限 {draw_limit}）"
    );
    if draws > draw_limit {
        eprintln!(
            "❌ 上传期间画了 {draws} 次单元格（应 ≤{draw_limit}）⇒ 有人在整表重画 ⇒ 主线程被绘制拖死 ⇒ 界面会卡住"
        );
        std::process::exit(101);
    }
    // ⚠️⚠️ **「一台服务器一条连接」这条约束不能被破坏**（用户 2026-11-11
    //    拍板）：传输另开了一条 **SFTP 通道**，但**底层还是同一条 SSH 连接**
    //    ⇒ 活着的连接数必须还是 1。
    let conns = puttyplus::backend::hub::live_connections();
    println!("活着的 SSH 连接：{conns:?}");
    if conns.len() != 1 {
        eprintln!(
            "❌ 活着的 SSH 连接有 {} 条（应 1 条）⇒ 破坏了「一台服务器一条连接」",
            conns.len()
        );
        std::process::exit(101);
    }
    let jumps: Vec<String> = trace
        .iter()
        .map(|(p, d)| format!("{p}%({d})"))
        .collect();
    println!("进度轨迹：{}", jumps.join(" "));

    // 断言：主线程不该被卡住超过 1.5 秒（用户现场是「几十秒完全无响应」）
    let limit = Duration::from_millis(1500);
    if max_iter > limit {
        eprintln!("❌ 主线程被卡了 {max_iter:?}（超过 {limit:?}）⇒ 上传期间界面会完全无响应");
        std::process::exit(101);
    }
    if max_tick_gap > Duration::from_millis(1500) {
        eprintln!("❌ 主循环定时器最长隔了 {max_tick_gap:?} ⇒ 界面会长时间没反应");
        std::process::exit(101);
    }
    // ⚠️⚠️ **这条才是「上传时界面卡死」的正身**（用户 2026-11-11）：
    //    传输期**不能整表重绘**。以前每个进度 tick（64KB 一次）+ 呼吸动画
    //    每一帧都 `view.queue_draw()` ⇒ 整棵文件列表重画 ⇒ 主线程被绘制
    //    拖死 ⇒ 堡垒机的远程桌面上整个界面卡住（菜单也点不动）⇒ 积压的
    //    事件一起回放 ⇒「进度从 2% 突然跳到 80%」。
    //    ⇒ 现在只标脏「正在传的那一行」⇒ 一次上传里整表重绘应该只有
    //    目录刷新那几次。
    if redraws > 8 {
        eprintln!(
            "❌ 上传期间整表重绘了 {redraws} 次（应 ≤8）⇒ 主线程被绘制拖死 ⇒ 界面会卡住"
        );
        std::process::exit(101);
    }
    println!(
        "✅ 主线程最长阻塞 {max_iter:?}、定时器最长间隔 {max_tick_gap:?}、整表重绘 {redraws} 次（都在 {limit:?} / 8 次以内）"
    );
}
