//! 阶段 53 冒烟：连接管理器对话框里的**「进程内端口映射」**分区。
//!
//! **不连服务器**——纯界面验证。
//!
//! 验三件事：
//! 1. 连接对话框里**有**「进程内端口映射」这一节（不是悄悄塞进别处）；
//! 2. 它跟原来的「端口转发」是两节、说明文字把分工讲清楚了
//!    （一份交给终端 ssh、一份跑在应用自己的连接上）；
//! 3. 截图存到 /tmp/conn_fwd.png（给人眼看）。
//!
//! 用法（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example conn_fwd_smoke

use gtk::prelude::*;
use puttyplus::connmgr;

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

/// 退出时把临时 ini 删掉（别往用户配置里塞东西）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 收集窗口里所有 Label 的文字。
fn labels(w: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        out.push(l.text().to_string());
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        labels(&x, out);
        c = x.next_sibling();
    }
}

/// 展开连接编辑器「转发」页里的折叠区（原始规则）。
fn expand_forward_expander() {
    let toplevels = gtk::Window::toplevels();
    let mut dlg: Option<gtk::Window> = None;
    for i in 0..toplevels.n_items() {
        if let Some(w) = toplevels.item(i) {
            if let Some(win) = w.downcast_ref::<gtk::Window>() {
                if win.title().as_deref() == Some("新建连接") {
                    dlg = Some(win.clone());
                    break;
                }
            }
        }
    }
    let Some(dlg) = dlg else { return };
    let mut exp: Option<gtk::Expander> = None;
    collect_expanders(dlg.upcast_ref::<gtk::Widget>(), &mut exp);
    if let Some(e) = exp {
        e.set_expanded(true);
        pump(200);
    }
}

/// 递归找第一个 Expander。
fn collect_expanders(w: &gtk::Widget, out: &mut Option<gtk::Expander>) {
    if out.is_none() {
        if let Some(e) = w.downcast_ref::<gtk::Expander>() {
            *out = Some(e.clone());
            return;
        }
    }
    let mut c = w.first_child();
    while let Some(x) = c {
        collect_expanders(&x, out);
        c = x.next_sibling();
    }
}

fn main() {
    let app = gtk::Application::builder()
        .application_id("x.y.PpConnFwd")
        .build();
    app.connect_activate(|app| {
        // ⚠️ **必须装主题**：自定义 CSS 类只有 theme::apply() 之后才生效
        puttyplus::theme::apply(puttyplus::theme::Theme::Dark);

        // ⚠️ 别碰用户的真实配置
        let conn_ini = std::env::temp_dir().join(format!(
            "pp-conn-fwd-smoke-{}.ini",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&conn_ini);
        unsafe { std::env::set_var("PUTTYPLUS_CONN_INI", &conn_ini) };
        let _guard = IniGuard(conn_ini.clone());

        connmgr::ui_new_connection(None, |_| {});
        pump(600);

        // ⚠️ **先把折叠区展开**（阶段 73b 修）：这两个分区标题在「转发」页的
        // 折叠区（Expander）里，而 **GTK4 折叠时把 Expander 的子控件摘掉**
        // ⇒ 不展开就**摸不到**它们 ⇒ 断言必然挂（这冒烟从阶段 71 把两个
        // 原始规则域搬进折叠区起就一直挂着，是老账）。
        expand_forward_expander();

        // 把所有顶层窗口里的文字捞出来
        let toplevels = gtk::Window::toplevels();
        let mut texts = Vec::new();
        for i in 0..toplevels.n_items() {
            if let Some(w) = toplevels.item(i) {
                if let Some(win) = w.downcast_ref::<gtk::Window>() {
                    labels(win.upcast_ref::<gtk::Widget>(), &mut texts);
                }
            }
        }
        println!("对话框文字：{texts:?}");

        // ---- 断言 1：有「进程内端口映射」这一节 ----
        assert!(
            texts.iter().any(|t| t == "进程内端口映射"),
            "连接对话框里没有「进程内端口映射」分区：{texts:?}"
        );

        // ---- 断言 2：说明文字讲清了两份规则的分工 ----
        // ⚠️ 断言只认「进程内 = 跑在应用自己那条」这一段：提示文案里
        // 「那条SSH」是**没有空格**的（Rust 的续行 `\` 把换行处的空白吃掉了），
        // 拿带空格的整句去匹配 ⇒ 必挂（老账）。
        assert!(
            texts.iter().any(|t| t.contains("进程内 = 跑在应用自己那条")),
            "缺少「进程内映射」的分工说明：{texts:?}"
        );
        // ⚠️ 阶段 71 把「端口转发」这个分区改叫「终端转发」（要和「进程内
        // 端口映射」区分开：一份交给终端 ssh，一份跑在应用自己那条连接上）
        assert!(
            texts.iter().any(|t| t == "终端转发"),
            "「终端转发」分区不该消失：{texts:?}"
        );

        // ---- 截图 ----
        let shot = std::env::var("PP_CONN_SHOT").unwrap_or_else(|_| "/tmp/conn_fwd.png".into());
        let _ = std::process::Command::new("/usr/bin/scrot")
            .arg("-o")
            .arg(&shot)
            .status();
        println!("截图：{shot}");

        println!("[OK] 连接对话框的进程内端口映射分区");
        app.quit();
    });
    app.run();
}
