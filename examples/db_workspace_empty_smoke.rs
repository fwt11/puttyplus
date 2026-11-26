//! 阶段 175 / UI-06 冒烟：**数据库空态以任务为主**（治 P1-3）。
//!
//! ⚠️ **背景**（UI 审核 P1-3）：未连接时 `refresh_empty()` 摆的是
//!   **大号应用图标 ＋ 产品名「PuTTYPlus」＋「数据库工作台」**⇒ 占满视觉
//!   中心 ⇒ 用户第一眼看到的是「品牌」不是「现在该干什么」；而且没连接
//!   时**没有当前会话内的直接动作**（只有一句「先在左侧打开一个数据库
//!   连接」）。
//!
//! **断言**：
//! 1. **没有品牌区**：空态里不再出现「PuTTYPlus」这个产品名；
//! 2. **未连接态有且只有一个明确主 CTA**，文字是「新建数据库连接…」；
//! 3. **点主 CTA 真的打开当前会话的新建对话框**（标题
//!    「新建数据库连接（当前会话）」）⇒ 走的是既有流程，不是复制一份；
//! 4. **已连接态**保留 chip ＋ 三个**真动作**（浏览对象 / 写一条查询 /
//!    执行历史）；
//! 5. **三种状态标题不同**：「没连接」「连失败」「连上了但没对象」不许
//!    混为一谈；
//! 6. **工作台顶部不加第二套连接表单**（审核验收）——界面上没有连接参数
//!    输入框。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example db_workspace_empty_smoke

use std::rc::Rc;

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::backend::db::DbProto;
use puttyplus::backend::dbstore::{self, DbProfile};
use puttyplus::panel::{dbui, SftpPanel};

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

/// 退出时把临时 ini 删掉（别往用户配置里塞假连接）。
struct IniGuard(std::path::PathBuf);

impl Drop for IniGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn main() {
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

    // ⚠️ **隔离配置**：不写用户真实的数据库连接。
    let db_ini = std::env::temp_dir().join(format!("pp_db_ws_empty_{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&db_ini);
    unsafe {
        std::env::set_var("PUTTYPLUS_DB_INI", &db_ini);
    }
    let _guard = IniGuard(db_ini.clone());

    let panel = SftpPanel::new(&ConnOptions::default());
    let st: Rc<puttyplus::panel::PanelState> = Rc::clone(&panel.st);
    // ⚠️ **要一个真的 Tabs**：主 CTA 走 `newmenu::run_in_session(&tabs, …)`
    //   （审核验收：「调用现有 `newmenu::run_in_session` 的数据库连接流程」）
    //   ⇒ 没有 Tabs 就点不出那个对话框。
    let _tabs = puttyplus::tabs::Tabs::new();

    // ---- ① 未连接 ----
    dbui::ensure(&st);
    pump(300);

    let texts = dbui::empty_state_text(&st);
    println!("[实测] 未连接空态文字：{texts:?}");
    check!(
        "**没有品牌区**（不再摆产品名「PuTTYPlus」）",
        !texts.iter().any(|t| t == "PuTTYPlus"),
        format!("{texts:?}")
    );
    check!(
        "未连接态的标题是「还没有数据库连接」",
        texts.iter().any(|t| t == "还没有数据库连接"),
        format!("{texts:?}")
    );
    check!(
        "未连接态有解释（说清楚能干什么）",
        texts.iter().any(|t| t.contains("查表")),
        format!("{texts:?}")
    );

    let btns = dbui::empty_state_buttons(&st);
    println!("[实测] 未连接空态按钮：{btns:?}");
    check!(
        "未连接态**有且只有一个**按钮（一个明确的主 CTA）",
        btns.len() == 1,
        format!("{} 个按钮", btns.len())
    );
    check!(
        "主 CTA 的文字是「新建数据库连接…」",
        btns.first().map(|(t, _)| t.as_str()) == Some("新建数据库连接…"),
        format!("{btns:?}")
    );
    check!(
        "主 CTA 可点（不是灰的）",
        btns.first().map(|(_, s)| *s).unwrap_or(false),
        format!("{btns:?}")
    );

    // ---- ② 点主 CTA ⇒ 真的打开「新建数据库连接（当前会话）」 ----
    {
        let primary = dbui::empty_state_primary_for_test(&st).expect("应当有主 CTA");
        primary.emit_clicked();
        pump(600);
        let dlg = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("新建数据库连接（当前会话）"));
        check!(
            "点主 CTA 打开了**当前会话**的新建数据库连接对话框",
            dlg.is_some(),
            format!("顶层窗口：{:?}", titles())
        );
        if let Some(d) = dlg {
            d.close();
        }
        pump(200);
    }

    // ---- ③ 已连接：chip ＋ 三个真动作 ----
    {
        let key = dbstore::session_key(&st.opts);
        let prof = DbProfile {
            name: "冒烟库".to_string(),
            proto: DbProto::Postgres,
            host: "127.0.0.1".to_string(),
            port: 5432,
            user: "postgres".to_string(),
            dbname: "postgres".to_string(),
            remember: false,
        };
        dbstore::save(&key, &prof);
        let ok = dbui::empty_state_set_connected(&st, "冒烟库");
        check!(
            "能切到「已连接」那一版空态",
            ok,
            "没切过去（可能连接定义没存上）".to_string()
        );
        pump(300);
        let texts = dbui::empty_state_text(&st);
        println!("[实测] 已连接空态文字：{texts:?}");
        check!(
            "已连接态的标题与「没连接」**不同**",
            texts.iter().any(|t| t == "当前库还没有对象")
                && !texts.iter().any(|t| t == "还没有数据库连接"),
            format!("{texts:?}")
        );
        check!(
            "已连接态显示当前库（chip）",
            texts.iter().any(|t| t == "冒烟库"),
            format!("{texts:?}")
        );
        let btns = dbui::empty_state_buttons(&st);
        println!("[实测] 已连接空态按钮：{btns:?}");
        for want in ["浏览对象", "写一条查询", "执行历史"] {
            check!(
                &format!("已连接态保留「{want}」这个真动作"),
                btns.iter().any(|(t, _)| t == want),
                format!("{btns:?}")
            );
        }
        check!(
            "**没有连接参数输入框**（工作台顶部不加第二套连接表单）",
            !texts.iter().any(|t| t.contains("端口") || t.contains("密码")),
            format!("{texts:?}")
        );
    }

    // ---- ④ 三种状态不许混为一谈 ----
    {
        let s1 = puttyplus::emptystate::db_not_connected(false);
        let s2 = puttyplus::emptystate::db_failed();
        let s3 = puttyplus::emptystate::db_connected();
        check!(
            "「没连接」「连失败」「连上了」三种标题各不相同",
            s1.title != s2.title && s2.title != s3.title && s1.title != s3.title,
            format!("{} / {} / {}", s1.title, s2.title, s3.title)
        );
        check!(
            "三种解释也各不相同",
            s1.desc != s2.desc && s2.desc != s3.desc && s1.desc != s3.desc,
            format!("{} / {} / {}", s1.desc, s2.desc, s3.desc)
        );
        for (n, s) in [("没连接", &s1), ("连失败", &s2), ("连上了", &s3)] {
            check!(
                &format!("{n} 这一态有标题、解释、主 CTA"),
                !s.title.is_empty() && !s.desc.is_empty() && !s.primary.label.is_empty(),
                format!("{n}：{s:?}")
            );
        }
    }

    if fails.is_empty() {
        println!("db_workspace_empty smoke ok");
    } else {
        println!("db_workspace_empty smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}

fn titles() -> Vec<String> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .map(|w| w.title().unwrap_or_default().to_string())
        .collect()
}
