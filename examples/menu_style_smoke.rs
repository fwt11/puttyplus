//! 菜单风格冒烟（阶段 35）：所有菜单都得跟终端右键菜单看齐。
//!
//! 查三件事：
//!
//! 1. 数据库对象树的右键菜单**每一项**都是统一构件建的
//!    （`pp-menu-item` 类 + 图标 + 挂着 hover 控制器）；
//! 2. 「删除表」是危险项（红字 `pp-menu-danger`）；
//! 3. **鼠标真的划过菜单项会高亮**——用 xdotool 把真指针挪到菜单项上，
//!    看它有没有拿到 `pp-menu-hover` 类。用户 2026-10-28 报的
//!    「连上数据库后左侧列表的菜单划过不高亮」就是这一条。
//!
//! 用法（需要 X 显示，无桌面时用 `xvfb-run -a`）：
//!
//! ```text
//! cargo run --example menu_style_smoke
//! ```
//!
//! 退出码 0 = 全过；非 0 = 有检查没过（stdout 里逐条打印）。

use gtk::prelude::*;
use puttyplus::backend::db::DbTable;
use puttyplus::panel::dbtree::DbTree;
use puttyplus::panel::worker::{DbMetaPayload, DbMetaReq};
use puttyplus::theme::{self, Theme};
use std::rc::Rc;

const TITLE: &str = "菜单风格检查";

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Dark);

    let tree = DbTree::new();
    let win = gtk::Window::new();
    win.set_title(Some(TITLE));
    win.set_default_size(900, 560);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bx.append(&tree.view);
    win.set_child(Some(&bx));
    win.present();

    // 假后端：模式 + 一张表 + 一个视图（够验证菜单项的敏感性）。
    {
        let t2 = Rc::clone(&tree);
        tree.set_request_handler(move |req| {
            match &req {
                DbMetaReq::Schemas => {
                    t2.fill(&req, &DbMetaPayload::Schemas(vec!["public".into()]));
                }
                DbMetaReq::Tables { schema } => {
                    t2.fill(
                        &req,
                        &DbMetaPayload::Tables(vec![
                            DbTable {
                                schema: schema.clone(),
                                name: "t_users".into(),
                                is_view: false,
                            },
                            DbTable {
                                schema: schema.clone(),
                                name: "v_active".into(),
                                is_view: true,
                            },
                        ]),
                    );
                }
                DbMetaReq::Routines { .. } => {
                    t2.fill(&req, &DbMetaPayload::Routines(vec![]));
                }
                DbMetaReq::Triggers { .. } => {
                    t2.fill(&req, &DbMetaPayload::Triggers(vec![]));
                }
                _ => {}
            }
        });
    }

    // 窗口 realized 之后再填树：展开 → 懒加载 → 菜单才有内容可测。
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        tree.reset("demo", "PostgreSQL 18.6");
        tree.expand_row_by_name("public");
        // 模式下的表是懒加载的：填完再展开一次才看得到
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            tree.expand_row_by_name("public");
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(120),
                move || {
                    if !tree.popup_menu_for("t_users") {
                        eprintln!("FAIL: 树里找不到 t_users，右键菜单没弹出来");
                        gtk::glib::MainLoop::new(None, false).quit();
                        std::process::exit(1);
                    }
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(250),
                        move || run_checks(&tree),
                    );
                },
            );
        });
    });

    gtk::glib::MainLoop::new(None, false).run();
}

/// 逐条检查 + 报告。
fn run_checks(tree: &Rc<DbTree>) {
    let mut fail = 0usize;

    let items = tree.menu_items();
    println!("== 对象树右键菜单：{} 项 ==", items.len());
    for (label, btn) in &items {
        let has_class = btn.has_css_class("pp-menu-item");
        let has_icon = btn
            .child()
            .and_then(|c| c.downcast::<gtk::Box>().ok())
            .and_then(|row| row.first_child())
            .map(|c| c.is::<gtk::Image>())
            .unwrap_or(false);
        let has_motion = has_motion_controller(btn);
        let ok = has_class && has_icon && has_motion;
        if !ok {
            fail += 1;
        }
        println!(
            "  [{}] {label}  类={has_class} 图标={has_icon} hover控制器={has_motion}",
            if ok { "ok" } else { "FAIL" }
        );
    }
    if items.len() < 11 {
        println!("  [FAIL] 菜单项数量不对（期望 >= 11，实际 {}）", items.len());
        fail += 1;
    }

    // 危险项：删除表要标红
    match items.iter().find(|(l, _)| l == "删除表") {
        Some((_, b)) => {
            let ok = b.has_css_class("pp-menu-danger");
            if !ok {
                fail += 1;
            }
            println!(
                "  [{}] 删除表 是危险项（pp-menu-danger）",
                if ok { "ok" } else { "FAIL" }
            );
        }
        None => {
            println!("  [FAIL] 菜单里没有「删除表」");
            fail += 1;
        }
    }

    // 弹层本身要有卡片样式（pp-menu）
    let pop_ok = tree
        .menu_popover()
        .map(|p| p.has_css_class("pp-menu"))
        .unwrap_or(false);
    if !pop_ok {
        fail += 1;
    }
    println!(
        "  [{}] 弹层带 pp-menu 卡片样式",
        if pop_ok { "ok" } else { "FAIL" }
    );

    // ⭐ 真指针 hover：把指针挪到某个菜单项上，看它有没有拿到 pp-menu-hover
    let hovered = hover_check(tree);
    if !hovered {
        fail += 1;
    }
    println!(
        "  [{}] 鼠标划过菜单项会高亮（pp-menu-hover）",
        if hovered { "ok" } else { "FAIL" }
    );

    if fail == 0 {
        println!("menu_style_smoke: OK");
    } else {
        println!("menu_style_smoke: FAIL（{fail} 项没过）");
    }
    std::process::exit(if fail == 0 { 0 } else { 1 });
}

/// 菜单项上有没有挂着 hover 用的 Motion 控制器。
fn has_motion_controller(b: &gtk::Button) -> bool {
    let model = b.observe_controllers();
    for i in 0..model.n_items() {
        let Some(obj) = model.item(i) else { continue };
        if obj.downcast_ref::<gtk::EventControllerMotion>().is_some() {
            return true;
        }
    }
    false
}

/// 把真指针挪到菜单项上，看它有没有被高亮。
///
/// **为什么不用「直接加个类」糊弄**：那测的是测试自己，不是用户看到的东西。
/// 这里走的是真指针 → GTK 事件 → Motion 控制器 → 加 `pp-menu-hover` 这条链。
fn hover_check(tree: &Rc<DbTree>) -> bool {
    let Some(pop) = tree.menu_popover() else {
        eprintln!("  （拿不到弹层，跳过 hover 验证）");
        return false;
    };
    // 挑一个可用（没被灰掉）的菜单项
    let Some((_, btn)) = tree
        .menu_items()
        .into_iter()
        .find(|(l, b)| l == "查看数据" && b.is_sensitive())
    else {
        eprintln!("  （没有可用的菜单项，跳过 hover 验证）");
        return false;
    };

    // 菜单项在弹层里的位置（同一个 surface 内，逐级累加偏移即可）
    let Some((bx, by)) = offset_in_ancestor(
        btn.upcast_ref::<gtk::Widget>(),
        pop.upcast_ref::<gtk::Widget>(),
    ) else {
        eprintln!("  （算不出菜单项位置，跳过 hover 验证）");
        return false;
    };
    // 瞄**中心**，别瞄左上角：指针正好压在边界上时 GTK 可能不算"进入"
    let alloc = btn.allocation();
    let (bx, by) = (bx + alloc.width() / 2, by + alloc.height() / 2);
    // 弹层自己的 X 窗口位置（GTK4 的 Popover 是独立的 override-redirect 窗口）
    let Some((ox, oy)) = popover_origin() else {
        eprintln!("  （找不到弹层的 X 窗口，跳过 hover 验证）");
        return false;
    };
    let target = (ox + bx, oy + by);
    println!("  （把指针挪到 {target:?}）");

    // 挪指针：先挪到一个远处的地方，确保产生"进入"事件
    let _ = run_xdotool(&["mousemove", "5", "5"]);
    pump(120);
    let _ = run_xdotool(&["mousemove", &target.0.to_string(), &target.1.to_string()]);
    pump(400);

    let hit = btn.has_css_class("pp-menu-hover");
    // 截图留证（PP_MENU_SHOT 指定路径时）
    if let Some(path) = std::env::var_os("PP_MENU_SHOT") {
        let _ = std::process::Command::new("scrot")
            .args(["-o", &path.to_string_lossy()])
            .output();
        println!("  （截图：{}）", path.to_string_lossy());
    }
    if std::env::var_os("PP_MENU_DEBUG").is_some() {
        eprintln!("指针现状：{}", pointer_pos());
        for (l, b) in tree.menu_items() {
            let off = offset_in_ancestor(
                b.upcast_ref::<gtk::Widget>(),
                pop.upcast_ref::<gtk::Widget>(),
            );
            eprintln!(
                "  {l}: hover={} sensitive={} offset={off:?}",
                b.has_css_class("pp-menu-hover"),
                b.is_sensitive()
            );
        }
    }
    hit
}

/// 指针当前在屏幕上的位置。
fn pointer_pos() -> String {
    let out = run_xdotool(&["getmouselocation"]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 控件在祖先里的偏移（逐级累加各自的 allocation）。
///
/// `gtk::Widget::compute_point` 的参数类型是 graphene 的 `Point`（f32），
/// 那是 gtk4 的传递依赖、不是本 crate 的直接依赖——为了一个例子去改
/// Cargo.toml 不值得，自己累加偏移更省事。
fn offset_in_ancestor(w: &gtk::Widget, ancestor: &gtk::Widget) -> Option<(i32, i32)> {
    let mut x = 0i32;
    let mut y = 0i32;
    let mut cur = w.clone();
    loop {
        if cur == *ancestor {
            return Some((x, y));
        }
        let a = cur.allocation();
        x += a.x();
        y += a.y();
        cur = cur.parent()?;
    }
}

/// 弹层那个 X 窗口在屏幕上的原点。
///
/// GTK4 的 `GtkPopover` 是**独立窗口**（不是主窗口的子控件），
/// 所以屏幕坐标得从 X 服务器那边问。
///
/// `xwininfo -root -tree` 给的几何是**相对父窗口**的偏移，要按层级累加
/// 才是屏幕绝对坐标。
fn popover_origin() -> Option<(i32, i32)> {
    let out = std::process::Command::new("xwininfo")
        .args(["-root", "-tree"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if std::env::var_os("PP_MENU_DEBUG").is_some() {
        eprintln!("--- xwininfo -root -tree ---\n{text}\n---");
    }
    // 每层缩进 2 个空格；`stack[i]` = 第 i 层的绝对原点
    let mut stack: Vec<(i32, i32)> = Vec::new();
    let mut fallback = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        if indent < 2 {
            continue;
        }
        let level = indent / 2 - 1;
        let Some((x, y)) = parse_geom_offset(line) else {
            continue;
        };
        let parent = if level == 0 {
            (0, 0)
        } else {
            stack.get(level - 1).copied().unwrap_or((0, 0))
        };
        let abs = (parent.0 + x, parent.1 + y);
        stack.truncate(level);
        stack.push(abs);
        let unnamed = line.contains("(has no name)");
        let is_root = line.contains("the root window");
        if unnamed && !is_root {
            // 弹层是主窗口的子窗口（缩进更深）；同名无名窗口只作兜底
            if level >= 1 {
                return Some(abs);
            }
            fallback = Some(abs);
        }
    }
    if fallback.is_none() {
        eprintln!("--- xwininfo -root -tree ---\n{text}\n---");
    }
    fallback
}

/// 从一行 `xwininfo -tree` 输出里取出 `WxH+X+Y` 里的 X / Y（相对父窗口）。
fn parse_geom_offset(line: &str) -> Option<(i32, i32)> {
    let tok = line
        .split_whitespace()
        .find(|w| w.contains('x') && w.contains('+'))?;
    let mut it = tok.split('+');
    let _wh = it.next()?; // "240x400"
    let x: i32 = it.next()?.parse().ok()?;
    let y: i32 = it.next()?.parse().ok()?;
    Some((x, y))
}

fn run_xdotool(args: &[&str]) -> std::process::Output {
    std::process::Command::new("xdotool").args(args).output().unwrap_or_else(|e| {
        eprintln!("xdotool 跑不起来：{e}");
        std::process::Output {
            status: Default::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    })
}

/// 泵一会儿主循环（等 GTK 把事件处理掉）。
fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        ctx.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
