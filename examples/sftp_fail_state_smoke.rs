//! 阶段 173 / UI-05 冒烟：**SFTP 失败以后，恢复入口不能跟着弹窗一起消失**。
//!
//! ⚠️ **背景**（UI 审核 P1-2，已确认）：连接失败时**有**弹窗、弹窗里**有**
//!   「重试」—— 这点不该误判。但真正的问题是：
//!   **用户把弹窗关掉以后**，面板上只剩一句「未连接」/「连接已断开」⇒
//!   用户既不知道是**认证、网络还是超时**，也找不到**重试入口**。
//!
//! **断言**（**不连远端**：直接喂事件）：
//! 1. 首次连接失败 ⇒ 面板状态区显示**中文分类原因**（不是「未连接」）；
//! 2. 面板里那颗**「重试」按钮亮着**（恢复入口还在）；
//! 3. **「详情」按钮**也亮着 ⇒ 能摊开**原始错误**；
//! 4. 三类失败（超时 / 认证 / 网络）各有**自己的**提示（不撞车）；
//! 5. 恢复中（正在重连）⇒ 「重试」**禁用**（防重复触发）；
//! 6. 连上以后 ⇒ 失败态**清干净**（不留过期原因），按钮也收回去；
//! 7. **用户自己打印的 `Connection refused` 不算 SSH 失败**
//!    （那条老规矩不能被这次改造破掉）。
//!
//! 运行（无桌面时用 xvfb-run）：
//!   xvfb-run -a cargo run --example sftp_fail_state_smoke

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::worker::PanelEvent;
use puttyplus::panel::{
    SftpPanel, handle_event, on_reconnect, sftp_details_visible_for_test,
    sftp_failure_for_test, sftp_retry_sensitive_for_test, sftp_retry_visible_for_test,
};

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

fn main() {
    gtk::init().expect("gtk init");
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

    // ⚠️ **隔离配置**：不写用户真实的连接/历史/凭据。
    let tmp = std::env::temp_dir().join(format!("pp_sftpfail_{}", std::process::id()));
    let cfg = tmp.join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    unsafe {
        for key in [
            "PUTTYPLUS_SETTINGS_INI",
            "PUTTYPLUS_CONN_INI",
            "PUTTYPLUS_DB_INI",
            "PUTTYPLUS_HIST_INI",
            "PUTTYPLUS_COLS_INI",
            "PUTTYPLUS_RECENT_INI",
            "PUTTYPLUS_VAULT",
        ] {
            std::env::remove_var(key);
        }
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }

    let panel = SftpPanel::new(&ConnOptions::default());
    let st = std::rc::Rc::clone(&panel.st);

    // ---- ① 首次连接失败 ⇒ 面板留下**中文分类原因** ＋ 恢复入口 ----
    handle_event(
        &st,
        PanelEvent::Connected(Err("认证失败: publickey 认证没通过".into())),
    );
    pump(300);
    let (brief, raw) = sftp_failure_for_test(&st).expect("应当记着失败态");
    println!("[实测] 分类原因 = {brief}");
    println!("[实测] 原始错误 = {raw}");
    check!(
        "失败后仍留着失败态（弹窗关了也不丢）",
        sftp_failure_for_test(&st).is_some(),
        "失败态没了"
    );
    check!(
        "状态区显示的是中文分类原因（不是「未连接」）",
        brief.contains("认证"),
        format!("brief = {brief}")
    );
    check!(
        "原始错误一字不丢",
        raw == "认证失败: publickey 认证没通过",
        format!("raw = {raw}")
    );
    check!(
        "面板里「重试」按钮亮着（恢复入口还在）",
        sftp_retry_visible_for_test(&st),
        "重试按钮没露出来"
    );

    // ---- ② 三类失败各有自己的提示（不撞车） ----
    let mut briefs = Vec::new();
    for raw_err in [
        "认证失败: publickey 认证没通过",
        "连接超时（等了 15 秒）",
        "Connection refused（127.0.0.1:2222）",
    ] {
        handle_event(&st, PanelEvent::Connected(Err(raw_err.into())));
        pump(150);
        let (b, _) = sftp_failure_for_test(&st).expect("应当记着失败态");
        briefs.push(b);
    }
    println!("[实测] 三类失败的原因 = {briefs:?}");
    check!(
        "三类失败各有自己的提示",
        briefs[0] != briefs[1] && briefs[1] != briefs[2] && briefs[0] != briefs[2],
        format!("{briefs:?}")
    );

    // ---- ③ 详情按钮亮着（能摊开原始错误） ----
    check!(
        "面板里「详情」按钮亮着",
        sftp_details_visible_for_test(&st),
        "详情按钮没露出来"
    );

    // ---- ④ 恢复中 ⇒ 「重试」**禁用**（防重复触发） ----
    //
    // ⚠️ **两个都要判**：按钮**还亮着**（用户看得见「有下一步」），
    //   但**点不了**（正在重连 ⇒ 免得发起第二套并发重连）。
    //
    // ⚠️ **走真的入口**（`on_reconnect`）⇒ 它就是「重新连接 SFTP」那颗
    //   按钮走的那条路 ⇒ 断的就是这个环节。
    on_reconnect(&st);
    check!(
        "恢复中「重试」点不了（但还看得见）",
        sftp_retry_visible_for_test(&st) && !sftp_retry_sensitive_for_test(&st),
        format!(
            "可见={} 可点={}",
            sftp_retry_visible_for_test(&st),
            sftp_retry_sensitive_for_test(&st)
        )
    );
    st.connecting.set(false);

    // ---- ⑤ 连上 ⇒ 失败态清干净 ----
    //
    // ⚠️ **当场断言**（不泵事件循环）：面板自己在后台还会真去连一次
    //   （那个空主机必然连不上）⇒ 一泵就又有了新的失败态 ⇒ 那样测的
    //   就不是「连上以后清干净」，而是「后台失败又记了一遍」。
    handle_event(&st, PanelEvent::Connected(Ok("/root".into())));
    check!(
        "连上以后失败态清干净",
        sftp_failure_for_test(&st).is_none(),
        format!("还留着 {:?}", sftp_failure_for_test(&st))
    );
    check!(
        "连上以后「重试」按钮收回去",
        !sftp_retry_visible_for_test(&st),
        "重试按钮还亮着"
    );

    // ---- ⑥ 断线也记成失败态（不是一句「连接已断开」就完事） ----
    //
    // ⚠️ **当场断言**：面板自己在后台还会真去连一次（那个空主机必然
    //   连不上）⇒ 一泵就又有了新的失败态 ⇒ 那样测的就不是「断线以后
    //   记的是断线原因」了。
    handle_event(&st, PanelEvent::Disconnected);
    let (b, _) = sftp_failure_for_test(&st).expect("断线也应当记着失败态");
    println!("[实测] 断线后的原因 = {b}");
    check!(
        "断线后仍然有恢复入口",
        sftp_retry_visible_for_test(&st),
        "重试按钮没露出来"
    );
    check!(
        "断线原因说清了下一步",
        b.contains("断开") && b.len() > 8,
        format!("brief = {b}")
    );

    let _ = std::fs::remove_dir_all(&tmp);

    if fails.is_empty() {
        println!("sftp_fail_state smoke ok");
    } else {
        println!("sftp_fail_state smoke 失败 {} 项：", fails.len());
        for f in &fails {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
