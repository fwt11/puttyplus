//! 文件列表「大小」列的列宽冒烟（2026-11-10，用户反馈）。
//!
//! **用户反馈**：「如果文件列表中显示了文件大小列，那么一定要保证最长的
//! 大小显示完整，就是列宽按最长的文件大小来定」——截图里 `104.4 …`、
//! `253.1 …` 被截断，单位都没了，用户根本看不出文件多大，「列宽也没
//! 办法手动调整」。
//!
//! **断言**：
//! 1. 填完列表后，**大小列的宽度 ≥ 最长的那行大小文字的实测宽度**
//!    ⇒ 最长的一定显示完整（不会被省略号削掉）；
//! 2. 用户**手动拉宽**过 ⇒ 尊重他的宽度（不被弹回去）；
//! 3. 用户把它**拉窄**到比内容还窄 ⇒ 弹回「装得下内容」的宽度
//!    ⇒ 「最长的一定完整」是**保证**，不是「默认如此」；
//! 4. **横向滚动条是自己画的那条**（`pp-hscroll`）：内容溢出时可见、
//!    够粗（≥10px）⇒ 好抓（用户：「很难选中并拖动」）；
//! 5. 横向溢出时 ScrolledWindow **不画**自己那条（否则两条叠着），
//!    竖向照旧（overlay，不吃文件名宽度）。
//!
//! ```text
//! xvfb-run -a cargo run --example size_col_smoke
//! ```

use gtk::prelude::*;
use puttyplus::panel::pane::{self, RowData};
use puttyplus::panel::SftpPanel;
use puttyplus::args::ConnOptions;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn rows() -> Vec<RowData> {
    // 大小跨度拉开：字节、KB、MB、GB 都要有 ⇒ 最长的一定是「xxx.x MB/GB」
    let specs: [(&str, u64); 5] = [
        ("tiny.bin", 512),
        ("small.bin", 1024 * 1024),
        ("medium.bin", 104_857_600),
        ("large.bin", 5_368_709_120),
        ("一个中文名.bin", 1024 * 1024 * 300),
    ];
    specs
        .iter()
        .map(|(name, size)| RowData {
            name: name.to_string(),
            size: *size,
            is_dir: false,
            perm: "-rw-r--r--".into(),
            mtime: 1_767_225_600,
            hidden: false,
        })
        .collect()
}

fn main() {
    // ⚠️ **先指走设置文件**：这个例子会写「记住的列宽」，不许写用户真实配置。
    let dir = std::env::temp_dir().join(format!("pp-size-col-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        std::env::set_var("PUTTYPLUS_SETTINGS_INI", dir.join("settings.ini"));
    }

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let mut fails: Vec<String> = Vec::new();
    macro_rules! check {
        ($name:expr, $cond:expr, $detail:expr) => {{
            if $cond {
                println!("[ok] {}", $name);
            } else {
                println!("[FAIL] {}：{}", $name, $detail);
                fails.push(format!("{}：{}", $name, $detail));
            }
        }};
    }

    // ⚠️ **先写一个「记住的列宽」**：150px。
    //    以前 `set_fixed_width` 落在 GrowOnly 的列上会被**忽略** ⇒ 用户拖过
    //    的列宽记进了设置却恢复不了（用户：「列宽也没办法手动调整」）。
    let saved_w = 150i32;
    puttyplus::settings::set_col_widths(
        true,
        puttyplus::settings::ColWidths {
            size: Some(saved_w),
            perm: None,
            mtime: None,
        },
    );

    // ---- 造一个真的远程面板 ----
    let mut opts = ConnOptions::default();
    opts.proto = "ssh".into();
    opts.host = "127.0.0.1".into();
    opts.port = 2233;
    opts.user = "fwt".into();
    let panel = SftpPanel::new(&opts);
    let st = std::rc::Rc::clone(&panel.st);
    // ⚠️ 用 `SftpPanel::new` **自己建好的**那个远程面板（`st.remote_pane`）
    //    —— 自己再 build 一个会把 Paned 的两端换掉（Paned 只剩一端时
    //    `set_position` 不起作用 ⇒ 压根不溢出 ⇒ 测不到滚动条）。
    let p = st
        .remote_pane
        .borrow()
        .clone()
        .expect("SftpPanel::new 应当建好远程面板");

    // 放进窗口里（不摆进窗口 ⇒ GTK 不分配尺寸，量出来的宽度是 0）
    let win = gtk::ApplicationWindow::builder()
        .default_width(420)
        .default_height(500)
        .build();
    // ⚠️ 摆 **面板的根 Paned**（`build_pane` 把面板挂进它）⇒ 列才会被布局
    win.set_child(Some(&st.root));
    win.present();
    pump(300);
    // ⚠️ **把面板钉窄**（340px = 面板默认宽）⇒ 列加起来比它宽 ⇒ 横向溢出
    //    ⇒ 才会出现横向滚动条（不钉窄的话窗口够宽，压根不溢出）
    st.root.set_position(210);
    pump(300);
    println!("面板宽 {}px，Paned 位置 {}", p.sw.width(), st.root.position());

    // ---- 1. 填列表 ⇒ 大小列按最长的那个定宽 ----
    pane::fill_model(&p, true, rows());
    pump(200);

    // ⚠️ **记住的列宽要能恢复**（sizing 是 Fixed 才算数）
    check!(
        "记住的大小列宽能恢复（用户拖过的宽度不丢）",
        p.col_size.width() >= saved_w,
        format!("恢复了 {}px < 记住的 {saved_w}px", p.col_size.width())
    );

    // 最长的大小文案（这一批里最宽的那个）
    let texts: Vec<String> = vec![
        "512 B".into(),
        "1.0 MB".into(),
        "100.0 MB".into(),
        "5.0 GB".into(),
        "300.0 MB".into(),
    ];
    let mut need = 0i32;
    for t in &texts {
        let (w, _) = p.view.create_pango_layout(Some(t)).pixel_size();
        need = need.max(w);
    }
    let col_w = p.col_size.width();
    println!("最长的大小文字要 {need}px（加内边距 ⇒ 至少 {}px），列宽 {col_w}px", need + 12);
    check!(
        "大小列装得下最长的大小文字",
        col_w >= need + 8,
        format!("列宽 {col_w}px < 需要 {}px", need + 8)
    );

    // ---- 2. 宽度被弄成装不下内容 ⇒ 顶回「装得下」的宽度 ----
    //
    // ⚠️ 这一条钉的是「**最长的一定显示完整**」：宽度不够时 GTK 会把文字
    //    省略成「104.4 …」⇒ 用户根本看不出文件多大。
    //
    // ⚠️ 先把 `min_width` 放开：不然上一次算出来的下限会把「拉窄」挡掉
    //    （那正是它的作用，只是这里要单独验「顶回」这条）。
    p.col_size.set_min_width(0);
    p.col_size.set_fixed_width(40);
    pump(120);
    pane::fill_model(&p, true, rows());
    pump(200);
    let back = p.col_size.width();
    check!(
        "宽度装不下内容时被顶回「装得下最长大小」的宽度",
        back >= need + 8,
        format!("顶回 {back}px < 需要 {}px", need + 8)
    );

    // ---- 3. 用户手动拉宽 ⇒ 尊重 ----
    p.col_size.set_fixed_width(back + 60);
    pump(120);
    let wide = p.col_size.width();
    pane::fill_model(&p, true, rows());
    pump(200);
    check!(
        "用户拉宽过的宽度被尊重",
        p.col_size.width() >= wide,
        format!("拉宽到 {wide}px，刷新后变成 {}px", p.col_size.width())
    );

    // ---- 4. 横向滚动条：自己画的那条，够粗、溢出时可见 ----
    check!(
        "横向滚动条是自己画的那条（pp-hscroll）",
        p.h_bar.has_css_class("pp-hscroll"),
        "没有 pp-hscroll 类"
    );
    // ⚠️ **先把它显示出来再量**：隐藏的 widget 不参与布局 ⇒ 量出来是 0
    //    （平时它只在横向溢出时才出现 ⇒ 隐藏时量不得）。
    let was_visible = p.h_bar.is_visible();
    p.h_bar.set_visible(true);
    pump(80);
    let h = p.h_bar.height();
    if !was_visible {
        p.h_bar.set_visible(false);
    }
    println!("自绘横向滚动条高度 {h}px");
    check!(
        "横向滚动条够粗（≥10px，好抓）",
        h >= 10,
        format!("只有 {h}px")
    );

    // 内容溢出 ⇒ 可见（面板窄，列加起来比视口宽）
    let adj = p.sw.hadjustment();
    let over = adj.upper() - adj.page_size();
    println!("横向可滚动量 {over:.1}px（upper={:.1} page={:.1}）", adj.upper(), adj.page_size());
    check!(
        "内容溢出时自绘滚动条可见",
        p.h_bar.is_visible(),
        format!("可滚动量 {over:.1}px 却看不见")
    );

    // ---- 5. ScrolledWindow 不再画自己那条横向滚动条（否则两条叠着）----
    check!(
        "ScrolledWindow 横向是 External（它自己不画，那条是我们自己画的）",
        p.sw.hscrollbar_policy() == gtk::PolicyType::External,
        format!("策略是 {:?}", p.sw.hscrollbar_policy())
    );
    check!(
        "竖向照旧（Automatic + overlay，不吃文件名宽度）",
        p.sw.vscrollbar_policy() == gtk::PolicyType::Automatic,
        format!("策略是 {:?}", p.sw.vscrollbar_policy())
    );

    win.close();

    if fails.is_empty() {
        println!("size_col smoke ok");
    } else {
        println!("size_col smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
